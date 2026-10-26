use llm_gateway_lib::client_usage::{
    jobs::{JobView, UsageJobs},
    scan_from_home,
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("llmgw-qoder-history-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn write(&self, relative: &str, values: &[Value]) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            values.iter().map(|v| format!("{v}\n")).collect::<String>(),
        )
        .unwrap();
    }
}
fn event(id: &str, credits: Value, billable: Value) -> Value {
    json!({"type":"assistant", "timestamp":"2026-10-13T04:00:00Z", "message":{
        "id":format!("message-{id}"), "model":"qfmodel", "content":"PRIVATE-BODY-MUST-NOT-PERSIST",
        "usage":{"request_id":id,"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,
            "cache_creation_input_tokens":0,"credits":credits,"billable":billable}}})
}
fn scan(f: &Fixture) -> Value {
    serde_json::to_value(scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap()).unwrap()
}
#[test]
fn two_profiles_dedupe_forks_and_separate_platform_credits_from_tokens() {
    let f = Fixture::new();
    let records = vec![
        event("same", json!(2.5), json!(false)),
        event("billed", json!(3.0), json!(true)),
    ];
    f.write(".qoder/projects/project/a.jsonl", &records);
    f.write(".qoder/projects/fork/b.jsonl", &records);
    f.write(
        ".qoder-cn/projects/a.jsonl",
        &[event("same", json!(7.0), json!(true))],
    );
    let report = scan(&f);
    let rows = report["summary_rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "both history profiles should be scanned");
    let global = rows.iter().find(|r| r["profile"] == "global").unwrap();
    assert_eq!(global["source"], "qoder_history");
    assert_eq!(global["requests"], 2);
    assert_eq!(global["platform_credits"]["billed"], 3.0);
    assert_eq!(global["platform_credits"]["not_billed"], 2.5);
    assert_eq!(global["platform_credits"]["not_billed_records"], 1);
    assert_eq!(global["input_tokens"], Value::Null);
    assert_eq!(global["output_tokens"], Value::Null);
    assert_eq!(global["estimated_cost"], Value::Null);
    assert_eq!(global["cache_miss_requests"], 0);
    let cn = rows.iter().find(|r| r["profile"] == "cn").unwrap();
    assert_eq!(cn["platform_credits"]["billed"], 7.0);
}
async fn terminal(jobs: &UsageJobs, id: &str) -> JobView {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let view = jobs.status(id).await.unwrap();
            if view.state != "running" {
                return view;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
fn directory_link(link: &std::path::Path, target: &std::path::Path) {
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::{fs::MetadataExt, process::CommandExt};
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(target.to_string_lossy().replace('/', "\\"))
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_ne!(
            fs::symlink_metadata(link).unwrap().file_attributes() & 0x400,
            0
        );
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(target, link).unwrap();
}
#[tokio::test]
async fn known_suffix_migrated_roots_are_read_in_both_profiles_and_modes() {
    for (directory, profile) in [(".qoder", "global"), (".qoder-cn", "cn")] {
        let f = Fixture::new();
        f.write(
            &format!("moved/{directory}/projects/a.jsonl"),
            &[event("a", json!(7), json!(true))],
        );
        directory_link(
            &f.0.join(directory).join("projects"),
            &f.0.join("moved").join(directory).join("projects"),
        );
        let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
        let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
        let started = jobs
            .start(
                f.0.clone(),
                "2026-10-12".into(),
                "2026-10-14".into(),
                vec![],
            )
            .await
            .unwrap();
        let full = terminal(&jobs, &started.job_id).await;
        assert_eq!(full.state, "completed");
        let full_report = full.report.unwrap();
        for report in [&quick, &full_report] {
            assert_eq!(
                report.summary_rows.len(),
                1,
                "known migrated root {profile} must be readable"
            );
            let row = &report.summary_rows[0];
            assert_eq!(row.profile, profile);
            assert_eq!(row.requests, Some(1));
            assert_eq!(row.platform_credits.as_ref().unwrap().billed, 7.0);
            let source = report
                .sources
                .iter()
                .find(|s| s.source == "qoder_history" && s.profile == profile)
                .unwrap();
            assert_eq!(source.status, "ok");
            assert!(source
                .notes
                .iter()
                .any(|n| n == "known_source_root_resolved"));
        }
        assert_eq!(quick.daily_rows, full_report.daily_rows);
        assert_eq!(
            serde_json::to_value(quick.sources).unwrap(),
            serde_json::to_value(full_report.sources).unwrap()
        );
    }
}
#[tokio::test]
async fn migrated_roots_reject_the_other_profile_in_both_directions() {
    for (logical, wrong) in [(".qoder", ".qoder-cn"), (".qoder-cn", ".qoder")] {
        let f = Fixture::new();
        f.write(
            &format!("moved/{wrong}/projects/a.jsonl"),
            &[event("wrong", json!(999), json!(true))],
        );
        directory_link(
            &f.0.join(logical).join("projects"),
            &f.0.join("moved").join(wrong).join("projects"),
        );
        let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
        let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
        let started = jobs
            .start(
                f.0.clone(),
                "2026-10-12".into(),
                "2026-10-14".into(),
                vec![],
            )
            .await
            .unwrap();
        let full = terminal(&jobs, &started.job_id).await;
        assert_eq!(full.state, "completed");
        for report in [quick, full.report.unwrap()] {
            assert!(report.daily_rows.is_empty());
            let profile = if logical == ".qoder" { "global" } else { "cn" };
            let source = report
                .sources
                .iter()
                .find(|s| s.source == "qoder_history" && s.profile == profile)
                .unwrap();
            assert!(source.notes.iter().any(|n| n == "links_skipped"));
            assert!(!source
                .notes
                .iter()
                .any(|n| n == "known_source_root_resolved"));
            assert_eq!(source.bytes_read, 0);
        }
    }
}
#[tokio::test]
async fn migrated_roots_still_skip_child_links_even_with_another_known_suffix() {
    let f = Fixture::new();
    f.write(
        "moved/.qoder/projects/a.jsonl",
        &[event("main", json!(7), json!(true))],
    );
    f.write(
        "other/.qoder/projects/a.jsonl",
        &[event("child", json!(999), json!(true))],
    );
    let target = f.0.join("moved").join(".qoder").join("projects");
    directory_link(
        &target.join("nested").join(".qoder").join("projects"),
        &f.0.join("other").join(".qoder").join("projects"),
    );
    directory_link(&f.0.join(".qoder").join("projects"), &target);
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let started = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    let full = terminal(&jobs, &started.job_id).await;
    assert_eq!(full.state, "completed");
    let full_report = full.report.unwrap();
    for report in [&quick, &full_report] {
        assert_eq!(report.summary_rows.len(), 1);
        assert_eq!(report.summary_rows[0].requests, Some(1));
        assert_eq!(
            report.summary_rows[0]
                .platform_credits
                .as_ref()
                .unwrap()
                .billed,
            7.0
        );
        let source = report
            .sources
            .iter()
            .find(|s| s.source == "qoder_history" && s.profile == "global")
            .unwrap();
        assert_eq!(source.files_scanned, 1);
        assert!(source.notes.iter().any(|n| n == "links_skipped"));
    }
    assert_eq!(quick.daily_rows, full_report.daily_rows);
    assert_eq!(
        serde_json::to_value(quick.sources).unwrap(),
        serde_json::to_value(full_report.sources).unwrap()
    );
}
async fn fixture_cancel_completed(f: &Fixture, view: &JobView) {
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(f.0.join("owned-jobs/jobs.sqlite")),
    )
    .await
    .unwrap();
    let mut cancelled = view.clone();
    cancelled.state = "cancelled".into();
    cancelled.phase = "cancelled".into();
    cancelled.report = None;
    sqlx::query("UPDATE jobs SET view=?,cancelled=1 WHERE id=?")
        .bind(serde_json::to_string(&cancelled).unwrap())
        .bind(&view.job_id)
        .execute(&pool)
        .await
        .unwrap();
    let paths: Vec<String> = sqlx::query_scalar("SELECT path FROM files WHERE job=?")
        .bind(&view.job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(paths.len(), 1);
    let frozen_home: String = sqlx::query_scalar("SELECT home FROM jobs WHERE id=?")
        .bind(&view.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        PathBuf::from(&paths[0]),
        PathBuf::from(frozen_home)
            .join(".qoder")
            .join("projects")
            .join("a.jsonl"),
        "frozen paths must remain logical"
    );
    pool.close().await;
}
#[tokio::test]
async fn migrated_root_resume_keeps_original_file_identity_and_rejects_retargeted_copy() {
    let f = Fixture::new();
    let record = event("main", json!(7), json!(true));
    f.write(
        "moved/.qoder/projects/a.jsonl",
        std::slice::from_ref(&record),
    );
    f.write("other/.qoder/projects/a.jsonl", &[record]);
    let logical = f.0.join(".qoder").join("projects");
    let original = f.0.join("moved").join(".qoder").join("projects");
    let replacement = f.0.join("other").join(".qoder").join("projects");
    directory_link(&logical, &original);
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let started = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    let completed = terminal(&jobs, &started.job_id).await;
    assert_eq!(completed.state, "completed");
    fixture_cancel_completed(&f, &completed).await;
    jobs.resume(&started.job_id).await.unwrap();
    let unchanged = terminal(&jobs, &started.job_id).await;
    assert_eq!(unchanged.state, "completed");
    assert_eq!(
        unchanged.report.as_ref().unwrap().summary_rows[0].requests,
        Some(1)
    );
    fixture_cancel_completed(&f, &unchanged).await;
    assert!(logical.starts_with(&f.0));
    assert_eq!(
        fs::canonicalize(&logical).unwrap(),
        fs::canonicalize(&original).unwrap()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_ne!(
            fs::symlink_metadata(&logical).unwrap().file_attributes() & 0x400,
            0
        );
        // Nonrecursive removal deletes this owned junction node, never its target.
        fs::remove_dir(&logical).unwrap();
    }
    #[cfg(not(windows))]
    fs::remove_file(&logical).unwrap();
    assert!(original.join("a.jsonl").is_file());
    directory_link(&logical, &replacement);
    assert_eq!(
        fs::read(original.join("a.jsonl")).unwrap(),
        fs::read(replacement.join("a.jsonl")).unwrap()
    );
    jobs.resume(&started.job_id).await.unwrap();
    let changed = terminal(&jobs, &started.job_id).await;
    assert_eq!(
        changed.state, "failed",
        "a byte-identical copy has a different frozen file identity"
    );
    assert!(changed.report.is_none());
}
#[test]
fn missing_negative_overflow_and_unknown_billable_are_explicit() {
    let f = Fixture::new();
    let mut missing = event("missing", Value::Null, json!(true));
    missing["message"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("credits");
    f.write(
        ".qoder/projects/a.jsonl",
        &[
            missing,
            event("negative", json!(-1), json!(false)),
            event("overflow", json!("1e999"), json!(true)),
            event("unknown", json!(4.5), Value::Null),
        ],
    );
    let r = scan(&f);
    let row = &r["summary_rows"][0];
    assert_eq!(row["requests"], 4);
    assert_eq!(row["platform_credits"]["missing_records"], 3);
    assert_eq!(row["platform_credits"]["unclassified"], 4.5);
    assert_eq!(row["platform_credits"]["unclassified_records"], 1);
    assert_eq!(row["platform_credits"]["billed_records"], 0);
}
#[test]
fn nonfinite_raw_json_is_rejected_without_coercing_credits_to_zero() {
    let f = Fixture::new();
    f.write(
        ".qoder/projects/a.jsonl",
        &[event("valid", json!(2), json!(true))],
    );
    let path = f.0.join(".qoder/projects/nonfinite.jsonl");
    fs::write(path, "{\"type\":\"assistant\",\"timestamp\":\"2026-10-13T04:00:00Z\",\"message\":{\"model\":\"qfmodel\",\"usage\":{\"request_id\":\"bad\",\"credits\":1e999}}}\n").unwrap();
    let report = scan(&f);
    assert_eq!(report["summary_rows"][0]["requests"], 1);
    assert_eq!(report["summary_rows"][0]["platform_credits"]["billed"], 2.0);
    let source = report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["source"] == "qoder_history" && s["profile"] == "global")
        .unwrap();
    assert!(source["notes"]
        .as_array()
        .unwrap()
        .contains(&json!("invalid_json")));
    assert_eq!(source["status"], "partial");
}
#[test]
fn conflicting_duplicates_exclude_amount_and_token_claims() {
    let f = Fixture::new();
    let a = event("conflict", json!(5), json!(true));
    let mut b = a.clone();
    b["message"]["usage"]["credits"] = json!(6);
    f.write(".qoder/projects/a.jsonl", &[a.clone(), b.clone(), a, b]);
    let report = scan(&f);
    assert_eq!(report["summary_rows"][0]["requests"], 1);
    assert_eq!(report["summary_rows"][0]["platform_credits"]["billed"], 0.0);
    assert_eq!(
        report["summary_rows"][0]["platform_credits"]["conflicting_records"],
        1
    );
    let source = report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["source"] == "qoder_history" && r["profile"] == "global")
        .unwrap();
    assert_eq!(source["status"], "partial");
}
#[test]
fn model_day_and_billable_conflicts_all_exclude_credits() {
    for field in ["model", "day", "billable"] {
        let f = Fixture::new();
        let a = event("conflict", json!(5), json!(true));
        let mut b = a.clone();
        match field {
            "model" => b["message"]["model"] = json!("other"),
            "day" => b["timestamp"] = json!("2026-10-14T04:00:00Z"),
            _ => b["message"]["usage"]["billable"] = json!(false),
        }
        f.write(".qoder/projects/a.jsonl", &[a, b]);
        let report = scan(&f);
        assert_eq!(
            report["summary_rows"][0]["platform_credits"]["conflicting_records"], 1,
            "{field}"
        );
        assert_eq!(report["summary_rows"][0]["platform_credits"]["billed"], 0.0);
    }
}
#[test]
fn message_fallback_is_namespaced_and_missing_identity_cannot_claim_requests() {
    let f = Fixture::new();
    let a = event("shared", json!(2), json!(true));
    let mut b = event("fallback", json!(3), json!(false));
    b["message"]["id"] = json!("shared");
    b["message"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("request_id");
    f.write(".qoder/projects/a.jsonl", &[a, b.clone(), b]);
    let r = scan(&f);
    assert_eq!(r["summary_rows"][0]["requests"], 2);
    assert_eq!(r["summary_rows"][0]["platform_credits"]["not_billed"], 3.0);
    let mut unidentified = event("x", json!(999), json!(true));
    unidentified["message"]
        .as_object_mut()
        .unwrap()
        .remove("id");
    unidentified["message"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("request_id");
    f.write(".qoder/projects/missing.jsonl", &[unidentified]);
    let r = scan(&f);
    assert_eq!(r["summary_rows"][0]["requests"], Value::Null);
    assert_eq!(r["summary_rows"][0]["platform_credits"]["billed"], 2.0);
    assert_eq!(
        r["summary_rows"][0]["platform_credits"]["missing_records"],
        1
    );
}
#[test]
fn daily_and_range_merge_credits_without_counting_result_total_or_nonassistant() {
    let f = Fixture::new();
    let a = event("a", json!(1), json!(true));
    let mut b = event("b", json!(2), json!(true));
    b["timestamp"] = json!("2026-10-14T04:00:00Z");
    let mut user = event("user", json!(1000), json!(true));
    user["type"] = json!("user");
    f.write(
        ".qoder/projects/a.jsonl",
        &[
            a,
            b,
            user,
            json!({"type":"result","total_credits":9999,"usage":{"credits":9999}}),
        ],
    );
    let r = scan(&f);
    assert_eq!(r["daily_rows"].as_array().unwrap().len(), 2);
    for key in ["rows", "summary_rows"] {
        assert_eq!(r[key][0]["platform_credits"]["billed"], 3.0);
        assert_eq!(r[key][0]["platform_credits"]["billed_records"], 2);
    }
}
#[test]
fn explicit_nonzero_outputs_and_cache_never_guess_input_or_api_price() {
    let f = Fixture::new();
    let mut a = event("a", json!(1), json!(true));
    a["message"]["usage"]["input_tokens"] = json!(500);
    a["message"]["usage"]["output_tokens"] = json!(20);
    a["message"]["usage"]["cache_read_input_tokens"] = json!(30);
    a["message"]["usage"]["cache_creation_input_tokens"] = json!(40);
    f.write(".qoder/projects/a.jsonl", &[a]);
    let report = scan(&f);
    let r = &report["summary_rows"][0];
    assert_eq!(r["input_tokens"], Value::Null);
    assert_eq!(r["output_tokens"], 20);
    assert_eq!(r["cache_read_tokens"], 30);
    assert_eq!(r["cache_write_tokens"], 40);
    assert_eq!(r["cache_hit_requests"], 0);
    assert_eq!(r["cache_unknown_requests"], 1);
    assert_eq!(r["estimated_cost"], Value::Null);
    assert_eq!(r["estimated_currency"], Value::Null);
}
#[tokio::test]
async fn quick_full_equivalence_and_project_body_and_arbitrary_ids_never_persist() {
    let f = Fixture::new();
    let raw_id = "PRIVATE-ARBITRARY-ID-".repeat(100);
    let mut a = event(&raw_id, json!(1.25), json!(true));
    a["message"]["id"] = json!(raw_id);
    let mut fallback = a.clone();
    fallback["message"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("request_id");
    f.write(".qoder/projects/a.jsonl", &[a.clone(), fallback.clone()]);
    f.write(".qoder/projects/fork/a.jsonl", &[a, fallback]);
    f.write(
        ".qoder-cn/projects/a.jsonl",
        &[event("cn", json!(2), json!(false))],
    );
    f.write(
        ".qoder/projects/invalid-billable.jsonl",
        &[
            event("string-billable", json!(3), json!("true")),
            event("array-billable", json!(4), json!([true])),
            event("object-billable", json!(5), json!({"value":true})),
        ],
    );
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let started = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    let full = terminal(&jobs, &started.job_id).await;
    assert_eq!(full.state, "completed", "{full:?}");
    let report = full.report.unwrap();
    assert_eq!(quick.daily_rows, report.daily_rows);
    assert_eq!(
        serde_json::to_value(&quick.sources).unwrap(),
        serde_json::to_value(&report.sources).unwrap()
    );
    let scratch = fs::read(f.0.join("owned-jobs/jobs.sqlite")).unwrap();
    for private in ["PRIVATE-BODY-MUST-NOT-PERSIST", "PRIVATE-ARBITRARY-ID-"] {
        assert!(
            !scratch
                .windows(private.len())
                .any(|w| w == private.as_bytes()),
            "{private}"
        );
    }
}
#[test]
fn old_serialized_rows_default_to_no_platform_credits() {
    let f = Fixture::new();
    f.write(
        ".qoder/projects/a.jsonl",
        &[event("a", json!(1), json!(true))],
    );
    let mut r = scan(&f)["summary_rows"][0].clone();
    r.as_object_mut().unwrap().remove("platform_credits");
    let row: llm_gateway_lib::client_usage::ClientUsageRow = serde_json::from_value(r).unwrap();
    assert!(row.platform_credits.is_none());
}
#[tokio::test]
async fn empty_request_ids_use_distinct_message_fallback_and_empty_message_ids_are_unknown() {
    for has_message in [true, false] {
        let f = Fixture::new();
        let mut a = event("a", json!(1), json!(true));
        let mut b = event("b", json!(2), json!(true));
        if has_message {
            a["message"]["usage"]["request_id"] = json!("");
            b["message"]["usage"]["request_id"] = json!("");
        } else {
            for record in [&mut a, &mut b] {
                record["message"]["usage"]
                    .as_object_mut()
                    .unwrap()
                    .remove("request_id");
                record["message"]["id"] = json!("");
            }
        }
        f.write(".qoder/projects/a.jsonl", &[a, b]);
        let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
        let row = &quick.summary_rows[0];
        if has_message {
            assert_eq!(row.requests, Some(2));
            assert_eq!(row.platform_credits.as_ref().unwrap().billed, 3.0);
        } else {
            assert_eq!(row.requests, None);
            assert_eq!(row.platform_credits.as_ref().unwrap().billed, 0.0);
            assert_eq!(row.platform_credits.as_ref().unwrap().missing_records, 2);
        }
        let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
        let started = jobs
            .start(
                f.0.clone(),
                "2026-10-12".into(),
                "2026-10-14".into(),
                vec![],
            )
            .await
            .unwrap();
        let full = terminal(&jobs, &started.job_id).await;
        assert_eq!(full.state, "completed");
        assert_eq!(
            quick.daily_rows,
            full.report.unwrap().daily_rows,
            "has_message={has_message}"
        );
    }
}
#[tokio::test]
async fn unsafe_model_names_still_conflict_without_persisting_raw_labels() {
    let f = Fixture::new();
    let mut a = event("same", json!(5), json!(true));
    let mut b = a.clone();
    a["message"]["model"] = json!("PRIVATE custom model A");
    b["message"]["model"] = json!("PRIVATE custom model B");
    f.write(".qoder/projects/a.jsonl", &[a, b]);
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let started = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    let full = terminal(&jobs, &started.job_id).await;
    assert_eq!(full.state, "completed");
    for report in [quick, full.report.unwrap()] {
        let row = &report.summary_rows[0];
        assert_eq!(row.model, "unknown");
        assert_eq!(
            row.platform_credits.as_ref().unwrap().conflicting_records,
            1
        );
        assert_eq!(row.platform_credits.as_ref().unwrap().billed, 0.0);
    }
    let scratch = fs::read(f.0.join("owned-jobs/jobs.sqlite")).unwrap();
    assert!(!scratch
        .windows("PRIVATE custom model".len())
        .any(|w| w == b"PRIVATE custom model"));
}
#[tokio::test]
async fn project_root_junction_is_skipped_in_both_modes_without_reading_owned_target() {
    let f = Fixture::new();
    f.write(
        "owned-target/a.jsonl",
        &[event("target", json!(123), json!(true))],
    );
    let target = f.0.join("owned-target");
    let link = f.0.join(".qoder").join("projects");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::{fs::MetadataExt, process::CommandExt};
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert_ne!(
            fs::symlink_metadata(&link).unwrap().file_attributes() & 0x400,
            0
        );
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let started = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    let full = terminal(&jobs, &started.job_id).await;
    assert_eq!(full.state, "completed");
    for report in [quick, full.report.unwrap()] {
        assert!(
            report.daily_rows.is_empty(),
            "a source-root link must not be followed"
        );
        let source = report
            .sources
            .iter()
            .find(|s| s.source == "qoder_history" && s.profile == "global")
            .unwrap();
        assert!(source.notes.iter().any(|n| n == "links_skipped"));
        assert_eq!(source.files_scanned, 0);
        assert_eq!(source.bytes_read, 0);
    }
}
#[tokio::test]
async fn old_frozen_snapshot_resumes_without_adding_new_project_sources() {
    let f = Fixture::new();
    f.write(".codex/sessions/a.jsonl", &[json!({"type":"token_usage_record", "timestamp":"2026-10-13T04:00:00Z",
        "payload":{"thread_id":"fixture","response_id":"a","usage":{"input_tokens":10,"output_tokens":1}}})]);
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let started = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    let mut frozen = terminal(&jobs, &started.job_id).await;
    assert_eq!(frozen.state, "completed");
    assert_eq!(frozen.files_total, 1);
    // This is exclusively the fixture's owned scratch DB. Remove the new scope
    // markers to reproduce a snapshot generated by the prior scanner version.
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(f.0.join("owned-jobs/jobs.sqlite")),
    )
    .await
    .unwrap();
    let raw: String = sqlx::query_scalar("SELECT notes FROM jobs WHERE id=?")
        .bind(&started.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut notes: Vec<(String, Vec<String>)> = serde_json::from_str(&raw).unwrap();
    notes.retain(|(path, _)| {
        !path.ends_with(".qoder/projects") && !path.ends_with(".qoder-cn/projects")
    });
    // Windows paths carry backslashes.
    notes.retain(|(path, _)| {
        !path.ends_with(".qoder\\projects") && !path.ends_with(".qoder-cn\\projects")
    });
    frozen.state = "cancelled".into();
    frozen.phase = "cancelled".into();
    frozen.report = None;
    sqlx::query("UPDATE jobs SET view=?,notes=?,cancelled=1 WHERE id=?")
        .bind(serde_json::to_string(&frozen).unwrap())
        .bind(serde_json::to_string(&notes).unwrap())
        .bind(&started.job_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    f.write(
        ".qoder/projects/new.jsonl",
        &[event("new", json!(100), json!(true))],
    );
    f.write(
        ".qoder-cn/projects/new.jsonl",
        &[event("new", json!(200), json!(true))],
    );
    jobs.resume(&started.job_id).await.unwrap();
    let resumed = terminal(&jobs, &started.job_id).await;
    assert_eq!(resumed.state, "completed", "{resumed:?}");
    assert_eq!(resumed.files_total, 1);
    assert!(
        resumed.coverage_complete,
        "legacy frozen-file coverage must keep its original meaning"
    );
    let report = resumed.report.unwrap();
    assert!(!report
        .summary_rows
        .iter()
        .any(|r| r.source == "qoder_history"));
    for source in report
        .sources
        .iter()
        .filter(|s| s.source == "qoder_history")
    {
        assert_eq!(source.status, "unavailable");
        assert!(source
            .notes
            .iter()
            .any(|n| n == "snapshot_source_not_included"));
        assert_eq!(source.files_scanned, 0);
    }
}
#[test]
fn credits_and_diagnostic_sources_do_not_merge_with_original_usd_api_estimate() {
    use llm_gateway_lib::client_usage::{
        apply_reference_prices, scan_from_home_with_prices, UsagePriceReference,
    };
    let f = Fixture::new();
    let mut history = event("a", json!(100), json!(true));
    history["message"]["model"] = json!("m");
    history["message"]["usage"]["input_tokens"] = json!(100);
    history["message"]["usage"]["output_tokens"] = json!(10);
    f.write(".qoder/projects/a.jsonl", &[history]);
    f.write(".codex/sessions/a.jsonl", &[
        json!({"type":"session_meta","payload":{"id":"session","model_provider":"p"}}),
        json!({"type":"turn_context","payload":{"turn_id":"turn","model":"m"}}),
        json!({"type":"token_usage_record","timestamp":"2026-10-13T04:00:00Z","payload":{
            "thread_id":"session","response_id":"a","turn_id":"turn","usage":{
                "input_tokens":100,"output_tokens":10,"cached_input_tokens":0,"cache_write_input_tokens":0}}})]);
    f.write(
        ".qoder/logs/sessions/session/nested/a.jsonl",
        &[json!({"type":"model.response.completed",
        "ts":"2026-10-13T04:00:00Z","request_id":"a","data":{"model":"m","output_tokens":10}})],
    );
    let prices = vec![UsagePriceReference {
        provider: "p".into(),
        models: vec!["m".into()],
        price: serde_json::from_value(
            json!({"prompt":1,"completion":1,"cache_read":0.1,"cache_creation":1,"currency":"usd"}),
        )
        .unwrap(),
    }];
    let mut report = scan_from_home_with_prices(&f.0, "2026-10-12", "2026-10-14", &prices).unwrap();
    assert_eq!(report.summary_rows.len(), 3);
    let codex = report
        .summary_rows
        .iter()
        .find(|r| r.source == "codex")
        .unwrap();
    assert!(codex.estimated_cost.unwrap() > 0.0);
    assert_eq!(codex.estimated_currency.as_deref(), Some("usd"));
    assert!(codex.platform_credits.is_none());
    apply_reference_prices(&mut report, &prices);
    let history = report
        .summary_rows
        .iter()
        .find(|r| r.source == "qoder_history")
        .unwrap();
    assert_eq!(history.platform_credits.as_ref().unwrap().billed, 100.0);
    assert!(history.estimated_cost.is_none());
    assert!(history.estimated_currency.is_none());
}
