//! 重复请求抑制（去重）的端到端验收测试。
//!
//! 用 axum mock 上游（模式抄自 `tests/response_cache.rs`），起一个真网关，
//! 用真 HTTP 请求走完整链路。
//!
//! ## 为什么必须是端到端
//!
//! 单元测试能证明 `DedupRegistry` 的判定逻辑，但证明不了三件事：
//!
//! 1. **接线真的在 `dispatch` 里**：没接的话网关照常回 200，测试全绿。
//! 2. **触发的确实是「同一次请求发两遍」**：只有真发两次相同请求才知道
//!    指纹算出来一样（会话 ID、请求体序列化任一不稳定，去重就永不命中）。
//! 3. **上游真的只被调用一次**：这是「答两次」被挡住的唯一直接证据 ——
//!    只看响应状态码会被「网关自己拒绝但上游仍被调用」骗过。
//!
//! **每组「开启时」的用例都配了一条「关闭时」的反例** ——
//! 只断言「开启时出现 X」是不够的：如果那段代码一直在发 X，
//! 断言照样绿，而去重其实没生效。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::config::AppConfig;
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider};
use llm_gateway_lib::proxy::dedup::DedupMode;
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 夹具的判定口径与真实上游对齐：网关发的是 POST + JSON body，
/// 返回裸的 chat.completion 对象（不额外包一层）。
#[derive(Clone, Default)]
struct MockState {
    calls: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
}

async fn mock_upstream(state: MockState) -> Router {
    Router::new().fallback(post(move |Json(body): Json<serde_json::Value>| {
        let state = state.clone();
        async move {
            let n = state.calls.fetch_add(1, Ordering::SeqCst);
            state.bodies.lock().unwrap().push(body);
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

fn model(alias: &str) -> ModelRef {
    ModelRef {
        enabled: true,
        alias: alias.into(),
        upstream: alias.into(),
        context_window: 32_768,
        supports_tools: false,
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
    }
}

fn provider(id: &str, base_url: String) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: id.into(),
        dialect: Dialect::OpenAI,
        base_url,
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        models: vec![model("plain-model")],
        rpm_limit: 0,
        intelligence: 80,
        note: None,
        runtime_id: None,
        created_at: now,
        updated_at: now,
    }
}

async fn wait_for_gateway(gateway: &GatewayState, task: &JoinHandle<()>) -> String {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(!task.is_finished(), "本测试网关在就绪前退出");
        // 端口 0 由 serve 一次性绑定；只取本实例的真实地址，不能误等其他测试。
        if let Some(listener) = gateway.listener_snapshot() {
            let base_url = format!("http://{}", listener.bound_addr);
            if tokio::time::timeout_at(deadline, client.get(format!("{base_url}/healthz")).send())
                .await
                .is_ok_and(|result| result.is_ok_and(|r| r.status() == reqwest::StatusCode::OK))
            {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "本测试网关超时才就绪"
                );
                return base_url;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "本测试网关未在预期时间内就绪"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

const KEY: &str = "lgw-dedup-test-key";

struct Harness {
    upstream: MockState,
    gateway: Arc<GatewayState>,
    base_url: String,
    /// 保留 JoinHandle，drop 掉 harness 时任务仍在跑（tokio 运行时自己收）。
    _task: JoinHandle<()>,
    _db: db::Db,
}

/// 起一套「mock 上游 + 真网关」。`mode` 控制去重模式。
async fn spawn(mode: DedupMode) -> Harness {
    let upstream_state = MockState::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = mock_upstream(upstream_state.clone()).await;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    let database = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(database.pool(), &provider("mock", format!("http://{addr}")))
        .await
        .unwrap();

    let mut config = AppConfig {
        port: 0,
        unified_key: KEY.into(),
        ..Default::default()
    };
    config.dedup.mode = mode;
    config.dedup.window_secs = 60;
    config.dedup.wait_secs = 2;

    let gateway = Arc::new(GatewayState::new(database.clone(), config.clone()));
    gateway.reload_providers().await.unwrap();
    let task = tokio::spawn({
        let gateway = gateway.clone();
        async move {
            serve(gateway).await.expect("本测试网关服务失败");
        }
    });
    let base_url = wait_for_gateway(&gateway, &task).await;

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

/// 同一份请求体发两次。**必须逐字节相同**，否则测的不是「同一次请求的两次投递」。
async fn post_twice(
    base_url: &str,
    body: &serde_json::Value,
) -> (reqwest::Response, reqwest::Response) {
    let first = client()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(body)
        .send()
        .await
        .expect("第一次请求网关");
    let second = client()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(body)
        .send()
        .await
        .expect("第二次请求网关");
    (first, second)
}

fn chat_body(messages: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "model": "plain-model", "messages": messages })
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// 读 `/gw/stats` 的原始 JSON。用来断言「运行态计数真的暴露出来了」——
/// 只在代码里加字段不算交付，能从一个真实 HTTP 响应里读出来才算。
async fn gateway_stats(base_url: &str) -> serde_json::Value {
    client()
        .get(format!("{base_url}/gw/stats"))
        .bearer_auth(KEY)
        .send()
        .await
        .expect("请求 /gw/stats")
        .json()
        .await
        .expect("/gw/stats 必须是 JSON")
}

/// 反例（对照组）：去重关着时，同一份请求发两次 ⇒ 上游被调用两次。
///
/// 这条必须存在：否则「开启时上游只被调用一次」可能只是因为上游根本没被调用过。
#[tokio::test]
async fn 关闭时同一请求发两次会调用上游两次() {
    let h = spawn(DedupMode::Off).await;
    let body = chat_body(serde_json::json!([{ "role": "user", "content": "同一个问题" }]));

    let (first, second) = post_twice(&h.base_url, &body).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK, "第一次必须成功");
    assert_eq!(
        second.status(),
        reqwest::StatusCode::OK,
        "关闭时第二次也必须成功"
    );
    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        2,
        "关闭时上游必须被调用两次"
    );
    assert_eq!(
        h.gateway.dedup.stats().duplicates,
        0,
        "关闭时不该出现任何重复判定"
    );
}

/// 观察模式：重复被计数，但**不拦** —— 上游照样被调用两次，响应一个字节不改。
///
/// 这条是「只观察」的判据：计数动了（说明判定真的在跑），
/// 而行为没变（说明没有偷偷拦）。
#[tokio::test]
async fn 观察模式下重复被计数但不拦截() {
    let h = spawn(DedupMode::Observe).await;
    let body = chat_body(serde_json::json!([{ "role": "user", "content": "同一个问题" }]));

    let (first, second) = post_twice(&h.base_url, &body).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    assert_eq!(
        second.status(),
        reqwest::StatusCode::OK,
        "观察模式不许改变响应"
    );
    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        2,
        "观察模式不该少发一次上游"
    );
    assert_eq!(
        h.gateway.dedup.stats().duplicates,
        1,
        "重复必须被计数 —— 计数不动就说明判定没在跑"
    );
}

/// 抑制模式：同一份请求发两次 ⇒ 第二次被明确拒绝，**上游只被调用一次**。
///
/// 这是「问两次 / 答两次」的修复判据：上游调用次数是唯一骗不了人的证据。
#[tokio::test]
async fn 抑制模式下第二次被拒绝且上游只调用一次() {
    let h = spawn(DedupMode::Enforce).await;
    let body = chat_body(serde_json::json!([{ "role": "user", "content": "同一个问题" }]));

    let (first, second) = post_twice(&h.base_url, &body).await;
    assert_eq!(first.status(), reqwest::StatusCode::OK, "第一次必须成功");
    assert_eq!(
        second.status(),
        reqwest::StatusCode::CONFLICT,
        "第二次必须被明确拒绝（而不是再答一遍）"
    );
    assert_eq!(
        header(&second, "x-dedup").as_deref(),
        Some("conflict"),
        "拒绝响应必须带 X-Dedup 便于排查"
    );
    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        1,
        "**上游只能被调用一次** —— 这是「不再答两次」的直接判据"
    );

    let stats = h.gateway.dedup.stats();
    assert_eq!(stats.duplicates, 1);
    assert_eq!(stats.conflicts, 1);
}

/// 抑制模式下的反例：**不同**的请求仍然照常执行。
///
/// 只断言「相同请求被拦」是不够的 —— 把去重做成「一律只放一条」也能过那条断言，
/// 而那会让用户拿不到第二份回答。
#[tokio::test]
async fn 抑制模式下不同请求照常执行() {
    let h = spawn(DedupMode::Enforce).await;
    let first_body = chat_body(serde_json::json!([{ "role": "user", "content": "第一个问题" }]));
    let second_body = chat_body(serde_json::json!([{ "role": "user", "content": "另一个问题" }]));

    let first = client()
        .post(format!("{}/v1/chat/completions", h.base_url))
        .bearer_auth(KEY)
        .json(&first_body)
        .send()
        .await
        .expect("第一次请求网关");
    let second = client()
        .post(format!("{}/v1/chat/completions", h.base_url))
        .bearer_auth(KEY)
        .json(&second_body)
        .send()
        .await
        .expect("第二次请求网关");

    assert_eq!(first.status(), reqwest::StatusCode::OK);
    assert_eq!(
        second.status(),
        reqwest::StatusCode::OK,
        "不同内容的请求必须照常执行"
    );
    assert_eq!(
        h.upstream.calls.load(Ordering::SeqCst),
        2,
        "不同请求必须各发一次上游"
    );
    assert_eq!(h.gateway.dedup.stats().duplicates, 0, "不该误判成重复");
}

/// 抑制模式下的另一条反例：带 `tools` 的请求**不参与去重**。
///
/// 工具调用的正确性依赖当轮上下文，合并会执行过期动作 —— 所以这类请求
/// 即使逐字节相同也必须各跑一次。
#[tokio::test]
async fn 抑制模式下带工具的重复请求不参与去重() {
    let h = spawn(DedupMode::Enforce).await;
    let body = serde_json::json!({
        "model": "plain-model",
        "messages": [{ "role": "user", "content": "同一个问题" }],
        "tools": [{
            "type": "function",
            "function": { "name": "lookup", "parameters": { "type": "object" } }
        }],
    });

    let (first, second) = post_twice(&h.base_url, &body).await;
    // 两侧都不该被去重拦住；上游是否接受工具是另一回事，这里只断言「没被去重拦」。
    assert_ne!(
        second.status(),
        reqwest::StatusCode::CONFLICT,
        "带 tools 的请求不该被去重拒绝"
    );
    assert_ne!(
        header(&second, "x-dedup").as_deref(),
        Some("conflict"),
        "带 tools 的请求不该出现冲突标记"
    );
    assert_eq!(
        h.gateway.dedup.stats().duplicates,
        0,
        "带 tools 的请求不该参与判定"
    );
    let _ = first;
}

/// `/gw/stats` 必须把去重计数暴露出来 —— 观察模式的全部价值就在这个数字上。
///
/// 判据分两段：重复前必须是 0，重复后必须是 1。只断言「字段存在」不够：
/// 一个永远返回 0 的字段也能让「字段存在」的断言通过，而那样观察模式
/// 就等于没在观察。
#[tokio::test]
async fn 观察模式的重复计数通过_gw_stats_可见() {
    let h = spawn(DedupMode::Observe).await;

    let before = gateway_stats(&h.base_url).await;
    assert_eq!(
        before["dedup"]["duplicates"], 0,
        "还没有重复时计数必须是 0（否则计数是假的）"
    );
    assert_eq!(before["dedup"]["mode"], "observe");
    assert_eq!(before["dedup"]["observed"], 0);
    assert!(
        before["dedup"]["window_note"].is_string(),
        "必须标明这是进程内累计，避免与 24 小时窗口混淆"
    );
    assert!(
        before["cache"].is_object(),
        "缓存计数同样要在同一响应里可读"
    );

    let body = chat_body(serde_json::json!([{ "role": "user", "content": "同一个问题" }]));
    let _ = post_twice(&h.base_url, &body).await;

    let after = gateway_stats(&h.base_url).await;
    assert_eq!(
        after["dedup"]["duplicates"], 1,
        "重复发生后计数必须变成 1 —— 这是「网关是不是重复来源」的唯一证据"
    );
    assert_eq!(
        after["dedup"]["observed"], 1,
        "观察模式下的重复计入 observed"
    );
    assert_eq!(
        after["dedup"]["conflicts"], 0,
        "观察模式不许拒绝，更不许记成拒绝"
    );
}
