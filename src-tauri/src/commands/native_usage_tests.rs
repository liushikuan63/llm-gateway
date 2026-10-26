use super::stats_overview_from_pool;
use crate::db::{repo, Db};
use serde_json::{json, Value};

#[test]
fn latency_percentile_uses_nearest_observed_rank_and_empty_is_unknown() {
    assert_eq!(super::latency_percentile(&[], 1, 2), None);
    assert_eq!(super::latency_percentile(&[10, 20, 30, 40], 1, 2), Some(20));
    assert_eq!(
        super::latency_percentile(&[10, 20, 30, 40], 19, 20),
        Some(40)
    );
    assert_eq!(super::latency_percentile(&[10, 20, 30, 40], 0, 2), None);
}

async fn seed(db: &Db, ts: i64, provider: Option<&str>, model: Option<&str>, raw: Option<&str>) {
    repo::log_request_at(
        db.pool(),
        ts,
        repo::RequestLog {
            session_id: None,
            client: None,
            requested_model: "auto",
            routed_provider: provider,
            routed_model: model,
            status: Some(200),
            latency_ms: 100,
            prompt_tokens: 999,
            completion_tokens: 888,
            fallback_attempts: 0,
            error: None,
            cost: None,
            currency: None,
            rate_label: None,
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

fn marker(report: Value, snapshot: Value) -> String {
    json!([{"native_exact":{"version":1,"usage_evidence":report,
        "configured_price_snapshot":snapshot}}])
    .to_string()
}

#[tokio::test]
async fn ipc_latency_window_excludes_missing_negative_and_old_but_keeps_zero() {
    let db = Db::connect_in_memory().await.unwrap();
    for (ts, latency) in [
        (99, Some(9999_i64)),
        (100, Some(40)),
        (101, None),
        (102, Some(-100)),
        (103, Some(0)),
        (104, Some(20)),
        (105, Some(10)),
        (106, Some(30)),
    ] {
        seed(&db, ts, None, None, None).await;
        sqlx::query("UPDATE requests SET latency_ms=? WHERE ts=?")
            .bind(latency)
            .bind(ts)
            .execute(db.pool())
            .await
            .unwrap();
    }
    let stats = stats_overview_from_pool(db.pool(), 100).await.unwrap();
    assert_eq!(stats.latency_sample_count, 5);
    assert_eq!(stats.latency_p50_ms, Some(20));
    assert_eq!(stats.latency_p95_ms, Some(40));
    // The compatibility average still includes NULL as zero and the legacy negative row.
    assert_eq!(stats.avg_latency_ms, 0);

    let absent = stats_overview_from_pool(db.pool(), 107).await.unwrap();
    assert_eq!(absent.latency_sample_count, 0);
    assert_eq!(absent.latency_p50_ms, None);
    assert_eq!(absent.latency_p95_ms, None);
}

#[tokio::test]
async fn ipc_legacy_numbers_shape_alltime_count_and_distinct_status_rules_stay_intact() {
    let db = Db::connect_in_memory().await.unwrap();
    seed(&db, 99, Some("old"), None, None).await;
    seed(&db, 100, Some("a"), None, None).await;
    seed(&db, 101, None, None, None).await;
    sqlx::query("UPDATE requests SET status=NULL,latency_ms=NULL,fallback_attempts=2 WHERE ts=101")
        .execute(db.pool())
        .await
        .unwrap();
    let stats =
        serde_json::to_value(stats_overview_from_pool(db.pool(), 100).await.unwrap()).unwrap();
    assert_eq!(stats["total_requests"], 3);
    assert_eq!(stats["today_requests"], 2);
    assert_eq!(stats["total_prompt_tokens"], 1998);
    assert_eq!(stats["total_completion_tokens"], 1776);
    assert_eq!(stats["success_rate"], 1.0); // IPC intentionally counts NULL status as success.
    assert_eq!(stats["avg_latency_ms"], 50); // IPC intentionally COALESCEs NULL latency.
    assert_eq!(stats["latency_sample_count"], 1);
    assert_eq!(stats["latency_p50_ms"], 100);
    assert_eq!(stats["latency_p95_ms"], 100);
    assert_eq!(stats["total_fallbacks"], 2);
    assert_eq!(stats["fallback_request_count"], 1);
    assert_eq!(stats["fallback_rate"], 0.5);
    assert!(stats.get("native_usage").is_none());
    assert!(stats.get("native_usage_by_model").is_none());
    for provider in stats["provider_distribution"].as_array().unwrap() {
        assert!(provider.get("native_usage").is_none());
        assert_eq!(provider["prompt_tokens"], 999);
        assert_eq!(provider["completion_tokens"], 888);
    }
}

#[tokio::test]
async fn ipc_native_coverage_provider_model_groups_and_inclusive_cutoff_use_same_projector() {
    let db = Db::connect_in_memory().await.unwrap();
    seed(
        &db,
        99,
        Some("old"),
        Some("old"),
        Some(&marker(
            json!({"state":"partial","prompt_tokens":9999}),
            Value::Null,
        )),
    )
    .await;
    seed(&db, 100, Some("a"), Some("model"), None).await;
    seed(
        &db,
        100,
        Some("a"),
        Some("model"),
        Some(&marker(
            json!({"state":"partial","prompt_tokens":7}),
            Value::Null,
        )),
    )
    .await;
    seed(
        &db,
        101,
        Some("b"),
        None,
        Some(&marker(json!({"state":"missing"}), Value::Null)),
    )
    .await;
    seed(&db, 102, Some("b"), None, Some("invalid")).await;
    seed(
        &db,
        102,
        None,
        None,
        Some(&marker(
            json!({"state":"complete","prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
            Value::Null,
        )),
    )
    .await;
    let stats =
        serde_json::to_value(stats_overview_from_pool(db.pool(), 100).await.unwrap()).unwrap();
    assert_eq!(stats["total_requests"], 6);
    assert_eq!(stats["today_requests"], 5);
    assert_eq!(stats["total_prompt_tokens"], 1006);
    assert_eq!(stats["total_completion_tokens"], 888);
    assert_eq!(stats["native_usage"]["native_requests"], 4);
    assert_eq!(stats["native_usage"]["legacy_requests"], 1);
    assert_eq!(stats["native_usage"]["prompt_tokens"]["known_requests"], 2);
    assert_eq!(
        stats["native_usage"]["prompt_tokens"]["unknown_requests"],
        2
    );
    let providers = stats["provider_distribution"].as_array().unwrap();
    assert_eq!(providers.len(), 3);
    let a = providers.iter().find(|p| p["provider_id"] == "a").unwrap();
    assert_eq!(a["prompt_tokens"], 1006);
    assert_eq!(a["completion_tokens"], 888);
    assert_eq!(a["native_usage"]["native_requests"], 1);
    assert_eq!(a["native_usage"]["legacy_requests"], 1);
    let b = providers.iter().find(|p| p["provider_id"] == "b").unwrap();
    assert_eq!(b["prompt_tokens"], 0);
    assert_eq!(b["native_usage"]["invalid_requests"], 1);
    assert_eq!(b["native_usage"]["missing_requests"], 1);
    let groups = stats["native_usage_by_model"].as_array().unwrap();
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0]["provider_id"], Value::Null);
    assert!(groups[0]["model"].is_null());
    assert_eq!(groups[1]["provider"], "a");
    assert_eq!(groups[1]["model"], "model");
    assert_eq!(groups[2]["provider"], "b");
}

#[tokio::test]
async fn ipc_reference_fees_use_immutable_snapshot_utc_tiers_and_ignore_current_disabled_price() {
    let db = Db::connect_in_memory().await.unwrap();
    sqlx::query("INSERT INTO providers(id,name,base_url,api_key_enc,created_at,updated_at) VALUES('a','Display A','http://127.0.0.1:1','','2026','2026')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO models(id,provider_id,alias,upstream,price_json) VALUES('m','a','model','model','{\"prompt\":999999,\"completion\":999999,\"currency\":\"usd\"}')")
        .execute(db.pool()).await.unwrap();
    let snapshot = json!({"prompt":2,"completion":8,"currency":"cny",
        "tiers":[{"min_prompt_tokens":1000,"prompt":4,"completion":10}],
        "rules":[{"label":"night","start_minute":1380,"end_minute":60,"prompt_multiplier":0.5,"completion_multiplier":0.25}]});
    for ts in [3599, 3600] {
        seed(
            &db,
            ts,
            Some("a"),
            Some("model"),
            Some(&marker(
                json!({"state":"complete","prompt_tokens":1000,
            "completion_tokens":1000,"total_tokens":2000}),
                snapshot.clone(),
            )),
        )
        .await;
    }
    let before =
        serde_json::to_value(stats_overview_from_pool(db.pool(), 0).await.unwrap()).unwrap();
    let fee = &before["native_usage"]["reference_fees"][0];
    assert_eq!(fee["currency"], "cny");
    assert!((fee["min"].as_f64().unwrap() - 0.0185).abs() < 1e-12);
    assert_eq!(fee["min"], fee["max"]);
    assert_eq!(fee["priced_requests"], 2);
    assert_eq!(before["provider_distribution"][0]["provider"], "Display A");
    sqlx::query("UPDATE models SET enabled=0,price_json='{\"prompt\":1,\"completion\":1,\"currency\":\"usd\"}'")
        .execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE providers SET enabled=0")
        .execute(db.pool())
        .await
        .unwrap();
    let after =
        serde_json::to_value(stats_overview_from_pool(db.pool(), 0).await.unwrap()).unwrap();
    assert_eq!(
        before, after,
        "current configuration must never reprice history"
    );
    let rows = repo::query_requests_for_export(db.pool(), &Default::default())
        .await
        .unwrap();
    let day = rows[0].to_json();
    let night = rows[1].to_json();
    assert!(
        (day["native_usage"]["reference_fee"]["min"]
            .as_f64()
            .unwrap()
            - 0.014)
            .abs()
            < 1e-12
    );
    assert!(
        (night["native_usage"]["reference_fee"]["min"]
            .as_f64()
            .unwrap()
            - 0.0045)
            .abs()
            < 1e-12
    );
    assert!(day["cost"].is_null());
}

#[tokio::test]
async fn s2_timing_ipc_extension_uses_shared_projector_and_keeps_legacy_scalars() {
    let db = Db::connect_in_memory().await.unwrap();
    let raw=json!([{"provider_id":"own","model":"own","ok":true,"timing":{"schema_version":1,"mode":"streaming","terminal":"completed","response_headers_ms":0,"first_text_ms":0,"first_reasoning_ms":null,"first_refusal_ms":null,"first_tool_ms":null,"finished_ms":2}}]).to_string();
    seed(&db, 100, Some("own"), Some("own"), Some(&raw)).await;
    let v = serde_json::to_value(stats_overview_from_pool(db.pool(), 100).await.unwrap()).unwrap();
    assert_eq!(v["request_timings"]["observed_requests"], 1);
    assert_eq!(
        v["request_timings"]["groups"][0]["metrics"]["first_text"]["p50_ms"],
        0
    );
    assert_eq!(v["avg_latency_ms"], 100);
    assert_eq!(v["total_prompt_tokens"], 999);
}
