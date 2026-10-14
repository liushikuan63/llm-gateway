//! Full gateway dispatch through a local WorkBuddy protocol fixture; no vendor requests.
use crate::{
    config::AppConfig,
    db::{repo, Db},
    domain::{AgentRuntime, Dialect, Provider},
    proxy::{
        server::{serve, GatewayState},
        workbuddy::WorkBuddyModelClient,
    },
    workbuddy_auth::WorkBuddyAuth,
};
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Clone, Default)]
struct Fixture {
    requests: Arc<parking_lot::Mutex<Vec<Value>>>,
    posts: Arc<AtomicUsize>,
}
struct Servers {
    gateway: Arc<GatewayState>,
    url: String,
    fixture: Fixture,
    jobs: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Servers {
    fn drop(&mut self) {
        for job in &self.jobs {
            job.abort();
        }
    }
}
async fn catalog() -> Json<Value> {
    Json(
        json!({"code":0,"data":{"models":[{"id":"fixture-model","maxInputTokens":65536,"maxOutputTokens":256,"supportsToolCall":true,"supportsImages":true}]}}),
    )
}
async fn completion(
    State(f): State<Fixture>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    assert_eq!(headers["authorization"], "Bearer offline-gateway-access");
    assert_eq!(headers["x-user-id"], "777");
    assert!(!headers.contains_key("x-refresh-token"));
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["messages"][0]["role"], "system",
        "the real international API requires a system head"
    );
    let tools = body.get("tools").is_some();
    f.posts.fetch_add(1, Ordering::SeqCst);
    f.requests.lock().push(body);
    if tools {
        let frames = [
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_lookup","type":"function","function":{"name":"look","arguments":"{\"q\":\""}}]}}]}),
            json!({"choices":[{"delta":{"content":"between tools","reasoning_content":"vendor thought"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"up","arguments":"foo\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13}}),
        ];
        let mut text = frames
            .into_iter()
            .map(|value| format!("data: {value}\n\n"))
            .collect::<String>();
        text.push_str("data: [DONE]\n\n");
        return (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            text,
        );
    }
    ([(axum::http::header::CONTENT_TYPE,"text/event-stream")],
     "data: {\"id\":\"fixture-answer\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"vendor reasoning\",\"content\":\"answer\"},\"finish_reason\":\"\"}]}\n\n\
      data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
      data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":3,\"total_tokens\":13,\"completion_tokens_details\":{\"reasoning_tokens\":1},\"prompt_tokens_details\":{\"cached_tokens\":4},\"credit\":0}}\n\n\
      data: [DONE]\n\n".to_owned())
}
async fn setup() -> Servers {
    use base64::Engine;
    if std::env::var_os("LLMGW_MASTER_KEY").is_none() {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            base64::engine::general_purpose::STANDARD.encode([73; 32]),
        );
    }
    let db = Db::connect_in_memory().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let fixture = Fixture::default();
    let upstream = Router::new()
        .route("/v3/config", get(catalog))
        .route("/v2/chat/completions", post(completion))
        .with_state(fixture.clone());
    let mock = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let auth = WorkBuddyAuth::with_test_origin(db.pool().clone(), origin.clone());
    auth.test_seed_account(
        "gateway-fixture",
        "777",
        "offline-gateway-access",
        "offline-gateway-refresh",
    )
    .await;
    let mut runtime = AgentRuntime::new("wb-gateway", "workbuddy-model", "WB fixture");
    runtime.options =
        Some(json!({"account_id":"gateway-fixture","model_aliases":{"saved-id":"fixture-model"}}));
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let provider=Provider {id:"wb-gateway-provider".into(),name:"WB gateway fixture".into(),dialect:Dialect::OpenAI,
        // This must never be used by account runtime dispatch.
        base_url:"http://127.0.0.1:1/placeholder".into(),api_key_enc:String::new(),enabled:true,priority:1,rpm_limit:0,intelligence:50,
        models:vec![serde_json::from_value(json!({"alias":"public","upstream":"saved-id","context_window":65536,
           "supports_tools":true,"supports_vision":true,"supports_stream":true,"supports_thinking":true,
           "overrides":{"temperature":0.2,"max_tokens":32}})).unwrap()],runtime_id:Some(runtime.id),note:None,created_at:now,updated_at:now};
    repo::upsert_provider(db.pool(), &provider).await.unwrap();
    let cfg = AppConfig {
        port: 0,
        unified_key: "offline-gateway-key".into(),
        http_proxy: None,
        ..Default::default()
    };
    let mut gateway = GatewayState::new(db, cfg);
    gateway.workbuddy_auth = Ok(auth.clone());
    gateway.workbuddy = Ok(Arc::new(WorkBuddyModelClient::with_test_origin(
        auth, origin,
    )));
    let gateway = Arc::new(gateway);
    gateway.reload_providers().await.unwrap();
    let for_task = gateway.clone();
    let server = tokio::spawn(async move {
        serve(for_task).await.unwrap();
    });
    let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(snapshot) = gateway.listener_snapshot() {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    Servers {
        gateway,
        url: format!("http://{}", snapshot.bound_addr),
        fixture,
        jobs: vec![mock, server],
    }
}
#[tokio::test]
async fn independent_account_gateway_preserves_protocols_usage_and_history() {
    let s = setup().await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for stream in [false, true] {
        let response=http.post(format!("{}/v1/chat/completions",s.url)).bearer_auth("offline-gateway-key")
            .json(&json!({"model":"public","stream":stream,"messages":[{"role":"system","content":"system context"},{"role":"user","content":"question"}],"temperature":0.8,"max_tokens":64})).send().await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.text().await.unwrap();
        if stream {
            assert!(body.contains("reasoning_content"));
            assert!(body.contains("vendor reasoning"));
            assert!(body.contains("answer"));
            assert_eq!(body.matches("data: [DONE]").count(), 1);
            assert!(body.contains("reasoning_tokens"));
        } else {
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["choices"][0]["message"]["content"], "answer");
            assert_eq!(
                body["choices"][0]["message"]["reasoning_content"],
                "vendor reasoning"
            );
            assert_eq!(body["usage"]["prompt_tokens"], 10);
            assert_eq!(body["usage"]["credit"].as_f64(), Some(0.0));
        }
    }
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 2);
    for request in s.fixture.requests.lock().iter() {
        assert_eq!(request["model"], "fixture-model");
        assert_eq!(request["messages"][0]["content"], "system context");
        assert!((request["temperature"].as_f64().unwrap() - 0.2).abs() < 0.000001);
        assert_eq!(request["max_tokens"], 32);
    }
    tokio::time::timeout(Duration::from_secs(5),async {loop {
        let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE status=200 AND prompt_tokens=10 AND completion_tokens=3").fetch_one(s.gateway.db.pool()).await.unwrap();
        if count==2 {break;} tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.unwrap();
    let history: String =
        sqlx::query_scalar("SELECT group_concat(content,'|') FROM session_messages")
            .fetch_one(s.gateway.db.pool())
            .await
            .unwrap();
    assert!(history.contains("question") && history.contains("answer"));
    assert!(!history.contains("vendor reasoning"));
}

#[tokio::test]
async fn disabled_direct_account_does_not_call_http_or_cli() {
    let s = setup().await;
    let mut runtime = repo::get_agent_runtime(s.gateway.db.pool(), "wb-gateway")
        .await
        .unwrap()
        .unwrap();
    runtime.enabled = false;
    repo::upsert_agent_runtime(s.gateway.db.pool(), &runtime)
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for stream in [false, true] {
        let response=http.post(format!("{}/v1/chat/completions",s.url)).bearer_auth("offline-gateway-key")
            .json(&json!({"model":"public","stream":stream,"messages":[{"role":"user","content":"question"}]})).send().await.unwrap();
        assert!(!response.status().is_success());
    }
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fragmented_tool_names_and_arguments_survive_all_five_gateway_streams() {
    let s = setup().await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let function = json!({"name":"lookup","parameters":{"type":"object","properties":{"q":{"type":"string"}}}});
    let cases = [
        (
            "openai",
            "/v1/chat/completions",
            json!({"model":"public","stream":true,"messages":[{"role":"user","content":"call"}],"tools":[{"type":"function","function":function}]}),
        ),
        (
            "anthropic",
            "/v1/messages",
            json!({"model":"public","stream":true,"max_tokens":32,"messages":[{"role":"user","content":"call"}],"tools":[{"name":"lookup","input_schema":function["parameters"]}]}),
        ),
        (
            "responses",
            "/v1/responses",
            json!({"model":"public","stream":true,"input":"call","tools":[{"type":"function","name":"lookup","parameters":function["parameters"]}]}),
        ),
        (
            "ollama",
            "/api/chat",
            json!({"model":"public","stream":true,"messages":[{"role":"user","content":"call"}],"tools":[{"type":"function","function":function}]}),
        ),
        (
            "gemini",
            "/v1beta/models/public:streamGenerateContent?alt=sse",
            json!({"contents":[{"role":"user","parts":[{"text":"call"}]}],"tools":[{"functionDeclarations":[function]}]}),
        ),
    ];
    for (protocol, path, body) in cases {
        let response = http
            .post(format!("{}{path}", s.url))
            .bearer_auth("offline-gateway-key")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK, "{protocol}");
        let wire = response.text().await.unwrap();
        let frames: Vec<Value> = wire
            .lines()
            .filter_map(|line| {
                let line = if protocol == "ollama" {
                    line
                } else {
                    line.strip_prefix("data: ")?
                };
                serde_json::from_str(line).ok()
            })
            .collect();
        match protocol {
            "gemini" => {
                let tools: Vec<&Value> = frames
                    .iter()
                    .flat_map(|v| {
                        v["candidates"][0]["content"]["parts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                    })
                    .filter_map(|v| v.get("functionCall"))
                    .collect();
                assert_eq!(tools.len(), 1, "Gemini wire: {wire}");
                assert_eq!(tools[0]["name"], "lookup");
                assert_eq!(tools[0]["args"], json!({"q":"foo"}));
            }
            "ollama" => {
                let tools: Vec<&Value> = frames
                    .iter()
                    .flat_map(|v| v["message"]["tool_calls"].as_array().into_iter().flatten())
                    .collect();
                assert_eq!(tools.len(), 1);
                assert_eq!(tools[0]["function"]["name"], "lookup");
                assert_eq!(tools[0]["function"]["arguments"], json!({"q":"foo"}));
            }
            "anthropic" => {
                let arguments: String = frames
                    .iter()
                    .filter_map(|v| v["delta"]["partial_json"].as_str())
                    .collect();
                assert_eq!(
                    serde_json::from_str::<Value>(&arguments).unwrap(),
                    json!({"q":"foo"})
                );
                assert!(frames
                    .iter()
                    .any(|v| v["content_block"]["name"] == "lookup"));
            }
            "responses" => {
                let arguments: String = frames
                    .iter()
                    .filter(|v| v["type"] == "response.function_call_arguments.delta")
                    .filter_map(|v| v["delta"].as_str())
                    .collect();
                assert_eq!(
                    serde_json::from_str::<Value>(&arguments).unwrap(),
                    json!({"q":"foo"})
                );
            }
            _ => {
                let tools: Vec<&Value> = frames
                    .iter()
                    .flat_map(|v| {
                        v["choices"][0]["delta"]["tool_calls"]
                            .as_array()
                            .into_iter()
                            .flatten()
                    })
                    .collect();
                let arguments: String = tools
                    .iter()
                    .filter_map(|v| v["function"]["arguments"].as_str())
                    .collect();
                assert_eq!(
                    serde_json::from_str::<Value>(&arguments).unwrap(),
                    json!({"q":"foo"})
                );
                assert_eq!(
                    tools
                        .iter()
                        .filter_map(|v| v["function"]["name"].as_str())
                        .collect::<String>(),
                    "lookup"
                );
            }
        }
    }
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 5);
}
