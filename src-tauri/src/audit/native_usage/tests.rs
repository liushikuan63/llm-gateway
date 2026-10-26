use super::*;
use serde_json::json;

fn marker(usage: Value) -> Vec<Value> {
    vec![json!({"native_exact":{"version":1,"usage_evidence":usage}})]
}

fn usage(prompt: Value, completion: Value, total: Value) -> Value {
    json!({"state":"partial", "prompt_tokens":prompt,
        "completion_tokens":completion,"total_tokens":total})
}

fn native(attempts: &[Value], ts: i64) -> NativeUsage {
    match project_attempts(attempts, ts) {
        UsageProjection::Native(value) => value,
        UsageProjection::Legacy => panic!("native evidence must not fall back to scalar usage"),
    }
}

fn price() -> Value {
    json!({"prompt":2.0,"completion":8.0,"currency":"usd"})
}

fn priced(report: Value, snapshot: Value, ts: i64) -> NativeUsage {
    let mut attempts = marker(report);
    attempts[0]["native_exact"]["configured_price_snapshot"] = snapshot;
    native(&attempts, ts)
}

fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
}

#[test]
fn legacy_without_marker_is_untouched_even_with_other_usage_metadata() {
    assert_eq!(project_attempts(&[], 0), UsageProjection::Legacy);
    assert_eq!(
        project_attempts(
            &[json!({"ok":true,"usage_evidence":{"prompt_tokens":0}})],
            0
        ),
        UsageProjection::Legacy
    );
}

#[test]
fn complete_zero_is_known_and_missing_is_unknown() {
    let zero = native(&marker(usage(json!(0), json!(0), json!(0))), 0);
    assert_eq!(zero.state, NativeState::Complete);
    assert_eq!(zero.prompt_tokens, Some(0));
    assert_eq!(zero.total_tokens, Some(0));
    assert_eq!(zero.validity, NativeValidity::StateNormalized);
    let missing = native(&marker(json!({"state":"missing"})), 0);
    assert_eq!(missing.state, NativeState::Missing);
    assert_eq!(missing.total_tokens, None);
    assert_eq!(missing.credit, None);
}

#[test]
fn partial_prompt_never_manufactures_output_total_or_cache_miss() {
    let result = native(&marker(usage(json!(50), Value::Null, Value::Null)), 0);
    assert_eq!(result.state, NativeState::Partial);
    assert_eq!(result.prompt_tokens, Some(50));
    assert_eq!(result.completion_tokens, None);
    assert_eq!(result.total_tokens, None);
    assert_eq!(result.cache_read_tokens, None);
    assert_eq!(result.cache_creation_tokens, None);
    assert_eq!(result.reasoning_tokens, None);
}

#[test]
fn credit_and_optional_zero_are_evidence_without_core_counts() {
    for report in [
        json!({"state":"missing","credit":0.0}),
        json!({"state":"missing","cache_read_tokens":0}),
    ] {
        let result = native(&marker(report), 0);
        assert_eq!(result.state, NativeState::Partial);
        assert_eq!(result.prompt_tokens, None);
    }
}

#[test]
fn wrong_counts_are_invalid_without_scalar_fallback() {
    for bad in [
        json!(-1),
        json!(1.5),
        json!(4294967296_u64),
        json!("10"),
        json!(true),
    ] {
        let result = native(&marker(usage(bad, json!(0), Value::Null)), 0);
        assert_eq!(result.state, NativeState::Invalid);
        assert_eq!(result.reason, Some(NativeReason::CountInvalid));
        assert_eq!(result.prompt_tokens, None);
    }
}

#[test]
fn total_overflow_and_contradictory_subsets_are_invalid() {
    let reports = [
        usage(json!(2), json!(3), json!(4)),
        usage(json!(u32::MAX), json!(1), json!(0)),
        json!({"state":"partial","prompt_tokens":4,"cache_read_tokens":3,"cache_creation_tokens":2}),
        json!({"state":"partial","prompt_tokens":4,"cache_read_tokens":5}),
        json!({"state":"partial","completion_tokens":2,"reasoning_tokens":3}),
        json!({"state":"partial","credit":-1.0}),
    ];
    for report in reports {
        let result = native(&marker(report), 0);
        assert_eq!(result.state, NativeState::Invalid);
        assert_eq!(result.reference_fee, None);
    }
}

#[test]
fn malformed_marker_version_multiple_and_report_do_not_use_legacy() {
    for attempts in [
        vec![json!({"native_exact":null})],
        vec![json!({"native_exact":{"version":2}})],
        vec![json!({"native_exact":{"version":"1"}})],
        vec![json!({"native_exact":{"version":1,"usage_evidence":[]}})],
        vec![json!({"native_exact":{"version":1,"usage_evidence":{"state":"invented"}}})],
        vec![marker(json!({"state":"missing"}))[0].clone(); 2],
    ] {
        assert_eq!(native(&attempts, 0).state, NativeState::Invalid);
    }
}

#[test]
fn known_core_without_total_remains_partial_but_can_be_priced() {
    let result = priced(
        usage(json!(1_000_000), json!(250_000), Value::Null),
        price(),
        0,
    );
    assert_eq!(result.state, NativeState::Partial);
    assert_eq!(result.total_tokens, None);
    let fee = result.reference_fee.unwrap();
    near(fee.min, 4.0);
    near(fee.max, 4.0);
    assert_eq!(fee.exact, Some(4.0));
    assert_eq!(fee.currency, Currency::Usd);
    assert_eq!(fee.kind, "api_reference");
    assert_eq!(fee.source, "configured_price_snapshot");
}

#[test]
fn snapshot_remains_fixed_when_unrelated_current_price_changes() {
    let snapshot = price();
    let mut current_price = snapshot.clone();
    current_price["prompt"] = json!(999.0);
    let result = priced(usage(json!(1_000_000), json!(0), Value::Null), snapshot, 0);
    near(result.reference_fee.unwrap().min, 2.0);
    assert_eq!(current_price["prompt"], json!(999.0));
}

#[test]
fn unknown_cache_rates_produce_bounded_reference_range() {
    let mut snapshot = price();
    snapshot["cache_read"] = json!(0.5);
    snapshot["cache_creation"] = json!(3.0);
    let result = priced(
        usage(json!(1_000_000), json!(250_000), Value::Null),
        snapshot,
        0,
    );
    assert_eq!(result.cache_read_tokens, None);
    let fee = result.reference_fee.unwrap();
    near(fee.min, 2.5);
    near(fee.max, 5.0);
    assert_eq!(fee.exact, None);
    assert_eq!(fee.reason, ReferenceReason::CacheAllocationUnknown);
}

#[test]
fn known_cache_counts_are_fixed_while_only_unknown_category_varies() {
    let snapshot = json!({"prompt":2.0,"completion":8.0,"cache_read":0.5,"cache_creation":3.0,"currency":"usd"});
    let report = json!({"state":"partial","prompt_tokens":1_000_000,"completion_tokens":250_000,"cache_read_tokens":200_000});
    let result = priced(report, snapshot, 0);
    let fee = result.reference_fee.unwrap();
    near(fee.min, 3.7);
    near(fee.max, 4.5);
}

#[test]
fn cache_and_reasoning_subsets_are_not_double_counted() {
    let snapshot = json!({"prompt":2.0,"completion":8.0,"cache_read":0.5,"cache_creation":3.0,"currency":"usd"});
    let report = json!({"state":"complete","prompt_tokens":1_000_000,"completion_tokens":250_000,"total_tokens":1_250_000,"cache_read_tokens":200_000,"cache_creation_tokens":100_000,"reasoning_tokens":100_000});
    let result = priced(report, snapshot, 0);
    let fee = result.reference_fee.unwrap();
    near(fee.min, 3.8);
    near(fee.max, 3.8);
    assert_eq!(fee.exact, Some(fee.min));
}

#[test]
fn equal_cache_rates_collapse_without_claiming_known_cache() {
    let result = priced(usage(json!(1_000_000), json!(0), Value::Null), price(), 0);
    assert_eq!(result.cache_read_tokens, None);
    assert_eq!(result.cache_creation_tokens, None);
    assert_eq!(result.reference_fee.unwrap().exact, Some(2.0));
}

#[test]
fn missing_input_or_output_has_no_total_reference() {
    for report in [
        usage(json!(10), Value::Null, Value::Null),
        usage(Value::Null, json!(10), Value::Null),
    ] {
        let result = priced(report, price(), 0);
        assert_eq!(result.reference_fee, None);
        assert_eq!(
            result.reference_unavailable_reason,
            Some(ReferenceReason::CoreUsageMissing)
        );
    }
}

#[test]
fn configured_zero_price_differs_from_unpriced_and_bad_snapshot() {
    let report = usage(json!(100), json!(10), json!(110));
    let zero = priced(
        report.clone(),
        json!({"prompt":0,"completion":0,"currency":"cny"}),
        0,
    );
    assert_eq!(zero.reference_fee.unwrap().exact, Some(0.0));
    let unpriced = native(&marker(report.clone()), 0);
    assert_eq!(
        unpriced.reference_unavailable_reason,
        Some(ReferenceReason::SnapshotMissing)
    );
    for snapshot in [
        json!({"prompt":-1,"completion":0,"currency":"usd"}),
        json!({"prompt":0,"completion":0,"currency":"eur"}),
        json!({}),
    ] {
        let bad = priced(report.clone(), snapshot, 0);
        assert_eq!(bad.reference_fee, None);
        assert_eq!(
            bad.reference_unavailable_reason,
            Some(ReferenceReason::SnapshotInvalid)
        );
        assert_ne!(bad.state, NativeState::Invalid);
    }
}

#[test]
fn tiers_and_cross_midnight_rules_use_request_utc_timestamp() {
    let snapshot = json!({"prompt":2,"completion":8,"currency":"cny",
        "tiers":[{"min_prompt_tokens":1000,"prompt":4,"completion":10}],
        "rules":[{"label":"night","start_minute":1380,"end_minute":60,"prompt_multiplier":0.5,"completion_multiplier":0.25}]});
    for (input, ts, expected) in [
        (999, 0, 0.002999),
        (1000, 0, 0.0045),
        (1000, 3599, 0.0045),
        (1000, 3600, 0.014),
        (1000, 82800, 0.0045),
        (1000, 86400, 0.0045),
        (1000, -1, 0.0045),
    ] {
        let result = priced(
            usage(json!(input), json!(1000), Value::Null),
            snapshot.clone(),
            ts,
        );
        let fee = result.reference_fee.unwrap();
        near(fee.min, expected);
        assert_eq!(fee.currency, Currency::Cny);
    }
}

#[test]
fn extreme_valid_rates_do_not_publish_infinite_money() {
    let result = priced(
        usage(json!(u32::MAX), json!(0), Value::Null),
        json!({"prompt":1e308,"completion":0,"currency":"usd"}),
        0,
    );
    assert_eq!(result.reference_fee, None);
    assert_eq!(
        result.reference_unavailable_reason,
        Some(ReferenceReason::AmountNonFinite)
    );
}

#[test]
fn bounded_input_rejects_deep_wide_and_malformed_native_data() {
    assert_eq!(
        project_json("[", 0).native().unwrap().state,
        NativeState::Invalid
    );
    assert_eq!(
        project_json("{}", 0).native().unwrap().state,
        NativeState::Invalid
    );
    let mut attempts = marker(json!({"state":"missing"}));
    let mut nested = json!(0);
    for _ in 0..20 {
        nested = json!([nested]);
    }
    attempts[0]["native_exact"]["future"] = nested;
    assert_eq!(
        native(&attempts, 0).reason,
        Some(NativeReason::LimitsExceeded)
    );
    attempts[0]["native_exact"]["future"] = json!("x".repeat(MAX_JSON_BYTES));
    assert_eq!(
        native(&attempts, 0).reason,
        Some(NativeReason::LimitsExceeded)
    );
    let mut many = vec![json!({}); MAX_ATTEMPTS + 1];
    assert_eq!(project_attempts(&many, 0), UsageProjection::Legacy);
    many[0] = marker(json!({"state":"missing"}))[0].clone();
    assert_eq!(native(&many, 0).reason, Some(NativeReason::LimitsExceeded));
}

#[test]
fn safe_projection_omits_identity_and_snapshot() {
    let mut attempts = marker(json!({"state":"missing"}));
    attempts[0]["native_exact"]["account_id"] = json!("private-account");
    attempts[0]["native_exact"]["principal_hash"] = json!("private-principal");
    let encoded = serde_json::to_string(&native(&attempts, 0)).unwrap();
    assert!(!encoded.contains("private-account"));
    assert!(!encoded.contains("private-principal"));
    assert!(encoded.contains("\"prompt_tokens\":null"));
}

#[test]
fn accumulator_tracks_known_subtotals_and_unknown_request_counts() {
    let mut summary = UsageAccumulator::default();
    summary.add(&UsageProjection::Legacy);
    summary.add(&UsageProjection::Native(priced(
        usage(json!(0), json!(0), json!(0)),
        price(),
        0,
    )));
    summary.add(&UsageProjection::Native(priced(
        usage(json!(100), Value::Null, Value::Null),
        price(),
        0,
    )));
    summary.add(&UsageProjection::Native(native(
        &marker(json!({"state":"missing"})),
        0,
    )));
    summary.add(&UsageProjection::Native(native(
        &marker(json!({"state":"bad"})),
        0,
    )));
    assert_eq!(summary.legacy_requests, 1);
    assert_eq!(summary.native_requests, 4);
    assert_eq!(summary.complete_requests, 1);
    assert_eq!(summary.partial_requests, 1);
    assert_eq!(summary.missing_requests, 1);
    assert_eq!(summary.invalid_requests, 1);
    assert_eq!(summary.prompt_tokens.known_subtotal, 100);
    assert_eq!(summary.prompt_tokens.known_requests, 2);
    assert_eq!(summary.prompt_tokens.unknown_requests, 2);
    assert_eq!(summary.completion_tokens.known_requests, 1);
    assert_eq!(summary.completion_tokens.unknown_requests, 3);
    assert_eq!(summary.unpriced_requests, 3);
    assert_eq!(summary.reference_fees.len(), 1);
    assert_eq!(summary.reference_fees[0].priced_requests, 1);
    near(summary.reference_fees[0].min, 0.0);
}

#[test]
fn accumulator_keeps_currencies_and_range_totals_separate() {
    let mut summary = UsageAccumulator::default();
    for snapshot in [
        price(),
        json!({"prompt":2,"completion":8,"cache_read":0.5,"currency":"cny"}),
    ] {
        summary.add(&UsageProjection::Native(priced(
            usage(json!(1_000_000), json!(0), Value::Null),
            snapshot,
            0,
        )));
    }
    assert_eq!(summary.reference_fees.len(), 2);
    assert_eq!(summary.reference_fees[0].currency, Currency::Usd);
    assert_eq!(summary.reference_fees[0].min, 2.0);
    assert_eq!(summary.reference_fees[1].currency, Currency::Cny);
    assert_eq!(summary.reference_fees[1].min, 0.5);
    assert_eq!(summary.reference_fees[1].max, 2.0);
    assert_eq!(summary.reference_fees[1].range_requests, 1);
}
#[test]
fn personal_unverified_usage_never_uses_legacy_zero_or_native_receipt() {
    for outcome in ["not_dispatched", "dispatched_unknown", "reply_unverified"] {
        let rows = serde_json::json!([{"personal_test":{"version":1,"usage":"unverified","outcome":outcome}}]);
        let projected = super::project_attempts(rows.as_array().unwrap(), 0);
        let usage = projected.native().unwrap();
        assert_eq!(usage.state, super::NativeState::Missing);
        assert_eq!(
            usage.reason,
            Some(super::NativeReason::PersonalTestUnverified)
        );
        assert!(
            usage.prompt_tokens.is_none()
                && usage.completion_tokens.is_none()
                && usage.credit.is_none()
                && usage.reference_fee.is_none()
        );
    }
    for rows in [
        serde_json::json!([{"personal_test":{"version":1,"usage":"unverified","outcome":"verified"}}]),
        serde_json::json!([{"personal_test":{"version":1,"usage":"unverified","outcome":"reply_unverified"},"native_exact":{}}]),
    ] {
        assert_eq!(
            super::project_attempts(rows.as_array().unwrap(), 0)
                .native()
                .unwrap()
                .state,
            super::NativeState::Invalid
        );
    }
}
