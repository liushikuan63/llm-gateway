//! C2 Gemini 原生入站的端到端验收。
//!
//! 起真 mock 上游 + 真网关，用 Gemini 的路径形态发请求。
//!
//! 对照用例（卡片点名）是关键：同一个内部请求经 **OpenAI 入站**与
//! **Gemini 入站**打到同一个上游，必须产生**同一种上游行为**。
//! 只测 Gemini 一侧的话，「它是不是走了同一条 dispatch」无从判断 ——
//! 旁路同样能让单侧用例全绿。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::config::AppConfig;
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone, Default)]
struct MockState {
    calls: Arc<AtomicUsize>,
    /// 收到的上游请求体，供对照用例比对。
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
}

fn model(alias: &str, vision: bool) -> ModelRef {
    ModelRef {
        enabled: true,
        alias: alias.into(),
        upstream: alias.into(),
        context_window: 32_768,
        supports_tools: true,
        supports_vision: vision,
        supports_audio: false,
        supports_video: false,
        supports_thinking: false,
        supports_stream: true,
        model_type: llm_gateway_lib::domain::ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: None,
        local: None,
    }
}

fn provider(base_url: String) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: "mock".into(),
        name: "mock".into(),
        dialect: Dialect::OpenAI,
        base_url,
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        models: vec![model("plain-model", false), model("vision-model", true)],
        rpm_limit: 0,
        intelligence: 80,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

async fn mock_upstream(state: MockState) -> Router {
    Router::new().fallback(post(move |Json(body): Json<serde_json::Value>| {
        let state = state.clone();
        async move {
            state.calls.fetch_add(1, Ordering::SeqCst);
            state.bodies.lock().unwrap().push(body.clone());
            let model = body
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or("plain-model")
                .to_string();
            // **必须按 stream 分流**：对 `stream:true` 回普通 JSON 会让上游
            // SSE 解析器读到 EOF 而没有任何事件，报「在输出首个事件前结束」——
            // 症状看起来像网关坏了，其实是夹具形状不对。
            // （B1 的 response_cache 夹具踩过同一个坑。）
            if body.get("stream").and_then(|s| s.as_bool()) == Some(true) {
                let sse = format!(
                    "data: {}\n\ndata: {}\n\ndata: {}\n\n",
                    serde_json::json!({
                        "id": "chatcmpl-mock", "object": "chat.completion.chunk",
                        "model": model,
                        "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
                    }),
                    serde_json::json!({
                        "id": "chatcmpl-mock", "object": "chat.completion.chunk",
                        "model": model,
                        "choices": [{"index": 0, "delta": {"content": "你好"}, "finish_reason": null}]
                    }),
                    serde_json::json!({
                        "id": "chatcmpl-mock", "object": "chat.completion.chunk",
                        "model": model,
                        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
                    }),
                );
                // 上游是 **OpenAI 方言**，它的流以 `data: [DONE]` 结束。
                // 少了这个哨兵，网关会认为上游异常终止并回 502 ——
                // 报错指向的是「上游返回 502」，跟「少了一行」看不出关系。
                // （同款见 tests/response_cache.rs:348。）
                let sse = format!("{sse}data: [DONE]\n\n");
                return (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    sse,
                )
                    .into_response();
            }
            Json(serde_json::json!({
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "你好"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
            }))
            .into_response()
        }
    }))
}

async fn unused_loopback_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn wait_for_gateway(base_url: &str) {
    // 必须 no_proxy，否则走系统代理时每轮等到超时（B2 实测 411 秒）
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for _ in 0..200 {
        if client
            .get(format!("{base_url}/healthz"))
            .send()
            .await
            .is_ok_and(|r| r.status() == reqwest::StatusCode::OK)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("网关没在预期时间内起来");
}

const KEY: &str = "lgw-gemini-test-key";

struct Harness {
    upstream: MockState,
    base_url: String,
    _db: db::Db,
    _task: JoinHandle<()>,
}

async fn spawn() -> Harness {
    let upstream_state = MockState::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = mock_upstream(upstream_state.clone()).await;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    let database = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(database.pool(), &provider(format!("http://{addr}")))
        .await
        .unwrap();

    let config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: KEY.into(),
        ..Default::default()
    };
    let gateway = Arc::new(GatewayState::new(database.clone(), config.clone()));
    gateway.reload_providers().await.unwrap();
    let task = tokio::spawn({
        let gateway = gateway.clone();
        async move {
            let _ = serve(gateway).await;
        }
    });
    let base_url = format!("http://{}:{}", config.bind, config.port);
    wait_for_gateway(&base_url).await;

    Harness {
        upstream: upstream_state,
        base_url,
        _db: database,
        _task: task,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("构造 client")
}

/// 发一个 Gemini 原生请求。`model_action` 形如 `models/gemini-2.5-pro:generateContent`。
async fn gemini_post(
    base_url: &str,
    model_action: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    client()
        .post(format!("{base_url}/v1beta/{model_action}"))
        .bearer_auth(KEY)
        .json(&body)
        .send()
        .await
        .expect("发送 Gemini 请求")
}

fn text_body(text: &str) -> serde_json::Value {
    serde_json::json!({
        "contents": [{"role": "user", "parts": [{"text": text}]}]
    })
}

// ------------------------------ 非流式 ------------------------------

#[tokio::test]
async fn generatecontent_返回_gemini_原生结构而不是_openai_的_choices() {
    // 【卡片原文的用例名是 `generatecontent_返回_choices_结构`】
    //
    // 这里刻意**不**照抄那个名字：`/v1beta/models/{m}:generateContent` 的
    // 契约是 Gemini 原生的 `candidates[].content.parts[]`，不是 OpenAI 的
    // `choices`。Gemini CLI 的解析器按同名形状读，回 `choices` 会得到
    // 「响应为空」而**不是报错** —— 那种用例就算绿了也是在钉一个错的契约。
    //
    // 判据同时断言两件事：有 `candidates`、**没有** `choices`。
    let h = spawn().await;
    let resp = gemini_post(
        &h.base_url,
        "models/plain-model:generateContent",
        text_body("你好"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert!(
        body.get("candidates").is_some(),
        "Gemini 原生的形状是 candidates：{body}"
    );
    assert!(
        body.get("choices").is_none(),
        "不能回 OpenAI 的 choices：{body}"
    );
    assert_eq!(body["candidates"][0]["content"]["role"], "model");
    assert_eq!(body["candidates"][0]["content"]["parts"][0]["text"], "你好");
    assert_eq!(body["candidates"][0]["finishReason"], "STOP");
    assert_eq!(body["usageMetadata"]["promptTokenCount"], 5);
    assert_eq!(body["usageMetadata"]["totalTokenCount"], 7);
}

#[tokio::test]
async fn 入站路径带_models_前缀能正确解析模型名() {
    let h = spawn().await;
    // 标准形态：路由前缀 `/v1beta/models/` 之后是裸模型名
    let standard = gemini_post(
        &h.base_url,
        "models/plain-model:generateContent",
        text_body("a"),
    )
    .await;
    assert_eq!(standard.status(), 200, "标准路径必须能解析");

    // 卡片提醒的形态：`{model}` 自带 `models/` 前缀 ⇒ 路径里出现两次。
    // 用通配路由 + 剥前缀都要能走通，否则这条会 404。
    let doubled = gemini_post(
        &h.base_url,
        "models/models/plain-model:generateContent",
        text_body("b"),
    )
    .await;
    assert_eq!(
        doubled.status(),
        200,
        "`models/models/<名>` 也要能解析（卡片刻意提醒前缀可能出现两次）"
    );

    // 关键判据：上游收到的是**裸模型名**，不是 `models/plain-model`
    let bodies = h.upstream.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    for body in &bodies {
        let m = body.get("model").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !m.starts_with("models/"),
            "上游收到的模型名不该带 models/ 前缀：{m}"
        );
    }
}

#[tokio::test]
async fn 模型列表带_models_前缀() {
    let h = spawn().await;
    let resp = client()
        .get(format!("{}/v1beta/models", h.base_url))
        .bearer_auth(KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let models = body["models"].as_array().expect("要有 models 数组");
    assert!(!models.is_empty());
    for m in models {
        let name = m["name"].as_str().unwrap();
        assert!(
            name.starts_with("models/"),
            "返回的 name 必须带 models/ 前缀（客户端会原样拼回路径）：{name}"
        );
    }
}

// ------------------------------ 流式 ------------------------------

#[tokio::test]
async fn streamgeneratecontent_产出_sse_且以收尾帧结束() {
    let h = spawn().await;
    let resp = gemini_post(
        &h.base_url,
        "models/plain-model:streamGenerateContent?alt=sse",
        text_body("你好"),
    )
    .await;
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert_eq!(status, 200, "流式请求应当 200，实际 {status}：{text}");

    assert!(text.contains("data: "), "必须是 SSE：{text}");
    // 每一帧以空行分隔
    assert!(text.contains("\n\n"), "SSE 帧以空行结束：{text:?}");
    // **没有** OpenAI 的 [DONE] 哨兵
    assert!(
        !text.contains("[DONE]"),
        "Gemini 的 alt=sse 没有 [DONE]，加一个会让严格解析的客户端报错：{text}"
    );
    // 至少最后一帧带 finishReason
    let frames: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .collect();
    assert!(!frames.is_empty(), "应当有 data 帧：{text}");
    let last: serde_json::Value = serde_json::from_str(frames.last().unwrap()).unwrap();
    assert_eq!(
        last["candidates"][0]["finishReason"], "STOP",
        "最后一帧要带 finishReason（Gemini 靠它判结束）：{last}"
    );
    assert!(last["usageMetadata"].is_object(), "收尾帧要带用量：{last}");
}

// ------------------------------ 媒体 ------------------------------

#[tokio::test]
async fn 带_base64_图片的请求命中多模态模型() {
    let h = spawn().await;
    let body = serde_json::json!({
        "contents": [{"role": "user", "parts": [
            {"text": "这是什么"},
            {"inlineData": {"mimeType": "image/png", "data": "aGVsbG8="}}
        ]}]
    });
    // 用 `auto` 之外的方式不好确保命中多模态；这里直接打 vision-model，
    // 判据是「带图请求能正常走通」而不是被能力过滤拦下。
    let resp = gemini_post(&h.base_url, "models/vision-model:generateContent", body).await;
    assert_eq!(resp.status(), 200);

    // 上游收到的是 OpenAI 的 image_url 形态（data URL），说明媒体没被丢
    let bodies = h.upstream.bodies.lock().unwrap().clone();
    let raw = serde_json::to_string(&bodies).unwrap();
    assert!(
        raw.contains("data:image/png;base64,aGVsbG8="),
        "base64 图片必须转成 data URL 传给上游：{raw}"
    );
}

#[tokio::test]
async fn 带远程图片地址返回_model_capability_unavailable() {
    // 反例组：Gemini 原生不接受任意 http 图片地址。
    // **必须报错而不是悄悄丢掉** —— 丢媒体的后果是模型看不见图却照常回答。
    let h = spawn().await;
    let body = serde_json::json!({
        "contents": [{"role": "user", "parts": [
            {"text": "看看这张图"},
            {"fileData": {"mimeType": "image/png", "fileUri": "https://example.com/a.png"}}
        ]}]
    });
    let resp = gemini_post(&h.base_url, "models/vision-model:generateContent", body).await;
    assert_eq!(resp.status(), 400, "应当是 400 而不是 500");

    let text = resp.text().await.unwrap();
    assert!(
        text.contains("model_capability_unavailable"),
        "错误码必须是 model_capability_unavailable：{text}"
    );
    assert!(text.contains("example.com"), "要说清是哪个地址：{text}");
    // 且**没有**打到上游 —— 被拒的请求不该消耗配额
    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        0,
        "被拒的请求不该打到上游"
    );
}

// ------------------------------ 错误路径 ------------------------------

#[tokio::test]
async fn 未知模型名返回_明确错误而不是_500() {
    let h = spawn().await;
    let resp = gemini_post(
        &h.base_url,
        "models/根本不存在:generateContent",
        text_body("hi"),
    )
    .await;
    let status = resp.status();
    assert_ne!(status, 500, "客户端传错了，不是网关坏了");
    assert!(
        status == 404 || status == 400,
        "应当是 404 或 400，实际 {status}"
    );
    let text = resp.text().await.unwrap();
    assert!(
        text.contains("根本不存在"),
        "错误里要点出是哪个模型：{text}"
    );
}

#[tokio::test]
async fn 不支持的动作返回明确错误() {
    let h = spawn().await;
    let resp = gemini_post(
        &h.base_url,
        "models/plain-model:countTokens",
        text_body("hi"),
    )
    .await;
    assert_ne!(resp.status(), 500);
    let text = resp.text().await.unwrap();
    assert!(text.contains("countTokens"), "要说清是哪个动作：{text}");
}

#[tokio::test]
async fn 路径不是_模型冒号动作_的形态时返回_404() {
    let h = spawn().await;
    // 没有冒号
    let resp = gemini_post(&h.base_url, "models/plain-model", text_body("hi")).await;
    let status = resp.status();
    assert!(
        status == 404 || status == 400,
        "应当是明确的 404/400，实际 {status}"
    );
}

// ------------------------------ 对照用例 ------------------------------

#[tokio::test]
async fn 同一请求经_openai_入站与_gemini_入站_得到同一上游行为() {
    // 卡片点名的对照用例。判据是「上游收到的请求体在关键字段上一致」——
    // 只测 Gemini 一侧的话，它是不是走了同一条 dispatch 无从判断，
    // 而旁路同样能让单侧用例全绿。
    let h = spawn().await;

    let openai = client()
        .post(format!("{}/v1/chat/completions", h.base_url))
        .bearer_auth(KEY)
        .json(&serde_json::json!({
            "model": "plain-model",
            "messages": [{"role": "user", "content": "同样的内容"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(openai.status(), 200);

    let gemini = gemini_post(
        &h.base_url,
        "models/plain-model:generateContent",
        text_body("同样的内容"),
    )
    .await;
    assert_eq!(gemini.status(), 200);

    let bodies = h.upstream.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2, "两次请求都要打到上游");

    // 上游看到的模型名必须一致
    let m0 = bodies[0]
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let m1 = bodies[1]
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(m0, m1, "两个入站面必须解析出同一个上游模型名");

    // 上游看到的最后一条 user 文本必须一致
    let last_user = |body: &serde_json::Value| -> String {
        body.get("messages")
            .and_then(|m| m.as_array())
            .and_then(|arr| {
                arr.iter()
                    .rev()
                    .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            })
            .and_then(|m| m.get("content"))
            .map(|c| c.to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        last_user(&bodies[0]),
        last_user(&bodies[1]),
        "两个入站面必须产生同一条上游消息：{:?} vs {:?}",
        bodies[0],
        bodies[1]
    );
    assert!(last_user(&bodies[1]).contains("同样的内容"));

    // 两个入站面都要走流式开关：非流式请求不该带 stream=true
    for body in &bodies {
        assert_ne!(
            body.get("stream").and_then(|s| s.as_bool()),
            Some(true),
            "非流式请求不该向上游要流：{body}"
        );
    }
}
