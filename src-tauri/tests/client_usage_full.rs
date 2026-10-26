use llm_gateway_lib::client_usage::jobs::{JobView, UsageJobs};
use llm_gateway_lib::client_usage::scan_from_home;
use serde_json::json;
use std::{fs, io::Write, path::PathBuf};

#[tokio::test]
async fn new_job_persists_versioned_complete_price_snapshot() {
    use llm_gateway_lib::client_usage::UsagePriceReference;
    let f = Fixture::new();
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let configured = vec![UsagePriceReference {
        provider: "manual".into(),
        models: vec!["fixture".into()],
        price: serde_json::from_value(json!({"prompt":2,"completion":10,"currency":"usd"}))
            .unwrap(),
    }];
    let expected = serde_json::to_value(&configured).unwrap();
    let started = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            configured,
        )
        .await
        .unwrap();
    assert_eq!(terminal(&jobs, &started.job_id).await.state, "completed");
    let pool = owned_pool(&f).await;
    let raw: String = sqlx::query_scalar("SELECT prices FROM jobs WHERE id=?")
        .bind(&started.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        stored["schema_version"], 1,
        "new task must freeze builtin prices too: {raw}"
    );
    assert_eq!(stored["catalog"], "2026-10-15");
    assert_eq!(stored["configured"], expected);
    pool.close().await;
    jobs.shutdown().await.unwrap();
}

async fn owned_pool(f: &Fixture) -> sqlx::SqlitePool {
    sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(f.0.join("owned-jobs/jobs.sqlite")),
    )
    .await
    .unwrap()
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("llmgw-full-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".codex/sessions")).unwrap();
        Self(root)
    }
    fn record(id: &str, input: u64) -> String {
        json!({"timestamp":"2026-10-13T04:00:00Z","type":"token_usage_record","payload":{"thread_id":"session","response_id":id,"turn_id":"turn","usage":{"input_tokens":input,"output_tokens":1,"cached_input_tokens":0,"cache_write_input_tokens":0}}}).to_string()+"\n"
    }
    fn large(&self, index: usize) {
        let mut file =
            fs::File::create(self.0.join(format!(".codex/sessions/{index}.jsonl"))).unwrap();
        file.write_all(b"{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"turn\",\"model\":\"fixture\"}}\n").unwrap();
        file.write_all(Self::record(&format!("head-{index}"), 100).as_bytes())
            .unwrap();
        file.write_all(b"{\"message\":\"").unwrap();
        let block = vec![b'x'; 1024 * 1024];
        for _ in 0..34 {
            file.write_all(&block).unwrap();
        }
        file.write_all(b"\"}\n").unwrap();
        file.write_all(Self::record(&format!("tail-{index}"), 7).as_bytes())
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn reads_frozen_heads_tails_more_than_source_limit_and_500_files() {
    let f = Fixture::new();
    for i in 0..5 {
        f.large(i);
    }
    for i in 5..506 {
        fs::write(
            f.0.join(format!(".codex/sessions/{i}.jsonl")),
            Fixture::record(&format!("small-{i}"), 1),
        )
        .unwrap();
    }
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    assert!(
        quick
            .summary_rows
            .iter()
            .filter_map(|r| r.input_tokens)
            .sum::<u64>()
            < 1036
    );
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let view = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    let completed = terminal(&jobs, &view.job_id).await;
    assert_eq!(completed.state, "completed", "{:?}", completed);
    assert!(completed.coverage_complete);
    let report = completed.report.unwrap();
    assert_eq!(
        report
            .summary_rows
            .iter()
            .filter_map(|r| r.input_tokens)
            .sum::<u64>(),
        1036
    );
    assert_eq!(report.sources[0].files_scanned, 506);
}

async fn terminal(jobs: &UsageJobs, id: &str) -> JobView {
    tokio::time::timeout(std::time::Duration::from_secs(180), async {
        loop {
            let view = jobs.status(id).await.unwrap();
            if view.state != "running" {
                return view;
            }
            assert!(view.report.is_none());
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("job should stop")
}
async fn full(f: &Fixture) -> JobView {
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let view = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    terminal(&jobs, &view.job_id).await
}
fn many(f: &Fixture) -> PathBuf {
    let path = f.0.join(".codex/sessions/many.jsonl");
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"session\",\"model_provider\":\"p\"}}\n{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"turn\",\"model\":\"fixture\"}}\n").unwrap();
    for i in 0..8000 {
        file.write_all(Fixture::record(&format!("id-{i}"), 1).as_bytes())
            .unwrap();
    }
    path
}
async fn checkpoint_cancel(f: &Fixture) -> (UsageJobs, String) {
    checkpoint_cancel_priced(f, vec![]).await
}
async fn checkpoint_cancel_priced(
    f: &Fixture,
    prices: Vec<llm_gateway_lib::client_usage::UsagePriceReference>,
) -> (UsageJobs, String) {
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let view = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            prices,
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let status = jobs.status(&view.job_id).await.unwrap();
            assert_eq!(status.state, "running");
            if status.records_scanned >= 256 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let cancel = jobs.cancel(&view.job_id).await.unwrap();
    assert!(cancel.cancel_requested);
    let stop = terminal(&jobs, &view.job_id).await;
    assert_eq!(stop.state, "cancelled");
    assert!(stop.records_scanned >= 256 && stop.records_scanned < 8000);
    assert!(stop.report.is_none());
    (jobs, view.job_id)
}

#[tokio::test]
async fn small_full_and_quick_have_same_usage_prices_and_source_evidence() {
    let f = Fixture::new();
    let bad = json!({"type":"session_meta","payload":{"id":"child","forked_from_id":{"text":"SECRET_BODY_MUST_NOT_PERSIST"}}});
    let usage = json!({"timestamp":"2026-10-13T04:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"model":"fixture","total_token_usage":{"input_tokens":100,"output_tokens":10,"cached_input_tokens":0,"cache_write_input_tokens":0},"last_token_usage":{"input_tokens":100,"output_tokens":10,"cached_input_tokens":0,"cache_write_input_tokens":0}}}});
    fs::write(
        f.0.join(".codex/sessions/child.jsonl"),
        format!("{bad}\n{usage}\n"),
    )
    .unwrap();
    fs::write(
        f.0.join(".codex/sessions/later.jsonl"),
        format!(
            "broken\n{}\n{usage}\n",
            json!({"type":"session_meta","payload":{"id":"late"}})
        ),
    )
    .unwrap();
    for scope in ["a", "b"] {
        let p =
            f.0.join(format!(".qoder/logs/sessions/{scope}/nested/events.jsonl"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p,format!("{}\n{}\n",json!({"type":"model.response.completed","ts":"2026-10-13T04:00:00Z","request_id":"same","data":{"model":"fixture","output_tokens":4}}),json!({"type":"model.response.completed","ts":"2026-10-13T04:00:00Z","request_id":"same","data":{"model":"fixture","output_tokens":4}}))).unwrap();
    }
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    let result = full(&f).await;
    assert_eq!(result.state, "completed");
    assert!(!result.coverage_complete);
    let report = result.report.unwrap();
    assert_eq!(quick.daily_rows, report.daily_rows);
    assert_eq!(
        serde_json::to_value(&quick.sources).unwrap(),
        serde_json::to_value(&report.sources).unwrap()
    );
    assert!(!fs::read(f.0.join("owned-jobs/jobs.sqlite"))
        .unwrap()
        .windows(28)
        .any(|w| w == b"SECRET_BODY_MUST_NOT_PERSIST"));
}

#[tokio::test]
async fn checkpoint_resume_uses_frozen_prefix_and_no_duplicate_usage() {
    let f = Fixture::new();
    let path = many(&f);
    let (jobs, id) = checkpoint_cancel(&f).await;
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(Fixture::record("appended", 9000).as_bytes())
        .unwrap();
    drop(file);
    jobs.resume(&id).await.unwrap();
    let done = terminal(&jobs, &id).await;
    assert_eq!(done.state, "completed");
    assert_eq!(
        done.report
            .unwrap()
            .summary_rows
            .iter()
            .filter_map(|r| r.input_tokens)
            .sum::<u64>(),
        8000
    );
}

#[tokio::test]
async fn truncated_replaced_and_rewritten_snapshots_fail_closed() {
    for mutation in ["truncate", "replace", "rewrite"] {
        let f = Fixture::new();
        let path = many(&f);
        let (jobs, id) = checkpoint_cancel(&f).await;
        match mutation {
            "truncate" => fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(0)
                .unwrap(),
            "replace" => {
                let bytes = fs::read(&path).unwrap();
                fs::rename(&path, path.with_extension("old")).unwrap();
                fs::write(&path, bytes).unwrap();
            }
            _ => {
                let mut bytes = fs::read(&path).unwrap();
                let pos = bytes.windows(4).position(|w| w == b"id-0").unwrap();
                bytes[pos] = b'X';
                fs::write(&path, bytes).unwrap();
            }
        }
        jobs.resume(&id).await.unwrap();
        let failed = terminal(&jobs, &id).await;
        assert_eq!(failed.state, "failed", "{mutation}");
        assert!(failed.report.is_none());
        assert!(!failed.coverage_complete);
        assert!(jobs.resume(&id).await.is_err());
    }
}

#[tokio::test]
async fn invalid_dates_ids_relative_paths_and_quick_full_concurrency_are_rejected() {
    let f = Fixture::new();
    let jobs = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    assert!(UsageJobs::new(PathBuf::from("relative")).is_err());
    assert!(jobs
        .start(
            PathBuf::from("relative"),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![]
        )
        .await
        .is_err());
    assert!(jobs
        .start(f.0.clone(), "bad".into(), "2026-10-14".into(), vec![])
        .await
        .is_err());
    assert!(jobs.status("bad/id").await.is_err());
    assert!(jobs.resume("bad").await.is_err());
    assert!(jobs.cancel("bad").await.is_err());
    let permit = jobs.quick_permit().unwrap();
    assert!(jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![]
        )
        .await
        .is_err());
    drop(permit);
    many(&f);
    let view = jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    assert!(jobs.quick_permit().is_err());
    assert!(jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![]
        )
        .await
        .is_err());
    assert!(jobs.resume(&view.job_id).await.is_err());
    jobs.shutdown().await.unwrap();
    assert_eq!(jobs.status(&view.job_id).await.unwrap().state, "cancelled");
    assert!(jobs.quick_permit().is_err());
}

#[tokio::test]
async fn giant_unknown_field_does_not_hide_usage_on_same_line_or_following_line() {
    let f = Fixture::new();
    let p = f.0.join(".codex/sessions/giant.jsonl");
    let mut file = fs::File::create(&p).unwrap();
    file.write_all(b"{\"message\":\"").unwrap();
    file.write_all(&vec![b'z'; 2 * 1024 * 1024]).unwrap();
    let usage = Fixture::record("giant", 77);
    file.write_all(b"\",").unwrap();
    file.write_all(&usage.as_bytes()[1..]).unwrap();
    file.write_all(Fixture::record("after", 11).as_bytes())
        .unwrap();
    drop(file);
    let done = full(&f).await;
    assert_eq!(done.state, "completed");
    assert!(done.coverage_complete);
    assert_eq!(
        done.report
            .unwrap()
            .summary_rows
            .iter()
            .filter_map(|r| r.input_tokens)
            .sum::<u64>(),
        88
    );
}

#[tokio::test]
async fn restart_only_reads_owned_state_then_explicit_resume_validates_snapshot() {
    let f = Fixture::new();
    let path = many(&f);
    let (jobs, id) = checkpoint_cancel(&f).await;
    jobs.shutdown().await.unwrap();
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(f.0.join("owned-jobs/jobs.sqlite")),
    )
    .await
    .unwrap();
    let mut view = jobs.status(&id).await.unwrap();
    let old_read = view.snapshot_bytes_read;
    view.state = "running".into();
    sqlx::query("UPDATE jobs SET view=? WHERE id=?")
        .bind(serde_json::to_string(&view).unwrap())
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    drop(jobs);
    let restarted = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    let interrupted = restarted.current().await.unwrap().unwrap();
    assert_eq!(interrupted.state, "interrupted");
    assert!(interrupted.report.is_none());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        restarted.status(&id).await.unwrap().snapshot_bytes_read,
        old_read
    );
    let mut appended = fs::OpenOptions::new().append(true).open(path).unwrap();
    appended
        .write_all(Fixture::record("late", 9000).as_bytes())
        .unwrap();
    drop(appended);
    restarted.resume(&id).await.unwrap();
    let result = terminal(&restarted, &id).await;
    assert_eq!(result.state, "completed");
    assert_eq!(
        result
            .report
            .unwrap()
            .summary_rows
            .iter()
            .filter_map(|r| r.input_tokens)
            .sum::<u64>(),
        8000
    );
}

#[tokio::test]
async fn checkpoint_save_failure_is_failed_and_does_not_advance_durable_cursor() {
    let f = Fixture::new();
    many(&f);
    let (jobs, id) = checkpoint_cancel(&f).await;
    let before = jobs.status(&id).await.unwrap();
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(f.0.join("owned-jobs/jobs.sqlite")),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TRIGGER reject_projection BEFORE INSERT ON records BEGIN SELECT RAISE(ABORT,'owned fixture write failure'); END").execute(&pool).await.unwrap();
    pool.close().await;
    jobs.resume(&id).await.unwrap();
    let failed = terminal(&jobs, &id).await;
    assert_eq!(failed.state, "failed");
    assert!(failed.report.is_none());
    assert_eq!(failed.snapshot_bytes_read, before.snapshot_bytes_read);
    assert_eq!(failed.records_scanned, before.records_scanned);
    assert!(jobs.resume(&id).await.is_err());
}

#[tokio::test]
async fn dsh_full_ledger_preserves_numeric_unknown_fields_and_profiles() {
    let f = Fixture::new();
    for profile in [".dsh", ".dsh-beta"] {
        let path = f.0.join(format!("{profile}/dsh-usage/usage-ledger.json"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path,json!({"version":1,"ignored":{"prompt":"SECRET_BODY_MUST_NOT_PERSIST"},"days":{"2026-10-13":{"p":{"m":{"inputTokens":9,"outputTokens":3,"cacheReadTokens":2,"calls":2}}}}}).to_string()).unwrap();
    }
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    let done = full(&f).await;
    assert_eq!(done.state, "completed");
    assert!(done.coverage_complete);
    let report = done.report.unwrap();
    assert_eq!(quick.daily_rows, report.daily_rows);
    assert_eq!(
        serde_json::to_value(&quick.sources).unwrap(),
        serde_json::to_value(&report.sources).unwrap()
    );
    assert!(!fs::read(f.0.join("owned-jobs/jobs.sqlite"))
        .unwrap()
        .windows(28)
        .any(|w| w == b"SECRET_BODY_MUST_NOT_PERSIST"));
}

#[tokio::test]
async fn qoder_arbitrary_long_ids_preserve_exact_equality_without_plaintext_persistence() {
    let f = Fixture::new();
    let path = f.0.join(".qoder/logs/sessions/s/n/events.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let prefix = "QODER_PRIVATE_LONG_IDENTIFIER_".repeat(30);
    let mut body = String::new();
    for suffix in ["a", "b", "a"] {
        body+=&json!({"type":"model.response.completed","ts":"2026-10-13T04:00:00Z","request_id":format!("{prefix}{suffix}"),"data":{"model":"fixture","output_tokens":5}}).to_string();
        body.push('\n');
    }
    fs::write(path, body).unwrap();
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    let completed = full(&f).await;
    assert_eq!(completed.state, "completed");
    assert!(completed.coverage_complete);
    let report = completed.report.unwrap();
    assert_eq!(quick.daily_rows, report.daily_rows);
    assert_eq!(report.summary_rows[0].requests, Some(2));
    let bytes = fs::read(f.0.join("owned-jobs/jobs.sqlite")).unwrap();
    assert!(!bytes.windows(prefix.len()).any(|w| w == prefix.as_bytes()));
}

#[tokio::test]
async fn resume_uses_durable_original_prices_and_aggregation_matches_quick() {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    let f = Fixture::new();
    many(&f);
    let prices = vec![UsagePriceReference {
        provider: "p".into(),
        models: vec!["fixture".into()],
        price: serde_json::from_value(
            json!({"prompt":1,"completion":2,"cache_read":0.1,"cache_creation":1,"currency":"usd"}),
        )
        .unwrap(),
    }];
    let (jobs, id) = checkpoint_cancel_priced(&f, prices.clone()).await;
    jobs.resume(&id).await.unwrap();
    let result = terminal(&jobs, &id).await;
    assert_eq!(result.state, "completed");
    let report = result.report.unwrap();
    let quick = scan_from_home_with_prices(&f.0, "2026-10-12", "2026-10-14", &prices).unwrap();
    assert_eq!(quick.daily_rows, report.daily_rows);
    assert!((report.summary_rows[0].estimated_cost.unwrap() - 0.024).abs() < 1e-10);
}

#[tokio::test]
async fn incomplete_frozen_tail_is_partial_and_cannot_prove_an_independent_legacy_request() {
    let f = Fixture::new();
    let path = f.0.join(".codex/sessions/tail.jsonl");
    let head = json!({"type":"session_meta","payload":{"id":"session"}});
    let usage = json!({"timestamp":"2026-10-13T04:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"model":"fixture","total_token_usage":{"input_tokens":100,"output_tokens":10},"last_token_usage":{"input_tokens":100,"output_tokens":10}}}});
    fs::write(
        path,
        format!(
            "{head}\n{usage}\n{}",
            Fixture::record("unterminated", 900).trim_end()
        ),
    )
    .unwrap();
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    let completed = full(&f).await;
    assert_eq!(completed.state, "completed");
    assert!(!completed.coverage_complete);
    let report = completed.report.unwrap();
    assert_eq!(quick.daily_rows, report.daily_rows);
    assert!(report.sources[0]
        .notes
        .iter()
        .any(|n| n == "incomplete_line"));
}

#[tokio::test]
async fn managers_for_same_scratch_root_share_real_gate_and_cancellation() {
    let f = Fixture::new();
    let root = f.0.join("owned-jobs");
    let a = UsageJobs::new(root.clone()).unwrap();
    let b = UsageJobs::new(root).unwrap();
    let permit = a.quick_permit().unwrap();
    assert!(
        b.quick_permit().is_err(),
        "a second manager must not run another quick worker"
    );
    drop(permit);
    many(&f);
    let job = a
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![],
        )
        .await
        .unwrap();
    let status = b.current().await.unwrap().unwrap();
    assert_eq!(status.job_id, job.job_id);
    assert_eq!(status.state, "running");
    assert!(b
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![]
        )
        .await
        .is_err());
    assert!(b.resume(&job.job_id).await.is_err());
    assert!(b.cancel(&job.job_id).await.unwrap().cancel_requested);
    assert_eq!(terminal(&a, &job.job_id).await.state, "cancelled");
}

#[tokio::test]
async fn owner_lease_is_exclusive_across_real_processes() {
    const MODE: &str = "LLMGW_OWNED_USAGE_LEASE_TEST_MODE";
    if let Ok(mode) = std::env::var(MODE) {
        let root = PathBuf::from(std::env::var("LLMGW_OWNED_USAGE_LEASE_TEST_ROOT").unwrap());
        let jobs = UsageJobs::new(root.clone()).unwrap();
        if mode == "blocked" {
            assert!(
                jobs.quick_permit().is_err(),
                "another process already holds the real usage worker lease"
            );
            return;
        }
        let home = PathBuf::from(std::env::var("LLMGW_OWNED_USAGE_LEASE_TEST_HOME").unwrap());
        let view = jobs
            .start(home, "2026-10-12".into(), "2026-10-14".into(), vec![])
            .await
            .unwrap();
        fs::write(root.join("child-ready"), &view.job_id).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !root.join("child-stop").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        jobs.cancel(&view.job_id).await.unwrap();
        assert_eq!(terminal(&jobs, &view.job_id).await.state, "cancelled");
        jobs.shutdown().await.unwrap();
        return;
    }
    let f = Fixture::new();
    let root = f.0.join("owned-jobs");
    let jobs = UsageJobs::new(root.clone()).unwrap();
    let command = |mode: &str| {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("owner_lease_is_exclusive_across_real_processes")
            .arg("--nocapture")
            .env(MODE, mode)
            .env("LLMGW_OWNED_USAGE_LEASE_TEST_ROOT", &root)
            .env("LLMGW_OWNED_USAGE_LEASE_TEST_HOME", &f.0);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command
    };
    let permit = jobs.quick_permit().unwrap();
    let blocked = command("blocked").output().unwrap();
    assert!(
        blocked.status.success(),
        "child: {}",
        String::from_utf8_lossy(&blocked.stdout)
    );
    drop(permit);
    let path = many(&f);
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    for i in 8000..68000 {
        file.write_all(Fixture::record(&format!("cross-{i}"), 1).as_bytes())
            .unwrap();
    }
    drop(file);
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        command("full")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !root.join("child-ready").exists() {
            assert!(child.0.try_wait().unwrap().is_none());
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let id = fs::read_to_string(root.join("child-ready")).unwrap();
    let status = jobs.current().await.unwrap().unwrap();
    assert_eq!(status.job_id, id);
    assert_eq!(status.state, "running");
    assert_eq!(jobs.status(&id).await.unwrap().state, "running");
    assert!(jobs
        .start(
            f.0.clone(),
            "2026-10-12".into(),
            "2026-10-14".into(),
            vec![]
        )
        .await
        .is_err());
    assert!(jobs.resume(&id).await.is_err());
    assert!(jobs.cancel(&id).await.is_err(),"cross-process cancel must explicitly refuse rather than pretend it delivered an atomic cancellation");
    fs::write(root.join("child-stop"), b"").unwrap();
    assert!(child.0.wait().unwrap().success());
    assert_eq!(jobs.status(&id).await.unwrap().state, "cancelled");
    assert!(jobs.quick_permit().is_ok());
    // A real process death releases its OS lease, then and only then may another
    // process recover running -> interrupted and explicitly resume its checkpoint.
    fs::remove_file(root.join("child-ready")).unwrap();
    fs::remove_file(root.join("child-stop")).unwrap();
    let mut crashed = Child(
        command("full")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let crash_id = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            assert!(crashed.0.try_wait().unwrap().is_none());
            if let Ok(id) = fs::read_to_string(root.join("child-ready")) {
                if !id.is_empty() {
                    let status = jobs.status(&id).await.unwrap();
                    assert_eq!(status.state, "running");
                    if status.records_scanned >= 256 {
                        return id;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    crashed.0.kill().unwrap();
    crashed.0.wait().unwrap();
    let interrupted = jobs.current().await.unwrap().unwrap();
    assert_eq!(interrupted.job_id, crash_id);
    assert_eq!(interrupted.state, "interrupted");
    assert!(interrupted.report.is_none());
    jobs.resume(&crash_id).await.unwrap();
    let completed = terminal(&jobs, &crash_id).await;
    assert_eq!(completed.state, "completed");
    assert_eq!(
        completed
            .report
            .unwrap()
            .summary_rows
            .iter()
            .filter_map(|r| r.input_tokens)
            .sum::<u64>(),
        68000
    );
}

#[tokio::test]
async fn ignored_fields_cannot_turn_invalid_raw_unicode_or_numbers_into_complete_fork_evidence() {
    for bad in [
        b"{\"message\":\"\xff\"}\n".as_slice(),
        b"{\"message\":\"\\uD800\"}\n".as_slice(),
        b"{\"noise\":1e999}\n".as_slice(),
    ] {
        let f = Fixture::new();
        let path = f.0.join(".codex/sessions/invalid.jsonl");
        let mut file = fs::File::create(path).unwrap();
        file.write_all(b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"session\"}}\n")
            .unwrap();
        file.write_all(bad).unwrap();
        let usage = json!({"timestamp":"2026-10-13T04:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"model":"fixture","total_token_usage":{"input_tokens":100,"output_tokens":10},"last_token_usage":{"input_tokens":100,"output_tokens":10}}}});
        file.write_all(format!("{usage}\n").as_bytes()).unwrap();
        drop(file);
        let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
        assert!(quick.sources[0].notes.iter().any(|n| n == "invalid_json"));
        let completed = full(&f).await;
        assert_eq!(completed.state, "completed");
        assert!(!completed.coverage_complete, "bad raw bytes: {bad:?}");
        let report = completed.report.unwrap();
        assert_eq!(quick.daily_rows, report.daily_rows);
        assert!(report.sources[0].notes.iter().any(|n| n == "invalid_json"));
    }
}

#[tokio::test]
async fn valid_utf8_surrogate_pairs_literal_escapes_and_long_finite_numbers_remain_complete() {
    let f = Fixture::new();
    let path = f.0.join(".codex/sessions/unicode.jsonl");
    let mut file = fs::File::create(path).unwrap();
    file.write_all(b"{\"message\":\"\\uD83D\\uDE00\\\\uD800\"}\n")
        .unwrap();
    file.write_all(
        format!(
            "{{\"message\":\"{}😀\",\"noise\":0.{}1}}\n",
            "x".repeat(8190),
            "0".repeat(1500)
        )
        .as_bytes(),
    )
    .unwrap();
    file.write_all(Fixture::record("positive", 12).as_bytes())
        .unwrap();
    drop(file);
    let quick = scan_from_home(&f.0, "2026-10-12", "2026-10-14").unwrap();
    assert!(!quick.sources[0].notes.iter().any(|n| n == "invalid_json"));
    let completed = full(&f).await;
    assert_eq!(completed.state, "completed");
    assert!(completed.coverage_complete);
    let report = completed.report.unwrap();
    assert_eq!(quick.daily_rows, report.daily_rows);
    assert_eq!(report.summary_rows[0].input_tokens, Some(12));
}

fn snapshot_sources(f: &Fixture) {
    let path = many(f);
    let raw = fs::read_to_string(&path)
        .unwrap()
        .replace("\"model_provider\":\"p\"", "\"model_provider\":\"openai\"")
        .replace("\"model\":\"fixture\"", "\"model\":\"gpt-6.1-sol\"");
    fs::write(path, raw).unwrap();
    let path = f.0.join(".dsh/dsh-usage/usage-ledger.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path,json!({"version":1,"days":{"2026-10-13":{"commandcode":{"deepseek/deepseek-v4.1-flash":{"inputTokens":10,"outputTokens":40,"cacheReadTokens":20,"cacheWriteTokens":30,"calls":1}}}}}).to_string()).unwrap();
}
async fn durable_watermark(pool: &sqlx::SqlitePool, id: &str) -> serde_json::Value {
    use sqlx::Row;
    let job = sqlx::query("SELECT prices,discovered,notes FROM jobs WHERE id=?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    let files =
        sqlx::query("SELECT ordinal,offset,prefix,done FROM files WHERE job=? ORDER BY ordinal")
            .bind(id)
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                json!([
                    row.get::<i64, _>("ordinal"),
                    row.get::<i64, _>("offset"),
                    row.get::<String, _>("prefix"),
                    row.get::<i64, _>("done")
                ])
            })
            .collect::<Vec<_>>();
    let records = sqlx::query(
        "SELECT file,ordinal,projection,complete FROM records WHERE job=? ORDER BY file,ordinal",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        json!([
            row.get::<i64, _>("file"),
            row.get::<i64, _>("ordinal"),
            row.get::<Option<String>, _>("projection"),
            row.get::<i64, _>("complete")
        ])
    })
    .collect::<Vec<_>>();
    json!({"prices":job.get::<String,_>("prices"),"discovered":job.get::<i64,_>("discovered"),"notes":job.get::<String,_>("notes"),"files":files,"records":records})
}
#[tokio::test]
async fn catalog_and_configured_prices_survive_cross_manager_restart_without_upgrading_legacy_arrays(
) {
    use llm_gateway_lib::client_usage::{scan_from_home_with_prices, UsagePriceReference};
    for mode in ["legacy", "current", "legacy-configured"] {
        let f = Fixture::new();
        snapshot_sources(&f);
        let configured = if mode == "legacy-configured" {
            vec![UsagePriceReference {provider:"commandcode".into(),models:vec!["deepseek/deepseek-v4.1-flash".into()],price:serde_json::from_value(json!({"prompt":2,"completion":10,"cache_read":0.1,"cache_creation":2,"currency":"usd"})).unwrap()}]
        } else {
            vec![]
        };
        let (jobs, id) = checkpoint_cancel_priced(&f, configured.clone()).await;
        let before = jobs.status(&id).await.unwrap();
        assert!(before.records_scanned > 0);
        jobs.shutdown().await.unwrap();
        drop(jobs);
        let pool = owned_pool(&f).await;
        if mode != "current" {
            sqlx::query("UPDATE jobs SET prices=? WHERE id=?")
                .bind(serde_json::to_string(&configured).unwrap())
                .bind(&id)
                .execute(&pool)
                .await
                .unwrap();
        }
        let watermark = durable_watermark(&pool, &id).await;
        let restarted = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
        let saved = restarted.current().await.unwrap().unwrap();
        assert_eq!(saved.records_scanned, before.records_scanned);
        assert_eq!(
            durable_watermark(&pool, &id).await,
            watermark,
            "loading is not a migration"
        );
        restarted.resume(&id).await.unwrap();
        let result = terminal(&restarted, &id).await;
        assert_eq!(result.state, "completed", "{mode}: {result:?}");
        let report = result.report.unwrap();
        let openai = report
            .summary_rows
            .iter()
            .find(|r| r.source == "codex")
            .unwrap();
        assert_eq!(openai.input_tokens, Some(8000));
        assert!((openai.estimated_cost.unwrap() - 0.096).abs() < 1e-10);
        let date = if mode == "current" {
            "2026-10-15"
        } else {
            "2026-10-14"
        };
        assert!(openai.price_reference.as_ref().unwrap().contains(date));
        let dsh = report
            .summary_rows
            .iter()
            .find(|r| r.source == "dsh")
            .unwrap();
        assert_eq!(dsh.input_tokens, Some(60));
        match mode {
            "legacy" => {
                assert_eq!(dsh.estimate_note.as_deref(), Some("price_missing"));
                assert!(dsh.estimated_cost_min.is_none());
            }
            "current" => {
                assert!((dsh.estimated_cost_min.unwrap() - 0.00003006).abs() < 1e-12);
                assert!((dsh.estimated_cost_max.unwrap() - 0.00006012).abs() < 1e-12);
            }
            _ => {
                assert!((dsh.estimated_cost.unwrap() - 0.000482).abs() < 1e-12);
                assert!(dsh.price_reference.as_ref().unwrap().contains("本机参考价"));
            }
        }
        let quick =
            scan_from_home_with_prices(&f.0, "2026-10-12", "2026-10-14", &configured).unwrap();
        assert!(quick
            .summary_rows
            .iter()
            .find(|r| r.source == "codex")
            .unwrap()
            .price_reference
            .as_ref()
            .unwrap()
            .contains("2026-10-15"));
        if mode == "current" {
            assert_eq!(report.daily_rows, quick.daily_rows);
        } else if mode == "legacy" {
            assert!(quick
                .summary_rows
                .iter()
                .find(|r| r.source == "dsh")
                .unwrap()
                .estimated_cost_min
                .is_some());
        }
        let after = durable_watermark(&pool, &id).await;
        assert_eq!(
            after["prices"], watermark["prices"],
            "resume must never rewrite arrays"
        );
        assert_eq!(report.sources[0].records_scanned, 8002);
        pool.close().await;
        restarted.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn unknown_or_corrupt_snapshot_fails_before_source_discovery_and_retains_checkpoint_bytes() {
    let f = Fixture::new();
    many(&f);
    let (jobs, id) = checkpoint_cancel(&f).await;
    let original = jobs.status(&id).await.unwrap();
    let pool = owned_pool(&f).await;
    for raw in [
        r#"{"schema_version":9,"catalog":"2026-10-15","configured":[]}"#,
        r#"{"schema_version":1,"catalog":"future","configured":[]}"#,
        r#"{"schema_version":1,"catalog":"2026-10-15","configured":[],"unknown":1}"#,
        r#"{"schema_version":1,"catalog":"2026-10-15","configured":[],"catalog":"2026-10-14"}"#,
        r#"{"schema_version":1,"catalog":"2026-10-15","configured":null}"#,
        "broken",
    ] {
        sqlx::query("UPDATE jobs SET prices=?,view=? WHERE id=?")
            .bind(raw)
            .bind(serde_json::to_string(&original).unwrap())
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        let before = durable_watermark(&pool, &id).await;
        // A new source must not be discovered or added when the metadata is invalid.
        fs::write(
            f.0.join(".codex/sessions/new.jsonl"),
            Fixture::record("new", 9000),
        )
        .unwrap();
        jobs.resume(&id).await.unwrap();
        let failed = terminal(&jobs, &id).await;
        assert_eq!(failed.state, "failed", "{raw}");
        assert!(failed.report.is_none());
        assert_eq!(
            (
                failed.records_scanned,
                failed.snapshot_bytes_read,
                failed.files_done
            ),
            (
                original.records_scanned,
                original.snapshot_bytes_read,
                original.files_done
            )
        );
        assert_eq!(
            durable_watermark(&pool, &id).await,
            before,
            "invalid metadata must not mutate {raw}"
        );
    }
    // The same rejection also precedes first-time discovery with no snapshots at all.
    sqlx::query("UPDATE jobs SET discovered=0,prices=?,view=? WHERE id=?")
        .bind(r#"{"schema_version":1,"catalog":"future","configured":[]}"#)
        .bind(serde_json::to_string(&original).unwrap())
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let before = durable_watermark(&pool, &id).await;
    jobs.resume(&id).await.unwrap();
    assert_eq!(terminal(&jobs, &id).await.state, "failed");
    assert_eq!(durable_watermark(&pool, &id).await, before);
    pool.close().await;
    jobs.shutdown().await.unwrap();
}
#[tokio::test]
async fn completed_historical_reports_remain_readable_without_sources_or_price_decoding() {
    let f = Fixture::new();
    fs::write(
        f.0.join(".codex/sessions/a.jsonl"),
        Fixture::record("one", 10),
    )
    .unwrap();
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
    let expected = serde_json::to_value(&completed).unwrap();
    fs::remove_file(f.0.join(".codex/sessions/a.jsonl")).unwrap();
    let pool = owned_pool(&f).await;
    for raw in [
        "[]",
        "not-json",
        r#"{"schema_version":99,"catalog":"future","configured":[]}"#,
    ] {
        sqlx::query("UPDATE jobs SET prices=? WHERE id=?")
            .bind(raw)
            .bind(&started.job_id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(jobs.status(&started.job_id).await.unwrap()).unwrap(),
            expected
        );
        assert_eq!(
            serde_json::to_value(jobs.current().await.unwrap().unwrap()).unwrap(),
            expected
        );
        let stored: String = sqlx::query_scalar("SELECT prices FROM jobs WHERE id=?")
            .bind(&started.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, raw);
    }
    jobs.shutdown().await.unwrap();
    drop(jobs);
    let restarted = UsageJobs::new(f.0.join("owned-jobs")).unwrap();
    assert_eq!(
        serde_json::to_value(restarted.current().await.unwrap().unwrap()).unwrap(),
        expected
    );
    pool.close().await;
    restarted.shutdown().await.unwrap();
}
