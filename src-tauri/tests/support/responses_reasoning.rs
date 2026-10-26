//! Owned HTTP controls: the request must carry reasoning, not just describe its loss.
use super::*;
use futures_util::FutureExt;
use serde_json::{json, Value};

async fn native_upstream() -> (String, MockState, JoinHandle<()>) {
    async fn respond(State(state): State<MockState>, Json(body): Json<Value>) -> Response {
        state.hits.fetch_add(1, Ordering::SeqCst);
        state.requests.lock().unwrap().push(body.clone());
        let reply = json!({"id":"resp_owned_reasoning","object":"response","status":"completed",
            "model":"integration-model","output":[{"type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"owned reasoning reply"}]}],
            "usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}});
        if body["stream"] == true {
            let frames = [
                json!({"type":"response.created","response":{"id":"resp_owned_reasoning","status":"in_progress"}}),
                json!({"type":"response.output_text.delta","output_index":0,"delta":"owned reasoning reply"}),
                json!({"type":"response.completed","response":reply}),
            ];
            let sse: String = frames
                .iter()
                .map(|frame| {
                    format!(
                        "event: {}\r\ndata: {frame}\r\n\r\n",
                        frame["type"].as_str().unwrap()
                    )
                })
                .collect();
            ([(CONTENT_TYPE, "text/event-stream")], sse).into_response()
        } else {
            Json(reply).into_response()
        }
    }
    let state = MockState::default();
    let app = Router::new()
        .route("/responses", post(respond))
        .fallback(respond)
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, state, task)
}

async fn exercise(dialect: Dialect, compaction: bool, summary: bool, reject: bool) {
    for stream in [false, true] {
        let (url, seen, source) = if dialect != Dialect::OpenAI {
            native_upstream().await
        } else {
            spawn_openai_upstream(OpenAiBehavior::Complete("owned reasoning reply")).await
        };
        let mut p = provider("owned-responses-reasoning", url, dialect, 1);
        p.models[0].supports_thinking = true;
        let (backup_url, backup_seen, backup_task) = native_upstream().await;
        let mut backup = provider(
            "owned-reasoning-backup",
            backup_url,
            Dialect::Responses,
            1000,
        );
        backup.models[0].supports_thinking = true;
        let db = db::Db::connect_in_memory().await.unwrap();
        for candidate in [p, backup] {
            repo::upsert_provider(db.pool(), &candidate).await.unwrap();
        }
        let cfg = AppConfig {
            port: unused_loopback_port().await,
            unified_key: "owned-reasoning-key".into(),
            failover_enabled: true,
            max_fallback_attempts: 3,
            ..Default::default()
        };
        let state = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
        state.reload_providers().await.unwrap();
        let health_before = serde_json::to_value(state.health.snapshot()).unwrap();
        let owned = state.clone();
        let gateway = tokio::spawn(async move {
            let _ = serve(owned).await;
        });
        let base = format!("http://{}:{}", cfg.bind, cfg.port);
        wait_for_gateway(&base).await;
        let outcome = std::panic::AssertUnwindSafe(async {
            let mut reasoning = json!({"effort":"high"});
            if summary { reasoning["summary"] = json!("auto"); }
            let input = if compaction {
                json!([{"role":"user","content":"owned input"},{"type":"compaction_trigger"}])
            } else { json!("owned input") };
            let response = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(4)).build().unwrap()
                .post(format!("{base}/v1/responses")).bearer_auth(&cfg.unified_key)
                .json(&json!({"model":"integration-model","input":input,"stream":stream,"reasoning":reasoning}))
                .send().await.unwrap();
            let status = response.status();
            let text = response.text().await.unwrap();
            if reject {
                assert_eq!(status, StatusCode::BAD_REQUEST, "unrepresentable summary must be explicit: {text}");
                assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["error"]["type"], "invalid_request_error");
                assert_eq!(seen.hits.load(Ordering::SeqCst), 0, "local contract rejection must not send HTTP");
                assert_eq!(backup_seen.hits.load(Ordering::SeqCst), 0, "must not silently downgrade to another dialect");
                assert_eq!(state.health.total_failovers(), 0);
                assert_eq!(serde_json::to_value(state.health.snapshot()).unwrap(), health_before);
                assert!(repo::list_providers(db.pool()).await.unwrap().iter().all(|p| p.enabled));
                return;
            }
            assert_eq!(status, StatusCode::OK, "{text}");
            assert_eq!(seen.hits.load(Ordering::SeqCst), 1);
            let wire = seen.requests.lock().unwrap()[0].clone();
            // Remote compaction's existing upstream call is always buffered;
            // the caller may independently request an SSE response wrapper.
            assert_eq!(wire["stream"], stream && !compaction);
            assert_eq!(backup_seen.hits.load(Ordering::SeqCst), 0);
            if dialect == Dialect::Responses {
                assert_eq!(wire["reasoning"], reasoning, "native controls must reach actual HTTP unchanged");
                assert!(wire.get("reasoning_effort").is_none());
            } else {
                assert_eq!(wire["reasoning_effort"], "high", "Responses effort must use Chat Completions field");
                assert!(wire.get("reasoning").is_none());
            }
            assert!(wire.get("thinking").is_none(), "do not invent a third dialect control");
            let mut saved = None;
            for _ in 0..100 {
                let raw: Option<(Option<String>,)> = sqlx::query_as("SELECT attempts_json FROM requests ORDER BY id DESC LIMIT 1")
                    .fetch_optional(db.pool()).await.unwrap();
                if let Some((Some(raw),)) = raw { saved = Some(serde_json::from_str::<Value>(&raw).unwrap()); break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let attempts = saved.expect("owned audit settled");
            assert_eq!(attempts.as_array().unwrap().len(), 1);
            let modes = &attempts[0]["wire_modes"];
            assert_eq!(modes["caller"]["dialect"], "responses");
            for stage in ["normalized_ingress", "effective_request", "wire"] {
                assert_eq!(modes[stage]["reasoning"], "enabled", "{stage}: {modes}");
            }
        }).catch_unwind().await;
        gateway.abort();
        source.abort();
        backup_task.abort();
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}

#[tokio::test]
async fn responses_reasoning_native_preserves_effort_and_summary() {
    exercise(Dialect::Responses, false, true, false).await;
}
#[tokio::test]
async fn responses_reasoning_chat_completions_maps_effort_without_thinking_leak() {
    exercise(Dialect::OpenAI, false, false, false).await;
}
#[tokio::test]
async fn responses_reasoning_compaction_preserves_requested_effort() {
    for dialect in [Dialect::Responses, Dialect::OpenAI] {
        exercise(dialect, true, false, false).await;
    }
}
#[tokio::test]
async fn responses_reasoning_chat_completions_summary_rejected_before_http() {
    exercise(Dialect::OpenAI, false, true, true).await;
}

#[tokio::test]
async fn responses_reasoning_unmapped_dialects_reject_without_health_or_fallback() {
    for dialect in [Dialect::Anthropic, Dialect::Gemini, Dialect::Ollama] {
        exercise(dialect, false, false, true).await;
    }
}

#[tokio::test]
async fn responses_reasoning_chat_effort_maps_back_to_native_responses() {
    for stream in [false, true] {
        let (url, seen, source) = native_upstream().await;
        let mut p = provider("owned-reasoning-reverse", url, Dialect::Responses, 1);
        p.models[0].supports_thinking = true;
        let (_db, cfg, gateway, base) = spawn_gateway(vec![p]).await;
        let outcome = std::panic::AssertUnwindSafe(async {
            let mut body = openai_body("integration-model");
            body["stream"] = json!(stream);
            body["reasoning_effort"] = json!("xhigh");
            let response = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(4))
                .build()
                .unwrap()
                .post(format!("{base}/v1/chat/completions"))
                .bearer_auth(&cfg.unified_key)
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let text = response.text().await.unwrap();
            assert_eq!(status, StatusCode::OK, "{text}");
            assert_eq!(seen.hits.load(Ordering::SeqCst), 1);
            let wire = seen.requests.lock().unwrap()[0].clone();
            assert_eq!(wire["reasoning"], json!({"effort":"xhigh"}));
            assert!(wire.get("thinking").is_none());
            assert!(wire.get("reasoning_effort").is_none());
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

#[tokio::test]
async fn responses_reasoning_conflicting_ir_efforts_reject_at_actual_http_boundary() {
    for dialect in [Dialect::Responses, Dialect::OpenAI] {
        for stream in [false, true] {
            let (url, seen, source) = native_upstream().await;
            let mut p = provider("owned-reasoning-conflict", url, dialect, 1);
            p.models[0].supports_thinking = true;
            let mut req =
                serde_json::from_value::<llm_gateway_lib::protocol::openai::OaChatRequest>(
                    openai_body("integration-model"),
                )
                .unwrap()
                .to_internal()
                .unwrap();
            req.extra
                .insert("reasoning".into(), json!({"effort":"low"}));
            req.extra.insert("reasoning_effort".into(), json!("high"));
            let client = llm_gateway_lib::proxy::upstream::UpstreamClient::new();
            let defaults = llm_gateway_lib::config::OllamaOptionsConfig::default();
            let error = if stream {
                client
                    .call_stream(
                        &p,
                        &req,
                        "integration-model",
                        Duration::from_secs(3),
                        &defaults,
                    )
                    .await
                    .map(|_| ())
                    .expect_err("local conflict")
            } else {
                client
                    .call(
                        &p,
                        &req,
                        "integration-model",
                        Duration::from_secs(3),
                        &defaults,
                    )
                    .await
                    .map(|_| ())
                    .expect_err("local conflict")
            };
            source.abort();
            assert!(matches!(
                error,
                llm_gateway_lib::error::GatewayError::RequestControlUnavailable { .. }
            ));
            assert_eq!(error.http_status(), StatusCode::BAD_REQUEST);
            assert!(!error.retryable());
            assert_eq!(seen.hits.load(Ordering::SeqCst), 0);
        }
    }
}
