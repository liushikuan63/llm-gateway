use axum::{
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use llm_gateway_lib::{
    domain::Dialect,
    provider_quota::{query, QuotaAdapter},
};
use serde_json::json;

async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), handle)
}

#[tokio::test]
async fn automatic_detection_falls_back_from_missing_newapi_to_subscription_response() {
    let (base, handle) = server(Router::new().route("/prefix/v1/usage", get(|headers: HeaderMap| async move {
        assert_eq!(headers.get("authorization").unwrap(), "Bearer fixture-quota-key");
        Json(json!({"mode":"unrestricted","isValid":true,"subscription":{"daily_limit_usd":10,"daily_usage_usd":2,"expires_at":"2027-01-01T00:00:00Z"}}))
    }))).await;
    let result = query(
        "p",
        Dialect::OpenAI,
        &format!("{base}/prefix/v1"),
        "fixture-quota-key",
        QuotaAdapter::Auto,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.status, "ok");
    assert_eq!(result.metrics[0].remaining, Some(8.0));
    assert_eq!(result.expirations[0].scope, "subscription");
    handle.abort();
}

#[tokio::test]
async fn redirects_and_error_bodies_cannot_leak_keys_or_become_zero_balance() {
    let (base, handle) = server(
        Router::new()
            .route(
                "/v1/key",
                get(|| async {
                    (
                        StatusCode::FOUND,
                        [("location", "https://example.invalid/stolen")],
                    )
                }),
            )
            .route(
                "/api/usage/token",
                get(|| async { (StatusCode::UNAUTHORIZED, "reflected fixture-quota-key") }),
            )
            .route(
                "/v1/usage",
                get(|| async {
                    Json(json!({"success":false,"message":"reflected fixture-quota-key"}))
                }),
            ),
    )
    .await;
    for adapter in [
        QuotaAdapter::Openrouter,
        QuotaAdapter::Newapi,
        QuotaAdapter::Sub2api,
    ] {
        let error = query(
            "p",
            Dialect::OpenAI,
            &format!("{base}/v1"),
            "fixture-quota-key",
            adapter,
            None,
        )
        .await
        .unwrap_err();
        assert!(!error.contains("fixture-quota-key"));
        assert!(!error.contains("example.invalid"));
    }
    handle.abort();
}

#[tokio::test]
async fn unsupported_html_and_missing_optional_subscription_are_explicit() {
    let (base, handle) = server(Router::new().fallback(get(|| async {
        "<!doctype html><title>Console</title>".into_response()
    })))
    .await;
    let result = query(
        "p",
        Dialect::OpenAI,
        &base,
        "fixture-quota-key",
        QuotaAdapter::Auto,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.status, "unsupported");
    assert!(result.metrics.is_empty());
    assert!(result.expirations.is_empty());
    handle.abort();
}

#[tokio::test]
#[ignore = "需要用户授权的 OpenRouter Key，只读查询"]
async fn live_openrouter_quota_is_read_only() {
    let key = std::env::var("LLMGW_LIVE_OPENROUTER_KEY").expect("请设置测试环境变量");
    let result = query(
        "live",
        Dialect::OpenAI,
        "https://openrouter.ai/api/v1",
        &key,
        QuotaAdapter::Auto,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.status, "ok");
    assert_eq!(result.metrics[0].scope, "key");
    println!(
        "OpenRouter quota: source={}, metrics={}, expirations={}",
        result.source,
        result.metrics.len(),
        result.expirations.len()
    );
}

#[tokio::test]
#[ignore = "需要用户授权的 Air Outer Key，只读查询"]
async fn live_air_outer_quota_is_read_only() {
    let key = std::env::var("LLMGW_LIVE_AIR_OUTER_KEY").expect("请设置测试环境变量");
    let result = query(
        "live",
        Dialect::OpenAI,
        "https://ps.air-outer.com/v1",
        &key,
        QuotaAdapter::Auto,
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.status, "ok");
    println!(
        "Air Outer quota: source={}, metrics={}, expirations={}",
        result.source,
        result.metrics.len(),
        result.expirations.len()
    );
}
