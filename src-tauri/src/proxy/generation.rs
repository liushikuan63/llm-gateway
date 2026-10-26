//! Protocol-reported generation telemetry, never invoice/quality evidence or
//! a replacement for legacy end-to-end health/routing rates.
use super::timing::{AttemptTiming, TimingMode, TimingTerminal};
use parking_lot::Mutex;
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::sync::Arc;
#[cfg(test)]
#[path = "generation/tests.rs"]
mod tests;

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub schema_version: u8,
    pub source: &'static str,
    pub state: &'static str,
    pub reason: Option<&'static str>,
    pub timing_mode: Option<TimingMode>,
    pub output_tokens: Option<u32>,
    pub eval_duration_ns: Option<String>,
    pub total_duration_ns: Option<String>,
    pub load_duration_ns: Option<String>,
    pub prompt_eval_duration_ns: Option<String>,
    pub rate_tokens_per_second: Option<f64>,
}
impl Snapshot {
    fn blank(reason: &'static str) -> Self {
        Self {
            schema_version: 1,
            source: "ollama_native_eval",
            state: "unreported",
            reason: Some(reason),
            timing_mode: None,
            output_tokens: None,
            eval_duration_ns: None,
            total_duration_ns: None,
            load_duration_ns: None,
            prompt_eval_duration_ns: None,
            rate_tokens_per_second: None,
        }
    }
    fn final_report(raw: &Value) -> Option<Self> {
        if raw.get("done").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let mut s = Self::blank("final_report_incomplete");
        s.output_tokens = raw
            .get("eval_count")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        let eval = raw.get("eval_duration").and_then(Value::as_u64);
        let total = raw.get("total_duration").and_then(Value::as_u64);
        s.eval_duration_ns = eval.map(|n| n.to_string());
        s.total_duration_ns = total.map(|n| n.to_string());
        s.load_duration_ns = raw
            .get("load_duration")
            .and_then(Value::as_u64)
            .map(|n| n.to_string());
        s.prompt_eval_duration_ns = raw
            .get("prompt_eval_duration")
            .and_then(Value::as_u64)
            .map(|n| n.to_string());
        s.state = "unknown";
        s.reason = if raw.get("error").is_some_and(|e| !e.is_null()) {
            Some("inline_error_report")
        } else if s.output_tokens.is_none() {
            Some(if raw.get("eval_count").is_none() {
                "output_tokens_missing"
            } else {
                "output_tokens_invalid"
            })
        } else if eval.is_none() {
            Some(if raw.get("eval_duration").is_none() {
                "eval_duration_missing"
            } else {
                "eval_duration_invalid"
            })
        } else if eval == Some(0) {
            Some("eval_duration_zero")
        } else if matches!((eval,total),(Some(a),Some(b)) if a>b) {
            Some("duration_inconsistent")
        } else {
            None
        };
        if s.reason.is_none() {
            let rate = f64::from(s.output_tokens?) * 1_000_000_000.0 / (eval? as f64);
            if rate.is_finite() {
                s.rate_tokens_per_second = Some(rate);
                s.state = "reported";
            } else {
                s.reason = Some("rate_not_finite");
            }
        }
        Some(s)
    }
}
#[derive(Debug, Clone)]
enum Report {
    Empty,
    One(Snapshot),
    Ambiguous,
}
#[derive(Debug, Clone)]
pub struct SharedGeneration {
    report: Arc<Mutex<Report>>,
    timing: Arc<Mutex<Option<AttemptTiming>>>,
}
impl SharedGeneration {
    pub(crate) fn new(timing: Arc<Mutex<Option<AttemptTiming>>>) -> Self {
        Self {
            report: Arc::new(Mutex::new(Report::Empty)),
            timing,
        }
    }
    pub(crate) fn observe(&self, raw: &Value) {
        let Some(report) = Snapshot::final_report(raw) else {
            return;
        };
        let mut slot = self.report.lock();
        *slot = match &*slot {
            Report::Empty => Report::One(report),
            Report::One(_) | Report::Ambiguous => Report::Ambiguous,
        };
    }
    pub fn snapshot(&self) -> Snapshot {
        let timing = self.timing.lock().as_ref().map(AttemptTiming::snapshot);
        let mut report = match &*self.report.lock() {
            Report::Empty => Snapshot::blank("final_report_not_observed"),
            Report::One(s) => s.clone(),
            Report::Ambiguous => {
                let mut s = Snapshot::blank("multiple_final_reports");
                s.state = "unknown";
                s
            }
        };
        report.timing_mode = timing.as_ref().map(|t| t.mode);
        if timing.as_ref().and_then(|t| t.terminal) != Some(TimingTerminal::Completed) {
            report.state = "not_completed";
            report.reason = Some("invocation_not_completed");
            report.rate_tokens_per_second = None;
        }
        report
    }
}
impl Serialize for SharedGeneration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.snapshot().serialize(serializer)
    }
}
