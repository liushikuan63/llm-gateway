//! Numeric per-attempt observations. No content, remote compute estimates or scoring.
//! Boundaries: [request timing](../../../docs/wiki/request-timing.md).
use super::upstream::UpstreamEvent;
use serde::{Deserialize, Serialize};
pub(crate) mod observation;
pub use observation::SharedTiming;

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingMode {
    Streaming,
    Buffered,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingTerminal {
    Completed,
    Failed,
    Cancelled,
    Incomplete,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimingSnapshot {
    pub schema_version: u8,
    pub mode: TimingMode,
    pub terminal: Option<TimingTerminal>,
    pub response_headers_ms: Option<u64>,
    pub first_byte_ms: Option<u64>,
    pub first_text_ms: Option<u64>,
    pub first_reasoning_ms: Option<u64>,
    pub first_refusal_ms: Option<u64>,
    pub first_tool_ms: Option<u64>,
    pub finished_ms: Option<u64>,
}
impl<'de> Deserialize<'de> for TimingSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::project(&value).ok_or_else(|| serde::de::Error::custom("invalid timing observation"))
    }
}
impl TimingSnapshot {
    /// Strict historical v1 or current v2 input; normalize to v2 with unknown
    /// first-byte for v1. Never infer that value from headers or semantic data.
    pub fn project(value: &serde_json::Value) -> Option<Self> {
        const MAX_SAFE_MILLISECONDS: u64 = 9_007_199_254_740_991;
        let object = value.as_object()?;
        let version = object.get("schema_version")?.as_u64()?;
        if !matches!(version, 1 | 2) || object.len() != if version == 1 { 9 } else { 10 } {
            return None;
        }
        let mode = match object.get("mode")?.as_str()? {
            "streaming" => TimingMode::Streaming,
            "buffered" => TimingMode::Buffered,
            _ => return None,
        };
        let terminal = match object.get("terminal")? {
            serde_json::Value::Null => None,
            value => Some(match value.as_str()? {
                "completed" => TimingTerminal::Completed,
                "failed" => TimingTerminal::Failed,
                "cancelled" => TimingTerminal::Cancelled,
                "incomplete" => TimingTerminal::Incomplete,
                _ => return None,
            }),
        };
        let number = |key: &str| -> Option<Option<u64>> {
            match object.get(key)? {
                serde_json::Value::Null => Some(None),
                value => value
                    .as_u64()
                    .filter(|v| *v <= MAX_SAFE_MILLISECONDS)
                    .map(Some),
            }
        };
        let result = Self {
            schema_version: 2,
            mode,
            terminal,
            response_headers_ms: number("response_headers_ms")?,
            first_byte_ms: if version == 1 {
                None
            } else {
                number("first_byte_ms")?
            },
            first_text_ms: number("first_text_ms")?,
            first_reasoning_ms: number("first_reasoning_ms")?,
            first_refusal_ms: number("first_refusal_ms")?,
            first_tool_ms: number("first_tool_ms")?,
            finished_ms: number("finished_ms")?,
        };
        if result.terminal.is_some() != result.finished_ms.is_some() {
            return None;
        }
        let first = [
            result.first_text_ms,
            result.first_reasoning_ms,
            result.first_refusal_ms,
            result.first_tool_ms,
        ];
        if result.mode == TimingMode::Buffered && first.iter().any(Option::is_some) {
            return None;
        }
        if first.iter().flatten().any(|v| {
            result.response_headers_ms.is_some_and(|h| *v < h)
                || result.first_byte_ms.is_some_and(|b| *v < b)
                || result.finished_ms.is_some_and(|end| *v > end)
        }) {
            return None;
        }
        if let Some(byte) = result.first_byte_ms {
            if result.response_headers_ms.map_or(true, |h| byte < h)
                || result.finished_ms.is_some_and(|end| byte > end)
            {
                return None;
            }
        }
        if result
            .response_headers_ms
            .zip(result.finished_ms)
            .is_some_and(|(headers, end)| headers > end)
        {
            return None;
        }
        Some(result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObservationError {
    ClockRegression,
    Overflow,
    Finalized,
}
#[derive(Debug)]
pub(crate) struct AttemptTiming {
    value: TimingSnapshot,
    last: Duration,
}
impl AttemptTiming {
    pub(crate) fn new(mode: TimingMode) -> Self {
        Self {
            value: TimingSnapshot {
                schema_version: 2,
                mode,
                terminal: None,
                response_headers_ms: None,
                first_byte_ms: None,
                first_text_ms: None,
                first_reasoning_ms: None,
                first_refusal_ms: None,
                first_tool_ms: None,
                finished_ms: None,
            },
            last: Duration::ZERO,
        }
    }
    fn advance(&mut self, at: Duration) -> Result<u64, ObservationError> {
        if self.value.terminal.is_some() {
            return Err(ObservationError::Finalized);
        }
        if at < self.last {
            return Err(ObservationError::ClockRegression);
        }
        let milliseconds = u64::try_from(at.as_millis()).map_err(|_| ObservationError::Overflow)?;
        self.last = at;
        Ok(milliseconds)
    }
    pub(crate) fn headers(&mut self, at: Duration) -> Result<(), ObservationError> {
        let milliseconds = self.advance(at)?;
        self.value.response_headers_ms.get_or_insert(milliseconds);
        Ok(())
    }
    pub(crate) fn first_byte(&mut self, at: Duration) -> Result<(), ObservationError> {
        let milliseconds = self.advance(at)?;
        self.value.first_byte_ms.get_or_insert(milliseconds);
        Ok(())
    }
    pub(crate) fn event(
        &mut self,
        event: &UpstreamEvent,
        at: Duration,
    ) -> Result<(), ObservationError> {
        let milliseconds = self.advance(at)?;
        // A buffered body is observed only after completion; it cannot establish
        // an upstream first-token timestamp, regardless of the decoded content.
        if self.value.mode != TimingMode::Streaming {
            return Ok(());
        }
        let field = match event {
            UpstreamEvent::Delta(text) if !text.is_empty() => Some(&mut self.value.first_text_ms),
            UpstreamEvent::ReasoningDelta(text) if !text.is_empty() => {
                Some(&mut self.value.first_reasoning_ms)
            }
            UpstreamEvent::RefusalDelta(text) if !text.is_empty() => {
                Some(&mut self.value.first_refusal_ms)
            }
            UpstreamEvent::ToolCalls(calls) if meaningful_tools(calls) => {
                Some(&mut self.value.first_tool_ms)
            }
            _ => None,
        };
        if let Some(field) = field {
            field.get_or_insert(milliseconds);
        }
        Ok(())
    }
    pub(crate) fn finish(
        &mut self,
        terminal: TimingTerminal,
        at: Duration,
    ) -> Result<(), ObservationError> {
        let milliseconds = self.advance(at)?;
        self.value.finished_ms = Some(milliseconds);
        self.value.terminal = Some(terminal);
        Ok(())
    }
    pub(crate) fn snapshot(&self) -> TimingSnapshot {
        self.value.clone()
    }
}

fn meaningful_tools(value: &serde_json::Value) -> bool {
    value.as_array().is_some_and(|calls| {
        calls.iter().any(|call| {
            ["/function/name", "/function/arguments"]
                .iter()
                .any(|field| {
                    call.pointer(field)
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|text| !text.is_empty())
                })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }
    #[test]
    fn semantic_first_events_keep_separate_first_offsets() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        t.headers(ms(5)).unwrap();
        t.event(&UpstreamEvent::Ping, ms(6)).unwrap();
        t.event(&UpstreamEvent::Delta(String::new()), ms(7))
            .unwrap();
        t.event(
            &UpstreamEvent::ReasoningDelta("private reasoning".into()),
            ms(8),
        )
        .unwrap();
        t.event(&UpstreamEvent::Delta("text".into()), ms(13))
            .unwrap();
        t.event(
            &UpstreamEvent::RefusalDelta("sensitive refusal payload".into()),
            ms(15),
        )
        .unwrap();
        t.event(
            &UpstreamEvent::ToolCalls(json!([{"index":0,"function":{"name":"owned"}}])),
            ms(20),
        )
        .unwrap();
        t.event(&UpstreamEvent::Delta("later".into()), ms(22))
            .unwrap();
        t.event(&UpstreamEvent::Usage(Default::default()), ms(25))
            .unwrap();
        t.finish(TimingTerminal::Completed, ms(30)).unwrap();
        let s = t.snapshot();
        assert_eq!(s.response_headers_ms, Some(5));
        assert_eq!(s.first_reasoning_ms, Some(8));
        assert_eq!(s.first_text_ms, Some(13));
        assert_eq!(s.first_refusal_ms, Some(15));
        assert_eq!(s.first_tool_ms, Some(20));
        assert_eq!(s.finished_ms, Some(30));
        assert_eq!(s.terminal, Some(TimingTerminal::Completed));
        let serialized = serde_json::to_string(&s).unwrap();
        for secret in [
            "private reasoning",
            "sensitive refusal payload",
            "owned",
            "later",
        ] {
            assert!(!serialized.contains(secret), "payload leaked: {secret}");
        }
    }
    #[test]
    fn metadata_empty_tools_finish_and_usage_do_not_create_content_samples() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        for e in [
            UpstreamEvent::Ping,
            UpstreamEvent::Delta("".into()),
            UpstreamEvent::ReasoningDelta("".into()),
            UpstreamEvent::RefusalDelta("".into()),
            UpstreamEvent::ToolCalls(json!([])),
            UpstreamEvent::ToolCalls(json!([{"index":0,"id":"call-metadata-only"}])),
            UpstreamEvent::ToolCalls(json!([{"index":0,"function":{"arguments":""}}])),
            UpstreamEvent::Usage(Default::default()),
            UpstreamEvent::Finish {
                finish_reason: Some("stop".into()),
            },
            UpstreamEvent::Done {
                finish_reason: None,
                usage: None,
            },
        ] {
            t.event(&e, ms(2)).unwrap();
        }
        assert_eq!(t.snapshot().first_text_ms, None);
        assert_eq!(t.snapshot().first_tool_ms, None);
        assert_eq!(t.snapshot().terminal, None);
        t.finish(TimingTerminal::Completed, ms(4)).unwrap();
        assert_eq!(t.snapshot().terminal, Some(TimingTerminal::Completed));
    }
    #[test]
    fn refusal_and_reasoning_only_are_not_text_ttft() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        t.event(&UpstreamEvent::ReasoningDelta("r".into()), ms(3))
            .unwrap();
        t.event(&UpstreamEvent::RefusalDelta("no".into()), ms(7))
            .unwrap();
        t.finish(TimingTerminal::Completed, ms(9)).unwrap();
        assert_eq!(t.snapshot().first_text_ms, None);
        assert_eq!(t.snapshot().first_reasoning_ms, Some(3));
        assert_eq!(t.snapshot().first_refusal_ms, Some(7));
    }
    #[test]
    fn buffered_completion_does_not_claim_a_first_token_observation() {
        let mut t = AttemptTiming::new(TimingMode::Buffered);
        t.headers(ms(2)).unwrap();
        t.event(&UpstreamEvent::Delta("whole body".into()), ms(90))
            .unwrap();
        t.finish(TimingTerminal::Completed, ms(95)).unwrap();
        assert_eq!(t.snapshot().response_headers_ms, Some(2));
        assert_eq!(t.snapshot().first_text_ms, None);
        assert_eq!(t.snapshot().finished_ms, Some(95));
    }
    #[test]
    fn cancellation_before_headers_keeps_unknown_offsets_and_real_terminal() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        t.finish(TimingTerminal::Cancelled, ms(4)).unwrap();
        assert_eq!(t.snapshot().response_headers_ms, None);
        assert_eq!(t.snapshot().first_text_ms, None);
        assert_eq!(t.snapshot().finished_ms, Some(4));
        assert_eq!(t.snapshot().terminal, Some(TimingTerminal::Cancelled));
    }
    #[test]
    fn partial_failure_keeps_observation_without_becoming_completed() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        t.event(&UpstreamEvent::Delta("partial".into()), ms(3))
            .unwrap();
        t.finish(TimingTerminal::Failed, ms(8)).unwrap();
        assert_eq!(t.snapshot().first_text_ms, Some(3));
        assert_eq!(t.snapshot().terminal, Some(TimingTerminal::Failed));
    }
    #[test]
    fn eof_without_protocol_completion_is_incomplete() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        t.finish(TimingTerminal::Incomplete, ms(6)).unwrap();
        assert_eq!(t.snapshot().terminal, Some(TimingTerminal::Incomplete));
    }
    #[test]
    fn finalized_observation_rejects_late_events_and_duplicate_terminal() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        t.finish(TimingTerminal::Cancelled, ms(7)).unwrap();
        let before = t.snapshot();
        assert_eq!(
            t.event(&UpstreamEvent::Delta("late".into()), ms(8)),
            Err(ObservationError::Finalized)
        );
        assert_eq!(
            t.finish(TimingTerminal::Completed, ms(10)),
            Err(ObservationError::Finalized)
        );
        assert_eq!(t.snapshot(), before);
    }
    #[test]
    fn clock_regression_rejects_whole_observation_without_mutating_snapshot() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        t.headers(ms(5)).unwrap();
        let before = t.snapshot();
        assert_eq!(
            t.event(&UpstreamEvent::Delta("bad".into()), ms(4)),
            Err(ObservationError::ClockRegression)
        );
        assert_eq!(t.snapshot(), before);
        t.event(&UpstreamEvent::Delta("ok".into()), ms(6)).unwrap();
        assert_eq!(t.snapshot().first_text_ms, Some(6));
    }
    #[test]
    fn submillisecond_order_is_checked_before_numeric_flooring() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        t.headers(Duration::from_micros(500)).unwrap();
        assert_eq!(
            t.event(
                &UpstreamEvent::Delta("bad".into()),
                Duration::from_micros(100)
            ),
            Err(ObservationError::ClockRegression)
        );
        t.event(
            &UpstreamEvent::Delta("ok".into()),
            Duration::from_micros(700),
        )
        .unwrap();
        assert_eq!(t.snapshot().first_text_ms, Some(0));
    }
    #[test]
    fn duration_overflow_is_rejected_instead_of_wrapping_or_saturating() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        let before = t.snapshot();
        assert_eq!(t.headers(Duration::MAX), Err(ObservationError::Overflow));
        assert_eq!(t.snapshot(), before);
        t.headers(ms(1)).unwrap();
        assert_eq!(t.snapshot().response_headers_ms, Some(1));
    }
    #[test]
    fn attempts_have_independent_clocks_and_no_inferred_remote_fields() {
        let mut a = AttemptTiming::new(TimingMode::Streaming);
        a.headers(ms(15)).unwrap();
        a.finish(TimingTerminal::Failed, ms(50)).unwrap();
        let mut b = AttemptTiming::new(TimingMode::Streaming);
        b.headers(ms(2)).unwrap();
        b.event(&UpstreamEvent::Delta("b".into()), ms(4)).unwrap();
        b.finish(TimingTerminal::Completed, ms(7)).unwrap();
        assert_eq!(a.snapshot().finished_ms, Some(50));
        assert_eq!(b.snapshot().finished_ms, Some(7));
        let v = serde_json::to_value(b.snapshot()).unwrap();
        assert_eq!(v["schema_version"], 2);
        for key in ["queue_ms", "provider_compute_ms", "tool_wait_ms"] {
            assert!(v.get(key).is_none());
        }
    }
    #[test]
    fn s2_numeric_projection_rejects_invalid_shapes_offsets_and_future_versions() {
        let good = serde_json::json!({"schema_version":1,"mode":"streaming","terminal":"completed","response_headers_ms":0,"first_text_ms":0,"first_reasoning_ms":null,"first_refusal_ms":null,"first_tool_ms":null,"finished_ms":3});
        assert_eq!(
            TimingSnapshot::project(&good).unwrap().first_text_ms,
            Some(0)
        );
        for (field, bad) in [
            ("schema_version", serde_json::json!(99)),
            ("mode", serde_json::json!("PRIVATE")),
            ("terminal", serde_json::json!("SECRET")),
            ("terminal", serde_json::Value::Null),
            ("first_text_ms", serde_json::json!(-1)),
            ("first_text_ms", serde_json::json!(0.5)),
            ("first_text_ms", serde_json::json!("0")),
            (
                "first_text_ms",
                serde_json::json!(9_007_199_254_740_992_u64),
            ),
            ("first_text_ms", serde_json::json!(4)),
            ("response_headers_ms", serde_json::json!(1)),
            ("mode", serde_json::json!("buffered")),
        ] {
            let mut bad_value = good.clone();
            bad_value[field] = bad;
            assert!(
                TimingSnapshot::project(&bad_value).is_none(),
                "invalid projection accepted: {field}"
            );
        }
        let mut extra = good.clone();
        extra["private_content"] = serde_json::json!("sensitive");
        assert!(TimingSnapshot::project(&extra).is_none());
        let mut missing = good;
        missing.as_object_mut().unwrap().remove("first_tool_ms");
        assert!(TimingSnapshot::project(&missing).is_none());
    }
    #[test]
    fn first_byte_keeps_first_offset_and_obeys_monotonic_clock() {
        let mut t = AttemptTiming::new(TimingMode::Streaming);
        t.headers(ms(1)).unwrap();
        t.first_byte(ms(3)).unwrap();
        t.first_byte(ms(5)).unwrap();
        assert_eq!(t.snapshot().first_byte_ms, Some(3));
        assert_eq!(t.first_byte(ms(4)), Err(ObservationError::ClockRegression));
        t.event(&UpstreamEvent::Delta("owned".into()), ms(6))
            .unwrap();
        t.finish(TimingTerminal::Completed, ms(7)).unwrap();
        assert_eq!(t.snapshot().first_byte_ms, Some(3));
        assert_eq!(t.snapshot().first_text_ms, Some(6));
    }
    #[test]
    fn buffered_first_byte_is_measured_without_semantic_first_content() {
        let mut t = AttemptTiming::new(TimingMode::Buffered);
        t.headers(ms(0)).unwrap();
        t.first_byte(ms(0)).unwrap();
        t.event(&UpstreamEvent::Delta("owned".into()), ms(1))
            .unwrap();
        t.finish(TimingTerminal::Completed, ms(2)).unwrap();
        assert_eq!(t.snapshot().first_byte_ms, Some(0));
        assert_eq!(t.snapshot().first_text_ms, None);
    }
}
