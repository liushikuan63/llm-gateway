//! Observation negatives use real owned HTTP and the persisted production audit.
use super::*;
use futures_util::FutureExt;
use serde_json::{json, Value};

async fn stalled(stage: u8) -> (String, MockState, JoinHandle<()>) {
    stalled_with_status(stage, StatusCode::OK).await
}

async fn stalled_with_status(stage: u8, status: StatusCode) -> (String, MockState, JoinHandle<()>) {
    let state = MockState::default();
    let captured = state.clone();
    let app = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
        let state = captured.clone();
        async move {
            state.hits.fetch_add(1, Ordering::SeqCst);
            let streaming = body["stream"] == true;
            state.requests.lock().unwrap().push(body);
            if stage == 0 { tokio::time::sleep(Duration::from_secs(3)).await; }
            let prefix = match stage {
                2 if streaming => ": owned ping\n\n".to_string(),
                2 => "{\"id\":\"owned-partial-json\",\"choices\":[".to_string(),
                3 => format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"content":"owned partial text"}}]})),
                _ => String::new(),
            };
            let chunks = async_stream::stream! {
                if !prefix.is_empty() { yield Ok::<_, std::io::Error>(axum::body::Bytes::from(prefix)); }
                tokio::time::sleep(Duration::from_secs(3)).await;
                yield Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b": owned late\n\n"));
            };
            Response::builder().status(status).header(CONTENT_TYPE, if streaming {"text/event-stream"} else {"application/json"})
                .body(axum::body::Body::from_stream(chunks)).unwrap()
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, state, task)
}

async fn audit(db: &db::Db, count: usize) -> Value {
    for _ in 0..100 {
        let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
        if let Some(row) = rows
            .into_iter()
            .find(|row| row["attempts"].as_array().is_some_and(|v| v.len() == count))
        {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("owned audit did not settle");
}

async fn timeout(stage: u8, stream: bool) {
    let (bad_url, bad, source) = stalled(stage).await;
    let (good_url, good, backup) =
        spawn_openai_upstream(OpenAiBehavior::Complete("owned timeout backup")).await;
    let (db, cfg, gateway, base) = spawn_gateway_with(
        vec![
            provider("owned-timeout", bad_url, Dialect::OpenAI, 1),
            provider("owned-timeout-backup", good_url, Dialect::OpenAI, 1000),
        ],
        |cfg| {
            cfg.upstream_timeout_secs = 1;
            cfg.failover_enabled = true;
            cfg.max_fallback_attempts = 2;
        },
    )
    .await;
    let outcome = std::panic::AssertUnwindSafe(async {
        let mut body = openai_body("integration-model");
        body["stream"] = json!(stream);
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(&cfg.unified_key)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let text = response.text().await.unwrap();
        let partial = stage == 3;
        let row = audit(&db, if partial { 1 } else { 2 }).await;
        assert_eq!(bad.hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            good.hits.load(Ordering::SeqCst),
            if partial { 0 } else { 1 }
        );
        if partial {
            assert!(text.contains("owned partial text"));
            assert!(!text.contains("owned timeout backup"));
            assert_eq!(
                row["status"], 504,
                "post-selection timeout is not a completed response: {row}"
            );
        } else {
            assert!(text.contains("owned timeout backup"));
            assert_eq!(row["status"], 200);
        }
        let first = &row["attempts"][0];
        assert_eq!(first["provider_id"], "owned-timeout");
        // Pre-selection failure records preserve a bounded readable diagnostic;
        // the post-selection audit stores the stable classifier. Do not change
        // that established field into a new enum just to satisfy this fixture.
        if partial {
            assert_eq!(first["reason"], "timeout");
        } else {
            assert!(
                first["reason"].as_str().unwrap().contains("超时"),
                "timeout diagnostic lost: {first}"
            );
        }
        assert_eq!(first["ok"], false);
        let timing = &first["timing"];
        assert_eq!(timing["schema_version"], 2);
        assert_eq!(
            timing["mode"],
            if stream { "streaming" } else { "buffered" }
        );
        assert_eq!(timing["terminal"], "failed");
        assert_eq!(timing["response_headers_ms"].is_u64(), stage != 0);
        assert_eq!(timing["first_byte_ms"].is_u64(), stage >= 2);
        assert_eq!(timing["first_text_ms"].is_u64(), partial);
        assert!(timing["finished_ms"].is_u64());
        for key in ["queue_ms", "provider_compute_ms", "tool_wait_ms"] {
            assert!(timing.get(key).is_none());
        }
        if !partial {
            let second = &row["attempts"][1];
            assert_eq!(second["provider_id"], "owned-timeout-backup");
            assert_eq!(second["ok"], true);
            assert_eq!(second["timing"]["terminal"], "completed");
            assert!(second["timing"]["first_byte_ms"].is_u64());
            assert_eq!(second["timing"]["first_text_ms"].is_u64(), stream);
        }
        let overview = repo::stats_overview(db.pool()).await.unwrap();
        let measured = &overview["request_timings"];
        assert_eq!(measured["total_requests"], 1);
        assert_eq!(measured["valid_attempts"], if partial { 1 } else { 2 });
        let groups = measured["groups"].as_array().unwrap();
        let failed = groups
            .iter()
            .find(|v| v["provider_id"] == "owned-timeout")
            .unwrap();
        assert_eq!(
            failed["metrics"]["response_headers"]["samples"],
            if stage == 0 { 0 } else { 1 }
        );
        assert_eq!(
            failed["metrics"]["first_byte"]["samples"],
            if stage >= 2 { 1 } else { 0 }
        );
        assert_eq!(
            failed["metrics"]["first_text"]["samples"],
            if partial { 1 } else { 0 }
        );
    })
    .catch_unwind()
    .await;
    gateway.abort();
    source.abort();
    backup.abort();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn timing_failure_cache_timeout_before_headers_has_no_invented_samples() {
    for stream in [false, true] {
        timeout(0, stream).await;
    }
}
#[tokio::test]
async fn timing_failure_cache_timeout_after_headers_before_body_keeps_timeout_kind() {
    timeout(1, true).await;
}
#[tokio::test]
async fn timing_failure_cache_ping_before_timeout_is_first_byte_not_first_content() {
    timeout(2, true).await;
}
#[tokio::test]
async fn timing_failure_cache_timeout_after_content_never_replays_and_is_failed() {
    timeout(3, true).await;
}

#[tokio::test]
async fn timing_failure_cache_body_timeout_remains_a_typed_gateway_error() {
    use futures_util::StreamExt;
    for stage in [1, 2, 3] {
        let (url, seen, source) = stalled(stage).await;
        let p = provider("owned-direct-timeout", url, Dialect::OpenAI, 1);
        let request = serde_json::from_value::<llm_gateway_lib::protocol::openai::OaChatRequest>(
            openai_body("integration-model"),
        )
        .unwrap()
        .to_internal()
        .unwrap();
        let client = llm_gateway_lib::proxy::upstream::UpstreamClient::new();
        let mut stream = client
            .call_stream(
                &p,
                &request,
                "integration-model",
                Duration::from_secs(1),
                &llm_gateway_lib::config::OllamaOptionsConfig::default(),
            )
            .await
            .unwrap();
        let mut failure = None;
        while let Some(event) = stream.next().await {
            if let Err(error) = event {
                failure = Some(error);
                break;
            }
        }
        source.abort();
        assert_eq!(seen.hits.load(Ordering::SeqCst), 1);
        let error = failure.expect("actual reqwest body deadline must fail");
        assert!(
            matches!(error, llm_gateway_lib::error::GatewayError::Timeout(_)),
            "typed body timeout lost: {error:?}"
        );
        assert_eq!(error.http_status(), StatusCode::GATEWAY_TIMEOUT);
    }
}

#[tokio::test]
async fn timing_failure_cache_hit_does_not_borrow_leader_http_samples_or_cost() {
    let (url, seen, source) =
        spawn_openai_upstream(OpenAiBehavior::Complete("owned cached text")).await;
    let (db, cfg, gateway, base) = spawn_gateway_with(
        vec![provider("owned-cache", url, Dialect::OpenAI, 1)],
        |cfg| cfg.cache.enabled = true,
    )
    .await;
    let outcome = std::panic::AssertUnwindSafe(async {
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(4)).build().unwrap();
        for (index, trace) in ["11111111111111111111111111111111", "22222222222222222222222222222222"].into_iter().enumerate() {
            let response = client.post(format!("{base}/v1/chat/completions")).bearer_auth(&cfg.unified_key)
                .header("x-trace-id", trace).json(&openai_body("integration-model")).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            if index == 1 { assert_eq!(response.headers().get("x-cache").unwrap(), "HIT"); }
            response.bytes().await.unwrap();
        }
        assert_eq!(seen.hits.load(Ordering::SeqCst), 1);
        let mut raw = None;
        for _ in 0..100 {
            raw = sqlx::query_as::<_, (i64, i64, f64, String)>("SELECT prompt_tokens, completion_tokens, cost, attempts_json FROM requests WHERE trace_id = ?")
                .bind("22222222222222222222222222222222").fetch_optional(db.pool()).await.unwrap();
            if raw.is_some() { break; } tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (prompt, completion, cost, attempts) = raw.expect("owned HIT audit");
        assert_eq!((prompt, completion, cost), (0, 0, 0.0));
        assert_eq!(serde_json::from_str::<Value>(&attempts).unwrap(), json!([]));
        // MISS and HIT audits are independent spawned writes. Seeing the HIT
        // does not establish that its leader has committed; wait for both rows.
        let mut persisted = 0_i64;
        for _ in 0..100 {
            persisted = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM requests WHERE trace_id IN (?, ?)")
                .bind("11111111111111111111111111111111")
                .bind("22222222222222222222222222222222")
                .fetch_one(db.pool()).await.unwrap();
            if persisted == 2 { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(persisted, 2, "both owned audits must exist before aggregate assertions");
        let overview = repo::stats_overview(db.pool()).await.unwrap(); let measured = &overview["request_timings"];
        assert_eq!(measured["total_requests"], 2);
        assert_eq!(measured["observed_requests"], 1);
        assert_eq!(measured["valid_attempts"], 1);
        assert_eq!(measured["observed_control_attempts"], 1);
        let groups = measured["groups"].as_array().unwrap(); assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["attempts"], 1);
        assert_eq!(groups[0]["metrics"]["first_byte"]["samples"], 1);
        assert_eq!(groups[0]["metrics"]["first_text"]["samples"], 0);
    }).catch_unwind().await;
    gateway.abort();
    source.abort();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn timing_failure_cache_buffered_body_timeout_does_not_become_invalid_json() {
    for stage in [1, 2] {
        timeout(stage, false).await;
    }
}

#[tokio::test]
async fn timing_failure_cache_buffered_body_timeout_remains_a_typed_gateway_error() {
    for stage in [1, 2] {
        let (url, seen, source) = stalled(stage).await;
        let p = provider("owned-buffered-body-timeout", url, Dialect::OpenAI, 1);
        let request = serde_json::from_value::<llm_gateway_lib::protocol::openai::OaChatRequest>(
            openai_body("integration-model"),
        )
        .unwrap()
        .to_internal()
        .unwrap();
        let client = llm_gateway_lib::proxy::upstream::UpstreamClient::new();
        let result = client
            .call(
                &p,
                &request,
                "integration-model",
                Duration::from_secs(1),
                &llm_gateway_lib::config::OllamaOptionsConfig::default(),
            )
            .await;
        source.abort();
        assert_eq!(seen.hits.load(Ordering::SeqCst), 1);
        let error = result.expect_err("actual buffered body deadline must fail");
        assert!(
            matches!(error, llm_gateway_lib::error::GatewayError::Timeout(_)),
            "buffered body timeout lost: {error:?}"
        );
        assert_eq!(error.http_status(), StatusCode::GATEWAY_TIMEOUT);
    }
}

#[tokio::test]
async fn timing_failure_cache_buffered_body_error_status_remains_authoritative() {
    for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        let (url, seen, source) = stalled_with_status(1, status).await;
        let p = provider("owned-http-error-body", url, Dialect::OpenAI, 1);
        let request = serde_json::from_value::<llm_gateway_lib::protocol::openai::OaChatRequest>(
            openai_body("integration-model"),
        )
        .unwrap()
        .to_internal()
        .unwrap();
        let client = llm_gateway_lib::proxy::upstream::UpstreamClient::new();
        let result = client
            .call(
                &p,
                &request,
                "integration-model",
                Duration::from_secs(1),
                &llm_gateway_lib::config::OllamaOptionsConfig::default(),
            )
            .await;
        source.abort();
        assert_eq!(seen.hits.load(Ordering::SeqCst), 1);
        let error = result.expect_err("known HTTP error remains an error after its body stalls");
        assert_eq!(error.upstream_status(), Some(status.as_u16()));
        assert_eq!(error.http_status(), status);
        assert_eq!(
            error.retryable(),
            matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            )
        );
    }
}

#[tokio::test]
async fn timing_failure_cache_dedup_enforce_and_observe_measure_only_actual_http() {
    use llm_gateway_lib::proxy::dedup::DedupMode;
    for mode in [DedupMode::Observe, DedupMode::Enforce] {
        let enforce = matches!(mode, DedupMode::Enforce);
        let (url, seen, source) =
            spawn_openai_upstream(OpenAiBehavior::Complete("owned dedup text")).await;
        let (db, cfg, gateway, base) = spawn_gateway_with(
            vec![provider("owned-dedup", url, Dialect::OpenAI, 1)],
            |cfg| {
                cfg.cache.enabled = false;
                cfg.dedup.mode = mode;
                cfg.dedup.window_secs = 60;
                cfg.dedup.wait_secs = 0;
            },
        )
        .await;
        let outcome = std::panic::AssertUnwindSafe(async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(4))
                .build()
                .unwrap();
            for index in 0..2 {
                let response = client
                    .post(format!("{base}/v1/chat/completions"))
                    .bearer_auth(&cfg.unified_key)
                    .json(&openai_body("integration-model"))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    if enforce && index == 1 {
                        StatusCode::CONFLICT
                    } else {
                        StatusCode::OK
                    }
                );
                if enforce && index == 1 {
                    assert_eq!(response.headers().get("x-dedup").unwrap(), "conflict");
                }
                response.bytes().await.unwrap();
            }
            let expected = if enforce { 1 } else { 2 };
            assert_eq!(seen.hits.load(Ordering::SeqCst), expected);
            let mut measured = Value::Null;
            for _ in 0..100 {
                measured =
                    repo::stats_overview(db.pool()).await.unwrap()["request_timings"].clone();
                if measured["observed_requests"] == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(measured["observed_requests"], expected);
            assert_eq!(measured["valid_attempts"], expected);
            assert_eq!(measured["observed_control_attempts"], expected);
            let groups = measured["groups"].as_array().unwrap();
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0]["attempts"], expected);
            assert_eq!(groups[0]["metrics"]["first_byte"]["samples"], expected);
            assert_eq!(groups[0]["metrics"]["finished"]["samples"], expected);
            assert_eq!(groups[0]["metrics"]["first_text"]["samples"], 0);
        })
        .catch_unwind()
        .await;
        gateway.abort();
        source.abort();
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}
