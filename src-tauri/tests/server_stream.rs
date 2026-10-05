use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::config::AppConfig;
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone, Default)]
struct MockState {
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[derive(Clone, Default)]
struct RateLimitedState {
    hits: Arc<AtomicUsize>,
}

fn mock_provider(base_url: String) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: "stream-mock".into(),
        name: "stream mock".into(),
        dialect: Dialect::OpenAI,
        base_url,
        api_key_enc: String::new(),
        enabled: true,
        priority: 1,
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

async fn mock_chat(
    State(state): State<MockState>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    state.requests.lock().unwrap().push(request.clone());
    if request
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        let with_tools = request.get("tools").is_some();
        let body = Body::from_stream(async_stream::stream! {
            if with_tools {
                yield Ok::<Bytes, Infallible>(Bytes::from_static(
                    b"data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"before tool\"}}]}\n\n",
                ));
                yield Ok::<Bytes, Infallible>(Bytes::from_static(
                    b"data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_lookup\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":\\\"a\\\"}\"}}]}}]}\n\n",
                ));
                yield Ok::<Bytes, Infallible>(Bytes::from_static(
                    b"data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
                ));
            } else {
                yield Ok::<Bytes, Infallible>(Bytes::from_static(
                    b"data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hel\"}}]}\n\n",
                ));
                yield Ok::<Bytes, Infallible>(Bytes::from_static(
                    b"data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
                ));
            }
            yield Ok::<Bytes, Infallible>(Bytes::from_static(
                b"data: {\"id\":\"chatcmpl-stream\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
            ));
            yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: [DONE]\n\n"));
        });
        return Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/event-stream")
            .body(body)
            .unwrap();
    }

    if request.get("tools").is_some() {
        return Json(serde_json::json!({
            "id": "chatcmpl-tool-normal",
            "object": "chat.completion",
            "model": "integration-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_lookup",
                        "type": "function",
                        "function": { "name": "lookup", "arguments": "{\"q\":\"a\"}" },
                    }],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": { "prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5 },
        }))
        .into_response();
    }

    Json(serde_json::json!({
        "id": "chatcmpl-normal",
        "object": "chat.completion",
        "model": "integration-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "stored reply" },
            "finish_reason": "stop",
        }],
        "usage": { "prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5 },
    }))
    .into_response()
}

async fn spawn_mock_upstream() -> (String, MockState, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = MockState::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_chat))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), state, task)
}

async fn mock_truncated_stream() -> Response {
    let body = Body::from_stream(async_stream::stream! {
        yield Ok::<Bytes, Infallible>(Bytes::from_static(
            b"data: {\"id\":\"chatcmpl-truncated\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial answer\"}}]}\n\n",
        ));
        // 故意不发送 [DONE]，模拟上游在首个 delta 后异常关闭连接。
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .body(body)
        .unwrap()
}

async fn spawn_truncated_stream_upstream() -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route("/v1/chat/completions", post(mock_truncated_stream));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), task)
}

async fn mock_rate_limited(State(state): State<RateLimitedState>) -> Response {
    state.hits.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({
            "error": { "message": "rate limited", "type": "rate_limit" },
        })),
    )
        .into_response()
}

async fn spawn_rate_limited_upstream() -> (String, RateLimitedState, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = RateLimitedState::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_rate_limited))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), state, task)
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

async fn spawn_gateway_with_providers(
    providers: Vec<Provider>,
) -> (db::Db, AppConfig, JoinHandle<()>, String) {
    let config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: "stream-test-key".into(),
        ..Default::default()
    };
    spawn_gateway_with_config(providers, config).await
}

async fn spawn_gateway_with_config(
    providers: Vec<Provider>,
    config: AppConfig,
) -> (db::Db, AppConfig, JoinHandle<()>, String) {
    let db = db::Db::connect_in_memory().await.unwrap();
    for provider in providers {
        repo::upsert_provider(db.pool(), &provider).await.unwrap();
    }
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

async fn spawn_gateway(upstream_url: String) -> (db::Db, AppConfig, JoinHandle<()>, String) {
    spawn_gateway_with_providers(vec![mock_provider(upstream_url)]).await
}

fn openai_stream_body(message: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "integration-model",
        "stream": true,
        "messages": [{ "role": "user", "content": message }],
    })
}

fn parse_sse_events(body: &str) -> Vec<(String, serde_json::Value)> {
    body.split("\n\n")
        .filter_map(|frame| {
            let event = frame
                .lines()
                .find_map(|line| line.strip_prefix("event: "))?
                .to_owned();
            let data = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .collect::<String>();
            Some((event, serde_json::from_str(&data).unwrap()))
        })
        .collect()
}

#[tokio::test]
async fn streaming_openai_has_role_usage_and_ollama_is_real_ndjson() {
    let (upstream_url, _mock_state, upstream_task) = spawn_mock_upstream().await;
    let (db, config, gateway_task, base_url) = spawn_gateway(upstream_url).await;
    let client = reqwest::Client::new();

    let openai = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", "stream-openai")
        .json(&openai_stream_body("hello"))
        .send()
        .await
        .unwrap();
    assert_eq!(openai.status(), StatusCode::OK);
    assert!(openai
        .headers()
        .get(CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let openai_text = openai.text().await.unwrap();
    let frames: Vec<&str> = openai_text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect();
    let first: serde_json::Value = serde_json::from_str(frames[0]).unwrap();
    assert_eq!(first["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(first["choices"][0]["delta"]["content"], "hel");
    assert_eq!(frames.last().copied(), Some("[DONE]"));

    // `.text()` 返回时已收到最终 [DONE]。上下文和 usage 必须已经同步写入，
    // 不能依赖 sleep 等待后台任务。
    let stored = repo::recent_messages(db.pool(), "stream-openai", 10)
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].prompt_tokens, 3);
    assert_eq!(stored[1].completion_tokens, 2);
    assert_eq!(stored[1].content, "hello");

    let ollama = client
        .post(format!("{base_url}/api/chat"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", "stream-ollama")
        .json(&serde_json::json!({
            "model": "integration-model",
            "stream": true,
            "messages": [{ "role": "user", "content": "hello" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ollama.status(), StatusCode::OK);
    assert!(ollama
        .headers()
        .get(CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("application/x-ndjson"));
    let ollama_text = ollama.text().await.unwrap();
    assert!(!ollama_text.contains("data:"));
    let lines: Vec<serde_json::Value> = ollama_text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(lines[0]["message"]["role"], "assistant");
    assert_eq!(lines[0]["message"]["content"], "hel");
    assert_eq!(lines.last().unwrap()["done"], true);

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn truncated_stream_is_reported_as_failure_without_a_false_done_or_sticky_route() {
    let (upstream_url, upstream_task) = spawn_truncated_stream_upstream().await;
    let (db, config, gateway_task, base_url) = spawn_gateway(upstream_url).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", "truncated-stream")
        .json(&openai_stream_body("请给出完整回答"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "流式头已发出时只能在流内报告错误"
    );
    let body = response.text().await.unwrap();
    assert!(body.contains("partial answer"));
    assert!(body.contains("error"));
    assert!(!body.contains("[DONE]"), "上游未完成时不能伪造完成帧");

    // 客户端已经看到的部分回答必须进入会话，避免下一轮上下文与客户端所见分叉；
    // 但它不是成功回答，不能提升成功率或写入 sticky 路由。
    let stored = repo::recent_messages(db.pool(), "truncated-stream", 10)
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].content, "partial answer");
    let session = repo::get_or_create_session(db.pool(), "truncated-stream")
        .await
        .unwrap();
    assert!(session.sticky_provider_id.is_none());

    let mut saw_failure_audit = false;
    for _ in 0..80 {
        if repo::recent_requests(db.pool(), 10)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry["status"] == StatusCode::BAD_GATEWAY.as_u16())
        {
            saw_failure_audit = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(saw_failure_audit, "截断流必须写入失败审计");

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn a_stream_that_crosses_the_threshold_is_compacted_before_the_next_turn() {
    let (upstream_url, _state, upstream_task) = spawn_mock_upstream().await;
    let config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: "stream-test-key".into(),
        compact_threshold_tokens: 1,
        compact_keep_recent: 1,
        ..Default::default()
    };
    let (db, config, gateway_task, base_url) =
        spawn_gateway_with_config(vec![mock_provider(upstream_url)], config).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", "stream-compaction")
        .json(&openai_stream_body("触发流式压缩"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.text().await.unwrap().contains("[DONE]"));

    // 响应后任务和下一请求前保护共享同一压缩锁；有界等待只用于观察后台任务，
    // 不依赖任意 sleep。
    let mut compacted = None;
    for _ in 0..80 {
        let session = repo::get_or_create_session(db.pool(), "stream-compaction")
            .await
            .unwrap();
        if session.summary.is_some() && session.compact_count > 0 {
            compacted = Some(session);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let compacted = compacted.expect("跨阈值的流式交换应在写入后触发自动压缩");
    assert!(compacted.summary.unwrap().contains("stored reply"));

    let follow_up = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", "stream-compaction")
        .json(&serde_json::json!({
            "model": "integration-model",
            "messages": [{ "role": "user", "content": "下一问" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(follow_up.status(), StatusCode::OK);

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn immediate_follow_up_reads_synchronously_persisted_context() {
    let (upstream_url, mock_state, upstream_task) = spawn_mock_upstream().await;
    let (_db, config, gateway_task, base_url) = spawn_gateway(upstream_url).await;
    let client = reqwest::Client::new();

    for message in ["first question", "second question"] {
        let response = client
            .post(format!("{base_url}/v1/chat/completions"))
            .bearer_auth(&config.unified_key)
            .header("x-session-id", "immediate-follow-up")
            .json(&serde_json::json!({
                "model": "integration-model",
                "messages": [{ "role": "user", "content": message }],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let requests = mock_state.requests.lock().unwrap();
    let normal: Vec<&serde_json::Value> = requests
        .iter()
        .filter(|request| request["stream"] == serde_json::Value::Bool(false))
        .collect();
    assert_eq!(normal.len(), 2);
    let messages = normal[1]["messages"].as_array().unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["user", "assistant", "user"]
    );
    assert_eq!(messages[0]["content"], "first question");
    assert_eq!(messages[1]["content"], "stored reply");
    assert_eq!(messages[2]["content"], "second question");

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn rate_limit_fails_over_to_backup_and_records_the_successful_route() {
    let (primary_url, primary_state, primary_task) = spawn_rate_limited_upstream().await;
    let (backup_url, backup_state, backup_task) = spawn_mock_upstream().await;
    let mut primary = mock_provider(primary_url);
    primary.id = "primary".into();
    primary.name = "primary".into();
    primary.priority = 1;
    let mut backup = mock_provider(backup_url);
    backup.id = "backup".into();
    backup.name = "backup".into();
    backup.priority = 2;
    let (db, config, gateway_task, base_url) =
        spawn_gateway_with_providers(vec![primary, backup]).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", "failover-session")
        .json(&serde_json::json!({
            "model": "integration-model",
            "messages": [{ "role": "user", "content": "please retry" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("x-routed-via").unwrap(),
        "backup/integration-model"
    );
    assert_eq!(response.headers().get("x-fallback-attempts").unwrap(), "2");
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["choices"][0]["message"]["content"], "stored reply");
    assert_eq!(primary_state.hits.load(Ordering::SeqCst), 1);
    assert_eq!(backup_state.requests.lock().unwrap().len(), 1);

    let session = repo::get_or_create_session(db.pool(), "failover-session")
        .await
        .unwrap();
    assert_eq!(session.sticky_provider_id.as_deref(), Some("backup"));
    assert_eq!(session.sticky_model.as_deref(), Some("integration-model"));

    // 审计写入是刻意异步的，不能靠任意 sleep 猜测；有界等待直到该记录可见。
    let mut audit = None;
    for _ in 0..80 {
        let records = repo::recent_requests(db.pool(), 10).await.unwrap();
        if let Some(record) = records
            .into_iter()
            .find(|record| record["routed_provider"] == "backup")
        {
            audit = Some(record);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let audit = audit.expect("成功响应必须最终写入审计日志");
    assert_eq!(audit["routed_model"], "integration-model");
    assert_eq!(audit["status"], 200);
    assert_eq!(audit["fallback_attempts"], 1);

    gateway_task.abort();
    primary_task.abort();
    backup_task.abort();
}

#[tokio::test]
async fn a_small_context_backup_never_receives_an_oversized_history() {
    let (primary_url, primary_state, primary_task) = spawn_rate_limited_upstream().await;
    let (backup_url, backup_state, backup_task) = spawn_mock_upstream().await;
    let mut primary = mock_provider(primary_url);
    primary.id = "large-window-primary".into();
    primary.name = "large-window-primary".into();
    primary.models[0].context_window = 4_096;
    primary.priority = 1;
    let mut backup = mock_provider(backup_url);
    backup.id = "small-window-backup".into();
    backup.name = "small-window-backup".into();
    backup.models[0].context_window = 300;
    backup.priority = 2;
    let (_db, config, gateway_task, base_url) =
        spawn_gateway_with_providers(vec![primary, backup]).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", "small-context-backup")
        .json(&serde_json::json!({
            "model": "integration-model",
            "max_tokens": 10,
            "messages": [{ "role": "user", "content": "长".repeat(1200) }],
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(primary_state.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        backup_state.requests.lock().unwrap().len(),
        0,
        "窗口不足的备选 Provider 不得收到原始超窗请求"
    );

    gateway_task.abort();
    primary_task.abort();
    backup_task.abort();
}

#[tokio::test]
async fn non_stream_responses_preserve_function_calls() {
    let (upstream_url, mock_state, upstream_task) = spawn_mock_upstream().await;
    let (_db, config, gateway_task, base_url) = spawn_gateway(upstream_url).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base_url}/v1/responses"))
        .bearer_auth(&config.unified_key)
        .json(&serde_json::json!({
            "model": "integration-model",
            "input": "look this up",
            "tools": [{
                "type": "function",
                "name": "lookup",
                "parameters": { "type": "object" },
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload: serde_json::Value = response.json().await.unwrap();
    assert_eq!(payload["object"], "response");
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["output"].as_array().unwrap().len(), 1);
    assert_eq!(payload["output"][0]["type"], "function_call");
    assert_eq!(payload["output"][0]["status"], "completed");
    assert_eq!(payload["output"][0]["call_id"], "call_lookup");
    assert_eq!(payload["output"][0]["name"], "lookup");
    assert_eq!(payload["output"][0]["arguments"], "{\"q\":\"a\"}");
    assert_eq!(payload["usage"]["input_tokens"], 2);
    assert_eq!(payload["usage"]["output_tokens"], 3);
    assert_eq!(payload["usage"]["total_tokens"], 5);
    assert!(payload["usage"].get("prompt_tokens").is_none());

    let upstream_requests = mock_state.requests.lock().unwrap();
    let upstream = upstream_requests.last().unwrap();
    assert_eq!(upstream["messages"][0]["content"], "look this up");
    assert_eq!(upstream["tools"][0]["function"]["name"], "lookup");

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn anthropic_and_responses_streams_emit_complete_item_lifecycles() {
    let (upstream_url, mock_state, upstream_task) = spawn_mock_upstream().await;
    let (_db, config, gateway_task, base_url) = spawn_gateway(upstream_url).await;
    let client = reqwest::Client::new();
    let tools = serde_json::json!([{
        "type": "function",
        "function": {
            "name": "lookup",
            "parameters": { "type": "object" },
        },
    }]);

    let anthropic = client
        .post(format!("{base_url}/v1/messages"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", "stream-anthropic")
        .json(&serde_json::json!({
            "model": "integration-model",
            "stream": true,
            "max_tokens": 128,
            "tools": tools,
            "messages": [{ "role": "user", "content": "use a tool" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(anthropic.status(), StatusCode::OK);
    let anthropic_events = parse_sse_events(&anthropic.text().await.unwrap());
    assert_eq!(
        anthropic_events
            .iter()
            .map(|(event, _)| event.as_str())
            .collect::<Vec<_>>(),
        [
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    let blocks: Vec<&serde_json::Value> = anthropic_events
        .iter()
        .filter(|(event, _)| event == "content_block_start")
        .map(|(_, data)| data)
        .collect();
    assert_eq!(blocks[0]["index"], 0);
    assert_eq!(blocks[0]["content_block"]["type"], "text");
    assert_eq!(blocks[1]["index"], 1);
    assert_eq!(blocks[1]["content_block"]["type"], "tool_use");
    assert_eq!(blocks[1]["content_block"]["id"], "call_lookup");

    let responses = client
        .post(format!("{base_url}/v1/responses"))
        .bearer_auth(&config.unified_key)
        .header("x-session-id", "stream-responses")
        .json(&serde_json::json!({
            "model": "integration-model",
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "use a tool" }],
            }],
            "tools": [{
                "type": "function",
                "name": "lookup",
                "parameters": { "type": "object" },
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(responses.status(), StatusCode::OK);
    let response_events = parse_sse_events(&responses.text().await.unwrap());
    let names: Vec<&str> = response_events
        .iter()
        .map(|(event, _)| event.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "response.created",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    let function_done = response_events
        .iter()
        .find(|(event, _)| event == "response.function_call_arguments.done")
        .map(|(_, data)| data)
        .unwrap();
    assert_eq!(function_done["call_id"], "call_lookup");
    assert_eq!(function_done["name"], "lookup");
    assert_eq!(function_done["arguments"], "{\"q\":\"a\"}");

    let upstream_requests = mock_state.requests.lock().unwrap();
    let responses_upstream = upstream_requests
        .iter()
        .rfind(|request| request["stream"] == serde_json::Value::Bool(true))
        .unwrap();
    assert_eq!(responses_upstream["messages"][0]["content"], "use a tool");
    assert_eq!(responses_upstream["tools"][0]["function"]["name"], "lookup");

    gateway_task.abort();
    upstream_task.abort();
}
