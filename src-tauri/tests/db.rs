use std::{fs, path::PathBuf, time::Duration};

use chrono::Utc;
use llm_gateway_lib::{
    db::{repo, Db},
    domain::{Dialect, ModelRef, Provider},
};

fn temporary_database_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "llm-gateway-db-{}-{}.sqlite",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

fn provider() -> Provider {
    let now = Utc::now();
    Provider {
        id: "provider-a".to_owned(),
        name: "Provider A".to_owned(),
        dialect: Dialect::OpenAI,
        base_url: "https://example.invalid/v1".to_owned(),
        api_key_enc: "encrypted-key".to_owned(),
        enabled: true,
        priority: 10,
        models: vec![ModelRef {
            enabled: true,
            alias: "model-a".to_owned(),
            upstream: "upstream-a".to_owned(),
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
        }],
        rpm_limit: 60,
        intelligence: 80,
        note: Some("integration test".to_owned()),
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn file_database_uses_wal_and_creates_all_storage_tables() {
    let path = temporary_database_path();
    let db = Db::connect_path(&path)
        .await
        .expect("文件数据库初始化应成功");

    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(db.pool())
        .await
        .expect("应读取 journal mode");
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");

    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .fetch_all(db.pool())
            .await
            .expect("应读取表列表");
    for expected in [
        "providers",
        "models",
        "sessions",
        "session_messages",
        "requests",
        "usage_daily",
        "snapshots",
        "app_secrets",
    ] {
        assert!(
            tables.iter().any(|table| table == expected),
            "缺少表 {expected}"
        );
    }

    // 本批新增的三处迁移：models 表的两列 + requests 表的四列。
    // ensure_column 是可重入的，所以列必须真实存在而不是靠「跑过就算」。
    for (table, column) in [
        ("models", "supports_thinking"),
        ("models", "local_json"),
        ("requests", "route_intent"),
        ("requests", "route_classifier"),
        ("requests", "route_search"),
        ("requests", "route_search_hits"),
    ] {
        let found: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = '{column}'"
        ))
        .fetch_one(db.pool())
        .await
        .expect("应能读取列信息");
        assert_eq!(found, 1, "{table} 缺少列 {column}");
    }

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'session_messages'",
    )
    .fetch_all(db.pool())
    .await
    .expect("应读取消息表索引");
    assert!(indexes.iter().any(|index| index == "idx_msgs_session"));

    let message_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('session_messages')")
            .fetch_all(db.pool())
            .await
            .expect("应读取消息表列");
    assert!(
        message_columns
            .iter()
            .any(|column| column == "content_json"),
        "多模态内容序列化列必须存在"
    );

    db.pool().close().await;
    drop(db);
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{suffix}", path.display())));
    }
}

#[tokio::test]
async fn local_model_metadata_and_thinking_flag_survive_a_round_trip() {
    // 这两个字段是本批新增的，漏写进 repo 的 INSERT/SELECT 只会在这里暴露。
    let db = Db::connect_in_memory().await.expect("内存库应可用");
    let mut p = provider();
    p.models[0].supports_thinking = true;
    p.models[0].local = Some(llm_gateway_lib::domain::LocalMeta {
        runtime: "ollama".into(),
        family: Some("gemma4".into()),
        parameter_size: Some("11.9B".into()),
        quantization: Some("Q4_K_M".into()),
        disk_bytes: Some(8_021_618_941),
        capabilities: vec!["vision".into(), "tools".into(), "thinking".into()],
    });
    repo::upsert_provider(db.pool(), &p)
        .await
        .expect("写入应成功");

    let models = repo::list_models_of(db.pool(), "provider-a")
        .await
        .expect("读取应成功");
    assert_eq!(models.len(), 1);
    assert!(models[0].supports_thinking, "思维链能力位必须往返");
    let local = models[0].local.as_ref().expect("本地元数据必须往返");
    assert_eq!(local.runtime, "ollama");
    assert_eq!(local.parameter_size.as_deref(), Some("11.9B"));
    assert_eq!(local.disk_bytes, Some(8_021_618_941));
    assert!(local.capabilities.contains(&"thinking".to_string()));
}

#[tokio::test]
async fn 坏的本地元数据被静默降级而不是让查询失败() {
    // 手工改库能塞进坏 JSON；一次坏模型不该让整条候选链查不出来。
    let db = Db::connect_in_memory().await.expect("内存库应可用");
    repo::upsert_provider(db.pool(), &provider())
        .await
        .expect("写入应成功");
    sqlx::query("UPDATE models SET local_json = '{不是合法 JSON' WHERE provider_id = ?")
        .bind("provider-a")
        .execute(db.pool())
        .await
        .expect("应当能写入坏数据");

    let models = repo::list_models_of(db.pool(), "provider-a")
        .await
        .expect("查询必须成功");
    assert_eq!(models.len(), 1);
    assert!(
        models[0].local.is_none(),
        "坏 JSON 应降级为 None 而不是报错"
    );
}

#[tokio::test]
async fn 应用密钥可写入_覆盖_删除_且不与_meta_表混用() {
    let db = Db::connect_in_memory().await.expect("内存库应可用");
    assert_eq!(
        repo::get_secret(db.pool(), repo::SECRET_SEARCH_API_KEY)
            .await
            .expect("查询应成功"),
        None,
        "初始应没有密钥"
    );

    repo::set_secret(db.pool(), repo::SECRET_SEARCH_API_KEY, "密文-1")
        .await
        .expect("写入应成功");
    assert_eq!(
        repo::get_secret(db.pool(), repo::SECRET_SEARCH_API_KEY)
            .await
            .expect("查询应成功"),
        Some("密文-1".to_string())
    );

    repo::set_secret(db.pool(), repo::SECRET_SEARCH_API_KEY, "密文-2")
        .await
        .expect("覆盖应成功");
    assert_eq!(
        repo::get_secret(db.pool(), repo::SECRET_SEARCH_API_KEY)
            .await
            .expect("查询应成功"),
        Some("密文-2".to_string()),
        "同名重复写入必须是覆盖而不是插入失败"
    );

    // 反向对照：meta 表是明文配置，密钥绝不能落到那里。
    assert_eq!(
        repo::meta_get(db.pool(), repo::SECRET_SEARCH_API_KEY)
            .await
            .expect("查询"),
        None
    );

    assert!(repo::delete_secret(db.pool(), repo::SECRET_SEARCH_API_KEY)
        .await
        .expect("删除应成功"));
    assert!(
        !repo::delete_secret(db.pool(), repo::SECRET_SEARCH_API_KEY)
            .await
            .expect("重复删除不应报错"),
        "第二次删除应当返回 false，让调用方区分"
    );
}

#[tokio::test]
async fn session_listing_reads_every_field_the_management_view_needs() {
    // 真机启动时发现过：命令层自己写 SQL，新增 token_ratio 列后漏改 SELECT，
    // sqlx 在运行时才报 ColumnNotFound，而进程 panic=abort 会直接退出应用。
    // 这里把该查询固定在 repo 层并用真实数据库覆盖，避免同类事故复发。
    let db = Db::connect_in_memory()
        .await
        .expect("内存数据库初始化应成功");
    repo::upsert_provider(db.pool(), &provider())
        .await
        .expect("provider 应写入");
    let session = repo::get_or_create_session(db.pool(), "session-view")
        .await
        .expect("session 应创建");
    repo::append_message(
        db.pool(),
        &session.id,
        "user",
        "列表页需要读的消息",
        None,
        None,
        None,
        4,
        0,
    )
    .await
    .expect("消息应写入");
    repo::update_sticky(db.pool(), &session.id, "provider-a", "model-a", 9_999)
        .await
        .expect("粘性应写入");
    repo::update_session_token_ratio(db.pool(), &session.id, 1.75)
        .await
        .expect("校准比值应写入");

    let rows = repo::list_sessions_with_counts(db.pool(), 10)
        .await
        .expect("会话列表查询必须与 Session 结构体字段保持一致");
    assert_eq!(rows.len(), 1);
    let (session, message_count) = &rows[0];
    assert_eq!(session.id, "session-view");
    assert_eq!(session.sticky_provider_id.as_deref(), Some("provider-a"));
    assert_eq!(session.sticky_model.as_deref(), Some("model-a"));
    assert_eq!(session.sticky_expires_at, Some(9_999));
    assert_eq!(session.token_ratio, Some(1.75));
    assert_eq!(*message_count, 1, "消息数必须来自 JOIN 统计");

    // 无消息的会话也要出现在列表里，且计数为 0（LEFT JOIN 不能把空会话丢掉）。
    repo::get_or_create_session(db.pool(), "session-empty")
        .await
        .expect("空会话应创建");
    let rows = repo::list_sessions_with_counts(db.pool(), 10)
        .await
        .expect("空会话列表应可读");
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|(session, count)| session.id == "session-empty" && *count == 0));

    let all = repo::list_sessions(db.pool(), 10)
        .await
        .expect("repo 的会话列表同样要能读出新增列");
    assert_eq!(all.len(), 2);
    assert_eq!(
        all.iter()
            .find(|item| item.id == "session-view")
            .and_then(|item| item.token_ratio),
        Some(1.75)
    );
}

#[tokio::test]
async fn repository_persists_provider_session_messages_in_ascending_order_and_compacts() {
    let db = Db::connect_in_memory()
        .await
        .expect("内存数据库初始化应成功");
    repo::upsert_provider(db.pool(), &provider())
        .await
        .expect("provider 应写入");
    let providers = repo::list_providers(db.pool())
        .await
        .expect("provider 应读回");
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, "provider-a");
    assert_eq!(providers[0].models[0].alias, "model-a");

    let session = repo::get_or_create_session(db.pool(), "session-a")
        .await
        .expect("session 应创建");
    for (content, prompt_tokens) in [("first", 2), ("second", 3), ("third", 5)] {
        repo::append_message(
            db.pool(),
            &session.id,
            "user",
            content,
            None,
            None,
            None,
            prompt_tokens,
            0,
        )
        .await
        .expect("消息应写入");
    }

    let messages = repo::recent_messages(db.pool(), &session.id, 10)
        .await
        .expect("消息应按时间读取");
    let contents: Vec<&str> = messages
        .iter()
        .map(|message| message.content.as_str())
        .collect();
    assert_eq!(contents, ["first", "second", "third"]);

    repo::apply_compaction(db.pool(), &session.id, messages[1].id, "已压缩摘要")
        .await
        .expect("压缩状态应写入");
    let remaining = repo::recent_messages(db.pool(), &session.id, 10)
        .await
        .expect("未压缩消息应读回");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].content, "third");

    let compacted: i64 = sqlx::query_scalar("SELECT compacted FROM session_messages WHERE id = ?")
        .bind(messages[0].id)
        .fetch_one(db.pool())
        .await
        .expect("应读取压缩标记");
    assert_eq!(compacted, 1);
    let updated = repo::get_or_create_session(db.pool(), &session.id)
        .await
        .expect("session 应读回");
    assert_eq!(updated.summary.as_deref(), Some("已压缩摘要"));
    assert_eq!(updated.compact_count, 1);
    assert_eq!(updated.total_tokens, 10);
}

#[tokio::test]
async fn sticky_update_changes_session_state_and_foreign_keys_are_enforced() {
    let db = Db::connect_in_memory()
        .await
        .expect("内存数据库初始化应成功");
    let session = repo::get_or_create_session(db.pool(), "session-sticky")
        .await
        .expect("session 应创建");
    let before: String = sqlx::query_scalar("SELECT updated_at FROM sessions WHERE id = ?")
        .bind(&session.id)
        .fetch_one(db.pool())
        .await
        .expect("应读取更新时间");

    tokio::time::sleep(Duration::from_millis(2)).await;
    repo::update_sticky(db.pool(), &session.id, "provider-a", "model-a", 1_234_567)
        .await
        .expect("粘性路由应更新");
    let after: String = sqlx::query_scalar("SELECT updated_at FROM sessions WHERE id = ?")
        .bind(&session.id)
        .fetch_one(db.pool())
        .await
        .expect("应读取更新时间");
    let updated = repo::get_or_create_session(db.pool(), &session.id)
        .await
        .expect("session 应读回");
    assert_eq!(updated.sticky_provider_id.as_deref(), Some("provider-a"));
    assert_eq!(updated.sticky_model.as_deref(), Some("model-a"));
    assert_eq!(updated.sticky_expires_at, Some(1_234_567));
    assert_ne!(before, after, "更新粘性路由必须刷新 updated_at");

    let orphan = repo::append_message(
        db.pool(),
        "missing-session",
        "user",
        "不应写入",
        None,
        None,
        None,
        0,
        0,
    )
    .await;
    assert!(orphan.is_err(), "外键约束应拒绝孤儿消息");
}

/* -------------------- 编码丢失的识别 -------------------- */
//
// 真机现象：会话列表里出现 `???????,???`。查库确认存的是 `3F 3F 3F…`（真问号），
// 不是显示问题——**客户端在发过来之前就已把非 ASCII 字符替换成了 `?`**。
//
// 这里只负责**识别与告警**，刻意不自动改写内容：擅自猜回原文比留问号更危险。

#[test]
fn 连续问号_判为编码丢失() {
    assert!(repo::looks_like_encoding_loss("???????,???"));
    assert!(repo::looks_like_encoding_loss("abc???def"));
    // 半角 `?` 才是编码替换后的产物；全角 `？` 是合法中文输入，不该误报。
    assert!(!repo::looks_like_encoding_loss("中文全角问号？？？"));
}

#[test]
fn 单个问号不误报_对照组() {
    // 用户真的在提问，这是绝大多数情况；误报会让日志没法看。
    assert!(!repo::looks_like_encoding_loss("为什么报错?"));
    assert!(!repo::looks_like_encoding_loss("？"));
    assert!(!repo::looks_like_encoding_loss("??"));
    // 正常中文与英文一律不报。
    assert!(!repo::looks_like_encoding_loss("限流是做什么的，一句话"));
    assert!(!repo::looks_like_encoding_loss(
        "2026年最新的 Rust 1.99 有什么新特性"
    ));
    assert!(!repo::looks_like_encoding_loss(""));
}
