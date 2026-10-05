use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::config::{AppConfig, AuthFailureMode};
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone, Default)]
struct MockState {
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    hits: Arc<AtomicUsize>,
}

#[derive(Clone)]
enum OpenAiBehavior {
    Complete(&'static str),
    CompleteWithCachedUsage(&'static str),
    Status(StatusCode),
    Html,
    TruncatedJson,
}

#[derive(Clone)]
struct OpenAiMock {
    state: MockState,
    behavior: OpenAiBehavior,
}

fn provider(id: &str, base_url: String, dialect: Dialect, priority: i32) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: id.into(),
        dialect,
        base_url,
        api_key_enc: String::new(),
        enabled: true,
        priority,
        models: vec![ModelRef {
            enabled: true,
            alias: "integration-model".into(),
            upstream: "integration-model".into(),
            context_window: 16_384,
            supports_tools: true,
            supports_vision: false,
            supports_audio: false,
            supports_video: false,
            supports_thinking: false,
            supports_stream: true,
            model_type: llm_gateway_lib::domain::ModelType::Chat,
            upstream_path: None,
            price: None,
            overrides: None,
            local: None,
        }],
        rpm_limit: 0,
        intelligence: 50,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

async fn mock_openai_chat(
    State(mock): State<OpenAiMock>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    mock.state.hits.fetch_add(1, Ordering::SeqCst);
    mock.state.requests.lock().unwrap().push(request.clone());

    if request
        .get("stream")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        let (content, usage) = match &mock.behavior {
            OpenAiBehavior::Complete(content) => (
                *content,
                serde_json::json!({
                    "prompt_tokens": 3,
                    "completion_tokens": 2,
                    "total_tokens": 5
                }),
            ),
            OpenAiBehavior::CompleteWithCachedUsage(content) => (
                *content,
                serde_json::json!({
                    "prompt_tokens": 10,
                    "completion_tokens": 2,
                    "total_tokens": 12,
                    "prompt_tokens_details": {
                        "cached_tokens": 8,
                        "cache_write_tokens": 1
                    }
                }),
            ),
            _ => ("", serde_json::json!({})),
        };
        if !content.is_empty() {
            let first = serde_json::json!({
                "id": "chatcmpl-stream",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant", "content": content }
                }]
            });
            let finish = serde_json::json!({
                "id": "chatcmpl-stream",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }]
            });
            let usage = serde_json::json!({
                "id": "chatcmpl-stream",
                "choices": [],
                "usage": usage
            });
            return Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/event-stream")
                .body(
                    format!("data: {first}\n\ndata: {finish}\n\ndata: {usage}\n\ndata: [DONE]\n\n")
                        .into(),
                )
                .unwrap();
        }
    }

    match mock.behavior {
        OpenAiBehavior::Complete(content) => Json(serde_json::json!({
            "id": "chatcmpl-e2e",
            "object": "chat.completion",
            "model": "integration-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop",
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 },
        }))
        .into_response(),
        OpenAiBehavior::CompleteWithCachedUsage(content) => Json(serde_json::json!({
            "id": "chatcmpl-cache",
            "object": "chat.completion",
            "model": "integration-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 2,
                "total_tokens": 12,
                "prompt_tokens_details": {
                    "cached_tokens": 8,
                    "cache_write_tokens": 1
                }
            },
        }))
        .into_response(),
        OpenAiBehavior::Status(status) => (
            status,
            Json(serde_json::json!({
                "error": { "message": format!("mock status {}", status.as_u16()) },
            })),
        )
            .into_response(),
        OpenAiBehavior::Html => Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .body("<html><body>upstream proxy error</body></html>".into())
            .unwrap(),
        OpenAiBehavior::TruncatedJson => Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body("{\"choices\":[{\"message\":{\"content\":\"truncated".into())
            .unwrap(),
    }
}

async fn mock_openai_embeddings(
    State(mock): State<OpenAiMock>,
    _uri: axum::http::Uri,
    Json(request): Json<serde_json::Value>,
) -> Response {
    mock.state.hits.fetch_add(1, Ordering::SeqCst);
    mock.state.requests.lock().unwrap().push(request);
    Json(serde_json::json!({
        "object": "list",
        "data": [{ "object": "embedding", "embedding": [0.9], "index": 0 }],
        "model": "integration-model",
        "usage": { "prompt_tokens": 2, "total_tokens": 2 },
    }))
    .into_response()
}

async fn mock_openai_images(
    State(mock): State<OpenAiMock>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    mock.state.hits.fetch_add(1, Ordering::SeqCst);
    mock.state.requests.lock().unwrap().push(request);
    Json(serde_json::json!({
        "created": 1,
        "data": [{ "b64_json": "AAAA" }],
    }))
    .into_response()
}

async fn mock_openai_speech(
    State(mock): State<OpenAiMock>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    mock.state.hits.fetch_add(1, Ordering::SeqCst);
    mock.state.requests.lock().unwrap().push(request);
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "audio/mpeg")
        .body("ID3mock-audio".into())
        .unwrap()
}

async fn spawn_openai_upstream(behavior: OpenAiBehavior) -> (String, MockState, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = MockState::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_openai_chat))
        .route("/v1/embeddings", post(mock_openai_embeddings))
        .route("/v1/images/generations", post(mock_openai_images))
        .route("/v1/audio/speech", post(mock_openai_speech))
        .route("/custom/embed", post(mock_openai_embeddings))
        .route("/custom/models/:model/chat", post(mock_openai_chat))
        .with_state(OpenAiMock {
            state: state.clone(),
            behavior,
        });
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/v1"), state, task)
}

#[derive(Clone)]
struct AnthropicMock {
    state: MockState,
}

async fn mock_anthropic_message(
    State(mock): State<AnthropicMock>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    mock.state.hits.fetch_add(1, Ordering::SeqCst);
    mock.state.requests.lock().unwrap().push(request);
    Json(serde_json::json!({
        "id": "msg-e2e",
        "type": "message",
        "role": "assistant",
        "model": "integration-model",
        "content": [{ "type": "text", "text": "anthropic reply" }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": { "input_tokens": 4, "output_tokens": 2 },
    }))
    .into_response()
}

async fn spawn_anthropic_upstream() -> (String, MockState, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = MockState::default();
    let app = Router::new()
        .route("/v1/messages", post(mock_anthropic_message))
        .with_state(AnthropicMock {
            state: state.clone(),
        });
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/v1"), state, task)
}

async fn unused_loopback_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn wait_for_gateway(base_url: &str) {
    let client = reqwest::Client::new();
    for _ in 0..80 {
        if client
            .get(format!("{base_url}/healthz"))
            .send()
            .await
            .is_ok_and(|response| response.status() == StatusCode::OK)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gateway did not start in time");
}

/// 给测试供应商补一把 Key。
///
/// **为什么必须补**：鉴权失败策略里有一条硬约束——上游拒了凭据之后不得回落到
/// **免 Key** 后端（本地 Ollama 那类）。夹具若不带 Key，备选会被这条约束正确地
/// 跳过，于是测试测到的不是「换下一家」而是「没有下一家」。真实云供应商必然带 Key，
/// 夹具必须对齐真实形状。
fn keyed(mut provider: Provider) -> Provider {
    // 必须是**真的**密文：转发前会 `crypto::decrypt`，塞明文串会得到 500，
    // 症状看着像服务端炸了，实际是夹具形状不对（`tests/live_provider_smoke.rs` 同款做法）。
    provider.api_key_enc =
        llm_gateway_lib::crypto::encrypt("test-key").expect("测试应能用真加密格式的 Key");
    provider
}

async fn spawn_gateway(providers: Vec<Provider>) -> (db::Db, AppConfig, JoinHandle<()>, String) {
    spawn_gateway_with(providers, |_| {}).await
}

/// 带配置改写的网关实例。鉴权失败策略这类「同一套代码、不同档位」的行为
/// 必须各自钉住，否则改了一档就会悄悄带走另一档。
async fn spawn_gateway_with(
    providers: Vec<Provider>,
    tune: impl FnOnce(&mut AppConfig),
) -> (db::Db, AppConfig, JoinHandle<()>, String) {
    let db = db::Db::connect_in_memory().await.unwrap();
    for provider in providers {
        repo::upsert_provider(db.pool(), &provider).await.unwrap();
    }
    let mut config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: "e2e-gateway-key".into(),
        ..Default::default()
    };
    tune(&mut config);
    let gateway = Arc::new(GatewayState::new(db.clone(), config.clone()));
    gateway.reload_providers().await.unwrap();
    let task = tokio::spawn({
        let gateway = gateway.clone();
        async move {
            let _ = serve(gateway).await;
        }
    });
    let base_url = format!("http://{}:{}", config.bind, config.port);
    wait_for_gateway(&base_url).await;
    (db, config, task, base_url)
}

fn openai_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": "hello" }],
    })
}

fn parse_sse_json(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter_map(|line| line.trim().strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .filter_map(|data| serde_json::from_str(data).ok())
        .collect()
}

#[tokio::test]
async fn local_auth_health_models_unknown_model_and_thinking_contracts_hold_over_http() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("normal reply")).await;
    let (_db, config, gateway_task, base_url) =
        spawn_gateway(vec![provider("primary", upstream_url, Dialect::OpenAI, 1)]).await;
    let client = reqwest::Client::new();

    let health = client
        .get(format!("{base_url}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(health.text().await.unwrap(), "ok");

    let models = client
        .get(format!("{base_url}/v1/models"))
        .bearer_auth(&config.unified_key)
        .send()
        .await
        .unwrap();
    assert_eq!(models.status(), StatusCode::OK);
    let models: serde_json::Value = models.json().await.unwrap();
    assert_eq!(models["object"], "list");
    assert_eq!(models["data"][0]["id"], "integration-model");
    assert_eq!(models["data"][0]["backed_by"], 1);

    let bad_key = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth("incorrect-key")
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_key.status(), StatusCode::UNAUTHORIZED);
    let bad_key: serde_json::Value = bad_key.json().await.unwrap();
    assert_eq!(bad_key["error"]["type"], "invalid_api_key");
    assert_eq!(state.hits.load(Ordering::SeqCst), 0);

    let missing = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&openai_body("not-configured"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let missing: serde_json::Value = missing.json().await.unwrap();
    assert!(missing["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not-configured"));

    let mut body = openai_body("integration-model");
    body["thinking"] = serde_json::json!({ "type": "enabled" });
    body["reasoning"] = serde_json::json!({ "effort": "high" });
    body["reasoning_effort"] = serde_json::json!("high");
    let success = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(success.status(), StatusCode::OK);
    let success: serde_json::Value = success.json().await.unwrap();
    assert_eq!(success["choices"][0]["message"]["content"], "normal reply");
    let upstream_request = state.requests.lock().unwrap().last().cloned().unwrap();
    assert!(upstream_request.get("thinking").is_none());
    assert!(upstream_request.get("reasoning").is_none());
    assert!(upstream_request.get("reasoning_effort").is_none());

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn anonymous_session_id_is_returned_and_reused_without_crossing_same_first_prompt() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("stored reply")).await;
    let (db, config, gateway_task, base_url) =
        spawn_gateway(vec![provider("primary", upstream_url, Dialect::OpenAI, 1)]).await;
    let client = reqwest::Client::new();
    let first_body = serde_json::json!({
        "model": "integration-model",
        "messages": [{ "role": "user", "content": "我的标识是匿名甲" }],
    });

    let first = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&first_body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let session_id = first
        .headers()
        .get("x-session-id")
        .expect("匿名响应必须回传会话 ID")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(session_id.starts_with("a-"));

    let continuation = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", &session_id)
        .json(&serde_json::json!({
            "model": "integration-model",
            "messages": [{ "role": "user", "content": "请用此前信息继续回答" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(continuation.status(), StatusCode::OK);
    assert_eq!(
        continuation.headers()["x-session-id"].to_str().unwrap(),
        session_id
    );

    let separate = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&first_body)
        .send()
        .await
        .unwrap();
    assert_eq!(separate.status(), StatusCode::OK);
    let separate_session_id = separate.headers()["x-session-id"].to_str().unwrap();
    assert_ne!(separate_session_id, session_id);

    {
        let upstream_requests = state.requests.lock().unwrap();
        assert_eq!(upstream_requests.len(), 3);
        let continued_messages = upstream_requests[1]["messages"].as_array().unwrap();
        assert!(continued_messages
            .iter()
            .any(|message| message["content"] == "我的标识是匿名甲"));
        assert!(continued_messages
            .iter()
            .any(|message| message["content"] == "请用此前信息继续回答"));
        let separate_messages = upstream_requests[2]["messages"].as_array().unwrap();
        assert!(separate_messages
            .iter()
            .all(|message| message["content"] != "请用此前信息继续回答"));
    }

    assert_eq!(repo::list_sessions(db.pool(), 10).await.unwrap().len(), 2);

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn malformed_successful_upstreams_are_retryable_and_fall_back_at_gateway_boundary() {
    for behavior in [OpenAiBehavior::Html, OpenAiBehavior::TruncatedJson] {
        let (bad_url, bad_state, bad_task) = spawn_openai_upstream(behavior).await;
        let (good_url, good_state, good_task) =
            spawn_openai_upstream(OpenAiBehavior::Complete("backup reply")).await;
        let (_db, config, gateway_task, base_url) = spawn_gateway(vec![
            provider("malformed", bad_url, Dialect::OpenAI, 1),
            provider("backup", good_url, Dialect::OpenAI, 2),
        ])
        .await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}/v1/chat/completions"))
            .bearer_auth(&config.unified_key)
            .json(&openai_body("integration-model"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["x-routed-via"],
            "backup/integration-model"
        );
        assert_eq!(response.headers()["x-fallback-attempts"], "2");
        let response: serde_json::Value = response.json().await.unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "backup reply");
        assert_eq!(bad_state.hits.load(Ordering::SeqCst), 1);
        assert_eq!(good_state.hits.load(Ordering::SeqCst), 1);

        gateway_task.abort();
        bad_task.abort();
        good_task.abort();
    }
}

/// 上游 **404「我这儿没这个模型」** 是「换一家」的典型理由，不是客户端错：
/// 候选链按别名**跨供应商**组装，同名模型常常另一家还有。
///
/// 2026-10-05 实测事故：`openrouter/stealth/space-bunny-alpha` 回 404 被判
/// 不可重试（`fallback_attempts=0`），链上 `commandcode` 的同名模型**一次都
/// 没被试**，整个会话直接失败。
///
/// 这条同时钉住同一次修复的两面：
///   ① 404 必须继续试下一家 —— 两家各命中一次才算数；
///   ② **全部候选都失败**时，审计里的 `fallback_attempts` 必须是真实回退数。
///      修前它恒为 0：候选耗尽后 failover 返回的是**最后一个上游错误**，
///      `AllProvidersFailed{attempts}` 成了走不到的死分支，审计只能从错误里
///      反推出 1，于是「切换了但都失败」与「切换完全没生效」在审计里长得一样。
#[tokio::test]
async fn upstream_404_falls_back_and_failed_chain_reports_real_fallback_count() {
    let (first_url, first_state, first_task) =
        spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::NOT_FOUND)).await;
    let (second_url, second_state, second_task) =
        spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::NOT_FOUND)).await;

    let (db, config, gateway_task, base_url) = spawn_gateway(vec![
        provider("gone-a", first_url, Dialect::OpenAI, 1),
        provider("gone-b", second_url, Dialect::OpenAI, 2),
    ])
    .await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();

    // 两家都 404 ⇒ 整体失败，**但两家都必须被试过**。
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        first_state.hits.load(Ordering::SeqCst),
        1,
        "第一家必须被试 —— 404 若判不可重试，这里就是死路"
    );
    assert_eq!(
        second_state.hits.load(Ordering::SeqCst),
        1,
        "404 之后必须换第二家；命中 0 次说明第一家的 404 直接把链掐断了"
    );

    // 审计写入是异步的；轮询等待那条失败记录出现。
    let mut logged = None;
    for _ in 0..80 {
        let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
        if let Some(row) = rows.into_iter().find(|r| r["status"].as_i64() == Some(404)) {
            logged = Some(row);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let logged = logged.expect("失败请求也必须写审计日志");
    assert_eq!(
        logged["fallback_attempts"].as_i64(),
        Some(1),
        "试了 2 家就是 1 次回退；写成 0 会把「切换了但都失败」误报成「切换没生效」"
    );
    // 注意字段名：`recent_requests` 把 `attempts_json` 列映射成 `attempts`，
    // 且已经是数组，不需要再 parse 一次字符串。
    let chain = logged["attempts"]
        .as_array()
        .expect("失败请求也应记录尝试链");
    assert_eq!(
        chain.len(),
        2,
        "尝试链必须同时记下这两家，否则事后无法判断到底试过谁"
    );
    assert!(
        chain.iter().all(|a| a["retryable"].as_bool() == Some(true)),
        "两家都是 404，必须都记为可重试：{chain:?}"
    );

    gateway_task.abort();
    first_task.abort();
    second_task.abort();
}

/// 鉴权失败的新契约（用户 2026-10-05 定）：先复测确认，确认不可用才换下一家。
///
/// 旧契约是「一次 401 就原样返回、不碰备选」——它对应的是 `strict` 档，
/// 由下一条测试继续钉住。两条都在，档位之间的差异才是真的被守住。
#[tokio::test]
async fn upstream_auth_failure_is_confirmed_then_falls_back_to_the_backup() {
    let (bad_url, bad_state, bad_task) =
        spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::UNAUTHORIZED)).await;
    let (good_url, good_state, good_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("backup reply")).await;
    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![
        keyed(provider("invalid-key", bad_url, Dialect::OpenAI, 1)),
        keyed(provider("backup", good_url, Dialect::OpenAI, 2)),
    ])
    .await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-routed-via"],
        "backup/integration-model"
    );
    assert_eq!(
        bad_state.hits.load(Ordering::SeqCst),
        2,
        "首次 + 一次确认复测"
    );
    assert_eq!(good_state.hits.load(Ordering::SeqCst), 1);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "backup reply");

    gateway_task.abort();
    bad_task.abort();
    good_task.abort();
}

/// 对照组：`strict` 档保持旧行为——一次 401 立刻原样返回，备选一次都不碰。
///
/// 没有这条，「新契约通过」可能只是因为 `strict` 早已没人走。
#[tokio::test]
async fn strict_mode_keeps_returning_the_auth_error_without_touching_the_backup() {
    let (bad_url, bad_state, bad_task) =
        spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::UNAUTHORIZED)).await;
    let (good_url, good_state, good_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("must not be used")).await;
    let (_db, config, gateway_task, base_url) = spawn_gateway_with(
        vec![
            keyed(provider("invalid-key", bad_url, Dialect::OpenAI, 1)),
            keyed(provider("backup", good_url, Dialect::OpenAI, 2)),
        ],
        |cfg| cfg.auth_failure.mode = AuthFailureMode::Strict,
    )
    .await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(bad_state.hits.load(Ordering::SeqCst), 1, "strict 不复测");
    assert_eq!(good_state.hits.load(Ordering::SeqCst), 0);

    gateway_task.abort();
    bad_task.abort();
    good_task.abort();
}

#[tokio::test]
async fn anthropic_non_streaming_entrance_lifts_system_for_anthropic_upstream() {
    let (upstream_url, state, upstream_task) = spawn_anthropic_upstream().await;
    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![provider(
        "anthropic",
        upstream_url,
        Dialect::Anthropic,
        1,
    )])
    .await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/messages"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({
            "model": "integration-model",
            "max_tokens": 64,
            "system": "You are a concise assistant.",
            "messages": [{ "role": "user", "content": "Hello" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response: serde_json::Value = response.json().await.unwrap();
    assert_eq!(response["type"], "message");
    assert_eq!(response["role"], "assistant");
    assert_eq!(response["content"][0]["text"], "anthropic reply");
    assert_eq!(response["stop_reason"], "end_turn");
    assert_eq!(response["usage"]["input_tokens"], 4);

    let upstream = state.requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(upstream["system"], "You are a concise assistant.");
    assert_eq!(upstream["messages"][0]["role"], "user");
    assert_eq!(upstream["max_tokens"], 64);

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn responses_non_streaming_string_input_uses_responses_output_contract() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("responses reply")).await;
    let (_db, config, gateway_task, base_url) =
        spawn_gateway(vec![provider("primary", upstream_url, Dialect::OpenAI, 1)]).await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/responses"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({
            "model": "integration-model",
            "input": "use the responses interface",
            "max_output_tokens": 42,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response: serde_json::Value = response.json().await.unwrap();
    assert_eq!(response["object"], "response");
    assert_eq!(response["status"], "completed");
    assert_eq!(response["output"][0]["type"], "message");
    assert_eq!(
        response["output"][0]["content"][0]["text"],
        "responses reply"
    );
    assert_eq!(response["usage"]["input_tokens"], 3);
    assert_eq!(response["usage"]["output_tokens"], 2);

    let upstream = state.requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        upstream["messages"][0]["content"],
        "use the responses interface"
    );
    assert_eq!(upstream["max_tokens"], 42);

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn responses_remote_compaction_v2_returns_one_compaction_item_and_round_trips() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("任务摘要：目标、进度、待办")).await;
    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![provider(
        "compaction",
        upstream_url,
        Dialect::OpenAI,
        1,
    )])
    .await;
    let client = reqwest::Client::new();

    let compact = client
        .post(format!("{base_url}/v1/responses"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({
            "model": "integration-model",
            "stream": true,
            "input": [
                { "type": "message", "role": "user", "content": "原始任务上下文" },
                { "type": "compaction_trigger" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(compact.status(), StatusCode::OK);
    let events = parse_sse_json(&compact.text().await.unwrap());
    let items = events
        .iter()
        .filter(|event| event["type"] == "response.output_item.done")
        .collect::<Vec<_>>();
    assert_eq!(items.len(), 1, "Codex V2 必须收到恰好一个输出项");
    assert_eq!(items[0]["item"]["type"], "compaction");
    let encrypted = items[0]["item"]["encrypted_content"]
        .as_str()
        .expect("compaction encrypted_content");
    assert!(encrypted.starts_with("llm-gateway-compaction-v1:"));
    assert!(events
        .iter()
        .any(|event| event["type"] == "response.completed"));

    let compact_upstream = state.requests.lock().unwrap()[0].clone();
    assert!(!compact_upstream.to_string().contains("compaction_trigger"));
    assert_eq!(compact_upstream["messages"][0]["role"], "system");
    assert!(compact_upstream["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("结构化"));

    let follow_up = client
        .post(format!("{base_url}/v1/responses"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({
            "model": "integration-model",
            "input": [
                { "type": "compaction", "encrypted_content": encrypted },
                { "type": "message", "role": "user", "content": "继续下一步" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(follow_up.status(), StatusCode::OK);
    let follow_up_upstream = state.requests.lock().unwrap().last().cloned().unwrap();
    assert!(follow_up_upstream["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "system"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("任务摘要"))
        }));

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn responses_media_parts_survive_openai_conversion() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("media reply")).await;
    let mut multimodal = provider("multimodal", upstream_url, Dialect::OpenAI, 1);
    multimodal.models[0].supports_vision = true;
    multimodal.models[0].supports_audio = true;
    multimodal.models[0].supports_video = true;
    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![multimodal]).await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/responses"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({
            "model": "integration-model",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "分析" },
                    { "type": "input_image", "image_url": "data:image/png;base64,AAAA" },
                    { "type": "input_audio", "input_audio": { "data": "BBBB", "format": "wav" } },
                    { "type": "input_video", "video_url": "https://www.youtube.com/watch?v=abc" }
                ]
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let upstream = state.requests.lock().unwrap().last().cloned().unwrap();
    let parts = upstream["messages"][0]["content"].as_array().unwrap();
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[1]["type"], "image_url");
    assert_eq!(parts[2]["type"], "input_audio");
    assert_eq!(parts[3]["type"], "video_url");

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn non_chat_model_types_route_to_their_dedicated_openai_endpoints() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("unused")).await;
    let mut embedding = provider("embedding", upstream_url.clone(), Dialect::OpenAI, 1);
    embedding.models[0].model_type = llm_gateway_lib::domain::ModelType::Embedding;
    embedding.models[0].upstream_path = Some("/custom/embed".into());
    let mut image = provider("image", upstream_url.clone(), Dialect::OpenAI, 2);
    image.models[0].model_type = llm_gateway_lib::domain::ModelType::Image;
    let mut speech = provider("speech", upstream_url, Dialect::OpenAI, 3);
    speech.models[0].model_type = llm_gateway_lib::domain::ModelType::Speech;
    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![embedding, image, speech]).await;
    let client = reqwest::Client::new();

    let embedding_response = client
        .post(format!("{base_url}/v1/embeddings"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({ "model": "integration-model", "input": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(embedding_response.status(), StatusCode::OK);
    assert_eq!(
        embedding_response.headers()["x-routed-via"],
        "embedding/integration-model"
    );
    let embedding_body: serde_json::Value = embedding_response.json().await.unwrap();
    assert_eq!(embedding_body["data"][0]["embedding"][0], 0.9);

    let image_response = client
        .post(format!("{base_url}/v1/images/generations"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({ "model": "integration-model", "prompt": "a cat" }))
        .send()
        .await
        .unwrap();
    assert_eq!(image_response.status(), StatusCode::OK);
    assert_eq!(
        image_response.headers()["x-routed-via"],
        "image/integration-model"
    );
    let image_body: serde_json::Value = image_response.json().await.unwrap();
    assert_eq!(image_body["data"][0]["b64_json"], "AAAA");

    let speech_response = client
        .post(format!("{base_url}/v1/audio/speech"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({ "model": "integration-model", "input": "hello", "voice": "alloy" }))
        .send()
        .await
        .unwrap();
    assert_eq!(speech_response.status(), StatusCode::OK);
    assert_eq!(
        speech_response.headers()["x-routed-via"],
        "speech/integration-model"
    );
    assert_eq!(speech_response.headers()["content-type"], "audio/mpeg");
    assert_eq!(
        speech_response.bytes().await.unwrap().as_ref(),
        b"ID3mock-audio"
    );

    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["model"], "integration-model");
    assert_eq!(requests[1]["prompt"], "a cat");
    assert_eq!(requests[2]["voice"], "alloy");

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn per_model_upstream_path_supports_custom_chat_route_and_model_placeholder() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("custom path reply")).await;
    let mut routed = provider("custom-path", upstream_url, Dialect::OpenAI, 1);
    routed.models[0].upstream_path = Some("/custom/models/{model}/chat".into());
    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![routed]).await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "custom path reply"
    );
    assert_eq!(state.hits.load(Ordering::SeqCst), 1);

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn responses_tool_image_normalizes_codex_original_detail_for_openai_upstream() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("image reply")).await;
    let mut vision = provider("vision", upstream_url, Dialect::OpenAI, 1);
    vision.models[0].supports_vision = true;
    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![vision]).await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/responses"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({
            "model": "integration-model",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_image",
                    "name": "view_image",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_image",
                    "output": [{
                        "type": "input_image",
                        "image_url": "data:image/png;base64,AAAA",
                        "detail": "original"
                    }]
                }
            ]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let upstream = state.requests.lock().unwrap().last().cloned().unwrap();
    let tool_message = upstream["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("Responses function_call_output must become a tool message");
    assert_eq!(tool_message["content"][0]["image_url"]["detail"], "high");

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn responses_private_codex_tools_are_flattened_or_dropped_for_chat_upstreams() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("tool normalization reply")).await;
    let (_db, config, gateway_task, base_url) =
        spawn_gateway(vec![provider("tools", upstream_url, Dialect::OpenAI, 1)]).await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/responses"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({
            "model": "integration-model",
            "input": "hello",
            "tools": [
                {
                    "type": "namespace",
                    "name": "multi_agent_v1",
                    "tools": [{
                        "type": "function",
                        "name": "spawn_agent",
                        "description": "spawn",
                        "parameters": { "type": "object" }
                    }]
                },
                { "type": "web_search", "external_web_access": false },
                {
                    "type": "function",
                    "name": "plain_tool",
                    "description": "plain",
                    "parameters": { "type": "object" }
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let upstream = state.requests.lock().unwrap().last().cloned().unwrap();
    let tools = upstream["tools"]
        .as_array()
        .expect("tools must be forwarded");
    let names = tools
        .iter()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .and_then(|name| name.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["lgw__multi_agent_v1__spawn_agent", "plain_tool"]
    );
    assert!(tools
        .iter()
        .all(|tool| tool["type"].as_str() == Some("function")));

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn cached_input_tokens_use_cache_prices_in_audit_cost() {
    use llm_gateway_lib::domain::{Currency, ModelPrice, PriceSource};

    let (upstream_url, _state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::CompleteWithCachedUsage("cached reply")).await;
    let mut priced = provider("cache-priced", upstream_url, Dialect::OpenAI, 1);
    priced.models[0].price = Some(ModelPrice {
        prompt: 1.0,
        completion: 2.0,
        cache_read: Some(0.1),
        cache_creation: Some(3.0),
        currency: Currency::Usd,
        tiers: Vec::new(),
        rules: Vec::new(),
        source: PriceSource::Manual,
    });
    let (db, config, gateway_task, base_url) = spawn_gateway(vec![priced]).await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut logged = None;
    for _ in 0..80 {
        let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
        if let Some(row) = rows
            .into_iter()
            .find(|row| row["routed_provider"] == "cache-priced")
        {
            logged = Some(row);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let logged = logged.expect("缓存用量请求必须写入审计");
    let expected = (1.0 * 1.0 + 8.0 * 0.1 + 1.0 * 3.0 + 2.0 * 2.0) / 1_000_000.0;
    let cost = logged["cost"].as_f64().expect("缓存价必须参与花费估算");
    assert!((cost - expected).abs() < 1e-12, "实际成本 {cost}");

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
#[ignore = "requires the local Codex CLI"]
async fn codex_cli_can_use_gateway_as_responses_proxy() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("proxy takeover ok")).await;
    let mut codex_provider = provider("codex-proxy", upstream_url, Dialect::OpenAI, 1);
    codex_provider.models[0].context_window = 0;
    codex_provider.models[0].supports_vision = true;
    codex_provider.models[0].supports_audio = true;
    codex_provider.models[0].supports_video = true;
    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![codex_provider]).await;

    let root =
        std::env::temp_dir().join(format!("llm-gateway-codex-proxy-{}", uuid::Uuid::new_v4()));
    let codex_home = root.join("codex-home");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"model = "integration-model"
model_provider = "llm_gateway"
approval_policy = "never"
sandbox_mode = "read-only"

[model_providers.llm_gateway]
name = "LLM Gateway"
base_url = "{base_url}/v1"
wire_api = "responses"
experimental_bearer_token = "{}"
requires_openai_auth = false

[features]
plugins = false
recommended_plugins = false
"#,
            config.unified_key
        ),
    )
    .unwrap();

    let codex = std::env::var("CODEX_CLI_PATH").unwrap_or_else(|_| "codex".into());
    let output = tokio::process::Command::new(codex)
        .args([
            "exec",
            "--skip-git-repo-check",
            "--ephemeral",
            "--color",
            "never",
            "-C",
        ])
        .arg(&root)
        .arg("Reply with exactly: proxy takeover ok")
        .env("CODEX_HOME", &codex_home)
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        output.status.success(),
        "Codex CLI failed (upstream_hits={}): {combined}",
        state.hits.load(Ordering::SeqCst)
    );
    assert!(
        combined.contains("proxy takeover ok"),
        "Codex CLI did not receive the gateway response: {combined}"
    );
    assert!(
        state.hits.load(Ordering::SeqCst) > 0,
        "gateway must have forwarded the Codex request to the mock upstream"
    );

    let _ = std::fs::remove_dir_all(&root);
    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn priced_requests_record_cost_and_attempt_chain_while_unpriced_requests_do_not() {
    use llm_gateway_lib::domain::{Currency, ModelPrice};

    let (bad_url, bad_state, bad_task) =
        spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::TOO_MANY_REQUESTS)).await;
    let (good_url, good_state, good_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("priced reply")).await;

    let mut primary = provider("priced-primary", bad_url, Dialect::OpenAI, 1);
    primary.models[0].price = Some(ModelPrice {
        prompt: 1_000_000.0,
        completion: 2_000_000.0,
        cache_read: None,
        cache_creation: None,
        currency: Currency::Usd,
        tiers: Vec::new(),
        rules: Vec::new(),
        source: llm_gateway_lib::domain::PriceSource::Manual,
    });
    let mut backup = provider("priced-backup", good_url, Dialect::OpenAI, 2);
    backup.models[0].price = Some(ModelPrice {
        prompt: 0.5,
        completion: 1.5,
        cache_read: None,
        cache_creation: None,
        currency: Currency::Cny,
        tiers: Vec::new(),
        rules: Vec::new(),
        source: llm_gateway_lib::domain::PriceSource::Manual,
    });

    let (db, config, gateway_task, base_url) = spawn_gateway(vec![primary, backup]).await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-routed-via"],
        "priced-backup/integration-model"
    );
    assert_eq!(bad_state.hits.load(Ordering::SeqCst), 1);
    assert_eq!(good_state.hits.load(Ordering::SeqCst), 1);

    // 审计写入是异步的；轮询等待目标记录出现。
    let mut logged = None;
    for _ in 0..80 {
        let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
        if let Some(row) = rows
            .into_iter()
            .find(|row| row["routed_provider"] == "priced-backup")
        {
            logged = Some(row);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let logged = logged.expect("实际路由到备用的请求必须写入审计");

    // 计价使用被真正路由到的模型价格（备份：人民币 0.5 / 1.5 每 100 万）。
    assert_eq!(logged["currency"], "cny");
    let expected = 3.0 / 1_000_000.0 * 0.5 + 2.0 / 1_000_000.0 * 1.5;
    let cost = logged["cost"].as_f64().expect("配置了价格就必须记录花费");
    assert!(
        (cost - expected).abs() < 1e-12,
        "花费应等于按 token 计算的 {expected}，实际 {cost}"
    );

    // 降级链逐跳：第一跳 429，第二跳成功。
    let attempts = logged["attempts"]
        .as_array()
        .expect("有降级的请求必须记录逐跳明细");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["provider_id"], "priced-primary");
    assert_eq!(attempts[0]["ok"], false);
    assert_eq!(attempts[0]["status"], 429);
    assert!(attempts[0]["reason"].as_str().unwrap().contains("429"));
    assert_eq!(attempts[1]["provider_id"], "priced-backup");
    assert_eq!(attempts[1]["ok"], true);

    // 未配置价格的模型不得伪造出 0 花费。
    let (plain_url, _plain_state, plain_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("unpriced reply")).await;
    let (_db2, config2, gateway_task2, base_url2) =
        spawn_gateway(vec![provider("unpriced", plain_url, Dialect::OpenAI, 1)]).await;
    let unpriced = reqwest::Client::new()
        .post(format!("{base_url2}/v1/chat/completions"))
        .bearer_auth(&config2.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(unpriced.status(), StatusCode::OK);

    gateway_task.abort();
    bad_task.abort();
    good_task.abort();
    gateway_task2.abort();
    plain_task.abort();
}

#[tokio::test]
async fn spend_tables_separate_currencies_and_report_unpriced_requests() {
    use llm_gateway_lib::db::repo::RequestLog;

    let db = db::Db::connect_in_memory().await.unwrap();
    for (provider, model, currency, cost, tokens) in [
        ("prov-a", "model-a", Some("usd"), Some(0.5), 10),
        ("prov-a", "model-a", Some("usd"), Some(0.25), 5),
        ("prov-b", "model-b", Some("cny"), Some(1.5), 20),
        ("prov-c", "model-c", None, None, 7),
    ] {
        repo::log_request(
            db.pool(),
            RequestLog {
                session_id: None,
                client: Some("local-unified-key"),
                access_key_id: llm_gateway_lib::budget::access_key_id_of(Some("local-unified-key")),
                requested_model: "auto",
                routed_provider: Some(provider),
                routed_model: Some(model),
                status: Some(200),
                latency_ms: 10,
                prompt_tokens: tokens,
                completion_tokens: 0,
                fallback_attempts: 0,
                error: None,
                cost,
                currency,
                rate_label: None,
                estimated_prompt_tokens: None,
                attempts_json: None,
                route: Default::default(),
            },
        )
        .await
        .unwrap();
    }

    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let buckets = repo::spend_buckets(db.pool(), &day).await.unwrap();
    let usd = buckets
        .iter()
        .find(|bucket| bucket.currency == "usd")
        .unwrap();
    let cny = buckets
        .iter()
        .find(|bucket| bucket.currency == "cny")
        .unwrap();
    assert_eq!(buckets.len(), 2, "币种必须分开汇总，不能相加");
    assert!((usd.cost - 0.75).abs() < 1e-9);
    assert_eq!(usd.requests, 2);
    assert!((cny.cost - 1.5).abs() < 1e-9);
    assert_eq!(cny.requests, 1);

    // 有 token 但没有价格的请求单独计数：它是「未知」，不是 0。
    assert_eq!(
        repo::unpriced_request_count(db.pool(), &day).await.unwrap(),
        1
    );

    let by_provider = repo::spend_by_dimension(db.pool(), &day, false)
        .await
        .unwrap();
    assert!(
        by_provider.iter().all(|row| row["provider_id"] != "prov-c"),
        "未计价供应商不得出现在花费表里"
    );
    let by_model = repo::spend_by_dimension(db.pool(), &day, true)
        .await
        .unwrap();
    assert_eq!(by_model.len(), 2);
}

#[tokio::test]
async fn time_rules_and_input_tiers_shape_the_recorded_cost_and_rate_label() {
    use llm_gateway_lib::domain::{Currency, ModelPrice, PriceRule, PriceTier};

    let (upstream_url, _state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("peak reply")).await;
    let mut primary = provider("peak-priced", upstream_url, Dialect::OpenAI, 1);
    primary.models[0].price = Some(ModelPrice {
        prompt: 2.0,
        completion: 8.0,
        cache_read: None,
        cache_creation: None,
        currency: Currency::Usd,
        tiers: vec![PriceTier {
            // 阈值 0 与基础档重复，不参与档位选择，也不应污染 rate_label。
            min_prompt_tokens: 0,
            prompt: 2.0,
            completion: 8.0,
            cache_read: None,
            cache_creation: None,
        }],
        // 起止相同的规则按「全天生效」处理：断言不依赖运行时刻。
        rules: vec![PriceRule {
            label: "谷时".into(),
            start_minute: 30,
            end_minute: 30,
            prompt_multiplier: 0.5,
            completion_multiplier: 0.25,
        }],
        source: llm_gateway_lib::domain::PriceSource::Manual,
    });

    let (db, config, gateway_task, base_url) = spawn_gateway(vec![primary]).await;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut logged = None;
    for _ in 0..80 {
        let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
        if let Some(row) = rows
            .into_iter()
            .find(|row| row["routed_provider"] == "peak-priced")
        {
            logged = Some(row);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let logged = logged.expect("成功请求必须写入审计");

    // 上游 mock 返回 prompt=3 / completion=2；基础档 2.0 / 8.0 再乘时段倍率。
    let expected = 3.0 / 1_000_000.0 * 2.0 * 0.5 + 2.0 / 1_000_000.0 * 8.0 * 0.25;
    let cost = logged["cost"].as_f64().expect("配置了价格必须记录花费");
    assert!(
        (cost - expected).abs() < 1e-12,
        "时段价计算错误：期望 {expected}，实际 {cost}"
    );
    assert_eq!(logged["rate_label"], "谷时");
    // 估算值必须落库，否则无法从日志做校准。
    assert!(logged["estimated_prompt_tokens"].as_i64().unwrap_or(0) > 0);

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn successful_requests_calibrate_the_local_estimate_and_session_ratio() {
    let (upstream_url, _state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("calibrated reply")).await;
    let (db, config, gateway_task, base_url) = spawn_gateway(vec![provider(
        "calibrating",
        upstream_url,
        Dialect::OpenAI,
        1,
    )])
    .await;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session_id = response.headers()["x-session-id"]
        .to_str()
        .unwrap()
        .to_owned();

    let mut calibration = None;
    for _ in 0..80 {
        let rows = repo::list_calibrations(db.pool()).await.unwrap();
        if let Some(row) = rows
            .into_iter()
            .find(|row| row.model == "integration-model")
        {
            calibration = Some(row);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let calibration = calibration.expect("成功请求必须留下校准样本");
    assert_eq!(calibration.provider_id, "calibrating");
    assert_eq!(calibration.samples, 1);
    // 上游 mock 只报告 3 个 prompt token，估算值明显更大，比值会被截断到区间下界。
    assert!(
        (0.5..=3.0).contains(&calibration.ratio),
        "校准比值必须在合理区间内，实际 {}",
        calibration.ratio
    );

    let session = repo::get_or_create_session(db.pool(), &session_id)
        .await
        .unwrap();
    assert_eq!(
        session.token_ratio,
        Some(calibration.ratio),
        "会话必须记录本次校准比值，供压缩阈值换算"
    );

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn multimodal_requests_route_only_to_capable_models() {
    let (upstream_url, state, upstream_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("vision reply")).await;
    let mut text_only = provider(
        "text-only",
        "http://127.0.0.1:1/v1".to_string(),
        Dialect::OpenAI,
        1,
    );
    text_only.models[0].supports_vision = false;
    let mut vision = provider("vision-capable", upstream_url, Dialect::OpenAI, 2);
    vision.models[0].supports_vision = true;

    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![text_only, vision]).await;
    let client = reqwest::Client::new();
    let image_body = serde_json::json!({
        "model": "integration-model",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "这张图里是什么？" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
            ],
        }],
    });

    let routed = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .json(&image_body)
        .send()
        .await
        .unwrap();
    assert_eq!(routed.status(), StatusCode::OK);
    assert_eq!(
        routed.headers()["x-routed-via"],
        "vision-capable/integration-model",
        "带图片的请求必须绕开纯文本模型"
    );
    // 视觉模型确实收到了图片内容，而不是被丢掉的空消息。
    let upstream_request = state.requests.lock().unwrap().last().cloned().unwrap();
    let parts = upstream_request["messages"][0]["content"]
        .as_array()
        .expect("多模态消息应保持 parts 结构");
    assert!(parts.iter().any(|part| part["type"] == "image_url"));

    // Gemini 方言承载不了远程图片：候选必须被剔除，并给出可操作的原因。
    let (gemini_url, gemini_state, gemini_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("must not be used")).await;
    let mut gemini = provider("gemini-native", gemini_url, Dialect::Gemini, 1);
    gemini.models[0].supports_vision = true;
    let (_db2, config2, gateway_task2, base_url2) = spawn_gateway(vec![gemini]).await;
    let remote_image = serde_json::json!({
        "model": "integration-model",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "看图" },
                { "type": "image_url", "image_url": { "url": "https://example.test/a.png" } },
            ],
        }],
    });
    let blocked = reqwest::Client::new()
        .post(format!("{base_url2}/v1/chat/completions"))
        .bearer_auth(&config2.unified_key)
        .json(&remote_image)
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::BAD_REQUEST);
    let blocked: serde_json::Value = blocked.json().await.unwrap();
    assert_eq!(blocked["error"]["type"], "model_capability_unavailable");
    let message = blocked["error"]["message"].as_str().unwrap();
    assert!(message.contains("base64"), "错误必须说明原因：{message}");
    assert_eq!(
        gemini_state.hits.load(Ordering::SeqCst),
        0,
        "承载不了的请求不得发给上游"
    );

    // 音频请求没有任何支持音频的候选时，同样是明确的能力错误而不是静默丢弃。
    let mut audio_model = provider(
        "no-audio",
        "http://127.0.0.1:1/v1".to_string(),
        Dialect::OpenAI,
        1,
    );
    audio_model.models[0].supports_audio = false;
    let (_db3, config3, gateway_task3, base_url3) = spawn_gateway(vec![audio_model]).await;
    let audio_body = serde_json::json!({
        "model": "integration-model",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "转写" },
                { "type": "input_audio", "input_audio": { "data": "AAAA", "format": "wav" } },
            ],
        }],
    });
    let audio = reqwest::Client::new()
        .post(format!("{base_url3}/v1/chat/completions"))
        .bearer_auth(&config3.unified_key)
        .json(&audio_body)
        .send()
        .await
        .unwrap();
    assert_eq!(audio.status(), StatusCode::BAD_REQUEST);
    let audio: serde_json::Value = audio.json().await.unwrap();
    assert_eq!(audio["error"]["type"], "model_capability_unavailable");
    assert!(audio["error"]["message"].as_str().unwrap().contains("音频"));

    gateway_task.abort();
    upstream_task.abort();
    gemini_task.abort();
    gateway_task2.abort();
    gateway_task3.abort();
}
