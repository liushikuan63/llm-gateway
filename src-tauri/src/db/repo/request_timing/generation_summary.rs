//! Passive report coverage. No inference from end-to-end timing or token totals.
use super::{key, ControlObservation, GroupKey, Limits};
use crate::proxy::timing::{TimingMode, TimingSnapshot, TimingTerminal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Overview {
    pub schema_version: u8,
    pub parsed_attempts: u64,
    pub inspected_requests: u64,
    pub unavailable_requests: u64,
    pub no_details_requests: u64,
    pub invalid_records: u64,
    pub usable_requests: u64,
    pub usable_attempts: u64,
    pub absent_attempts: u64,
    pub unknown_attempts: u64,
    pub invalid_attempts: u64,
    pub ungrouped_attempts: u64,
    pub group_limit_reached: bool,
    pub sample_limit_reached: bool,
    pub groups: Vec<Group>,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Rate {
    samples: u64,
    retained_samples: u64,
    complete: bool,
    p50_tokens_per_second: Option<f64>,
    p95_tokens_per_second: Option<f64>,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Group {
    provider_id: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    mode: &'static str,
    input_band: &'static str,
    reasoning_event: &'static str,
    tool_event: &'static str,
    wire_modes_state: &'static str,
    wire_modes: Option<crate::proxy::control_modes::WireModeSnapshot>,
    observation_context_state: String,
    observation_context: Option<crate::proxy::observation_context::Snapshot>,
    rate: Rate,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    schema_version: u8,
    source: String,
    state: String,
    reason: Option<String>,
    timing_mode: Option<TimingMode>,
    output_tokens: Option<u32>,
    eval_duration_ns: Option<String>,
    total_duration_ns: Option<String>,
    load_duration_ns: Option<String>,
    prompt_eval_duration_ns: Option<String>,
    rate_tokens_per_second: Option<f64>,
}
fn ns(raw: &str) -> Option<u64> {
    // Decimal strings preserve u64 precision in JavaScript. Noncanonical forms
    // are invalid metadata, never a new label or a silent zero sample.
    let value = raw.parse::<u64>().ok()?;
    (value.to_string() == raw).then_some(value)
}
fn project(value: &Value) -> Option<Report> {
    if value.as_object()?.len() != 11 {
        return None;
    }
    let mut r: Report = serde_json::from_value(value.clone()).ok()?;
    if r.schema_version != 1
        || r.source != "ollama_native_eval"
        || !matches!(
            r.state.as_str(),
            "reported" | "unknown" | "unreported" | "not_completed"
        )
        || r.reason.as_ref().is_some_and(|s| s.len() > 256)
        || [
            &r.eval_duration_ns,
            &r.total_duration_ns,
            &r.load_duration_ns,
            &r.prompt_eval_duration_ns,
        ]
        .iter()
        .any(|v| v.as_ref().is_some_and(|s| ns(s).is_none()))
    {
        return None;
    }
    if r.state == "reported" {
        let count = r.output_tokens?;
        let eval = ns(r.eval_duration_ns.as_deref()?)?;
        if eval == 0
            || r.reason.is_some()
            || r.timing_mode.is_none()
            || r.total_duration_ns
                .as_deref()
                .and_then(ns)
                .is_some_and(|total| eval > total)
        {
            return None;
        }
        let expected = f64::from(count) * 1e9 / eval as f64;
        // The locked JSON decoder can change a serialized f64 by one ULP.
        // Accept only its exact deterministic roundtrip, not an epsilon or an
        // arbitrary nearby rate. Integer count/ns remain authoritative.
        let persisted: f64 = serde_json::from_str(&serde_json::to_string(&expected).ok()?).ok()?;
        if r.rate_tokens_per_second != Some(expected) && r.rate_tokens_per_second != Some(persisted)
        {
            return None;
        }
        r.rate_tokens_per_second = Some(expected);
    } else if r.rate_tokens_per_second.is_some() {
        return None;
    }
    Some(r)
}
/// Normalize only the redundant, validated floating derivative in read DTOs.
/// Stored JSON and all authoritative integer fields stay untouched. The same
/// projector serves summary and detail, so exact JSON roundtrip is not a new
/// heuristic or a second independently maintained interpretation.
pub(super) fn normalize_reports(attempts: &mut [Value]) {
    for attempt in attempts {
        let Some(report) = attempt.get("generation").and_then(project) else {
            continue;
        };
        if let Some(rate) = report.rate_tokens_per_second {
            attempt["generation"]["rate_tokens_per_second"] = serde_json::json!(rate);
        }
    }
}
#[derive(Default)]
struct Samples {
    count: u64,
    rates: Vec<f64>,
    lost: bool,
}
pub(super) struct Accumulator {
    overview: Overview,
    groups: BTreeMap<GroupKey, Samples>,
    retained: usize,
}
impl Default for Accumulator {
    fn default() -> Self {
        Self {
            overview: Overview {
                schema_version: 1,
                ..Default::default()
            },
            groups: BTreeMap::new(),
            retained: 0,
        }
    }
}
impl Accumulator {
    pub(super) fn unavailable(&mut self) {
        self.overview.unavailable_requests += 1;
    }
    pub(super) fn invalid(&mut self) {
        self.overview.invalid_records += 1;
    }
    pub(super) fn empty(&mut self) {
        self.overview.no_details_requests += 1;
    }
    pub(super) fn inspected(&mut self) {
        self.overview.inspected_requests += 1;
    }
    pub(super) fn observe(
        &mut self,
        attempt: &Value,
        estimate: Option<i64>,
        limits: Limits,
    ) -> bool {
        let o = &mut self.overview;
        o.parsed_attempts += 1;
        let Some(a) = attempt.as_object() else {
            o.invalid_attempts += 1;
            return false;
        };
        let Some(raw) = a.get("generation").filter(|v| !v.is_null()) else {
            o.absent_attempts += 1;
            return false;
        };
        let Some(report) = project(raw) else {
            o.invalid_attempts += 1;
            return false;
        };
        let timing = a.get("timing").and_then(TimingSnapshot::project);
        if a.contains_key("native_exact")
            || a.get("runtime_kind").is_some_and(|v| !v.is_null())
            || matches!(ControlObservation::from_attempt(attempt), ControlObservation::Observed(w) if w.dialect != crate::proxy::control_modes::WireDialect::Ollama)
        {
            o.invalid_attempts += 1;
            return false;
        }
        let Some(t) = timing else {
            o.invalid_attempts += 1;
            return false;
        };
        if report.state != "reported"
            || report.timing_mode != Some(t.mode)
            || t.terminal != Some(TimingTerminal::Completed)
            || a.get("ok").and_then(Value::as_bool) != Some(true)
        {
            o.unknown_attempts += 1;
            return false;
        }
        o.usable_attempts += 1;
        let k = key(attempt, &t, estimate);
        if !self.groups.contains_key(&k) && self.groups.len() >= limits.max_groups {
            o.ungrouped_attempts += 1;
            o.group_limit_reached = true;
            return true;
        }
        let group = self.groups.entry(k).or_default();
        group.count += 1;
        if self.retained < limits.max_samples {
            group.rates.push(
                report
                    .rate_tokens_per_second
                    .expect("validated reported rate"),
            );
            self.retained += 1;
        } else {
            group.lost = true;
            o.sample_limit_reached = true;
        }
        true
    }
    pub(super) fn finish(mut self, usable_requests: u64) -> Overview {
        self.overview.usable_requests = usable_requests;
        self.overview.groups = self
            .groups
            .into_iter()
            .map(|(k, mut samples)| {
                samples.rates.sort_by(f64::total_cmp);
                let p = |n: usize| {
                    if samples.lost || samples.rates.is_empty() {
                        None
                    } else {
                        samples
                            .rates
                            .get((samples.rates.len() * n).div_ceil(100) - 1)
                            .copied()
                    }
                };
                Group {
                    provider_id: k.provider_id,
                    provider: k.provider,
                    model: k.model,
                    mode: k.mode,
                    input_band: k.input_band,
                    reasoning_event: k.reasoning_event,
                    tool_event: k.tool_event,
                    wire_modes_state: k.controls.state(),
                    wire_modes: k.controls.value(),
                    observation_context_state: k.context.state().into(),
                    observation_context: k.context.value(),
                    rate: Rate {
                        samples: samples.count,
                        retained_samples: samples.rates.len() as u64,
                        complete: !samples.lost,
                        p50_tokens_per_second: p(50),
                        p95_tokens_per_second: p(95),
                    },
                }
            })
            .collect();
        self.overview
    }
}
