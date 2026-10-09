//! B1 精确响应缓存的验收测试。
//!
//! 用 axum mock 上游（模式抄自 `tests/route_headers.rs`），起一个真网关，
//! 用真 HTTP 请求走完整链路。
//!
//! **每组「开启时」的用例都配了一条「关闭时」的反例** ——
//! 只断言「开启时出现 X」是不够的：如果那段代码一直在发 X，
//! 断言照样绿，而缓存其实没生效。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::config::AppConfig;
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 夹具的判定口径与真实上游对齐：网关发的是 POST + JSON body，
/// 返回裸的 chat.completion 对象（不额外包一层）。
#[derive(Clone, Default)]
struct MockState {
    calls: Arc<AtomicUsize>,
    responses: Arc<Mutex<Vec<serde_json::Value>>>,
    pause_first: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
}

async fn mock_upstream(state: MockState) -> Router {
    Router::new().fallback(post(move |Json(body): Json<serde_json::Value>| {
        let state = state.clone();
        async move {
            let n = state.calls.fetch_add(1, Ordering::SeqCst);
            state.responses.lock().unwrap().push(body.clone());
            if n == 0 {
                if let Some((started, release)) = &state.pause_first {
                    started.notify_one();
                    release.notified().await;
                }
            }
            // 每次返回的 id 都不同：这样「第二次拿到的是缓存还是新响应」
            // 可以从 body 里直接读出来，而不是靠计数推断。
            //
            // 但缓存命中时返回的是**第一次**的 body，判断依据是内容相同。
            // 所以生成的 id 也要能被断言。id 随调用次数递增。
            Json(serde_json::json!({
                "id": format!("chatcmpl-{n}"),
                "object": "chat.completion",
                "model": "mock-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": format!("answer-{n}")},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 11, "completion_tokens": 2, "total_tokens": 13},
            }))
        }
    }))
}

fn model(alias: &str, tools: bool, vision: bool) -> ModelRef {
    ModelRef {
        enabled: true,
        alias: alias.into(),
        upstream: alias.into(),
        context_window: 32_768,
        supports_tools: tools,
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
        capabilities: None,
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
        runtime_id: None,
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

const KEY: &str = "lgw-cache-test-key";

struct Harness {
    upstream: MockState,
    gateway: Arc<GatewayState>,
    base_url: String,
    /// 保留 JoinHandle，drop 掉 harness 时任务仍在跑（tokio 运行时自己收）。
    _task: JoinHandle<()>,
    _db: db::Db,
}

/// 起一套「mock 上游 + 真网关」。`cache_enabled` 与 `mutate` 控制缓存与场景。
async fn spawn(cache_enabled: bool, mutate: impl FnOnce(&mut AppConfig)) -> Harness {
    spawn_with_mock(cache_enabled, mutate, MockState::default()).await
}

async fn spawn_with_mock(
    cache_enabled: bool,
    mutate: impl FnOnce(&mut AppConfig),
    upstream_state: MockState,
) -> Harness {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = mock_upstream(upstream_state.clone()).await;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    let database = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(
        database.pool(),
        &provider(
            "mock",
            format!("http://{addr}"),
            vec![
                model("plain-model", false, false),
                model("tool-model", true, false),
                model("vision-model", false, true),
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
    config.cache.enabled = cache_enabled;
    mutate(&mut config);

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
        gateway,
        base_url,
        _task: task,
        _db: database,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("构造 client")
}

async fn post_chat(base_url: &str, model: &str, messages: serde_json::Value) -> reqwest::Response {
    post_chat_extra(base_url, model, messages, serde_json::json!({})).await
}

async fn post_chat_extra(
    base_url: &str,
    model: &str,
    messages: serde_json::Value,
    extra: serde_json::Value,
) -> reqwest::Response {
    let mut body = serde_json::json!({ "model": model, "messages": messages });
    if let Some(obj) = body.as_object_mut() {
        if let Some(extra_obj) = extra.as_object() {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    client()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(&body)
        .send()
        .await
        .expect("请求网关")
}

fn text_messages(text: &str) -> serde_json::Value {
    serde_json::json!([{ "role": "user", "content": text }])
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// 缓存关闭时，`X-Cache*` 三个头**一个都不许出现**（铁律 2）。
///
/// `X-Cache-Reason` 与 `X-Cache-Key` 也要一起断言 —— 只查 `X-Cache`
/// 会漏掉「关着却还在发 key」这种半开状态。
#[tokio::test]
async fn 关闭时响应里不出现任何_x_cache_头() {
    let h = spawn(false, |_| {}).await;
    let resp = post_chat(&h.base_url, "plain-model", text_messages("你好")).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    for name in ["x-cache", "x-cache-reason", "x-cache-key"] {
        assert!(
            header(&resp, name).is_none(),
            "缓存关闭时不该出现 {name}，实际 {:?}",
            header(&resp, name)
        );
    }
    // 对照组：上游确实被调用了，所以「没有头」不是因为请求没走通
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 1);
}

/// 开启后第二次命中，且**上游只被调用一次**（计数型断言）。
#[tokio::test]
async fn 开启时相同请求第二次命中且上游只被调用一次() {
    let h = spawn(true, |_| {}).await;
    let msgs = text_messages("同一个问题");

    let first = post_chat(&h.base_url, "plain-model", msgs.clone()).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    assert_eq!(
        header(&first, "x-cache").as_deref(),
        Some("MISS"),
        "第一次必须 MISS"
    );
    assert!(
        header(&first, "x-cache-key").is_some(),
        "MISS 也要带 key，否则排查时无从下手"
    );
    let first_body: serde_json::Value = first.json().await.unwrap();

    let second = post_chat(&h.base_url, "plain-model", msgs.clone()).await;
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    assert_eq!(
        header(&second, "x-cache").as_deref(),
        Some("HIT"),
        "相同请求第二次必须 HIT"
    );
    let second_body: serde_json::Value = second.json().await.unwrap();

    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        1,
        "命中缓存时不该再打上游"
    );
    assert_eq!(
        first_body["choices"][0]["message"]["content"],
        second_body["choices"][0]["message"]["content"],
        "命中必须返回第一次的内容"
    );
}

#[tokio::test]
async fn 缓存命中后立即续接保留完整会话且不重复消费() {
    let h = spawn(true, |_| {}).await;
    let first = post_chat(&h.base_url, "plain-model", text_messages("first question")).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let first_sid = header(&first, "x-session-id").unwrap();
    first.bytes().await.unwrap();

    let hit = post_chat(&h.base_url, "plain-model", text_messages("first question")).await;
    assert_eq!(hit.status(), reqwest::StatusCode::OK);
    assert_eq!(header(&hit, "x-cache").as_deref(), Some("HIT"));
    let hit_sid = header(&hit, "x-session-id").unwrap();
    assert_ne!(hit_sid, first_sid, "匿名请求应分别获得独立会话");
    assert_eq!(
        header(&hit, "x-routed-via").as_deref(),
        Some("mock/plain-model")
    );
    assert_eq!(header(&hit, "x-fallback-attempts").as_deref(), Some("0"));
    hit.bytes().await.unwrap();
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 1);

    let history = repo::recent_messages(h._db.pool(), &hit_sid, 10)
        .await
        .unwrap();
    assert_eq!(history.len(), 2, "HIT 返回前必须提交本轮问答");
    assert_eq!(history[0].content, "first question");
    assert_eq!(history[1].content, "answer-0");
    let session = repo::get_or_create_session(h._db.pool(), &hit_sid)
        .await
        .unwrap();
    assert_eq!(session.total_tokens, 0, "缓存重放不增加上游 token 消费");
    assert_eq!(session.sticky_provider_id.as_deref(), Some("mock"));
    assert_eq!(session.sticky_model.as_deref(), Some("plain-model"));

    let next = client()
        .post(format!("{}/v1/chat/completions", h.base_url))
        .bearer_auth(KEY)
        .header("x-session-id", &hit_sid)
        .json(&serde_json::json!({"model": "plain-model", "messages": text_messages("follow up")}))
        .send()
        .await
        .unwrap();
    assert_eq!(next.status(), reqwest::StatusCode::OK);
    next.bytes().await.unwrap();
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 2);
    let requests = h.upstream.responses.lock().unwrap();
    assert_eq!(
        requests[1]["messages"],
        serde_json::json!([
            {"role": "user", "content": "first question"},
            {"role": "assistant", "content": "answer-0"},
            {"role": "user", "content": "follow up"}
        ]),
        "续接必须携带命中时保存的完整问答"
    );
}

#[tokio::test]
async fn 缓存命中写入零消费审计并关联本次_trace() {
    let h = spawn(true, |_| {}).await;
    let warm = post_chat(&h.base_url, "plain-model", text_messages("audit question")).await;
    assert_eq!(warm.status(), reqwest::StatusCode::OK);
    warm.bytes().await.unwrap();
    let trace_id = "abcdefabcdefabcdefabcdefabcdefab";
    let hit = client().post(format!("{}/v1/chat/completions", h.base_url))
        .bearer_auth(KEY)
        .header("x-trace-id", trace_id)
        .json(&serde_json::json!({"model": "plain-model", "messages": text_messages("audit question")}))
        .send().await.unwrap();
    assert_eq!(header(&hit, "x-cache").as_deref(), Some("HIT"));
    assert_eq!(header(&hit, "x-trace-id").as_deref(), Some(trace_id));
    let sid = header(&hit, "x-session-id").unwrap();
    hit.bytes().await.unwrap();
    let mut audit = None;
    for _ in 0..100 {
        audit = sqlx::query_as::<_, (String, String, String, i64, i64, f64, String)>(
            "SELECT session_id, routed_provider, routed_model, prompt_tokens, completion_tokens, cost, attempts_json FROM requests WHERE trace_id = ?"
        ).bind(trace_id).fetch_optional(h._db.pool()).await.unwrap();
        if audit.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (stored_sid, provider, model, prompt, completion, cost, attempts) =
        audit.expect("每次 HIT 必须存在可追踪的审计记录");
    assert_eq!(stored_sid, sid);
    assert_eq!(provider, "mock");
    assert_eq!(model, "plain-model");
    assert_eq!((prompt, completion, cost), (0, 0, 0.0));
    assert_eq!(attempts, "[]", "缓存命中没有新增上游尝试");
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 1);
}

/// 反例：同样两次请求，缓存**关闭**时上游必须被调用两次。
///
/// 没有这条，「上游只被调用一次」可能只是因为网关把请求去重了、
/// 或者第二次请求根本没发出去。
#[tokio::test]
async fn 关闭时相同请求会打两次上游() {
    let h = spawn(false, |_| {}).await;
    let msgs = text_messages("同一个问题");
    let a = post_chat(&h.base_url, "plain-model", msgs.clone()).await;
    assert_eq!(a.status(), reqwest::StatusCode::OK);
    let b = post_chat(&h.base_url, "plain-model", msgs.clone()).await;
    assert_eq!(b.status(), reqwest::StatusCode::OK);
    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        2,
        "缓存关着，两次请求必须都打上游"
    );
    // 而且两次内容不同（mock 每次换 id）—— 证明第二次不是复用了第一次的结果
    let ba: serde_json::Value = a.json().await.unwrap();
    let bb: serde_json::Value = b.json().await.unwrap();
    assert_ne!(
        ba["choices"][0]["message"]["content"],
        bb["choices"][0]["message"]["content"]
    );
}

/// 流式请求：返回 BYPASS 且不缓存。开/关两种情况都断言。
///
/// 这里必须用**真 SSE 上游**：mock 返回裸 JSON 时 `call_stream` 解析不出事件，
/// 网关会回 500，测到的是「上游形状不对」而不是缓存行为
/// （CLAUDE.md 第 9 条：mock 的返回结构要对齐真实）。
#[tokio::test]
async fn 流式请求返回_bypass_且不缓存() {
    use axum::response::IntoResponse;

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_route = calls.clone();
    let app = Router::new().fallback(post(move |Json(body): Json<serde_json::Value>| {
        let calls = calls_for_route.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            // 按请求的 `stream` 分流：真实上游就是这么做的。
            // 只返回 SSE 会让非流式请求解析失败，那时测到的是
            // 「上游形状不对」而不是缓存行为。
            let wants_stream = body
                .get("stream")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if wants_stream {
                let sse = concat!(
                    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"mock-model\",",
                    "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"讲\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"mock-model\",",
                    "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"故事\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"mock-model\",",
                    "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],",
                    "\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
                    "data: [DONE]\n\n"
                );
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    sse,
                )
                    .into_response()
            } else {
                Json(serde_json::json!({
                    "id": "chatcmpl-nonstream",
                    "object": "chat.completion",
                    "model": "mock-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "非流式回答"},
                        "finish_reason": "stop",
                    }],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7},
                }))
                .into_response()
            }
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    let database = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(
        database.pool(),
        &provider(
            "mock",
            format!("http://{addr}"),
            vec![model("plain-model", false, false)],
        ),
    )
    .await
    .unwrap();
    let mut config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: KEY.into(),
        ..Default::default()
    };
    config.cache.enabled = true;
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

    let resp = post_chat_extra(
        &base_url,
        "plain-model",
        text_messages("讲个故事"),
        serde_json::json!({"stream": true}),
    )
    .await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "流式请求本身要成功，否则测的是上游形状而不是缓存"
    );
    assert_eq!(
        header(&resp, "x-cache").as_deref(),
        Some("BYPASS"),
        "流式必须 BYPASS"
    );
    assert_eq!(
        header(&resp, "x-cache-reason").as_deref(),
        Some("streaming")
    );
    // 不该出现 key：没查过表就没有键
    assert!(header(&resp, "x-cache-key").is_none());
    drop(resp);

    // 反例组：同一个网关，非流式请求是能缓存的 ——
    // 证明「BYPASS」是流式这条路径特有的，不是缓存整体没工作
    let a = post_chat(&base_url, "plain-model", text_messages("讲个故事")).await;
    assert_eq!(header(&a, "x-cache").as_deref(), Some("MISS"));
    let b = post_chat(&base_url, "plain-model", text_messages("讲个故事")).await;
    assert_eq!(header(&b, "x-cache").as_deref(), Some("HIT"));
    // 上游被调用两次：流式一次 + 非流式首次一次
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    drop(task);
}

/// 请求带 tools → BYPASS(has_tools)。
#[tokio::test]
async fn 带_tools_的请求返回_bypass() {
    let h = spawn(true, |_| {}).await;
    let resp = post_chat_extra(
        &h.base_url,
        "tool-model",
        text_messages("查一下青岛天气"),
        serde_json::json!({
            "tools": [{"type": "function", "function": {"name": "weather", "parameters": {}}}]
        }),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(header(&resp, "x-cache").as_deref(), Some("BYPASS"));
    assert_eq!(
        header(&resp, "x-cache-reason").as_deref(),
        Some("has_tools")
    );

    // 反例：不带 tools 的同一句话是可缓存的
    let a = post_chat(&h.base_url, "tool-model", text_messages("查一下青岛天气")).await;
    assert_eq!(header(&a, "x-cache").as_deref(), Some("MISS"));
}

/// **响应**含 tool_calls 时不缓存。这一条与上一条不同：
/// 请求里没有 `tools` 字段，只有拿到响应才知道它带了工具调用。
#[tokio::test]
async fn 含_tool_calls_的响应不被缓存() {
    // 单独一个 mock：它无条件返回带 tool_calls 的响应
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_route = calls.clone();
    let app = Router::new().fallback(post(move |Json(_body): Json<serde_json::Value>| {
        let calls = calls_for_route.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({
                "id": "chatcmpl-tools",
                "object": "chat.completion",
                "model": "mock-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call-1",
                            "type": "function",
                            "function": {"name": "weather", "arguments": "{}"}
                        }]
                    },
                    "finish_reason": "tool_calls",
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6},
            }))
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    let database = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(
        database.pool(),
        &provider(
            "mock",
            format!("http://{addr}"),
            vec![model("plain-model", false, false)],
        ),
    )
    .await
    .unwrap();
    let mut config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: KEY.into(),
        ..Default::default()
    };
    config.cache.enabled = true;
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

    let msgs = text_messages("帮我查天气");
    let first = post_chat(&base_url, "plain-model", msgs.clone()).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    assert_eq!(
        header(&first, "x-cache").as_deref(),
        Some("BYPASS"),
        "响应含 tool_calls 必须降级为 BYPASS"
    );
    assert_eq!(
        header(&first, "x-cache-reason").as_deref(),
        Some("response_has_tool_calls")
    );

    let second = post_chat(&base_url, "plain-model", msgs.clone()).await;
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    assert_eq!(header(&second, "x-cache").as_deref(), Some("BYPASS"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "带 tool_calls 的响应不该被缓存，第二次必须再打上游"
    );
    drop(task);
}

/// 触发过搜索预取 → BYPASS(search_injected)。
///
/// 卡片原文是「触发了搜索预取注入的请求（**X-Route-Search 非空**）」——
/// 非空有两种取值：后端名（成功）或 `failed`（预取跑了但没拿到结果）。
/// 两种都算「这个请求被搜索链路碰过」，都不该缓存。
///
/// 这里刻意走 `failed` 那条：给一个连不上的搜索后端，预取必然失败。
/// 好处是不用去对齐某个真实搜索后端的请求形状与返回结构
/// （CLAUDE.md 第 9 条要求 mock 五项对齐，对齐错了测出来的是别的东西），
/// 而断言的不变量完全一样。
#[tokio::test]
async fn 触发过搜索预取的请求不被缓存() {
    // 127.0.0.1:1 是保留端口，连不上 → 预取必然失败
    let h = spawn(true, |cfg| {
        cfg.smart_routing.enabled = true;
        // 只开总开关不够：`smart_mode_active` 还要求「策略是 smart」或
        // 「客户端点名 smart」。少了这一行分类根本不会跑，搜索也就不会触发 ——
        // 第一版就是这样，断言拿到的是 `x-route-search: None`。
        cfg.routing_strategy = llm_gateway_lib::config::RoutingStrategy::Smart;
        cfg.search.enabled = true;
        cfg.search.searxng_url = Some("http://127.0.0.1:1".into());
        cfg.search.timeout_ms = 300;
    })
    .await;
    // 同时命中 WEB_KEYWORDS 里的「今天」与「新闻」两个词
    let msgs = text_messages("今天有什么新闻");

    let first = post_chat(&h.base_url, "auto", msgs.clone()).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    // 前置条件：搜索确实被触发过（否则这条测的就不是它）
    let search_header = header(&first, "x-route-search");
    assert_eq!(
        search_header.as_deref(),
        Some("failed"),
        "搜索预取应当跑过并失败 —— 否则这条用例没测到目标场景"
    );
    assert_eq!(
        header(&first, "x-cache").as_deref(),
        Some("BYPASS"),
        "搜索碰过的请求必须 BYPASS，不是 MISS"
    );
    assert_eq!(
        header(&first, "x-cache-reason").as_deref(),
        Some("search_injected")
    );

    // 第二次也不能命中
    let second = post_chat(&h.base_url, "auto", msgs.clone()).await;
    assert_ne!(
        header(&second, "x-cache").as_deref(),
        Some("HIT"),
        "搜索碰过的请求永远不该命中"
    );
    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        2,
        "两次都必须真打上游"
    );
}

/// 配置变更后缓存被清空。
///
/// 这条挡的是「改了配置却不生效」：界面显示保存成功，运行时还在返回旧答案。
#[tokio::test]
async fn 配置变更后缓存被清空() {
    let h = spawn(true, |_| {}).await;
    let msgs = text_messages("会被缓存的请求");

    let a = post_chat(&h.base_url, "plain-model", msgs.clone()).await;
    assert_eq!(header(&a, "x-cache").as_deref(), Some("MISS"));
    let b = post_chat(&h.base_url, "plain-model", msgs.clone()).await;
    assert_eq!(header(&b, "x-cache").as_deref(), Some("HIT"));
    let before = h.gateway.cache.stats();
    assert_eq!(before.entries, 1, "此刻应有一条缓存");

    // 走真实的 reload_providers 入口（供应商/模型/价格三类变更都汇到这里）
    h.gateway.reload_providers().await.unwrap();
    let after = h.gateway.cache.stats();
    assert_eq!(after.entries, 0, "reload_providers 之后缓存必须清空");
    assert_eq!(after.invalidations, before.invalidations + 1);

    // 清空后再请求必须是 MISS，且要再打一次上游
    let c = post_chat(&h.base_url, "plain-model", msgs.clone()).await;
    assert_eq!(
        header(&c, "x-cache").as_deref(),
        Some("MISS"),
        "清空后必须重新走上游"
    );
    // 计数是 2 不是 3：中间那次 HIT 没有打上游。
    // 这个数本身就证明了命中真的省掉了一次上游调用。
    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        2,
        "MISS + HIT + MISS ⇒ 上游只该被调用两次"
    );
}

/// 相同内容不同 Map 顺序算出同一个键。
///
/// 这条挡的是「规范化漏了排序」：JSON 对象字段顺序不同但语义相同，
/// 若键不同则缓存永不命中，而功能看起来只是「没生效」。
#[tokio::test]
async fn 相同内容不同_map_顺序算出同一个_key() {
    let h = spawn(true, |_| {}).await;

    // 两条消息内容一致，但 JSON 字段顺序相反。
    // 用字符串拼出原始 body 才能控制字段顺序 —— serde_json 的
    // Map 默认有序，构造不出「顺序不同」的 Value。
    let body_a = r#"{"model":"plain-model","messages":[{"role":"user","content":"顺序测试"}],"temperature":0.7}"#;
    let body_b = r#"{"temperature":0.7,"messages":[{"content":"顺序测试","role":"user"}],"model":"plain-model"}"#;

    // 用具名函数而不是闭包：闭包按值捕获 base_url 后只能调一次。
    async fn send_raw(base_url: &str, raw: &str) -> reqwest::Response {
        client()
            .post(format!("{base_url}/v1/chat/completions"))
            .bearer_auth(KEY)
            .header("content-type", "application/json")
            .body(raw.to_string())
            .send()
            .await
            .expect("请求网关")
    }

    let first = send_raw(&h.base_url, body_a).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let key_a = header(&first, "x-cache-key").expect("MISS 应带 key");
    assert_eq!(header(&first, "x-cache").as_deref(), Some("MISS"));

    let second = send_raw(&h.base_url, body_b).await;
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    let key_b = header(&second, "x-cache-key").expect("应带 key");
    assert_eq!(key_a, key_b, "字段顺序不同但内容相同的请求必须算出同一个键");
    assert_eq!(
        header(&second, "x-cache").as_deref(),
        Some("HIT"),
        "同键必须命中"
    );
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 1);
}

/// 不同采样参数算出不同键，且互不命中。
#[tokio::test]
async fn 不同采样参数算出不同_key() {
    let h = spawn(true, |_| {}).await;
    let msgs = text_messages("采样参数测试");

    let a = post_chat_extra(
        &h.base_url,
        "plain-model",
        msgs.clone(),
        serde_json::json!({"temperature": 0.1}),
    )
    .await;
    let key_a = header(&a, "x-cache-key").expect("应带 key");
    assert_eq!(header(&a, "x-cache").as_deref(), Some("MISS"));

    let b = post_chat_extra(
        &h.base_url,
        "plain-model",
        msgs.clone(),
        serde_json::json!({"temperature": 0.9}),
    )
    .await;
    let key_b = header(&b, "x-cache-key").expect("应带 key");
    assert_ne!(key_a, key_b, "不同 temperature 必须算出不同键");
    assert_eq!(
        header(&b, "x-cache").as_deref(),
        Some("MISS"),
        "不同采样参数不该命中"
    );
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 2);
}

/// 带多模态的请求 → BYPASS(multimodal)。
#[tokio::test]
async fn 含图片的请求返回_bypass() {
    let h = spawn(true, |_| {}).await;
    let msgs = serde_json::json!([{
        "role": "user",
        "content": [
            {"type": "text", "text": "这张图里是什么"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}}
        ]
    }]);
    let resp = post_chat(&h.base_url, "vision-model", msgs).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        header(&resp, "x-cache").as_deref(),
        Some("BYPASS"),
        "多模态必须 BYPASS"
    );
    assert_eq!(
        header(&resp, "x-cache-reason").as_deref(),
        Some("multimodal")
    );

    // 反例：同一模型、纯文本请求是可缓存的
    let a = post_chat(&h.base_url, "vision-model", text_messages("纯文本")).await;
    assert_eq!(header(&a, "x-cache").as_deref(), Some("MISS"));
}

#[tokio::test]
async fn 重载期间旧请求完成后不能回填已失效缓存() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let h = spawn_with_mock(
        true,
        |_| {},
        MockState {
            pause_first: Some((started.clone(), release.clone())),
            ..Default::default()
        },
    )
    .await;
    let base = h.base_url.clone();
    let pending = tokio::spawn(async move {
        post_chat(&base, "plain-model", text_messages("in flight question")).await
    });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    h.gateway.reload_providers().await.unwrap();
    release.notify_one();
    let response = pending.await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.bytes().await.unwrap();
    assert_eq!(
        h.gateway.cache.stats().entries,
        0,
        "重载后不能留下在途旧请求的响应"
    );

    let fresh = post_chat(
        &h.base_url,
        "plain-model",
        text_messages("in flight question"),
    )
    .await;
    assert_eq!(header(&fresh, "x-cache").as_deref(), Some("MISS"));
    fresh.bytes().await.unwrap();
    let hit = post_chat(
        &h.base_url,
        "plain-model",
        text_messages("in flight question"),
    )
    .await;
    assert_eq!(header(&hit, "x-cache").as_deref(), Some("HIT"));
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn 热更新容量立即限制新缓存而无需重启() {
    let h = spawn(true, |_| {}).await;
    let mut cfg = h.gateway.cfg_snapshot();
    cfg.cache.capacity = 1;
    h.gateway.update_cfg(cfg);
    for text in ["capacity a", "capacity b"] {
        let response = post_chat(&h.base_url, "plain-model", text_messages(text)).await;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        response.bytes().await.unwrap();
    }
    assert_eq!(h.gateway.cache.stats().entries, 1, "新容量必须立即生效");
    let latest = post_chat(&h.base_url, "plain-model", text_messages("capacity b")).await;
    assert_eq!(header(&latest, "x-cache").as_deref(), Some("HIT"));
    latest.bytes().await.unwrap();
    let evicted = post_chat(&h.base_url, "plain-model", text_messages("capacity a")).await;
    assert_eq!(header(&evicted, "x-cache").as_deref(), Some("MISS"));
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn 热更新_ttl_到期后重新调用上游() {
    let h = spawn(true, |_| {}).await;
    let mut cfg = h.gateway.cfg_snapshot();
    cfg.cache = serde_json::from_value(serde_json::json!({
        "enabled": true, "capacity": 200, "ttl_secs": 1
    }))
    .unwrap();
    h.gateway.update_cfg(cfg);
    let first = post_chat(&h.base_url, "plain-model", text_messages("ttl question")).await;
    assert_eq!(header(&first, "x-cache").as_deref(), Some("MISS"));
    first.bytes().await.unwrap();
    let second = post_chat(&h.base_url, "plain-model", text_messages("ttl question")).await;
    assert_eq!(header(&second, "x-cache").as_deref(), Some("HIT"));
    second.bytes().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let expired = post_chat(&h.base_url, "plain-model", text_messages("ttl question")).await;
    assert_eq!(header(&expired, "x-cache").as_deref(), Some("MISS"));
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn 网关启动及热更新代理确实改变_https_连接出口() {
    let first_proxy = MockState::default();
    let second_proxy = MockState::default();
    let mut tasks = Vec::new();
    let mut proxy_urls = Vec::new();
    for state in [first_proxy.clone(), second_proxy.clone()] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        proxy_urls.push(format!("http://{}", listener.local_addr().unwrap()));
        // HTTPS 的 CONNECT 在本机直接终止，避免 TLS/真实域名解析或收费上游。
        let app = Router::new().fallback(move || {
            let state = state.clone();
            async move {
                state.calls.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            }
        });
        tasks.push(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
    }
    let h = spawn(false, |cfg| cfg.http_proxy = Some(proxy_urls[0].clone())).await;
    let mut provider = h.gateway.providers.read()[0].clone();
    provider.base_url = "https://gateway-upstream.invalid/v1".into();
    repo::upsert_provider(h._db.pool(), &provider)
        .await
        .unwrap();
    h.gateway.reload_providers().await.unwrap();

    let first = post_chat(&h.base_url, "plain-model", text_messages("proxy first")).await;
    let status = first.status();
    let body = first.text().await.unwrap();
    assert!(status.is_server_error(), "{status}: {body}");
    assert_eq!(
        first_proxy.calls.load(Ordering::SeqCst),
        1,
        "启动时必须使用配置的代理"
    );
    assert_eq!(second_proxy.calls.load(Ordering::SeqCst), 0);

    let mut cfg = h.gateway.cfg_snapshot();
    cfg.http_proxy = Some(proxy_urls[1].clone());
    h.gateway.update_cfg(cfg);
    // 第一条 CONNECT 的故意拒绝会触发健康冷却；清除该夹具状态只验证出口热切换。
    h.gateway.health.reset(&provider.id, "plain-model");
    let second = post_chat(&h.base_url, "plain-model", text_messages("proxy second")).await;
    assert!(second.status().is_server_error());
    second.bytes().await.unwrap();
    assert_eq!(
        first_proxy.calls.load(Ordering::SeqCst),
        1,
        "热更新后不能继续使用旧代理"
    );
    assert_eq!(second_proxy.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        0,
        "请求必须经由代理而非测试原上游"
    );
    for task in tasks {
        task.abort();
    }
}
