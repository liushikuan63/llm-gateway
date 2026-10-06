/// 模型级禁用：禁用的模型不进路由候选链，且**禁用状态撑得过一次保存**。
///
/// 这条能力此前只有半截实现：`models` 表早有 `enabled` 列、`list_models_of`
/// 也一直在 `WHERE enabled = 1` 过滤，但 `ModelRef` 里没有对应字段 ——
/// 于是 INSERT 不写这一列（走列默认值 1），`upsert_provider` 又是
/// DELETE 后重插，用户下次点保存，刚禁用的模型就复活了。
///
/// 下面四条分别钉住：默认启用、序列化往返、路由过滤、以及那条最关键的
/// 「读出来再存回去不得翻转禁用位」。
use llm_gateway_lib::db::repo;
use llm_gateway_lib::domain::{Dialect, ModelRef, ModelType, Provider};

fn model(alias: &str, enabled: bool) -> ModelRef {
    ModelRef {
        alias: alias.into(),
        enabled,
        upstream: alias.into(),
        context_window: 128_000,
        supports_tools: true,
        supports_vision: false,
        supports_audio: false,
        supports_video: false,
        supports_thinking: false,
        supports_stream: true,
        model_type: ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: None,
        local: None,
        capabilities: None,
    }
}

fn provider_with(models: Vec<ModelRef>) -> Provider {
    Provider {
        id: "p1".into(),
        name: "测试供应商".into(),
        dialect: Dialect::OpenAI,
        base_url: "https://example.test/v1".into(),
        api_key_enc: "cipher".into(),
        enabled: true,
        priority: 10,
        models,
        rpm_limit: 0,
        intelligence: 70,
        note: None,
        runtime_id: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

async fn pool() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("内存库");
    // 手写建表：**加列时要跟着改这里**。
    // D1 加了 `capabilities_json`，漏加会让 upsert 报
    // "table models has no column named capabilities_json" ——
    // 而这个文件里的两条用例与能力分无关，报错完全指不到原因。
    sqlx::query("CREATE TABLE models (id TEXT PRIMARY KEY, provider_id TEXT, alias TEXT, upstream TEXT, context_window INTEGER, supports_tools INTEGER, supports_vision INTEGER, supports_audio INTEGER, supports_video INTEGER, supports_thinking INTEGER, supports_stream INTEGER, model_type TEXT, upstream_path TEXT, enabled INTEGER NOT NULL DEFAULT 1, price_json TEXT, overrides_json TEXT, local_json TEXT, capabilities_json TEXT)")
        .execute(&pool)
        .await
        .expect("建表");
    // 手写建表：**加列时要跟着改这里**。
    // D1 加了 `capabilities_json`、任务卡二 A5 加了 `runtime_id`，
    // 漏加会让 upsert 报 "table providers has no column named runtime_id"
    // —— 而这个文件里的用例与账号型上游无关，报错完全指不到原因。
    sqlx::query("CREATE TABLE providers (id TEXT PRIMARY KEY, name TEXT, dialect TEXT, base_url TEXT, api_key_enc TEXT, enabled INTEGER, priority INTEGER, rpm_limit INTEGER, intelligence INTEGER, note TEXT, runtime_id TEXT, created_at TEXT, updated_at TEXT)")
        .execute(&pool)
        .await
        .expect("建表");
    pool
}

/// 反向判据：字段必须有默认值。
///
/// 若 `#[serde(default = "default_true")]` 写成 `#[serde(default)]`（即默认
/// false），那么**所有旧配置在反序列化后会被整体禁用** —— 升级即断供，
/// 且很难查。这条断言挡的就是它。
#[test]
fn 未显式给出_enabled_时默认启用() {
    let payload = r#"{
        "alias": "m", "upstream": "m", "context_window": 1024,
        "supports_tools": true, "supports_vision": false,
        "supports_audio": false, "supports_video": false,
        "supports_thinking": false, "supports_stream": true,
        "model_type": "chat", "upstream_path": null,
        "price": null, "overrides": null, "local": null
    }"#;
    let parsed: ModelRef = serde_json::from_str(payload).expect("旧格式载荷应能解析");
    assert!(
        parsed.enabled,
        "缺 enabled 的旧载荷必须默认为启用；默认 false 会让升级即断供"
    );
}

/// 显式给 false 时必须真的解析成 false，不能被 default 覆盖。
#[test]
fn 显式禁用_解析后确实是禁用() {
    let payload = r#"{
        "alias": "m", "upstream": "m", "context_window": 1024,
        "supports_tools": true, "supports_vision": false,
        "supports_audio": false, "supports_video": false,
        "supports_thinking": false, "supports_stream": true,
        "model_type": "chat", "upstream_path": null,
        "enabled": false,
        "price": null, "overrides": null, "local": null
    }"#;
    let parsed: ModelRef = serde_json::from_str(payload).expect("应能解析");
    assert!(!parsed.enabled, "显式 enabled:false 必须被尊重");
}

/// 核心：禁用状态必须撑得过一次「读出来 → 整体回写」。
///
/// 这正是修复前的行为—— INSERT 不写 enabled 列，走默认值 1，模型复活。
/// 对照组：enabled: true 的模型回写后仍为 true（否则这条测试可能只是
/// 「反正全被禁用了也说得通」）。
#[tokio::test]
async fn 禁用状态撑得过一次保存() {
    let pool = pool().await;
    let original = provider_with(vec![model("alive", true), model("muted", false)]);
    repo::upsert_provider(&pool, &original)
        .await
        .expect("首次写入");

    // 直接查库，确认 enabled 列真的落到了 0
    let raw: i64 =
        sqlx::query_scalar("SELECT enabled FROM models WHERE provider_id='p1' AND alias='muted'")
            .fetch_one(&pool)
            .await
            .expect("应能查到");
    assert_eq!(raw, 0, "INSERT 必须真的写入 enabled=0，而不是靠列默认值");

    // 走一遍真实的编辑流程：读 → 回写
    let mut reloaded = repo::list_providers(&pool).await.expect("读取");
    assert_eq!(reloaded.len(), 1);
    let muted = reloaded[0].models.iter().find(|m| m.alias == "muted");
    assert!(
        muted.is_some(),
        "前端必须能看到已禁用的模型，否则无法在界面上勾回来"
    );

    // 模拟用户在编辑器里改了点别的（改个上下文长度）后保存
    reloaded[0]
        .models
        .iter_mut()
        .for_each(|m| m.context_window = 200_000);
    repo::upsert_provider(&pool, &reloaded[0])
        .await
        .expect("回写");

    let after: i64 =
        sqlx::query_scalar("SELECT enabled FROM models WHERE provider_id='p1' AND alias='muted'")
            .fetch_one(&pool)
            .await
            .expect("应能查到");
    assert_eq!(after, 0, "保存不得把已禁用的模型复活");
    let alive_after: i64 =
        sqlx::query_scalar("SELECT enabled FROM models WHERE provider_id='p1' AND alias='alive'")
            .fetch_one(&pool)
            .await
            .expect("应能查到");
    assert_eq!(alive_after, 1, "启用的模型不应被误禁");
}

/// 路由侧只应看到已启用的模型；前端侧要看到全部。
#[tokio::test]
async fn 路由只拿启用态_前端拿全量() {
    let pool = pool().await;
    repo::upsert_provider(
        &pool,
        &provider_with(vec![model("alive", true), model("muted", false)]),
    )
    .await
    .expect("写入");

    let routable = repo::list_routable_providers(&pool)
        .await
        .expect("路由视图");
    assert_eq!(
        routable[0].models.len(),
        1,
        "路由候选链里不应出现禁用的模型"
    );
    assert_eq!(routable[0].models[0].alias, "alive");

    let all = repo::list_providers(&pool).await.expect("前端视图");
    assert_eq!(
        all[0].models.len(),
        2,
        "前端必须能看到全部模型，否则无法重新启用"
    );

    let named = repo::list_routable_models_of(&pool, "p1")
        .await
        .expect("点名视图");
    assert_eq!(named.len(), 1, "精确点名时同样只认启用态");
}
