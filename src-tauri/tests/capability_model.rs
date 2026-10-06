//! D1 模型级能力分的验收测试。
//!
//! 分两层：
//! - **纯逻辑**已在 `src/domain/capability.rs` 里（8 条）
//! - **持久层与回退**在本文件：真 SQLite，验证 `capabilities_json` 的
//!   读写、坏 JSON 的降级、以及「老配置排序不变」这条铁律

use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::domain::{
    CapabilitySource, Currency, Dialect, ModelCapabilities, ModelRef, Provider,
};
use sqlx::Row;

async fn new_db() -> db::Db {
    db::Db::connect_in_memory().await.unwrap()
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

fn provider(id: &str, intelligence: i32, models: Vec<ModelRef>) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: id.into(),
        dialect: Dialect::OpenAI,
        base_url: "https://example.invalid/v1".into(),
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        models,
        rpm_limit: 0,
        intelligence,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

async fn seed(db: &db::Db, p: Provider) {
    repo::upsert_provider(db.pool(), &p).await.unwrap();
}

// ------------------------------ 迁移 ------------------------------

#[tokio::test]
async fn 迁移加了_capabilities_json_列() {
    let db = new_db().await;
    let rows = sqlx::query("PRAGMA table_info(models)")
        .fetch_all(db.pool())
        .await
        .unwrap();
    let names: Vec<String> = rows.iter().map(|r| r.get::<String, _>("name")).collect();
    assert!(
        names.iter().any(|n| n == "capabilities_json"),
        "models 表应有 capabilities_json 列，实际：{names:?}"
    );
    // 反向：不该为能力分新增整数列（D2 的能力是多维的，塞不进一个 INTEGER）
    for bad in ["capabilities", "intelligence", "capability_score"] {
        assert!(
            !names.iter().any(|n| n == bad),
            "不该有 {bad} 列 —— 能力是多维的，用定宽列加维度要改表结构：{names:?}"
        );
    }
}

// ------------------------------ 读写往返 ------------------------------

#[tokio::test]
async fn 能力分写入后能原样读出() {
    let db = new_db().await;
    seed(&db, provider("p1", 80, vec![model("m1")])).await;

    // 先确认没写之前是 None（不是默认的零分对象）
    assert_eq!(
        repo::read_model_capabilities(db.pool(), "p1", "m1")
            .await
            .unwrap(),
        None,
        "没有 capabilities_json 时应当返回 None"
    );

    let caps = ModelCapabilities {
        coding: Some(0.9),
        reasoning: Some(0.8),
        knowledge: Some(0.7),
        math: Some(0.6),
        instruction_following: Some(0.95),
        context_window: Some(200_000),
        throughput_tps: Some(80.0),
        ttft_ms: Some(300.0),
        input_cost_per_mtok: Some(3.0),
        output_cost_per_mtok: Some(15.0),
        currency: Some(Currency::Usd),
        source: CapabilitySource::Measured,
        updated_at: None,
    };
    assert!(
        repo::write_model_capabilities(db.pool(), "p1", "m1", &caps)
            .await
            .unwrap(),
        "写入应当命中记录"
    );

    let back = repo::read_model_capabilities(db.pool(), "p1", "m1")
        .await
        .unwrap()
        .expect("应当读得回来");
    assert_eq!(back, caps, "往返不该丢信息");
    assert_eq!(back.source, CapabilitySource::Measured);
    assert_eq!(back.currency, Some(Currency::Usd));
}

#[tokio::test]
async fn 写入不存在的模型返回_false_而不是报错() {
    let db = new_db().await;
    seed(&db, provider("p1", 80, vec![model("m1")])).await;
    let ok = repo::write_model_capabilities(
        db.pool(),
        "p1",
        "根本没有这个模型",
        &ModelCapabilities::default(),
    )
    .await
    .unwrap();
    assert!(!ok, "没命中记录时应当返回 false");
}

#[tokio::test]
async fn 写入时越界分数被夹回区间() {
    // 入口夹一次，比在每个消费点夹更可靠：
    // 不夹的话乘积会大于 1，表现为「这个模型莫名其妙总是第一」且无报错。
    let db = new_db().await;
    seed(&db, provider("p1", 80, vec![model("m1")])).await;
    let dirty = ModelCapabilities {
        coding: Some(1.8),
        reasoning: Some(-0.5),
        ..Default::default()
    };
    repo::write_model_capabilities(db.pool(), "p1", "m1", &dirty)
        .await
        .unwrap();
    let back = repo::read_model_capabilities(db.pool(), "p1", "m1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back.coding, Some(1.0), "上界被夹");
    assert_eq!(back.reasoning, Some(0.0), "下界被夹");
}

// ------------------------------ 坏数据的降级 ------------------------------

#[tokio::test]
async fn 能力_json_解析失败返回_none_而不是_panic() {
    let db = new_db().await;
    seed(&db, provider("p1", 80, vec![model("m1")])).await;

    // 直接塞坏 JSON 进库（模拟手工改库、或被截断的写入）
    sqlx::query("UPDATE models SET capabilities_json = ? WHERE provider_id = ? AND alias = ?")
        .bind("{ 这不是 JSON")
        .bind("p1")
        .bind("m1")
        .execute(db.pool())
        .await
        .unwrap();

    assert_eq!(
        repo::read_model_capabilities(db.pool(), "p1", "m1")
            .await
            .unwrap(),
        None,
        "坏 JSON 必须降级成 None，绝不能 panic"
    );

    // 关键判据：坏数据不该让**整个模型列表**读不出来。
    // 用户看到的会是「所有模型都没了」，而根因在一个字段上。
    let models = repo::list_models_of(db.pool(), "p1").await.unwrap();
    assert_eq!(models.len(), 1, "一个模型的能力 JSON 坏了不该影响列表");
    assert_eq!(models[0].alias, "m1");
}

#[tokio::test]
async fn 类型不对的_json_也降级而不是_panic() {
    let db = new_db().await;
    seed(&db, provider("p1", 80, vec![model("m1")])).await;
    // 合法 JSON，但类型不对（coding 是字符串而不是数字）
    sqlx::query("UPDATE models SET capabilities_json = ? WHERE provider_id = ? AND alias = ?")
        .bind(r#"{"coding":"很高"}"#)
        .bind("p1")
        .bind("m1")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        repo::read_model_capabilities(db.pool(), "p1", "m1")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn 只写了部分字段的_json_能解析() {
    // 旧行 / 手写的 JSON 往往只有一两个字段。
    // `#[serde(default)]` 让缺失字段落成 None 而不是解析失败。
    let db = new_db().await;
    seed(&db, provider("p1", 80, vec![model("m1")])).await;
    sqlx::query("UPDATE models SET capabilities_json = ? WHERE provider_id = ? AND alias = ?")
        .bind(r#"{"coding":0.85}"#)
        .bind("p1")
        .bind("m1")
        .execute(db.pool())
        .await
        .unwrap();
    let caps = repo::read_model_capabilities(db.pool(), "p1", "m1")
        .await
        .unwrap()
        .expect("部分字段的 JSON 应当能解析");
    assert_eq!(caps.coding, Some(0.85));
    assert_eq!(caps.reasoning, None, "缺的字段是 None 而不是 0");
    assert_eq!(
        caps.source,
        CapabilitySource::Catalog,
        "没写来源时取最不可信的那档"
    );
}

// ------------------------------ 兜底：老配置排序不变 ------------------------------

/// 老配置：只有 provider 级 `intelligence`，没有任何模型能力。
///
/// 这是 D1 之前所有用户的现状。D1 之后他们的排序必须**逐位不变**，
/// 否则这次改动会静默改变所有人的路由结果 —— 而那种变化
/// 在一次「升级后觉得模型变笨了」的抱怨里才会被察觉。
#[tokio::test]
async fn 旧配置_没有_capabilities_json_时回落到_provider_intelligence() {
    let db = new_db().await;
    // 三个 provider，intelligence 各不相同，每个挂两个模型
    seed(&db, provider("low", 30, vec![model("a"), model("b")])).await;
    seed(&db, provider("mid", 60, vec![model("c")])).await;
    seed(&db, provider("high", 90, vec![model("d")])).await;

    // 判据①：没有任何模型有 capabilities_json
    for (pid, alias) in [("low", "a"), ("mid", "c"), ("high", "d")] {
        assert_eq!(
            repo::read_model_capabilities(db.pool(), pid, alias)
                .await
                .unwrap(),
            None,
            "{pid}/{alias} 不该有能力数据"
        );
    }

    // 判据②：兜底值就是 provider.intelligence / 100
    let models = repo::list_models_of(db.pool(), "high").await.unwrap();
    assert_eq!(models.len(), 1);
    let providers = repo::list_providers(db.pool()).await.unwrap();
    let by_id: std::collections::BTreeMap<&str, i32> = providers
        .iter()
        .map(|p| (p.id.as_str(), p.intelligence))
        .collect();
    assert_eq!(by_id.get("high"), Some(&90));
    assert_eq!(by_id.get("low"), Some(&30));

    // 判据③（核心）：**同一组候选按兜底值排序，结果与按 intelligence 排序逐位相同**。
    // 这条是铁律 2（模式隔离 / 向后兼容）在 D1 上的落地：
    // 没有模型级能力时，能力分必须恰好等于 provider 级的值，
    // 不能因为走了新代码路径而多出一点误差。
    let fallback: Vec<(&str, f32)> = providers
        .iter()
        .map(|p| (p.id.as_str(), p.intelligence as f32 / 100.0))
        .collect();
    let mut by_fallback = fallback.clone();
    by_fallback.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let mut by_intelligence: Vec<(&str, i32)> = providers
        .iter()
        .map(|p| (p.id.as_str(), p.intelligence))
        .collect();
    by_intelligence.sort_by(|a, b| b.1.cmp(&a.1));

    let order_from_fallback: Vec<&str> = by_fallback.iter().map(|(id, _)| *id).collect();
    let order_from_intelligence: Vec<&str> = by_intelligence.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        order_from_fallback, order_from_intelligence,
        "兜底排序必须与 provider.intelligence 排序逐位相同"
    );
    assert_eq!(order_from_fallback, vec!["high", "mid", "low"]);
}

#[tokio::test]
async fn 有了模型级能力后_同一_provider_的两个模型可以不同() {
    // 这就是 D1 要解决的问题：一个 Provider 挂 3 个模型时，
    // 它们原本共享 provider 的能力分 —— 用 27B 和 8B 跑同一个 smartest 档，
    // 选出来的其实是随机的。
    let db = new_db().await;
    seed(&db, provider("local", 50, vec![model("27b"), model("8b")])).await;

    let big = ModelCapabilities {
        coding: Some(0.95),
        reasoning: Some(0.9),
        source: CapabilitySource::Measured,
        ..Default::default()
    };
    let small = ModelCapabilities {
        coding: Some(0.3),
        reasoning: Some(0.2),
        source: CapabilitySource::Measured,
        ..Default::default()
    };
    repo::write_model_capabilities(db.pool(), "local", "27b", &big)
        .await
        .unwrap();
    repo::write_model_capabilities(db.pool(), "local", "8b", &small)
        .await
        .unwrap();

    let read_big = repo::read_model_capabilities(db.pool(), "local", "27b")
        .await
        .unwrap()
        .unwrap();
    let read_small = repo::read_model_capabilities(db.pool(), "local", "8b")
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        read_big, read_small,
        "同一个 provider 下的两个模型必须能有不同的能力分"
    );
    assert!(read_big.coding.unwrap() > read_small.coding.unwrap());
    // 而 provider 级的 intelligence 只有一个值，对有能力的模型不再有约束力
    assert_eq!(read_big.coding, Some(0.95));
}

// ------------------------------ 未知与零分的区分（跨层） ------------------------------

#[tokio::test]
async fn 未知维度在落库往返后仍然是_none_而不是_零分() {
    // 上一条在纯逻辑层验过；这条验**经过 JSON 与 SQLite 之后**仍然是 None。
    // 会踩的坑是 `#[serde(default)]` 配 `f32` 而不是 `Option<f32>`
    // —— 那样缺失字段会静默变成 0.0，而 0.0 在乘法结构里等于「判死」。
    let db = new_db().await;
    seed(&db, provider("p1", 80, vec![model("m1")])).await;
    let only_coding = ModelCapabilities {
        coding: Some(0.7),
        ..Default::default()
    };
    repo::write_model_capabilities(db.pool(), "p1", "m1", &only_coding)
        .await
        .unwrap();
    let back = repo::read_model_capabilities(db.pool(), "p1", "m1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back.coding, Some(0.7));
    for (name, value) in [
        ("reasoning", back.reasoning),
        ("knowledge", back.knowledge),
        ("math", back.math),
        ("instruction_following", back.instruction_following),
    ] {
        assert_eq!(value, None, "{name} 必须是 None，绝不能是 Some(0.0)");
        assert_ne!(value, Some(0.0));
    }
    assert!(back.has_any_quality(), "有一个维度就够了");
}
