//! 实际 HTTP 调用验证运行时数据库解析，以及权益管理路由的权限边界。
use std::sync::Arc;
use std::time::Duration;

use llm_gateway_lib::config::AppConfig;
use llm_gateway_lib::db::{repo, Db};
use llm_gateway_lib::domain::{AgentRuntime, Dialect, ModelRef, ModelType, Provider};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use reqwest::StatusCode;
use serde_json::{json, Value};

struct RunningGateway {
    state: Arc<GatewayState>,
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for RunningGateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start(db: Db) -> RunningGateway {
    let cfg = AppConfig {
        port: 0,
        unified_key: "account-route-test-key".into(),
        ..Default::default()
    };
    let state = Arc::new(GatewayState::new(db, cfg));
    state.reload_providers().await.unwrap();
    let task_state = state.clone();
    let task = tokio::spawn(async move { serve(task_state).await.unwrap() });
    let client = reqwest::Client::new();
    for _ in 0..100 {
        if let Some(snapshot) = state.listener_snapshot() {
            let url = format!("http://{}", snapshot.bound_addr);
            if client.get(format!("{url}/healthz")).send().await.is_ok() {
                return RunningGateway { state, url, task };
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task.abort();
    panic!("isolated gateway failed to start");
}

fn account_provider(runtime: &str) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: "account-only".into(),
        name: "account HTTP regression".into(),
        dialect: Dialect::OpenAI,
        base_url: "http://127.0.0.1:1/v1".into(),
        api_key_enc: String::new(),
        enabled: true,
        priority: 1,
        models: vec![ModelRef {
            enabled: true,
            alias: "account-test".into(),
            upstream: "vendor-model".into(),
            context_window: 16_384,
            supports_tools: false,
            supports_vision: false,
            supports_audio: false,
            supports_video: false,
            supports_thinking: false,
            supports_stream: false,
            model_type: ModelType::Chat,
            upstream_path: None,
            price: None,
            overrides: None,
            local: None,
            capabilities: None,
        }],
        rpm_limit: 0,
        intelligence: 50,
        note: None,
        runtime_id: Some(runtime.into()),
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn custom_runtime_id_resolves_database_kind_in_actual_chat_and_honors_disable() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut runtime = AgentRuntime::new("my-personal-account", "fake", "自定义账号");
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    repo::upsert_provider(db.pool(), &account_provider(&runtime.id))
        .await
        .unwrap();
    let running = start(db).await;
    let client = reqwest::Client::new();
    let request =
        json!({"model":"account-test", "messages":[{"role":"user","content":"runtime-check"}]});
    let response = client
        .post(format!("{}/v1/chat/completions", running.url))
        .bearer_auth("account-route-test-key")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    let text = body["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(text.contains("[fake-agent]") && text.contains("runtime-check"));
    assert!(text.contains("vendor-model"));

    runtime.enabled = false;
    repo::upsert_agent_runtime(running.state.db.pool(), &runtime)
        .await
        .unwrap();
    let response = client
        .post(format!("{}/v1/chat/completions", running.url))
        .bearer_auth("account-route-test-key")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_client_error(),
        "disabled runtime must be rejected"
    );
}

#[tokio::test]
async fn benefit_management_uses_auth_and_blocks_forwarded_clients_without_side_effects() {
    let running = start(Db::connect_in_memory().await.unwrap()).await;
    let client = reqwest::Client::new();
    for (method, route) in [("GET", "/gw/benefits"), ("POST", "/gw/benefits/claim")] {
        let request = || {
            client
                .request(method.parse().unwrap(), format!("{}{route}", running.url))
                .json(&json!({"accountId":"absent"}))
        };
        assert_eq!(
            request().send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request()
                .bearer_auth("account-route-test-key")
                .header("x-forwarded-for", "203.0.113.1")
                .header("x-forwarded-proto", "https")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let response = client
        .get(format!("{}/gw/benefits", running.url))
        .bearer_auth("account-route-test-key")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["enabled"], false);
    let response = client
        .post(format!("{}/gw/benefits/claim", running.url))
        .bearer_auth("account-route-test-key")
        .json(&json!({"accountId":"absent"}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    assert!(running
        .state
        .benefits
        .runs(None, 10)
        .await
        .unwrap()
        .is_empty());
}
