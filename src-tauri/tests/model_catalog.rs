use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use llm_gateway_lib::crypto;
use llm_gateway_lib::domain::{Dialect, ModelType, Provider};
use llm_gateway_lib::model_catalog::{
    discover, matches_saved_provider_target, normalize_base_url, ContextSource, DiscoveryInput,
    DEFAULT_CONTEXT_WINDOW,
};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const TEST_MASTER_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

fn use_test_master_key() {
    static SET_TEST_MASTER_KEY: Once = Once::new();
    SET_TEST_MASTER_KEY.call_once(|| {
        // 本集成测试进程不读取或创建用户实际的 master.key。
        std::env::set_var("LLMGW_MASTER_KEY", TEST_MASTER_KEY);
    });
}

async fn spawn(app: Router) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), task)
}

fn input(dialect: Dialect, base_url: String, api_key: &str) -> DiscoveryInput {
    DiscoveryInput {
        provider_id: None,
        dialect,
        base_url,
        api_key: api_key.into(),
    }
}

fn provider(dialect: Dialect, base_url: String, api_key_enc: &str) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: "saved-provider".into(),
        name: "saved provider".into(),
        dialect,
        base_url,
        api_key_enc: api_key_enc.into(),
        enabled: true,
        priority: 1,
        models: Vec::new(),
        rpm_limit: 0,
        intelligence: 50,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

#[derive(Clone, Default)]
struct HeaderCapture {
    authorization: Arc<Mutex<Option<String>>>,
    api_key: Arc<Mutex<Option<String>>>,
    version: Arc<Mutex<Option<String>>>,
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

async fn openrouter_models(
    State(capture): State<HeaderCapture>,
    headers: HeaderMap,
) -> Json<Value> {
    *capture.authorization.lock().unwrap() = header(&headers, "authorization");
    Json(json!({
        "data": [{
            "id": "openrouter/example",
            "name": "Example Vision",
            "context_length": 131072,
            "supported_parameters": ["tools"],
            "architecture": {
                "input_modalities": ["text", "image"],
                "modality": "text+image->text"
            },
            "pricing": { "prompt": "0", "completion": "0" }
        }, {
            "id": "openrouter/paid",
            "context_window": 65536,
            "model_type": "embedding",
            "pricing": { "prompt": "0", "completion": "0.0001" }
        }, {
            "id": "openrouter/incomplete-pricing",
            "max_input_tokens": 4096,
            "pricing": { "prompt": "0" }
        }]
    }))
}

#[tokio::test]
async fn openai_catalog_normalizes_full_url_sends_bearer_and_reads_openrouter_metadata() {
    let capture = HeaderCapture::default();
    let app = Router::new()
        .route("/v1/models", get(openrouter_models))
        .with_state(capture.clone());
    let (base, task) = spawn(app).await;

    let response = discover(
        &input(
            Dialect::OpenAI,
            format!("{base}/v1/chat/completions"),
            "catalog-test-key",
        ),
        None,
        None,
    )
    .await
    .unwrap();
    task.abort();

    assert_eq!(response.base_url, format!("{base}/v1"));
    assert_eq!(response.models.len(), 3);
    let model = response
        .models
        .iter()
        .find(|model| model.id == "openrouter/example")
        .unwrap();
    assert_eq!(model.id, "openrouter/example");
    assert_eq!(model.name, "Example Vision");
    assert_eq!(model.context_window, 131_072);
    assert_eq!(model.context_source, ContextSource::Provider);
    assert_eq!(model.supports_tools, Some(true));
    assert_eq!(model.supports_vision, Some(true));
    assert_eq!(model.model_type, Some(ModelType::Chat));
    assert_eq!(model.supports_stream, None);
    assert_eq!(model.is_free, Some(true));
    let paid = response
        .models
        .iter()
        .find(|model| model.id == "openrouter/paid")
        .unwrap();
    assert_eq!(paid.context_window, 65_536);
    assert_eq!(paid.context_source, ContextSource::Provider);
    assert_eq!(paid.model_type, Some(ModelType::Embedding));
    assert_eq!(paid.is_free, Some(false));
    let incomplete = response
        .models
        .iter()
        .find(|model| model.id == "openrouter/incomplete-pricing")
        .unwrap();
    assert_eq!(incomplete.context_window, 4_096);
    assert_eq!(incomplete.context_source, ContextSource::Provider);
    assert_eq!(incomplete.is_free, None);
    assert_eq!(
        capture.authorization.lock().unwrap().as_deref(),
        Some("Bearer catalog-test-key")
    );
}

#[tokio::test]
async fn blank_key_reuses_saved_key_only_for_the_same_normalized_target() {
    use_test_master_key();
    let capture = HeaderCapture::default();
    let app = Router::new()
        .route("/v1/models", get(openrouter_models))
        .with_state(capture.clone());
    let (base, task) = spawn(app).await;
    let saved = provider(
        Dialect::OpenAI,
        format!("{base}/v1"),
        &crypto::encrypt("stored-catalog-key").unwrap(),
    );
    let mut request = input(Dialect::OpenAI, format!("{base}/v1/chat/completions"), "");
    request.provider_id = Some(saved.id.clone());

    discover(&request, Some(&saved), None).await.unwrap();
    assert_eq!(
        capture.authorization.lock().unwrap().as_deref(),
        Some("Bearer stored-catalog-key")
    );

    request.base_url = format!("{base}/other/v1/chat/completions");
    let error = discover(&request, Some(&saved), None)
        .await
        .expect_err("an edited target must not receive the stored key");
    task.abort();
    assert!(error.contains("重新输入 API Key"));
    assert!(!error.contains("stored-catalog-key"));
}

async fn openai_without_metadata() -> Json<Value> {
    Json(json!({
        "data": [
            { "id": "plain-model" },
            { "id": "plain-model" }
        ]
    }))
}

#[tokio::test]
async fn openai_catalog_deduplicates_and_marks_missing_metadata_as_default() {
    let (base, task) = spawn(Router::new().route("/v1/models", get(openai_without_metadata))).await;

    let response = discover(
        &input(Dialect::OpenAI, format!("{base}/v1"), ""),
        None,
        None,
    )
    .await
    .unwrap();
    task.abort();

    assert_eq!(response.models.len(), 1);
    let model = &response.models[0];
    assert_eq!(model.context_window, DEFAULT_CONTEXT_WINDOW);
    assert_eq!(model.context_source, ContextSource::Default);
    assert_eq!(model.supports_tools, None);
    assert_eq!(model.supports_vision, None);
    assert_eq!(model.supports_stream, None);
    assert_eq!(model.model_type, None);
    assert_eq!(model.is_free, None);
}

async fn anthropic_models(
    State(capture): State<HeaderCapture>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Json<Value> {
    *capture.api_key.lock().unwrap() = header(&headers, "x-api-key");
    *capture.version.lock().unwrap() = header(&headers, "anthropic-version");
    if query.get("after_id").is_some_and(|id| id == "claude-test") {
        Json(json!({
            "data": [{
                "id": "claude-test-second",
                "display_name": "Claude Test Second",
                "context_length": 100000
            }],
            "has_more": false
        }))
    } else {
        Json(json!({
            "data": [{
                "id": "claude-test",
                "display_name": "Claude Test",
                "context_window": 200000
            }],
            "has_more": true,
            "last_id": "claude-test"
        }))
    }
}

#[tokio::test]
async fn anthropic_catalog_uses_messages_base_and_required_headers() {
    let capture = HeaderCapture::default();
    let app = Router::new()
        .route("/v1/models", get(anthropic_models))
        .with_state(capture.clone());
    let (base, task) = spawn(app).await;

    let response = discover(
        &input(
            Dialect::Anthropic,
            format!("{base}/v1/messages"),
            "anthropic-test-key",
        ),
        None,
        None,
    )
    .await
    .unwrap();
    task.abort();

    assert_eq!(response.base_url, format!("{base}/v1"));
    assert_eq!(response.models.len(), 2);
    let first = response
        .models
        .iter()
        .find(|model| model.id == "claude-test")
        .unwrap();
    assert_eq!(first.name, "Claude Test");
    assert_eq!(first.context_window, 200_000);
    assert_eq!(first.context_source, ContextSource::Provider);
    let second = response
        .models
        .iter()
        .find(|model| model.id == "claude-test-second")
        .unwrap();
    assert_eq!(second.context_window, 100_000);
    assert_eq!(second.context_source, ContextSource::Provider);
    assert_eq!(
        capture.api_key.lock().unwrap().as_deref(),
        Some("anthropic-test-key")
    );
    assert_eq!(
        capture.version.lock().unwrap().as_deref(),
        Some("2023-06-01")
    );
}

#[derive(Clone, Default)]
struct GeminiCapture {
    requests: Arc<Mutex<Vec<GeminiRequest>>>,
}

#[derive(Debug, PartialEq, Eq)]
struct GeminiRequest {
    page_token: Option<String>,
    page_size: Option<String>,
    api_key: Option<String>,
}

async fn gemini_models(
    State(capture): State<GeminiCapture>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Json<Value> {
    capture.requests.lock().unwrap().push(GeminiRequest {
        page_token: query.get("pageToken").cloned(),
        page_size: query.get("pageSize").cloned(),
        api_key: header(&headers, "x-goog-api-key"),
    });
    if query
        .get("pageToken")
        .is_some_and(|token| token == "next-page")
    {
        Json(json!({
            "models": [{
                "name": "models/gemini-second-001",
                "displayName": "Gemini Second",
                "inputTokenLimit": 65536,
                "supportedGenerationMethods": ["generateContent"]
            }]
        }))
    } else {
        Json(json!({
            "models": [{
                "name": "models/gemini-first-002",
                "baseModelId": "gemini-first",
                "displayName": "Gemini First",
                "inputTokenLimit": 1048576,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]
            }, {
                "name": "models/text-embedding-004",
                "displayName": "Embedding only",
                "inputTokenLimit": 2048,
                "supportedGenerationMethods": ["embedContent"]
            }],
            "nextPageToken": "next-page"
        }))
    }
}

#[tokio::test]
async fn gemini_catalog_paginates_and_uses_input_token_limit_only_when_provided() {
    let capture = GeminiCapture::default();
    let app = Router::new()
        .route("/v1beta/models", get(gemini_models))
        .with_state(capture.clone());
    let (base, task) = spawn(app).await;

    let response = discover(
        &input(
            Dialect::Gemini,
            format!("{base}/v1beta/models"),
            "gemini-test-key",
        ),
        None,
        None,
    )
    .await
    .unwrap();
    task.abort();

    assert_eq!(response.base_url, format!("{base}/v1beta"));
    assert_eq!(response.models.len(), 2);
    assert_eq!(response.models[0].id, "gemini-first-002");
    assert_eq!(response.models[0].context_window, 1_048_576);
    assert_eq!(response.models[0].context_source, ContextSource::Provider);
    assert_eq!(response.models[0].supports_stream, Some(true));
    assert_eq!(response.models[1].id, "gemini-second-001");
    assert_eq!(response.models[1].context_window, 65_536);
    assert_eq!(response.models[1].supports_stream, None);
    assert_eq!(response.warnings, Vec::<String>::new());

    let requests = capture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0],
        GeminiRequest {
            page_token: None,
            page_size: Some("1000".into()),
            api_key: Some("gemini-test-key".into()),
        }
    );
    assert_eq!(
        requests[1],
        GeminiRequest {
            page_token: Some("next-page".into()),
            page_size: Some("1000".into()),
            api_key: Some("gemini-test-key".into()),
        }
    );
}

async fn malformed_models() -> Json<Value> {
    Json(json!({
        "data": { "not": "an array" },
        "server_body_secret": "must-not-escape"
    }))
}

#[tokio::test]
async fn malformed_catalog_and_unsafe_url_fail_without_echoing_secrets() {
    let (base, task) = spawn(Router::new().route("/v1/models", get(malformed_models))).await;
    let error = discover(
        &input(
            Dialect::OpenAI,
            format!("{base}/v1"),
            "client-secret-must-not-escape",
        ),
        None,
        None,
    )
    .await
    .expect_err("object data must not be accepted as a models list");
    task.abort();
    assert!(!error.contains("client-secret-must-not-escape"));
    assert!(!error.contains("server_body_secret"));
    assert!(!error.contains("must-not-escape"));

    let error = normalize_base_url(
        Dialect::OpenAI,
        "https://api.example.test/v1?key=query-secret-must-not-escape",
    )
    .expect_err("query parameters are not valid provider base URLs");
    assert!(!error.contains("query-secret-must-not-escape"));
    assert!(
        normalize_base_url(Dialect::OpenAI, "http://169.254.169.254/latest/meta-data").is_err()
    );
}

async fn ollama_tags() -> Json<Value> {
    Json(json!({
        "models": [
            { "name": "good:latest", "model": "Good model" },
            { "name": "bad:latest", "model": "Bad model" },
            { "name": "good:latest", "model": "Duplicate should be removed" }
        ]
    }))
}

async fn ollama_show(Json(body): Json<Value>) -> Response {
    match body.get("model").and_then(Value::as_str) {
        Some("good:latest") => Json(json!({
            "parameters": "temperature 0.7\nnum_ctx 8192",
            "capabilities": ["completion", "vision", "tools"],
            "model_info": { "llama.context_length": 32768 }
        }))
        .into_response(),
        _ => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "partial metadata failure body" })),
        )
            .into_response(),
    }
}

#[tokio::test]
async fn ollama_partial_show_failure_keeps_tag_models_with_explicit_default_metadata() {
    let app = Router::new()
        .route("/api/tags", get(ollama_tags))
        .route("/api/show", post(ollama_show));
    let (base, task) = spawn(app).await;

    let response = discover(
        &input(Dialect::Ollama, format!("{base}/api/chat"), ""),
        None,
        None,
    )
    .await
    .unwrap();
    task.abort();

    assert_eq!(response.base_url, base);
    assert_eq!(response.models.len(), 2);
    let good = response
        .models
        .iter()
        .find(|model| model.id == "good:latest")
        .unwrap();
    assert_eq!(good.context_window, 8_192);
    assert_eq!(good.context_source, ContextSource::Provider);
    assert_eq!(good.supports_tools, Some(true));
    assert_eq!(good.supports_vision, Some(true));
    let bad = response
        .models
        .iter()
        .find(|model| model.id == "bad:latest")
        .unwrap();
    assert_eq!(bad.context_window, DEFAULT_CONTEXT_WINDOW);
    assert_eq!(bad.context_source, ContextSource::Default);
    assert_eq!(bad.supports_tools, None);
    assert_eq!(bad.supports_vision, None);
    assert_eq!(bad.supports_stream, None);
    assert!(response
        .warnings
        .iter()
        .any(|warning| warning.contains("1 个 Ollama")));
}

#[test]
fn saved_key_reuse_requires_the_same_normalized_url_and_dialect() {
    let saved = provider(
        Dialect::OpenAI,
        "https://127.0.0.1:18443/v1".into(),
        "encrypted-value-not-read-by-this-test",
    );
    let same = normalize_base_url(
        Dialect::OpenAI,
        "https://127.0.0.1:18443/v1/chat/completions",
    )
    .unwrap();
    let changed_scheme = normalize_base_url(
        Dialect::OpenAI,
        "http://127.0.0.1:18443/v1/chat/completions",
    )
    .unwrap();

    assert!(matches_saved_provider_target(
        &saved,
        Dialect::OpenAI,
        &same
    ));
    assert!(!matches_saved_provider_target(
        &saved,
        Dialect::OpenAI,
        &changed_scheme
    ));
    assert!(!matches_saved_provider_target(
        &saved,
        Dialect::Anthropic,
        &same
    ));
}

async fn assert_live_catalog(dialect: Dialect, base_url: &str, key_variable: &str) {
    let api_key = std::env::var(key_variable).unwrap_or_else(|_| panic!("缺少 {key_variable}"));
    let response = discover(&input(dialect, base_url.into(), &api_key), None, None)
        .await
        .unwrap_or_else(|_| panic!("{key_variable} 对应的模型目录读取失败"));
    assert!(
        !response.models.is_empty(),
        "{key_variable} 对应的模型目录没有可用模型"
    );
}

#[tokio::test]
#[ignore = "需要用户明确提供本机 OpenRouter Key；只读模型目录且不会输出 Key"]
async fn live_openrouter_catalog_is_read_only() {
    assert_live_catalog(
        Dialect::OpenAI,
        "https://openrouter.ai/api/v1",
        "LLMGW_LIVE_OPENROUTER_KEY",
    )
    .await;
}

#[tokio::test]
#[ignore = "需要用户明确提供本机 SenseNova Key；只读模型目录且不会输出 Key"]
async fn live_sensenova_catalog_is_read_only() {
    assert_live_catalog(
        Dialect::OpenAI,
        "https://token.sensenova.cn/v1",
        "LLMGW_LIVE_SENSENOVA_KEY",
    )
    .await;
}
