//! Loopback response-consumer acceptance, not a real WorkBuddy generation test.
use axum::{http::StatusCode, routing::post, Json, Router};
use llm_gateway_lib::{
    config::AppConfig,
    desktop_chat::{self, ChatInput, ChatMessage},
    error::GatewayError,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[tokio::test]
async fn desktop_chat_keeps_legacy_error_code_and_surfaces_safe_policy_hint_once() {
    for is_policy in [false, true] {
        let error = GatewayError::Upstream {
            provider: "WorkBuddy".into(),
            model: "fixture-model".into(),
            status: 400,
            body: format!(
                "WorkBuddy 拒绝请求或返回无效响应; code=11128; request_id=owned-private-canary{}",
                if is_policy {
                    "; rejection=policy_denied"
                } else {
                    ""
                }
            ),
        }
        .no_replay();
        assert!(!error.retryable());
        let public = error.to_openai_error();
        assert_eq!(public["error"]["code"], "workbuddy_11128");
        assert_eq!(public["error"]["type"], "upstream_error");
        assert_eq!(
            public["error"]["diagnostic"].as_str(),
            is_policy.then_some("policy_denied")
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let response = public.clone();
                let calls = observed.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::BAD_REQUEST, Json(response))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = AppConfig {
            unified_key: "owned-loopback-key".into(),
            ..AppConfig::default()
        };
        let result = desktop_chat::send(
            &cfg,
            address,
            ChatInput {
                personal_test: false,
                model: "fixture-model".into(),
                session_id: uuid::Uuid::new_v4().to_string(),
                max_tokens: 32,
                messages: vec![ChatMessage {
                    reasoning_content: None,
                    role: "user".into(),
                    content: "owned fixture".into(),
                }],
            },
        )
        .await;
        task.abort();
        let _ = task.await;
        let text = result.unwrap_err();
        assert!(text.starts_with("HTTP 400 Bad Request："));
        assert_eq!(text.contains("策略拒绝"), is_policy);
        assert!(!text.contains("owned-private-canary"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
