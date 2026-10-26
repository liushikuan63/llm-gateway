use llm_gateway_lib::client_usage::{
    jobs::{JobView, UsageJobs},
    scan_from_home, ClientUsageReport,
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("llmgw-codex-archive-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn write(&self, relative: &str, content: impl AsRef<[u8]>) {
        let p = self.0.join(relative);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    fn jsonl(&self, relative: &str, values: Vec<Value>) {
        self.write(
            relative,
            values.iter().map(|v| format!("{v}\n")).collect::<String>(),
        );
    }
    fn scan(&self) -> ClientUsageReport {
        scan_from_home(&self.0, "2026-10-12", "2026-10-14").unwrap()
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn meta(id: &str, provider: &str) -> Value {
    json!({"type":"session_meta","timestamp":"2026-10-13T03:00:00Z","payload":{"id":id,"model_provider":provider}})
}
fn context(model: &str) -> Value {
    json!({"type":"turn_context","payload":{"turn_id":"t","model":model}})
}
fn modern(id: &str, input: u64) -> Value {
    json!({"type":"token_usage_record","timestamp":"2026-10-13T04:00:00Z","payload":{"thread_id":"s","turn_id":"t","response_id":id,"usage":{"input_tokens":input,"output_tokens":10,"cached_input_tokens":0,"cache_write_input_tokens":0}}})
}
fn legacy(total: u64, last: u64, second: u32) -> Value {
    json!({"type":"event_msg","timestamp":format!("2026-10-13T04:00:{second:02}Z"),"payload":{"type":"token_count","info":{"model":"fixture","total_token_usage":{"input_tokens":total,"output_tokens":total/10,"cached_input_tokens":0,"cache_write_input_tokens":0},"last_token_usage":{"input_tokens":last,"output_tokens":last/10,"cached_input_tokens":0,"cache_write_input_tokens":0}}}})
}
fn total(report: &ClientUsageReport) -> u64 {
    report
        .rows
        .iter()
        .filter(|r| r.source == "codex")
        .filter_map(|r| r.input_tokens)
        .sum()
}
fn note(report: &ClientUsageReport, note: &str) -> bool {
    report.sources[0].notes.iter().any(|n| n == note)
}
async fn terminal(jobs: &UsageJobs, id: &str) -> JobView {
    tokio::time::timeout(std::time::Duration::from_secs(180), async {
        loop {
            let v = jobs.status(id).await.unwrap();
            if v.state != "running" {
                return v;
            }
            tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        }
    })
    .await
    .unwrap()
}
async fn full(h: &Home) -> JobView {
    let jobs = UsageJobs::new(h.0.join("owned-jobs")).unwrap();
    let v = jobs
        .start(
            h.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    terminal(&jobs, &v.job_id).await
}
#[tokio::test]
async fn active_archive_exact_modern_mirrors_share_one_source_and_dedup_domain() {
    let h = Home::new();
    let mirrored = vec![meta("s", "openai"), context("fixture"), modern("same", 100)];
    h.jsonl(".codex/sessions/a.jsonl", mirrored.clone());
    h.jsonl(".codex/archived_sessions/a.jsonl", mirrored);
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![
            meta("s", "openai"),
            context("fixture"),
            modern("archive-only", 70),
        ],
    );
    let quick = h.scan();
    assert_eq!(total(&quick), 170);
    assert_eq!(quick.sources[0].files_scanned, 3);
    assert_eq!(
        quick.sources.iter().filter(|s| s.source == "codex").count(),
        1
    );
    assert!(note(&quick, "codex_archive_scope_included"));
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    assert!(done.coverage_complete);
    let report = done.report.unwrap();
    assert_eq!(report.rows, quick.rows);
    assert_eq!(
        serde_json::to_value(report.sources).unwrap(),
        serde_json::to_value(quick.sources).unwrap()
    );
}
#[test]
fn archive_only_is_available_and_both_missing_is_unavailable() {
    let h = Home::new();
    assert_eq!(h.scan().sources[0].status, "unavailable");
    h.jsonl(
        ".codex/archived_sessions/a.jsonl",
        vec![meta("s", "openai"), context("fixture"), modern("r", 91)],
    );
    let report = h.scan();
    assert_eq!(total(&report), 91);
    assert_eq!(report.sources[0].status, "ok");
    assert!(note(&report, "codex_archive_scope_included"));
}
#[test]
fn legacy_mirrors_and_cross_directory_parent_fork_are_planned_together() {
    let h = Home::new();
    let parent = vec![
        meta("parent", "openai"),
        legacy(100, 100, 1),
        legacy(150, 50, 2),
    ];
    h.jsonl(".codex/sessions/parent.jsonl", parent.clone());
    h.jsonl(".codex/archived_sessions/mirror.jsonl", parent);
    let mut child = meta("child", "openai");
    child["payload"]["forked_from_id"] = json!("parent");
    child["timestamp"] = json!("2026-10-13T04:00:03Z");
    h.jsonl(
        ".codex/archived_sessions/child.jsonl",
        vec![
            child,
            legacy(100, 100, 1),
            legacy(150, 50, 2),
            legacy(190, 40, 4),
        ],
    );
    assert_eq!(total(&h.scan()), 190);
}
fn conflict(kind: &str) {
    let h = Home::new();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta("s", "openai"),
            context("fixture"),
            modern("conflict", 100),
            modern("safe", 7),
        ],
    );
    let mut record = modern("conflict", 100);
    if kind == "usage" {
        record["payload"]["usage"]["input_tokens"] = json!(200);
    }
    if kind == "day" {
        record["timestamp"] = json!("2026-10-14T04:00:00Z");
    }
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![
            meta(
                "s",
                if kind == "provider" {
                    "other"
                } else {
                    "openai"
                },
            ),
            context(if kind == "model" {
                "other-model"
            } else {
                "fixture"
            }),
            record,
        ],
    );
    let report = h.scan();
    assert_eq!(total(&report), 7, "{kind}");
    assert_eq!(report.sources[0].status, "partial");
    assert!(note(&report, "codex_response_conflict"));
}
#[test]
fn conflicting_archived_response_usage_is_excluded() {
    conflict("usage");
}
#[test]
fn conflicting_archived_response_day_is_excluded() {
    conflict("day");
}
#[test]
fn conflicting_archived_response_model_is_excluded() {
    conflict("model");
}
#[test]
fn conflicting_archived_response_provider_is_excluded() {
    conflict("provider");
}
#[tokio::test]
async fn quick_file_limit_is_shared_and_fair_between_fixed_roots() {
    let h = Home::new();
    for root in ["sessions", "archived_sessions"] {
        for i in 0..501 {
            h.jsonl(
                &format!(".codex/{root}/{i:04}.jsonl"),
                vec![
                    context(if root == "sessions" {
                        "active"
                    } else {
                        "archive"
                    }),
                    modern(&format!("{root}-{i}"), 1),
                ],
            );
        }
    }
    let report = h.scan();
    assert_eq!(report.sources[0].files_scanned, 500);
    assert_eq!(total(&report), 500);
    for model in ["active", "archive"] {
        assert_eq!(
            report
                .rows
                .iter()
                .filter(|r| r.model == model)
                .filter_map(|r| r.input_tokens)
                .sum::<u64>(),
            250
        );
    }
    assert!(note(&report, "scan_limit"));
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    assert!(done.coverage_complete);
    let report = done.report.unwrap();
    assert_eq!(total(&report), 1002);
    assert_eq!(report.sources[0].files_scanned, 1002);
    assert!(!note(&report, "scan_limit"));
}
#[tokio::test]
async fn archive_bad_unicode_and_giant_body_preserve_full_projection_validation() {
    let h = Home::new();
    let mut bytes = format!("{}\n{}\n", meta("s", "openai"), context("fixture")).into_bytes();
    bytes.extend_from_slice(b"{\"message\":\"\\uD800\"}\n");
    bytes.extend_from_slice(b"{\"message\":\"");
    bytes.resize(bytes.len() + 2 * 1024 * 1024, b'x');
    bytes.extend_from_slice(b"\"}\n");
    bytes.extend_from_slice(format!("{}\n", modern("valid", 83)).as_bytes());
    h.write(".codex/archived_sessions/a.jsonl", bytes);
    let quick = h.scan();
    assert_eq!(total(&quick), 83);
    assert!(note(&quick, "invalid_json"));
    assert!(note(&quick, "large_line"));
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    assert!(!done.coverage_complete);
    let report = done.report.unwrap();
    assert_eq!(total(&report), 83);
    assert!(note(&report, "invalid_json"));
    let db = fs::read(h.0.join("owned-jobs/jobs.sqlite")).unwrap();
    assert!(!db.windows(128).any(|w| w.iter().all(|b| *b == b'x')));
}
#[tokio::test]
async fn full_freezes_archive_empty_scope_and_old_snapshot_never_expands_on_resume() {
    for old in [false, true] {
        let h = Home::new();
        let mut rows = vec![meta("s", "openai"), context("fixture")];
        for i in 0..8000 {
            rows.push(modern(&format!("r-{i}"), 1));
        }
        h.jsonl(".codex/sessions/a.jsonl", rows);
        let jobs = UsageJobs::new(h.0.join("owned-jobs")).unwrap();
        let v = jobs
            .start(
                h.0.clone(),
                "2026-10-12".into(),
                "2026-10-14".into(),
                vec![],
            )
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let s = jobs.status(&v.job_id).await.unwrap();
                assert_eq!(s.state, "running");
                if s.records_scanned >= 256 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        jobs.cancel(&v.job_id).await.unwrap();
        assert_eq!(terminal(&jobs, &v.job_id).await.state, "cancelled");
        let pool = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(h.0.join("owned-jobs/jobs.sqlite")),
        )
        .await
        .unwrap();
        let raw: String = sqlx::query_scalar("SELECT notes FROM jobs WHERE id=?")
            .bind(&v.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut scope: Vec<(String, Vec<String>)> = serde_json::from_str(&raw).unwrap();
        assert!(scope
            .iter()
            .any(|(p, _)| PathBuf::from(p).ends_with(".codex/archived_sessions")));
        if old {
            scope.retain(|(p, _)| !PathBuf::from(p).ends_with(".codex/archived_sessions"));
            sqlx::query("UPDATE jobs SET notes=? WHERE id=?")
                .bind(serde_json::to_string(&scope).unwrap())
                .bind(&v.job_id)
                .execute(&pool)
                .await
                .unwrap();
        }
        pool.close().await;
        h.jsonl(
            ".codex/archived_sessions/new.jsonl",
            vec![
                meta("new", "openai"),
                context("fixture"),
                modern("must-not-enter-snapshot", 90000),
            ],
        );
        jobs.resume(&v.job_id).await.unwrap();
        let done = terminal(&jobs, &v.job_id).await;
        assert_eq!(done.state, "completed");
        assert!(done.coverage_complete);
        let report = done.report.unwrap();
        assert_eq!(total(&report), 8000);
        assert_eq!(note(&report, "codex_archive_scope_included"), !old);
        assert_eq!(note(&report, "codex_archive_snapshot_not_included"), old);
        if old {
            assert_eq!(report.sources[0].status, "partial");
        }
    }
}

#[test]
fn conflicting_contexts_inside_one_file_cannot_hide_duplicate_response_conflict() {
    let h = Home::new();
    h.jsonl(
        ".codex/archived_sessions/a.jsonl",
        vec![
            meta("s", "openai"),
            context("first-model"),
            modern("same", 100),
            context("second-model"),
            modern("same", 100),
        ],
    );
    let report = h.scan();
    assert_eq!(total(&report), 0);
    assert!(note(&report, "codex_identity_conflict"));
}
#[test]
fn contradictory_provider_metadata_cannot_price_duplicate_response_as_known_usage() {
    let h = Home::new();
    h.jsonl(
        ".codex/archived_sessions/a.jsonl",
        vec![
            meta("s", "openai"),
            context("fixture"),
            modern("same", 100),
            meta("s", "other"),
            modern("same", 100),
        ],
    );
    let report = h.scan();
    assert_eq!(total(&report), 0);
    assert!(note(&report, "codex_identity_conflict"));
}
#[test]
fn contradictory_thread_aliases_cannot_rebill_uncertain_legacy_history() {
    let h = Home::new();
    let mut a = modern("a", 3);
    a["payload"]["thread_id"] = json!("one");
    let mut b = modern("b", 4);
    b["payload"]["thread_id"] = json!("two");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta("s", "openai"),
            context("fixture"),
            legacy(100, 100, 1),
            a,
        ],
    );
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![meta("s", "openai"), context("fixture"), b],
    );
    let report = h.scan();
    assert_eq!(total(&report), 7);
    assert!(note(&report, "codex_identity_conflict"));
}
#[test]
fn response_conflict_outside_selected_date_is_still_excluded() {
    let h = Home::new();
    let mut old = modern("same", 500);
    old["timestamp"] = json!("2020-01-01T04:00:00Z");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![
            meta("s", "openai"),
            context("fixture"),
            modern("same", 100),
            modern("safe", 9),
        ],
    );
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![meta("s", "openai"), context("fixture"), old],
    );
    let report = h.scan();
    assert_eq!(total(&report), 9);
    assert!(note(&report, "codex_response_conflict"));
}
#[tokio::test]
async fn archive_root_and_child_links_are_rejected_without_following_targets() {
    for child in [false, true] {
        let h = Home::new();
        h.jsonl(
            "owned-target/a.jsonl",
            vec![
                meta("s", "openai"),
                context("fixture"),
                modern("excluded", 900),
            ],
        );
        h.jsonl(
            ".codex/sessions/a.jsonl",
            vec![meta("s", "openai"), context("fixture"), modern("safe", 11)],
        );
        let target = h.0.join("owned-target");
        let link = h.0.join(if child {
            ".codex/archived_sessions/linked"
        } else {
            ".codex/archived_sessions"
        });
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link.to_string_lossy().replace('/', "\\"))
                .arg(target.to_string_lossy().replace('/', "\\"))
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        #[cfg(not(windows))]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let quick = h.scan();
        let done = full(&h).await;
        assert_eq!(done.state, "completed");
        assert!(!done.coverage_complete);
        for report in [quick, done.report.unwrap()] {
            assert_eq!(total(&report), 11);
            assert!(note(&report, "links_skipped"));
            assert_eq!(report.sources[0].files_scanned, 1);
        }
        #[cfg(windows)]
        fs::remove_dir(&link).unwrap();
        #[cfg(not(windows))]
        fs::remove_file(&link).unwrap();
        assert!(target.join("a.jsonl").is_file());
    }
}
#[tokio::test]
async fn full_archive_heads_and_tails_exceed_shared_quick_bytes_without_truncation() {
    use std::io::Write;
    let h = Home::new();
    let block = vec![b'x'; 1024 * 1024];
    for i in 0..5 {
        let relative = format!(
            ".codex/{}/{i}.jsonl",
            if i == 0 {
                "sessions"
            } else {
                "archived_sessions"
            }
        );
        let p = h.0.join(relative);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut file = fs::File::create(p).unwrap();
        writeln!(
            file,
            "{}\n{}\n{}",
            meta("s", "openai"),
            context("fixture"),
            modern(&format!("head-{i}"), 100)
        )
        .unwrap();
        file.write_all(b"{\"message\":\"").unwrap();
        for _ in 0..34 {
            file.write_all(&block).unwrap();
        }
        file.write_all(b"\"}\n").unwrap();
        writeln!(file, "{}", modern(&format!("tail-{i}"), 7)).unwrap();
    }
    let quick = h.scan();
    assert!(quick.sources[0].bytes_read <= 128 * 1024 * 1024);
    assert!(total(&quick) < 535);
    assert!(note(&quick, "tail_only"));
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    assert!(done.coverage_complete);
    assert!(done.snapshot_bytes_read > 128 * 1024 * 1024);
    let report = done.report.unwrap();
    assert_eq!(total(&report), 535);
    assert_eq!(report.sources[0].files_scanned, 5);
    assert!(!note(&report, "tail_only"));
    assert!(!note(&report, "large_line"));
}

async fn unsafe_conflict(kind: &str) {
    let h = Home::new();
    for (root, label) in [
        ("sessions", "PRIVATE first label"),
        ("archived_sessions", "PRIVATE second label"),
    ] {
        h.jsonl(
            &format!(".codex/{root}/a.jsonl"),
            vec![
                meta("s", if kind == "provider" { label } else { "openai" }),
                context(if kind == "model" { label } else { "fixture" }),
                modern("same", 100),
            ],
        );
    }
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 0);
        assert!(note(&report, "codex_response_conflict"));
    }
    let db = fs::read(h.0.join("owned-jobs/jobs.sqlite")).unwrap();
    assert!(!db.windows(7).any(|w| w == b"PRIVATE"));
}
#[tokio::test]
async fn unsafe_model_labels_keep_private_conflict_identity_in_quick_and_full() {
    unsafe_conflict("model").await;
}
#[tokio::test]
async fn unsafe_provider_labels_keep_private_conflict_identity_in_quick_and_full() {
    unsafe_conflict("provider").await;
}

#[tokio::test]
async fn codex_parent_junction_is_rejected_in_both_modes_without_reading_targets() {
    let h = Home::new();
    for root in ["sessions", "archived_sessions"] {
        h.jsonl(
            &format!("owned-target/.codex/{root}/a.jsonl"),
            vec![meta("s", "openai"), context("fixture"), modern(root, 900)],
        );
    }
    let link = h.0.join(".codex");
    let target = h.0.join("owned-target/.codex");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(target.to_string_lossy().replace('/', "\\"))
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    assert!(!done.coverage_complete);
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 0);
        assert!(note(&report, "links_skipped"));
        assert_eq!(report.sources[0].files_scanned, 0);
    }
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
    #[cfg(not(windows))]
    fs::remove_file(&link).unwrap();
    assert!(target.join("sessions/a.jsonl").is_file());
}
#[tokio::test]
async fn oversized_unsafe_model_and_provider_labels_remain_distinct_without_plaintext() {
    for kind in ["model", "provider"] {
        let h = Home::new();
        for (root, suffix) in [("sessions", "甲"), ("archived_sessions", "乙")] {
            let label = format!("PRIVATE {} {suffix}", "x".repeat(1024));
            h.jsonl(
                &format!(".codex/{root}/a.jsonl"),
                vec![
                    meta("s", if kind == "provider" { &label } else { "openai" }),
                    context(if kind == "model" { &label } else { "fixture" }),
                    modern("same", 100),
                ],
            );
        }
        let quick = h.scan();
        let done = full(&h).await;
        assert_eq!(done.state, "completed");
        for report in [quick, done.report.unwrap()] {
            assert_eq!(total(&report), 0);
            assert!(note(&report, "codex_response_conflict"));
        }
        let db = fs::read(h.0.join("owned-jobs/jobs.sqlite")).unwrap();
        assert!(!db.windows(7).any(|w| w == b"PRIVATE"));
        assert!(!db.windows(128).any(|w| w.iter().all(|b| *b == b'x')));
    }
}

#[tokio::test]
async fn archive_modern_mirror_covers_active_legacy_total_without_double_counting() {
    let h = Home::new();
    let before = legacy(100, 100, 1);
    let mirror = legacy(200, 100, 2);
    let mut record = modern("modern", 100);
    record["timestamp"] = json!("2026-10-13T04:00:03Z");
    record["payload"]["thread_token_usage"] =
        mirror["payload"]["info"]["total_token_usage"].clone();
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta("s", "openai"), before, mirror],
    );
    h.jsonl(
        ".codex/archived_sessions/a.jsonl",
        vec![meta("s", "openai"), context("fixture"), record],
    );
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 200);
        assert_eq!(report.sources[0].status, "ok");
    }
}
#[tokio::test]
async fn archive_first_raw_header_evidence_is_required_for_initial_legacy_request() {
    for valid in [false, true] {
        let h = Home::new();
        let content = format!(
            "{}{}\n{}\n",
            if valid { "" } else { "broken\n" },
            meta("s", "openai"),
            legacy(100, 100, 1)
        );
        h.write(".codex/archived_sessions/a.jsonl", content);
        let quick = h.scan();
        let done = full(&h).await;
        assert_eq!(done.state, "completed");
        assert_eq!(done.coverage_complete, valid);
        for report in [quick, done.report.unwrap()] {
            assert_eq!(total(&report), if valid { 100 } else { 0 });
            assert_eq!(note(&report, "invalid_json"), !valid);
        }
    }
}

async fn legacy_attribution_conflict(kind: &str) {
    let h = Home::new();
    let unsafe_label = format!("PRIVATE {} 甲", "x".repeat(1024));
    let mut a = legacy(100, 100, 1);
    a["payload"]["info"]["model"] = json!("model-a");
    let mut b = a.clone();
    if kind.contains("model") {
        b["payload"]["info"]["model"] = json!(if kind.starts_with("unsafe") {
            unsafe_label.as_str()
        } else {
            "model-b"
        });
    }
    if kind == "day" {
        b["timestamp"] = json!("2026-10-14T04:00:01Z");
    }
    let mut later = legacy(130, 30, 2);
    later["payload"]["info"]["model"] = json!("confirmed-model");
    if kind == "day" {
        later["timestamp"] = json!("2026-10-14T04:00:02Z");
    }
    h.jsonl(".codex/sessions/a.jsonl", vec![meta("s", "openai"), a]);
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![
            meta(
                "s",
                if kind == "provider" {
                    "other"
                } else if kind == "unsafe_provider" {
                    &unsafe_label
                } else {
                    "openai"
                },
            ),
            b,
            later,
        ],
    );
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 30, "{kind}");
        assert!(note(&report, "codex_identity_conflict"));
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].model, "confirmed-model");
        assert_eq!(report.rows[0].requests, Some(1));
    }
    let db = fs::read(h.0.join("owned-jobs/jobs.sqlite")).unwrap();
    assert!(!db.windows(7).any(|w| w == b"PRIVATE"));
}
#[tokio::test]
async fn legacy_model_mirror_conflict_excludes_position_and_preserves_later_increment() {
    legacy_attribution_conflict("model").await;
}
#[tokio::test]
async fn legacy_provider_mirror_conflict_excludes_position_and_preserves_later_increment() {
    legacy_attribution_conflict("provider").await;
}
#[tokio::test]
async fn legacy_day_mirror_conflict_excludes_position_and_preserves_later_increment() {
    legacy_attribution_conflict("day").await;
}
#[tokio::test]
async fn legacy_unsafe_model_mirror_conflict_keeps_private_identity() {
    legacy_attribution_conflict("unsafe_model").await;
}
#[tokio::test]
async fn legacy_unsafe_provider_mirror_conflict_keeps_private_identity() {
    legacy_attribution_conflict("unsafe_provider").await;
}
#[tokio::test]
async fn legacy_real_model_switch_changes_position_without_identity_conflict() {
    let h = Home::new();
    let mut a = legacy(100, 100, 1);
    a["payload"]["info"]["model"] = json!("model-a");
    let mut b = legacy(150, 50, 2);
    b["payload"]["info"]["model"] = json!("model-b");
    h.jsonl(".codex/sessions/a.jsonl", vec![meta("s", "openai"), a]);
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![meta("s", "openai"), b],
    );
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 150);
        assert!(!note(&report, "codex_identity_conflict"));
        assert_eq!(report.rows.len(), 2);
    }
}
#[tokio::test]
async fn legacy_missing_attribution_backfill_is_not_a_conflicting_mirror() {
    let h = Home::new();
    let mut a = legacy(100, 100, 1);
    a["payload"]["info"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    let mut header = meta("s", "openai");
    header["payload"]
        .as_object_mut()
        .unwrap()
        .remove("model_provider");
    h.jsonl(".codex/sessions/a.jsonl", vec![header, a]);
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![meta("s", "openai"), legacy(100, 100, 1)],
    );
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 100);
        assert!(!note(&report, "codex_identity_conflict"));
    }
}

#[tokio::test]
async fn legacy_conflict_still_advances_water_for_later_cumulative_only_delta() {
    let h = Home::new();
    let mut a = legacy(100, 100, 1);
    a["payload"]["info"]["model"] = json!("model-a");
    let mut b = a.clone();
    b["payload"]["info"]["model"] = json!("model-b");
    let mut later = legacy(130, 30, 2);
    later["payload"]["info"]
        .as_object_mut()
        .unwrap()
        .remove("last_token_usage");
    h.jsonl(".codex/sessions/a.jsonl", vec![meta("s", "openai"), a]);
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![meta("s", "openai"), b, later],
    );
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 30);
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].granularity, "cumulative_delta");
        assert_eq!(report.rows[0].requests, None);
        assert!(note(&report, "codex_identity_conflict"));
    }
}
#[tokio::test]
async fn legacy_same_file_provider_conflict_excludes_only_ambiguous_position() {
    let h = Home::new();
    h.jsonl(
        ".codex/archived_sessions/a.jsonl",
        vec![
            meta("s", "openai"),
            legacy(100, 100, 1),
            meta("s", "other"),
            legacy(100, 100, 1),
            legacy(130, 30, 2),
        ],
    );
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 30);
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].provider.as_deref(), Some("other"));
        assert!(note(&report, "codex_identity_conflict"));
    }
}
#[tokio::test]
async fn legacy_explicit_provider_change_at_new_position_keeps_confirmed_allocations() {
    let h = Home::new();
    h.jsonl(
        ".codex/archived_sessions/a.jsonl",
        vec![
            meta("s", "openai"),
            legacy(100, 100, 1),
            meta("s", "other"),
            legacy(130, 30, 2),
        ],
    );
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 130);
        assert!(!note(&report, "codex_identity_conflict"));
        assert_eq!(report.rows.len(), 2);
        assert_eq!(
            report
                .rows
                .iter()
                .find(|r| r.provider.as_deref() == Some("openai"))
                .unwrap()
                .input_tokens,
            Some(100)
        );
        assert_eq!(
            report
                .rows
                .iter()
                .find(|r| r.provider.as_deref() == Some("other"))
                .unwrap()
                .input_tokens,
            Some(30)
        );
    }
}
#[tokio::test]
async fn modern_cache_backfill_merges_known_facts_once_and_prices_them() {
    for cache in [0, 50] {
        let h = Home::new();
        let mut first = modern("same", 100);
        first["payload"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cached_input_tokens");
        first["payload"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cache_write_input_tokens");
        let mut second = modern("same", 100);
        second["payload"]["usage"]["cached_input_tokens"] = json!(cache);
        h.jsonl(
            ".codex/sessions/a.jsonl",
            vec![meta("s", "openai"), context("gpt-6.1-sol"), first],
        );
        h.jsonl(
            ".codex/archived_sessions/b.jsonl",
            vec![meta("s", "openai"), context("gpt-6.1-sol"), second],
        );
        let quick = llm_gateway_lib::client_usage::scan_from_home_with_prices(
            &h.0,
            "2026-10-12",
            "2026-10-14",
            &[],
        )
        .unwrap();
        let done = full(&h).await;
        assert_eq!(done.state, "completed");
        for report in [quick, done.report.unwrap()] {
            assert_eq!(total(&report), 100);
            assert!(!note(&report, "codex_response_conflict"));
            let row = &report.rows[0];
            assert_eq!(row.requests, Some(1));
            assert_eq!(row.cache_read_tokens, Some(cache));
            assert_eq!(row.cache_write_tokens, Some(0));
            assert!(row.partial_fields.is_empty());
            let expected = (100 - cache) as f64 * 0.000002 + cache as f64 * 0.0000001 + 0.0001;
            assert!((row.estimated_cost.unwrap() - expected).abs() < 1e-12);
        }
    }
}
#[tokio::test]
async fn modern_complementary_input_output_and_identity_facts_are_merged_before_push() {
    let h = Home::new();
    let mut first = modern("same", 100);
    first["payload"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("output_tokens");
    let mut second = modern("same", 100);
    second["payload"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("input_tokens");
    h.jsonl(".codex/sessions/a.jsonl", vec![meta("s", "openai"), first]);
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![meta("s", "openai"), context("gpt-6.1-sol"), second],
    );
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 100);
        assert!(!note(&report, "codex_response_conflict"));
        assert_eq!(report.rows[0].output_tokens, Some(10));
        assert_eq!(report.rows[0].requests, Some(1));
        assert_eq!(report.rows[0].model, "gpt-6.1-sol");
    }
}
#[tokio::test]
async fn modern_cache_aliases_and_nonbillable_usage_fields_do_not_manufacture_conflicts() {
    let h = Home::new();
    let mut first = modern("same", 100);
    first["payload"]["usage"]["cached_input_tokens"] = json!(20);
    first["payload"]["usage"]["reasoning_output_tokens"] = json!(2);
    let mut second = first.clone();
    second["payload"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("cached_input_tokens");
    second["payload"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_write_input_tokens");
    second["payload"]["usage"]["input_tokens_details"] =
        json!({"cached_tokens":20,"cache_write_tokens":0});
    second["payload"]["usage"]["reasoning_output_tokens"] = json!(5);
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta("s", "openai"), context("fixture"), first],
    );
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![meta("s", "openai"), context("fixture"), second],
    );
    let quick = h.scan();
    let done = full(&h).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 100);
        assert!(!note(&report, "codex_response_conflict"));
        assert_eq!(report.rows[0].requests, Some(1));
        assert_eq!(report.rows[0].cache_read_tokens, Some(20));
        assert_eq!(report.rows[0].cache_write_tokens, Some(0));
    }
}
#[test]
fn modern_known_cache_disagreement_still_excludes_both_copies() {
    let h = Home::new();
    for (root, cache) in [("sessions", 10), ("archived_sessions", 11)] {
        let mut record = modern("same", 100);
        record["payload"]["usage"]["cached_input_tokens"] = json!(cache);
        h.jsonl(
            &format!(".codex/{root}/a.jsonl"),
            vec![meta("s", "openai"), context("fixture"), record],
        );
    }
    let report = h.scan();
    assert_eq!(total(&report), 0);
    assert!(note(&report, "codex_response_conflict"));
}
#[tokio::test]
async fn modern_same_day_disagreeing_price_minutes_keep_conservative_fee_bounds() {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    let h = Home::new();
    let mut second = modern("same", 100);
    second["timestamp"] = json!("2026-10-13T05:00:00Z");
    h.jsonl(
        ".codex/sessions/a.jsonl",
        vec![meta("s", "openai"), context("fixture"), modern("same", 100)],
    );
    h.jsonl(
        ".codex/archived_sessions/b.jsonl",
        vec![meta("s", "openai"), context("fixture"), second],
    );
    let price=serde_json::from_value(json!({"prompt":2,"completion":10,"cache_read":0.1,"cache_creation":2.5,"currency":"usd","rules":[{"label":"04:00UTC","start_minute":240,"end_minute":241,"prompt_multiplier":0.5,"completion_multiplier":0.5}]})).unwrap();
    let prices = vec![UsagePriceReference {
        provider: "openai".into(),
        models: vec!["fixture".into()],
        price,
    }];
    let quick = scan_from_home_with_prices(&h.0, "2026-10-12", "2026-10-14", &prices).unwrap();
    let jobs = UsageJobs::new(h.0.join("owned-jobs")).unwrap();
    let job = jobs
        .start(
            h.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            prices,
        )
        .await
        .unwrap();
    let done = terminal(&jobs, &job.job_id).await;
    assert_eq!(done.state, "completed");
    for report in [quick, done.report.unwrap()] {
        assert_eq!(total(&report), 100);
        assert!(!note(&report, "codex_response_conflict"));
        let row = &report.rows[0];
        assert_eq!(row.estimated_cost, None);
        assert!(row.estimated_cost_min.unwrap() <= 0.00015 + 1e-12);
        assert!(row.estimated_cost_max.unwrap() >= 0.0003 - 1e-12);
    }
}

#[tokio::test]
async fn modern_explicit_unknown_provider_keeps_price_unknown_until_known_copy_proves_it() {
    for backfill in [false, true] {
        let h = Home::new();
        h.jsonl(
            ".codex/sessions/a.jsonl",
            vec![
                meta("s", "unknown"),
                context("gpt-6.1-sol"),
                modern("same", 100),
            ],
        );
        if backfill {
            h.jsonl(
                ".codex/archived_sessions/b.jsonl",
                vec![
                    meta("s", "openai"),
                    context("gpt-6.1-sol"),
                    modern("same", 100),
                ],
            );
        }
        let quick = llm_gateway_lib::client_usage::scan_from_home_with_prices(
            &h.0,
            "2026-10-12",
            "2026-10-14",
            &[],
        )
        .unwrap();
        let done = full(&h).await;
        assert_eq!(done.state, "completed");
        for report in [quick, done.report.unwrap()] {
            assert_eq!(total(&report), 100);
            assert!(!note(&report, "codex_response_conflict"));
            let row = &report.rows[0];
            assert_eq!(row.requests, Some(1));
            assert_eq!(
                row.provider.as_deref(),
                Some(if backfill { "openai" } else { "unknown" })
            );
            assert_eq!(row.estimated_cost.is_some(), backfill);
            if backfill {
                assert!((row.estimated_cost.unwrap() - 0.0003).abs() < 1e-12);
            }
        }
    }
}
