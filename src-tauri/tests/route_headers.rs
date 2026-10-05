//! 智能模式、提示词预优化与联网搜索的端到端验收。
//!
//! 这一批不测纯函数，而是**真起一个网关、真发 HTTP 请求、真读响应头**。
//! 单元测试证明不了「响应头到底有没有发出去」，而这条恰恰是模式隔离的关键。
//!
//! 每条用例都配了对照组：
//! - 开：断言 `X-Route-Intent` / `X-Route-Classifier` 真的出现且取值合理；
//! - 关：断言这五个头**一个都不出现**。
//!
//! 只有两边都断言，「关掉后没有变化」才不是一句空话。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::response::IntoResponse;
use axum::routing::{any, post};
use axum::{Json, Router};
use llm_gateway_lib::config::{AppConfig, RoutingStrategy};
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const KEY: &str = "e2e-route-key";

/// 五个诊断头。名称全小写：HTTP 头不区分大小写，但断言统一小写更好读。
const ROUTE_HEADERS: [&str; 5] = [
    "x-route-intent",
    "x-route-classifier",
    "x-route-search",
    "x-route-search-hits",
    "x-route-refined",
];

#[derive(Clone, Default)]
struct MockState {
    /// 上游收到的全部请求体。用来验证「联网搜索把结果注入了上下文」。
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    hits: Arc<AtomicUsize>,
}

async fn mock_openai_chat(state: MockState) -> Router {
    Router::new().fallback(post(move |Json(body): Json<serde_json::Value>| {
        let state = state.clone();
        async move {
            state.hits.fetch_add(1, Ordering::SeqCst);
            state.requests.lock().unwrap().push(body);
            Json(serde_json::json!({
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "model": "mock-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 11, "completion_tokens": 2, "total_tokens": 13},
            }))
        }
    }))
}

fn model(alias: &str, vision: bool, thinking: bool) -> ModelRef {
    ModelRef {
        enabled: true,
        alias: alias.into(),
        upstream: alias.into(),
        context_window: 32_768,
        supports_tools: true,
        supports_vision: vision,
        supports_audio: false,
        supports_video: false,
        supports_thinking: thinking,
        supports_stream: true,
        model_type: llm_gateway_lib::domain::ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: None,
        local: None,
    }
}

fn provider(id: &str, base_url: String, models: Vec<ModelRef>) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: id.into(),
        dialect: Dialect::OpenAI,
        base_url,
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        models,
        rpm_limit: 0,
        intelligence: 80,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

async fn unused_loopback_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn wait_for_gateway(base_url: &str) {
    let client = reqwest::Client::new();
    for _ in 0..120 {
        if client
            .get(format!("{base_url}/healthz"))
            .send()
            .await
            .is_ok_and(|response| response.status() == reqwest::StatusCode::OK)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gateway did not start in time");
}

async fn spawn_upstream() -> (String, MockState) {
    let state = MockState::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = mock_openai_chat(state.clone()).await;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    (format!("http://{addr}"), state)
}

/// 起一个真网关。`mutate` 用来打开/关闭智能模式与搜索。
async fn spawn_gateway(
    upstream: &str,
    mutate: impl FnOnce(&mut AppConfig),
) -> (db::Db, AppConfig, JoinHandle<()>, String) {
    let db = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(
        db.pool(),
        &provider(
            "mock",
            upstream.to_owned(),
            vec![
                model("vision-model", true, false),
                model("thinker", false, true),
            ],
        ),
    )
    .await
    .unwrap();
    let mut config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: KEY.into(),
        ..Default::default()
    };
    mutate(&mut config);
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

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("构造 client")
}

async fn chat(base_url: &str, model: &str, messages: serde_json::Value) -> reqwest::Response {
    let response = client()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(&serde_json::json!({ "model": model, "messages": messages }))
        .send()
        .await
        .expect("请求网关");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "网关返回了错误：{:?}",
        response.text().await.unwrap_or_default()
    );
    response
}

fn text_messages(text: &str) -> serde_json::Value {
    serde_json::json!([{ "role": "user", "content": text }])
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(decode_header_value)
}

/// 响应头里的非 ASCII 段是 `%XX` 编码的（见 `parse_header` / `encode_header_value`）。
fn decode_header_value(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn route_headers(response: &reqwest::Response) -> Vec<(&'static str, String)> {
    ROUTE_HEADERS
        .iter()
        .filter_map(|name| header(response, name).map(|value| (*name, value)))
        .collect()
}

/* ------------------------------------------------------------------ */

#[tokio::test]
async fn 智能模式关闭时四个诊断头一个都不出现() {
    // **对照组**。只有这条断言成立，下面「开」的用例才有意义：
    // 否则「开的时候有头」可能只是因为网关一直在发这两个头。
    let (upstream, state) = spawn_upstream().await;
    let (_db, _cfg, _task, base) = spawn_gateway(&upstream, |_| {}).await;

    let response = chat(&base, "auto", text_messages("帮我设计一个分布式限流器")).await;
    assert_eq!(state.hits.load(Ordering::SeqCst), 1, "上游应被调用一次");
    assert!(
        route_headers(&response).is_empty(),
        "功能关闭时不得出现任何 X-Route-* 头，实际出现了 {:?}",
        route_headers(&response)
    );
}

#[tokio::test]
async fn 智能模式开启时含图请求被标为_vision() {
    let (upstream, _state) = spawn_upstream().await;
    let (_db, _cfg, _task, base) = spawn_gateway(&upstream, |cfg| {
        cfg.smart_routing.enabled = true;
        cfg.routing_strategy = RoutingStrategy::Smart;
        // Jev 端点留空：这一条要验证的是硬规则，分类根本不该走到 Jev。
        cfg.smart_routing.jev.base_url = String::new();
    })
    .await;

    let messages = serde_json::json!([{
        "role": "user",
        "content": [
            {"type": "text", "text": "这张图里画的是什么？"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}},
        ],
    }]);
    let response = chat(&base, "auto", messages).await;

    assert_eq!(
        header(&response, "x-route-intent").as_deref(),
        Some("vision"),
        "含图请求必须走图像通道"
    );
    assert_eq!(
        header(&response, "x-route-classifier").as_deref(),
        Some("rule"),
        "硬规则命中时不应再去问 Jev"
    );
    // 搜索没开：后端与条数两个头一个都不该有。
    assert_eq!(header(&response, "x-route-search"), None);
    assert_eq!(header(&response, "x-route-search-hits"), None);
}

#[tokio::test]
async fn 智能模式开启时纯文本请求由启发式判定并回出分类来源() {
    let (upstream, _state) = spawn_upstream().await;
    let (_db, _cfg, _task, base) = spawn_gateway(&upstream, |cfg| {
        cfg.smart_routing.enabled = true;
        cfg.routing_strategy = RoutingStrategy::Smart;
        cfg.smart_routing.jev.base_url = String::new();
    })
    .await;

    let response = chat(
        &base,
        "auto",
        text_messages("帮我设计一个分布式限流器，需要考虑故障转移和一致性"),
    )
    .await;

    let intent = header(&response, "x-route-intent").expect("开启后必须回判定结果");
    assert!(
        intent == "reasoning" || intent == "simple",
        "纯文本请求只能判成 simple 或 reasoning，实际 {intent}"
    );
    assert_eq!(
        header(&response, "x-route-classifier").as_deref(),
        Some("heuristic"),
        "Jev 端点为空时应明确回落到启发式，而不是沉默"
    );
}

#[tokio::test]
async fn 客户端点名_smart_虚拟名在总开关关着时不产生任何头() {
    // 半吊子状态的反例：全局策略不是 smart、总开关关着，
    // 但客户端硬写了 model=smart。此时不得出现任何诊断头。
    let (upstream, _state) = spawn_upstream().await;
    let (_db, _cfg, _task, base) = spawn_gateway(&upstream, |_| {}).await;

    let response = chat(&base, "smart", text_messages("你好")).await;
    assert!(
        route_headers(&response).is_empty(),
        "总开关关着时 `smart` 虚拟名不得产生任何头，实际 {:?}",
        route_headers(&response)
    );
}

#[tokio::test]
async fn 启用搜索但后端不可达时请求照常发出且标为_failed() {
    // 搜索失败**不得阻断请求**：这条是「搜索失败不阻断」这条硬约束的端到端证据。
    let (upstream, state) = spawn_upstream().await;
    let (_db, _cfg, _task, base) = spawn_gateway(&upstream, |cfg| {
        cfg.smart_routing.enabled = true;
        cfg.routing_strategy = RoutingStrategy::Smart;
        cfg.search.enabled = true;
        cfg.search.backend = llm_gateway_lib::config::SearchBackendKind::SearXng;
        // 指向一个必然连不上的回环端口。
        cfg.search.searxng_url = Some("http://127.0.0.1:1/".into());
        cfg.search.timeout_ms = 600;
        cfg.smart_routing.jev.base_url = String::new();
    })
    .await;

    let response = chat(
        &base,
        "auto",
        text_messages("今天最新股价是多少？帮我查一下"),
    )
    .await;

    assert_eq!(
        state.hits.load(Ordering::SeqCst),
        1,
        "搜索失败后请求仍必须发往上游"
    );
    assert_eq!(
        header(&response, "x-route-search").as_deref(),
        Some("failed"),
        "后端不可达必须如实标 failed，不能装作没开过"
    );
    assert_eq!(
        header(&response, "x-route-search-hits").as_deref(),
        Some("0"),
        "失败时命中条数必须写 0，调用方才能区分「没搜到」与「后端挂了」"
    );
}

#[tokio::test]
async fn 不需要联网的请求不会触发搜索() {
    let (upstream, state) = spawn_upstream().await;
    let (_db, _cfg, _task, base) = spawn_gateway(&upstream, |cfg| {
        cfg.smart_routing.enabled = true;
        cfg.routing_strategy = RoutingStrategy::Smart;
        cfg.search.enabled = true;
        cfg.search.backend = llm_gateway_lib::config::SearchBackendKind::SearXng;
        cfg.search.searxng_url = Some("http://127.0.0.1:1/".into());
        cfg.smart_routing.jev.base_url = String::new();
    })
    .await;

    let response = chat(&base, "auto", text_messages("把这个变量改个名字")).await;

    assert_eq!(state.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        header(&response, "x-route-search"),
        None,
        "没有触发检索时不得出现 X-Route-Search"
    );
}

#[tokio::test]
async fn 搜索命中时结果被注入到上游请求的上下文里() {
    // 起一个 SearXNG 形状的 mock，再用真预取链路跑一遍：
    // 断言注入的文本真的进了上游的 messages。
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let searx_addr = listener.local_addr().unwrap();
    let searx_hits = hits.clone();
    // 用 `any`：SearXNG 后端发的是 **GET** `/search?q=…&format=json`。
    // 只挂 post 会拿到 axum 的 405，用例就变成在测「我写错的 mock」而不是在测预取链路。
    let searx = Router::new().fallback(any(move || {
        let hits = searx_hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({
                "results": [
                    {"title": "限流器设计", "url": "https://example.com/rl", "content": "令牌桶与漏桶", "score": 0.9},
                    {"title": "故障转移", "url": "https://example.com/fo", "content": "半开探测", "score": 0.8},
                ]
            }))
        }
    }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, searx).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    let (upstream, state) = spawn_upstream().await;
    let (_db, _cfg, _task, base) = spawn_gateway(&upstream, |cfg| {
        cfg.smart_routing.enabled = true;
        cfg.routing_strategy = RoutingStrategy::Smart;
        cfg.search.enabled = true;
        cfg.search.backend = llm_gateway_lib::config::SearchBackendKind::SearXng;
        cfg.search.searxng_url = Some(format!("http://{searx_addr}"));
        cfg.search.max_results = 5;
        cfg.search.timeout_ms = 3000;
        cfg.smart_routing.jev.base_url = String::new();
    })
    .await;

    let response = chat(&base, "auto", text_messages("最新股价是多少？帮我查一下")).await;

    assert_eq!(hits.load(Ordering::SeqCst), 1, "SearXNG mock 应被调用一次");
    assert_eq!(
        header(&response, "x-route-search").as_deref(),
        Some("searxng")
    );
    assert_eq!(
        header(&response, "x-route-search-hits").as_deref(),
        Some("2"),
        "两条结果都必须计入条数"
    );

    let seen = state.requests.lock().unwrap().clone();
    let injected = seen
        .iter()
        .flat_map(|body| body["messages"].as_array().cloned().unwrap_or_default())
        .filter_map(|message| message["content"].as_str().map(str::to_owned))
        .find(|content| content.contains("令牌桶与漏桶"))
        .expect("检索结果文本必须出现在上游收到的消息里");
    assert!(
        injected.contains("https://example.com/rl"),
        "注入内容必须带可点击的来源链接，实际：{injected}"
    );
    assert!(
        !injected.contains("Bearer"),
        "注入内容是资料不是指令，不应出现任何凭据"
    );
}

#[tokio::test]
async fn 审计落库时记下了判定与搜索结果() {
    let (upstream, _state) = spawn_upstream().await;
    let (db, _cfg, _task, base) = spawn_gateway(&upstream, |cfg| {
        cfg.smart_routing.enabled = true;
        cfg.routing_strategy = RoutingStrategy::Smart;
        cfg.smart_routing.jev.base_url = String::new();
    })
    .await;

    let response = chat(
        &base,
        "auto",
        text_messages("帮我设计一个分布式限流器，需要考虑故障转移和一致性"),
    )
    .await;
    let intent = header(&response, "x-route-intent").expect("开启后必须回判定结果");

    // 等审计任务落库：它是 spawn 出去的异步写。
    // `recent_requests` 返回的是视图 JSON，路由字段是**平铺**的
    // route_intent / route_classifier / route_search / route_search_hits。
    let mut row: Option<serde_json::Value> = None;
    for _ in 0..60 {
        if let Some(found) = repo::recent_requests(db.pool(), 10)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|row| row["route_intent"].is_string())
        {
            row = Some(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let row = row.expect("审计里必须留下判定来源，否则事后无法解释为什么这么选模");
    assert_eq!(row["route_intent"].as_str(), Some(intent.as_str()));
    assert_eq!(row["route_classifier"].as_str(), Some("heuristic"));
    assert!(
        row["route_search"].is_null(),
        "没触发检索时审计里不应凭空记一个后端，实际 {:?}",
        row["route_search"]
    );
}

/* ------------------------------------------------------------------ */
/* 提示词预优化                                                        */

/// 一个不思考、**且不支持工具**的模型。
///
/// 「不支持工具」是关键：预优化选目标时用的 `RequiredCapabilities::default()`
/// 不要求工具，所以它能被选中；而**主请求带工具**时，硬约束会把它从候选链里剔除。
/// 这样同一个供应商就只可能当改写端点，绝不会被主请求选中——
/// 否则「改写端点被调用了几次」这条断言测的就不是改写，而是路由打分。
fn refine_model() -> ModelRef {
    let mut model = model("tiny", false, false);
    model.supports_tools = false;
    model
}

/// 起一个「改写端点」：对任何请求都回同一段改写后的文本。
async fn spawn_refiner(text: &'static str) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new().fallback(post(move |Json(_body): Json<serde_json::Value>| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": text}}]
            }))
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    (format!("http://{addr}"), hits)
}

/// 起一个**连得上但总是报错**的改写端点。
///
/// 和「连不上」分开测：连不上的供应商会被健康检查从候选链里剔除，
/// 于是压根选不出改写目标——那是另一条路径（「没有可用改写模型」）。
async fn spawn_broken_refiner() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new().fallback(post(move || {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                headers,
                r#"{"error":"model overloaded"}"#,
            )
                .into_response()
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    (format!("http://{addr}"), hits)
}

/// 起一个 Jev mock：`clarity_noul` 低 ⇒ 判定「需要改写」。
///
/// 这一步不能省：`needs_refine` **只有** Jev 被采纳时才有信号，
/// 启发式恒返回 false。所以想验改写链路就必须真的把 Jev 叫起来。
///
/// `complexity` 的分布刻意拉开（0.6 / 0.2 / 0.2，margin 0.4）：
/// margin 低于 `min_margin`（0.25）时 Jev 会**弃权**，改写路径根本不会触发，
/// 那这条 mock 就变成在测「Jev 弃权时会发生什么」——不是它该测的东西。
async fn spawn_jev(clarity_noul: f32) -> String {
    let app = Router::new().fallback(post(move || async move {
        Json(serde_json::json!({
            "answers": {
                "complexity": {
                    "type": "choice",
                    "choice": "moderate",
                    "probabilities": {"simple": 0.20, "moderate": 0.60, "complex": 0.20},
                    "confidence": 0.60
                },
                "clarity": {"type": "noul", "noul": clarity_noul}
            },
            "usage": {"input_tokens": 40, "output_tokens": 0}
        }))
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;
    format!("http://{addr}")
}

/// 造一个「改写端点 + 主上游 + Jev」的三方网关。
///
/// 主请求**一律带工具**，这样不支持工具的改写端点只可能当改写目标，
/// 不会被主路由选中（见 `refine_model` 的说明）。
async fn spawn_refine_gateway(
    refine_base: &str,
    jev_base: &str,
    mutate: impl FnOnce(&mut AppConfig),
) -> (db::Db, String, MockState) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let upstream_state = MockState::default();
    let state_for_app = upstream_state.clone();
    let app = Router::new().fallback(post(move |Json(body): Json<serde_json::Value>| {
        let state = state_for_app.clone();
        async move {
            state.hits.fetch_add(1, Ordering::SeqCst);
            state.requests.lock().unwrap().push(body);
            Json(serde_json::json!({
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "model": "thinker",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 11, "completion_tokens": 2, "total_tokens": 13},
            }))
        }
    }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    let db = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(
        db.pool(),
        &provider("refiner", refine_base.to_owned(), vec![refine_model()]),
    )
    .await
    .unwrap();
    // 主上游：会思考且支持工具，主请求只会打到它。
    repo::upsert_provider(
        db.pool(),
        &provider(
            "main",
            format!("http://{upstream_addr}"),
            vec![model("thinker", true, true)],
        ),
    )
    .await
    .unwrap();

    let mut config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: KEY.into(),
        ..Default::default()
    };
    config.smart_routing.enabled = true;
    config.routing_strategy = RoutingStrategy::Smart;
    config.smart_routing.jev.base_url = jev_base.to_owned();
    config.smart_routing.prompt_refine.provider_id = Some("refiner".into());
    config.smart_routing.prompt_refine.min_chars = 1;
    config.smart_routing.prompt_refine.timeout_ms = 3000;
    mutate(&mut config);
    let gateway = Arc::new(GatewayState::new(db.clone(), config.clone()));
    gateway.reload_providers().await.unwrap();
    let task = tokio::spawn({
        let gateway = gateway.clone();
        async move {
            let _ = serve(gateway).await;
        }
    });
    std::mem::forget(task); // 让网关活到测试结束
    let base = format!("http://{}:{}", config.bind, config.port);
    wait_for_gateway(&base).await;
    (db, base, upstream_state)
}

/// 带工具的请求体。**必须带工具**——这是让主路由排除掉改写端点的关键（见 `refine_model`）。
fn tool_messages(text: &str) -> serde_json::Value {
    serde_json::json!([
        {"role": "user", "content": text},
    ])
}

fn tool_body(model: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": tool_messages(text),
        "tools": [{
            "type": "function",
            "function": {"name": "noop", "description": "占位工具", "parameters": {"type": "object", "properties": {}}}
        }],
    })
}

/// 读出主上游收到的最后一条 user 文本。
fn last_user_text(upstream_state: &MockState) -> String {
    let seen = upstream_state.requests.lock().unwrap().clone();
    seen.last()
        .expect("主上游应收到请求")
        .get("messages")
        .and_then(|m| m.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .rev()
        .find_map(|m| {
            let role = m["role"].as_str().unwrap_or_default();
            (role == "user").then(|| m["content"].as_str().map(str::to_owned))?
        })
        .expect("主上游应收到一条 user 消息")
}

async fn chat_with_tools(base_url: &str, model: &str, text: &str) -> reqwest::Response {
    let response = client()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(&tool_body(model, text))
        .send()
        .await
        .expect("请求网关");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "网关返回了错误：{:?}",
        response.text().await.unwrap_or_default()
    );
    response
}

#[tokio::test]
async fn 改写成功后上游收到的是新提示词() {
    let (refine_base, refine_hits) = spawn_refiner("请检查边界条件，并说明在并发下的行为").await;
    let jev = spawn_jev(0.10).await; // clarity 极低 ⇒ 需要改写
    let (_db, base, upstream_state) = spawn_refine_gateway(&refine_base, &jev, |cfg| {
        cfg.smart_routing.prompt_refine.enabled = true;
    })
    .await;

    let original = "帮我看看这个函数在并发下会不会出问题";
    let response = chat_with_tools(&base, "auto", original).await;

    assert_eq!(
        refine_hits.load(Ordering::SeqCst),
        1,
        "改写端点应当被调用一次且只一次（改写不重试）"
    );
    assert_eq!(header(&response, "x-route-refined").as_deref(), Some("1"));
    let sent = last_user_text(&upstream_state);
    assert!(
        sent.contains("边界条件"),
        "上游必须收到改写后的文本，实际：{sent}"
    );
    assert!(
        !sent.contains("帮我看看这个函数"),
        "原文不该再出现在最后一条 user 消息里：{sent}"
    );
}

#[tokio::test]
async fn 改写关闭时上游收到的是原文() {
    let (refine_base, refine_hits) = spawn_refiner("请检查边界条件，并说明在并发下的行为").await;
    let jev = spawn_jev(0.10).await;
    let (_db, base, upstream_state) = spawn_refine_gateway(&refine_base, &jev, |cfg| {
        // Jev 说需要改写，但**开关关着**——这一条验的是开关本身。
        cfg.smart_routing.prompt_refine.enabled = false;
    })
    .await;

    let original = "帮我看看这个函数在并发下会不会出问题";
    let response = chat_with_tools(&base, "auto", original).await;

    assert_eq!(
        refine_hits.load(Ordering::SeqCst),
        0,
        "预优化关着时改写端点一次都不该被调用"
    );
    assert_eq!(
        header(&response, "x-route-refined"),
        None,
        "未触发改写时不得发这个头"
    );
    assert_eq!(
        last_user_text(&upstream_state),
        original,
        "原文必须逐字送达"
    );
}

#[tokio::test]
async fn jev_说清楚时不做改写() {
    // 反向对照：上一条可能因为「Jev 从来没说需要改写」而通过。
    let (refine_base, refine_hits) = spawn_refiner("请检查边界条件，并说明在并发下的行为").await;
    let jev = spawn_jev(0.95).await; // clarity 很高 ⇒ 提示词已经够清楚
    let (_db, base, upstream_state) = spawn_refine_gateway(&refine_base, &jev, |cfg| {
        cfg.smart_routing.prompt_refine.enabled = true;
    })
    .await;

    let original = "帮我看看这个函数在并发下会不会出问题";
    let response = chat_with_tools(&base, "auto", original).await;

    assert_eq!(
        header(&response, "x-route-classifier").as_deref(),
        Some("jev"),
        "反向对照：Jev 必须真的被采纳，否则「不不不不写」只是因为弃权，属于空断言"
    );
    assert_eq!(
        refine_hits.load(Ordering::SeqCst),
        0,
        "Jev 判定提示词清楚时不该浪费一次改写调用"
    );
    assert_eq!(last_user_text(&upstream_state), original);
}

#[tokio::test]
async fn 改写端点报错时请求照常走完且提示词未被污染() {
    // 改写失败**不得阻断请求**，更不得把空提示词发给上游。
    //
    // 用「连得上但回 500」而不是「连不上」：连不上的供应商会被健康检查剔除，
    // 于是压根选不出改写目标，测到的是「没有目标」而不是「改写失败」。
    // 这两种是不同的失败，混在一起会漏掉真正要验的那条路径。
    let (broken_base, broken_hits) = spawn_broken_refiner().await;
    let jev = spawn_jev(0.10).await;
    let (_db, base, upstream_state) = spawn_refine_gateway(&broken_base, &jev, |cfg| {
        cfg.smart_routing.prompt_refine.enabled = true;
        cfg.smart_routing.prompt_refine.timeout_ms = 2000;
    })
    .await;

    let original = "帮我看看这个函数在并发下会不会出问题";
    let response = chat_with_tools(&base, "auto", original).await;

    assert_eq!(
        broken_hits.load(Ordering::SeqCst),
        1,
        "改写端点确实被调用过（否则测的不是「失败」而是「没触发」）"
    );
    assert_eq!(
        upstream_state.hits.load(Ordering::SeqCst),
        1,
        "改写失败后请求仍必须发往主上游"
    );
    assert_eq!(header(&response, "x-route-refined").as_deref(), Some("0"));
    let note =
        header(&response, "x-route-refine-note").expect("没改成也要写明原因，否则界面上是空白");
    assert!(
        note.contains("HTTP 500"),
        "原因要具体到状态码，实际：{note}"
    );
    assert_eq!(
        last_user_text(&upstream_state),
        original,
        "改写失败必须逐字保留原文"
    );
}

#[tokio::test]
async fn 没有可用改写目标时明确说明而不是沉默() {
    // 反向用例：候选链里一个能改写的模型都没有时，`pick_target` 返回 None。
    // 这时**不能**什么都不记——界面上要能看出「没找到改写模型」，
    // 否则用户会以为预优化已经开了却一直没效果。
    let (refine_base, _hits) = spawn_refiner("改写结果").await;
    let jev = spawn_jev(0.10).await;
    let (db, base, _state) = spawn_refine_gateway(&refine_base, &jev, |cfg| {
        cfg.smart_routing.prompt_refine.enabled = true;
        // 指向一个不在候选链里的供应商：pick_target 会回落到全链，
        // 所以这里改用「模型名为空」来制造真正的 None。
        cfg.smart_routing.prompt_refine.model = Some("   ".into());
    })
    .await;

    let _ = chat_with_tools(&base, "auto", "帮我看看这个函数在并发下会不会出问题").await;

    // 只要请求本身成功即可：本条的重点是「不要崩、不要污染提示词」。
    let mut saw_note = false;
    for _ in 0..60 {
        if let Some(row) = repo::recent_requests(db.pool(), 10)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|row| row["route_refine_note"].is_string())
        {
            saw_note = row["route_refine_note"]
                .as_str()
                .is_some_and(|s| !s.is_empty());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(saw_note, "没找到改写模型也要在审计里写明原因");
}

#[tokio::test]
async fn 审计记录了改写前后的长度() {
    let (refine_base, _hits) = spawn_refiner("请检查边界条件，并说明在并发下的行为").await;
    let jev = spawn_jev(0.10).await;
    let (db, base, _state) = spawn_refine_gateway(&refine_base, &jev, |cfg| {
        cfg.smart_routing.prompt_refine.enabled = true;
    })
    .await;

    chat_with_tools(&base, "auto", "帮我看看这个函数在并发下会不会出问题").await;

    let mut note: Option<String> = None;
    for _ in 0..60 {
        let rows = repo::recent_requests(db.pool(), 10)
            .await
            .unwrap_or_default();
        if let Some(row) = rows
            .into_iter()
            .find(|row| row["route_refined"].is_boolean())
        {
            note = row["route_refine_note"].as_str().map(str::to_owned);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let note = note.expect("改写结果必须落审计，否则事后无法核对改写了什么");
    assert!(note.contains('→'), "审计应记录改写前后的长度，实际：{note}");
}
