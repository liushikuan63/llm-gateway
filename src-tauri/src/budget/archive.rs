//! Minimal financial evidence. No prompts, errors, request/response JSON or feedback.
//! Legacy archive month follows original audit UTC timestamps; those old rows do
//! not prove an admission timestamp. Independent holds keep actual admission UTC month.
use crate::{
    domain::Currency,
    error::{GatewayError, Result},
};
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::BTreeMap;
#[cfg(test)]
#[path = "archive/tests.rs"]
mod tests;
#[cfg(test)]
tokio::task_local! {pub(crate) static AFTER_DELETE:(std::sync::Arc<tokio::sync::Notify>,std::sync::Arc<tokio::sync::Notify>);}
fn unavailable() -> GatewayError {
    GatewayError::BudgetRejected {
        code: "budget_unavailable",
        reason: "budget_storage_unavailable",
    }
    .no_replay()
}
pub(crate) async fn contract(c: &mut SqliteConnection) -> Result<()> {
    let version: Option<i64> =
        sqlx::query_scalar("SELECT contract_version FROM budget_archive_meta WHERE singleton=1")
            .fetch_optional(c)
            .await
            .map_err(|_| unavailable())?;
    if version != Some(crate::db::budget_archive_schema::VERSION) {
        return Err(unavailable());
    }
    Ok(())
}
async fn coverage(c: &mut SqliteConnection, key: &str, month: i64) -> Result<()> {
    contract(c).await?;
    let first: Option<i64> =
        sqlx::query_scalar("SELECT complete_from_month FROM budget_key_history WHERE key_id=?")
            .bind(key)
            .fetch_optional(c)
            .await
            .map_err(|_| unavailable())?;
    if !first.is_some_and(|v| {
        chrono::DateTime::from_timestamp(v, 0).is_some()
            && super::month_start_secs(v) == v
            && month >= v
    }) {
        return Err(super::holds::rejected("budget_history_incomplete"));
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq)]
struct Amount {
    cost: Option<f64>,
    currency: Option<String>,
}
fn amount(cost: Option<f64>, currency: Option<String>) -> Amount {
    Amount {
        cost: cost.filter(|v| v.is_finite() && *v >= 0.0),
        currency: currency
            .and_then(|v| Currency::parse(&v))
            .map(|c| c.code().into()),
    }
}
pub(super) async fn legacy_total(
    c: &mut SqliteConnection,
    key: &str,
    month: i64,
    currency: &str,
) -> Result<i64> {
    coverage(c, key, month).await?;
    let end = chrono::DateTime::from_timestamp(month, 0)
        .and_then(|v| v.checked_add_months(chrono::Months::new(1)))
        .ok_or_else(unavailable)?
        .timestamp();
    // One SQL snapshot spans both sides of cleanup. Dedup by original id BEFORE
    // rounding, including an identical live+archive overlap; a conflict fails closed.
    let rows=sqlx::query("SELECT r.id AS audit_id,r.cost AS reference_cost,r.currency FROM requests r WHERE r.access_key_id=? AND r.ts>=? AND r.ts<? AND NOT EXISTS(SELECT 1 FROM budget_holds h WHERE h.audit_request_id=r.id) UNION ALL SELECT a.audit_request_id AS audit_id,a.reference_cost,a.currency FROM budget_legacy_archive a WHERE a.key_id=? AND a.admission_month=? AND NOT EXISTS(SELECT 1 FROM budget_holds h WHERE h.audit_request_id=a.audit_request_id) ORDER BY audit_id")
        .bind(key).bind(month).bind(end).bind(key).bind(month).fetch_all(&mut *c).await.map_err(|_|unavailable())?;
    let mut entries = BTreeMap::new();
    for r in rows {
        let id: i64 = r.try_get("audit_id").map_err(|_| unavailable())?;
        let value = amount(
            r.try_get("reference_cost").map_err(|_| unavailable())?,
            r.try_get("currency").map_err(|_| unavailable())?,
        );
        if let Some(old) = entries.insert(id, value.clone()) {
            if old != value {
                return Err(super::holds::rejected("budget_archive_conflict"));
            }
        }
    }
    let mut total = 0.0;
    for v in entries.into_values() {
        let cost = v
            .cost
            .ok_or_else(|| super::holds::rejected("legacy_cost_unknown"))?;
        if cost == 0.0 {
            continue;
        }
        let code = v
            .currency
            .ok_or_else(|| super::holds::rejected("legacy_currency_unknown"))?;
        if code.eq_ignore_ascii_case(currency) {
            total += cost;
        }
    }
    if !total.is_finite() || total * 1e6 >= i64::MAX as f64 {
        return Err(unavailable());
    }
    Ok(super::units_to_micros(total))
}
pub(crate) async fn purge(pool: &SqlitePool, cutoff: i64) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    contract(&mut tx).await?;
    let mut last = 0i64;
    loop {
        // Reuse the audit reader's resource bound; one financial-only page, all
        // pages and final deletion remain in this SAME write transaction.
        let rows=sqlx::query("SELECT r.id,r.access_key_id,r.ts,r.cost,r.currency FROM requests r WHERE r.ts<? AND r.access_key_id IS NOT NULL AND r.id>? AND NOT EXISTS(SELECT 1 FROM budget_holds h WHERE h.audit_request_id=r.id) ORDER BY r.id LIMIT ?")
            .bind(cutoff).bind(last).bind(i64::from(crate::audit::MAX_PAGE_SIZE)).fetch_all(&mut *tx).await?;
        if rows.is_empty() {
            break;
        }
        for r in rows {
            let id: i64 = r.try_get("id")?;
            let key: String = r.try_get("access_key_id")?;
            let ts: i64 = r.try_get("ts")?;
            if chrono::DateTime::from_timestamp(ts, 0).is_none() {
                return Err(unavailable());
            }
            let month = super::month_start_secs(ts);
            // Legacy SQLite cells may predate strict typing. Invalid amount/currency
            // is archived as unknown, never copied as text or coerced to zero.
            let value = amount(
                r.try_get::<Option<f64>, _>("cost").ok().flatten(),
                r.try_get::<Option<String>, _>("currency").ok().flatten(),
            );
            let inserted=sqlx::query("INSERT INTO budget_legacy_archive(audit_request_id,key_id,admission_month,reference_cost,currency) VALUES(?,?,?,?,?) ON CONFLICT(audit_request_id) DO NOTHING")
                .bind(id).bind(&key).bind(month).bind(value.cost).bind(&value.currency).execute(&mut *tx).await?;
            if inserted.rows_affected() == 0 {
                let old=sqlx::query("SELECT key_id,admission_month,reference_cost,currency FROM budget_legacy_archive WHERE audit_request_id=?").bind(id).fetch_one(&mut *tx).await?;
                let previous = amount(old.try_get("reference_cost")?, old.try_get("currency")?);
                if old.try_get::<String, _>("key_id")? != key
                    || old.try_get::<i64, _>("admission_month")? != month
                    || previous != value
                {
                    return Err(super::holds::rejected("budget_archive_conflict"));
                }
            }
            last = id;
        }
    }
    sqlx::query("DELETE FROM requests WHERE ts < ?")
        .bind(cutoff)
        .execute(&mut *tx)
        .await?;
    #[cfg(test)]
    if let Ok((arrived, proceed)) = AFTER_DELETE.try_with(Clone::clone) {
        arrived.notify_one();
        proceed.notified().await;
    }
    tx.commit().await?;
    Ok(())
}
