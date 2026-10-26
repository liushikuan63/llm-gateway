//! Read-only local budget observation. Reuses admission accounting; it does not
//! reserve money, repair history, grant free eligibility or promise next admission.
use crate::{domain::Currency, error::GatewayError};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Row, SqlitePool};
const UNAVAILABLE: &str = "暂时无法读取本机预算账本；未将未知金额视为0";
#[cfg(test)]
tokio::task_local! {static AFTER_KEY_READ:(std::sync::Arc<tokio::sync::Notify>,std::sync::Arc<tokio::sync::Notify>);}
#[cfg(test)]
#[path = "observation/tests.rs"]
mod tests;

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub key_id: String,
    pub key_label: String,
    pub key_enabled: bool,
    pub key_updated_at: DateTime<Utc>,
    pub budget_enabled: bool,
    pub limit_micros: String,
    pub currency: Option<String>,
    pub period_start_utc: DateTime<Utc>,
    pub observed_at_utc: DateTime<Utc>,
    /// Decimal strings preserve i64 micros exactly across JS IPC.
    pub known_spend_micros: Option<String>,
    pub ledger_settled_micros: Option<String>,
    pub outstanding_micros: Option<String>,
    pub outstanding_calls: Option<u64>,
    pub other_currency_outstanding_calls: Option<u64>,
    pub total_micros: Option<String>,
    pub remaining_micros: Option<String>,
    pub blocked_reason: Option<&'static str>,
    pub mode: &'static str,
}
impl Snapshot {
    pub fn with_policy(mut self, enabled: bool) -> Self {
        self.budget_enabled = enabled;
        self.mode = if !self.key_enabled {
            "key_disabled"
        } else if !enabled {
            "disabled"
        } else if self.limit_micros == "0" {
            "unlimited"
        } else if self.blocked_reason.is_some() {
            "blocked_unknown"
        } else if self
            .remaining_micros
            .as_deref()
            .is_some_and(|s| s.starts_with('-') || s == "0")
        {
            "exhausted"
        } else {
            "reference_headroom"
        };
        self
    }
}
fn known_blocker(error: &GatewayError) -> Option<&'static str> {
    match error {
        GatewayError::NoReplay { source } => known_blocker(source),
        GatewayError::BudgetRejected { reason, .. }
            if matches!(
                *reason,
                "budget_history_incomplete"
                    | "budget_archive_conflict"
                    | "legacy_cost_unknown"
                    | "legacy_currency_unknown"
                    | "budget_currency_changed_with_existing_holds"
            ) =>
        {
            Some(*reason)
        }
        _ => None,
    }
}
pub async fn read(
    pool: &SqlitePool,
    id: &str,
    at: DateTime<Utc>,
) -> Result<Snapshot, &'static str> {
    let mut tx = pool.begin().await.map_err(|_| UNAVAILABLE)?;
    // Deliberately excludes key_hash, secrets, prompts and per-call target JSON.
    let key=sqlx::query("SELECT id,label,enabled,monthly_budget_micros,budget_currency,updated_at FROM remote_access_keys WHERE id=?").bind(id).fetch_optional(&mut *tx).await.map_err(|_|UNAVAILABLE)?.ok_or("访问Key不存在或已删除")?;
    #[cfg(test)]
    if let Ok((arrived, proceed)) = AFTER_KEY_READ.try_with(Clone::clone) {
        arrived.notify_one();
        proceed.notified().await;
    }
    let limit: i64 = key
        .try_get("monthly_budget_micros")
        .map_err(|_| UNAVAILABLE)?;
    if limit < 0 {
        return Err(UNAVAILABLE);
    }
    let currency = Currency::parse(
        &key.try_get::<String, _>("budget_currency")
            .map_err(|_| UNAVAILABLE)?,
    )
    .map(|c| c.code().to_owned());
    let month = super::month_start_secs(at.timestamp());
    let mut result = Snapshot {
        key_id: key.try_get("id").map_err(|_| UNAVAILABLE)?,
        key_label: key.try_get("label").map_err(|_| UNAVAILABLE)?,
        key_enabled: key.try_get::<i64, _>("enabled").map_err(|_| UNAVAILABLE)? != 0,
        key_updated_at: key.try_get("updated_at").map_err(|_| UNAVAILABLE)?,
        budget_enabled: false,
        limit_micros: limit.to_string(),
        currency: currency.clone(),
        period_start_utc: DateTime::from_timestamp(month, 0).ok_or(UNAVAILABLE)?,
        observed_at_utc: at,
        known_spend_micros: None,
        ledger_settled_micros: None,
        outstanding_micros: None,
        outstanding_calls: None,
        other_currency_outstanding_calls: None,
        total_micros: None,
        remaining_micros: None,
        blocked_reason: None,
        mode: "disabled",
    };
    if let Some(currency) = currency {
        result.outstanding_calls = Some(0);
        result.other_currency_outstanding_calls = Some(0);
        let total = match super::holds::used(&mut tx, id, month, &currency).await {
            Ok(total) => Some(total),
            Err(error) => {
                result.blocked_reason = Some(known_blocker(&error).ok_or(UNAVAILABLE)?);
                None
            }
        };
        let rows=sqlx::query("SELECT currency,state,reserved_micros,reported_reference_micros FROM budget_holds WHERE key_id=? AND admission_month=? AND state!='released'").bind(id).bind(month).fetch_all(&mut *tx).await.map_err(|_|UNAVAILABLE)?;
        let (mut settled, mut outstanding) = (0i64, 0i64);
        for row in rows {
            let same = row
                .try_get::<String, _>("currency")
                .map_err(|_| UNAVAILABLE)?
                .trim()
                .eq_ignore_ascii_case(&currency);
            let state: String = row.try_get("state").map_err(|_| UNAVAILABLE)?;
            match state.as_str() {
                "settled" if same => {
                    let amount = row
                        .try_get::<Option<i64>, _>("reported_reference_micros")
                        .map_err(|_| UNAVAILABLE)?
                        .filter(|n| *n >= 0)
                        .ok_or(UNAVAILABLE)?;
                    settled = settled.checked_add(amount).ok_or(UNAVAILABLE)?;
                }
                "settled" => {}
                "reserved" | "dispatched" if same => {
                    let amount = row
                        .try_get::<i64, _>("reserved_micros")
                        .map_err(|_| UNAVAILABLE)?;
                    if amount < 0 {
                        return Err(UNAVAILABLE);
                    }
                    outstanding = outstanding.checked_add(amount).ok_or(UNAVAILABLE)?;
                    *result.outstanding_calls.get_or_insert(0) += 1;
                }
                "reserved" | "dispatched" => {
                    *result.other_currency_outstanding_calls.get_or_insert(0) += 1;
                }
                _ => return Err(UNAVAILABLE),
            }
        }
        result.ledger_settled_micros = Some(settled.to_string());
        result.outstanding_micros = Some(outstanding.to_string());
        if let Some(total) = total {
            result.known_spend_micros = Some(
                total
                    .checked_sub(outstanding)
                    .filter(|n| *n >= 0)
                    .ok_or(UNAVAILABLE)?
                    .to_string(),
            );
            result.total_micros = Some(total.to_string());
            if limit > 0 {
                result.remaining_micros =
                    Some(limit.checked_sub(total).ok_or(UNAVAILABLE)?.to_string());
            }
        }
    } else if limit > 0 {
        result.blocked_reason = Some("budget_currency_unknown");
    }
    tx.commit().await.map_err(|_| UNAVAILABLE)?;
    result.observed_at_utc = Utc::now();
    Ok(result)
}
