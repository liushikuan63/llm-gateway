use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::config::{AppConfig, RemoteModeConfig};
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider, RemoteAccessKey};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};

fn key_hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn mock_provider(base_url: String) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: "remote-mock".into(),
        name: "remote mock".into(),
        dialect: Dialect::OpenAI,
        base_url,
        api_key_enc: String::new(),
        enabled: true,
        priority: 1,
        models: vec![ModelRef {
            enabled: true,
            alias: "remote-model".into(),
            upstream: "remote-model".into(),
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
            capabilities: None,
        }],
        rpm_limit: 0,
        intelligence: 50,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

#[derive(Clone, Default)]
struct MockState {
    requests: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
}

async fn mock_chat(
    State(state): State<MockState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    state.requests.fetch_add(1, Ordering::SeqCst);
    state.bodies.lock().unwrap().push(body);
    Json(serde_json::json!({
        "id": "chatcmpl-remote-test",
        "object": "chat.completion",
        "model": "remote-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok" },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    }))
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

fn chat_body() -> serde_json::Value {
    serde_json::json!({
        "model": "remote-model",
        "messages": [{ "role": "user", "content": "hello" }]
    })
}

#[test]
fn remote_mode_rejects_plain_http_and_forces_loopback_listener() {
    let mut config = AppConfig {
        allow_lan: true,
        bind: "0.0.0.0".into(),
        remote_mode: RemoteModeConfig {
            enabled: true,
            public_url: Some("http://gateway.example.test".into()),
        },
        ..Default::default()
    };
    config.normalize_listener();

    assert!(!config.allow_lan);
    assert_eq!(config.bind, "127.0.0.1");
    assert!(config.validate_remote_mode().is_err());

    config.remote_mode.public_url = Some("https://gateway.example.test".into());
    assert!(config.validate_remote_mode().is_ok());
}

#[tokio::test]
async fn remote_proxy_uses_individual_keys_hides_management_and_audits_key_id() {
    let (upstream_url, _upstream_state, upstream_task) = spawn_mock_upstream().await;
    let db = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(db.pool(), &mock_provider(upstream_url))
        .await
        .unwrap();

    let secret = "remote-test-key";
    let now = chrono::Utc::now();
    repo::create_remote_access_key(
        db.pool(),
        &RemoteAccessKey {
            id: "rk-test".into(),
            label: "test device".into(),
            key_hash: key_hash(secret),
            enabled: true,
            rpm_limit: 1,
            // B2 新增：默认不限预算、不限模型。闸门是 opt-in 的。
            monthly_budget_micros: 0,
            budget_currency: String::new(),
            allowed_models: Vec::new(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();

    let mut config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: "local-only-key".into(),
        remote_mode: RemoteModeConfig {
            enabled: true,
            public_url: Some("https://gateway.example.test".into()),
        },
        ..Default::default()
    };
    config.normalize_listener();

    let gateway = Arc::new(GatewayState::new(db.clone(), config.clone()));
    gateway.reload_providers().await.unwrap();
    let gateway_task = tokio::spawn({
        let gateway = gateway.clone();
        async move {
            let _ = serve(gateway).await;
        }
    });
    let base_url = format!("http://{}:{}", config.bind, config.port);
    wait_for_gateway(&base_url).await;

    let client = reqwest::Client::new();
    let external = || {
        client
            .post(format!("{base_url}/v1/chat/completions"))
            .header("x-forwarded-for", "198.51.100.7")
            .header("x-forwarded-proto", "https")
            .json(&chat_body())
    };

    // `X-Forwarded-For` 把这个请求标识为来自可信本机反代之后的外部客户端。
    // 外部调用不能继续使用只给本机的统一 Key，且必须由反代证明 TLS 已终止。
    assert_eq!(
        client
            .post(format!("{base_url}/v1/chat/completions"))
            .header("x-forwarded-for", "198.51.100.7")
            .bearer_auth("local-only-key")
            .json(&chat_body())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .post(format!("{base_url}/v1/chat/completions"))
            .header("x-forwarded-for", "198.51.100.7")
            .header("x-forwarded-proto", "http")
            .bearer_auth("local-only-key")
            .json(&chat_body())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .post(format!("{base_url}/v1/chat/completions"))
            .bearer_auth("local-only-key")
            .json(&chat_body())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "本机直连不依赖反代 HTTPS 头"
    );
    assert_eq!(
        external()
            .bearer_auth("local-only-key")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{base_url}/gw/stats"))
            .header("x-forwarded-for", "198.51.100.7")
            .header("x-forwarded-proto", "https")
            .bearer_auth(secret)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    assert_eq!(
        external()
            .bearer_auth(secret)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    // 每个远程 Key 独立限速；第二次请求不会再到达上游。
    assert_eq!(
        external()
            .bearer_auth(secret)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    // 配置切回本地但进程尚未重启时，旧反代不能借此退化为“本地统一 Key”访问。
    let mut local_only = gateway.cfg_snapshot();
    local_only.remote_mode.enabled = false;
    local_only.normalize_listener();
    *gateway.cfg.write() = local_only;
    assert_eq!(
        external()
            .bearer_auth("local-only-key")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    let mut audited = false;
    for _ in 0..40 {
        let client_id: Option<String> = sqlx::query_scalar(
            "SELECT client FROM requests WHERE client = 'remote-key:rk-test' LIMIT 1",
        )
        .fetch_optional(db.pool())
        .await
        .unwrap();
        if client_id.as_deref() == Some("remote-key:rk-test") {
            audited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        audited,
        "request audit must contain the non-secret Key identifier"
    );

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn remote_access_keys_cannot_share_a_logical_session_namespace() {
    let (upstream_url, upstream_state, upstream_task) = spawn_mock_upstream().await;
    let db = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(db.pool(), &mock_provider(upstream_url))
        .await
        .unwrap();

    let now = chrono::Utc::now();
    for (id, secret) in [
        ("rk-tenant-a", "tenant-a-secret"),
        ("rk-tenant-b", "tenant-b-secret"),
    ] {
        repo::create_remote_access_key(
            db.pool(),
            &RemoteAccessKey {
                id: id.into(),
                label: id.into(),
                key_hash: key_hash(secret),
                enabled: true,
                rpm_limit: 20,
                // B2 新增：默认不限预算、不限模型。闸门是 opt-in 的。
                monthly_budget_micros: 0,
                budget_currency: String::new(),
                allowed_models: Vec::new(),
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .unwrap();
    }

    let mut config = AppConfig {
        port: unused_loopback_port().await,
        remote_mode: RemoteModeConfig {
            enabled: true,
            public_url: Some("https://gateway.example.test".into()),
        },
        ..Default::default()
    };
    config.normalize_listener();
    let gateway = Arc::new(GatewayState::new(db.clone(), config.clone()));
    gateway.reload_providers().await.unwrap();
    let gateway_task = tokio::spawn({
        let gateway = gateway.clone();
        async move {
            let _ = serve(gateway).await;
        }
    });
    let base_url = format!("http://{}:{}", config.bind, config.port);
    wait_for_gateway(&base_url).await;

    let client = reqwest::Client::new();
    let post = |key: &str, message: &str| {
        client
            .post(format!("{base_url}/v1/chat/completions"))
            .header("x-forwarded-for", "198.51.100.8")
            .header("x-forwarded-proto", "https")
            .header("x-session-id", "shared-logical-session")
            .bearer_auth(key)
            .json(&serde_json::json!({
                "model": "remote-model",
                "messages": [{ "role": "user", "content": message }],
            }))
    };

    let first = post("tenant-a-secret", "A 的私密上下文")
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["x-session-id"], "shared-logical-session");
    let second = post("tenant-b-secret", "B 的独立问题")
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(second.headers()["x-session-id"], "shared-logical-session");

    {
        let upstream_bodies = upstream_state.bodies.lock().unwrap();
        assert_eq!(upstream_bodies.len(), 2);
        let second_messages = upstream_bodies[1]["messages"].as_array().unwrap();
        assert!(second_messages
            .iter()
            .all(|message| { !message.to_string().contains("A 的私密上下文") }));
        assert!(second_messages
            .iter()
            .any(|message| message["content"] == "B 的独立问题"));
    }

    let sessions = repo::list_sessions(db.pool(), 10).await.unwrap();
    assert_eq!(sessions.len(), 2, "相同逻辑 ID 必须落入两个独立存储会话");
    assert!(sessions
        .iter()
        .all(|session| session.id.starts_with("rk-") && session.id != "shared-logical-session"));

    gateway_task.abort();
    upstream_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_key_rpm_reservation_allows_only_one_concurrent_request() {
    const REQUESTS: usize = 16;

    let (upstream_url, upstream_state, upstream_task) = spawn_mock_upstream().await;
    let db = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(db.pool(), &mock_provider(upstream_url))
        .await
        .unwrap();

    let secret = "remote-concurrent-key";
    let now = chrono::Utc::now();
    repo::create_remote_access_key(
        db.pool(),
        &RemoteAccessKey {
            id: "rk-concurrent".into(),
            label: "concurrent test".into(),
            key_hash: key_hash(secret),
            enabled: true,
            rpm_limit: 1,
            // B2 新增：默认不限预算、不限模型。闸门是 opt-in 的。
            monthly_budget_micros: 0,
            budget_currency: String::new(),
            allowed_models: Vec::new(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();

    let mut config = AppConfig {
        port: unused_loopback_port().await,
        remote_mode: RemoteModeConfig {
            enabled: true,
            public_url: Some("https://gateway.example.test".into()),
        },
        ..Default::default()
    };
    config.normalize_listener();
    let gateway = Arc::new(GatewayState::new(db, config.clone()));
    gateway.reload_providers().await.unwrap();
    let gateway_task = tokio::spawn({
        let gateway = gateway.clone();
        async move {
            let _ = serve(gateway).await;
        }
    });
    let base_url = format!("http://{}:{}", config.bind, config.port);
    wait_for_gateway(&base_url).await;

    let client = reqwest::Client::new();
    let start = Arc::new(tokio::sync::Barrier::new(REQUESTS));
    let mut requests = JoinSet::new();
    for _ in 0..REQUESTS {
        let client = client.clone();
        let base_url = base_url.clone();
        let start = start.clone();
        requests.spawn(async move {
            start.wait().await;
            client
                .post(format!("{base_url}/v1/chat/completions"))
                .header("x-forwarded-for", "198.51.100.7")
                .header("x-forwarded-proto", "https")
                .bearer_auth(secret)
                .json(&chat_body())
                .send()
                .await
                .unwrap()
                .status()
        });
    }

    let mut accepted = 0;
    while let Some(result) = requests.join_next().await {
        match result.unwrap() {
            StatusCode::OK => accepted += 1,
            StatusCode::TOO_MANY_REQUESTS => {}
            status => panic!("unexpected concurrent remote request status: {status}"),
        }
    }
    assert_eq!(accepted, 1);
    assert_eq!(upstream_state.requests.load(Ordering::SeqCst), 1);

    gateway_task.abort();
    upstream_task.abort();
}
