use llm_gateway_lib::{
    audit::RequestFilter,
    db::{
        repo::request_feedback::{self as f, FeedbackInput},
        Db,
    },
};
use sqlx::Row;
async fn seed(db: &Db, ts: i64, provider: &str, model: &str, status: i64) -> i64 {
    sqlx::query("INSERT INTO requests(ts,requested_model,routed_provider,routed_model,status,trace_id,route_intent) VALUES (?,'owned-alias',?,?,?,'same-trace','simple') RETURNING id")
        .bind(ts).bind(provider).bind(model).bind(status).fetch_one(db.pool()).await.unwrap().get("id")
}
async fn mark(db: &Db, id: i64, class: Option<&str>, outcome: Option<&str>, revision: i64) {
    f::set(
        db.pool(),
        FeedbackInput {
            request_id: id,
            expected_revision: revision,
            confirmed_class: class.map(str::to_owned),
            outcome: outcome.map(str::to_owned),
        },
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn coverage_uses_human_labels_not_http_success_machine_class_or_cleared_tombstones() {
    let db = Db::connect_in_memory().await.unwrap();
    for i in 0..8 {
        let id = seed(
            &db,
            100 + i,
            "owned",
            "physical",
            if i == 1 { 499 } else { 200 },
        )
        .await;
        match i {
            0 => mark(&db, id, Some("vision"), Some("met_need"), 0).await,
            1 => mark(&db, id, Some("vision"), Some("unsure"), 0).await,
            2 => mark(&db, id, Some("reasoning"), Some("did_not_meet_need"), 0).await,
            3 => mark(&db, id, Some("reasoning"), None, 0).await,
            4 => {
                mark(&db, id, Some("vision"), Some("met_need"), 0).await;
                mark(&db, id, None, None, 1).await;
            }
            _ => {}
        }
    }
    let c = f::coverage(db.pool(), &Default::default()).await.unwrap();
    assert_eq!(
        (
            c.counts.requests,
            c.counts.annotated,
            c.counts.class_confirmed
        ),
        (8, 4, 4)
    );
    assert_eq!(
        (
            c.counts.met_need,
            c.counts.did_not_meet_need,
            c.counts.unsure,
            c.counts.missing_outcome
        ),
        (1, 1, 1, 5)
    );
    assert_eq!(c.groups.len(), 3);
    assert!(!c.groups_truncated);
    let unknown = c
        .groups
        .iter()
        .find(|g| g.confirmed_class.is_none())
        .unwrap();
    assert_eq!(unknown.counts.requests, 4);
    assert_eq!(unknown.counts.annotated, 0);
    assert!(
        !c.groups
            .iter()
            .any(|g| g.confirmed_class.as_deref() == Some("simple")),
        "machine route_intent never becomes a confirmed human class"
    );
}
#[tokio::test]
async fn coverage_filter_is_full_matching_population_not_current_page_and_empty_stays_empty() {
    let db = Db::connect_in_memory().await.unwrap();
    for i in 0..6 {
        seed(
            &db,
            100 + i,
            if i < 4 { "chosen" } else { "other" },
            "physical",
            200,
        )
        .await;
    }
    let filter = RequestFilter {
        provider: Some("chosen".into()),
        from_ts: Some(101),
        to_ts: Some(103),
        limit: Some(1),
        offset: Some(99),
        ..Default::default()
    };
    let c = f::coverage(db.pool(), &filter).await.unwrap();
    assert_eq!(c.counts.requests, 3);
    assert_eq!(c.counts.missing_outcome, 3);
    assert_eq!(c.counts.annotated, 0);
    let empty = f::coverage(
        db.pool(),
        &RequestFilter {
            provider: Some("missing".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(empty.counts.requests, 0);
    assert_eq!(empty.counts.missing_outcome, 0);
    assert!(empty.groups.is_empty());
}
#[tokio::test]
async fn coverage_caps_groups_without_truncating_totals_and_storage_failure_is_not_zero() {
    let db = Db::connect_in_memory().await.unwrap();
    for i in 0..201 {
        seed(&db, 100, "owned", &format!("physical-{i:03}"), 200).await;
    }
    let c = f::coverage(db.pool(), &Default::default()).await.unwrap();
    assert_eq!(c.counts.requests, 201);
    assert_eq!(c.groups.len(), 200);
    assert!(c.groups_truncated);
    assert_eq!(c.counts.missing_outcome, 201);
    sqlx::query("DROP TABLE request_feedback")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        f::coverage(db.pool(), &Default::default())
            .await
            .unwrap_err()
            .to_string(),
        "feedback_storage_failed"
    );
}
