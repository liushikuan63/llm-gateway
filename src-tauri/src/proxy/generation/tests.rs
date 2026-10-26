use super::*;
use serde_json::json;
use std::time::Duration;
fn complete() -> (SharedGeneration, Arc<Mutex<Option<AttemptTiming>>>) {
    let mut timing = AttemptTiming::new(TimingMode::Buffered);
    timing
        .finish(TimingTerminal::Completed, Duration::from_millis(10))
        .unwrap();
    let shared = Arc::new(Mutex::new(Some(timing)));
    (SharedGeneration::new(shared.clone()), shared)
}
#[test]
fn native_eval_rate_uses_reported_nanoseconds_and_keeps_exact_zero_and_large_durations() {
    let (g, _) = complete();
    g.observe(&json!({"done":true,"eval_count":18,"eval_duration":2000000000u64,"total_duration":3000000000u64,"load_duration":0,"message":{"thinking":"owned-private-no-persist"}}));
    let s = g.snapshot();
    assert_eq!(s.rate_tokens_per_second, Some(9.0));
    assert_eq!(s.eval_duration_ns.as_deref(), Some("2000000000"));
    assert_eq!(s.load_duration_ns.as_deref(), Some("0"));
    assert_eq!(s.prompt_eval_duration_ns, None);
    assert!(!serde_json::to_string(&s).unwrap().contains("owned-private"));
    let (zero, _) = complete();
    zero.observe(&json!({"done":true,"eval_count":0,"eval_duration":2000000000u64}));
    assert_eq!(zero.snapshot().rate_tokens_per_second, Some(0.0));
    assert_eq!(zero.snapshot().output_tokens, Some(0));
    let (large, _) = complete();
    large.observe(&json!({"done":true,"eval_count":1,"eval_duration":u64::MAX}));
    assert_eq!(
        large.snapshot().eval_duration_ns.as_deref(),
        Some("18446744073709551615")
    );
}
#[test]
fn missing_invalid_and_zero_duration_do_not_invent_rate_or_coerce_units() {
    for raw in [
        json!({"done":true,"eval_count":18}),
        json!({"done":true,"eval_duration":2000000000u64}),
        json!({"done":true,"eval_count":-1,"eval_duration":1}),
        json!({"done":true,"eval_count":1.2,"eval_duration":1}),
        json!({"done":true,"eval_count":u64::MAX,"eval_duration":1}),
        json!({"done":true,"eval_count":1,"eval_duration":-1}),
        json!({"done":true,"eval_count":1,"eval_duration":1.2}),
        json!({"done":true,"eval_count":1,"eval_duration":0}),
        json!({"done":true,"eval_count":1,"eval_duration":2,"total_duration":1}),
        json!({"done":true,"eval_count":1,"eval_duration":2,"error":"owned"}),
        json!({"done":true,"credits":18,"duration_ms":2}),
    ] {
        let (g, _) = complete();
        g.observe(&raw);
        assert_eq!(g.snapshot().rate_tokens_per_second, None, "{raw}");
        assert_eq!(g.snapshot().state, "unknown");
    }
}
#[test]
fn nonfinal_duplicate_and_noncompleted_calls_are_not_valid_samples() {
    let (g, _) = complete();
    g.observe(&json!({"done":false,"eval_count":18,"eval_duration":2}));
    assert_eq!(g.snapshot().state, "unreported");
    g.observe(&json!({"done":true,"eval_count":18,"eval_duration":2000000000u64}));
    g.observe(&json!({"done":true,"eval_count":0,"eval_duration":2000000000u64}));
    assert_eq!(g.snapshot().reason, Some("multiple_final_reports"));
    assert_eq!(g.snapshot().rate_tokens_per_second, None);
    for terminal in [
        TimingTerminal::Failed,
        TimingTerminal::Cancelled,
        TimingTerminal::Incomplete,
    ] {
        let (g, time) = complete();
        let mut next = AttemptTiming::new(TimingMode::Streaming);
        next.finish(terminal, Duration::from_millis(10)).unwrap();
        *time.lock() = Some(next);
        g.observe(&json!({"done":true,"eval_count":18,"eval_duration":2000000000u64}));
        assert_eq!(g.snapshot().rate_tokens_per_second, None);
        assert_eq!(g.snapshot().state, "not_completed");
    }
}
#[test]
fn separate_invocations_and_shared_late_serialization_do_not_mix_reports() {
    let (a, _) = complete();
    let (b, _) = complete();
    let clone = a.clone();
    assert_eq!(clone.snapshot().rate_tokens_per_second, None);
    a.observe(&json!({"done":true,"eval_count":18,"eval_duration":2000000000u64}));
    b.observe(&json!({"done":true,"eval_count":6,"eval_duration":3000000000u64}));
    assert_eq!(clone.snapshot().rate_tokens_per_second, Some(9.0));
    assert_eq!(b.snapshot().rate_tokens_per_second, Some(2.0));
}
