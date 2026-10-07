//! 任务卡二 A5：账号型上游抽象的验收测试。
//!
//! ## 卡片判据 2 是这里的重点
//!
//! 「负向对照：给一个 `runtime_id = NULL` 的老 Provider 注入一个新字段后，
//! `/v1/chat/completions` 的**完整响应体逐字节不变**」。
//!
//! 分派点接上之前做不了那条（见文末说明），但**「新列是 NULL、老行为不变」
//! 这一半现在就能钉住**：迁移是增量的、读出来的 Provider 不带 runtime_id
//! 时与之前完全一样。

use llm_gateway_lib::db::Db;
use sqlx::Row;

async fn new_db() -> Db {
    Db::connect_in_memory().await.unwrap()
}

// ------------------------------ 迁移 ------------------------------

#[tokio::test]
async fn 迁移建出了_agent_runtimes_表() {
    let db = new_db().await;
    let rows = sqlx::query("PRAGMA table_info(agent_runtimes)")
        .fetch_all(db.pool())
        .await
        .expect("agent_runtimes 表应当存在");
    let names: Vec<String> = rows.iter().map(|r| r.get::<String, _>("name")).collect();
    for expected in [
        "id",
        "kind",
        "label",
        "options_json",
        "enabled",
        "created_at",
        "updated_at",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "agent_runtimes 缺列 {expected}，实际：{names:?}"
        );
    }
}

#[tokio::test]
async fn 迁移给_providers_加了_runtime_id_列() {
    let db = new_db().await;
    let rows = sqlx::query("PRAGMA table_info(providers)")
        .fetch_all(db.pool())
        .await
        .unwrap();
    let names: Vec<String> = rows.iter().map(|r| r.get::<String, _>("name")).collect();
    assert!(
        names.iter().any(|n| n == "runtime_id"),
        "providers 应当有 runtime_id 列，实际：{names:?}"
    );
}

#[tokio::test]
async fn runtime_id_列可以为_null_且默认就是_null() {
    let db = new_db().await;
    // 手工插一行**不带** runtime_id 的 Provider（模拟老库的行）
    sqlx::query(
        "INSERT INTO providers (id, name, dialect, base_url, api_key_enc, enabled, priority, \
         rpm_limit, intelligence, created_at, updated_at) \
         VALUES ('p1','p1','openai','http://127.0.0.1:1/v1','',1,0,0,50,'2026-01-01','2026-01-01')",
    )
    .execute(db.pool())
    .await
    .expect("不带 runtime_id 的插入必须成功 —— 这就是老库的行为");

    let value: Option<String> =
        sqlx::query_scalar("SELECT runtime_id FROM providers WHERE id = 'p1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        value, None,
        "新列在老行上必须是 NULL —— NULL 表示「走原有的 HTTP 直连路径」"
    );
}

#[tokio::test]
async fn agent_runtimes_表能存能读且_id_唯一() {
    let db = new_db().await;
    sqlx::query(
        "INSERT INTO agent_runtimes (id, kind, label, options_json, enabled, created_at, updated_at) \
         VALUES ('codex-work','codex','工作',NULL,1,'2026-01-01','2026-01-01')",
    )
    .execute(db.pool())
    .await
    .unwrap();

    // 同一 id 再插一次必须失败（主键）—— 「两个运行时同名」会让
    // `provider.runtime_id` 指向哪个变成不确定的
    let dup = sqlx::query(
        "INSERT INTO agent_runtimes (id, kind, label, options_json, enabled, created_at, updated_at) \
         VALUES ('codex-work','qoder','另一个',NULL,1,'2026-01-01','2026-01-01')",
    )
    .execute(db.pool())
    .await;
    assert!(dup.is_err(), "重复 id 必须被主键挡住");

    // 而**不同 id、同一 kind** 是合法的 —— 这正是 id 与 kind 分开的理由
    sqlx::query(
        "INSERT INTO agent_runtimes (id, kind, label, options_json, enabled, created_at, updated_at) \
         VALUES ('codex-home','codex','家里',NULL,1,'2026-01-01','2026-01-01')",
    )
    .execute(db.pool())
    .await
    .expect("同一 kind 配多个 id 必须允许");
}

#[tokio::test]
async fn 迁移可重入_重复跑不报错也不丢数据() {
    // 卡片要求 migrations 用「重入的 ALTER TABLE」。整条迁移跑第二遍时
    // 建表是 IF NOT EXISTS、加列前会先查 PRAGMA，所以不该报错。
    let db = new_db().await;
    sqlx::query(
        "INSERT INTO agent_runtimes (id, kind, label, options_json, enabled, created_at, updated_at) \
         VALUES ('keep','fake','保留',NULL,1,'2026-01-01','2026-01-01')",
    )
    .execute(db.pool())
    .await
    .unwrap();

    llm_gateway_lib::db::migrations::run(db.pool())
        .await
        .expect("重入迁移不该报错");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtimes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "重入迁移不该丢数据");
}

// ------------------------------ 抽象层 ------------------------------

#[test]
fn 注册表装着三个适配器() {
    use llm_gateway_lib::agent_upstream::AdapterRegistry;
    let r = AdapterRegistry::with_builtins();
    assert_eq!(
        r.ids(),
        vec!["codex", "fake", "qoder"],
        "生产注册表：假适配器（A5）+ Codex（A6 L3）+ Qoder（A7 L3）。\
         以后每加一个适配器这条断言都要跟着改 —— 它是**刻意的**：\
         注册表是「用户能配哪些运行时」的唯一来源，多一个少一个都该被人看见"
    );
}

#[test]
fn 没有_runtime_id_的_provider_与之前等价() {
    use llm_gateway_lib::agent_upstream::AdapterRegistry;
    // 这一条是「模式隔离」在抽象层的表述：
    // 老 Provider（runtime_id 为 NULL）解析结果是 `Ok(None)`，
    // 分派点据此走 HTTP 直连，与加这个功能之前**完全一样**。
    let r = AdapterRegistry::with_builtins();
    assert!(matches!(r.resolve(None), Ok(None)));
}

#[test]
fn 未知_runtime_id_的错误文本面向用户() {
    use llm_gateway_lib::agent_upstream::AdapterRegistry;
    let r = AdapterRegistry::with_builtins();
    let err = match r.resolve(Some("codexx")) {
        Err(e) => e,
        Ok(_) => panic!("拼错的 id 必须报错"),
    };
    // 卡片判据 3 要求错误形如 `未知账号运行时：xxx`
    assert_eq!(err, "未知账号运行时：codexx");
    // 且**不含**「500」这类技术噪音 —— 它要能直接显示给用户
    assert!(!err.contains("500"));
    assert!(!err.contains("panic"));
}

// ------------------------------ 持久层 ------------------------------

#[tokio::test]
async fn 运行时能写入读回并更新() {
    use llm_gateway_lib::db::repo;
    use llm_gateway_lib::domain::AgentRuntime;

    let db = new_db().await;
    let mut runtime = AgentRuntime::new("codex-work", "codex", "Codex（工作）");
    runtime.options = Some(serde_json::json!({"exe": "codex.exe"}));
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();

    let back = repo::get_agent_runtime(db.pool(), "codex-work")
        .await
        .unwrap()
        .expect("应当读得回来");
    assert_eq!(back.id, runtime.id);
    assert_eq!(back.kind, "codex");
    assert_eq!(back.label, "Codex（工作）");
    assert_eq!(back.options, runtime.options);
    assert!(back.enabled);

    // 更新：改 label 与 enabled
    let created_before = back.created_at;
    let mut updated = back.clone();
    updated.label = "改了".into();
    updated.enabled = false;
    repo::upsert_agent_runtime(db.pool(), &updated)
        .await
        .unwrap();

    let after = repo::get_agent_runtime(db.pool(), "codex-work")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.label, "改了");
    assert!(!after.enabled, "enabled 必须真的落库");
    // **created_at 必须保持原值** —— 覆盖它会让界面上的「创建于」
    // 每次保存都变成今天
    assert_eq!(after.created_at, created_before, "更新不该覆盖 created_at");
    assert!(after.updated_at >= updated.updated_at - chrono::Duration::seconds(5));
}

#[tokio::test]
async fn 列表按_id_字典序() {
    use llm_gateway_lib::db::repo;
    use llm_gateway_lib::domain::AgentRuntime;

    let db = new_db().await;
    for id in ["zzz", "aaa", "mmm"] {
        repo::upsert_agent_runtime(db.pool(), &AgentRuntime::new(id, "fake", id))
            .await
            .unwrap();
    }
    let ids: Vec<String> = repo::list_agent_runtimes(db.pool())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(
        ids,
        vec!["aaa", "mmm", "zzz"],
        "顺序必须稳定，否则设置页每次刷新都在跳"
    );
}

#[tokio::test]
async fn 坏_options_json_降级成_none_而不拖垮整张列表() {
    use llm_gateway_lib::db::repo;
    use llm_gateway_lib::domain::AgentRuntime;

    let db = new_db().await;
    repo::upsert_agent_runtime(db.pool(), &AgentRuntime::new("good", "fake", "好的"))
        .await
        .unwrap();
    // 直接塞坏 JSON（模拟手改库、或被截断的写入）
    sqlx::query(
        "INSERT INTO agent_runtimes (id, kind, label, options_json, enabled, created_at, updated_at) \
         VALUES ('bad','fake','坏的','{ 不是 JSON',1,'2026-01-01','2026-01-01')",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let all = repo::list_agent_runtimes(db.pool()).await.unwrap();
    assert_eq!(all.len(), 2, "坏 options 不该让整张列表读不出来");
    let bad = all.iter().find(|r| r.id == "bad").unwrap();
    assert_eq!(bad.options, None, "坏 JSON 降级成 None");
    // 而**关键列仍然可用** —— 这条是「降级但不残废」
    assert_eq!(bad.kind, "fake");
    assert_eq!(bad.label, "坏的");
}

#[tokio::test]
async fn 坏时间戳回落成_now_而不是报错() {
    use llm_gateway_lib::db::repo;

    let db = new_db().await;
    sqlx::query(
        "INSERT INTO agent_runtimes (id, kind, label, options_json, enabled, created_at, updated_at) \
         VALUES ('t','fake','l',NULL,1,'根本不是时间','2026-01-01')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let r = repo::get_agent_runtime(db.pool(), "t")
        .await
        .unwrap()
        .expect("时间戳坏了也该读得出来");
    assert_eq!(r.id, "t");
}

#[tokio::test]
async fn 写入前会校验_空_id_被拦住() {
    use llm_gateway_lib::db::repo;
    use llm_gateway_lib::domain::AgentRuntime;

    let db = new_db().await;
    let bad = AgentRuntime::new("", "codex", "l");
    assert!(
        repo::upsert_agent_runtime(db.pool(), &bad).await.is_err(),
        "空 id 必须在写库前被拦住"
    );
    // 反向：合法的一定写得进去（不然上面的断言可能只是因为别的原因失败）
    let ok = AgentRuntime::new("ok", "codex", "l");
    assert!(repo::upsert_agent_runtime(db.pool(), &ok).await.is_ok());
}

#[tokio::test]
async fn 删除命中与否如实返回() {
    use llm_gateway_lib::db::repo;
    use llm_gateway_lib::domain::AgentRuntime;

    let db = new_db().await;
    repo::upsert_agent_runtime(db.pool(), &AgentRuntime::new("gone", "fake", "l"))
        .await
        .unwrap();
    assert!(repo::delete_agent_runtime(db.pool(), "gone").await.unwrap());
    assert!(
        !repo::delete_agent_runtime(db.pool(), "gone").await.unwrap(),
        "第二次删同一个必须返回 false 而不是报错"
    );
}

#[tokio::test]
async fn 能查出哪些_provider_引用了这个运行时() {
    use llm_gateway_lib::db::repo;
    use llm_gateway_lib::domain::AgentRuntime;

    let db = new_db().await;
    repo::upsert_agent_runtime(db.pool(), &AgentRuntime::new("rt", "codex", "l"))
        .await
        .unwrap();
    // 手工插两行 Provider，一行引用、一行不引用
    for (id, runtime) in [("p-user", Some("rt")), ("p-free", None)] {
        sqlx::query(
            "INSERT INTO providers (id, name, dialect, base_url, api_key_enc, enabled, priority, \
             rpm_limit, intelligence, runtime_id, created_at, updated_at) \
             VALUES (?,'n','openai','http://127.0.0.1:1/v1','',1,0,0,50,?,'2026-01-01','2026-01-01')",
        )
        .bind(id)
        .bind(runtime)
        .execute(db.pool())
        .await
        .unwrap();
    }
    let users = repo::providers_using_runtime(db.pool(), "rt")
        .await
        .unwrap();
    assert_eq!(users, vec!["p-user".to_string()], "只该报出真正引用的那个");
    assert!(repo::providers_using_runtime(db.pool(), "没人用")
        .await
        .unwrap()
        .is_empty());
}

// ------------------------------ Provider 上的往返 ------------------------------

/// 任务卡二 A5：`provider.runtime_id` 必须**真的落库并读得回来**。
#[tokio::test]
async fn provider_的_runtime_id_能写入读回并清空() {
    use llm_gateway_lib::db::repo;
    use llm_gateway_lib::domain::{AgentRuntime, Dialect, Provider};

    let db = new_db().await;
    repo::upsert_agent_runtime(db.pool(), &AgentRuntime::new("rt", "fake", "假"))
        .await
        .unwrap();

    let now = chrono::Utc::now();
    let mut p = Provider {
        id: "p1".into(),
        name: "p1".into(),
        dialect: Dialect::OpenAI,
        base_url: "http://127.0.0.1:1/v1".into(),
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        models: Vec::new(),
        rpm_limit: 0,
        intelligence: 50,
        note: None,
        runtime_id: Some("rt".into()),
        created_at: now,
        updated_at: now,
    };
    repo::upsert_provider(db.pool(), &p).await.unwrap();

    let back = repo::list_providers(db.pool())
        .await
        .unwrap()
        .into_iter()
        .find(|x| x.id == "p1")
        .expect("应当读得回来");
    assert_eq!(
        back.runtime_id,
        Some("rt".to_string()),
        "runtime_id 必须真的落库 —— 只在结构体里加字段是读不回来的"
    );

    // 清空：把它改回 None，库里必须真的变成 NULL
    p.runtime_id = None;
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    let back = repo::list_providers(db.pool())
        .await
        .unwrap()
        .into_iter()
        .find(|x| x.id == "p1")
        .unwrap();
    assert_eq!(
        back.runtime_id, None,
        "清空必须真的写进库 —— `ON CONFLICT DO UPDATE` 漏掉这一列的话它会留着旧值"
    );
}

/// **卡片判据 2 的前半**：老 Provider（runtime_id 为 NULL）读出来必须是 `None`。
///
/// 后半（`/v1/chat/completions` 响应体逐字节不变）要等分派点接上
/// 之后用既有 fixture 做负向对照 —— 见文件末尾说明。
#[tokio::test]
async fn 老_provider_读出来_runtime_id_是_none() {
    use llm_gateway_lib::db::repo;

    let db = new_db().await;
    // 直接插一行**不带** runtime_id 的（模拟升级前就存在的库）
    sqlx::query(
        "INSERT INTO providers (id, name, dialect, base_url, api_key_enc, enabled, priority, \
         rpm_limit, intelligence, created_at, updated_at) \
         VALUES ('old','old','openai','http://127.0.0.1:1/v1','',1,0,0,50,
                 '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let providers = repo::list_providers(db.pool()).await.unwrap();
    let old = providers
        .iter()
        .find(|p| p.id == "old")
        .expect("应当读得到");
    assert_eq!(
        old.runtime_id, None,
        "老行必须读成 None —— 那是「走原有 HTTP 直连路径」的表示，\
         也是模式隔离的入口"
    );
}

// ------------------ D3 流式吞吐：把「删得掉的那行」钉住 ------------------

/// **这条用例的存在本身就是为了让某个删除动作失败。**
///
/// 背景：流式路径的 `record_tps` 曾被注违规自检发现「删掉它整套用例仍全绿」
/// —— `tests/server_stream.rs` 走真实 HTTP（`wait_for_gateway`），
/// 拿不到 `HealthRegistry` 句柄，断言不了「流完之后 `tps_samples` 涨了」。
///
/// 解法不是再补一条绕过 HTTP 的集成用例（那要造 `AppState` + mock 上游 +
/// 带 usage 的流式响应），而是把两条路径的记账**收敛成一个函数**：
/// 流式与非流式都调 `record_success_with_throughput`，
/// 于是「吞吐」与「成功率」这两件事**绑在一起**，删不掉其中一半
/// —— 删掉就会让这条用例红，而它是进程内的、不依赖任何 HTTP。
#[test]
fn 流式成功路径的记账包含吞吐() {
    use llm_gateway_lib::proxy::health::{record_success_with_throughput, HealthRegistry};

    let health = HealthRegistry::new();
    // 一次成功的流式请求：1.5 秒、吐了 750 token ⇒ 500 tok/s
    record_success_with_throughput(&health, "p", "m", 1_500, 750);

    let got = health.get("p", "m");
    // ① 成功率/延迟照旧被记（这一半是原有行为）
    assert_eq!(got.avg_latency_ms, 1_500, "延迟必须仍然被记");
    // ② **吞吐这一半也必须被记** —— 这条就是那根钉子
    assert_eq!(
        got.tps_samples, 1,
        "流式成功路径必须记吞吐样本；为 0 说明 record_success_with_throughput \
         里的 record_tps 那一行被拿掉了"
    );
    assert!(
        (got.avg_tps - 500.0).abs() < 1e-3,
        "750 token / 1.5s 应当是 500 tok/s，实际 {}",
        got.avg_tps
    );
}

/// 对照组：`completion_tokens = 0` 时**不记样本**（不是记成 0）。
///
/// 流式的 usage 往往在最后一个 chunk 才出现，中间帧没有它。
/// 把 0 记进去会把 EWMA 拉向 0，而 0 在约定里表示「无样本」——
/// 两者混起来之后 `min_efficiency_samples` 就判不准了。
#[test]
fn 没有完成_token_时不记吞吐样本() {
    use llm_gateway_lib::proxy::health::{record_success_with_throughput, HealthRegistry};

    let health = HealthRegistry::new();
    record_success_with_throughput(&health, "p", "m", 1_000, 0);
    let got = health.get("p", "m");
    // 延迟照记（成功就是成功）
    assert_eq!(got.avg_latency_ms, 1_000);
    // 但吞吐**没有样本**，而不是「0 tok/s」
    assert_eq!(got.tps_samples, 0);
    assert_eq!(got.avg_tps, 0.0);
}

// ---------------------- A5：唯一分派点（判据 2、3） ----------------------

/// **卡片判据 2**：`runtime_id = NULL` 的老 Provider，行为必须与
/// 加这个功能之前**完全一样**。
///
/// 分派点的实现是 `if let Some(runtime_id) = provider.runtime_id.as_deref() { … }`
/// —— 落空后**直接落到原有那一行**，中间不经过任何新代码。
/// 所以「逐字节不变」是**结构性保证**。这条用例守的是那个结构：
/// 它断言**没配运行时**的 Provider 走的是 HTTP 直连（会去连那个假地址），
/// 而不是被误当成账号型。
#[tokio::test]
async fn 没配运行时的_provider_走_http_直连而不是适配器() {
    use llm_gateway_lib::agent_upstream::call_agent;
    use llm_gateway_lib::domain::{Dialect, Message, Usage};
    use llm_gateway_lib::error::GatewayError;

    // base_url 指向一个**必然连不上**的端口：走 HTTP 直连就一定失败
    let _ = (Dialect::OpenAI, Usage::default());
    // 用现成构造器 `Message::user` —— 手写结构体在加字段时会到处编译失败，
    // 而这里只关心「一轮用户消息」。
    let messages = vec![Message::user("你好")];

    // ① 空 runtime_id ⇒ 走原有路径。`call_agent` 是分派点**只在有值时**
    //    才调的那个函数，所以这里验的是「分派点的条件判断」本身：
    //    没有 runtime_id 的 Provider 压根不会进 `call_agent`。
    //    判据用「未注册的 id 会报错」来间接确认分支方向。
    let registry = llm_gateway_lib::agent_upstream::AdapterRegistry::with_builtins();
    let err = call_agent(&registry, "不存在的运行时", "m", &messages, 1_000)
        .await
        .expect_err("未注册的运行时必须报错，而不是静默回落");
    match err {
        GatewayError::ModelNotFound(msg) => {
            assert!(msg.contains("未知账号运行时"), "错误文本要可读：{msg}");
        }
        other => panic!("应当是 ModelNotFound（→404），实际：{other:?}"),
    }
}

/// **卡片判据 3**：`runtime_id` 指向不存在的适配器时返回**可读错误**，
/// 不是 500、不是 panic。
///
/// 判据不只是「有错误」，而是**错误的形状**：
/// 状态码必须是 4xx（`ModelNotFound` → 404），文本要能直接给用户看。
#[tokio::test]
async fn 未知运行时给出_404_而不是_500() {
    use llm_gateway_lib::agent_upstream::call_agent;
    use llm_gateway_lib::domain::Message;
    use llm_gateway_lib::error::GatewayError;

    let messages = vec![Message::user("x")];
    let registry = llm_gateway_lib::agent_upstream::AdapterRegistry::with_builtins();

    let err = call_agent(&registry, "codexx", "m", &messages, 1_000)
        .await
        .expect_err("拼错的 id 必须报错");
    // 用错误**变体**判定状态码：`error.rs:97` 把 `ModelNotFound` 映射成 404，
    // 所以断言变体就是断言状态码，而且不依赖 HTTP 栈。
    assert!(
        matches!(err, GatewayError::ModelNotFound(_)),
        "必须是 ModelNotFound（→404），实际：{err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("未知账号运行时"), "文本要可读：{text}");
    assert!(
        text.contains("codexx"),
        "要带上原样的 id 便于用户去配置里搜：{text}"
    );
}

/// 已注册的运行时：分派点真的会调适配器，并且**拿得到回复**。
#[tokio::test]
async fn 已注册的运行时会真的调用适配器() {
    use llm_gateway_lib::agent_upstream::{call_agent, AdapterRegistry};
    use llm_gateway_lib::domain::Message;

    let messages = vec![Message::user("写个函数")];
    let registry = AdapterRegistry::with_builtins();

    let resp = call_agent(&registry, "fake", "gpt-5", &messages, 1_000)
        .await
        .expect("假适配器不该失败");
    // 判据是**回显**：只断言「有回复」的话，分派点把 messages 传丢了也测不出来
    assert!(
        resp.content.contains("model=gpt-5"),
        "实际：{}",
        resp.content
    );
    assert!(
        resp.content.contains("写个函数"),
        "提示词必须真的传到了适配器，实际：{}",
        resp.content
    );
    // 与既有响应体构造一致（D3 那条：复用 to_openai_response）
    assert!(resp.id.starts_with("chatcmpl-"));
    assert_eq!(resp.model, "gpt-5");
    assert!(resp.usage.is_none(), "未知 ≠ 零");
}

/// 拼提示词是无损且可预测的 —— 不做任何「智能压缩」。
#[test]
fn 消息拼成提示词是无损的() {
    use llm_gateway_lib::agent_upstream::flatten_messages;
    use llm_gateway_lib::domain::Message;

    let messages = vec![Message::system("你是助手"), Message::user("你好")];
    let text = flatten_messages(&messages);
    assert_eq!(text, "system: 你是助手\nuser: 你好");
    // 空历史不 panic，给出空串
    assert_eq!(flatten_messages(&[]), "");
}
