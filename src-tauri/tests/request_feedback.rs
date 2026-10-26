use llm_gateway_lib::db::{self, repo, Db};
use repo::request_feedback::{self as feedback, FeedbackError as E, FeedbackInput};
use serde_json::json;
use sqlx::{Row, SqlitePool};

async fn seed(pool: &SqlitePool, status: i64) -> i64 {
    sqlx::query("INSERT INTO requests(ts,session_id,client,requested_model,routed_provider,routed_model,status,trace_id,route_intent,route_classifier,cost,currency) VALUES (1,'same-session','owned-fixture','smart','owned','actual-model',?,'same-trace','reasoning','rule',0.25,'USD') RETURNING id")
        .bind(status).fetch_one(pool).await.unwrap().get("id")
}
fn input(id: i64, revision: i64) -> FeedbackInput {
    FeedbackInput {
        request_id: id,
        expected_revision: revision,
        confirmed_class: Some("reasoning".into()),
        outcome: Some("met_need".into()),
    }
}
struct OwnedFile(std::path::PathBuf);
impl OwnedFile {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("llmgw-owned-feedback-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn db(&self) -> std::path::PathBuf {
        self.0.join("owned.sqlite")
    }
}
impl Drop for OwnedFile {
    fn drop(&mut self) {
        eprintln!("OWNED_FEEDBACK_FIXTURE={}", self.0.display());
    }
}

#[tokio::test]
async fn owned_database_initialization_has_additive_feedback_without_automatic_labels() {
    let db = Db::connect_in_memory().await.unwrap();
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='request_feedback'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        exists, 1,
        "normal database initialization must provide durable request feedback"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_feedback")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "migration must not manufacture human labels");
}

#[tokio::test]
async fn feedback_is_per_id_not_trace_preserves_machine_facts_and_clear_fences_stale_writes() {
    let db = Db::connect_in_memory().await.unwrap();
    let a = seed(db.pool(), 200).await;
    let b = seed(db.pool(), 499).await;
    let machine = repo::query_requests(db.pool(), &Default::default())
        .await
        .unwrap()
        .0;
    let unmarked = feedback::get(db.pool(), a).await.unwrap();
    assert_eq!(unmarked.revision, 0);
    assert!(unmarked.outcome.is_none());
    assert!(unmarked.confirmed_class.is_none());
    assert!(unmarked.source.is_none());
    let saved = feedback::set(db.pool(), input(a, 0)).await.unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.source.as_deref(), Some("local_user"));
    assert_eq!(saved.route_intent.as_deref(), Some("reasoning"));
    assert_eq!(saved.routed_model.as_deref(), Some("actual-model"));
    assert_eq!(feedback::get(db.pool(), b).await.unwrap().revision, 0);
    assert_eq!(
        feedback::set(db.pool(), input(a, 0)).await.unwrap_err(),
        E::FeedbackConflict
    );
    let cleared = feedback::set(
        db.pool(),
        FeedbackInput {
            confirmed_class: None,
            outcome: None,
            ..input(a, 1)
        },
    )
    .await
    .unwrap();
    assert_eq!(cleared.revision, 2);
    assert!(cleared.outcome.is_none());
    assert!(cleared.confirmed_class.is_none());
    assert_eq!(cleared.created_at, saved.created_at);
    assert_eq!(
        feedback::set(db.pool(), input(a, 1)).await.unwrap_err(),
        E::FeedbackConflict
    );
    let failed = feedback::set(
        db.pool(),
        FeedbackInput {
            outcome: Some("unsure".into()),
            confirmed_class: None,
            ..input(b, 0)
        },
    )
    .await
    .unwrap();
    assert_eq!(failed.status, Some(499));
    assert_eq!(failed.outcome.as_deref(), Some("unsure"));
    let after = repo::query_requests(db.pool(), &Default::default())
        .await
        .unwrap()
        .0;
    assert_eq!(
        serde_json::to_value(machine).unwrap(),
        serde_json::to_value(after).unwrap(),
        "human labels never rewrite audit, usage or billing"
    );
}

#[tokio::test]
async fn additive_upgrade_reopen_and_reentrant_migration_keep_feedback_and_old_audit() {
    let owned = OwnedFile::new();
    let db = Db::connect_path(owned.db()).await.unwrap();
    let id = seed(db.pool(), 200).await;
    sqlx::query("DROP TABLE request_feedback")
        .execute(db.pool())
        .await
        .unwrap();
    db.pool().close().await;
    let db = Db::connect_path(owned.db()).await.unwrap();
    assert_eq!(feedback::get(db.pool(), id).await.unwrap().revision, 0);
    feedback::set(db.pool(), input(id, 0)).await.unwrap();
    db::migrations::run(db.pool()).await.unwrap();
    db.pool().close().await;
    let reopened = Db::connect_path(owned.db()).await.unwrap();
    let saved = feedback::get(reopened.pool(), id).await.unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.outcome.as_deref(), Some("met_need"));
    assert_eq!(saved.status, Some(200));
    repo::purge_old_requests(reopened.pool(), 1).await.unwrap();
    assert_eq!(
        feedback::get(reopened.pool(), id).await.unwrap_err(),
        E::FeedbackRequestNotFound
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_feedback")
        .fetch_one(reopened.pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
    reopened.pool().close().await;
}

#[tokio::test]
async fn concurrent_writers_have_one_cas_winner_and_prune_never_leaves_orphans() {
    let owned = OwnedFile::new();
    let db = Db::connect_path(owned.db()).await.unwrap();
    let id = seed(db.pool(), 200).await;
    let (a, b) = tokio::join!(
        feedback::set(db.pool(), input(id, 0)),
        feedback::set(
            db.pool(),
            FeedbackInput {
                outcome: Some("did_not_meet_need".into()),
                ..input(id, 0)
            }
        )
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(a.err().or(b.err()), Some(E::FeedbackConflict));
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_feedback")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
    let id2 = seed(db.pool(), 504).await;
    let (writing, deleting) = tokio::join!(
        feedback::set(db.pool(), input(id2, 0)),
        sqlx::query("DELETE FROM requests WHERE id=?")
            .bind(id2)
            .execute(db.pool())
    );
    deleting.unwrap();
    assert!(writing.is_ok() || matches!(writing, Err(E::FeedbackRequestNotFound)));
    let orphans:i64=sqlx::query_scalar("SELECT COUNT(*) FROM request_feedback f LEFT JOIN requests r ON f.request_id=r.id WHERE r.id IS NULL").fetch_one(db.pool()).await.unwrap();
    assert_eq!(orphans, 0);
    db.pool().close().await;
}

#[tokio::test]
async fn invalid_and_missing_targets_or_storage_failure_never_fake_a_success() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = seed(db.pool(), 200).await;
    for bad in [0, -1, 9_007_199_254_740_992] {
        assert_eq!(
            feedback::get(db.pool(), bad).await.unwrap_err(),
            E::FeedbackInvalidInput
        );
        assert_eq!(
            feedback::set(db.pool(), input(bad, 0)).await.unwrap_err(),
            E::FeedbackInvalidInput
        );
    }
    for bad in [
        FeedbackInput {
            expected_revision: -1,
            ..input(id, 0)
        },
        FeedbackInput {
            confirmed_class: Some("Reasoning".into()),
            ..input(id, 0)
        },
        FeedbackInput {
            outcome: Some("http_200_means_good".into()),
            ..input(id, 0)
        },
    ] {
        assert_eq!(
            feedback::set(db.pool(), bad).await.unwrap_err(),
            E::FeedbackInvalidInput
        );
    }
    assert!(serde_json::from_value::<FeedbackInput>(json!({"request_id":id,"expected_revision":0,"confirmed_class":null,"outcome":null,"source":"trusted_issuer"})).is_err());
    assert_eq!(
        feedback::set(db.pool(), input(id + 10, 0))
            .await
            .unwrap_err(),
        E::FeedbackRequestNotFound
    );
    sqlx::raw_sql("CREATE TRIGGER owned_feedback_failure BEFORE INSERT ON request_feedback BEGIN SELECT RAISE(ABORT,'owned internal message must not leak'); END;").execute(db.pool()).await.unwrap();
    let error = feedback::set(db.pool(), input(id, 0)).await.unwrap_err();
    assert_eq!(
        serde_json::to_value(error).unwrap(),
        json!("feedback_storage_failed")
    );
    assert_eq!(feedback::get(db.pool(), id).await.unwrap().revision, 0);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_feedback")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
}
