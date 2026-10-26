//! Offline HTTP fixtures exercise actual signing/auth/transport and stream collection.
use super::*;

#[tokio::test]
async fn qoder_responses_reasoning_summary_rejected_before_authorization() {
    let server = setup().await;
    for stream in [false, true] {
        let mut req = request();
        req.extra.insert(
            "reasoning".into(),
            serde_json::json!({"effort":"high","summary":"auto"}),
        );
        let error = if stream {
            server
                .client
                .call_stream(
                    Region::Global,
                    "fixture-missing-account",
                    &req,
                    "fixture-model",
                    Duration::from_secs(3),
                )
                .await
                .map(|_| ())
                .expect_err("local shape rejection")
        } else {
            server
                .client
                .call(
                    Region::Global,
                    "fixture-missing-account",
                    &req,
                    "fixture-model",
                    Duration::from_secs(3),
                )
                .await
                .map(|_| ())
                .expect_err("local shape rejection")
        };
        assert!(matches!(
            error,
            GatewayError::RequestControlUnavailable { .. }
        ));
        assert_eq!(error.http_status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!error.retryable());
        assert_eq!(server.fixture.catalogs.load(Ordering::SeqCst), 0);
        assert_eq!(server.fixture.posts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn responses_reasoning_account_review_qoder_conflict_zero_authorization_http() {
    let server = setup().await;
    for stream in [false, true] {
        let mut req = request();
        req.extra
            .insert("reasoning".into(), serde_json::json!({"effort":"high"}));
        req.thinking = Some(serde_json::json!({"effort":"low"}));
        let error = if stream {
            server
                .client
                .call_stream(
                    Region::Global,
                    "main",
                    &req,
                    "synthetic-model",
                    Duration::from_secs(3),
                )
                .await
                .map(|_| ())
                .expect_err("local conflict")
        } else {
            server
                .client
                .call(
                    Region::Global,
                    "main",
                    &req,
                    "synthetic-model",
                    Duration::from_secs(3),
                )
                .await
                .map(|_| ())
                .expect_err("local conflict")
        };
        assert!(matches!(
            error,
            GatewayError::RequestControlUnavailable { .. }
        ));
        assert_eq!(error.http_status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!error.retryable());
        assert_eq!(server.fixture.catalogs.load(Ordering::SeqCst), 0);
        assert_eq!(server.fixture.posts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn responses_reasoning_qoder_effort_reaches_actual_signed_http_both_regions() {
    use futures_util::StreamExt;
    let server = setup().await;
    for region in [Region::Global, Region::Cn] {
        for stream in [false, true] {
            let mut req = request();
            req.extra
                .insert("reasoning".into(), serde_json::json!({"effort":"high"}));
            if stream {
                let mut events = server
                    .client
                    .call_stream(
                        region,
                        "main",
                        &req,
                        "synthetic-model",
                        Duration::from_secs(3),
                    )
                    .await
                    .unwrap();
                while let Some(event) = events.next().await {
                    event.unwrap();
                }
            } else {
                server
                    .client
                    .call(
                        region,
                        "main",
                        &req,
                        "synthetic-model",
                        Duration::from_secs(3),
                    )
                    .await
                    .unwrap();
            }
        }
    }
    assert_eq!(server.fixture.posts.load(Ordering::SeqCst), 4);
    for (_, wire) in server.fixture.requests.lock().iter() {
        assert_eq!(wire["parameters"]["reasoning_effort"], "high");
        assert!(wire.get("reasoning").is_none());
    }
}
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
fn normal_response_with_null_frames() -> String {
    // Inject only into the two production-consumer cases below. Preserve the
    // normal fixture and every other caller, including late usage and finish.
    let empty = envelope(Value::Null);
    let mut source = empty.clone();
    for frame in normal_response().split_inclusive("\n\n") {
        if frame.starts_with("event:finish") {
            source.push_str(frame);
        } else {
            source.push_str(frame);
            source.push_str(&empty);
        }
    }
    source
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
async fn responses_instructions_h10_qoder_both_regions_real_gateway_wire() {
    use crate::{
        config::AppConfig,
        db::repo,
        domain::{AgentRuntime, Dialect, Provider},
        proxy::server::{serve, GatewayState},
    };
    for region in [Region::Global, Region::Cn] {
        let f = setup().await;
        let mut runtime = AgentRuntime::new("qoder-instructions", "qoder-model", "Qoder fixture");
        runtime.options = Some(
            json!({"region":region,"account_id":"main","model_aliases":{"saved-model":"synthetic-model"}}),
        );
        repo::upsert_agent_runtime(f.db.pool(), &runtime)
            .await
            .unwrap();
        let now = chrono::Utc::now();
        let p=Provider{id:"qoder-instructions".into(),name:"Qoder fixture".into(),dialect:Dialect::OpenAI,base_url:region.model_origin().into(),api_key_enc:String::new(),enabled:true,priority:1,rpm_limit:0,intelligence:50,
            models:vec![serde_json::from_value(json!({"alias":"public","upstream":"saved-model","context_window":65536,"supports_stream":true,"supports_tools":true,"supports_vision":true,"supports_thinking":true,"overrides":{"max_tokens":32}})).unwrap()],runtime_id:Some(runtime.id),note:None,created_at:now,updated_at:now};
        repo::upsert_provider(f.db.pool(), &p).await.unwrap();
        let cfg = AppConfig {
            port: 0,
            unified_key: "qoder-instructions-key".into(),
            compact_threshold_tokens: u32::MAX,
            ..Default::default()
        };
        let mut gateway = GatewayState::new(f.db.clone(), cfg);
        gateway.qoder = Ok(f.client.clone());
        let gateway = Arc::new(gateway);
        gateway.reload_providers().await.unwrap();
        let state = gateway.clone();
        let task = tokio::spawn(async move { serve(state).await.unwrap() });
        struct Abort(tokio::task::JoinHandle<()>);
        impl Drop for Abort {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _guard = Abort(task);
        let listener = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(snapshot) = gateway.listener_snapshot() {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        for stream in [false, true] {
            let sid = format!("qoder-instructions-{stream}");
            for instruction in [
                Some("Qoder-A α\r\n"),
                Some("Qoder-B\t"),
                None,
                Some(" \t\r\n "),
            ] {
                let mut body = json!({"model":"public","input":"caller original","stream":stream});
                if let Some(instruction) = instruction {
                    body["instructions"] = json!(instruction);
                }
                let response = http
                    .post(format!("http://{}/v1/responses", listener.bound_addr))
                    .bearer_auth("qoder-instructions-key")
                    .header("x-session-id", &sid)
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                let text = response.text().await.unwrap();
                assert_eq!(status, 200, "{text}");
                if stream {
                    assert!(text.contains("response.completed"), "{text}");
                } else {
                    assert_eq!(
                        serde_json::from_str::<Value>(&text).unwrap()["status"],
                        "completed"
                    );
                }
                let wire = f.fixture.requests.lock().last().unwrap().1.clone();
                if let Some(instruction) = instruction {
                    assert_eq!(wire["system"], instruction);
                    assert_eq!(wire["messages"][0]["content"], instruction);
                } else {
                    assert_eq!(wire["system"], "");
                    assert!(!wire.to_string().contains("Qoder-A"));
                    assert!(!wire.to_string().contains("Qoder-B"));
                }
                let systems: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE role='system'")
                        .fetch_one(f.db.pool())
                        .await
                        .unwrap();
                assert_eq!(systems, 0);
            }
        }
        assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 8);
        assert_eq!(
            f.fixture.catalogs.load(Ordering::SeqCst),
            1,
            "only owned loopback catalog fixture"
        );
    }
}

#[tokio::test]
async fn both_regions_make_one_encoded_post_and_preserve_late_usage_without_a_client() {
    let f = setup().await;
    *f.fixture.response.lock() = Some(normal_response_with_null_frames());
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
        *f.fixture.response.lock() = Some(normal_response_with_null_frames());
        let gateway = gateway(&f, region, false).await;
        for stream in [false, true] {
            let response=http.post(format!("{}/v1/chat/completions",gateway.url)).bearer_auth("synthetic-gateway-key")
                .json(&json!({"model":"public","stream":stream,"messages":[{"role":"system","content":"retained system"},{"role":"user","content":"retained question"}],"max_tokens":64})).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.text().await.unwrap();
            if stream {
                assert_eq!(body.matches("data: [DONE]").count(), 1);
                assert!(body.contains("synthetic reasoning") && body.contains("OK"));
                let usage: Vec<Value> = body
                    .lines()
                    .filter_map(|line| line.strip_prefix("data: "))
                    .filter_map(|data| serde_json::from_str::<Value>(data).ok())
                    .filter_map(|packet| {
                        packet
                            .get("usage")
                            .filter(|usage| !usage.is_null())
                            .cloned()
                    })
                    .collect();
                assert_eq!(
                    usage.len(),
                    1,
                    "one late usage snapshot reaches the real streaming consumer"
                );
                assert_eq!(usage[0]["prompt_tokens"], 10);
                assert_eq!(usage[0]["completion_tokens"], 2);
                assert_eq!(usage[0]["total_tokens"], 12);
                assert_eq!(usage[0]["credit"], 0.5);
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

/// Verified authentication and a local zero price do not prove a supplier's hard stop.
#[tokio::test]
async fn account_prerequisite_qoder_valid_principal_zero_price_still_denies_free_http() {
    use crate::db::repo;
    for region in [Region::Global, Region::Cn] {
        let f = setup().await;
        let gateway = gateway(&f, region, false).await;
        let mut provider = gateway.state.providers.read()[0].clone();
        provider.models[0].price = Some(
            serde_json::from_value(json!({
                "prompt":0.0,"completion":0.0,"currency":"usd"
            }))
            .unwrap(),
        );
        repo::upsert_provider(f.db.pool(), &provider).await.unwrap();
        gateway.state.reload_providers().await.unwrap();
        let auth = f
            .auth
            .prepare_model_authorization(region, "main")
            .await
            .unwrap();
        f.auth.model_authorization_current(&auth).await.unwrap();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let positive = http.post(format!("{}/v1/chat/completions", gateway.url))
            .bearer_auth("synthetic-gateway-key")
            .json(&json!({"model":"public","stream":false,"messages":[{"role":"user","content":"owned prerequisite control"}]}))
            .send().await.unwrap();
        assert_eq!(positive.status(), StatusCode::OK);
        assert!(positive.text().await.unwrap().contains("OK"));
        assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
        assert_eq!(f.fixture.catalogs.load(Ordering::SeqCst), 1);
        let eligibility = repo::catalog::free_eligibility(
            f.db.pool(),
            &provider,
            &provider.models[0],
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
        assert!(!eligibility.eligible);
        assert_eq!(
            eligibility.reason,
            "account_hard_stop_and_reservation_unverified"
        );
        for global in [false, true] {
            let mut cfg = gateway.state.cfg.read().clone();
            cfg.free_routing.enabled = global;
            gateway.state.update_cfg(cfg);
            for stream in [false, true] {
                let response = http.post(format!("{}/v1/chat/completions", gateway.url))
                    .bearer_auth("synthetic-gateway-key")
                    .json(&json!({"model":if global {"public"} else {"free"},"stream":stream,"messages":[{"role":"user","content":"owned hard-stop rejection"}]}))
                    .send().await.unwrap();
                assert_eq!(response.status(), StatusCode::CONFLICT);
                let body: Value = response.json().await.unwrap();
                assert_eq!(body["error"]["type"], "free_policy_rejected");
                assert_eq!(
                    body["error"]["reason"],
                    "no_qualified_free_model; configure a fresh authoritative free catalog"
                );
                f.auth.model_authorization_current(&auth).await.unwrap();
                assert_eq!(
                    f.fixture.posts.load(Ordering::SeqCst),
                    1,
                    "valid principal cannot authorize an unproven free model POST"
                );
                assert_eq!(
                    f.fixture.catalogs.load(Ordering::SeqCst),
                    1,
                    "rejection must not initiate another native catalog request"
                );
            }
        }
        let reservations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM free_entitlement_reservations")
                .fetch_one(f.db.pool())
                .await
                .unwrap();
        assert_eq!(
            reservations, 0,
            "authentication cannot mint an entitlement or reservation"
        );
        assert_eq!(gateway.state.health.total_failovers(), 0);
    }
}

#[derive(Clone, Copy)]
enum EventEpochChange {
    Healthy,
    Cancel,
    Rotate,
    Expire,
    HoldDatabase,
}
async fn qoder_event_epoch_control(change: EventEpochChange) {
    for region in [Region::Global, Region::Cn] {
        let f = setup().await;
        let expires = chrono::Utc::now().timestamp_millis() + 2500;
        if matches!(change, EventEpochChange::Expire) {
            f.auth
                .test_seed_model_account(region, "main", expires)
                .await;
        }
        // One complete SSE frame necessarily decodes several events. TCP chunk
        // boundaries cannot split the decoder's returned vector for this frame.
        let payload = envelope(json!({"choices":[{"delta":{
            "content":"owned-first-".repeat(1024),
            "reasoning_content":"owned-second-".repeat(1024)
        },"finish_reason":""}],"usage":{"prompt_tokens":10,"completion_tokens":2}}))
            + &format!(
                "data:{}\n\n",
                json!({"statusCodeValue":200,"body":"[DONE]"})
            )
            + &envelope(json!({"choices":[{"delta":{},"finish_reason":"stop"}]}))
            + "event:finish\ndata:{}\n\n";
        let decoded = crate::qoder_auth::stream::QoderSseDecoder::default()
            .feed(payload.as_bytes(), true, &[])
            .expect("valid owned multi-event fixture");
        assert!(decoded.len() > 2);
        assert!(matches!(decoded.last(), Some(UpstreamEvent::Done { .. })));
        *f.fixture.response.lock() = Some(payload);
        let mut req = request();
        req.stream = true;
        let timeout = if matches!(change, EventEpochChange::HoldDatabase) {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(10)
        };
        let mut events = f
            .client
            .call_stream(region, "main", &req, "synthetic-model", timeout)
            .await
            .unwrap();
        let first = events.next().await.unwrap().unwrap();
        assert!(
            matches!(first, UpstreamEvent::Delta(ref text) if text.starts_with("owned-first-")),
            "positive boundary must consume the first event of a multi-event decoded frame"
        );
        assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
        let mut held = None;
        match change {
            EventEpochChange::Healthy => {
                let mut reasoning = false;
                let mut done = 0;
                while let Some(event) = events.next().await {
                    match event.unwrap() {
                        UpstreamEvent::ReasoningDelta(text) => {
                            reasoning |= text.starts_with("owned-second-")
                        }
                        UpstreamEvent::Done { .. } => done += 1,
                        _ => (),
                    }
                }
                assert!(reasoning);
                assert_eq!(done, 1);
            }
            EventEpochChange::Cancel => f.auth.job_cancel(region, "main").await.unwrap(),
            EventEpochChange::Rotate => {
                f.auth
                    .test_seed_model_account(
                        region,
                        "main",
                        chrono::Utc::now().timestamp_millis() + 3_600_000,
                    )
                    .await
            }
            EventEpochChange::Expire => {
                while chrono::Utc::now().timestamp_millis() <= expires {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            EventEpochChange::HoldDatabase => held = Some(f.db.pool().acquire().await.unwrap()),
        }
        if !matches!(change, EventEpochChange::Healthy) {
            let error = tokio::time::timeout(Duration::from_secs(4), events.next())
                .await
                .expect("per-event authorization must obey the original request deadline")
                .expect("an invalidated stream must report an error instead of clean EOF")
                .expect_err(
                    "already-decoded later events cannot outlive the captured authorization",
                );
            assert!(!error.retryable());
            assert_eq!(
                error.http_status(),
                if matches!(change, EventEpochChange::HoldDatabase) {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::UNAUTHORIZED
                }
            );
            drop(held);
            assert!(
                events.next().await.is_none(),
                "the rejected stream must terminate without a late Done"
            );
        }
        assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1, "no model replay");
        assert_eq!(
            f.fixture.catalogs.load(Ordering::SeqCst),
            1,
            "no new catalog or authorization request"
        );
    }
}
#[tokio::test]
async fn qoder_event_epoch_healthy_multievent_frame_preserves_output_and_done() {
    qoder_event_epoch_control(EventEpochChange::Healthy).await;
}
#[tokio::test]
async fn qoder_event_epoch_cancel_between_decoded_events_rejects_next_publication() {
    qoder_event_epoch_control(EventEpochChange::Cancel).await;
}
#[tokio::test]
async fn qoder_event_epoch_rotation_between_decoded_events_rejects_next_publication() {
    qoder_event_epoch_control(EventEpochChange::Rotate).await;
}
#[tokio::test]
async fn qoder_event_epoch_expiry_between_decoded_events_rejects_next_publication() {
    qoder_event_epoch_control(EventEpochChange::Expire).await;
}
#[tokio::test]
async fn qoder_event_epoch_database_wait_before_publication_obeys_original_deadline() {
    qoder_event_epoch_control(EventEpochChange::HoldDatabase).await;
}

async fn entitlement_consumer_success_control(stream: bool) {
    let f = setup().await;
    let region = Region::Global;
    let gateway = gateway(&f, region, false).await;
    let provider = gateway.state.providers.read()[0].clone();
    let auth = f
        .auth
        .prepare_model_authorization(region, "main")
        .await
        .unwrap();
    let proof = crate::entitlement::VerifiedEntitlement::artificial_dispatch(
        "artificial_qoder_global",
        "main",
        (auth.principal_fingerprint.clone(), auth.fingerprint.clone()),
        "synthetic-model",
        "chat",
        2,
    );
    gateway
        .state
        .cost_gate
        .accounts
        .install_artificial(&provider, "saved-model", proof.clone(), |_| Some(1))
        .await
        .unwrap();
    let ledger = crate::entitlement::Ledger::new(f.db.pool().clone());
    assert_eq!(ledger.balance(&proof).await.unwrap().held, 0);
    let mut cfg = gateway.state.cfg.read().clone();
    cfg.free_routing.enabled = true;
    gateway.state.update_cfg(cfg);
    let response = reqwest::Client::builder().no_proxy().build().unwrap()
        .post(format!("{}/v1/chat/completions", gateway.url))
        .bearer_auth("synthetic-gateway-key")
        .json(&json!({"model":"public","stream":stream,"messages":[{"role":"user","content":"owned entitlement success"}]}))
        .send().await.unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "explicit artificial grant must reach the production consumer: {body}"
    );
    assert!(body.contains("OK"));
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
    let balance = ledger.balance(&proof).await.unwrap();
    assert_eq!(
        (
            balance.ceiling,
            balance.held,
            balance.spent,
            balance.blocked
        ),
        (2, 0, 1, false)
    );
    let rows: Vec<(String, i64, Option<i64>)> =
        sqlx::query_as("SELECT phase,bound,confirmed_units FROM free_entitlement_reservations")
            .fetch_all(f.db.pool())
            .await
            .unwrap();
    assert_eq!(rows, vec![("settled".into(), 1, Some(1))]);
    gateway
        .state
        .drain_audits(Duration::from_secs(2))
        .await
        .unwrap();
}
#[tokio::test]
async fn entitlement_consumer_buffered_gateway_reserves_sends_and_settles() {
    entitlement_consumer_success_control(false).await;
}
#[tokio::test]
async fn entitlement_consumer_stream_gateway_reserves_sends_and_settles() {
    entitlement_consumer_success_control(true).await;
}

async fn consumer_fixture(
    capacity: i64,
    confirm: fn(&crate::domain::Usage) -> Option<i64>,
) -> (
    Servers,
    GatewayServer,
    crate::entitlement::VerifiedEntitlement,
) {
    let f = setup().await;
    let gateway = gateway(&f, Region::Global, false).await;
    let provider = gateway.state.providers.read()[0].clone();
    let auth = f
        .auth
        .prepare_model_authorization(Region::Global, "main")
        .await
        .unwrap();
    let proof = crate::entitlement::VerifiedEntitlement::artificial_dispatch(
        "artificial_qoder_global",
        "main",
        (auth.principal_fingerprint, auth.fingerprint),
        "synthetic-model",
        "chat",
        capacity,
    );
    gateway
        .state
        .cost_gate
        .accounts
        .install_artificial(&provider, "saved-model", proof.clone(), confirm)
        .await
        .unwrap();
    let mut cfg = gateway.state.cfg.read().clone();
    cfg.free_routing.enabled = true;
    cfg.failover_enabled = true;
    cfg.max_fallback_attempts = 2;
    gateway.state.update_cfg(cfg);
    (f, gateway, proof)
}
async fn consumer_send(base: &str) -> reqwest::Response {
    reqwest::Client::builder().no_proxy().build().unwrap()
        .post(format!("{base}/v1/chat/completions")).bearer_auth("synthetic-gateway-key")
        .json(&json!({"model":"public","stream":false,"messages":[{"role":"user","content":"owned consumer control"}]}))
        .send().await.unwrap()
}
#[tokio::test]
async fn entitlement_consumer_dispatch_is_committed_before_upstream_observes_post() {
    let (f, gateway, proof) = consumer_fixture(1, |_| Some(1)).await;
    f.fixture.hold.store(true, Ordering::SeqCst);
    let base = gateway.url.clone();
    let response = tokio::spawn(async move { consumer_send(&base).await });
    tokio::time::timeout(Duration::from_secs(3), f.fixture.entered.notified())
        .await
        .unwrap();
    let phase: String = sqlx::query_scalar("SELECT phase FROM free_entitlement_reservations")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(phase, "dispatched");
    let ledger = crate::entitlement::Ledger::new(f.db.pool().clone());
    let balance = ledger.balance(&proof).await.unwrap();
    assert_eq!((balance.held, balance.spent), (1, 0));
    f.fixture.release.notify_one();
    let response = response.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    assert_eq!(ledger.balance(&proof).await.unwrap().spent, 1);
}
#[tokio::test]
async fn entitlement_consumer_shared_pool_concurrent_aliases_admit_only_one_post() {
    let (f, gateway, proof) = consumer_fixture(1, |_| Some(1)).await;
    let mut second = gateway.state.providers.read()[0].clone();
    second.id = "owned-second-account-provider".into();
    second.priority = 2;
    crate::db::repo::upsert_provider(f.db.pool(), &second)
        .await
        .unwrap();
    gateway
        .state
        .cost_gate
        .accounts
        .install_artificial(&second, "saved-model", proof.clone(), |_| Some(1))
        .await
        .unwrap();
    gateway.state.reload_providers().await.unwrap();
    gateway
        .state
        .cost_gate
        .accounts
        .set_reservation_barrier(Arc::new(tokio::sync::Barrier::new(2)));
    f.fixture.hold.store(true, Ordering::SeqCst);
    let a = gateway.url.clone();
    let b = gateway.url.clone();
    let mut first = tokio::spawn(async move { consumer_send(&a).await });
    let mut second = tokio::spawn(async move { consumer_send(&b).await });
    tokio::time::timeout(Duration::from_secs(3), f.fixture.entered.notified())
        .await
        .unwrap();
    let (denied, remaining) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! { r=&mut first => (r.unwrap(), second), r=&mut second => (r.unwrap(), first) }
    }).await.unwrap();
    assert_eq!(denied.status(), StatusCode::CONFLICT);
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
    f.fixture.release.notify_one();
    let accepted = remaining.await.unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
    accepted.bytes().await.unwrap();
    let balance = crate::entitlement::Ledger::new(f.db.pool().clone())
        .balance(&proof)
        .await
        .unwrap();
    assert_eq!((balance.ceiling, balance.held, balance.spent), (1, 0, 1));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM free_entitlement_reservations")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[tokio::test]
async fn entitlement_consumer_dispatched_503_retains_hold_without_qualified_fallback() {
    let (f, gateway, proof) = consumer_fixture(2, |_| Some(1)).await;
    let mut second = gateway.state.providers.read()[0].clone();
    second.id = "owned-fallback".into();
    second.priority = 2;
    crate::db::repo::upsert_provider(f.db.pool(), &second)
        .await
        .unwrap();
    gateway
        .state
        .cost_gate
        .accounts
        .install_artificial(&second, "saved-model", proof.clone(), |_| Some(1))
        .await
        .unwrap();
    gateway.state.reload_providers().await.unwrap();
    f.fixture.status.store(503, Ordering::SeqCst);
    let response = consumer_send(&gateway.url).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    response.bytes().await.unwrap();
    assert_eq!(
        f.fixture.posts.load(Ordering::SeqCst),
        1,
        "unknown dispatch must not replay to the other qualified candidate"
    );
    let balance = crate::entitlement::Ledger::new(f.db.pool().clone())
        .balance(&proof)
        .await
        .unwrap();
    assert_eq!((balance.held, balance.spent), (1, 0));
    let phase: String = sqlx::query_scalar("SELECT phase FROM free_entitlement_reservations")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(phase, "dispatched");
}
#[tokio::test]
async fn entitlement_consumer_untrusted_terminal_receipt_keeps_full_bound() {
    let (f, gateway, proof) = consumer_fixture(1, |_| None).await;
    let response = consumer_send(&gateway.url).await;
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    let balance = crate::entitlement::Ledger::new(f.db.pool().clone())
        .balance(&proof)
        .await
        .unwrap();
    assert_eq!((balance.held, balance.spent), (1, 0));
    let rows: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT phase,confirmed_units FROM free_entitlement_reservations")
            .fetch_all(f.db.pool())
            .await
            .unwrap();
    assert_eq!(rows, vec![("dispatched".into(), None)]);
}
#[tokio::test]
async fn entitlement_consumer_settlement_database_failure_never_refunds_or_replays() {
    let (f, gateway, proof) = consumer_fixture(1, |_| Some(1)).await;
    sqlx::query("CREATE TRIGGER owned_settle_failure BEFORE UPDATE ON free_entitlement_budgets WHEN NEW.spent > OLD.spent BEGIN SELECT RAISE(ABORT,'owned settlement failure'); END")
        .execute(f.db.pool()).await.unwrap();
    let response = consumer_send(&gateway.url).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    response.bytes().await.unwrap();
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
    let balance = crate::entitlement::Ledger::new(f.db.pool().clone())
        .balance(&proof)
        .await
        .unwrap();
    assert_eq!((balance.held, balance.spent), (1, 0));
}
#[tokio::test]
async fn entitlement_consumer_bound_breach_blocks_pool_and_retains_hold() {
    let (f, gateway, proof) = consumer_fixture(2, |_| Some(2)).await;
    let response = consumer_send(&gateway.url).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    response.bytes().await.unwrap();
    let balance = crate::entitlement::Ledger::new(f.db.pool().clone())
        .balance(&proof)
        .await
        .unwrap();
    assert_eq!((balance.held, balance.spent, balance.blocked), (1, 0, true));
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn entitlement_consumer_summary_requires_its_own_purpose_and_settles_same_pool() {
    let (f, gateway, chat) = consumer_fixture(2, |_| Some(1)).await;
    let response = consumer_send(&gateway.url).await;
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    let messages = vec![crate::domain::Message::user("owned summary input")];
    assert!(
        gateway.state.summarize(messages.clone(), "").await.is_err(),
        "chat proof cannot authorize a summary"
    );
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
    let provider = gateway.state.providers.read()[0].clone();
    let summary = crate::entitlement::VerifiedEntitlement::artificial_dispatch_same_snapshot(
        &chat,
        "synthetic-model",
        "summary",
    );
    gateway
        .state
        .cost_gate
        .accounts
        .install_artificial(&provider, "saved-model", summary, |_| Some(1))
        .await
        .unwrap();
    assert_eq!(gateway.state.summarize(messages, "").await.unwrap(), "OK");
    let balance = crate::entitlement::Ledger::new(f.db.pool().clone())
        .balance(&chat)
        .await
        .unwrap();
    assert_eq!((balance.held, balance.spent), (0, 2));
    let purposes: Vec<String> =
        sqlx::query_scalar("SELECT purpose FROM free_entitlement_reservations ORDER BY purpose")
            .fetch_all(f.db.pool())
            .await
            .unwrap();
    assert_eq!(purposes, vec!["chat", "summary"]);
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn entitlement_consumer_wrong_subject_does_not_borrow_valid_credential() {
    let (f, gateway, proof) = consumer_fixture(1, |_| Some(1)).await;
    let provider = gateway.state.providers.read()[0].clone();
    let auth = f
        .auth
        .prepare_model_authorization(Region::Global, "main")
        .await
        .unwrap();
    let wrong = crate::entitlement::VerifiedEntitlement::artificial_dispatch(
        "artificial_qoder_global",
        "owned-wrong-subject",
        ("a".repeat(64), auth.fingerprint),
        "synthetic-model",
        "chat",
        1,
    );
    // Separate artificial slot avoids falsely treating a same-epoch subject change
    // as a successful issuer update; capability still presents the captured credential.
    gateway
        .state
        .cost_gate
        .accounts
        .install_artificial(&provider, "saved-model", wrong.clone(), |_| Some(1))
        .await
        .unwrap();
    let response = consumer_send(&gateway.url).await;
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "valid credential cannot spend another subject's proof"
    );
    response.bytes().await.unwrap();
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 0);
    let balance = crate::entitlement::Ledger::new(f.db.pool().clone())
        .balance(&wrong)
        .await
        .unwrap();
    assert_eq!((balance.held, balance.spent), (0, 0));
    let rows: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT phase,confirmed_units FROM free_entitlement_reservations")
            .fetch_all(f.db.pool())
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![("cancelled".into(), None)],
        "only the known undispatched reservation may be refunded"
    );
    let _ = proof;
}
#[tokio::test]
async fn entitlement_consumer_cancel_after_dispatched_post_retains_full_hold() {
    use tokio::io::AsyncWriteExt;
    let (f, gateway, proof) = consumer_fixture(1, |_| Some(1)).await;
    f.fixture.hold.store(true, Ordering::SeqCst);
    let body=json!({"model":"public","stream":false,"messages":[{"role":"user","content":"owned cancel after dispatch"}]}).to_string();
    let address = gateway.url.strip_prefix("http://").unwrap();
    let trace = uuid::Uuid::new_v4().simple().to_string();
    let request=format!("POST /v1/chat/completions HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer synthetic-gateway-key\r\nX-Trace-Id: {trace}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len());
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    client.write_all(request.as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), f.fixture.entered.notified())
        .await
        .unwrap();
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE trace_id=? AND status=499")
                    .bind(&trace)
                    .fetch_one(f.db.pool())
                    .await
                    .unwrap();
            if count == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let ledger = crate::entitlement::Ledger::new(f.db.pool().clone());
    let balance = ledger.balance(&proof).await.unwrap();
    assert_eq!((balance.held, balance.spent), (1, 0));
    let row: (String, Option<i64>) =
        sqlx::query_as("SELECT phase,confirmed_units FROM free_entitlement_reservations")
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert_eq!(row, ("dispatched".into(), None));
    f.fixture.release.notify_one();
    assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn entitlement_consumer_epoch_change_after_post_keeps_unknown_and_refunds_only_new_presend() {
    let (f, gateway, proof) = consumer_fixture(2, |_| Some(1)).await;
    f.fixture.hold.store(true, Ordering::SeqCst);
    let base = gateway.url.clone();
    let response = tokio::spawn(async move { consumer_send(&base).await });
    tokio::time::timeout(Duration::from_secs(3), f.fixture.entered.notified())
        .await
        .unwrap();
    f.auth
        .test_seed_model_account(
            Region::Global,
            "main",
            chrono::Utc::now().timestamp_millis() + 3_600_000,
        )
        .await;
    f.fixture.release.notify_one();
    let response = response.await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    response.bytes().await.unwrap();
    let ledger = crate::entitlement::Ledger::new(f.db.pool().clone());
    let old = ledger.balance(&proof).await.unwrap();
    assert_eq!((old.held, old.spent), (1, 0));
    // Stale proof still has one remaining request, but the new credential cannot
    // use it. Only this new, definitely undispatched reservation is cancelled.
    // The old instance correctly applies its existing health cooldown after401.
    // Recreate only the owned GatewayState over the same owned DB: neither the
    // old dispatched hold nor the old issuer binding may reset on this recovery.
    drop(gateway);
    let resumed = self::gateway(&f, Region::Global, false).await;
    let provider = resumed.state.providers.read()[0].clone();
    resumed
        .state
        .cost_gate
        .accounts
        .install_artificial(&provider, "saved-model", proof.clone(), |_| Some(1))
        .await
        .unwrap();
    let mut cfg = resumed.state.cfg.read().clone();
    cfg.free_routing.enabled = true;
    resumed.state.update_cfg(cfg);
    let second = consumer_send(&resumed.url).await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    second.bytes().await.unwrap();
    let balance = ledger.balance(&proof).await.unwrap();
    assert_eq!((balance.held, balance.spent), (1, 0));
    assert_eq!(
        f.fixture.posts.load(Ordering::SeqCst),
        1,
        "no replay after rotation and no stale-capability POST"
    );
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT phase,confirmed_units FROM free_entitlement_reservations ORDER BY phase",
    )
    .fetch_all(f.db.pool())
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![("cancelled".into(), None), ("dispatched".into(), None)]
    );
}

#[path = "qoder_tests/subject_receipt_tests.rs"]
mod subject_receipt_tests;
