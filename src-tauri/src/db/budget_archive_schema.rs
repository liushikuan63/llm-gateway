//! B archive contract is activated atomically, never inferred from surviving logs.
use sqlx::{Row, SqliteConnection, SqlitePool};
pub(crate) const VERSION: i64 = 1;
pub(crate) const TABLES:&str="CREATE TABLE budget_archive_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),contract_version INTEGER NOT NULL,activated_at INTEGER NOT NULL);
CREATE TABLE budget_legacy_archive(audit_request_id INTEGER PRIMARY KEY CHECK(audit_request_id>0),key_id TEXT NOT NULL,admission_month INTEGER NOT NULL,reference_cost REAL CHECK(reference_cost IS NULL OR reference_cost>=0),currency TEXT CHECK(currency IS NULL OR currency IN ('usd','cny')));
CREATE INDEX idx_budget_archive_key_month ON budget_legacy_archive(key_id,admission_month,audit_request_id);
CREATE TABLE budget_key_history(key_id TEXT PRIMARY KEY,complete_from_month INTEGER NOT NULL);";

async fn inspect(c: &mut SqliteConnection) -> anyhow::Result<bool> {
    let n:i64=sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('budget_archive_meta','budget_legacy_archive','budget_key_history')").fetch_one(&mut *c).await?;
    if n == 0 {
        return Ok(false);
    }
    anyhow::ensure!(
        n == 3,
        "budget_archive_partial_contract: archive requires forward repair, not fallback"
    );
    let rows =
        sqlx::query("SELECT singleton,contract_version,activated_at FROM budget_archive_meta")
            .fetch_all(&mut *c)
            .await?;
    anyhow::ensure!(
        rows.len() == 1 && rows[0].try_get::<i64, _>("singleton")? == 1,
        "budget_archive_invalid_marker"
    );
    anyhow::ensure!(
        rows[0].try_get::<i64, _>("contract_version")? == VERSION,
        "budget_archive_contract_unsupported: direct downgrade is not supported"
    );
    anyhow::ensure!(
        chrono::DateTime::from_timestamp(rows[0].try_get("activated_at")?, 0).is_some(),
        "budget_archive_invalid_activation"
    );
    sqlx::query("SELECT audit_request_id,key_id,admission_month,reference_cost,currency FROM budget_legacy_archive LIMIT 0").execute(&mut *c).await?;
    sqlx::query("SELECT key_id,complete_from_month FROM budget_key_history LIMIT 0")
        .execute(&mut *c)
        .await?;
    Ok(true)
}
pub(super) async fn preflight(pool: &SqlitePool) -> anyhow::Result<()> {
    let mut c = pool.acquire().await?;
    inspect(&mut c).await?;
    Ok(())
}
pub(super) async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    migrate_at(pool, chrono::Utc::now().timestamp()).await
}
pub(crate) async fn migrate_at(pool: &SqlitePool, now: i64) -> anyhow::Result<()> {
    let stamp = chrono::DateTime::from_timestamp(now, 0)
        .ok_or_else(|| anyhow::anyhow!("budget_archive_invalid_clock"))?;
    let month = crate::budget::month_start_secs(stamp.timestamp());
    let next = chrono::DateTime::from_timestamp(month, 0)
        .and_then(|v| v.checked_add_months(chrono::Months::new(1)))
        .ok_or_else(|| anyhow::anyhow!("budget_archive_invalid_month"))?
        .timestamp();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    if !inspect(&mut tx).await? {
        sqlx::raw_sql(TABLES).execute(&mut *tx).await?;
        // Remaining rows cannot prove a pre-B key's complete current-month history.
        sqlx::query("INSERT INTO budget_key_history(key_id,complete_from_month) SELECT id,? FROM remote_access_keys").bind(next).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO budget_archive_meta(singleton,contract_version,activated_at) VALUES(1,?,?)").bind(VERSION).bind(now).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
