use super::*;
use crate::db::Db;
use serde_json::{json, Value};

async fn seed(db: &Db, ts: i64, raw: Option<&str>, estimate: Option<i64>) {
    sqlx::query("INSERT INTO requests(ts,requested_model,status,latency_ms,prompt_tokens,completion_tokens,fallback_attempts,attempts_json,estimated_prompt_tokens,routed_model) VALUES (?,'owned',200,99999,123,456,0,?,?,'ROW-WINNER')")
        .bind(ts).bind(raw).bind(estimate).execute(db.pool()).await.unwrap();
}
fn observed(model: &str, mode: &str, terminal: &str, first: Option<u64>, end: u64) -> Value {
    json!({"provider_id":model,"provider":model,"model":model,"ok":true,"timing":{"schema_version":1,"mode":mode,"terminal":terminal,"response_headers_ms":0,"first_text_ms":first,"first_reasoning_ms":null,"first_refusal_ms":null,"first_tool_ms":null,"finished_ms":end}})
}

fn report(count: u32, ns: &str) -> Value {
    json!({"schema_version":1,"source":"ollama_native_eval","state":"reported","reason":null,
        "timing_mode":"streaming","output_tokens":count,"eval_duration_ns":ns,
        "total_duration_ns":null,"load_duration_ns":null,"prompt_eval_duration_ns":null,
        "rate_tokens_per_second":f64::from(count)*1e9/ns.parse::<u64>().unwrap() as f64})
}
fn attempt(model: &str, generation: Value) -> Value {
    let mut a = observed(model, "streaming", "completed", Some(1), 10);
    a["generation"] = generation;
    a
}
#[tokio::test]
async fn generation_window_denominators_zero_unknown_and_actual_hop_grouping() {
    let db = Db::connect_in_memory().await.unwrap();
    seed(
        &db,
        99,
        Some(&json!([attempt("old", report(900, "1"))]).to_string()),
        None,
    )
    .await;
    seed(
        &db,
        100,
        Some(
            &json!([
                attempt("a", report(18, "2000000000")),
                attempt("b", report(0, "1000000000"))
            ])
            .to_string(),
        ),
        Some(100),
    )
    .await;
    seed(&db, 101, Some("[]"), None).await;
    seed(&db, 102, None, None).await;
    seed(&db, 103, Some("not-json"), None).await;
    let mut unknown = report(2, "1000000000");
    unknown["state"] = json!("unknown");
    unknown["reason"] = json!("eval_duration_missing");
    unknown["eval_duration_ns"] = Value::Null;
    unknown["rate_tokens_per_second"] = Value::Null;
    seed(
        &db,
        104,
        Some(
            &json!([
                observed("legacy", "buffered", "completed", None, 100),
                attempt("unknown", unknown),
                3
            ])
            .to_string(),
        ),
        None,
    )
    .await;
    seed(
        &db,
        86501,
        Some(&json!([attempt("future", report(99, "1"))]).to_string()),
        None,
    )
    .await;
    let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
    let g = &v["generation_reports"];
    assert_eq!(
        g["parsed_attempts"], 5,
        "missing summary must fail, not silently default"
    );
    assert_eq!(g["usable_attempts"], 2);
    assert_eq!(g["usable_requests"], 1);
    assert_eq!(g["absent_attempts"], 1);
    assert_eq!(g["unknown_attempts"], 1);
    assert_eq!(g["invalid_attempts"], 1);
    assert_eq!(g["inspected_requests"], 2);
    assert_eq!(g["unavailable_requests"], 1);
    assert_eq!(g["no_details_requests"], 1);
    assert_eq!(g["invalid_records"], 1);
    assert_eq!(v["total_requests"], 5);
    assert_eq!(g["groups"].as_array().unwrap().len(), 2);
    let a = g["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["model"] == "a")
        .unwrap();
    assert_eq!(a["rate"]["p50_tokens_per_second"], 9.0);
    let b = g["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["model"] == "b")
        .unwrap();
    assert_eq!(b["rate"]["p50_tokens_per_second"], 0.0);
    assert!(!g.to_string().contains("ROW-WINNER"));
}

#[tokio::test]
async fn generation_corrupt_or_failed_reports_never_donate_a_sample() {
    let db = Db::connect_in_memory().await.unwrap();
    let good = attempt("owned", report(18, "2000000000"));
    let mut invalid = Vec::new();
    for (field, value) in [
        ("schema_version", json!(2)),
        ("source", json!("untrusted")),
        ("output_tokens", json!(-1)),
        ("eval_duration_ns", json!("0")),
        ("eval_duration_ns", json!("02000000000")),
        ("total_duration_ns", json!("1")),
        ("rate_tokens_per_second", json!(99)),
        ("load_duration_ns", json!("18446744073709551616")),
    ] {
        let mut a = good.clone();
        a["generation"][field] = value;
        invalid.push(a);
    }
    let mut extra = good.clone();
    extra["generation"]["private"] = json!("must-not-echo");
    invalid.push(extra);
    let mut no_timing = good.clone();
    no_timing["timing"] = Value::Null;
    invalid.push(no_timing);
    let mut native = good.clone();
    native["native_exact"] = json!({});
    invalid.push(native);
    let mut unknown = vec![];
    for (field, value) in [
        ("ok", json!(false)),
        ("mode", json!("buffered")),
        ("terminal", json!("cancelled")),
    ] {
        let mut a = good.clone();
        if field == "ok" {
            a[field] = value;
        } else {
            a["timing"][field] = value;
            if field == "mode" {
                a["timing"]["first_text_ms"] = Value::Null;
            }
        }
        unknown.push(a);
    }
    let invalid_count = invalid.len();
    invalid.extend(unknown);
    seed(&db, 100, Some(&Value::Array(invalid).to_string()), None).await;
    seed(&db, 101, Some(&" ".repeat(65537)), None).await;
    seed(
        &db,
        102,
        Some(&json!(vec![json!({}); 65]).to_string()),
        None,
    )
    .await;
    let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
    let g = &v["generation_reports"];
    assert_eq!(g["invalid_attempts"], invalid_count);
    assert_eq!(g["unknown_attempts"], 3);
    assert_eq!(g["usable_attempts"], 0);
    assert_eq!(g["invalid_records"], 2);
    assert_eq!(g["parsed_attempts"], invalid_count + 3);
    assert_eq!(g["groups"], json!([]));
    assert!(!g.to_string().contains("must-not-echo"));
}

#[tokio::test]
async fn generation_resource_limits_preserve_counts_and_hide_partial_percentiles() {
    let db = Db::connect_in_memory().await.unwrap();
    for (ts, model, count) in [(100, "a", 1), (101, "a", 2), (102, "b", 3)] {
        seed(
            &db,
            ts,
            Some(&json!([attempt(model, report(count, "1000000000"))]).to_string()),
            None,
        )
        .await;
    }
    let v = serde_json::to_value(
        rolling_with_limits(
            db.pool(),
            100,
            Limits {
                max_groups: 1,
                max_samples: 1,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let g = &v["generation_reports"];
    assert_eq!(g["usable_attempts"], 3);
    assert_eq!(g["usable_requests"], 3);
    assert_eq!(g["ungrouped_attempts"], 1);
    assert_eq!(g["group_limit_reached"], true);
    assert_eq!(g["sample_limit_reached"], true);
    assert_eq!(
        g["groups"][0]["rate"],
        json!({"samples":2,"retained_samples":1,"complete":false,"p50_tokens_per_second":null,"p95_tokens_per_second":null})
    );
}

#[tokio::test]
async fn generation_exact_report_precision_and_observed_controls_stay_separate() {
    let db = Db::connect_in_memory().await.unwrap();
    let controls = json!({"reasoning":"disabled","tools":"empty","tool_choice":"none"});
    let wire = json!({"schema_version":1,"dialect":"ollama","declared_thinking":false,"caller":{"dialect":"openai","modes":controls},"normalized_ingress":controls,"effective_request":controls,"wire":controls});
    let mut first = attempt("same-model", report(1, "18446744073709551615"));
    first["wire_modes"] = wire.clone();
    let mut second = first.clone();
    second["wire_modes"]["wire"]["reasoning"] = json!("enabled");
    let mut buffered = first.clone();
    buffered["timing"]["mode"] = json!("buffered");
    buffered["timing"]["first_text_ms"] = Value::Null;
    buffered["generation"]["timing_mode"] = json!("buffered");
    let mut other_dialect = first.clone();
    other_dialect["wire_modes"]["dialect"] = json!("openai");
    seed(
        &db,
        100,
        Some(&json!([first, second, buffered, other_dialect]).to_string()),
        Some(1),
    )
    .await;
    let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
    let g = &v["generation_reports"];
    assert_eq!(g["usable_attempts"], 3);
    assert_eq!(g["invalid_attempts"], 1);
    assert_eq!(
        g["groups"].as_array().unwrap().len(),
        3,
        "mode and actual wire controls must not merge"
    );
    for group in g["groups"].as_array().unwrap() {
        assert_eq!(group["wire_modes_state"], "observed");
        let rate = group["rate"]["p50_tokens_per_second"].as_f64().unwrap();
        assert!(rate > 0.0 && rate < 0.001, "tiny report is not zero");
    }
}

#[test]
fn generation_readback_only_normalizes_valid_derived_report_not_other_audit_fields() {
    let tiny = attempt("owned", report(1, "18446744073709551615"));
    let mut decoded: Value = serde_json::from_str(&json!([tiny]).to_string()).unwrap();
    normalize_generation_reports(decoded.as_array_mut().unwrap());
    assert_eq!(
        decoded[0]["generation"]["rate_tokens_per_second"]
            .as_f64()
            .unwrap()
            .to_bits(),
        (1e9 / u64::MAX as f64).to_bits()
    );
    assert_eq!(
        decoded[0]["generation"]["eval_duration_ns"],
        "18446744073709551615"
    );
    let mut corrupt = attempt("bad", report(1, "18446744073709551615"));
    corrupt["generation"]["rate_tokens_per_second"] = json!(99);
    let mut unknown = attempt("unknown", report(1, "18446744073709551615"));
    unknown["generation"]["state"] = json!("unknown");
    unknown["generation"]["reason"] = json!("multiple_final_reports");
    unknown["generation"]["rate_tokens_per_second"] = Value::Null;
    let mut private = attempt("private", report(1, "18446744073709551615"));
    private["generation"]["private"] = json!("owned-not-reflected");
    let mut unchanged = vec![
        corrupt,
        unknown,
        private,
        attempt("zero", report(0, "2000000000")),
        observed("legacy", "streaming", "completed", Some(0), 1),
        json!(3),
    ];
    let before = unchanged.clone();
    normalize_generation_reports(&mut unchanged);
    assert_eq!(
        unchanged, before,
        "no rewriting unknown, invalid, legacy, zero or unrelated audit fields"
    );
}

#[tokio::test]
async fn generation_file_database_reopen_preserves_reports_modes_and_unknown_versions() {
    use crate::audit::RequestFilter;
    use crate::db::repo;
    let root = std::env::var_os("LLMGW_OWNED_S4_PERSISTENCE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("s4-persistence-owned-{}", uuid::Uuid::new_v4()))
        });
    assert!(
        root.is_absolute()
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("s4-persistence-owned-")
    );
    std::fs::create_dir(&root).unwrap(); // unique owned fixture; never reuse a user DB
    let path = root.join("generation-audit.sqlite3");
    let db = Db::connect_path(&path).await.unwrap();
    let first = attempt("physical-a", report(1, "18446744073709551615"));
    let mut buffered = attempt("physical-a", report(18, "4000000000"));
    buffered["timing"]["mode"] = json!("buffered");
    buffered["timing"]["first_text_ms"] = Value::Null;
    buffered["generation"]["timing_mode"] = json!("buffered");
    let mut future_report = first.clone();
    future_report["generation"]["schema_version"] = json!(99);
    let mut future_timing = first.clone();
    future_timing["timing"]["schema_version"] = json!(99);
    let mut unknown = first.clone();
    unknown["generation"]["state"] = json!("unknown");
    unknown["generation"]["reason"] = json!("eval_duration_missing");
    unknown["generation"]["eval_duration_ns"] = Value::Null;
    unknown["generation"]["rate_tokens_per_second"] = Value::Null;
    let legacy = observed("legacy", "streaming", "completed", Some(1), 10);
    for (offset, item) in [
        first.clone(),
        buffered,
        future_report,
        future_timing,
        unknown,
        legacy,
    ]
    .into_iter()
    .enumerate()
    {
        seed(
            &db,
            100 + offset as i64,
            Some(&json!([item]).to_string()),
            Some(100),
        )
        .await;
    }
    seed(&db, 106, Some("[]"), None).await;
    seed(&db, 107, None, None).await;
    seed(&db, 108, Some("owned-not-json"), None).await;
    for ts in [99, 86501] {
        seed(&db, ts, Some(&json!([first.clone()]).to_string()), None).await;
    }
    let filter = RequestFilter {
        from_ts: Some(100),
        to_ts: Some(86500),
        ..Default::default()
    };
    let raw_before: Vec<(i64, Option<String>)> =
        sqlx::query_as("SELECT id,attempts_json FROM requests ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    let before = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
    let query_before =
        serde_json::to_value(repo::query_requests(db.pool(), &filter).await.unwrap()).unwrap();
    let export_before = serde_json::to_value(
        repo::query_requests_for_export(db.pool(), &filter)
            .await
            .unwrap(),
    )
    .unwrap();
    db.pool().close().await;
    assert!(
        db.pool().is_closed(),
        "release every production pool connection before reopen"
    );
    drop(db);
    assert!(path.is_file());
    let reopened = Db::connect_path(&path).await.unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(reopened.pool())
        .await
        .unwrap();
    let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(reopened.pool())
        .await
        .unwrap();
    let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(reopened.pool())
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(fk, 1);
    assert_eq!(integrity, "ok");
    let raw_after: Vec<(i64, Option<String>)> =
        sqlx::query_as("SELECT id,attempts_json FROM requests ORDER BY id")
            .fetch_all(reopened.pool())
            .await
            .unwrap();
    let after = serde_json::to_value(rolling(reopened.pool(), 100).await.unwrap()).unwrap();
    assert_eq!(
        raw_after, raw_before,
        "reentrant initialization and read projection do not rewrite saved audit"
    );
    assert_eq!(
        after, before,
        "complete coverage, groups and percentiles survive closed pool and reopen"
    );
    assert_eq!(
        serde_json::to_value(
            repo::query_requests(reopened.pool(), &filter)
                .await
                .unwrap()
        )
        .unwrap(),
        query_before
    );
    assert_eq!(
        serde_json::to_value(
            repo::query_requests_for_export(reopened.pool(), &filter)
                .await
                .unwrap()
        )
        .unwrap(),
        export_before
    );
    assert_eq!(after["total_requests"], 9);
    let g = &after["generation_reports"];
    assert_eq!(g["parsed_attempts"], 6);
    assert_eq!(g["usable_attempts"], 2);
    assert_eq!(g["usable_requests"], 2);
    assert_eq!(g["invalid_attempts"], 2);
    assert_eq!(g["unknown_attempts"], 1);
    assert_eq!(g["absent_attempts"], 1);
    assert_eq!(g["inspected_requests"], 6);
    assert_eq!(g["unavailable_requests"], 1);
    assert_eq!(g["no_details_requests"], 1);
    assert_eq!(g["invalid_records"], 1);
    assert_eq!(g["groups"].as_array().unwrap().len(), 2);
    let streaming = g["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["mode"] == "streaming")
        .unwrap();
    let buffered = g["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["mode"] == "buffered")
        .unwrap();
    assert_eq!(streaming["rate"]["samples"], 1);
    assert!(streaming["rate"]["p50_tokens_per_second"].as_f64().unwrap() > 0.0);
    assert_eq!(buffered["rate"]["samples"], 1);
    assert_eq!(buffered["rate"]["p50_tokens_per_second"], 4.5);
    assert!(!g.to_string().contains("ROW-WINNER"));
    reopened.pool().close().await;
    assert!(reopened.pool().is_closed());
    eprintln!(
        "S4_OWNED_FILE_REOPEN {}",
        json!({"database":path,"stored_rows":raw_after.len(),"window_requests":9,"known_reports":2,"unknown_or_invalid_reports":3,"absent_reports":1,"journal_mode":mode,"foreign_keys":fk,"quick_check":integrity,"pool_closed":true,"model_weights_version_verified":false})
    );
}

#[tokio::test]
async fn generation_observation_context_changes_do_not_merge_same_model_samples() {
    let db = Db::connect_in_memory().await.unwrap();
    let a = attempt("same-model", report(18, "2000000000"));
    let mut first = a.clone();
    let mut second = a.clone();
    first["observation_context"] = json!({"schema_version":1,"scope":"ollama_wire_projection_v1","provider_id":"same-model","physical_model":"same-model","fingerprint_sha256":"a".repeat(64),"model_revision":null});
    second["observation_context"] = first["observation_context"].clone();
    second["observation_context"]["fingerprint_sha256"] = json!("b".repeat(64));
    seed(&db, 100, Some(&json!([first, second]).to_string()), Some(1)).await;
    let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
    assert_eq!(
        v["generation_reports"]["groups"].as_array().unwrap().len(),
        2,
        "different captured send projections must not silently merge"
    );
    assert_eq!(v["groups"].as_array().unwrap().len(), 2);
    assert_eq!(v["generation_reports"]["usable_attempts"], 2);
    for group in v["generation_reports"]["groups"].as_array().unwrap() {
        assert_eq!(group["observation_context_state"], "observed");
        assert!(group["observation_context"]["model_revision"].is_null());
        assert_eq!(group["rate"]["samples"], 1);
    }
}

#[tokio::test]
async fn generation_unknown_or_invalid_context_does_not_drop_honest_numeric_reports() {
    let db = Db::connect_in_memory().await.unwrap();
    let legacy = attempt("owned", report(18, "2000000000"));
    let context = json!({"schema_version":1,"scope":"ollama_wire_projection_v1","provider_id":"owned","physical_model":"physical","fingerprint_sha256":"a".repeat(64),"model_revision":null});
    let mut attempts = vec![legacy.clone()];
    for (k, v) in [
        ("provider_id", json!("other-owner")),
        ("schema_version", json!(99)),
        ("scope", json!("owned-private-invalid-marker")),
    ] {
        let mut a = legacy.clone();
        a["observation_context"] = context.clone();
        a["observation_context"][k] = v;
        attempts.push(a);
    }
    seed(&db, 100, Some(&Value::Array(attempts).to_string()), None).await;
    let v = serde_json::to_value(rolling(db.pool(), 100).await.unwrap()).unwrap();
    let g = &v["generation_reports"];
    assert_eq!(g["usable_attempts"], 4);
    assert_eq!(g["groups"].as_array().unwrap().len(), 2);
    let unknown = g["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["observation_context_state"] == "unknown")
        .unwrap();
    let invalid = g["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["observation_context_state"] == "invalid")
        .unwrap();
    assert_eq!(unknown["rate"]["samples"], 1);
    assert_eq!(invalid["rate"]["samples"], 3);
    assert!(unknown["observation_context"].is_null() && invalid["observation_context"].is_null());
    assert!(!g.to_string().contains("owned-private-invalid-marker"));
}
