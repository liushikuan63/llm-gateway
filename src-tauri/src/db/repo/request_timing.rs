//! Read-only bounded timing statistics; no inference from legacy total latency.
use crate::error::Result;
use crate::proxy::control_modes::WireModeSnapshot;
use serde::Serialize;
use sqlx::SqlitePool;
mod generation_summary;

pub(super) fn normalize_generation_reports(attempts: &mut [serde_json::Value]) {
    generation_summary::normalize_reports(attempts);
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MetricSummary {
    pub samples: u64,
    pub retained_samples: u64,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub complete: bool,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TimingMetrics {
    pub response_headers: MetricSummary,
    pub first_byte: MetricSummary,
    pub first_text: MetricSummary,
    pub first_reasoning: MetricSummary,
    pub first_refusal: MetricSummary,
    pub first_tool: MetricSummary,
    pub finished: MetricSummary,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TimingGroup {
    pub provider_id: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub mode: String,
    pub outcome: String,
    pub input_band: String,
    pub reasoning_event: String,
    pub tool_event: String,
    pub wire_modes_state: String,
    pub wire_modes: Option<WireModeSnapshot>,
    pub observation_context_state: String,
    pub observation_context: Option<crate::proxy::observation_context::Snapshot>,
    pub attempts: u64,
    pub metrics: TimingMetrics,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TimingOverview {
    pub schema_version: u8,
    pub generation_reports: generation_summary::Overview,
    pub window: String,
    pub since_ts: i64,
    pub until_ts: i64,
    pub total_requests: u64,
    pub observed_requests: u64,
    pub unobserved_requests: u64,
    pub no_details_requests: u64,
    pub invalid_requests: u64,
    pub valid_attempts: u64,
    pub observed_control_attempts: u64,
    pub unobserved_control_attempts: u64,
    pub invalid_control_attempts: u64,
    pub unobserved_attempts: u64,
    pub invalid_attempts: u64,
    pub ungrouped_attempts: u64,
    pub group_limit_reached: bool,
    pub sample_limit_reached: bool,
    pub groups: Vec<TimingGroup>,
    #[serde(skip)]
    timing_metadata_seen: bool,
}
impl TimingOverview {
    fn empty(since: i64) -> Self {
        Self {
            schema_version: 1,
            generation_reports: Default::default(),
            window: "24h".into(),
            since_ts: since,
            until_ts: since.saturating_add(86400),
            total_requests: 0,
            observed_requests: 0,
            unobserved_requests: 0,
            no_details_requests: 0,
            invalid_requests: 0,
            valid_attempts: 0,
            observed_control_attempts: 0,
            unobserved_control_attempts: 0,
            invalid_control_attempts: 0,
            unobserved_attempts: 0,
            invalid_attempts: 0,
            ungrouped_attempts: 0,
            group_limit_reached: false,
            sample_limit_reached: false,
            groups: vec![],
            timing_metadata_seen: false,
        }
    }
    pub(crate) fn has_timing_metadata(&self) -> bool {
        self.timing_metadata_seen
    }
}
#[derive(Clone, Copy)]
struct Limits {
    max_groups: usize,
    max_samples: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_groups: 512,
            max_samples: 10000,
        }
    }
}
pub(crate) async fn rolling(pool: &SqlitePool, since: i64) -> Result<TimingOverview> {
    rolling_with_limits(pool, since, Limits::default()).await
}
// Resource limits govern observation storage only, never routing or cost policy.
const MAX_RAW_BYTES: i64 = 65_536;
const MAX_ROW_ATTEMPTS: usize = 64;
const MAX_LABEL_BYTES: usize = 256;

#[derive(Default)]
struct MetricAccumulator {
    samples: u64,
    values: Vec<u64>,
    lost: bool,
}
impl MetricAccumulator {
    fn add(&mut self, value: Option<u64>, retain: bool) {
        if let Some(value) = value {
            self.samples = self.samples.saturating_add(1);
            if retain {
                self.values.push(value);
            } else {
                self.lost = true;
            }
        }
    }
    fn finish(mut self) -> MetricSummary {
        self.values.sort_unstable();
        let percentile = |numerator: usize| {
            if self.lost || self.values.is_empty() {
                return None;
            }
            let rank = (self.values.len() * numerator).div_ceil(100);
            self.values.get(rank - 1).copied()
        };
        MetricSummary {
            samples: self.samples,
            retained_samples: self.values.len() as u64,
            p50_ms: percentile(50),
            p95_ms: percentile(95),
            complete: !self.lost,
        }
    }
}
// Control validity is independent of numeric validity. Corrupt metadata cannot
// hide a valid timing sample or donate untrusted grouping labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ControlObservation {
    Unknown,
    Invalid,
    Observed(WireModeSnapshot),
}
impl ControlObservation {
    fn from_attempt(attempt: &serde_json::Value) -> Self {
        match attempt.get("wire_modes") {
            None | Some(serde_json::Value::Null) => Self::Unknown,
            Some(raw) => WireModeSnapshot::project(raw)
                .map(Self::Observed)
                .unwrap_or(Self::Invalid),
        }
    }
    fn state(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Invalid => "invalid",
            Self::Observed(_) => "observed",
        }
    }
    fn value(self) -> Option<WireModeSnapshot> {
        match self {
            Self::Observed(value) => Some(value),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ContextObservation {
    Unknown,
    Invalid,
    Observed(crate::proxy::observation_context::Snapshot),
}
impl ContextObservation {
    fn from_attempt(a: &serde_json::Value) -> Self {
        match a.get("observation_context") {
            None | Some(serde_json::Value::Null) => Self::Unknown,
            Some(raw) => match crate::proxy::observation_context::Snapshot::project(raw) {
                Some(s)
                    if a.get("provider_id").and_then(serde_json::Value::as_str)
                        == Some(s.provider_id.as_str()) =>
                {
                    Self::Observed(s)
                }
                _ => Self::Invalid,
            },
        }
    }
    fn state(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Invalid => "invalid",
            Self::Observed(_) => "observed",
        }
    }
    fn value(&self) -> Option<crate::proxy::observation_context::Snapshot> {
        match self {
            Self::Observed(s) => Some(s.clone()),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    provider_id: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    mode: &'static str,
    outcome: &'static str,
    input_band: &'static str,
    reasoning_event: &'static str,
    tool_event: &'static str,
    controls: ControlObservation,
    context: ContextObservation,
}
#[derive(Default)]
struct GroupAccumulator {
    attempts: u64,
    metrics: [MetricAccumulator; 7],
}
fn label(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= MAX_LABEL_BYTES)
        .map(str::to_owned)
}
fn input_band(estimate: Option<i64>) -> &'static str {
    match estimate {
        Some(0..=4095) => "under_4k",
        Some(4096..=32767) => "4k_to_32k",
        Some(32768..=131071) => "32k_to_128k",
        Some(131072..) => "128k_plus",
        _ => "unknown",
    }
}
fn key(
    attempt: &serde_json::Value,
    timing: &crate::proxy::timing::TimingSnapshot,
    estimate: Option<i64>,
) -> GroupKey {
    use crate::proxy::timing::{TimingMode, TimingTerminal};
    let outcome = match timing.terminal {
        Some(TimingTerminal::Completed) => {
            match attempt.get("ok").and_then(serde_json::Value::as_bool) {
                Some(true) => "completed",
                Some(false) => "attempt_failed",
                None => "outcome_unknown",
            }
        }
        Some(TimingTerminal::Failed) => "failed",
        Some(TimingTerminal::Cancelled) => "cancelled",
        Some(TimingTerminal::Incomplete) => "incomplete",
        None => "pending",
    };
    GroupKey {
        provider_id: label(attempt.get("provider_id")),
        provider: label(attempt.get("provider")),
        model: label(attempt.get("model")),
        mode: match timing.mode {
            TimingMode::Streaming => "streaming",
            TimingMode::Buffered => "buffered",
        },
        outcome,
        input_band: input_band(estimate),
        controls: ControlObservation::from_attempt(attempt),
        context: ContextObservation::from_attempt(attempt),
        reasoning_event: if timing.first_reasoning_ms.is_some() {
            "observed"
        } else {
            "not_observed"
        },
        tool_event: if timing.first_tool_ms.is_some() {
            "observed"
        } else {
            "not_observed"
        },
    }
}
async fn rolling_with_limits(
    pool: &SqlitePool,
    since: i64,
    limits: Limits,
) -> Result<TimingOverview> {
    use crate::proxy::timing::TimingSnapshot;
    use futures_util::TryStreamExt;
    use serde_json::Value;
    use sqlx::Row;
    use std::collections::BTreeMap;
    let until = since.checked_add(86400).ok_or_else(|| {
        crate::error::GatewayError::Protocol("request_timing_window_invalid".into())
    })?;
    let mut result = TimingOverview::empty(since);
    let mut groups: BTreeMap<GroupKey, GroupAccumulator> = BTreeMap::new();
    let mut retained_attempts = 0usize;
    let mut generation = generation_summary::Accumulator::default();
    let mut generation_requests = 0u64;
    // SQL bounds bytes/types BEFORE SQLx allocates a String. A filtered NULL is
    // kept distinct from an original NULL; unknown audit payloads are not read.
    let mut rows = sqlx::query("SELECT estimated_prompt_tokens, CASE WHEN typeof(attempts_json)='text' AND length(CAST(attempts_json AS BLOB))<=? THEN attempts_json ELSE NULL END AS bounded_raw, CASE WHEN attempts_json IS NOT NULL AND (typeof(attempts_json)!='text' OR length(CAST(attempts_json AS BLOB))>?) THEN 1 ELSE 0 END AS invalid_raw FROM requests WHERE ts>=? AND ts<=? ORDER BY id")
        .bind(MAX_RAW_BYTES).bind(MAX_RAW_BYTES).bind(since).bind(until).fetch(pool);
    while let Some(row) = rows.try_next().await? {
        result.total_requests = result.total_requests.saturating_add(1);
        if row.get::<i64, _>("invalid_raw") != 0 {
            result.invalid_requests += 1;
            generation.invalid();
            continue;
        }
        let raw = match row.try_get::<Option<String>, _>("bounded_raw") {
            Ok(Some(raw)) => raw,
            Ok(None) => {
                result.unobserved_requests += 1;
                generation.unavailable();
                continue;
            }
            Err(_) => {
                result.invalid_requests += 1;
                generation.invalid();
                continue;
            }
        };
        let value = match serde_json::from_str::<Value>(&raw) {
            Ok(value) => value,
            Err(_) => {
                result.invalid_requests += 1;
                generation.invalid();
                continue;
            }
        };
        let Some(attempts) = value.as_array().filter(|a| a.len() <= MAX_ROW_ATTEMPTS) else {
            result.invalid_requests += 1;
            generation.invalid();
            continue;
        };
        if attempts.is_empty() {
            result.no_details_requests += 1;
            generation.empty();
            continue;
        }
        let estimate = row
            .try_get::<Option<i64>, _>("estimated_prompt_tokens")
            .ok()
            .flatten();
        generation.inspected();
        let mut usable_generation = false;
        let mut observed = false;
        let mut invalid = false;
        for attempt in attempts {
            usable_generation |= generation.observe(attempt, estimate, limits);
            let Some(object) = attempt.as_object() else {
                result.invalid_attempts += 1;
                invalid = true;
                continue;
            };
            if object.contains_key("native_exact")
                || object.get("runtime_kind").is_some_and(|v| !v.is_null())
            {
                result.unobserved_attempts += 1;
                continue;
            }
            let Some(raw_timing) = object.get("timing").filter(|v| !v.is_null()) else {
                result.unobserved_attempts += 1;
                continue;
            };
            result.timing_metadata_seen = true;
            let Some(timing) = TimingSnapshot::project(raw_timing) else {
                result.invalid_attempts += 1;
                invalid = true;
                continue;
            };
            result.valid_attempts += 1;
            observed = true;
            let group_key = key(attempt, &timing, estimate);
            match group_key.controls {
                ControlObservation::Observed(_) => result.observed_control_attempts += 1,
                ControlObservation::Unknown => result.unobserved_control_attempts += 1,
                ControlObservation::Invalid => result.invalid_control_attempts += 1,
            }
            if !groups.contains_key(&group_key) && groups.len() >= limits.max_groups {
                result.ungrouped_attempts += 1;
                result.group_limit_reached = true;
                continue;
            }
            let group = groups.entry(group_key).or_default();
            group.attempts += 1;
            let offsets = [
                timing.response_headers_ms,
                timing.first_byte_ms,
                timing.first_text_ms,
                timing.first_reasoning_ms,
                timing.first_refusal_ms,
                timing.first_tool_ms,
                timing.finished_ms,
            ];
            let retain = retained_attempts < limits.max_samples;
            if offsets.iter().any(Option::is_some) {
                if retain {
                    retained_attempts += 1;
                } else {
                    result.sample_limit_reached = true;
                }
            }
            for (metric, offset) in group.metrics.iter_mut().zip(offsets) {
                metric.add(offset, retain);
            }
        }
        generation_requests += u64::from(usable_generation);
        // Request categories are mutually exclusive; valid hops in an invalid
        // mixed row remain honest measured samples with their own attribution.
        if invalid {
            result.invalid_requests += 1;
        } else if observed {
            result.observed_requests += 1;
        } else {
            result.unobserved_requests += 1;
        }
    }
    result.groups = groups.into_iter().map(|(key,group)| {
        let [response_headers,first_byte,first_text,first_reasoning,first_refusal,first_tool,finished] = group.metrics.map(MetricAccumulator::finish);
        TimingGroup { provider_id:key.provider_id,provider:key.provider,model:key.model,mode:key.mode.into(),outcome:key.outcome.into(),input_band:key.input_band.into(),reasoning_event:key.reasoning_event.into(),tool_event:key.tool_event.into(),wire_modes_state:key.controls.state().into(),wire_modes:key.controls.value(),observation_context_state:key.context.state().into(),observation_context:key.context.value(),attempts:group.attempts,
            metrics:TimingMetrics {response_headers,first_byte,first_text,first_reasoning,first_refusal,first_tool,finished} }
    }).collect();
    result.generation_reports = generation.finish(generation_requests);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use serde_json::{json, Value};
    async fn seed(db: &Db, ts: i64, raw: Option<&str>, estimate: Option<i64>) {
        sqlx::query("INSERT INTO requests(ts,requested_model,status,latency_ms,prompt_tokens,completion_tokens,fallback_attempts,attempts_json,estimated_prompt_tokens,routed_provider,routed_model) VALUES (?,'owned',200,99999,123,456,0,?,?,'ROW-WINNER','ROW-WINNER')").bind(ts).bind(raw).bind(estimate).execute(db.pool()).await.unwrap();
    }
    fn observed(model: &str, mode: &str, outcome: &str, first: Option<u64>, end: u64) -> Value {
        json!({"provider_id":model,"provider":model,"model":model,"ok":true,"timing":{"schema_version":1,"mode":mode,"terminal":outcome,"response_headers_ms":0,"first_text_ms":first,"first_reasoning_ms":null,"first_refusal_ms":null,"first_tool_ms":null,"finished_ms":end}})
    }
    fn group<'a>(v: &'a Value, model: &str, mode: &str, outcome: &str) -> &'a Value {
        v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["model"] == model && g["mode"] == mode && g["outcome"] == outcome)
            .expect("expected actual timing group missing")
    }
    #[tokio::test]
    async fn window_zero_unknown_failed_and_per_hop_models_use_observations_only() {
        let db = Db::connect_in_memory().await.unwrap();
        seed(
            &db,
            99,
            Some(&json!([observed("old", "streaming", "completed", Some(9999), 9999)]).to_string()),
            Some(1),
        )
        .await;
        for (ts, row) in [
            (100, observed("a", "streaming", "completed", Some(0), 10)),
            (101, observed("a", "streaming", "completed", Some(20), 30)),
            (102, observed("a", "buffered", "completed", None, 100)),
            (103, observed("b", "streaming", "failed", Some(7), 9)),
        ] {
            seed(&db, ts, Some(&json!([row]).to_string()), Some(100)).await;
        }
        seed(&db, 104, None, None).await;
        seed(&db, 105, Some("[]"), Some(0)).await;
        seed(&db, 106, Some("not-json"), Some(42)).await;
        let first = observed("c", "streaming", "failed", None, 2);
        let mut second = observed("d", "streaming", "completed", Some(3), 4);
        second["timing"]["first_tool_ms"] = json!(2);
        seed(
            &db,
            107,
            Some(&json!([first, second]).to_string()),
            Some(100),
        )
        .await;
        let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
        assert_eq!(v["total_requests"], 8);
        assert_eq!(v["observed_requests"], 5);
        assert_eq!(v["unobserved_requests"], 1);
        assert_eq!(v["no_details_requests"], 1);
        assert_eq!(v["invalid_requests"], 1);
        assert_eq!(v["valid_attempts"], 6);
        let a = group(&v, "a", "streaming", "completed");
        assert_eq!(a["attempts"], 2);
        assert_eq!(
            a["metrics"]["first_text"],
            json!({"samples":2,"retained_samples":2,"p50_ms":0,"p95_ms":20,"complete":true})
        );
        assert_eq!(a["input_band"], "under_4k");
        assert_eq!(a["reasoning_event"], "not_observed");
        assert_eq!(
            group(&v, "a", "buffered", "completed")["metrics"]["first_text"]["p50_ms"],
            Value::Null
        );
        assert_eq!(
            group(&v, "b", "streaming", "failed")["metrics"]["first_text"]["p50_ms"],
            7
        );
        assert_eq!(
            group(&v, "d", "streaming", "completed")["tool_event"],
            "observed"
        );
        assert!(!v.to_string().contains("ROW-WINNER"));
    }
    #[tokio::test]
    async fn invalid_runtime_and_missing_identity_never_borrow_scalar_or_winner() {
        let db = Db::connect_in_memory().await.unwrap();
        let mut native = observed("native", "streaming", "completed", Some(0), 1);
        native["runtime_kind"] = json!("trae-model");
        native["native_exact"] = json!({"version":1});
        seed(&db, 100, Some(&json!([native]).to_string()), Some(1)).await;
        let mut invalid = observed("bad", "streaming", "completed", Some(0), 1);
        invalid["timing"]["schema_version"] = json!(99);
        let mut unattributed = observed("none", "streaming", "completed", Some(0), 1);
        for key in ["model", "provider_id", "provider"] {
            unattributed.as_object_mut().unwrap().remove(key);
        }
        seed(
            &db,
            101,
            Some(&json!([invalid, unattributed]).to_string()),
            Some(-1),
        )
        .await;
        seed(&db, 102, Some(&" ".repeat(65537)), None).await;
        seed(
            &db,
            103,
            Some(&json!(vec![json!({}); 65]).to_string()),
            None,
        )
        .await;
        let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
        assert_eq!(v["total_requests"], 4);
        assert_eq!(v["unobserved_requests"], 1);
        assert_eq!(v["invalid_requests"], 3);
        assert_eq!(v["observed_requests"], 0);
        assert_eq!(v["valid_attempts"], 1);
        assert_eq!(v["invalid_attempts"], 1);
        assert_eq!(v["unobserved_attempts"], 1);
        assert_eq!(v["groups"][0]["provider_id"], Value::Null);
        assert_eq!(v["groups"][0]["model"], Value::Null);
        assert_eq!(v["groups"][0]["input_band"], "unknown");
        assert_eq!(v["groups"][0]["metrics"]["first_text"]["p50_ms"], 0);
    }
    #[tokio::test]
    async fn bounded_retention_and_grouping_make_partial_percentiles_unknown() {
        let db = Db::connect_in_memory().await.unwrap();
        for (ts, model, value) in [(100, "a", 10), (101, "a", 20), (102, "b", 30)] {
            seed(
                &db,
                ts,
                Some(
                    &json!([observed(
                        model,
                        "streaming",
                        "completed",
                        Some(value),
                        value + 1
                    )])
                    .to_string(),
                ),
                Some(10),
            )
            .await;
        }
        let v = serde_json::to_value(
            rolling_with_limits(
                db.pool(),
                100,
                Limits {
                    max_groups: 1,
                    max_samples: 1,
                },
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v["valid_attempts"], 3);
        assert_eq!(v["groups"].as_array().unwrap().len(), 1);
        assert_eq!(v["ungrouped_attempts"], 1);
        assert_eq!(v["group_limit_reached"], true);
        assert_eq!(v["sample_limit_reached"], true);
        let m = &v["groups"][0]["metrics"]["first_text"];
        assert_eq!(m["samples"], 2);
        assert_eq!(m["retained_samples"], 1);
        assert_eq!(m["p50_ms"], Value::Null);
        assert_eq!(m["p95_ms"], Value::Null);
        assert_eq!(m["complete"], false);
    }
    #[tokio::test]
    async fn inclusive_bounds_exclude_future_rows_and_empty_window_is_unknown() {
        let db = Db::connect_in_memory().await.unwrap();
        for ts in [99, 100, 86500, 86501] {
            seed(
                &db,
                ts,
                Some(&json!([observed("bound", "streaming", "completed", Some(0), 0)]).to_string()),
                None,
            )
            .await;
        }
        let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
        assert_eq!(v["total_requests"], 2);
        assert_eq!(v["since_ts"], 100);
        assert_eq!(v["until_ts"], 86500);
        assert_eq!(
            group(&v, "bound", "streaming", "completed")["metrics"]["first_text"]["p50_ms"],
            0
        );
        let empty = serde_json::to_value(rolling(db.pool(), 90000).await.unwrap()).unwrap();
        assert_eq!(empty["total_requests"], 0);
        assert!(empty["groups"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn completed_http_with_failed_attempt_remains_a_failure_group() {
        let db = Db::connect_in_memory().await.unwrap();
        let mut a = observed("a", "streaming", "completed", Some(1), 2);
        a["ok"] = json!(false);
        seed(&db, 100, Some(&json!([a]).to_string()), Some(33000)).await;
        let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
        let g = group(&v, "a", "streaming", "attempt_failed");
        assert_eq!(g["input_band"], "32k_to_128k");
        assert_eq!(g["metrics"]["finished"]["p50_ms"], 2);
    }
    #[tokio::test]
    async fn sql_type_and_exact_byte_cap_preserve_classification_without_guessing_hops() {
        let db = Db::connect_in_memory().await.unwrap();
        let raw =
            serde_json::json!([observed("a", "streaming", "completed", Some(0), 1)]).to_string();
        let exact = format!("{}{}", raw, " ".repeat(65536 - raw.len()));
        seed(&db, 100, Some(&exact), Some(0)).await;
        seed(&db, 101, Some(&(exact + " ")), None).await;
        sqlx::query(
            "INSERT INTO requests(ts,requested_model,attempts_json) VALUES(102,'owned',x'fffe')",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
        assert_eq!(v["total_requests"], 3);
        assert_eq!(v["observed_requests"], 1);
        assert_eq!(v["invalid_requests"], 2);
        assert_eq!(v["valid_attempts"], 1);
        assert_eq!(v["invalid_attempts"], 0);
    }
    #[tokio::test]
    async fn mixed_hops_and_legacy_http_extension_gate_remain_distinct() {
        let db = Db::connect_in_memory().await.unwrap();
        seed(&db, 100, Some("broken"), None).await;
        let old = rolling(db.pool(), 100).await.unwrap();
        assert!(!old.has_timing_metadata());
        let good = observed("a", "streaming", "completed", Some(0), 2);
        let mut bad = good.clone();
        bad["timing"]["schema_version"] = json!(2);
        let mut native = good.clone();
        native["runtime_kind"] = json!("trae-model");
        native["native_exact"] = json!({"version":1});
        seed(
            &db,
            101,
            Some(&json!([good, bad, native]).to_string()),
            Some(100),
        )
        .await;
        let summary = rolling(db.pool(), 100).await.unwrap();
        assert!(summary.has_timing_metadata());
        let v = serde_json::to_value(summary).unwrap();
        assert_eq!(v["observed_requests"], 0);
        assert_eq!(v["invalid_requests"], 2);
        assert_eq!(v["valid_attempts"], 1);
        assert_eq!(v["invalid_attempts"], 1);
        assert_eq!(v["unobserved_attempts"], 1);
        assert_eq!(v["groups"][0]["attempts"], 1);
    }
    #[tokio::test]
    async fn first_byte_v2_zero_is_sampled_but_v1_absence_is_not_zero() {
        let db = Db::connect_in_memory().await.unwrap();
        let legacy = observed("a", "streaming", "completed", Some(5), 10);
        let mut modern = legacy.clone();
        modern["timing"]["schema_version"] = json!(2);
        modern["timing"]["first_byte_ms"] = json!(0);
        seed(&db, 100, Some(&json!([legacy]).to_string()), Some(100)).await;
        seed(&db, 101, Some(&json!([modern]).to_string()), Some(100)).await;
        let result = rolling(db.pool(), 100).await.unwrap();
        assert_eq!(result.valid_attempts, 2);
        let metrics = &result.groups[0].metrics;
        assert_eq!(metrics.first_byte.samples, 1);
        assert_eq!(metrics.first_byte.retained_samples, 1);
        assert_eq!(metrics.first_byte.p50_ms, Some(0));
        assert_eq!(metrics.first_text.samples, 2);
        assert_eq!(metrics.first_text.p50_ms, Some(5));
    }
    #[tokio::test]
    async fn controls_groups_keep_requested_wire_unknown_and_invalid_sources_separate() {
        let db = Db::connect_in_memory().await.unwrap();
        let controls = json!({"reasoning":"enabled","tools":"configured","tool_choice":"auto"});
        let good = json!({"schema_version":1,"dialect":"openai","declared_thinking":true,"caller":{"dialect":"openai","modes":controls},"normalized_ingress":controls,"effective_request":controls,"wire":controls});
        let mut stripped = good.clone();
        stripped["wire"]["reasoning"] = json!("absent");
        stripped["declared_thinking"] = json!(false);
        let mut bad = good.clone();
        bad["private_body"] = json!("PRIVATE-CONTROL-CONTENT");
        let mut partial = good.clone();
        partial["caller"] = Value::Null;
        partial["normalized_ingress"] = Value::Null;
        for (index, modes) in [Some(good), Some(stripped), None, Some(bad), Some(partial)]
            .into_iter()
            .enumerate()
        {
            let mut attempt = observed("owned", "buffered", "completed", None, 10);
            if let Some(modes) = modes {
                attempt["wire_modes"] = modes;
            }
            seed(
                &db,
                100 + index as i64,
                Some(&json!([attempt]).to_string()),
                Some(3),
            )
            .await;
        }
        let result = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
        assert_eq!(
            result["groups"].as_array().unwrap().len(),
            5,
            "control-mode differences must not collapse or borrow output-event flags"
        );
        assert_eq!(result["valid_attempts"], 5);
        assert_eq!(result["observed_control_attempts"], 3);
        assert_eq!(result["unobserved_control_attempts"], 1);
        assert_eq!(result["invalid_control_attempts"], 1);
        let groups = result["groups"].as_array().unwrap();
        for (state, count) in [("observed", 3), ("unknown", 1), ("invalid", 1)] {
            assert_eq!(
                groups
                    .iter()
                    .filter(|g| g["wire_modes_state"] == state)
                    .count(),
                count
            );
        }
        for group in groups {
            assert_eq!(group["attempts"], 1);
            assert_eq!(group["metrics"]["finished"]["samples"], 1);
        }
        assert!(!result.to_string().contains("PRIVATE-CONTROL-CONTENT"));
    }
}

#[cfg(test)]
#[path = "request_timing/generation_summary_tests.rs"]
mod generation_summary_tests;
