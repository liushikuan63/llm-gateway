//! Command-level file-effect witnesses for the v2 raw-export boundary.
//! No accounts, application state, service or formal database are used.
use super::{export_requests_v2_from_pool, stats_overview_from_pool, stats_overview_v2_from_pool};
use crate::audit::{
    self,
    v2::{MAX_PAYLOAD_BYTES, MAX_RECORD_BYTES},
    RequestFilter,
};
use crate::db::{
    self,
    repo::{self, audit_v2::RAW_EXPORT_UNAVAILABLE},
};
use std::path::{Path, PathBuf};

struct OwnedOutput(PathBuf);
impl OwnedOutput {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("llmgw-owned-v2-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let path = path.canonicalize().unwrap();
        assert!(path.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        Self(path)
    }
    fn dest(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for OwnedOutput {
    fn drop(&mut self) {
        // Clean only the exact owned child of the OS temp directory. Business
        // config paths and unrelated application state never enter this code.
        let Ok(path) = self.0.canonicalize() else {
            return;
        };
        let Ok(temp) = std::env::temp_dir().canonicalize() else {
            return;
        };
        if path.parent() == Some(temp.as_path())
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("llmgw-owned-v2-export-"))
        {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

async fn seed(db: &db::Db, ts: i64, raw: &str) {
    sqlx::query("INSERT INTO requests(ts,requested_model,routed_provider,routed_model,status,latency_ms,prompt_tokens,completion_tokens,fallback_attempts,cost,currency,rate_label,trace_id,attempts_json) VALUES(?,'auto','fixture','model',200,10,13,5,0,0.2,'USD','fixture','synthetic',?)")
        .bind(ts).bind(raw).execute(db.pool()).await.unwrap();
}
fn columns_path(path: &Path) -> PathBuf {
    let mut columns = path.to_path_buf();
    columns.set_file_name(format!(
        "{}_columns.md",
        path.file_stem().unwrap().to_string_lossy()
    ));
    columns
}
fn fixed_refusal(error: &str) {
    assert_eq!(error, RAW_EXPORT_UNAVAILABLE);
}
fn full_record() -> String {
    let raw = format!("[\"x\"{}]", " ".repeat(MAX_RECORD_BYTES - 5));
    assert_eq!(raw.len(), MAX_RECORD_BYTES);
    raw
}

#[tokio::test]
async fn v2_export_record_refusal_creates_no_main_sidecar_or_parent_directory() {
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(&db, 1, &" ".repeat(MAX_RECORD_BYTES + 1)).await;
    let output = OwnedOutput::new();
    for format in ["jsonl", "csv"] {
        let dest = output.dest(&format!("absent/{format}/report.{format}"));
        let columns = columns_path(&dest);
        let error = export_requests_v2_from_pool(
            db.pool(),
            RequestFilter::default(),
            format.to_owned(),
            dest.to_string_lossy().into_owned(),
        )
        .await
        .unwrap_err();
        fixed_refusal(&error);
        assert!(!dest.exists());
        assert!(!columns.exists());
        assert!(!dest.parent().unwrap().exists());
        assert_eq!(std::fs::read_dir(&output.0).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn v2_export_common_payload_refusal_precedes_all_filesystem_writes() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let raw = full_record();
    let count = MAX_PAYLOAD_BYTES / MAX_RECORD_BYTES + 1;
    for ts in 0..count {
        seed(&db, ts as i64, &raw).await;
    }
    let output = OwnedOutput::new();
    let dest = output.dest("absent/report.csv");
    let error = export_requests_v2_from_pool(
        db.pool(),
        RequestFilter::default(),
        "csv".into(),
        dest.to_string_lossy().into_owned(),
    )
    .await
    .unwrap_err();
    fixed_refusal(&error);
    assert_eq!(std::fs::read_dir(&output.0).unwrap().count(), 0);
    assert!(!dest.exists());
    assert!(!columns_path(&dest).exists());
    assert!(!dest.parent().unwrap().exists());
}

#[tokio::test]
async fn v2_export_refusal_preserves_existing_main_and_csv_sidecar_bytes() {
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(&db, 1, &" ".repeat(MAX_RECORD_BYTES + 1)).await;
    let output = OwnedOutput::new();
    for format in ["jsonl", "csv"] {
        let dest = output.dest(&format!("existing.{format}"));
        let columns = columns_path(&dest);
        let main_before: &[u8] = b"owned original main\0";
        let columns_before: &[u8] = b"owned original columns\n";
        std::fs::write(&dest, main_before).unwrap();
        std::fs::write(&columns, columns_before).unwrap();
        let error = export_requests_v2_from_pool(
            db.pool(),
            RequestFilter::default(),
            format.to_owned(),
            dest.to_string_lossy().into_owned(),
        )
        .await
        .unwrap_err();
        fixed_refusal(&error);
        assert_eq!(std::fs::read(&dest).unwrap().as_slice(), main_before);
        assert_eq!(std::fs::read(&columns).unwrap().as_slice(), columns_before);
    }
    // jsonl/csv share existing_columns.md, so there are exactly three files.
    assert_eq!(std::fs::read_dir(&output.0).unwrap().count(), 3);
}

#[tokio::test]
async fn v2_export_unreadable_bodies_never_touch_existing_files() {
    let mut marked = vec![serde_json::json!({"legacy": true}); 64];
    marked.push(serde_json::json!({
        "native_exact": {"version": 1, "usage_evidence": {"state": "missing"}}
    }));
    let over_native_limit = serde_json::to_string(&marked).unwrap();
    let output = OwnedOutput::new();
    let mut failures = Vec::new();
    for (case, raw) in [
        ("malformed", "{broken"),
        ("not_array", "{}"),
        ("native_limit", &over_native_limit),
    ] {
        let db = db::Db::connect_in_memory().await.unwrap();
        seed(&db, 1, raw).await;
        for format in ["jsonl", "csv"] {
            let dest = output.dest(&format!("{case}.{format}"));
            let columns = columns_path(&dest);
            let main_before = b"owned original main\0";
            let columns_before = b"owned original columns\n";
            std::fs::write(&dest, main_before).unwrap();
            std::fs::write(&columns, columns_before).unwrap();
            let result = export_requests_v2_from_pool(
                db.pool(),
                RequestFilter::default(),
                format.to_owned(),
                dest.to_string_lossy().into_owned(),
            )
            .await;
            if result.err().as_deref() != Some(RAW_EXPORT_UNAVAILABLE)
                || std::fs::read(&dest).unwrap() != main_before
                || std::fs::read(&columns).unwrap() != columns_before
            {
                failures.push((case, format));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "unreadable rows did not preserve files: {failures:?}"
    );
}

#[tokio::test]
async fn v2_overview_exposes_unknown_usage_without_legacy_zero_totals() {
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(&db, 1, "[]").await;
    let legacy = stats_overview_from_pool(db.pool(), 0).await.unwrap();
    assert_eq!(legacy.total_prompt_tokens, 13);
    assert_eq!(legacy.total_completion_tokens, 5);
    let v2 =
        serde_json::to_value(stats_overview_v2_from_pool(db.pool(), 0).await.unwrap()).unwrap();
    assert_eq!(v2["today_requests"], 1);
    assert_eq!(
        v2["usage_stats"]["usage_coverage"]["legacy_unverified_requests"],
        1
    );
    assert!(v2["usage_stats"]["usage_coverage"]["prompt_tokens"]["reliable_total"].is_null());
    assert!(v2["usage_stats"]["usage_coverage"]["completion_tokens"]["reliable_total"].is_null());
    assert!(
        v2.get("total_prompt_tokens").is_none(),
        "legacy prompt total appeared in the v2 overview"
    );
    assert!(v2.get("total_completion_tokens").is_none());
    assert!(v2["provider_distribution"][0]
        .get("prompt_tokens")
        .is_none());
    assert!(v2["provider_distribution"][0]
        .get("completion_tokens")
        .is_none());
}

#[tokio::test]
async fn v2_export_success_preserves_v1_jsonl_csv_and_columns_document() {
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(
        &db,
        1,
        r#"[{"provider":"fixture","model":"model","status":200}]"#,
    )
    .await;
    seed(&db, 2, "[]").await;
    sqlx::query("INSERT INTO requests(ts,requested_model,attempts_json) VALUES(3,'legacy',NULL)")
        .execute(db.pool())
        .await
        .unwrap();
    let filter = RequestFilter::default();
    let expected = repo::query_requests_for_export(db.pool(), &filter)
        .await
        .unwrap();
    let output = OwnedOutput::new();
    for format in ["jsonl", "csv"] {
        let dest = output.dest(&format!("success-{format}/report.{format}"));
        let result = export_requests_v2_from_pool(
            db.pool(),
            filter.clone(),
            format.to_owned(),
            dest.to_string_lossy().into_owned(),
        )
        .await
        .unwrap();
        assert_eq!(result.written, expected.len() as u64);
        assert_eq!(result.path, dest.to_string_lossy().into_owned());
        let content = std::fs::read_to_string(&dest).unwrap();
        if format == "jsonl" {
            assert_eq!(content, audit::to_jsonl(&expected));
            assert!(result.columns_doc.is_none());
            assert!(!columns_path(&dest).exists());
        } else {
            assert_eq!(content, audit::to_csv(&expected));
            let columns = columns_path(&dest);
            let columns_text = columns.to_string_lossy();
            assert_eq!(result.columns_doc.as_deref(), Some(columns_text.as_ref()));
            let max_attempts = expected
                .iter()
                .map(|row| row.attempts.len())
                .max()
                .unwrap_or(0);
            assert_eq!(
                std::fs::read_to_string(columns).unwrap(),
                audit::columns_doc(max_attempts)
            );
        }
    }
}
