//! B2 预算闸门与 per-key 模型白名单的验收测试。
//!
//! 用 axum mock 上游 + 真网关 + 远程模式（闸门只对远程 Key 生效）。
//!
//! **每组「开启时」的用例都配了一条反例**：只断言「超预算被拦」是不够的，
//! 如果那段代码无条件返回 429，断言照样绿而正常请求全挂。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use llm_gateway_lib::budget::MICROS_PER_UNIT;
use llm_gateway_lib::config::AppConfig;
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{Dialect, ModelRef, Provider, RemoteAccessKey};
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const KEY_SECRET: &str = "rk-budget-test-secret";

#[derive(Clone, Default)]
struct Mock {
    calls: Arc<AtomicUsize>,
}

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
        models: vec![model("alpha"), model("beta")],
        rpm_limit: 0,
        intelligence: 80,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 造一个远程 Key。`budget` 是 `(micros, currency, allowed_models)`。
fn access_key(id: &str, enabled: bool, budget: (i64, &str, Vec<String>)) -> RemoteAccessKey {
    let now = chrono::Utc::now();
    RemoteAccessKey {
        id: id.into(),
        label: id.into(),
        key_hash: sha256_hex(if id == "rk-main" { KEY_SECRET } else { id }),
        enabled,
        // RPM 给足，避免限流把预算断言盖掉
        rpm_limit: 10_000,
        monthly_budget_micros: budget.0,
        budget_currency: budget.1.into(),
        allowed_models: budget.2,
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
    // 必须 no_proxy：默认 client 会走系统代理，连不上时每轮要等到超时，
    // 200 轮下来是 400 秒 —— 实测第一次跑就是 411 秒才发现网关没起来。
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

struct Harness {
    db: db::Db,
    gateway: Arc<GatewayState>,
    base_url: String,
    mock_calls: Arc<AtomicUsize>,
    _task: JoinHandle<()>,
}

/// 起一套「mock 上游 + 真网关（远程模式）」。`keys` 是要建的远程 Key。
async fn spawn(keys: Vec<RemoteAccessKey>, mutate: impl FnOnce(&mut AppConfig)) -> Harness {
    let mock = Mock::default();
    let calls = mock.calls.clone();
    let app = Router::new().fallback(post(move |Json(_b): Json<serde_json::Value>| {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Json(serde_json::json!({
                "id": "chatcmpl-budget",
                "object": "chat.completion",
                "model": "mock-model",
                "choices": [{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
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
    repo::upsert_provider(database.pool(), &provider(format!("http://{addr}")))
        .await
        .unwrap();
    for key in &keys {
        repo::create_remote_access_key(database.pool(), key)
            .await
            .unwrap();
    }

    let mut config = AppConfig {
        port: unused_loopback_port().await,
        unified_key: "lgw-local-unified".into(),
        ..Default::default()
    };
    // 闸门只对**远程** Key 生效，所以要开远程模式。
    // 而且必须给一个合法的 https public_url —— `serve` 会跑
    // `validate_remote_mode`，缺了它网关直接不启动（报「网关没在预期时间内起来」，
    // 看不出是配置问题）。
    config.remote_mode = llm_gateway_lib::config::RemoteModeConfig {
        enabled: true,
        public_url: Some("https://gateway.example.test".into()),
    };
    config.normalize_listener();
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
        db: database,
        gateway,
        base_url,
        mock_calls: mock.calls.clone(),
        _task: task,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

/// 用远程 Key 发一次请求。`secret` 是明文。
///
/// 必须带 `x-forwarded-for` + `x-forwarded-proto: https`：
/// 网关靠这两个头判断「这是反代之后的外部客户端」，只有外部客户端
/// 才走远程 Key 那条分支。不带头的话请求会被当成来自本机，
/// 于是只认统一 Key，远程 Key 一律 401 —— 那样测的就不是闸门了。
async fn chat_with(base_url: &str, secret: &str, model: &str) -> reqwest::Response {
    client()
        .post(format!("{base_url}/v1/chat/completions"))
        .header("x-forwarded-for", "198.51.100.7")
        .header("x-forwarded-proto", "https")
        .bearer_auth(secret)
        .json(&serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "你好"}]
        }))
        .send()
        .await
        .expect("请求网关")
}

async fn error_code(
    response: reqwest::Response,
) -> (reqwest::StatusCode, String, serde_json::Value) {
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    let code = body["error"]["code"].as_str().unwrap_or("").to_string();
    (status, code, body)
}

/// 当前月的起点（与实现同口径，UTC 月初）。
fn month_start() -> i64 {
    llm_gateway_lib::budget::month_start_secs(chrono::Utc::now().timestamp())
}

/// 直接往 requests 表塞一条消费记录，模拟「已经花掉了这些钱」。
///
/// 不去伪造一次真实请求：闸门判定只看表里的累计值，
/// 而走真请求的话金额取决于 mock 上游返回的 usage 与模型价格配置，
/// 那是另一个变量（价格表），会把这条用例要测的东西搅浑。
async fn seed_spend(db: &db::Db, key_id: &str, cost: f64, currency: &str) {
    repo::log_request(
        db.pool(),
        repo::RequestLog {
            session_id: Some("s-seed"),
            client: Some(&format!("remote-key:{key_id}")),
            requested_model: "alpha",
            routed_provider: Some("mock"),
            routed_model: Some("alpha"),
            status: Some(200),
            latency_ms: 10,
            prompt_tokens: 1,
            completion_tokens: 1,
            fallback_attempts: 0,
            error: None,
            cost: Some(cost),
            currency: Some(currency),
            rate_label: None,
            estimated_prompt_tokens: Some(1),
            attempts_json: None,
            route: Default::default(),
            access_key_id: Some(key_id),
            refined_prompt: None,
        },
    )
    .await
    .unwrap();
}

// ---------- 放行路径 ----------

#[tokio::test]
async fn 未超预算时请求正常放行() {
    let h = spawn(
        vec![access_key(
            "rk-main",
            true,
            (10 * MICROS_PER_UNIT, "USD", vec![]),
        )],
        |_| {},
    )
    .await;
    seed_spend(&h.db, "rk-main", 1.0, "USD").await;

    let r = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(r.status(), reqwest::StatusCode::OK, "未超预算必须放行");
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 预算为_0_表示不限() {
    let h = spawn(
        vec![access_key("rk-main", true, (0, "USD", vec![]))],
        |_| {},
    )
    .await;
    // 塞一笔很大的消费
    seed_spend(&h.db, "rk-main", 999_999.0, "USD").await;

    let r = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(r.status(), reqwest::StatusCode::OK, "上限为 0 必须不限");
}

#[tokio::test]
async fn 白名单为空表示不限() {
    let h = spawn(vec![access_key("rk-main", true, (0, "", vec![]))], |_| {}).await;
    for m in ["alpha", "beta", "完全不存在的模型"] {
        let r = chat_with(&h.base_url, KEY_SECRET, m).await;
        // 不存在的模型会是别的错（ModelNotFound），但**不是 403**
        assert_ne!(
            r.status(),
            reqwest::StatusCode::FORBIDDEN,
            "白名单为空时 {m} 不该被闸门拒绝"
        );
    }
}

// ---------- 预算闸门 ----------

/// 计数型断言：前后各数一次 requests 行数。
#[tokio::test]
async fn 累计超过预算后返回_429_且_不写_requests_行() {
    let h = spawn(
        vec![access_key(
            "rk-main",
            true,
            (MICROS_PER_UNIT, "USD", vec![]),
        )],
        |_| {},
    )
    .await;
    let since = month_start();

    // 先确认可以正常发（对照组：证明这个 Key 本身是通的）
    let ok = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(ok.status(), reqwest::StatusCode::OK);

    let before = repo::count_requests_for_key(h.db.pool(), "rk-main", since)
        .await
        .unwrap();

    // 花掉 2 元，超过 1 元的上限
    seed_spend(&h.db, "rk-main", 2.0, "USD").await;
    let after_seed = repo::count_requests_for_key(h.db.pool(), "rk-main", since)
        .await
        .unwrap();
    assert_eq!(after_seed, before + 1, "塞进去的那条要能被数到");

    let calls_before = h.mock_calls.load(Ordering::SeqCst);
    let r = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    let (status, code, body) = error_code(r).await;

    assert_eq!(
        status,
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "超预算要 429"
    );
    assert_eq!(
        code, "budget_exceeded",
        "code 必须是 budget_exceeded，不是笼统的限流"
    );
    // 报具体的已用与上限，用户才知道该充值还是该换 Key
    assert_eq!(body["error"]["limit"], MICROS_PER_UNIT);
    assert_eq!(body["error"]["used"], 2 * MICROS_PER_UNIT);
    assert_eq!(body["error"]["currency"], "USD");

    // 关键：被拒的请求不该打上游，也不该进 requests 表
    assert_eq!(
        h.mock_calls.load(Ordering::SeqCst),
        calls_before,
        "被预算拦下的请求不该打上游"
    );
    let after = repo::count_requests_for_key(h.db.pool(), "rk-main", since)
        .await
        .unwrap();
    assert_eq!(
        after, after_seed,
        "被拒的请求不该写 requests —— 它没产生真实消费"
    );
}

#[tokio::test]
async fn 多币种不相加() {
    let h = spawn(
        vec![access_key(
            "rk-main",
            true,
            (MICROS_PER_UNIT, "USD", vec![]),
        )],
        |_| {},
    )
    .await;
    // CNY 花了 100，但 Key 的预算是 USD —— 两者不能相加
    seed_spend(&h.db, "rk-main", 100.0, "CNY").await;
    // USD 只花了 0.5，未超 1 USD
    seed_spend(&h.db, "rk-main", 0.5, "USD").await;

    let r = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(
        r.status(),
        reqwest::StatusCode::OK,
        "USD 只花 0.5 / 上限 1，不该因为 CNY 那 100 被拦"
    );

    // 对照组：USD 再花 0.6 就超了，必须拦 ——
    // 证明上面那次放行不是因为闸门整体没工作
    seed_spend(&h.db, "rk-main", 0.6, "USD").await;
    let r2 = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(r2.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn 月份切换后预算重新累计() {
    let h = spawn(
        vec![access_key(
            "rk-main",
            true,
            (MICROS_PER_UNIT, "USD", vec![]),
        )],
        |_| {},
    )
    .await;
    let since = month_start();

    // 本月花 2 元 → 超预算
    seed_spend(&h.db, "rk-main", 2.0, "USD").await;
    let blocked = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(blocked.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    // 把那条记录的 ts 改到上个月 —— 不改数据本身，只改时间。
    // 用可注入的时钟会更好，但 requests.ts 是 `Utc::now()` 写死的，
    // 改造成本高于收益；直接改 ts 达到同样的效果且更直观。
    sqlx::query("UPDATE requests SET ts = ? WHERE access_key_id = ?")
        .bind(since - 1)
        .bind("rk-main")
        .execute(h.db.pool())
        .await
        .unwrap();

    // 上月的不该计入本月
    let allowed = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(
        allowed.status(),
        reqwest::StatusCode::OK,
        "上个月的消费不该算进本月"
    );
}

// ---------- 模型白名单 ----------

#[tokio::test]
async fn 模型不在白名单时返回_403_且_不写_requests_行() {
    let h = spawn(
        vec![access_key(
            "rk-main",
            true,
            (0, "", vec!["alpha".to_string()]),
        )],
        |_| {},
    )
    .await;
    let since = month_start();

    // 对照：白名单里的模型可以走
    let ok = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(ok.status(), reqwest::StatusCode::OK, "白名单内的模型要放行");

    let before = repo::count_requests_for_key(h.db.pool(), "rk-main", since)
        .await
        .unwrap();
    let calls_before = h.mock_calls.load(Ordering::SeqCst);

    let r = chat_with(&h.base_url, KEY_SECRET, "beta").await;
    let (status, code, body) = error_code(r).await;

    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "不在白名单要 403");
    assert_eq!(code, "model_not_allowed");
    assert_eq!(body["error"]["model"], "beta", "要写明被拒的模型名");
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), calls_before);
    assert_eq!(
        repo::count_requests_for_key(h.db.pool(), "rk-main", since)
            .await
            .unwrap(),
        before,
        "被白名单拦下的请求不该写 requests"
    );
}

#[tokio::test]
async fn 白名单支持星号通配() {
    let h = spawn(
        vec![access_key(
            "rk-main",
            true,
            (0, "", vec!["al*".to_string()]),
        )],
        |_| {},
    )
    .await;
    let ok = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(ok.status(), reqwest::StatusCode::OK, "al* 应放行 alpha");
    let no = chat_with(&h.base_url, KEY_SECRET, "beta").await;
    assert_eq!(
        no.status(),
        reqwest::StatusCode::FORBIDDEN,
        "al* 不该放行 beta"
    );
}

// ---------- 反向用例 ----------

#[tokio::test]
async fn 禁用该_key_后不再放行() {
    // 必须同时存在一个**启用**的 Key：远程模式下 `serve` 要求
    // 「至少一个已启用的独立访问 Key」，一个都没有时网关直接不启动，
    // 症状是「网关没在预期时间内起来」而看不出是配置问题。
    // 第一版只建了停用的那个，于是这条用例挂住而不是给出断言失败。
    let h = spawn(
        vec![
            access_key("rk-live", true, (0, "", vec![])),
            access_key("rk-disabled", false, (0, "", vec![])),
        ],
        |_| {},
    )
    .await;

    // 停用的 Key：它的 hash 是自己 id 的哈希（见 access_key 辅助函数）
    let r = chat_with(&h.base_url, "rk-disabled", "alpha").await;
    assert_eq!(
        r.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "停用的 Key 必须 401"
    );

    // 对照组：同一个网关，启用的那个 Key 能正常走 ——
    // 证明上面的 401 是因为「停用」，不是因为网关整体不可用。
    // 秘密就是它自己的 id（见 access_key 辅助函数的 hash 口径）。
    let ok = chat_with(&h.base_url, "rk-live", "alpha").await;
    assert_eq!(
        ok.status(),
        reqwest::StatusCode::OK,
        "启用的 Key 必须能走，否则上面那条 401 说明不了什么"
    );
}

/// **反向用例：关闭预算功能后闸门完全不生效。**
///
/// 没有这条，上面每一条「开启时被拦」的用例都可能只是因为代码一直返回 429。
#[tokio::test]
async fn 关闭预算功能后闸门完全不生效() {
    let h = spawn(
        vec![access_key(
            "rk-main",
            true,
            (MICROS_PER_UNIT, "USD", vec!["alpha".to_string()]),
        )],
        |cfg| {
            cfg.budget.enabled = false;
        },
    )
    .await;
    // 既超预算、模型也不在白名单
    seed_spend(&h.db, "rk-main", 999.0, "USD").await;

    let r = chat_with(&h.base_url, KEY_SECRET, "beta").await;
    assert_eq!(
        r.status(),
        reqwest::StatusCode::OK,
        "总开关关掉后两道闸门都必须不生效"
    );

    // 对照组：同一个网关，把开关打开就必须拦 ——
    // 证明上面那次放行是「开关关着」，不是「闸门永远不拦」
    h.gateway.cfg.write().budget.enabled = true;
    let r2 = chat_with(&h.base_url, KEY_SECRET, "beta").await;
    assert_eq!(r2.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
}

/// 本机统一 Key 不受闸门影响：闸门是**per 远程 Key** 的。
#[tokio::test]
async fn 本机统一_key_不受闸门影响() {
    let h = spawn(
        vec![access_key(
            "rk-main",
            true,
            (MICROS_PER_UNIT, "USD", vec!["alpha".to_string()]),
        )],
        |_| {},
    )
    .await;
    seed_spend(&h.db, "rk-main", 999.0, "USD").await;

    // 用本机统一 Key 请求一个「不在那个远程 Key 白名单里」的模型
    let r = client()
        .post(format!("{}/v1/chat/completions", h.base_url))
        .bearer_auth("lgw-local-unified")
        .json(&serde_json::json!({
            "model": "beta",
            "messages": [{"role": "user", "content": "你好"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        reqwest::StatusCode::OK,
        "本机统一 Key 没有预算与白名单，不该被远程 Key 的配置影响"
    );
}

/// 真实请求会把 access_key_id 写进 requests —— 否则预算永远从 0 开始。
#[tokio::test]
async fn 真实请求把_access_key_id_写进_requests() {
    let h = spawn(vec![access_key("rk-main", true, (0, "", vec![]))], |_| {}).await;
    let since = month_start();
    let before = repo::count_requests_for_key(h.db.pool(), "rk-main", since)
        .await
        .unwrap();

    let r = chat_with(&h.base_url, KEY_SECRET, "alpha").await;
    assert_eq!(r.status(), reqwest::StatusCode::OK);

    let after = repo::count_requests_for_key(h.db.pool(), "rk-main", since)
        .await
        .unwrap();
    assert_eq!(
        after,
        before + 1,
        "真实请求必须归属到这个 Key —— 否则预算统计永远是 0，闸门形同虚设"
    );
}
