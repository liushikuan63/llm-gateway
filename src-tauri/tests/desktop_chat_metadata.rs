//! Exercises the real desktop HTTP consumer; all responses are synthetic loopback,
//! not six vendor accounts or proof of a supplier terminal/settlement receipt.
use axum::{http::StatusCode, routing::post, Json, Router};
use llm_gateway_lib::{
    config::AppConfig,
    desktop_chat::{self, ChatInput, ChatMessage},
};
use serde_json::{json, Value};

async fn reply(body: Value, status: StatusCode) -> Result<Value, String> {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let body = body.clone();
            async move {
                assert_eq!(request["stream"], false);
                (status, Json(body))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let result = desktop_chat::send(
        &AppConfig::default(),
        addr,
        ChatInput {
            personal_test: false,
            model: "synthetic-region-model".into(),
            session_id: uuid::Uuid::new_v4().to_string(),
            max_tokens: 32,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "synthetic prompt".into(),
                reasoning_content: None,
            }],
        },
    )
    .await;
    task.abort();
    result.map(|value| serde_json::to_value(value).unwrap())
}
fn body() -> Value {
    json!({"model":"synthetic-region-model", "choices":[{"message":{"role":"assistant","content":"synthetic reply"}}]})
}

#[tokio::test]
async fn desktop_reply_metadata_preserves_six_region_contracts_without_inventing_receipts() {
    // Removing finish propagation or reconstructing usage from token scalars breaks this contract.
    for (region, finish) in [
        ("Qoder CN", "stop"),
        ("Qoder Global", "length"),
        ("Trae CN", "stop"),
        ("Trae Global", "content_filter"),
        ("WorkBuddy CN", "length"),
        ("WorkBuddy Global", "vendor_specific"),
    ] {
        let mut input = body();
        input["choices"][0]["finish_reason"] = json!(finish);
        let usage = json!({"prompt_tokens":0,"completion_tokens":3,"total_tokens":3,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":2},"vendor_credit_units":"0.125","future_field":{"available":true}});
        input["usage"] = usage.clone();
        let result = reply(input, StatusCode::OK).await.unwrap();
        assert_eq!(result["finish_reason"], finish, "{region}");
        assert_eq!(
            result["usage"], usage,
            "preserve raw usage, including zero and extensions, {region}"
        );
        assert_eq!(
            result["response_metadata"],
            json!({"finish_reason":"reported","usage":"reported"}),
            "reported is shape-only, not verified usage, {region}"
        );
        assert!(result.get("cost").is_none());
        assert!(result.get("currency").is_none());
    }
}

#[tokio::test]
async fn desktop_reply_metadata_missing_null_invalid_and_partial_remain_distinct() {
    for (finish, usage, want_finish_state, want_usage_state) in [
        (None, None, "unavailable", "unavailable"),
        (
            Some(Value::Null),
            Some(Value::Null),
            "unavailable",
            "unavailable",
        ),
        (Some(json!("")), Some(json!({})), "unavailable", "reported"),
        (
            Some(json!(false)),
            Some(json!("not usage")),
            "invalid",
            "invalid",
        ),
        (
            Some(json!({"private":"synthetic-only"})),
            Some(json!([])),
            "invalid",
            "invalid",
        ),
        (
            None,
            Some(json!({"prompt_tokens":0})),
            "unavailable",
            "reported",
        ),
    ] {
        let mut input = body();
        if let Some(value) = finish {
            input["choices"][0]["finish_reason"] = value;
        }
        if let Some(value) = &usage {
            input["usage"] = value.clone();
        }
        let result = reply(input, StatusCode::OK).await.unwrap();
        assert!(
            result["finish_reason"].is_null(),
            "200 never fabricates stop"
        );
        assert_eq!(
            result["response_metadata"],
            json!({"finish_reason":want_finish_state,"usage":want_usage_state})
        );
        assert_eq!(
            result["usage"],
            usage.unwrap_or(Value::Null),
            "do not change existing raw usage compatibility"
        );
    }
}

#[tokio::test]
async fn desktop_reply_metadata_keeps_refusal_and_http_failure_semantics() {
    let mut refusal = body();
    refusal["choices"][0]["message"] = json!({"content":null,"refusal":"synthetic refusal"});
    refusal["choices"][0]["finish_reason"] = json!("content_filter");
    let result = reply(refusal, StatusCode::OK).await.unwrap();
    assert_eq!(result["refusal"], "synthetic refusal");
    assert_eq!(result["finish_reason"], "content_filter");
    assert_eq!(result["response_metadata"]["usage"], "unavailable");
    assert!(reply(
        json!({"error":{"message":"synthetic unavailable"},"choices":[{"finish_reason":"stop"}]}),
        StatusCode::SERVICE_UNAVAILABLE
    )
    .await
    .unwrap_err()
    .contains("503"));
}

#[test]
fn desktop_reply_metadata_old_result_deserializes_without_promoting_unknown() {
    let old = json!({"content":"old reply","reasoning":null,"model":"old","routed_via":null,"session_id":"old-session","trace_id":null,"fallback_attempts":0,"latency_ms":0,"usage":null,"cached":false});
    let value: desktop_chat::ChatResult = serde_json::from_value(old.clone()).unwrap();
    let result = serde_json::to_value(value).unwrap();
    assert!(result.get("finish_reason").is_none());
    assert!(result.get("response_metadata").is_none());
    assert_eq!(result["usage"], old["usage"]);
}

#[tokio::test]
async fn desktop_reply_metadata_ignores_compatibility_defaults_and_keeps_ordinary_wire_unchanged() {
    use llm_gateway_lib::{
        db::{repo, Db},
        domain::Provider,
        proxy::server::{serve, GatewayState},
    };
    use std::sync::Arc;
    let mut bare = body();
    bare["model"] = json!("synthetic-upstream");
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let bare = bare.clone();
            async move { Json(bare) }
        }),
    );
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_url = format!("http://{}/v1", upstream.local_addr().unwrap());
    let upstream_task = tokio::spawn(async move { axum::serve(upstream, app).await.unwrap() });
    let db = Db::connect_in_memory().await.unwrap();
    let provider:Provider=serde_json::from_value(json!({"id":"synthetic","name":"synthetic","dialect":"openai","base_url":upstream_url,"api_key_enc":"","enabled":true,"priority":0,"rpm_limit":0,"intelligence":60,"note":null,"runtime_id":null,"created_at":chrono::Utc::now(),"updated_at":chrono::Utc::now(),"models":[{"alias":"synthetic-model","upstream":"synthetic-upstream","enabled":true,"model_type":"chat","context_window":32768,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"price":null,"overrides":null,"local":null}]})).unwrap();
    repo::upsert_provider(db.pool(), &provider).await.unwrap();
    let mut cfg = AppConfig {
        port: std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port(),
        ..Default::default()
    };
    cfg.smart_routing.enabled = false;
    let gateway = Arc::new(GatewayState::new(db, cfg.clone()));
    gateway.reload_providers().await.unwrap();
    let job = tokio::spawn(serve(gateway.clone()));
    for _ in 0..100 {
        if gateway.listener_snapshot().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let addr = gateway.listener_snapshot().unwrap().bound_addr;
    let input = json!({"model":"synthetic-model","messages":[{"role":"user","content":"synthetic"}],"stream":false});
    let ordinary = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{addr}/v1/chat/completions"))
        .bearer_auth(&cfg.unified_key)
        .json(&input)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        ordinary["choices"][0]["finish_reason"], "stop",
        "preserve legacy compatible default"
    );
    assert_eq!(ordinary["usage"]["prompt_tokens"], 0);
    assert!(
        ordinary.get("llmgw_reply_observation").is_none(),
        "ordinary callers do not opt into desktop metadata"
    );
    let result = desktop_chat::send(
        &cfg,
        addr,
        ChatInput {
            personal_test: false,
            model: "synthetic-model".into(),
            session_id: uuid::Uuid::new_v4().to_string(),
            max_tokens: 32,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "desktop synthetic".into(),
                reasoning_content: None,
            }],
        },
    )
    .await;
    job.abort();
    upstream_task.abort();
    let result = serde_json::to_value(result.unwrap()).unwrap();
    assert!(
        result["finish_reason"].is_null(),
        "do not call legacy default stop a reported finish"
    );
    assert!(
        result["usage"].is_null(),
        "no reported usage must not become known zero"
    );
    assert_eq!(result["response_metadata"]["finish_reason"], "unavailable");
    assert_eq!(result["response_metadata"]["usage"], "unavailable");
}
