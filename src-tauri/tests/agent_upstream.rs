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
fn 注册表默认只装假适配器() {
    use llm_gateway_lib::agent_upstream::AdapterRegistry;
    let r = AdapterRegistry::with_builtins();
    assert_eq!(
        r.ids(),
        vec!["fake"],
        "生产注册表里现在只有假适配器 —— A6/A7 才加真的"
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
