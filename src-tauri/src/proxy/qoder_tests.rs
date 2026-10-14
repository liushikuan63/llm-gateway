//! Offline HTTP fixtures exercise actual signing/auth/transport and stream collection.
use super::*;
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};

#[derive(Default)]
pub(super) struct Fixture {
    pub(super) posts: AtomicUsize,
    pub(super) catalogs: AtomicUsize,
    pub(super) status: AtomicU16,
    pub(super) catalog_status: AtomicU16,
    pub(super) requests: parking_lot::Mutex<Vec<(String, Value)>>,
    pub(super) response: parking_lot::Mutex<Option<String>>,
    hold: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
pub(super) struct Servers {
    pub(super) client: Arc<QoderModelClient>,
    pub(super) auth: QoderAuth,
    pub(super) db: crate::db::Db,
    pub(super) fixture: Arc<Fixture>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Servers {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn envelope(value: Value) -> String {
    format!(
        "data:{}\n\n",
        json!({"statusCodeValue":200,"body":value.to_string()})
    )
}
fn normal_response() -> String {
    envelope(
        json!({"choices":[{"index":0,"delta":{"content":"OK","reasoning_content":"synthetic reasoning"},"finish_reason":""}],"usage":{"prompt_tokens":10,"completion_tokens":0}}),
    ) + &format!(
        "data:{}\n\n",
        json!({"statusCodeValue":200,"body":"[DONE]"})
    ) + &envelope(json!({"choices":[{"delta":{},"finish_reason":"stop"}]}))
        + &envelope(
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":6,"cacheable_tokens":2},"credits":0.5}}),
        )
        + "event:finish\ndata:{}\n\n"
}
fn tool_response() -> String {
    envelope(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"synthetic-call","type":"function","function":{"name":"look","arguments":"{\"q\":\""}}]}}]}),
    ) + &envelope(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"up","arguments":"foo\"}"}}]},"finish_reason":"tool_calls"}]}),
    ) + &envelope(json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}}))
        + "event:finish\ndata:{}\n\n"
}
async fn catalog(
    State(f): State<Arc<Fixture>>,
    Path(region): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    assert!(matches!(region.as_str(), "global" | "cn"));
    assert!(headers["authorization"]
        .to_str()
        .unwrap()
        .starts_with("Bearer COSY."));
    f.catalogs.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::from_u16(f.catalog_status.load(Ordering::SeqCst)).unwrap(),
        Json(
            json!({"assistant":[{"key":"synthetic-model","display_name":"Synthetic","format":"openai","enable":true,"is_vl":true,"is_reasoning":true}]}),
        ),
    )
}
async fn completion(
    State(f): State<Arc<Fixture>>,
    Path(region): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    assert_eq!(
        query.get("FetchKeys").map(String::as_str),
        Some("llm_model_result")
    );
    assert_eq!(
        query.get("AgentId").map(String::as_str),
        Some("agent_common")
    );
    assert_eq!(query.get("Encode").map(String::as_str), Some("1"));
    assert_eq!(query.len(), 3);
    assert!(headers["authorization"]
        .to_str()
        .unwrap()
        .starts_with("Bearer COSY."));
    assert_eq!(headers["x-model-key"], "synthetic-model");
    assert_eq!(headers["x-model-source"], "system");
    assert_eq!(headers["cosy-data-policy"], "disagree");
    assert!(
        !headers.contains_key("cosy-machinetoken") && !headers.contains_key("cosy-machinetype")
    );
    assert_eq!(headers["cosy-user"], "synthetic-owner-main");
    let encoded = std::str::from_utf8(&body).unwrap();
    assert!(!encoded.starts_with('"') && !encoded.starts_with('{'));
    let decoded = crate::qoder_auth::test_decode_wire_body(encoded).await;
    assert_eq!(decoded["model_config"]["key"], "synthetic-model");
    assert_eq!(decoded["parameters"]["max_tokens"], 32);
    assert_eq!(decoded["is_retry"], false);
    let tool_request = decoded["tools"].as_array().is_some_and(|v| !v.is_empty());
    f.requests.lock().push((region, decoded));
    f.posts.fetch_add(1, Ordering::SeqCst);
    if f.hold.load(Ordering::SeqCst) {
        f.entered.notify_one();
        f.release.notified().await;
    }
    let text = f.response.lock().clone().unwrap_or_else(|| {
        if tool_request {
            tool_response()
        } else {
            normal_response()
        }
    });
    (
        StatusCode::from_u16(f.status.load(Ordering::SeqCst)).unwrap(),
        [(reqwest::header::CONTENT_TYPE, "text/event-stream")],
        text,
    )
}
pub(super) async fn setup() -> Servers {
    use base64::Engine;
    if std::env::var_os("LLMGW_MASTER_KEY").is_none() {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            base64::engine::general_purpose::STANDARD.encode([73; 32]),
        );
    }
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let fixture = Arc::new(Fixture {
        status: AtomicU16::new(200),
        catalog_status: AtomicU16::new(200),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin =
        reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let app = Router::new()
        .route("/:region/algo/api/v2/model/list", get(catalog))
        .route(
            "/:region/algo/api/v2/service/pro/sse/agent_chat_generation",
            post(completion),
        )
        .with_state(fixture.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let auth = QoderAuth::with_model_test_origin(db.pool().clone(), origin.clone());
    for region in [Region::Global, Region::Cn] {
        auth.test_seed_model_account(
            region,
            "main",
            chrono::Utc::now().timestamp_millis() + 3_600_000,
        )
        .await;
    }
    let client = Arc::new(QoderModelClient::with_test_origin(auth.clone(), origin));
    Servers {
        client,
        auth,
        db,
        fixture,
        task,
    }
}
pub(super) fn request() -> ChatRequest {
    serde_json::from_value(json!({"model":"gateway-alias","stream":false,"max_tokens":32,"messages":[{"role":"system","content":"synthetic system"},{"role":"user","content":"synthetic question"}]})).unwrap()
}

#[tokio::test]
async fn both_regions_make_one_encoded_post_and_preserve_late_usage_without_a_client() {
    let f = setup().await;
    for region in [Region::Global, Region::Cn] {
        let result = f
            .client
            .call(
                region,
                "main",
                &request(),
                "synthetic-model",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(result.content, "OK");
        assert_eq!(
            result.reasoning_content.as_deref(),
            Some("synthetic reasoning")
        );
        assert_eq!(result.finish_reason.as_deref(), Some("stop"));
        assert_eq!(result.transport.as_deref(), Some("qoder-http"));
        let usage = result.usage.unwrap();
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (10, 2, 12)
        );
        assert_eq!(
            (
                usage.cache_read_tokens,
                usage.cache_creation_tokens,
                usage.credit
            ),
            (6, 2, Some(0.5))
        );
    }
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 2);
    assert_eq!(f.fixture.catalogs.load(Ordering::SeqCst), 2);
    let requests = f.fixture.requests.lock();
    assert_eq!(requests[0].0, "global");
    assert_eq!(requests[1].0, "cn");
    assert_eq!(requests[0].1["messages"][0]["content"], "synthetic system");
    assert_eq!(
        requests[0].1["messages"][1]["content"],
        "synthetic question"
    );
}
#[tokio::test]
async fn model_rejections_do_not_retry_or_echo_a_private_response() {
    for code in [401, 429, 502, 302] {
        let f = setup().await;
        f.fixture.status.store(code, Ordering::SeqCst);
        *f.fixture.response.lock() = Some("synthetic-private-response".into());
        let error = f
            .client
            .call(
                Region::Global,
                "main",
                &request(),
                "synthetic-model",
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert_eq!(error.upstream_status(), Some(code));
        assert!(!error.retryable());
        assert!(!error.to_string().contains("synthetic-private-response"));
        assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn catalog_cache_is_bound_to_account_region_and_credential_generation() {
    let f = setup().await;
    for _ in 0..2 {
        f.client
            .call(
                Region::Global,
                "main",
                &request(),
                "synthetic-model",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
    }
    assert_eq!(f.fixture.catalogs.load(Ordering::SeqCst), 1);
    f.auth
        .test_seed_model_account(
            Region::Global,
            "main",
            chrono::Utc::now().timestamp_millis() + 3_600_000,
        )
        .await;
    f.client
        .call(
            Region::Global,
            "main",
            &request(),
            "synthetic-model",
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(f.fixture.catalogs.load(Ordering::SeqCst), 2);
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn cancelled_missing_expired_and_unknown_models_fail_before_model_post() {
    let f = setup().await;
    assert!(f
        .client
        .call(
            Region::Global,
            "missing",
            &request(),
            "synthetic-model",
            Duration::from_secs(5)
        )
        .await
        .is_err());
    assert!(f
        .client
        .call(
            Region::Global,
            "main",
            &request(),
            "unknown",
            Duration::from_secs(5)
        )
        .await
        .is_err());
    f.auth.job_cancel(Region::Global, "main").await.unwrap();
    assert!(f
        .client
        .call(
            Region::Global,
            "main",
            &request(),
            "synthetic-model",
            Duration::from_secs(5)
        )
        .await
        .is_err());
    f.auth
        .test_seed_model_account(
            Region::Cn,
            "main",
            chrono::Utc::now().timestamp_millis() - 1,
        )
        .await;
    assert!(f
        .client
        .call(
            Region::Cn,
            "main",
            &request(),
            "synthetic-model",
            Duration::from_secs(5)
        )
        .await
        .is_err());
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn truncated_stream_cannot_become_a_successful_completion() {
    let f = setup().await;
    *f.fixture.response.lock() = Some(envelope(
        json!({"choices":[{"delta":{"content":"partial"},"finish_reason":"stop"}]}),
    ));
    assert!(f
        .client
        .call(
            Region::Global,
            "main",
            &request(),
            "synthetic-model",
            Duration::from_secs(5)
        )
        .await
        .is_err());
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
}

struct GatewayServer {
    state: Arc<crate::proxy::GatewayState>,
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for GatewayServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn gateway(f: &Servers, region: Region, remote: bool) -> GatewayServer {
    use crate::{
        config::AppConfig,
        db::repo,
        domain::{AgentRuntime, Dialect, Provider},
    };
    let mut runtime = AgentRuntime::new("qoder-fixture", "qoder-model", "Qoder direct fixture");
    runtime.options = Some(
        json!({"region":region,"account_id":"main","model_aliases":{"saved-model":"synthetic-model"}}),
    );
    repo::upsert_agent_runtime(f.db.pool(), &runtime)
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let provider=Provider {id:"qoder-fixture-provider".into(),name:"Qoder fixture".into(),dialect:Dialect::OpenAI,
        base_url:"http://127.0.0.1:1/must-not-be-used".into(),api_key_enc:String::new(),enabled:true,priority:1,rpm_limit:0,intelligence:50,
        models:vec![serde_json::from_value(json!({"alias":"public","upstream":"saved-model","context_window":65536,
            "supports_tools":true,"supports_vision":true,"supports_stream":true,"supports_thinking":true,
            "overrides":{"max_tokens":32,"temperature":0.2}})).unwrap()],runtime_id:Some(runtime.id),note:None,created_at:now,updated_at:now};
    repo::upsert_provider(f.db.pool(), &provider).await.unwrap();
    let mut cfg = AppConfig {
        port: 0,
        unified_key: "synthetic-gateway-key".into(),
        http_proxy: None,
        ..Default::default()
    };
    if remote {
        use sha2::{Digest, Sha256};
        let key = crate::domain::RemoteAccessKey {
            id: "synthetic-remote".into(),
            label: "Synthetic remote".into(),
            key_hash: format!("{:x}", Sha256::digest(b"synthetic-remote-key")),
            enabled: true,
            rpm_limit: 1000,
            monthly_budget_micros: 1,
            budget_currency: "USD".into(),
            allowed_models: vec!["public".into()],
            created_at: now,
            updated_at: now,
        };
        repo::create_remote_access_key(f.db.pool(), &key)
            .await
            .unwrap();
        cfg.remote_mode.enabled = true;
        cfg.remote_mode.public_url = Some("https://gateway.example.invalid".into());
        cfg.normalize_listener();
    }
    let mut state = crate::proxy::GatewayState::new(f.db.clone(), cfg);
    state.qoder_auth = Ok(f.auth.clone());
    state.qoder = Ok(f.client.clone());
    let state = Arc::new(state);
    state.reload_providers().await.unwrap();
    let server = state.clone();
    let task = tokio::spawn(async move {
        crate::proxy::server::serve(server).await.unwrap();
    });
    let address = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if task.is_finished() {
                panic!("owned Qoder gateway startup failed");
            }
            if let Some(snapshot) = state.listener_snapshot() {
                break snapshot.bound_addr;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    GatewayServer {
        state,
        url: format!("http://{address}"),
        task,
    }
}
#[tokio::test]
async fn gateway_both_regions_preserve_stream_nonstream_history_usage_and_audit() {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for region in [Region::Global, Region::Cn] {
        let f = setup().await;
        let gateway = gateway(&f, region, false).await;
        for stream in [false, true] {
            let response=http.post(format!("{}/v1/chat/completions",gateway.url)).bearer_auth("synthetic-gateway-key")
                .json(&json!({"model":"public","stream":stream,"messages":[{"role":"system","content":"retained system"},{"role":"user","content":"retained question"}],"max_tokens":64})).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.text().await.unwrap();
            if stream {
                assert_eq!(body.matches("data: [DONE]").count(), 1);
                assert!(body.contains("synthetic reasoning") && body.contains("OK"));
            } else {
                let body: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(body["choices"][0]["message"]["content"], "OK");
                assert_eq!(body["usage"]["total_tokens"], 12);
                assert_eq!(body["usage"]["credit"], 0.5);
            }
        }
        assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 2);
        for (actual, request) in f.fixture.requests.lock().iter() {
            assert_eq!(
                actual,
                if region == Region::Global {
                    "global"
                } else {
                    "cn"
                }
            );
            assert_eq!(request["messages"][0]["content"], "retained system");
            assert_eq!(request["messages"][1]["content"], "retained question");
        }
        tokio::time::timeout(Duration::from_secs(5),async{loop {
            let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE status=200 AND prompt_tokens=10 AND completion_tokens=2").fetch_one(gateway.state.db.pool()).await.unwrap();
            if count==2{break;}tokio::task::yield_now().await;
        }}).await.unwrap();
        let history: String =
            sqlx::query_scalar("SELECT group_concat(content,'|') FROM session_messages")
                .fetch_one(gateway.state.db.pool())
                .await
                .unwrap();
        assert!(history.contains("retained question") && history.contains("OK"));
        assert!(!history.contains("synthetic reasoning"));
    }
}
#[tokio::test]
async fn gateway_five_protocols_preserve_complete_tool_name_and_json_arguments() {
    let f = setup().await;
    let gateway = gateway(&f, Region::Global, false).await;
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
    for (protocol, path, request) in cases {
        let response = http
            .post(format!("{}{path}", gateway.url))
            .bearer_auth("synthetic-gateway-key")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{protocol}");
        let wire = response.text().await.unwrap();
        let frames: Vec<Value> = wire
            .lines()
            .filter_map(|line| {
                serde_json::from_str(if protocol == "ollama" {
                    line
                } else {
                    line.strip_prefix("data: ")?
                })
                .ok()
            })
            .collect();
        match protocol {
            "gemini" => {
                let tools: Vec<_> = frames
                    .iter()
                    .flat_map(|v| {
                        v["candidates"][0]["content"]["parts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                    })
                    .filter_map(|v| v.get("functionCall"))
                    .collect();
                assert_eq!(tools.len(), 1);
                assert_eq!(tools[0]["name"], "lookup");
                assert_eq!(tools[0]["args"], json!({"q":"foo"}));
            }
            "ollama" => {
                let tools: Vec<_> = frames
                    .iter()
                    .flat_map(|v| v["message"]["tool_calls"].as_array().into_iter().flatten())
                    .collect();
                assert_eq!(tools.len(), 1);
                assert_eq!(tools[0]["function"]["name"], "lookup");
                assert_eq!(tools[0]["function"]["arguments"], json!({"q":"foo"}));
            }
            "anthropic" => {
                let args: String = frames
                    .iter()
                    .filter_map(|v| v["delta"]["partial_json"].as_str())
                    .collect();
                assert_eq!(
                    serde_json::from_str::<Value>(&args).unwrap(),
                    json!({"q":"foo"})
                );
                assert!(frames
                    .iter()
                    .any(|v| v["content_block"]["name"] == "lookup"));
            }
            "responses" => {
                let args: String = frames
                    .iter()
                    .filter(|v| v["type"] == "response.function_call_arguments.delta")
                    .filter_map(|v| v["delta"].as_str())
                    .collect();
                assert_eq!(
                    serde_json::from_str::<Value>(&args).unwrap(),
                    json!({"q":"foo"})
                );
            }
            _ => {
                let tools: Vec<_> = frames
                    .iter()
                    .flat_map(|v| {
                        v["choices"][0]["delta"]["tool_calls"]
                            .as_array()
                            .into_iter()
                            .flatten()
                    })
                    .collect();
                let args: String = tools
                    .iter()
                    .filter_map(|v| v["function"]["arguments"].as_str())
                    .collect();
                let names: String = tools
                    .iter()
                    .filter_map(|v| v["function"]["name"].as_str())
                    .collect();
                assert_eq!(names, "lookup");
                assert_eq!(
                    serde_json::from_str::<Value>(&args).unwrap(),
                    json!({"q":"foo"})
                );
            }
        }
    }
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 5);
}
#[tokio::test]
async fn gateway_disabled_runtime_never_calls_http_or_a_cli() {
    use crate::db::repo;
    let f = setup().await;
    let gateway = gateway(&f, Region::Global, false).await;
    let mut runtime = repo::get_agent_runtime(f.db.pool(), "qoder-fixture")
        .await
        .unwrap()
        .unwrap();
    runtime.enabled = false;
    repo::upsert_agent_runtime(f.db.pool(), &runtime)
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for stream in [false, true] {
        let response=http.post(format!("{}/v1/chat/completions",gateway.url)).bearer_auth("synthetic-gateway-key").json(&json!({"model":"public","stream":stream,"messages":[{"role":"user","content":"call"}]})).send().await.unwrap();
        assert!(!response.status().is_success());
    }
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 0);
    assert_eq!(f.fixture.catalogs.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn gateway_auth_confirmation_policy_cannot_replay_a_qoder_model_post() {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for stream in [false, true] {
        let f = setup().await;
        let gateway = gateway(&f, Region::Global, false).await;
        f.fixture.status.store(401, Ordering::SeqCst);
        let mut cfg = gateway.state.cfg.read().clone();
        cfg.auth_failure.mode = crate::config::AuthFailureMode::Skip;
        cfg.auth_failure.confirm_retries = 1;
        cfg.max_fallback_attempts = 2;
        cfg.failover_enabled = true;
        gateway.state.update_cfg(cfg);
        let response=http.post(format!("{}/v1/chat/completions",gateway.url)).bearer_auth("synthetic-gateway-key")
            .json(&json!({"model":"public","stream":stream,"messages":[{"role":"user","content":"single synthetic request"}]})).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            f.fixture.posts.load(Ordering::SeqCst),
            1,
            "Qoder model POST must not be replayed by auth confirmation"
        );
    }
}
#[tokio::test]
async fn cancellation_during_dispatched_post_rejects_late_response_without_replay() {
    let f = setup().await;
    f.fixture.hold.store(true, Ordering::SeqCst);
    let client = f.client.clone();
    let task = tokio::spawn(async move {
        client
            .call(
                Region::Global,
                "main",
                &request(),
                "synthetic-model",
                Duration::from_secs(5),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), f.fixture.entered.notified())
        .await
        .unwrap();
    f.auth.job_cancel(Region::Global, "main").await.unwrap();
    f.fixture.release.notify_one();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.http_status(), StatusCode::UNAUTHORIZED);
    assert!(!error.retryable());
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn dispatched_model_timeout_remains_one_attempt_and_releases_the_response() {
    let f = setup().await;
    f.fixture.hold.store(true, Ordering::SeqCst);
    let client = f.client.clone();
    let task = tokio::spawn(async move {
        client
            .call(
                Region::Global,
                "main",
                &request(),
                "synthetic-model",
                Duration::from_secs(2),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), f.fixture.entered.notified())
        .await
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(4), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    f.fixture.release.notify_one();
    assert_eq!(error.http_status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(!error.retryable());
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn gateway_budget_still_blocks_account_models_and_disabled_gate_allows_one_post() {
    let f = setup().await;
    sqlx::query("INSERT INTO requests(ts,client,access_key_id,requested_model,status,cost,currency) VALUES(?,?,'synthetic-remote','seed',200,0.01,'USD')")
        .bind(chrono::Utc::now().timestamp()).bind("remote-key:synthetic-remote").execute(f.db.pool()).await.unwrap();
    let spent = crate::db::repo::sum_cost_by_currency_for_key(
        f.db.pool(),
        "synthetic-remote",
        crate::budget::month_start_secs(chrono::Utc::now().timestamp()),
    )
    .await
    .unwrap();
    assert_eq!(spent, vec![("USD".into(), 0.01)]);
    let gateway = gateway(&f, Region::Global, true).await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for (enabled, status) in [
        (true, StatusCode::TOO_MANY_REQUESTS),
        (false, StatusCode::OK),
    ] {
        let mut cfg = gateway.state.cfg.read().clone();
        cfg.budget.enabled = enabled;
        gateway.state.update_cfg(cfg);
        let response=http.post(format!("{}/v1/chat/completions",gateway.url)).bearer_auth("synthetic-remote-key")
            .header("x-forwarded-for","198.51.100.10").header("x-forwarded-proto","https")
            .json(&json!({"model":"public","stream":false,"messages":[{"role":"user","content":"single budget control"}]})).send().await.unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(
            f.fixture.posts.load(Ordering::SeqCst),
            usize::from(!enabled)
        );
    }
}
