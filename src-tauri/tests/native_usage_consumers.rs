//! Actual audit/SQLite/HTTP consumers; all data and listeners belong to this test.
use llm_gateway_lib::audit::{self, RequestFilter};
use llm_gateway_lib::db::{self, repo};
use serde_json::{json, Value};

fn marker(report: Value) -> String {
    json!([{"native_exact":{"version":1,"usage_evidence":report}}]).to_string()
}

#[tokio::test]
async fn personal_unverified_marker_preserves_unknown_in_query_export_and_coverage() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let raw =
        json!([{"personal_test":{"version":1,"usage":"unverified","outcome":"reply_unverified"}}])
            .to_string();
    seed(&db, chrono::Utc::now().timestamp(), Some(&raw)).await;
    let (rows, _) = repo::query_requests(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    let line: Value = serde_json::from_str(audit::to_jsonl(&rows).trim()).unwrap();
    assert!(
        line["prompt_tokens"].is_null()
            && line["completion_tokens"].is_null()
            && line["cost"].is_null()
    );
    assert_eq!(line["native_usage"]["reason"], "personal_test_unverified");
    assert_eq!(audit::csv_cells(&rows[0], 1)[9], "");
    let stats = repo::stats_overview(db.pool()).await.unwrap();
    assert_eq!(
        stats["native_usage"]["prompt_tokens"]["unknown_requests"],
        1
    );
    assert_eq!(
        stats["native_usage"]["completion_tokens"]["unknown_requests"],
        1
    );
    assert_eq!(stats["native_usage"]["missing_requests"], 1);
}

async fn seed(db: &db::Db, ts: i64, raw: Option<&str>) {
    repo::log_request_at(
        db.pool(),
        ts,
        repo::RequestLog {
            session_id: None,
            client: None,
            requested_model: "auto",
            routed_provider: Some("synthetic"),
            routed_model: Some("model-a"),
            status: Some(200),
            latency_ms: 100,
            prompt_tokens: 999,
            completion_tokens: 888,
            fallback_attempts: 0,
            error: None,
            cost: Some(77.0),
            currency: Some("USD"),
            rate_label: Some("conflicting"),
            estimated_prompt_tokens: Some(42),
            attempts_json: raw,
            route: Default::default(),
            access_key_id: None,
            refined_prompt: None,
            trace_id: "ffffffffffffffffffffffffffffffff",
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn recent_uses_authoritative_partial_and_unknown_instead_of_conflicting_scalars() {
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(
        &db,
        chrono::Utc::now().timestamp(),
        Some(&marker(json!({"state":"partial","prompt_tokens":7}))),
    )
    .await;
    let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
    assert_eq!(rows[0]["prompt_tokens"], 7);
    assert!(rows[0]["completion_tokens"].is_null());
    assert!(rows[0]["cost"].is_null());
    assert_eq!(rows[0]["native_usage"]["state"], "partial");
}

#[tokio::test]
async fn query_serde_jsonl_and_csv_preserve_unknown_native_evidence() {
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(&db, 1, Some(&marker(json!({"state":"missing"})))).await;
    let (rows, _) = repo::query_requests(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    let serialized = serde_json::to_value(&rows[0]).unwrap();
    assert!(serialized["prompt_tokens"].is_null());
    assert_eq!(serialized["native_usage"]["state"], "missing");
    assert!(rows[0].to_json()["completion_tokens"].is_null());
    let line: Value = serde_json::from_str(audit::to_jsonl(&rows).trim()).unwrap();
    assert!(line["prompt_tokens"].is_null());
    assert_eq!(audit::csv_cells(&rows[0], 1)[9], "");
}

#[tokio::test]
async fn rolling_stats_use_known_subtotals_with_coverage() {
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(
        &db,
        chrono::Utc::now().timestamp(),
        Some(&marker(json!({"state":"partial","prompt_tokens":7}))),
    )
    .await;
    let stats = repo::stats_overview(db.pool()).await.unwrap();
    assert_eq!(stats["prompt_tokens"], 7);
    assert_eq!(stats["completion_tokens"], 0);
    assert_eq!(
        stats["native_usage"]["completion_tokens"]["unknown_requests"],
        1
    );
}

#[tokio::test]
async fn raw_invalid_never_becomes_legacy_but_sql_null_and_marker_free_are_legacy() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let oversized = " ".repeat(256 * 1024 + 1);
    let invalids = [
        "{broken",
        "{}",
        "null",
        &oversized,
        r#"[{"native_exact":{"version":9}}]"#,
        r#"[{"native_exact":{"version":1,"usage_evidence":{"state":"partial","prompt_tokens":-1}}}]"#,
    ];
    for (i, raw) in invalids.iter().enumerate() {
        seed(&db, i as i64, Some(raw)).await;
    }
    seed(&db, 10, None).await;
    seed(&db, 11, Some("[]")).await;
    seed(&db, 12, Some(r#"[{"ok":true}]"#)).await;
    let rows = repo::query_requests_for_export(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 9);
    for row in &rows {
        let value = serde_json::to_value(row).unwrap();
        let recent = repo::recent_requests(db.pool(), 20)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r["ts"] == row.ts)
            .unwrap();
        if row.ts < 10 {
            for value in [&value, &recent] {
                assert_eq!(value["native_usage"]["state"], "invalid");
                assert!(value["prompt_tokens"].is_null());
                assert!(value["completion_tokens"].is_null());
                assert!(value["cost"].is_null());
                assert!(value["currency"].is_null());
                assert!(value["rate_label"].is_null());
            }
        } else {
            for value in [&value, &recent] {
                assert!(value.get("native_usage").is_none());
                assert_eq!(value["prompt_tokens"], 999);
                assert_eq!(value["completion_tokens"], 888);
                assert_eq!(value["cost"], 77.0);
                assert_eq!(value["currency"], "USD");
            }
        }
    }
}

fn priced_marker(report: Value, snapshot: Value) -> String {
    json!([{"native_exact":{"version":1,"usage_evidence":report,
        "configured_price_snapshot":snapshot,"principal":"private-fixture-never-in-new-field"}}])
    .to_string()
}

fn near(value: &Value, expected: f64) {
    assert!(
        (value.as_f64().unwrap() - expected).abs() < 1e-12,
        "{value} != {expected}"
    );
}

#[tokio::test]
async fn exports_keep_known_zero_credit_and_complete_safe_fee_range() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let raw = priced_marker(
        json!({"state":"complete","prompt_tokens":1_000_000,
        "completion_tokens":250_000,"total_tokens":1_250_000,"credit":3.25}),
        json!({"prompt":2,"completion":8,"cache_read":0.5,"cache_creation":3,"currency":"usd"}),
    );
    seed(&db, 2, Some(&raw)).await;
    seed(
        &db,
        3,
        Some(&marker(json!({"state":"complete","prompt_tokens":0,
        "completion_tokens":0,"total_tokens":0}))),
    )
    .await;
    let rows = repo::query_requests_for_export(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    let zero = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(zero["prompt_tokens"], 0);
    assert_eq!(audit::csv_cells(&rows[0], 1)[9], "0");
    let native_index = audit::csv_header(1)
        .iter()
        .position(|c| c == "native_usage")
        .unwrap();
    let cells = audit::csv_cells(&rows[1], 1);
    let safe: Value = serde_json::from_str(&cells[native_index]).unwrap();
    near(&safe["reference_fee"]["min"], 2.5);
    near(&safe["reference_fee"]["max"], 5.0);
    assert!(safe["reference_fee"]["exact"].is_null());
    assert_eq!(safe["reference_fee"]["currency"], "usd");
    assert_eq!(safe["credit"], 3.25);
    assert!(!safe.to_string().contains("private-fixture"));
    assert!(!safe.to_string().contains("configured_price_snapshot\":"));
    let jsonl = audit::to_jsonl(&rows);
    let parsed: Vec<Value> = jsonl
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(parsed[1]["native_usage"], safe);
    assert_eq!(
        serde_json::to_value(&rows[1]).unwrap()["native_usage"],
        safe
    );
    assert!(audit::to_csv(&rows).contains("native_usage"));
    // Old base columns retain their exact prefix; only the safe JSON column is appended.
    assert_eq!(
        &audit::csv_header(1)[..4],
        ["id", "ts", "session_id", "client"]
    );
    assert_eq!(audit::BASE_COLUMNS.last(), Some(&"native_usage"));
}

#[tokio::test]
async fn filters_pagination_and_export_keep_native_row_scope() {
    let db = db::Db::connect_in_memory().await.unwrap();
    for ts in 1..=5 {
        seed(
            &db,
            ts,
            Some(&marker(json!({"state":"partial","prompt_tokens":ts}))),
        )
        .await;
    }
    sqlx::query("UPDATE requests SET routed_provider='other' WHERE ts=3")
        .execute(db.pool())
        .await
        .unwrap();
    let filter = RequestFilter {
        from_ts: Some(2),
        to_ts: Some(4),
        provider: Some("synthetic".into()),
        model: Some("model-a".into()),
        status: Some("2xx".into()),
        limit: Some(1),
        ..Default::default()
    };
    let (rows, total) = repo::query_requests(db.pool(), &filter).await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ts, 4);
    let exported = repo::query_requests_for_export(db.pool(), &filter)
        .await
        .unwrap();
    assert_eq!(exported.len(), 2);
    assert_eq!(exported[1].ts, 2);
    assert_eq!(exported[0].to_json()["prompt_tokens"], 4);
    let next = RequestFilter {
        offset: Some(1),
        ..filter
    };
    let (rows, _) = repo::query_requests(db.pool(), &next).await.unwrap();
    assert_eq!(rows[0].ts, 2);
}

#[tokio::test]
async fn overview_separates_currency_state_models_providers_and_excludes_old_rows() {
    let db = db::Db::connect_in_memory().await.unwrap();
    let now = chrono::Utc::now().timestamp();
    seed(
        &db,
        now - 86401,
        Some(&marker(json!({"state":"partial","prompt_tokens":9999}))),
    )
    .await;
    seed(&db, now, None).await;
    seed(
        &db,
        now,
        Some(&priced_marker(
            json!({"state":"complete","prompt_tokens":1_000_000,
        "completion_tokens":250_000,"total_tokens":1_250_000}),
            json!({"prompt":2,"completion":8,"cache_read":0.5,"cache_creation":3,"currency":"usd"}),
        )),
    )
    .await;
    seed(
        &db,
        now,
        Some(&priced_marker(
            json!({"state":"complete","prompt_tokens":10,
        "completion_tokens":5,"total_tokens":15,"cache_read_tokens":0,"cache_creation_tokens":0}),
            json!({"prompt":2,"completion":8,"currency":"cny"}),
        )),
    )
    .await;
    sqlx::query("UPDATE requests SET routed_provider=NULL,routed_model=NULL WHERE id=4")
        .execute(db.pool())
        .await
        .unwrap();
    seed(
        &db,
        now,
        Some(&marker(json!({"state":"partial","prompt_tokens":7}))),
    )
    .await;
    seed(&db, now, Some(&marker(json!({"state":"missing"})))).await;
    seed(&db, now, Some("{broken")).await;
    seed(
        &db,
        now,
        Some(&marker(
            json!({"state":"complete","prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
        )),
    )
    .await;
    let stats = repo::stats_overview(db.pool()).await.unwrap();
    assert_eq!(stats["total"], 7);
    assert_eq!(stats["prompt_tokens"], 1_001_016);
    assert_eq!(stats["completion_tokens"], 250_893);
    let native = &stats["native_usage"];
    for (name, count) in [
        ("legacy_requests", 1),
        ("native_requests", 6),
        ("complete_requests", 3),
        ("partial_requests", 1),
        ("missing_requests", 1),
        ("invalid_requests", 1),
        ("unpriced_requests", 4),
    ] {
        assert_eq!(native[name], count, "{name}");
    }
    assert_eq!(native["prompt_tokens"]["known_subtotal"], 1_000_017);
    assert_eq!(native["prompt_tokens"]["known_requests"], 4);
    assert_eq!(native["prompt_tokens"]["unknown_requests"], 2);
    let fees = native["reference_fees"].as_array().unwrap();
    assert_eq!(fees.len(), 2);
    let usd = fees.iter().find(|f| f["currency"] == "usd").unwrap();
    near(&usd["min"], 2.5);
    near(&usd["max"], 5.0);
    assert_eq!(usd["range_requests"], 1);
    let cny = fees.iter().find(|f| f["currency"] == "cny").unwrap();
    near(&cny["min"], 0.00006);
    near(&cny["max"], 0.00006);
    let groups = stats["native_usage_by_model"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0]["provider_id"], Value::Null);
    assert_eq!(groups[0]["provider"], "未路由");
    assert_eq!(groups[0]["model"], Value::Null);
    assert_eq!(groups[0]["usage"]["native_requests"], 1);
    assert_eq!(groups[1]["provider"], "synthetic");
    assert_eq!(groups[1]["model"], "model-a");
    assert_eq!(groups[1]["usage"]["legacy_requests"], 1);
    assert_eq!(groups[1]["usage"]["native_requests"], 5);
}

#[tokio::test]
async fn marker_free_stats_keep_legacy_shape_numbers_and_status_rules() {
    let db = db::Db::connect_in_memory().await.unwrap();
    assert_eq!(
        repo::stats_overview(db.pool()).await.unwrap(),
        json!({"window":"24h","total":0,
        "success":0,"avg_latency_ms":0,"prompt_tokens":0,"completion_tokens":0,"fallback_attempts":0})
    );
    seed(&db, chrono::Utc::now().timestamp(), None).await;
    sqlx::query("UPDATE requests SET status=NULL,latency_ms=NULL,fallback_attempts=2")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        repo::stats_overview(db.pool()).await.unwrap(),
        json!({"window":"24h","total":1,
        "success":0,"avg_latency_ms":0,"prompt_tokens":999,"completion_tokens":888,"fallback_attempts":2})
    );
}

#[tokio::test]
async fn real_loopback_stats_exposes_safe_native_data_and_keeps_authentication() {
    use llm_gateway_lib::config::AppConfig;
    use llm_gateway_lib::proxy::server::{serve, GatewayState};
    use std::{sync::Arc, time::Duration};
    let db = db::Db::connect_in_memory().await.unwrap();
    seed(
        &db,
        chrono::Utc::now().timestamp(),
        Some(&marker(json!({"state":"partial","prompt_tokens":7}))),
    )
    .await;
    let state = Arc::new(GatewayState::new(
        db,
        AppConfig {
            bind: "127.0.0.1".into(),
            port: 0,
            unified_key: "synthetic-owned-test-key".into(),
            ..Default::default()
        },
    ));
    let task = tokio::spawn({
        let state = state.clone();
        async move {
            serve(state).await.unwrap();
        }
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let url = loop {
            assert!(!task.is_finished(), "owned gateway exited before ready");
            if let Some(listener) = state.listener_snapshot() {
                break format!("http://{}/gw/stats", listener.bound_addr);
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(&url)
                .bearer_auth("wrong-key")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        let response = client
            .get(&url)
            .bearer_auth("synthetic-owned-test-key")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let value: Value = response.json().await.unwrap();
        assert_eq!(value["prompt_tokens"], 7);
        assert_eq!(value["native_usage"]["partial_requests"], 1);
        assert_eq!(
            value["native_usage_by_model"][0]["usage"]["prompt_tokens"]["known_subtotal"],
            7
        );
        assert!(value.get("dedup").is_some());
        assert!(value.get("cache").is_some());
        // This local HTTP call has no timing metadata; legacy extension stays absent.
        assert!(value.get("request_timings").is_none());
        let timing = json!([{
            "provider_id":"owned-http", "provider":"Owned HTTP", "model":"hop-model",
            "ok":false,
            "timing":{"schema_version":1,"mode":"streaming","terminal":"completed",
                "response_headers_ms":0,"first_text_ms":0,"first_reasoning_ms":null,
                "first_refusal_ms":null,"first_tool_ms":null,"finished_ms":20}
        }])
        .to_string();
        seed(&state.db, chrono::Utc::now().timestamp(), Some(&timing)).await;
        let response = client
            .get(&url)
            .bearer_auth("synthetic-owned-test-key")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let value: Value = response.json().await.unwrap();
        let phased = &value["request_timings"];
        assert_eq!(phased["schema_version"], 1);
        assert_eq!(phased["total_requests"], 2);
        assert_eq!(phased["observed_requests"], 1);
        assert_eq!(phased["unobserved_requests"], 1);
        assert_eq!(phased["groups"][0]["model"], "hop-model");
        assert_eq!(phased["groups"][0]["outcome"], "attempt_failed");
        assert_eq!(phased["groups"][0]["metrics"]["first_text"]["p50_ms"], 0);
        assert!(phased["groups"][0]["metrics"]["first_reasoning"]["p50_ms"].is_null());
        assert_eq!(value["native_usage"]["partial_requests"], 1);
        assert_eq!(value["prompt_tokens"], 1006);
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
    }
    .await;
    task.abort();
    let _ = task.await;
}
