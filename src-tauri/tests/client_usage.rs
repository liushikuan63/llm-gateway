use llm_gateway_lib::client_usage::scan_from_home;

#[test]
fn verified_dsh_pairs_price_ordinary_read_and_write_once_with_peak_envelope() {
    use llm_gateway_lib::client_usage::scan_from_home_with_prices;
    for (profile, provider, model, source) in [
        (
            ".dsh",
            "commandcode",
            "deepseek/deepseek-v4.1-flash",
            "commandcode.ai/models/deepseek-v4-1-flash",
        ),
        (
            ".dsh-beta",
            "deepseek-account",
            "deepseek-flash",
            "api-docs.deepseek.com/quick_start/pricing/",
        ),
    ] {
        let h = Home::new();
        h.write(&format!("{profile}/dsh-usage/usage-ledger.json"), &json!({"version":1,"days":{"2026-10-13":{provider:{model:{"inputTokens":10,"outputTokens":40,"cacheReadTokens":20,"cacheWriteTokens":30,"calls":1}}}}}).to_string());
        let report = scan_from_home_with_prices(&h.0, "2026-10-12", "2026-10-14", &[]).unwrap();
        let row = &report.summary_rows[0];
        assert_eq!(row.input_tokens, Some(60));
        assert_eq!(row.cache_read_tokens, Some(20));
        assert_eq!(row.cache_write_tokens, Some(30));
        assert_eq!(row.estimated_currency.as_deref(), Some("usd"));
        assert!(row.estimated_cost.is_none());
        assert!((row.estimated_cost_min.unwrap() - 0.00003006).abs() < 1e-12);
        assert!((row.estimated_cost_max.unwrap() - 0.00006012).abs() < 1e-12);
        assert_eq!(row.estimate_note.as_deref(), Some("conditional_price"));
        let reference = row.price_reference.as_deref().unwrap();
        assert!(
            reference.contains("2026-10-15") && reference.contains(source),
            "{reference}"
        );
    }
}

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

fn last_snapshot(total: i64, last: i64, lane: &str, ordinal: u32) -> Value {
    let mut event = old(total, total / 10);
    event["timestamp"] = json!(format!("2026-10-13T04:00:{ordinal:02}Z"));
    event["payload"]["rate_limits"] = json!({"limit_id":lane});
    event["payload"]["info"]["last_token_usage"] = json!({"input_tokens":last,"output_tokens":last/10,"cached_input_tokens":0,"cache_write_input_tokens":0});
    event
}

fn fork_meta(id: &str, parent: Option<&str>) -> Value {
    let mut value =
        json!({"timestamp":"2026-10-13T04:00:10Z","type":"session_meta","payload":{"id":id}});
    if let Some(parent) = parent {
        value["payload"]["forked_from_id"] = json!(parent);
    }
    value
}

#[test]
fn first_last_usage_in_a_complete_independent_session_is_a_priced_request() {
    let h = Home::new();
    let mut usage = last_snapshot(100, 100, "a", 11);
    usage["payload"]["info"]["model"] = json!("gpt-6.1-sol");
    h.jsonl(".codex/sessions/a.jsonl", vec![meta(), usage]);
    let report = llm_gateway_lib::client_usage::scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        &[],
    )
    .unwrap();
    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    assert_eq!(
        (row.input_tokens, row.output_tokens, row.requests),
        (Some(100), Some(10), Some(1))
    );
    assert_eq!(row.granularity, "last_request");
    assert_eq!(row.cache_miss_requests, Some(1));
    assert!(row.estimated_cost.is_some());
    assert!((row.estimated_cost.unwrap() - 0.0003).abs() < 1e-12);
    assert!(!report.sources[0]
        .notes
        .iter()
        .any(|n| n == "cumulative_baseline"));
}

#[test]
fn first_last_usage_keeps_uncovered_total_and_cache_unknown_explicit() {
    let h = Home::new();
    let mut usage = last_snapshot(300000, 100, "a", 11);
    usage["payload"]["info"]["last_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cached_input_tokens");
    usage["payload"]["info"]["last_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_write_input_tokens");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "gpt-6.1-sol"), usage],
    );
    let report = llm_gateway_lib::client_usage::scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        &[],
    )
    .unwrap();
    let row = &report.rows[0];
    assert_eq!((row.input_tokens, row.requests), (Some(100), Some(1)));
    assert_eq!(row.cache_read_tokens, None);
    assert_eq!(row.cache_unknown_requests, Some(1));
    assert_eq!(row.estimated_cost, None);
    assert!(row.estimated_cost_min.is_some());
    assert!(report.sources[0]
        .notes
        .iter()
        .any(|n| n == "cumulative_baseline"));
}

#[test]
fn first_last_usage_requires_raw_header_and_valid_total_constraints() {
    for (prefix, total, last) in [
        (vec![], 100, 100),
        (vec![context("t", "m"), meta()], 100, 100),
        (vec![meta()], 100, 200),
    ] {
        let h = Home::new();
        let mut rows = prefix;
        rows.push(last_snapshot(total, last, "a", 11));
        h.jsonl(".codex/sessions/a.jsonl", rows);
        let report = scan(&h.0);
        assert!(report.rows.is_empty());
        assert_eq!(report.sources[0].status, "partial");
    }
}

#[test]
fn explicit_fork_skips_only_matching_parent_prefix_and_counts_child_new_request() {
    for spawn in [false, true] {
        let h = Home::new();
        let inherited = last_snapshot(100, 100, "a", 1);
        let mut child_meta = fork_meta("child", Some("parent"));
        if spawn {
            child_meta["payload"]
                .as_object_mut()
                .unwrap()
                .remove("forked_from_id");
            child_meta["payload"]["source"] =
                json!({"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}});
        }
        h.jsonl(
            ".codex/sessions/parent.jsonl",
            vec![
                fork_meta("parent", None),
                context("p", "m"),
                inherited.clone(),
            ],
        );
        h.jsonl(
            ".codex/sessions/child.jsonl",
            vec![
                child_meta,
                context("c", "m"),
                inherited,
                last_snapshot(200, 100, "a", 11),
            ],
        );
        let report = scan(&h.0);
        assert_eq!(
            (
                report.summary_rows[0].input_tokens,
                report.summary_rows[0].requests
            ),
            (Some(200), Some(2))
        );
    }
}

#[test]
fn parent_events_after_fork_never_mask_equal_child_requests() {
    let h = Home::new();
    let inherited = last_snapshot(100, 100, "a", 1);
    h.jsonl(
        ".codex/sessions/parent.jsonl",
        vec![
            fork_meta("parent", None),
            context("p", "m"),
            inherited.clone(),
            last_snapshot(200, 100, "a", 12),
        ],
    );
    h.jsonl(
        ".codex/sessions/child.jsonl",
        vec![
            fork_meta("child", Some("parent")),
            context("c", "m"),
            inherited,
            last_snapshot(200, 100, "a", 11),
        ],
    );
    let report = scan(&h.0);
    assert_eq!(
        (
            report.summary_rows[0].input_tokens,
            report.summary_rows[0].requests
        ),
        (Some(300), Some(3))
    );
}

#[test]
fn independent_threads_with_equal_first_usage_are_both_counted() {
    let h = Home::new();
    for id in ["one", "two"] {
        h.jsonl(
            &format!(".codex/sessions/{id}.jsonl"),
            vec![
                fork_meta(id, None),
                context(id, "m"),
                last_snapshot(100, 100, "a", 11),
            ],
        );
    }
    let report = scan(&h.0);
    assert_eq!(
        (
            report.summary_rows[0].input_tokens,
            report.summary_rows[0].requests
        ),
        (Some(200), Some(2))
    );
}

#[test]
fn fork_baseline_outside_date_range_still_prevents_double_counting() {
    let h = Home::new();
    let mut inherited = last_snapshot(100, 100, "a", 1);
    inherited["timestamp"] = json!("2026-10-12T04:00:01Z");
    h.jsonl(
        ".codex/sessions/parent.jsonl",
        vec![
            fork_meta("parent", None),
            context("p", "m"),
            inherited.clone(),
        ],
    );
    h.jsonl(
        ".codex/sessions/child.jsonl",
        vec![
            fork_meta("child", Some("parent")),
            context("c", "m"),
            inherited,
            last_snapshot(200, 100, "a", 11),
        ],
    );
    let report = scan_from_home(&h.0, "2026-10-13", "2026-10-13").unwrap();
    assert_eq!(
        (
            report.summary_rows[0].input_tokens,
            report.summary_rows[0].requests
        ),
        (Some(100), Some(1))
    );
}

#[test]
fn unresolved_conflicting_self_and_cyclic_parents_do_not_guess_first_request() {
    for case in ["missing", "conflicting", "self", "cycle", "malformed_spawn"] {
        let h = Home::new();
        let mut header = fork_meta(
            "child",
            Some(if case == "self" { "child" } else { "parent" }),
        );
        if case == "conflicting" {
            header["payload"]["source"] =
                json!({"subagent":{"thread_spawn":{"parent_thread_id":"other"}}});
        }
        if case == "malformed_spawn" {
            header["payload"]
                .as_object_mut()
                .unwrap()
                .remove("forked_from_id");
            header["payload"]["source"] = json!({"subagent":{"thread_spawn":{}}});
        }
        if case == "cycle" {
            h.jsonl(
                ".codex/sessions/parent.jsonl",
                vec![
                    fork_meta("parent", Some("child")),
                    last_snapshot(100, 100, "a", 1),
                ],
            );
        }
        h.jsonl(
            ".codex/sessions/child.jsonl",
            vec![header, last_snapshot(100, 100, "a", 1)],
        );
        let report = scan(&h.0);
        assert!(report.rows.is_empty(), "{case}");
        assert_eq!(report.sources[0].status, "partial");
        assert!(
            report.sources[0]
                .notes
                .iter()
                .any(|n| n.starts_with("fork_")),
            "{case}"
        );
    }
}

#[test]
fn tail_origin_is_per_file_and_a_header_inside_the_tail_does_not_prove_start() {
    use std::io::{Seek, SeekFrom, Write};
    let h = Home::new();
    h.write(".codex/sessions/tail.jsonl", "");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(h.0.join(".codex/sessions/tail.jsonl"))
        .unwrap();
    file.set_len(33 * 1024 * 1024).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    writeln!(
        file,
        "\n{}\n{}",
        fork_meta("tail", None),
        last_snapshot(100, 100, "a", 11)
    )
    .unwrap();
    drop(file);
    h.jsonl(
        ".codex/sessions/full.jsonl",
        vec![
            fork_meta("full", None),
            context("t", "m"),
            last_snapshot(100, 100, "a", 11),
        ],
    );
    let report = scan(&h.0);
    assert_eq!(report.summary_rows.len(), 1);
    assert_eq!(
        (
            report.summary_rows[0].input_tokens,
            report.summary_rows[0].requests
        ),
        (Some(100), Some(1))
    );
    assert!(report.sources[0].notes.iter().any(|n| n == "tail_only"));
}

#[test]
fn incomplete_parent_unknown_cutoff_and_conflicting_headers_leave_fork_partial() {
    for case in ["no_header", "bad_line", "cutoff", "metadata"] {
        let h = Home::new();
        let mut child_header = fork_meta("child", Some("parent"));
        if case == "cutoff" {
            child_header.as_object_mut().unwrap().remove("timestamp");
        }
        let inherited = last_snapshot(100, 100, "a", 1);
        let mut parent = vec![context("p", "m"), inherited.clone()];
        if case != "no_header" {
            parent.insert(0, fork_meta("parent", None));
        }
        h.jsonl(".codex/sessions/parent.jsonl", parent);
        if case == "bad_line" {
            use std::io::Write;
            writeln!(
                fs::OpenOptions::new()
                    .append(true)
                    .open(h.0.join(".codex/sessions/parent.jsonl"))
                    .unwrap(),
                "invalid"
            )
            .unwrap();
        }
        let mut child = vec![child_header, context("c", "m"), inherited];
        if case == "metadata" {
            child.insert(1, fork_meta("child", Some("other")));
        }
        h.jsonl(".codex/sessions/child.jsonl", child);
        let report = scan(&h.0);
        assert_eq!(report.sources[0].status, "partial", "{case}");
        assert!(
            report.sources[0]
                .notes
                .iter()
                .any(|n| n.starts_with("fork_")),
            "{case}"
        );
        // No guessed child first request; any retained request is the independent parent.
        assert!(
            report
                .summary_rows
                .iter()
                .all(|row| row.requests == Some(1)),
            "{case}"
        );
    }
}

#[test]
fn multiple_rewritten_fork_prefix_events_are_background_and_post_fork_requests_stay_real() {
    let h = Home::new();
    let first = last_snapshot(100, 100, "a", 1);
    let second = last_snapshot(200, 100, "a", 2);
    h.jsonl(
        ".codex/sessions/parent.jsonl",
        vec![
            fork_meta("parent", None),
            context("p", "m"),
            first.clone(),
            second.clone(),
            last_snapshot(300, 100, "a", 12),
        ],
    );
    let mut copied_first = first;
    let mut copied_second = second;
    copied_first["timestamp"] = json!("2026-10-13T04:00:10Z");
    copied_second["timestamp"] = json!("2026-10-13T04:00:10Z");
    h.jsonl(
        ".codex/sessions/child.jsonl",
        vec![
            fork_meta("child", Some("parent")),
            context("c", "m"),
            copied_first,
            copied_second,
            last_snapshot(300, 100, "a", 11),
        ],
    );
    let report = scan(&h.0);
    assert_eq!(
        (
            report.summary_rows[0].input_tokens,
            report.summary_rows[0].requests
        ),
        (Some(400), Some(4))
    );
    assert!(!report.sources[0]
        .notes
        .iter()
        .any(|note| note == "fork_prefix_unconfirmed"));
}

#[test]
fn unavailable_parent_history_is_unknown_but_explicit_post_fork_last_is_a_priced_request() {
    for with_history in [false, true] {
        let h = Home::new();
        let mut events = vec![
            fork_meta("child", Some("missing-parent")),
            context("c", "gpt-6.1-sol"),
        ];
        if with_history {
            events.extend([
                last_snapshot(100, 100, "a", 1),
                last_snapshot(200, 100, "a", 2),
                last_snapshot(200, 100, "a", 10),
            ]);
        }
        events.push(last_snapshot(300, 100, "a", 11));
        h.jsonl(".codex/sessions/child.jsonl", events);
        let report = llm_gateway_lib::client_usage::scan_from_home_with_prices(
            &h.0,
            "2026-10-12",
            "2026-10-14",
            &[],
        )
        .unwrap();
        let row = &report.summary_rows[0];
        assert_eq!(
            (row.input_tokens, row.output_tokens, row.requests),
            (Some(100), Some(10), Some(1))
        );
        assert!((row.estimated_cost.unwrap() - 0.0003).abs() < 1e-12);
        assert_eq!(report.sources[0].status, "partial");
        assert!(report.sources[0]
            .notes
            .iter()
            .any(|note| note == "fork_parent_unavailable"));
    }
}

#[test]
fn fork_without_reliable_cutoff_does_not_guess_any_legacy_request_or_fabricate_zero() {
    let h = Home::new();
    let mut header = fork_meta("child", Some("missing-parent"));
    header.as_object_mut().unwrap().remove("timestamp");
    h.jsonl(
        ".codex/sessions/child.jsonl",
        vec![
            header,
            context("c", "gpt-6.1-sol"),
            last_snapshot(100, 100, "a", 1),
            last_snapshot(200, 100, "a", 2),
            last_snapshot(300, 100, "a", 11),
        ],
    );
    let report = llm_gateway_lib::client_usage::scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        &[],
    )
    .unwrap();
    assert!(report.rows.is_empty());
    assert!(report.summary_rows.is_empty());
    assert_eq!(report.sources[0].status, "partial");
    assert!(report.sources[0]
        .notes
        .iter()
        .any(|note| note == "fork_time_unknown"));
}

#[test]
fn invalid_parent_provenance_preserves_valid_child_cutoff_and_explicit_own_last() {
    for case in ["self", "cycle", "malformed_parent"] {
        for with_history in [false, true] {
            let h = Home::new();
            if case != "self" {
                let mut parent = fork_meta("parent", Some("child"));
                if case == "malformed_parent" {
                    parent["payload"]["source"] =
                        json!({"subagent":{"thread_spawn":{"parent_thread_id":"other"}}});
                }
                h.jsonl(
                    ".codex/sessions/parent.jsonl",
                    vec![parent, last_snapshot(100, 100, "a", 1)],
                );
            }
            let mut events = vec![
                fork_meta(
                    "child",
                    Some(if case == "self" { "child" } else { "parent" }),
                ),
                context("c", "gpt-6.1-sol"),
            ];
            if with_history {
                events.extend([
                    last_snapshot(100, 100, "a", 1),
                    last_snapshot(200, 100, "a", 2),
                ]);
            }
            events.push(last_snapshot(300, 100, "a", 11));
            h.jsonl(".codex/sessions/child.jsonl", events);
            let report = llm_gateway_lib::client_usage::scan_from_home_with_prices(
                &h.0,
                "2026-10-12",
                "2026-10-14",
                &[],
            )
            .unwrap();
            assert_eq!(report.summary_rows.len(), 1, "{case}/{with_history}");
            let row = &report.summary_rows[0];
            assert_eq!(
                (row.input_tokens, row.output_tokens, row.requests),
                (Some(100), Some(10), Some(1)),
                "{case}/{with_history}"
            );
            assert!((row.estimated_cost.unwrap() - 0.0003).abs() < 1e-12);
            assert_eq!(report.sources[0].status, "partial");
            assert!(report.sources[0].notes.iter().any(|note| note
                == if case == "malformed_parent" {
                    "fork_metadata_conflict"
                } else {
                    "fork_parent_cycle"
                }));
        }
    }
}

#[test]
fn conflicting_child_id_or_cutoff_header_cannot_prove_any_owned_legacy_request() {
    for conflict in ["id", "time"] {
        let h = Home::new();
        let mut repeated = fork_meta("child", Some("missing-parent"));
        if conflict == "id" {
            repeated["payload"]["id"] = json!("different-child");
        } else {
            repeated["timestamp"] = json!("2026-10-13T04:00:09Z");
        }
        h.jsonl(
            ".codex/sessions/child.jsonl",
            vec![
                fork_meta("child", Some("missing-parent")),
                repeated,
                context("c", "gpt-6.1-sol"),
                last_snapshot(100, 100, "a", 1),
                last_snapshot(200, 100, "a", 11),
                last_snapshot(300, 100, "a", 12),
            ],
        );
        let report = llm_gateway_lib::client_usage::scan_from_home_with_prices(
            &h.0,
            "2026-10-12",
            "2026-10-14",
            &[],
        )
        .unwrap();
        assert!(report.rows.is_empty(), "{conflict}");
        assert_eq!(report.sources[0].status, "partial");
        assert!(report.sources[0]
            .notes
            .iter()
            .any(|note| note == "fork_metadata_conflict"));
    }
}

#[test]
fn legacy_interleaved_counters_use_exact_last_usage_and_ignore_lane_notifications() {
    let h = Home::new();
    let a = last_snapshot(1000, 100, "a", 1);
    let b = last_snapshot(2000, 200, "b", 2);
    let c = last_snapshot(1100, 100, "a", 3);
    let mut repeat = b.clone();
    repeat["timestamp"] = json!("2026-10-13T04:00:04Z");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "m"), old(0, 0), a, b, c, repeat],
    );
    let report = scan(&h.0);
    let row = &report.summary_rows[0];
    assert_eq!(
        (row.input_tokens, row.output_tokens, row.requests),
        (Some(400), Some(40), Some(3))
    );
}

#[test]
fn legacy_without_last_usage_does_not_rebill_recovered_cumulative_high_water() {
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "m"),
            old(0, 0),
            old(100, 10),
            old(200, 20),
            old(100, 10),
            old(200, 20),
        ],
    );
    let report = scan(&h.0);
    assert_eq!(report.summary_rows[0].input_tokens, Some(200));
    assert!(report.sources[0].notes.iter().any(|n| n == "counter_reset"));
}

#[test]
fn legacy_snapshot_last_field_backfill_does_not_bill_the_same_cumulative_position_twice() {
    let h = Home::new();
    let mut incomplete = old(100, 10);
    incomplete["payload"]["rate_limits"] = json!({"limit_id":"a"});
    let completed = last_snapshot(100, 100, "a", 1);
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "m"), old(0, 0), incomplete, completed],
    );
    let report = scan(&h.0);
    assert_eq!(
        (
            report.summary_rows[0].input_tokens,
            report.summary_rows[0].output_tokens
        ),
        (Some(100), Some(10))
    );
}

#[test]
fn missing_cumulative_cache_baseline_cannot_rebill_cache_already_reported_in_last_usage() {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    let h = Home::new();
    let mut first = last_snapshot(100, 100, "a", 1);
    first["payload"]["info"]["total_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cached_input_tokens");
    first["payload"]["info"]["last_token_usage"]["cached_input_tokens"] = json!(60);
    let mut second = old(200, 20);
    second["timestamp"] = json!("2026-10-13T04:00:02Z");
    second["payload"]["info"]["total_token_usage"]["cached_input_tokens"] = json!(100);
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "m"), old(0, 0), first, second],
    );
    let price = serde_json::from_value(
        json!({"prompt":1,"completion":1,"cache_read":0.1,"cache_creation":1,"currency":"usd"}),
    )
    .unwrap();
    let report = scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        &[UsagePriceReference {
            provider: "p".into(),
            models: vec!["m".into()],
            price,
        }],
    )
    .unwrap();
    let row = &report.summary_rows[0];
    assert_eq!(row.input_tokens, Some(200));
    assert_eq!(row.cache_read_tokens, Some(60));
    assert!(row.partial_fields.contains(&"cache_read_tokens".into()));
    assert!(row.estimated_cost.is_none());
    assert!(
        row.estimated_cost_min.unwrap() <= 0.00013 && row.estimated_cost_max.unwrap() >= 0.00013
    );
}

#[test]
fn revising_known_cache_at_a_counted_position_invalidates_its_old_fee_and_future_cache_delta() {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    let h = Home::new();
    let first = last_snapshot(100, 100, "a", 1);
    let mut revision = last_snapshot(100, 100, "a", 2);
    revision["payload"]["info"]["total_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cached_input_tokens");
    revision["payload"]["info"]["last_token_usage"]["cached_input_tokens"] = json!(60);
    let mut next = old(200, 20);
    next["timestamp"] = json!("2026-10-13T04:00:03Z");
    next["payload"]["info"]["total_token_usage"]["cached_input_tokens"] = json!(60);
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "model-a"),
            old(0, 0),
            first,
            revision,
            context("next", "model-b"),
            next,
        ],
    );
    let price = serde_json::from_value(
        json!({"prompt":1,"completion":1,"cache_read":0.1,"cache_creation":1,"currency":"usd"}),
    )
    .unwrap();
    let report = scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        &[UsagePriceReference {
            provider: "p".into(),
            models: vec!["model-a".into(), "model-b".into()],
            price,
        }],
    )
    .unwrap();
    for row in &report.summary_rows {
        assert_eq!(row.input_tokens, Some(100));
        assert_eq!(row.cache_read_tokens, None);
        assert!(row.estimated_cost.is_none());
    }
    assert_eq!(report.summary_rows.len(), 2);
}

#[test]
fn revised_cache_moves_old_request_classification_to_unknown_and_keeps_future_known_requests() {
    let h = Home::new();
    let first = last_snapshot(100, 100, "a", 1);
    let mut revision = last_snapshot(100, 100, "a", 2);
    revision["payload"]["info"]["total_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cached_input_tokens");
    revision["payload"]["info"]["last_token_usage"]["cached_input_tokens"] = json!(60);
    let mut next = last_snapshot(200, 100, "a", 3);
    next["payload"]["info"]["total_token_usage"]["cached_input_tokens"] = json!(60);
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "m"), old(0, 0), first, revision, next],
    );
    let report = scan(&h.0);
    let row = &report.summary_rows[0];
    assert_eq!(row.requests, Some(2));
    assert_eq!(
        (
            row.cache_hit_requests,
            row.cache_miss_requests,
            row.cache_unknown_requests
        ),
        (Some(0), Some(1), Some(1))
    );
}

#[test]
fn legacy_stale_other_lane_signature_does_not_swallow_a_genuine_last_usage_reset() {
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "m"),
            old(0, 0),
            last_snapshot(100, 10, "a", 1),
            last_snapshot(200, 20, "a", 2),
            last_snapshot(300, 30, "b", 3),
            last_snapshot(100, 10, "b", 4),
        ],
    );
    let report = scan(&h.0);
    assert_eq!(
        (
            report.summary_rows[0].input_tokens,
            report.summary_rows[0].requests
        ),
        (Some(70), Some(4))
    );
}

#[test]
fn legacy_thread_settings_and_explicit_usage_model_update_attribution() {
    let h = Home::new();
    let mut explicit = old(400, 40);
    explicit["payload"]["info"]["model"] = json!("model-c");
    h.jsonl(".codex/sessions/a.jsonl",vec![meta(),context("t","model-a"),old(100,10),old(200,20),json!({"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"model-b"}}}),old(300,30),explicit]);
    let report = scan(&h.0);
    assert_eq!(report.summary_rows.len(), 3);
    for name in ["model-a", "model-b", "model-c"] {
        assert_eq!(
            report
                .summary_rows
                .iter()
                .find(|r| r.model == name)
                .unwrap()
                .input_tokens,
            Some(100)
        );
    }
}

#[test]
fn legacy_cumulative_window_does_not_use_its_final_event_time_as_all_request_times() {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "m"), old(0, 0), old(100, 10)],
    );
    let price = serde_json::from_value(json!({"prompt":1,"completion":1,"currency":"usd","rules":[{"label":"discount","start_minute":240,"end_minute":241,"prompt_multiplier":0.1,"completion_multiplier":0.1}]})).unwrap();
    let report = scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        &[UsagePriceReference {
            provider: "p".into(),
            models: vec!["m".into()],
            price,
        }],
    )
    .unwrap();
    let row = &report.summary_rows[0];
    assert!(row.estimated_cost.is_none());
    assert!((row.estimated_cost_min.unwrap() - 0.000011).abs() < 1e-12);
    assert!((row.estimated_cost_max.unwrap() - 0.000110).abs() < 1e-12);
}

#[test]
fn aggregate_fee_envelope_contains_mixed_requests_with_opposite_tier_or_time_prices() {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    for condition in [
        json!({"tiers":[{"min_prompt_tokens":101,"prompt":1,"completion":10}]}),
        json!({"rules":[{"label":"opposite","start_minute":240,"end_minute":241,"prompt_multiplier":0.1,"completion_multiplier":10}]}),
    ] {
        let h = Home::new();
        h.write(".dsh/dsh-usage/usage-ledger.json", &json!({"version":1,"days":{"2026-10-13":{"p":{"m":{"inputTokens":200,"outputTokens":1000,"cacheReadTokens":0,"cacheWriteTokens":0,"calls":2}}}}}).to_string());
        let mut price = json!({"prompt":10,"completion":1,"currency":"usd"});
        price
            .as_object_mut()
            .unwrap()
            .extend(condition.as_object().unwrap().clone());
        let report = scan_from_home_with_prices(
            &h.0,
            "2026-10-12",
            "2026-10-14",
            &[UsagePriceReference {
                provider: "p".into(),
                models: vec!["m".into()],
                price: serde_json::from_value(price).unwrap(),
            }],
        )
        .unwrap();
        let row = &report.summary_rows[0];
        // Request A: input=99/output=1000 at base; B: input=101/output=0 at alternate price.
        assert!(row.estimated_cost_min.unwrap() <= 0.002091);
        // The reverse allocation may cost more than either homogeneous aggregate.
        assert!(row.estimated_cost_max.unwrap() >= 0.011091);
        assert!(row.estimated_cost.is_none());
    }
}

#[test]
fn explicit_non_openai_provider_survives_aliases_and_rejected_names() {
    use llm_gateway_lib::client_usage::scan_from_home_with_prices;
    for provider in ["different-supplier", "invalid provider/private"] {
        let h = Home::new();
        let mut metadata = meta();
        metadata["payload"]["model_provider"] = json!(provider);
        let mut event = record("one", "t", 100, 10, 0);
        event["payload"]["thread_id"] = json!("renamed-thread");
        h.jsonl(
            ".codex/sessions/a.jsonl",
            vec![metadata, context("t", "gpt-6.1-sol"), event],
        );
        let report = scan_from_home_with_prices(&h.0, "2026-10-12", "2026-10-14", &[]).unwrap();
        assert!(report.summary_rows[0].estimated_cost_min.is_none());
        assert!(report.summary_rows[0].price_reference.is_none());
    }
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
fn event_prices_keep_short_requests_in_short_tier_when_daily_sum_is_long() {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "m"),
            record("one", "t", 100000, 1000, 0),
            record("two", "t", 100000, 1000, 0),
            record("three", "t", 100000, 1000, 0),
        ],
    );
    let price=serde_json::from_value(json!({"prompt":2,"completion":10,"cache_read":0.1,"cache_creation":2.5,"currency":"usd","tiers":[{"min_prompt_tokens":272001,"prompt":4,"completion":15,"cache_read":0.2,"cache_creation":5}]})).unwrap();
    let report = scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        &[UsagePriceReference {
            provider: "configured".into(),
            models: vec!["m".into()],
            price,
        }],
    )
    .unwrap();
    assert!((report.summary_rows[0].estimated_cost.unwrap() - 0.63).abs() < 1e-10);
}

#[test]
fn known_openai_model_uses_dated_standard_reference_without_a_configured_provider() {
    use llm_gateway_lib::client_usage::scan_from_home_with_prices;
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "gpt-6.1-sol"),
            record("one", "t", 200000, 2000, 180000),
        ],
    );
    let report = scan_from_home_with_prices(&h.0, "2026-10-12", "2026-10-14", &[]).unwrap();
    assert!((report.summary_rows[0].estimated_cost.unwrap() - 0.078).abs() < 1e-10);
    assert!(report.summary_rows[0]
        .price_reference
        .as_deref()
        .unwrap()
        .contains("2026-10-15"));
}

#[test]
fn missing_cache_writes_produces_fee_bounds_instead_of_zero_or_a_fake_exact_cost() {
    use llm_gateway_lib::client_usage::scan_from_home_with_prices;
    let h = Home::new();
    let mut event = record("one", "t", 200000, 2000, 180000);
    event["payload"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_write_input_tokens");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "gpt-6.1-sol"), event],
    );
    let report = serde_json::to_value(
        scan_from_home_with_prices(&h.0, "2026-10-12", "2026-10-14", &[]).unwrap(),
    )
    .unwrap();
    assert!(report["summary_rows"][0]["estimated_cost"].is_null());
    assert!(
        (report["summary_rows"][0]["estimated_cost_min"]
            .as_f64()
            .unwrap()
            - 0.078)
            .abs()
            < 1e-10
    );
    assert!(
        (report["summary_rows"][0]["estimated_cost_max"]
            .as_f64()
            .unwrap()
            - 0.088)
            .abs()
            < 1e-10
    );
}

#[test]
fn official_tier_boundary_and_legacy_last_usage_match_are_priced_per_request() {
    use llm_gateway_lib::client_usage::scan_from_home_with_prices;
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "gpt-6.1-sol"),
            record("short", "t", 272000, 1000, 0),
            record("long", "t", 272001, 1000, 0),
        ],
    );
    let r = scan_from_home_with_prices(&h.0, "2026-10-12", "2026-10-14", &[]).unwrap();
    assert!((r.summary_rows[0].estimated_cost.unwrap() - (0.554 + 1.103004)).abs() < 1e-10);
    let h = Home::new();
    let mut base = old(0, 0);
    base["payload"]["info"]["total_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_write_input_tokens");
    let mut next = old(300000, 1000);
    next["payload"]["info"]["total_token_usage"]["cached_input_tokens"] = json!(200000);
    next["payload"]["info"]["total_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_write_input_tokens");
    next["payload"]["info"]["last_token_usage"] =
        next["payload"]["info"]["total_token_usage"].clone();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "gpt-6.1-sol"), base, next],
    );
    let r = scan_from_home_with_prices(&h.0, "2026-10-12", "2026-10-14", &[]).unwrap();
    assert!((r.summary_rows[0].estimated_cost_min.unwrap() - 0.455).abs() < 1e-10);
    assert!((r.summary_rows[0].estimated_cost_max.unwrap() - 0.555).abs() < 1e-10);
}

#[test]
fn nested_cache_usage_and_utc_time_rules_are_applied_before_daily_grouping() {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    let h = Home::new();
    let mut event = record("one", "t", 100, 10, 0);
    event["payload"]["usage"] = json!({"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":20,"cache_write_tokens":30}});
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta(), context("t", "m"), event],
    );
    let price=serde_json::from_value(json!({"prompt":2,"completion":10,"cache_read":0.1,"cache_creation":2.5,"currency":"usd","rules":[{"label":"04:00UTC","start_minute":240,"end_minute":241,"prompt_multiplier":0.5,"completion_multiplier":0.5}]})).unwrap();
    let r = scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        &[UsagePriceReference {
            provider: "manual".into(),
            models: vec!["m".into()],
            price,
        }],
    )
    .unwrap();
    assert!((r.summary_rows[0].estimated_cost.unwrap() - 0.0001385).abs() < 1e-12);
    assert_eq!(r.summary_rows[0].cache_write_tokens, Some(30));
}

#[test]
fn configured_conflicts_and_non_openai_provider_never_get_hidden_by_official_fallback() {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta(),
            context("t", "gpt-6.1-sol"),
            record("one", "t", 100, 10, 0),
        ],
    );
    let price =
        serde_json::from_value(json!({"prompt":1,"completion":2,"currency":"cny"})).unwrap();
    let first = UsagePriceReference {
        provider: "user".into(),
        models: vec!["gpt-6.1-sol".into()],
        price,
    };
    let r = scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        std::slice::from_ref(&first),
    )
    .unwrap();
    assert_eq!(r.summary_rows[0].estimated_currency.as_deref(), Some("cny"));
    assert!((r.summary_rows[0].estimated_cost.unwrap() - 0.00012).abs() < 1e-12);
    let mut other = first.clone();
    other.price.prompt = 3.;
    let r = scan_from_home_with_prices(&h.0, "2026-10-12", "2026-10-14", &[first, other]).unwrap();
    assert_eq!(
        r.summary_rows[0].estimate_note.as_deref(),
        Some("price_ambiguous")
    );
    assert_eq!(r.summary_rows[0].estimated_cost_min, None);
    let h = Home::new();
    let mut metadata = meta();
    metadata["payload"]["model_provider"] = json!("different-supplier");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            metadata,
            context("t", "gpt-6.1-sol"),
            record("one", "t", 100, 10, 0),
        ],
    );
    let r = scan_from_home_with_prices(&h.0, "2026-10-12", "2026-10-14", &[]).unwrap();
    assert_eq!(
        r.summary_rows[0].provider.as_deref(),
        Some("different-supplier")
    );
    assert_eq!(r.summary_rows[0].estimated_cost, None);
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

fn dsh_reference(
    provider: &str,
    model: &str,
    unit: f64,
) -> llm_gateway_lib::client_usage::UsagePriceReference {
    llm_gateway_lib::client_usage::UsagePriceReference {
        provider: provider.into(), models: vec![model.into()],
        price: serde_json::from_value(json!({"prompt":unit,"completion":unit,"cache_read":unit,"cache_creation":unit,"currency":"usd"})).unwrap(),
    }
}
fn dsh_priced(
    h: &Home,
    provider: &str,
    model: &str,
    usage: Value,
    configured: &[llm_gateway_lib::client_usage::UsagePriceReference],
) -> llm_gateway_lib::client_usage::ClientUsageReport {
    h.write(
        ".dsh/dsh-usage/usage-ledger.json",
        &json!({"version":1,"days":{"2026-10-13":{provider:{model:usage}}}}).to_string(),
    );
    llm_gateway_lib::client_usage::scan_from_home_with_prices(
        &h.0,
        "2026-10-12",
        "2026-10-14",
        configured,
    )
    .unwrap()
}
#[test]
fn dsh_official_reference_requires_exact_provider_model_source_and_complete_input_parts() {
    let full = json!({"inputTokens":10,"outputTokens":40,"cacheReadTokens":20,"cacheWriteTokens":30,"calls":1});
    for (provider, model) in [
        ("unknown", "deepseek-flash"),
        ("other", "deepseek-flash"),
        ("commandcode", "deepseek-flash"),
        ("deepseek-account", "deepseek/deepseek-v4.1-flash"),
        ("commandcode", "deepseek/deepseek-v4.1-flash:free"),
        ("commandcode", "deepseek/deepseek-v4.1-flash-fast"),
        ("deepseek-account", "DeepSeek-Flash"),
        ("deepseek-account", "deepseek-flash-Fast"),
    ] {
        let h = Home::new();
        let r = dsh_priced(&h, provider, model, full.clone(), &[]);
        assert!(
            r.summary_rows[0].estimated_cost_min.is_none(),
            "{provider}/{model}"
        );
    }
    for missing in [
        "inputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
        "outputTokens",
    ] {
        let h = Home::new();
        let mut partial = full.clone();
        partial.as_object_mut().unwrap().remove(missing);
        let r = dsh_priced(
            &h,
            "commandcode",
            "deepseek/deepseek-v4.1-flash",
            partial,
            &[],
        );
        assert_eq!(
            r.summary_rows[0].estimate_note.as_deref(),
            Some("usage_incomplete"),
            "{missing}"
        );
        assert!(r.summary_rows[0].estimated_cost_min.is_none());
    }
    let h = Home::new();
    let mut r = dsh_priced(&h, "commandcode", "deepseek/deepseek-v4.1-flash", full, &[]);
    for source in ["codex", "qoder"] {
        r.daily_rows[0].source = source.into();
        llm_gateway_lib::client_usage::apply_reference_prices(&mut r, &[]);
        assert!(r.rows[0].estimated_cost_min.is_none());
    }
    r.daily_rows[0].source = "dsh".into();
    r.daily_rows[0].provider = None;
    llm_gateway_lib::client_usage::apply_reference_prices(&mut r, &[]);
    assert!(r.rows[0].estimated_cost_min.is_none());
}
#[test]
fn dsh_configured_prices_override_catalog_while_conflicts_invalid_and_wrong_provider_block() {
    let h = Home::new();
    let model = "deepseek/deepseek-v4.1-flash";
    let full = json!({"inputTokens":10,"outputTokens":40,"cacheReadTokens":20,"cacheWriteTokens":30,"calls":1});
    for unit in [0., 2.] {
        let r = dsh_priced(
            &h,
            "commandcode",
            model,
            full.clone(),
            &[dsh_reference("commandcode", model, unit)],
        );
        assert_eq!(r.summary_rows[0].estimated_cost, Some(100. * unit / 1e6));
        assert!(r.summary_rows[0]
            .price_reference
            .as_ref()
            .unwrap()
            .contains("本机参考价"));
    }
    for (configured, note) in [
        (
            vec![
                dsh_reference("commandcode", model, 1.),
                dsh_reference("commandcode", model, 2.),
            ],
            "price_ambiguous",
        ),
        (
            vec![dsh_reference("other", model, 1.)],
            "provider_price_unmatched",
        ),
        (
            vec![dsh_reference("commandcode", model, -1.)],
            "invalid_price",
        ),
    ] {
        let r = dsh_priced(&h, "commandcode", model, full.clone(), &configured);
        assert_eq!(r.summary_rows[0].estimate_note.as_deref(), Some(note));
        assert!(r.summary_rows[0].estimated_cost_min.is_none());
    }
}
#[test]
fn dsh_calendar_dates_remain_envelopes_and_unknown_neighbors_never_become_zero() {
    let h = Home::new();
    h.write(".dsh-beta/dsh-usage/usage-ledger.json", &json!({"version":1,"days":{
        "2026-10-01":{"deepseek-account":{"deepseek-flash":{"inputTokens":10,"outputTokens":40,"cacheReadTokens":20,"cacheWriteTokens":30}}},
        "2026-10-11":{"deepseek-account":{"deepseek-flash":{"inputTokens":10,"outputTokens":40,"cacheReadTokens":20,"cacheWriteTokens":30},"unknown-model":{"inputTokens":10,"outputTokens":40,"cacheReadTokens":20,"cacheWriteTokens":30}}}
    }}).to_string());
    let r = llm_gateway_lib::client_usage::scan_from_home_with_prices(
        &h.0,
        "2026-10-01",
        "2026-10-11",
        &[],
    )
    .unwrap();
    let known = r
        .summary_rows
        .iter()
        .find(|r| r.model == "deepseek-flash")
        .unwrap();
    assert!((known.estimated_cost_min.unwrap() - 0.00006012).abs() < 1e-12);
    assert!((known.estimated_cost_max.unwrap() - 0.00012024).abs() < 1e-12);
    let unknown = r
        .summary_rows
        .iter()
        .find(|r| r.model == "unknown-model")
        .unwrap();
    assert_eq!(unknown.estimate_note.as_deref(), Some("price_missing"));
    assert!(unknown.estimated_cost_min.is_none());
    for row in r.daily_rows.iter().filter(|r| r.model == "deepseek-flash") {
        assert!((row.estimated_cost_max.unwrap() - 0.00006012).abs() < 1e-12);
    }
}
