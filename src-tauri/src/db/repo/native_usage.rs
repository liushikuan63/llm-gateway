//! Shared consumers of the approved, bounded ledger projector. No current price lookup.
use std::collections::BTreeMap;

use crate::audit::native_usage::{self, UsageAccumulator, UsageProjection};
use crate::error::Result;
use futures_util::TryStreamExt;
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};

pub(super) fn projection(raw: Option<&str>, ts: i64) -> UsageProjection {
    raw.map_or(UsageProjection::Legacy, |raw| {
        native_usage::project_json(raw, ts)
    })
}

pub(super) struct RowUsage {
    pub prompt: Option<i64>,
    pub completion: Option<i64>,
    pub cost: Option<f64>,
    pub currency: Option<String>,
    pub rate_label: Option<String>,
    pub native: Option<Value>,
}

impl RowUsage {
    pub(super) fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        let raw: Option<String> = row.get("attempts_json");
        let projected = projection(raw.as_deref(), row.get("ts"));
        match projected.native() {
            Some(usage) => Self {
                prompt: usage.prompt_tokens.map(i64::from),
                completion: usage.completion_tokens.map(i64::from),
                cost: None,
                currency: None,
                rate_label: None,
                native: Some(json!(usage)),
            },
            None => Self {
                prompt: Some(row.get("prompt_tokens")),
                completion: Some(row.get("completion_tokens")),
                cost: row.get("cost"),
                currency: row.get("currency"),
                rate_label: row.get("rate_label"),
                native: None,
            },
        }
    }
}

#[derive(Default)]
pub(crate) struct UsageTotals {
    pub prompt: i64,
    pub completion: i64,
    pub usage: UsageAccumulator,
}

impl UsageTotals {
    fn add(&mut self, projected: &UsageProjection, prompt: i64, completion: i64) {
        // Compatibility scalars are known subtotals. Unknown coverage is retained
        // by the accumulator below, never interpreted as an observed zero.
        let (prompt, completion) = projected.native().map_or((prompt, completion), |usage| {
            (
                i64::from(usage.prompt_tokens.unwrap_or(0)),
                i64::from(usage.completion_tokens.unwrap_or(0)),
            )
        });
        self.prompt = self.prompt.saturating_add(prompt);
        self.completion = self.completion.saturating_add(completion);
        self.usage.add(projected);
    }

    pub(crate) fn native_json(&self) -> Option<Value> {
        (self.usage.native_requests > 0).then(|| json!(self.usage))
    }
}

#[derive(Default)]
pub(crate) struct RollingUsage {
    pub totals: UsageTotals,
    pub providers: BTreeMap<Option<String>, UsageTotals>,
    models: BTreeMap<(Option<String>, String, Option<String>), UsageAccumulator>,
}

impl RollingUsage {
    pub(crate) fn models_json(&self) -> Option<Value> {
        if self.totals.usage.native_requests == 0 {
            return None;
        }
        Some(Value::Array(
            self.models
                .iter()
                .filter(|(_, usage)| usage.native_requests > 0)
                .map(|((id, provider, model), usage)| {
                    json!({
                        "provider_id":id,"provider":provider,"model":model,"usage":usage
                    })
                })
                .collect(),
        ))
    }
}

/// Constant row memory; only distinct provider/model groups survive each iteration.
/// `since` is supplied by the caller so its SQL metrics and coverage share a cutoff.
pub(crate) async fn rolling_usage(pool: &SqlitePool, since: i64) -> Result<RollingUsage> {
    let mut result = RollingUsage::default();
    let mut rows = sqlx::query(
        "SELECT r.ts, r.attempts_json, r.prompt_tokens, r.completion_tokens, \
         r.routed_provider, r.routed_model, \
         COALESCE(p.name, r.routed_provider, '未路由') AS provider \
         FROM requests r LEFT JOIN providers p ON p.id = r.routed_provider \
         WHERE r.ts >= ? ORDER BY r.id",
    )
    .bind(since)
    .fetch(pool);
    while let Some(row) = rows.try_next().await? {
        let raw: Option<String> = row.get("attempts_json");
        let projected = projection(raw.as_deref(), row.get("ts"));
        let prompt = row.get("prompt_tokens");
        let completion = row.get("completion_tokens");
        let provider_id: Option<String> = row.get("routed_provider");
        result.totals.add(&projected, prompt, completion);
        result
            .providers
            .entry(provider_id.clone())
            .or_default()
            .add(&projected, prompt, completion);
        result
            .models
            .entry((provider_id, row.get("provider"), row.get("routed_model")))
            .or_default()
            .add(&projected);
    }
    Ok(result)
}
