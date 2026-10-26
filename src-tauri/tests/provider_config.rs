//! Provider form updates must preserve account runtime bindings.

use llm_gateway_lib::db::{repo, Db};
use llm_gateway_lib::domain::{Dialect, Provider};

fn provider(id: &str) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: id.into(),
        dialect: Dialect::OpenAI,
        base_url: "http://127.0.0.1:1/v1".into(),
        api_key_enc: String::new(),
        enabled: true,
        priority: 10,
        models: Vec::new(),
        rpm_limit: 0,
        intelligence: 50,
        note: None,
        runtime_id: None,
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn form_updates_preserve_runtime_when_disabling_and_reordering() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut saved = provider("bound");
    saved.runtime_id = Some("codex-work".into());
    repo::upsert_provider(db.pool(), &saved).await.unwrap();

    let mut form = provider("bound");
    form.enabled = false;
    form.priority = 20;
    form.name = "Edited provider".into();
    repo::upsert_provider_config(db.pool(), &form)
        .await
        .unwrap();

    let updated = repo::list_providers(db.pool()).await.unwrap().remove(0);
    assert_eq!(updated.runtime_id.as_deref(), Some("codex-work"));
    assert!(!updated.enabled);
    assert_eq!(updated.priority, 20);
    assert_eq!(updated.name, "Edited provider");
}

#[tokio::test]
async fn new_and_copied_provider_ids_start_without_runtime_bindings() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut original = provider("original");
    original.runtime_id = Some("codex-work".into());
    repo::upsert_provider(db.pool(), &original).await.unwrap();

    repo::upsert_provider_config(db.pool(), &provider("new"))
        .await
        .unwrap();
    let mut copy = original.clone();
    copy.id = "copy".into();
    repo::upsert_provider_config(db.pool(), &copy)
        .await
        .unwrap();

    let rows = repo::list_providers(db.pool()).await.unwrap();
    for id in ["new", "copy"] {
        assert_eq!(
            rows.iter().find(|row| row.id == id).unwrap().runtime_id,
            None,
            "new configuration IDs must not inherit a runtime: {id}",
        );
    }
    assert_eq!(
        rows.iter()
            .find(|row| row.id == "original")
            .unwrap()
            .runtime_id
            .as_deref(),
        Some("codex-work"),
    );
}

#[tokio::test]
async fn explicit_runtime_updates_can_still_clear_bindings() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut saved = provider("bound");
    saved.runtime_id = Some("codex-work".into());
    repo::upsert_provider(db.pool(), &saved).await.unwrap();
    saved.runtime_id = None;
    repo::upsert_provider(db.pool(), &saved).await.unwrap();
    assert_eq!(
        repo::list_providers(db.pool()).await.unwrap()[0].runtime_id,
        None
    );
}

async fn owned_wal_provider_db() -> Db {
    let path = std::env::temp_dir().join(format!(
        "llmgw-owned-provider-audit-{}.db",
        uuid::Uuid::new_v4()
    ));
    let db = Db::connect_path(&path).await.unwrap();
    eprintln!("owned provider/audit WAL fixture: {}", path.display());
    db
}

#[tokio::test]
async fn finalfix_wal_save_waits_for_competing_audit_commit() {
    let db = owned_wal_provider_db().await;
    let mut saved = provider("wal-bound");
    saved.runtime_id = Some("codex-work".into());
    repo::upsert_provider(db.pool(), &saved).await.unwrap();
    for preserve in [true, false] {
        let mut audit = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("INSERT INTO requests(ts,requested_model,status,trace_id) VALUES(1,'wal-control',200,?)")
            .bind(format!("held-audit-{preserve}")).execute(&mut *audit).await.unwrap();
        let mut proposed = saved.clone();
        proposed.name = format!("edited-{preserve}");
        if preserve {
            proposed.runtime_id = None;
        }
        let saving = async {
            if preserve {
                repo::upsert_provider_config(db.pool(), &proposed).await
            } else {
                repo::upsert_provider(db.pool(), &proposed).await
            }
        };
        tokio::pin!(saving);
        // A known live writer owns the lock: a save must wait, not upgrade an
        // old read snapshot and return SQLITE_BUSY/SNAPSHOT to the user.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut saving)
                .await
                .is_err(),
            "provider save failed before the competing audit could commit"
        );
        audit.commit().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), saving)
            .await
            .unwrap()
            .unwrap();
        let row = repo::list_providers(db.pool()).await.unwrap().remove(0);
        assert_eq!(row.name, format!("edited-{preserve}"));
        assert_eq!(row.runtime_id.as_deref(), Some("codex-work"));
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn finalfix_wal_real_audits_and_configuration_saves_complete_without_lost_rows() {
    let db = owned_wal_provider_db().await;
    let mut saved = provider("wal-bound");
    saved.runtime_id = Some("codex-work".into());
    repo::upsert_provider(db.pool(), &saved).await.unwrap();
    let edits = async {
        for index in 0..24 {
            let mut form = provider("wal-bound");
            form.name = format!("edit-{index}");
            form.priority = index;
            repo::upsert_provider_config(db.pool(), &form)
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
    };
    let audits = async {
        for index in 0..24 {
            let trace = format!("parallel-audit-{index}");
            repo::log_request_with_usage(
                db.pool(),
                repo::RequestLog {
                    session_id: None,
                    client: None,
                    requested_model: "fixture-model",
                    routed_provider: Some("wal-bound"),
                    routed_model: Some("fixture-model"),
                    status: Some(200),
                    latency_ms: 1,
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    fallback_attempts: 0,
                    error: None,
                    cost: None,
                    currency: None,
                    rate_label: None,
                    estimated_prompt_tokens: None,
                    attempts_json: None,
                    route: Default::default(),
                    access_key_id: None,
                    refined_prompt: None,
                    trace_id: &trace,
                },
                &[repo::UsageEntry {
                    provider_id: "wal-bound".into(),
                    model: "fixture-model".into(),
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    cost: None,
                    currency: None,
                    errors: 0,
                }],
            )
            .await
            .unwrap();
            tokio::task::yield_now().await;
        }
    };
    tokio::join!(edits, audits);
    let row = repo::list_providers(db.pool()).await.unwrap().remove(0);
    assert_eq!(row.name, "edit-23");
    assert_eq!(row.priority, 23);
    assert_eq!(row.runtime_id.as_deref(), Some("codex-work"));
    let totals: (i64, i64, i64) =
        sqlx::query_as("SELECT COUNT(*),SUM(prompt_tokens),SUM(completion_tokens) FROM requests")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(totals, (24, 72, 48));
    let unique: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT trace_id) FROM requests")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(unique, 24);
}
