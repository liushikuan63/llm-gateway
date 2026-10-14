use llm_gateway_lib::client_usage::scan_from_home;

#[test]
fn real_shape_mirror_before_first_modern_timestamp_is_counted_only_once() {
    let h = Home::new();
    let mut baseline = old(100, 10);
    baseline["timestamp"] = json!("2026-10-13T03:00:00Z");
    let mut mirror = old(200, 20);
    mirror["timestamp"] = json!("2026-10-13T03:59:59.999Z");
    mirror["payload"]["info"]["total_token_usage"]["total_tokens"] = json!(220);
    mirror["payload"]["info"]["total_token_usage"]["reasoning_output_tokens"] = json!(7);
    let mut modern = record("r", "t", 100, 10, 0);
    modern["payload"]["thread_token_usage"] =
        mirror["payload"]["info"]["total_token_usage"].clone();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "fixture-model"),
            baseline,
            mirror,
            modern,
        ],
    );
    let report = scan(&h.0);
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].input_tokens, Some(100));
}
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("llmgw-client-usage-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn write(&self, file: &str, content: &str) {
        let p = self.0.join(file);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    fn jsonl(&self, file: &str, rows: Vec<Value>) {
        self.write(
            file,
            &(rows
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"),
        );
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn meta() -> Value {
    json!({"type":"session_meta","payload":{"id":"session-fixture"}})
}
fn context(turn: &str, model: &str) -> Value {
    json!({"type":"turn_context","payload":{"turn_id":turn,"model":model}})
}
fn record(id: &str, turn: &str, input: u64, output: u64, cache: u64) -> Value {
    json!({"timestamp":"2026-10-13T04:00:00Z","type":"token_usage_record","payload":{"thread_id":"session-fixture","response_id":id,"turn_id":turn,"usage":{"input_tokens":input,"output_tokens":output,"cached_input_tokens":cache,"cache_write_input_tokens":0}}})
}
fn old(input: i64, output: i64) -> Value {
    json!({"timestamp":"2026-10-13T04:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":input,"output_tokens":output,"cached_input_tokens":0,"cache_write_input_tokens":0}}}})
}
fn scan(home: &Path) -> llm_gateway_lib::client_usage::ClientUsageReport {
    scan_from_home(home, "2026-10-12", "2026-10-14").unwrap()
}

#[test]
fn range_totals_combine_days_but_daily_series_and_single_day_filter_remain_available() {
    let h = Home::new();
    h.write(".dsh/dsh-usage/usage-ledger.json", &json!({"version":1,"days":{
        "2026-10-12":{"p":{"m":{"inputTokens":10,"outputTokens":4,"cacheReadTokens":20,"cacheWriteTokens":0,"calls":2}}},
        "2026-10-13":{"p":{"m":{"inputTokens":30,"outputTokens":6,"cacheReadTokens":40,"cacheWriteTokens":0,"calls":3}}}
    }}).to_string());
    let full = serde_json::to_value(scan(&h.0)).unwrap();
    assert_eq!(full["summary_rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(full["summary_rows"][0]["input_tokens"], 100);
    assert_eq!(full["summary_rows"][0]["requests"], 5);
    assert!(full["summary_rows"][0]["day"].is_null());
    assert_eq!(full["daily_rows"].as_array().map(Vec::len), Some(2));
    let single =
        serde_json::to_value(scan_from_home(&h.0, "2026-10-13", "2026-10-13").unwrap()).unwrap();
    assert_eq!(single["summary_rows"][0]["input_tokens"], 70);
    assert_eq!(single["daily_rows"].as_array().map(Vec::len), Some(1));
}

#[test]
fn reference_estimate_counts_cached_input_once_and_prices_the_range_sum() {
    use llm_gateway_lib::client_usage::{apply_reference_prices, UsagePriceReference};
    let h = Home::new();
    h.write(".dsh/dsh-usage/usage-ledger.json", &json!({"version":1,"days":{
        "2026-10-12":{"p":{"m":{"inputTokens":10,"outputTokens":4,"cacheReadTokens":20,"cacheWriteTokens":0,"calls":2}}},
        "2026-10-13":{"p":{"m":{"inputTokens":30,"outputTokens":6,"cacheReadTokens":40,"cacheWriteTokens":0,"calls":3}}}
    }}).to_string());
    let mut report = scan(&h.0);
    assert_eq!(report.summary_rows[0].estimated_cost, None);
    let price = serde_json::from_value(
        json!({"prompt":1,"completion":2,"cache_read":0.1,"currency":"usd"}),
    )
    .unwrap();
    apply_reference_prices(
        &mut report,
        &[UsagePriceReference {
            provider: "p".into(),
            models: vec!["m".into()],
            price,
        }],
    );
    assert!((report.summary_rows[0].estimated_cost.unwrap() - 0.000066).abs() < 1e-12);
    assert_eq!(
        report.summary_rows[0].estimated_currency.as_deref(),
        Some("usd")
    );
    assert_eq!(report.daily_rows.len(), 2);
    assert!(
        (report
            .daily_rows
            .iter()
            .map(|r| r.estimated_cost.unwrap())
            .sum::<f64>()
            - 0.000066)
            .abs()
            < 1e-12
    );
}

#[test]
fn reference_estimates_keep_unknown_cache_usage_and_ambiguous_prices_unknown() {
    use llm_gateway_lib::client_usage::{apply_reference_prices, UsagePriceReference};
    let h = Home::new();
    let mut incomplete = record("r", "t", 100, 10, 20);
    incomplete["payload"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("cached_input_tokens");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "m"), incomplete],
    );
    let price = serde_json::from_value(
        json!({"prompt":1,"completion":2,"cache_read":0.1,"currency":"usd"}),
    )
    .unwrap();
    let a = UsagePriceReference {
        provider: "a".into(),
        models: vec!["m".into()],
        price,
    };
    let mut report = scan(&h.0);
    apply_reference_prices(&mut report, std::slice::from_ref(&a));
    assert_eq!(report.summary_rows[0].estimated_cost, None);
    assert_eq!(
        report.summary_rows[0].estimate_note.as_deref(),
        Some("cache_fields_unknown")
    );
    let mut b = a.clone();
    b.provider = "b".into();
    b.price.prompt = 2.;
    apply_reference_prices(&mut report, &[a, b]);
    assert_eq!(report.summary_rows[0].estimated_cost, None);
    assert_eq!(
        report.summary_rows[0].estimate_note.as_deref(),
        Some("price_ambiguous")
    );
}

#[test]
fn daily_series_preserves_codex_dates_while_legacy_and_range_rows_stay_combined() {
    let h = Home::new();
    let mut earlier = record("a", "t", 10, 1, 0);
    earlier["timestamp"] = json!("2026-10-12T04:00:00Z");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "m"),
            earlier,
            record("b", "t", 20, 2, 0),
        ],
    );
    let report = scan(&h.0);
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.summary_rows.len(), 1);
    assert_eq!(report.summary_rows[0].input_tokens, Some(30));
    assert_eq!(report.daily_rows.len(), 2);
    assert!(report.daily_rows.iter().all(|r| r.day.is_some()));
}

#[test]
fn codex_new_events_replace_mirrors_and_deduplicate_identity_not_equal_numbers() {
    let h = Home::new();
    let events = vec![
        meta(),
        context("a", "model-a"),
        old(100, 20),
        record("r1", "a", 100, 20, 60),
        record("r1", "a", 100, 20, 60),
        record("r2", "a", 100, 20, 0),
        context("b", "model-b"),
        record("r3", "b", 50, 10, 0),
        record("r4", "missing", 7, 3, 0),
    ];
    h.jsonl(".codex/sessions/a.jsonl", events.clone());
    h.jsonl(".codex/sessions/copy.jsonl", events);
    let report = scan(&h.0);
    assert_eq!(report.rows.len(), 3);
    let a = report.rows.iter().find(|r| r.model == "model-a").unwrap();
    assert_eq!(
        (a.input_tokens, a.output_tokens, a.cache_read_tokens),
        (Some(200), Some(40), Some(60))
    );
    assert_eq!(a.requests, Some(2));
    assert_eq!(
        (a.cache_hit_requests, a.cache_miss_requests),
        (Some(1), Some(1))
    );
    assert!(report
        .rows
        .iter()
        .any(|r| r.model == "unknown" && r.input_tokens == Some(7)));
    assert!(report
        .rows
        .iter()
        .any(|r| r.model == "model-b" && r.input_tokens == Some(50)));
}
#[test]
fn dsh_profiles_are_separate_snapshots_input_includes_cache_and_hit_counts_unknown() {
    let h = Home::new();
    let ledger = json!({"version":1,"days":{"2026-10-13":{"fixture-provider":{"fixture-model":{"inputTokens":10,"outputTokens":40,"cacheReadTokens":20,"cacheWriteTokens":30,"calls":1}}}}});
    for profile in [".dsh", ".dsh-beta"] {
        h.write(
            &format!("{profile}/dsh-usage/usage-ledger.json"),
            &ledger.to_string(),
        );
    }
    let a = scan(&h.0);
    let b = scan(&h.0);
    assert_eq!(a.rows, b.rows);
    assert_eq!(a.rows.len(), 2);
    for row in a.rows {
        assert_eq!(row.input_tokens, Some(60));
        assert_eq!(row.output_tokens, Some(40));
        assert_eq!(row.requests, Some(1));
        assert_eq!(
            (
                row.cache_hit_requests,
                row.cache_miss_requests,
                row.cache_unknown_requests
            ),
            (None, None, None)
        );
        assert_eq!(row.day.as_deref(), Some("2026-10-13"));
    }
    assert!(scan_from_home(&h.0, "2026-10-14", "2026-10-15")
        .unwrap()
        .rows
        .is_empty());
}
#[test]
fn qoder_placeholder_zero_is_unknown_and_request_index_does_not_deduplicate() {
    let h = Home::new();
    let event = |id: &str| json!({"ts":"2026-10-13T04:00:00Z","type":"model.response.completed","request_id":id,"data":{"model":"auto","request_index":1,"content_block_count":1,"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}});
    h.jsonl(
        ".qoder/logs/sessions/project/session/segments/a.jsonl",
        vec![
            event("a"),
            event("b"),
            event("a"),
            json!({"type":"turn.finished","data":{"input_tokens":100}}),
        ],
    );
    let r = scan(&h.0).rows.remove(0);
    assert_eq!(r.requests, Some(2));
    assert_eq!(r.unknown_usage_records, 2);
    assert_eq!(r.input_tokens, None);
    assert_eq!(r.output_tokens, None);
    assert_eq!(r.cache_unknown_requests, Some(2));
    assert_eq!(r.cache_miss_requests, Some(0));
}
#[test]
fn legacy_counters_use_deltas_ignore_duplicates_and_report_resets() {
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/old.jsonl",
        vec![
            meta(),
            context("t", "legacy"),
            old(100, 10),
            old(200, 20),
            old(200, 20),
            old(50, 5),
        ],
    );
    let report = scan(&h.0);
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].input_tokens, Some(100));
    assert_eq!(report.rows[0].requests, None);
    assert_eq!(report.rows[0].granularity, "cumulative_delta");
    assert!(report.sources[0].notes.iter().any(|n| n == "counter_reset"));
}
#[test]
fn missing_and_negative_fields_remain_unknown_or_explicit_partial_totals() {
    let h = Home::new();
    let mut bad = record("r2", "t", 2, 2, 0);
    bad["payload"]["usage"]["input_tokens"] = json!(-7);
    bad["payload"]["usage"]["cached_input_tokens"] = Value::Null;
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "fixture-model"),
            record("r1", "t", 10, 2, 3),
            bad,
        ],
    );
    let row = scan(&h.0).rows.remove(0);
    assert_eq!(row.input_tokens, Some(10));
    assert!(row.partial_fields.contains(&"input_tokens".into()));
    assert!(row.partial_fields.contains(&"cache_read_tokens".into()));
    assert_eq!(row.cache_unknown_requests, Some(1));
}
#[test]
fn malformed_large_and_partial_lines_are_bounded_and_do_not_export_private_content() {
    let h = Home::new();
    let private = "private-conversation-fixture";
    let text = format!(
        "{}\nnot-json\n{}\n{{\"incomplete\":\"{}",
        record("r", "t", 7, 3, 0),
        "x".repeat(1024 * 1024 + 1),
        private
    );
    h.write(".codex/sessions/a.jsonl", &text);
    let report = scan(&h.0);
    assert_eq!(report.rows[0].input_tokens, Some(7));
    assert_eq!(report.sources[0].skipped_records, 3);
    assert_eq!(report.sources[0].status, "partial");
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains(private));
    assert!(!serialized.contains(h.0.to_string_lossy().as_ref()));
    assert!(scan_from_home(&h.0, "2026-10-14", "2026-10-13").is_err());
    assert!(scan_from_home(&h.0, "invalid", "2026-10-13").is_err());
}
#[test]
fn absent_sources_are_unavailable_and_never_fabricate_zero_token_rows() {
    let h = Home::new();
    let r = scan(&h.0);
    assert!(r.rows.is_empty());
    assert!(r.sources.iter().all(|s| s.status == "unavailable"));
}

#[test]
fn tail_without_session_meta_does_not_add_legacy_mirrors_to_modern_usage() {
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/tail.jsonl",
        vec![
            context("t", "fixture-model"),
            old(100, 10),
            old(200, 20),
            record("r1", "t", 100, 10, 0),
        ],
    );
    let r = scan(&h.0);
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].input_tokens, Some(100));
}

#[test]
fn dsh_missing_cache_field_preserves_known_input_lower_bound() {
    let h = Home::new();
    h.write(".dsh/dsh-usage/usage-ledger.json",&json!({"version":1,"days":{"2026-10-13":{"p":{"m":{"inputTokens":10,"outputTokens":4,"cacheReadTokens":20,"calls":1}}}}}).to_string());
    let r = scan(&h.0).rows.remove(0);
    assert_eq!(r.input_tokens, Some(30));
    assert!(r.partial_fields.contains(&"input_tokens".into()));
    assert_eq!(r.cache_write_tokens, None);
}

#[test]
fn legacy_history_before_modern_upgrade_is_not_silently_hidden() {
    let h = Home::new();
    let mut a = old(100, 10);
    a["timestamp"] = json!("2026-10-12T04:00:00Z");
    let mut b = old(200, 20);
    b["timestamp"] = json!("2026-10-12T05:00:00Z");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "legacy"),
            a,
            b,
            record("r", "t", 50, 5, 0),
        ],
    );
    let report = scan_from_home(&h.0, "2026-10-12", "2026-10-12").unwrap();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].input_tokens, Some(100));
}
