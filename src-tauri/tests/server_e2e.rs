use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    hits: Arc<AtomicUsize>,
}

#[derive(Clone)]
enum OpenAiBehavior {
    Complete(&'static str),
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
            alias: "integration-model".into(),
            upstream: "integration-model".into(),
            context_window: 16_384,
            supports_tools: true,
            supports_vision: false,
            supports_stream: true,
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
    mock.state.requests.lock().unwrap().push(request);

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

async fn spawn_openai_upstream(behavior: OpenAiBehavior) -> (String, MockState, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = MockState::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_openai_chat))
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

async fn spawn_gateway(providers: Vec<Provider>) -> (db::Db, AppConfig, JoinHandle<()>, String) {
    let db = db::Db::connect_in_memory().await.unwrap();
    for provider in providers {
        repo::upsert_provider(db.pool(), &provider).await.unwrap();
    }
    let config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: "e2e-gateway-key".into(),
        ..Default::default()
    };
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

#[tokio::test]
async fn upstream_auth_failure_is_returned_without_trying_the_backup() {
    let (bad_url, bad_state, bad_task) =
        spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::UNAUTHORIZED)).await;
    let (good_url, good_state, good_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("must not be used")).await;
    let (_db, config, gateway_task, base_url) = spawn_gateway(vec![
        provider("invalid-key", bad_url, Dialect::OpenAI, 1),
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
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(bad_state.hits.load(Ordering::SeqCst), 1);
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
