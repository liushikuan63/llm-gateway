use super::*;
use crate::{
    db::{repo, Db},
    domain::RemoteAccessKey,
};
use chrono::{DateTime, Utc};
fn at() -> DateTime<Utc> {
    "2026-10-20T12:00:00Z".parse().unwrap()
}
async fn key(db: &Db, id: &str) {
    repo::create_remote_access_key(
        db.pool(),
        &RemoteAccessKey {
            id: id.into(),
            label: "owned archive".into(),
            key_hash: format!("owned-hash-{id}"),
            enabled: true,
            rpm_limit: 20,
            monthly_budget_micros: 10_000_000,
            budget_currency: "USD".into(),
            allowed_models: vec![],
            created_at: at(),
            updated_at: at(),
        },
    )
    .await
    .unwrap();
}
async fn row(db: &Db, key: &str, ts: i64, cost: Option<f64>, currency: Option<&str>) -> i64 {
    sqlx::query_scalar("INSERT INTO requests(ts,requested_model,access_key_id,cost,currency,refined_prompt,error,attempts_json) VALUES(?,'owned',?,?,?,'owned private prompt','owned private diagnostic','owned private detail') RETURNING id")
        .bind(ts).bind(key).bind(cost).bind(currency).fetch_one(db.pool()).await.unwrap()
}
async fn used(db: &Db, key: &str, month: i64, currency: &str) -> Result<i64> {
    let mut c = db.pool().acquire().await?;
    crate::budget::holds::used(&mut c, key, month, currency).await
}
async fn count(db: &Db, table: &str) -> i64 {
    assert!(matches!(
        table,
        "requests" | "request_feedback" | "budget_legacy_archive" | "budget_holds"
    ));
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(db.pool())
        .await
        .unwrap()
}
async fn legacy_schema(db: &Db) {
    sqlx::raw_sql("DROP TABLE budget_archive_meta;DROP TABLE budget_legacy_archive;DROP TABLE budget_key_history;").execute(db.pool()).await.unwrap();
}

#[tokio::test]
async fn mixed_live_archive_ledger_currency_and_aggregate_rounding_are_identical_after_cleanup() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "k").await;
    let m = crate::budget::month_start_secs(at().timestamp());
    row(&db, "k", m + 1, Some(0.0000004), Some("USD")).await;
    row(&db, "k", m + 20, Some(0.0000004), Some("usd")).await;
    row(&db, "k", m + 1, Some(2.0), Some("CNY")).await;
    let linked = row(&db, "k", m + 1, Some(999.0), Some("USD")).await;
    sqlx::query("INSERT INTO budget_holds(hold_id,gateway_request_uuid,call_ordinal,key_id,admission_month,currency,state,reserved_micros,reported_reference_micros,target_price_json,audit_request_id,created_at,updated_at) VALUES('owned','owned',0,'k',?,'usd','settled',2,2,'owned',?,0,0)").bind(m).bind(linked).execute(db.pool()).await.unwrap();
    assert_eq!(used(&db, "k", m, "usd").await.unwrap(), 3);
    assert_eq!(used(&db, "k", m, "cny").await.unwrap(), 2_000_000);
    purge(db.pool(), m + 10).await.unwrap();
    assert_eq!(count(&db, "requests").await, 1);
    assert_eq!(count(&db, "budget_legacy_archive").await, 2);
    assert_eq!(used(&db, "k", m, "usd").await.unwrap(), 3);
    assert_eq!(used(&db, "k", m, "cny").await.unwrap(), 2_000_000);
    let link: Option<i64> = sqlx::query_scalar("SELECT audit_request_id FROM budget_holds")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(link, None);
    purge(db.pool(), m + 10).await.unwrap();
    assert_eq!(count(&db, "budget_legacy_archive").await, 2);
    assert_eq!(used(&db, "k", m, "usd").await.unwrap(), 3);
    let names: Vec<String> = sqlx::query("PRAGMA table_info(budget_legacy_archive)")
        .fetch_all(db.pool())
        .await
        .unwrap()
        .iter()
        .map(|r| r.get("name"))
        .collect();
    assert_eq!(
        names,
        [
            "audit_request_id",
            "key_id",
            "admission_month",
            "reference_cost",
            "currency"
        ]
    );
}

#[tokio::test]
async fn unknown_cost_currency_and_known_zero_keep_their_meaning_after_content_deletion() {
    let m = crate::budget::month_start_secs(at().timestamp());
    for (cost, currency, reason) in [
        (None, Some("USD"), Some("legacy_cost_unknown")),
        (Some(-1.0), Some("USD"), Some("legacy_cost_unknown")),
        (
            Some(f64::INFINITY),
            Some("CNY"),
            Some("legacy_cost_unknown"),
        ),
        (Some(1.0), Some("JPY"), Some("legacy_currency_unknown")),
        (Some(0.0), None, None),
    ] {
        let db = Db::connect_in_memory().await.unwrap();
        key(&db, "k").await;
        row(&db, "k", m + 1, cost, currency).await;
        let before = crate::budget::observation::read(db.pool(), "k", at())
            .await
            .unwrap();
        purge(db.pool(), m + 2).await.unwrap();
        let after = crate::budget::observation::read(db.pool(), "k", at())
            .await
            .unwrap();
        assert_eq!(before.total_micros, after.total_micros);
        assert_eq!(after.blocked_reason, reason);
        assert_eq!(count(&db, "requests").await, 0);
        assert_eq!(count(&db, "budget_legacy_archive").await, 1);
        if reason.is_some() {
            assert!(after.total_micros.is_none());
        } else {
            assert_eq!(after.total_micros.as_deref(), Some("0"));
        }
    }
}

#[tokio::test]
async fn malformed_legacy_numeric_storage_becomes_unknown_finance_without_retaining_content() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "k").await;
    let m = crate::budget::month_start_secs(at().timestamp());
    row(&db, "k", m + 1, None, Some("usd")).await;
    sqlx::query("UPDATE requests SET cost='owned nonnumeric legacy cost'")
        .execute(db.pool())
        .await
        .unwrap();
    purge(db.pool(), m + 2)
        .await
        .expect("malformed legacy amount must archive as unknown, not retain expired content");
    assert_eq!(count(&db, "requests").await, 0);
    let value: Option<f64> = sqlx::query_scalar("SELECT reference_cost FROM budget_legacy_archive")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(value, None);
    let snapshot = crate::budget::observation::read(db.pool(), "k", at())
        .await
        .unwrap();
    assert_eq!(snapshot.total_micros, None);
    assert_eq!(snapshot.blocked_reason, Some("legacy_cost_unknown"));
}

#[tokio::test]
async fn failed_delete_rolls_back_finance_and_content_then_retry_commits_once() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "k").await;
    let m = crate::budget::month_start_secs(at().timestamp());
    let id = row(&db, "k", m + 1, Some(1.0), Some("usd")).await;
    sqlx::query("INSERT INTO request_feedback(request_id,revision,source,created_at,updated_at) VALUES(?,1,'local_user',1,1)").bind(id).execute(db.pool()).await.unwrap();
    sqlx::raw_sql("CREATE TRIGGER owned_delete_fault BEFORE DELETE ON requests BEGIN SELECT RAISE(ABORT,'owned deletion fault');END;").execute(db.pool()).await.unwrap();
    assert!(purge(db.pool(), m + 2).await.is_err());
    assert_eq!(count(&db, "budget_legacy_archive").await, 0);
    assert_eq!(count(&db, "requests").await, 1);
    assert_eq!(count(&db, "request_feedback").await, 1);
    assert_eq!(used(&db, "k", m, "usd").await.unwrap(), 1_000_000);
    sqlx::query("DROP TRIGGER owned_delete_fault")
        .execute(db.pool())
        .await
        .unwrap();
    purge(db.pool(), m + 2).await.unwrap();
    purge(db.pool(), m + 2).await.unwrap();
    assert_eq!(count(&db, "requests").await, 0);
    assert_eq!(count(&db, "request_feedback").await, 0);
    assert_eq!(count(&db, "budget_legacy_archive").await, 1);
    assert_eq!(used(&db, "k", m, "usd").await.unwrap(), 1_000_000);
}

#[tokio::test]
async fn identical_overlap_deduplicates_but_conflicting_financial_identity_rejects() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "k").await;
    let m = crate::budget::month_start_secs(at().timestamp());
    let id = row(&db, "k", m + 1, Some(1.0), Some("USD")).await;
    sqlx::query("INSERT INTO budget_legacy_archive VALUES(?, 'k',?,1.0,'usd')")
        .bind(id)
        .bind(m)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(used(&db, "k", m, "usd").await.unwrap(), 1_000_000);
    sqlx::query("UPDATE budget_legacy_archive SET reference_cost=2.0")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(used(&db, "k", m, "usd").await.is_err());
    assert!(purge(db.pool(), m + 2).await.is_err());
    assert_eq!(count(&db, "requests").await, 1);
}

#[tokio::test]
async fn legacy_migration_fences_current_month_reopen_is_idempotent_and_future_version_refuses() {
    let runtime_db = Db::connect_in_memory().await.unwrap();
    let runtime: (String, String) = sqlx::query_as("SELECT sqlite_version(),sqlite_source_id()")
        .fetch_one(runtime_db.pool())
        .await
        .unwrap();
    eprintln!(
        "OWNED_SQLITE_RUNTIME {}",
        serde_json::json!({"version":runtime.0,"source_id":runtime.1})
    );
    runtime_db.pool().close().await;
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "old").await;
    legacy_schema(&db).await;
    let now = Utc::now();
    let m = crate::budget::month_start_secs(now.timestamp());
    let next = chrono::DateTime::from_timestamp(m, 0)
        .unwrap()
        .checked_add_months(chrono::Months::new(1))
        .unwrap()
        .timestamp();
    crate::db::migrations::run(db.pool()).await.unwrap();
    assert!(used(&db, "old", m, "usd").await.is_err());
    let snapshot = crate::budget::observation::read(db.pool(), "old", now)
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(snapshot.blocked_reason, Some("budget_history_incomplete"));
    assert!(snapshot.total_micros.is_none());
    assert_eq!(snapshot.mode, "blocked_unknown");
    let first: i64 =
        sqlx::query_scalar("SELECT complete_from_month FROM budget_key_history WHERE key_id='old'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(first, next);
    crate::db::budget_archive_schema::migrate_at(db.pool(), next + 86400)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT complete_from_month FROM budget_key_history WHERE key_id='old'"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        first
    );
    assert_eq!(used(&db, "old", next, "usd").await.unwrap(), 0);
    key(&db, "new").await;
    assert_eq!(used(&db, "new", m, "usd").await.unwrap(), 0);
    sqlx::query("UPDATE budget_archive_meta SET contract_version=2")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(crate::db::migrations::run(db.pool()).await.is_err());
    assert!(used(&db, "new", m, "usd").await.is_err());
}

#[tokio::test]
async fn failed_atomic_schema_activation_leaves_legacy_data_and_no_half_contract() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "old").await;
    let m = crate::budget::month_start_secs(at().timestamp());
    row(&db, "old", m + 1, Some(1.0), Some("usd")).await;
    legacy_schema(&db).await;
    sqlx::query("CREATE TABLE idx_budget_archive_key_month(id INTEGER)")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(crate::db::migrations::run(db.pool()).await.is_err());
    let n:i64=sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name IN('budget_archive_meta','budget_legacy_archive','budget_key_history')").fetch_one(db.pool()).await.unwrap();
    assert_eq!(n, 0);
    assert_eq!(count(&db, "requests").await, 1);
    sqlx::query("DROP TABLE idx_budget_archive_key_month")
        .execute(db.pool())
        .await
        .unwrap();
    crate::db::migrations::run(db.pool()).await.unwrap();
    sqlx::query("DELETE FROM budget_archive_meta")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(crate::db::migrations::run(db.pool()).await.is_err());
}

#[tokio::test]
async fn archive_survives_utc_month_rollover_and_old_reader_is_not_a_supported_downgrade() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "k").await;
    let m = crate::budget::month_start_secs(at().timestamp());
    row(&db, "k", m + 1, Some(1.0), Some("usd")).await;
    purge(db.pool(), m + 2).await.unwrap();
    let old:i64=sqlx::query_scalar("SELECT COALESCE(CAST(ROUND(SUM(cost)*1000000) AS INTEGER),0) FROM requests WHERE access_key_id='k'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(old, 0, "negative proof: old reader ignores the new archive");
    assert_eq!(used(&db, "k", m, "usd").await.unwrap(), 1_000_000);
    sqlx::query("INSERT INTO budget_holds(hold_id,gateway_request_uuid,call_ordinal,key_id,admission_month,currency,state,reserved_micros,target_price_json,created_at,updated_at) VALUES('pending','pending',0,'k',?,'usd','dispatched',7,'owned',0,0)").bind(m).execute(db.pool()).await.unwrap();
    let next = chrono::DateTime::from_timestamp(m, 0)
        .unwrap()
        .checked_add_months(chrono::Months::new(1))
        .unwrap()
        .timestamp();
    purge(db.pool(), next).await.unwrap();
    assert_eq!(used(&db, "k", m, "usd").await.unwrap(), 1_000_007);
    assert_eq!(used(&db, "k", next, "usd").await.unwrap(), 0);
    assert_eq!(count(&db, "budget_holds").await, 1);
    assert_eq!(count(&db, "budget_legacy_archive").await, 1);
    let state: String = sqlx::query_scalar("SELECT state FROM budget_holds")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        state, "dispatched",
        "new month does not refund or erase unknown old delivery"
    );
}

#[tokio::test]
async fn utc_year_and_leap_months_keep_separate_financial_periods_after_content_expiry() {
    let db = Db::connect_in_memory().await.unwrap();
    key(&db, "k").await;
    let dates = [
        "2026-12-31T23:59:59Z",
        "2027-01-01T00:00:00Z",
        "2028-02-29T23:59:59Z",
        "2028-03-01T00:00:00Z",
    ];
    let mut periods = vec![];
    let mut last = 0;
    for date in dates {
        let date: DateTime<Utc> = date.parse().unwrap();
        let ts = date.timestamp();
        periods.push(crate::budget::month_start_secs(ts));
        row(&db, "k", ts, Some(1.0), Some("USD")).await;
        last = ts;
    }
    purge(db.pool(), last + 1).await.unwrap();
    assert_eq!(count(&db, "requests").await, 0);
    assert_eq!(count(&db, "budget_legacy_archive").await, 4);
    let stored: Vec<i64> = sqlx::query_scalar(
        "SELECT admission_month FROM budget_legacy_archive ORDER BY audit_request_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(stored, periods);
    for month in periods {
        assert_eq!(used(&db, "k", month, "usd").await.unwrap(), 1_000_000);
        assert_eq!(used(&db, "k", month, "cny").await.unwrap(), 0);
    }
}

#[tokio::test]
async fn new_key_and_history_origin_are_atomic_and_limit_toggle_cannot_repair_old_history() {
    let db = Db::connect_in_memory().await.unwrap();
    let value = RemoteAccessKey {
        id: "new".into(),
        label: "owned new".into(),
        key_hash: "owned-new-hash".into(),
        enabled: true,
        rpm_limit: 20,
        monthly_budget_micros: 10,
        budget_currency: "USD".into(),
        allowed_models: vec![],
        created_at: at(),
        updated_at: at(),
    };
    sqlx::raw_sql("CREATE TRIGGER owned_history_fault BEFORE INSERT ON budget_key_history BEGIN SELECT RAISE(ABORT,'owned history insert fault');END;").execute(db.pool()).await.unwrap();
    assert!(repo::create_remote_access_key(db.pool(), &value)
        .await
        .is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM remote_access_keys")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER owned_history_fault")
        .execute(db.pool())
        .await
        .unwrap();
    repo::create_remote_access_key(db.pool(), &value)
        .await
        .unwrap();
    let m = crate::budget::month_start_secs(at().timestamp());
    assert_eq!(used(&db, "new", m, "usd").await.unwrap(), 0);
    let next = chrono::DateTime::from_timestamp(m, 0)
        .unwrap()
        .checked_add_months(chrono::Months::new(1))
        .unwrap()
        .timestamp();
    sqlx::query("UPDATE budget_key_history SET complete_from_month=?")
        .bind(next)
        .execute(db.pool())
        .await
        .unwrap();
    repo::update_remote_access_key(db.pool(), "new", "owned", true, 20, (0, "USD", &[]))
        .await
        .unwrap();
    repo::update_remote_access_key(db.pool(), "new", "owned", true, 20, (10, "USD", &[]))
        .await
        .unwrap();
    assert!(used(&db, "new", m, "usd").await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT complete_from_month FROM budget_key_history")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        next
    );
}

#[path = "tests_recovery.rs"]
mod recovery;
