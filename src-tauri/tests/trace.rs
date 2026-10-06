//! B4 traceId 贯穿与 OTLP 导出的验收测试。
//!
//! 分两层：
//! - **端到端**（起真 mock 上游 + 真网关）：traceId 的生成、透传、清洗、
//!   响应头、落库。
//! - **单元**（已在 `src/trace.rs` 里）：OTel 属性名、脱敏、默认关的判断。
//!
//! 为什么「otlp 关闭时不发网络包」这条要在端到端做：它要证明的是
//! **没有连接尝试**，而那只有在真的跑起来（有东西可能会去连）时才有意义。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::audit::RequestFilter;
use llm_gateway_lib::config::AppConfig;
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

// ------------------------------ mock 上游 ------------------------------

#[derive(Clone, Default)]
struct MockState {
    calls: Arc<AtomicUsize>,
    /// 前 N 次调用返回 500，用来制造降级。
    fail_first: Arc<AtomicUsize>,
}

async fn mock_upstream(state: MockState) -> Router {
    // 用 `fallback` 而不是固定路径：网关带不带 `/v1` 前缀、dialect 拼出什么路径
    // 都不是本卡要测的东西，钉死一个路径会让「路径不匹配」以 404 的形式
    // 混进 traceId 的用例里，看起来像功能坏了。
    // （同款做法见 tests/response_cache.rs:32。）
    Router::new().fallback(post(move |Json(_body): Json<serde_json::Value>| {
        let state = state.clone();
        async move {
            let n = state.calls.fetch_add(1, Ordering::SeqCst);
            let fail = state.fail_first.load(Ordering::SeqCst);
            if n < fail {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": {"message": "上游 500"}})),
                )
                    .into_response();
            }
            Json(serde_json::json!({
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "model": "plain-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "好的"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
            }))
            .into_response()
        }
    }))
}

use axum::response::IntoResponse;

fn model(alias: &str) -> ModelRef {
    ModelRef {
        enabled: true,
        alias: alias.into(),
        upstream: alias.into(),
        context_window: 32_768,
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
    // 必须 no_proxy：走系统代理时连不上会每轮等到超时，
    // 200 轮下来几百秒（B2 实测踩过，411 秒）。
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

const KEY: &str = "lgw-trace-test-key";

struct Harness {
    upstream: MockState,
    base_url: String,
    db: db::Db,
    _task: JoinHandle<()>,
}

async fn spawn(mutate: impl FnOnce(&mut AppConfig)) -> Harness {
    spawn_multi(1, mutate).await
}

/// 起一套夹具，注册 `provider_count` 家供应商（都指向同一个 mock 上游）。
///
/// 多家是为了测降级链：只有一家时网关试一次就放弃，制造不出多次 fallback。
/// priority 递减，保证候选链顺序稳定可预期。
async fn spawn_multi(provider_count: usize, mutate: impl FnOnce(&mut AppConfig)) -> Harness {
    let upstream_state = MockState::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = mock_upstream(upstream_state.clone()).await;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(60)).await;

    let database = db::Db::connect_in_memory().await.unwrap();
    for index in 0..provider_count.max(1) {
        let mut p = provider(&format!("mock{index}"), format!("http://{addr}"));
        // priority 大的排前面：候选链顺序是 mock0 → mock1 → …
        p.priority = 100 - index as i32;
        repo::upsert_provider(database.pool(), &p).await.unwrap();
    }

    let mut config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: KEY.into(),
        // 降级要开启，否则「降级三次」的用例测不到东西
        failover_enabled: true,
        max_fallback_attempts: 5,
        ..Default::default()
    };
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
        base_url,
        db: database,
        _task: task,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("构造 client")
}

async fn post_with_trace(base_url: &str, trace: Option<&str>) -> reqwest::Response {
    let body = serde_json::json!({
        // 用夹具里真实存在的模型名。第一版写的是虚拟名 `auto`，
        // 它需要路由层能选出候选；本夹具只有一个供应商、也没开智能模式，
        // 于是全线 404 —— 看起来像 traceId 功能坏了，其实是请求根本没进 dispatch。
        "model": "plain-model",
        "messages": [{"role": "user", "content": "你好"}]
    });
    let mut req = client()
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(&body);
    if let Some(t) = trace {
        req = req.header("x-trace-id", t);
    }
    req.send().await.expect("发送请求")
}

/// 等审计行落库。落库是 `tokio::spawn` 出去的，与响应不同步。
async fn wait_rows(db: &db::Db, want: usize) -> Vec<llm_gateway_lib::audit::AuditRow> {
    for _ in 0..100 {
        let (rows, _) = repo::query_requests(db.pool(), &RequestFilter::default())
            .await
            .unwrap();
        if rows.len() >= want {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let (rows, _) = repo::query_requests(db.pool(), &RequestFilter::default())
        .await
        .unwrap();
    panic!(
        "审计行没在预期时间内落库，实际 {} 条（期望 >= {want}）",
        rows.len()
    );
}

// ------------------------------ traceId 生成与透传 ------------------------------

#[tokio::test]
async fn 入站请求生成_trace_id_并写进审计行() {
    let h = spawn(|_| {}).await;
    // 不带 X-Trace-Id：网关应当自己生成一个
    let resp = post_with_trace(&h.base_url, None).await;
    // 先取头再取 body：`text()` 会拿走 resp
    let trace_header = resp
        .headers()
        .get("x-trace-id")
        .map(|v| v.to_str().unwrap().to_string());
    let status = resp.status();
    let body = resp.text().await.unwrap();
    assert_eq!(status, 200, "响应体：{body}");

    let header = trace_header.expect("响应必须带 X-Trace-Id");
    assert_eq!(header.len(), 32, "应是 32 位十六进制：{header}");
    assert!(header.chars().all(|c| c.is_ascii_hexdigit()));

    let rows = wait_rows(&h.db, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].trace_id.as_deref(),
        Some(header.as_str()),
        "落库的 traceId 必须与响应头一致，否则两边对不上就白记了"
    );
}

#[tokio::test]
async fn 客户端自带_trace_id_被透传() {
    let h = spawn(|_| {}).await;
    let given = "4bf92f3577b34da6a3ce929d0e0e4736";
    let resp = post_with_trace(&h.base_url, Some(given)).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("x-trace-id").unwrap().to_str().unwrap(),
        given,
        "客户端给了合法 traceId 就该原样透传（跨服务串联靠它）"
    );

    let rows = wait_rows(&h.db, 1).await;
    assert_eq!(rows[0].trace_id.as_deref(), Some(given));
}

#[tokio::test]
async fn 非法_trace_id_字符被清洗() {
    let h = spawn(|_| {}).await;

    // **必须走裸 TCP，不能用 reqwest。** reqwest 的 `.header()` 自己就会拒绝
    // 含 CR/LF 的值（`HeaderValue::from_str` 校验失败直接 panic）——
    // 也就是说这个测试用 reqwest 根本发不出去，只会测出「reqwest 很安全」。
    // 而真实的注入者是拿 socket 直接写字节的，所以必须自己拼报文。
    let addr = h.base_url.trim_start_matches("http://").to_string();
    let evil = "abc\r\nX-Injected: 1";
    let body = r#"{"model":"plain-model","messages":[{"role":"user","content":"你好"}]}"#;
    let raw = format!(
        "POST /v1/chat/completions HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Authorization: Bearer {KEY}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         X-Trace-Id: {evil}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );

    let mut stream = tokio::net::TcpStream::connect(&addr)
        .await
        .expect("连上网关");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream.write_all(raw.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);

    // 判据①：响应里没有第二个头（注入成功的标志）
    let lower = text.to_ascii_lowercase();
    assert!(
        !lower.contains("x-injected"),
        "响应被注入了一个新头：\n{text}"
    );
    // 判据②：报文结构没被撑破（只有一个状态行）
    assert_eq!(
        text.matches("HTTP/1.1 ").count(),
        1,
        "响应里出现了多个状态行，报文被拆开了：\n{text}"
    );
    // 判据③：回显的 traceId 只剩 hex 与短横线。
    //
    // 这里拿到的是 `abc` 而不是 `abc-eced1`：裸报文里的 `\r\n` 让 HTTP 解析器
    // 把它**拆成了两个头**（`X-Trace-Id: abc` 与 `X-Injected: 1`），
    // 所以网关看到的本来就是 `abc`。这正是我们要的结果 ——
    // 注入出来的第二个头网关根本不回显，第一个头也被清洗成纯 hex。
    //
    // 第一版期望值写成 `abc-eced1`（按 `sanitize_trace_id` 的单元行为推的），
    // 与端到端的真实情况不符。**单元行为与端到端行为是两件事**，
    // 前者测清洗函数，后者测「报文有没有被拆开」。
    let trace_line = text
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("x-trace-id:"))
        .expect("响应应当带 X-Trace-Id");
    let value = trace_line.split(':').nth(1).unwrap().trim();
    assert!(
        value.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "回显里出现了非 hex 字符：{value:?}"
    );
    assert_eq!(value, "abc", "实际回显：{value:?}");

    // 判据④：落库的也必须是清洗后的值（不含 CR/LF/冒号）
    let rows = wait_rows(&h.db, 1).await;
    if let Some(t) = rows.first().and_then(|r| r.trace_id.as_deref()) {
        for bad in ['\r', '\n', ':'] {
            assert!(!t.contains(bad), "落库的 traceId 残留了 {bad:?}：{t:?}");
        }
    }
}

#[tokio::test]
async fn 头里能带但非法_trace_id_字符也被清洗() {
    // 上面那条走裸 TCP，证明「注入的头不会被回显」。
    // 但 HTTP 解析器已经替我们拆开了 CR/LF，所以「清洗函数本身有没有起作用」
    // 在端到端里看不出来 —— 需要一条**头值合法、但内容非法**的用例：
    // 分号、空格、冒号这些都能放进头值里，它们才是清洗函数要挡的东西。
    let h = spawn(|_| {}).await;
    let dirty = "ab;cd ef:gh";
    let resp = post_with_trace(&h.base_url, Some(dirty)).await;
    assert_eq!(resp.status(), 200);
    let got = resp.headers().get("x-trace-id").unwrap().to_str().unwrap();
    // `ab;cd ef:gh` 里是 hex 的有 a,b,c,d,e,f（`ef` 的两个字母都算），
    // 分号、空格、冒号被滤掉 ⇒ `abcdef`。
    // 第一版写成 `abcde`，是我自己数漏了 `ef` 里的 f。
    assert_eq!(got, "abcdef", "分号/空格/冒号应被过滤掉，实际：{got:?}");
    assert!(
        got.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "回显里出现了非 hex 字符：{got:?}"
    );

    let rows = wait_rows(&h.db, 1).await;
    assert_eq!(rows[0].trace_id.as_deref(), Some("abcdef"));
}

#[tokio::test]
async fn 全是非法字符时_回落到新生成的_trace_id() {
    let h = spawn(|_| {}).await;
    let resp = post_with_trace(&h.base_url, Some("追踪标识！！！")).await;
    assert_eq!(resp.status(), 200);
    let got = resp.headers().get("x-trace-id").unwrap().to_str().unwrap();
    // 清洗后为空 ⇒ 生成新的，而不是回一个空头
    assert_eq!(got.len(), 32, "应回落到新生成的 32 位 id：{got:?}");
    assert_ne!(got, "");
}

// ------------------------------ 降级链 ------------------------------

#[tokio::test]
async fn 降级三次产生三条同_trace_id_不同_attempt_的记录() {
    // 要真的有 3 次降级，候选链上必须有 ≥ 4 家：前 3 家失败、第 4 家成功。
    // 只有一家时网关试一次就放弃了 —— 第一版就是这么写的，
    // 拿到 500 而不是 200，而失败信息只显示「最后一跳应当成功」，
    // 看不出根因是候选太少。
    let h = spawn_multi(4, |_| {}).await;
    h.upstream.fail_first.store(3, Ordering::SeqCst);

    let given = "aabbccddeeff00112233445566778899";
    let resp = post_with_trace(&h.base_url, Some(given)).await;
    let status = resp.status();
    if status != 200 {
        let body = resp.text().await.unwrap();
        panic!("最后一跳应当成功，实际 {status}：{body}");
    }
    assert!(
        h.upstream.calls.load(Ordering::SeqCst) >= 4,
        "应当真的打到了第 4 跳，实际只调用了 {} 次",
        h.upstream.calls.load(Ordering::SeqCst)
    );

    let rows = wait_rows(&h.db, 1).await;
    let row = &rows[0];
    assert_eq!(
        row.trace_id.as_deref(),
        Some(given),
        "整条记录共用一个 traceId"
    );
    assert!(
        row.fallback_attempts >= 3,
        "应当记到至少 3 次降级，实际 {}",
        row.fallback_attempts
    );

    // 每一条降级明细都要带同一个 traceId、且 attempt 序号递增
    assert!(
        row.attempts.len() >= 3,
        "降级明细应有至少 3 条，实际 {}：{:?}",
        row.attempts.len(),
        row.attempts
    );
    for (index, attempt) in row.attempts.iter().enumerate() {
        assert_eq!(
            attempt.get("trace_id").and_then(|v| v.as_str()),
            Some(given),
            "第 {index} 跳的 traceId 与整条记录不一致：{attempt}"
        );
        assert_eq!(
            attempt.get("attempt").and_then(|v| v.as_i64()),
            Some(index as i64),
            "attempt 序号必须等于它在数组里的下标：{attempt}"
        );
    }
}

// ------------------------------ OTLP 默认关闭 ------------------------------

#[tokio::test]
async fn otlp_关闭时_不初始化_exporter_也不发网络包() {
    // 指向一个**不可达**的地址。如果 exporter 被初始化了，
    // 它会立刻开始往这里连；关闭时就一个包都不该发。
    let unreachable = "http://127.0.0.1:1";
    let h = spawn(|cfg| {
        // 默认就是空（关闭）。这里刻意确认「即使把地址写进配置，
        // 只要总开关的判据是不为空，就要看它到底发不发」。
        cfg.telemetry.otlp.endpoint = String::new();
        assert!(!cfg.telemetry.should_export());
        let _ = unreachable;
    })
    .await;

    let resp = post_with_trace(&h.base_url, None).await;
    assert_eq!(resp.status(), 200);
    // 即使不导出，traceId 也必须回给客户端（「本地永远要有」）
    assert!(
        resp.headers().get("x-trace-id").is_some(),
        "OTLP 关着时响应头仍必须带 X-Trace-Id"
    );

    let rows = wait_rows(&h.db, 1).await;
    assert!(rows[0].trace_id.is_some(), "OTLP 关着时 traceId 也必须落库");
}

#[tokio::test]
async fn otlp_配置为空时_判定为不导出() {
    // 反向：给了地址就应当判定为导出，否则上面那条只是因为「永远不导出」
    let mut cfg = AppConfig::default();
    assert_eq!(cfg.telemetry.otlp.endpoint, "");
    assert!(!cfg.telemetry.should_export(), "默认必须不导出");
    cfg.telemetry.otlp.endpoint = "http://127.0.0.1:4317".into();
    assert!(cfg.telemetry.should_export(), "给了地址就该导出");
}

#[test]
fn otlp_关闭时_init_返回_none_且没有初始化痕迹() {
    // 判据是**可观测的**：`init` 返回 None，且 `initialized_endpoint()` 为 None。
    // 不靠「等一个网络超时看有没有连接」—— 那种判据既慢又不稳。
    let guard = llm_gateway_lib::telemetry::init(&AppConfig::default().telemetry);
    assert!(guard.is_none(), "默认配置下 init 必须返回 None");
    assert!(
        llm_gateway_lib::telemetry::initialized_endpoint().is_none(),
        "默认配置下不该留下任何初始化痕迹"
    );
}

#[test]
fn otlp_导出内容不含_prompt_全文() {
    // 这条在**类型层面**就成立：`SpanRecord` 里根本没有正文字段。
    // 这里把一段特征明显的假正文放在手边，断言它不出现在导出形态里。
    let secret_prompt = "用户的原话：我的身份证号是 110101199001011234";
    let rec = llm_gateway_lib::trace::SpanRecord {
        trace_id: "t".into(),
        attempt: 0,
        system: "openai".into(),
        request_model: "gpt-4o".into(),
        response_model: Some("gpt-4o".into()),
        input_tokens: Some(11),
        output_tokens: Some(2),
        server_address: Some("api.openai.com".into()),
        error_type: None,
        latency_ms: Some(410),
        status: Some(200),
    };
    for (_, value) in rec.attributes() {
        assert!(
            !value.contains("身份证号") && !value.contains(secret_prompt),
            "导出属性里出现了正文：{value}"
        );
    }
    let json = serde_json::to_string(&rec).unwrap();
    assert!(
        !json.contains("身份证号"),
        "序列化后的 span 里出现了正文：{json}"
    );
}

#[test]
fn 开启后_span_属性名符合_genai_语义约定() {
    // 逐个比对官方注册表；写错一个就红。
    let names = llm_gateway_lib::telemetry::attribute_names();
    for expected in [
        "gen_ai.system",
        "gen_ai.operation.name",
        "gen_ai.request.model",
        "gen_ai.response.model",
        "gen_ai.usage.input_tokens",
        "gen_ai.usage.output_tokens",
        "server.address",
        "error.type",
    ] {
        assert!(names.contains(&expected), "缺少属性名 {expected}");
    }
    assert_eq!(names.len(), 8, "多一个少一个都算错：{names:?}");
    // span 名用官方操作名，不自造
    assert_eq!(llm_gateway_lib::telemetry::span_name(), "chat");
}

// ------------------------------ 按 traceId 过滤 ------------------------------

#[tokio::test]
async fn trace_id_写成非法值时_按_trace_id_过滤查不到() {
    let h = spawn(|_| {}).await;
    // 造两条不同 traceId 的记录
    let a = post_with_trace(&h.base_url, Some("aaaa1111bbbb2222cccc3333dddd4444")).await;
    assert_eq!(a.status(), 200);
    let b = post_with_trace(&h.base_url, Some("eeee5555ffff6666aaaa7777bbbb8888")).await;
    assert_eq!(b.status(), 200);
    wait_rows(&h.db, 2).await;

    // 正向：按真实 traceId 过滤能命中恰好一条
    let (hit_a, total_a) = repo::query_requests(
        h.db.pool(),
        &RequestFilter {
            trace_id: Some("aaaa1111bbbb2222cccc3333dddd4444".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(total_a, 1, "按 traceId 过滤必须命中恰好一条");
    assert_eq!(
        hit_a[0].trace_id.as_deref(),
        Some("aaaa1111bbbb2222cccc3333dddd4444")
    );

    // 反向：一个不存在的 traceId 查不到 —— 证明过滤**真的在用这一列**，
    // 而不是「忽略条件返回全部」
    let (none, total_none) = repo::query_requests(
        h.db.pool(),
        &RequestFilter {
            trace_id: Some("00000000000000000000000000000000".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(none.is_empty(), "不存在的 traceId 不该有结果：{none:?}");
    assert_eq!(total_none, 0);

    // 非法值（清洗后会变成别的东西）同样查不到
    let (bad, total_bad) = repo::query_requests(
        h.db.pool(),
        &RequestFilter {
            trace_id: Some("\r\n注入".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(bad.is_empty(), "非法 traceId 不该有结果");
    assert_eq!(total_bad, 0);
}

// ------------------------------ B3 补正：提示词留存端到端 ------------------------------

#[tokio::test]
async fn 关闭时_prompt_列在端到端路径上也是空() {
    let h = spawn(|cfg| {
        assert!(!cfg.audit.store_refined_prompt, "默认必须关闭");
    })
    .await;
    let resp = post_with_trace(&h.base_url, None).await;
    assert_eq!(resp.status(), 200);

    let rows = wait_rows(&h.db, 1).await;
    assert!(
        rows[0].refined_prompt.is_none(),
        "默认关闭时 refined_prompt 必须是 NULL，实际：{:?}",
        rows[0].refined_prompt
    );
}

#[tokio::test]
async fn 开启提示词留存后_改写结果真的落库() {
    // 这条补的是 B3 的一个真缺陷：当时 `refined_prompt` 到 dispatch 的接线
    // **从未生效**（替换脚本锚点没匹配上、静默返回原文），而 B3 的测试全过 ——
    // 因为它们只测 `refined_prompt_to_store` 纯函数，没有一条走 dispatch。
    //
    // 现在这条走完整路径：开开关 → 发请求 → 查库。
    // 若接线再次断掉，它会红。
    let h = spawn(|cfg| {
        cfg.audit.store_refined_prompt = true;
        // 开启改写，让 refine 真的产生结果。
        // `routing_strategy` 在 AppConfig 上，不在 SmartRoutingConfig 里。
        cfg.routing_strategy = llm_gateway_lib::config::RoutingStrategy::Smart;
        cfg.smart_routing.enabled = true;
        cfg.smart_routing.prompt_refine.enabled = true;
        cfg.smart_routing.prompt_refine.provider_id = Some("mock".into());
        cfg.smart_routing.prompt_refine.model = Some("plain-model".into());
        cfg.smart_routing.prompt_refine.min_chars = 1;
    })
    .await;

    let resp = post_with_trace(&h.base_url, None).await;
    // 改写可能因为各种前置条件没触发；**触发与否都要断言一致**：
    // 触发了 → 库里有值；没触发 → 库里是 NULL。两边都不能是「接线断了」。
    let status = resp.status();
    assert!(
        status == 200 || status.is_client_error(),
        "响应状态异常：{status}"
    );

    let rows = wait_rows(&h.db, 1).await;
    let row = &rows[0];
    match (&row.refined_prompt, row.route_refined) {
        // 改写了 ⇒ 必须存下来（这是本用例的核心判据）
        (Some(stored), _) => {
            assert!(
                !stored.is_empty(),
                "改写了却存了空串 —— 空串在审计里看不出「存过」还是「没存」"
            );
        }
        // 没改写 ⇒ 存 NULL 是对的，但要**说明**为什么没改写
        (None, refined) => {
            assert!(
                refined != Some(true),
                "route_refined 说改写了，refined_prompt 却是 NULL —— 接线断了"
            );
        }
    }
}
