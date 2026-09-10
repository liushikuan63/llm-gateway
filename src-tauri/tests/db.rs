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
            alias: "model-a".to_owned(),
            upstream: "upstream-a".to_owned(),
            context_window: 32_768,
            supports_tools: true,
            supports_vision: false,
            supports_stream: true,
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
    ] {
        assert!(
            tables.iter().any(|table| table == expected),
            "缺少表 {expected}"
        );
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
