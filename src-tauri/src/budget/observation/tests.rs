use super::*;
use crate::{db::Db, domain::RemoteAccessKey};
use std::sync::Arc;
fn time() -> DateTime<Utc> {
    "2026-10-20T00:00:00Z".parse().unwrap()
}
fn month() -> i64 {
    crate::budget::month_start_secs(time().timestamp())
}
async fn key(db: &Db, id: &str, limit: i64, currency: &str) {
    let at = time();
    crate::db::repo::create_remote_access_key(
        db.pool(),
        &RemoteAccessKey {
            id: id.into(),
            label: "owned-observation".into(),
            key_hash: format!("owned-hash-must-not-be-displayed-{id}"),
            enabled: true,
            rpm_limit: 20,
            monthly_budget_micros: limit,
            budget_currency: currency.into(),
            allowed_models: vec![],
            created_at: at,
            updated_at: at,
        },
    )
    .await
    .unwrap();
}
async fn hold(
    db: &Db,
    key: &str,
    state: &str,
    reserved: i64,
    reported: Option<i64>,
    currency: &str,
) {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO budget_holds (hold_id,gateway_request_uuid,call_ordinal,key_id,admission_month,currency,state,reserved_micros,reported_reference_micros,target_price_json,created_at,updated_at) VALUES (?,?,0,?,?,?,?,?,?,'owned-target-json-not-for-ui',0,0)")
 .bind(&id).bind(&id).bind(key).bind(month()).bind(currency).bind(state).bind(reserved).bind(reported).execute(db.pool()).await.unwrap();
}
async fn changes(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT total_changes()")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn unlimited_without_currency_has_unknown_counts_and_no_negative_balance() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "owned", 0, "").await;
    hold(&db, "owned", "dispatched", 4, None, "usd").await;
    let value = read(db.pool(), "owned", time())
        .await
        .unwrap()
        .with_policy(true);
    let wire = serde_json::to_value(&value).unwrap();
    assert!(
        wire["outstanding_calls"].is_null(),
        "without a chosen currency, unaggregated count must not become zero"
    );
    assert!(wire["other_currency_outstanding_calls"].is_null());
    sqlx::query("UPDATE remote_access_keys SET budget_currency='USD' WHERE id='owned'")
        .execute(db.pool())
        .await
        .unwrap();
    let value = read(db.pool(), "owned", time())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(value.mode, "unlimited");
    assert_eq!(value.total_micros.as_deref(), Some("4"));
    assert_eq!(
        value.remaining_micros, None,
        "unlimited is not a zero monetary limit"
    );
}

#[tokio::test]
async fn buckets_reuse_admission_total_without_writes_or_secrets() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "owned", 100, "USD").await;
    key(&db, "other", 100, "USD").await;
    sqlx::query("INSERT INTO requests(ts,requested_model,access_key_id,cost,currency) VALUES (?,'owned','owned',0.0000015,' USD ')").bind(month()+1).execute(db.pool()).await.unwrap();
    for (owner, state, reserved, reported, currency) in [
        ("owned", "settled", 50, Some(3), "usd"),
        ("owned", "reserved", 4, None, "USD"),
        ("owned", "dispatched", 2, None, "usd"),
        ("owned", "released", 90, None, "usd"),
        ("owned", "settled", 999, Some(999), "cny"),
        ("other", "dispatched", 99, None, "usd"),
    ] {
        hold(&db, owner, state, reserved, reported, currency).await;
    }
    let before = changes(&db).await;
    let s = read(db.pool(), "owned", time())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(changes(&db).await, before);
    assert_eq!(s.ledger_settled_micros.as_deref(), Some("3"));
    assert_eq!(s.known_spend_micros.as_deref(), Some("5"));
    assert_eq!(s.outstanding_micros.as_deref(), Some("6"));
    assert_eq!(s.outstanding_calls, Some(2));
    assert_eq!(s.total_micros.as_deref(), Some("11"));
    assert_eq!(s.remaining_micros.as_deref(), Some("89"));
    assert_eq!(s.mode, "reference_headroom");
    assert_eq!(
        crate::budget::holds::used_for_auth(db.pool(), "owned", month(), "usd")
            .await
            .unwrap(),
        11
    );
    let wire = serde_json::to_string(&s).unwrap();
    assert!(
        !wire.contains("owned-hash") && !wire.contains("target-json") && !wire.contains("key_hash")
    );
}
#[tokio::test]
async fn unknown_history_does_not_zero_known_ledger_or_invent_remaining() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "owned", 100, "USD").await;
    hold(&db, "owned", "settled", 3, Some(3), "usd").await;
    hold(&db, "owned", "dispatched", 4, None, "usd").await;
    sqlx::query("INSERT INTO requests(ts,requested_model,access_key_id,cost,currency) VALUES (?,'owned','owned',NULL,'usd')").bind(month()+1).execute(db.pool()).await.unwrap();
    let before = changes(&db).await;
    let s = read(db.pool(), "owned", time())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(changes(&db).await, before);
    assert_eq!(s.blocked_reason, Some("legacy_cost_unknown"));
    assert_eq!(s.mode, "blocked_unknown");
    assert_eq!(s.known_spend_micros, None);
    assert_eq!(s.total_micros, None);
    assert_eq!(s.remaining_micros, None);
    assert_eq!(s.ledger_settled_micros.as_deref(), Some("3"));
    assert_eq!(s.outstanding_micros.as_deref(), Some("4"));
}
#[tokio::test]
async fn other_currency_pending_is_not_added_or_refunded() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "owned", 100, "USD").await;
    hold(&db, "owned", "dispatched", 4, None, "usd").await;
    hold(&db, "owned", "reserved", 900, None, "cny").await;
    let before = changes(&db).await;
    let s = read(db.pool(), "owned", time())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(changes(&db).await, before);
    assert_eq!(
        s.blocked_reason,
        Some("budget_currency_changed_with_existing_holds")
    );
    assert_eq!(s.other_currency_outstanding_calls, Some(1));
    assert_eq!(s.outstanding_micros.as_deref(), Some("4"));
    assert_eq!(s.total_micros, None);
    assert_eq!(s.remaining_micros, None);
}
#[tokio::test]
async fn exact_i64_decimal_and_overrun_negative_are_not_floats() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "owned", 100, "USD").await;
    hold(&db, "owned", "settled", 1, Some(9007199254740993), "usd").await;
    let s = read(db.pool(), "owned", time())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(s.total_micros.as_deref(), Some("9007199254740993"));
    assert_eq!(s.remaining_micros.as_deref(), Some("-9007199254740893"));
    assert_eq!(s.mode, "exhausted");
    assert!(serde_json::to_value(&s).unwrap()["total_micros"].is_string());
}
#[tokio::test]
async fn modes_and_new_period_do_not_enable_or_clear_existing_occupancy() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "owned", 100, "USD").await;
    key(&db, "unlimited", 0, "").await;
    hold(&db, "owned", "dispatched", 4, None, "usd").await;
    assert_eq!(
        read(db.pool(), "owned", time())
            .await
            .unwrap()
            .with_policy(false)
            .mode,
        "disabled"
    );
    let open = read(db.pool(), "unlimited", time())
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(open.mode, "unlimited");
    assert_eq!(open.total_micros, None);
    assert_eq!(open.currency, None);
    sqlx::query("UPDATE remote_access_keys SET enabled=0 WHERE id='owned'")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        read(db.pool(), "owned", time())
            .await
            .unwrap()
            .with_policy(true)
            .mode,
        "key_disabled"
    );
    let next = read(db.pool(), "owned", "2026-11-01T00:00:00Z".parse().unwrap())
        .await
        .unwrap();
    assert_eq!(next.total_micros.as_deref(), Some("0"));
    assert_eq!(next.outstanding_calls, Some(0));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT reserved_micros FROM budget_holds")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        4
    );
}
#[tokio::test]
async fn missing_storage_and_null_settlement_never_return_fictitious_zero() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "owned", 100, "USD").await;
    assert_eq!(
        read(db.pool(), "missing", time()).await.unwrap_err(),
        "访问Key不存在或已删除"
    );
    hold(&db, "owned", "settled", 3, None, "usd").await;
    assert_eq!(
        read(db.pool(), "owned", time()).await.unwrap_err(),
        UNAVAILABLE
    );
    sqlx::query("DROP TABLE budget_holds")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        read(db.pool(), "owned", time()).await.unwrap_err(),
        UNAVAILABLE
    );
}
#[tokio::test]
async fn wal_writer_cannot_mix_old_key_metadata_with_new_ledger() {
    let root = std::env::temp_dir().join(format!(
        "llmgw-owned-budget-observation-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root).unwrap();
    let db = Db::connect_path(root.join("owned.sqlite")).await.unwrap();
    key(&db, "owned", 100, "USD").await;
    hold(&db, "owned", "settled", 3, Some(3), "usd").await;
    let arrived = Arc::new(tokio::sync::Notify::new());
    let proceed = Arc::new(tokio::sync::Notify::new());
    let task = tokio::spawn({
        let db = db.clone();
        let pair = (arrived.clone(), proceed.clone());
        async move {
            AFTER_KEY_READ
                .scope(pair, read(db.pool(), "owned", time()))
                .await
                .unwrap()
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), arrived.notified())
        .await
        .unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query("UPDATE remote_access_keys SET monthly_budget_micros=200 WHERE id='owned'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE budget_holds SET reported_reference_micros=30")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    proceed.notify_one();
    let old = task.await.unwrap();
    assert_eq!(old.limit_micros, "100");
    assert_eq!(old.total_micros.as_deref(), Some("3"));
    let current = read(db.pool(), "owned", time()).await.unwrap();
    assert_eq!(current.limit_micros, "200");
    assert_eq!(current.total_micros.as_deref(), Some("30"));
    eprintln!("owned readonly WAL fixture: {}", root.display());
}
