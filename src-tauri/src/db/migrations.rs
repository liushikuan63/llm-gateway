//! 建表语句。版本号递增即可平滑升级，不做破坏性变更。

use sqlx::SqlitePool;

pub const SCHEMA: &[(&str, &str)] = &[
    (
        "providers",
        r#"
CREATE TABLE IF NOT EXISTS providers (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    dialect       TEXT NOT NULL DEFAULT 'openai',
    base_url      TEXT NOT NULL,
    api_key_enc   TEXT NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1,
    priority      INTEGER NOT NULL DEFAULT 100,
    rpm_limit     INTEGER NOT NULL DEFAULT 0,
    intelligence  INTEGER NOT NULL DEFAULT 50,
    note          TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_providers_enabled ON providers(enabled, priority);
"#,
    ),
    (
        "models",
        r#"
CREATE TABLE IF NOT EXISTS models (
    id               TEXT PRIMARY KEY,
    provider_id      TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    alias            TEXT NOT NULL,
    upstream         TEXT NOT NULL,
    context_window   INTEGER NOT NULL DEFAULT 8192,
    supports_tools   INTEGER NOT NULL DEFAULT 0,
    supports_vision  INTEGER NOT NULL DEFAULT 0,
    supports_stream  INTEGER NOT NULL DEFAULT 1,
    enabled          INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_models_alias ON models(alias);
"#,
    ),
    (
        "sessions",
        r#"
CREATE TABLE IF NOT EXISTS sessions (
    id                 TEXT PRIMARY KEY,
    snapshot_id        TEXT,
    title              TEXT NOT NULL DEFAULT '',
    sticky_provider_id TEXT,
    sticky_model       TEXT,
    sticky_expires_at  INTEGER,
    total_tokens       INTEGER NOT NULL DEFAULT 0,
    compact_count      INTEGER NOT NULL DEFAULT 0,
    summary            TEXT,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);
"#,
    ),
    (
        "session_messages",
        r#"
CREATE TABLE IF NOT EXISTS session_messages (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role              TEXT NOT NULL,
    content           TEXT NOT NULL,
    content_json      TEXT,
    tool_calls        TEXT,
    tool_call_id      TEXT,
    name              TEXT,
    routed_provider   TEXT,
    routed_model      TEXT,
    compacted         INTEGER NOT NULL DEFAULT 0,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_msgs_session ON session_messages(session_id, id);
"#,
    ),
    (
        "requests",
        r#"
CREATE TABLE IF NOT EXISTS requests (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                INTEGER NOT NULL,
    session_id        TEXT,
    client            TEXT,
    requested_model   TEXT NOT NULL,
    routed_provider   TEXT,
    routed_model      TEXT,
    status            INTEGER,
    latency_ms        INTEGER,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    fallback_attempts INTEGER NOT NULL DEFAULT 0,
    error             TEXT
);
CREATE INDEX IF NOT EXISTS idx_req_ts ON requests(ts DESC);
"#,
    ),
    (
        "usage_daily",
        r#"
CREATE TABLE IF NOT EXISTS usage_daily (
    day               TEXT NOT NULL,
    provider_id       TEXT NOT NULL,
    model             TEXT NOT NULL,
    requests          INTEGER NOT NULL DEFAULT 0,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    errors            INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, provider_id, model)
);
"#,
    ),
    (
        "snapshots",
        r#"
CREATE TABLE IF NOT EXISTS snapshots (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    payload     TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
"#,
    ),
    (
        "remote_access_keys",
        r#"
CREATE TABLE IF NOT EXISTS remote_access_keys (
    id          TEXT PRIMARY KEY,
    label       TEXT NOT NULL,
    key_hash    TEXT NOT NULL UNIQUE,
    enabled     INTEGER NOT NULL DEFAULT 1,
    rpm_limit   INTEGER NOT NULL DEFAULT 60,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_remote_access_keys_enabled ON remote_access_keys(enabled);
"#,
    ),
];

pub async fn run(pool: &SqlitePool) -> anyhow::Result<()> {
    for (name, sql) in SCHEMA {
        sqlx::raw_sql(sql)
            .execute(pool)
            .await
            .map_err(|e| anyhow::anyhow!("migration `{name}` failed: {e}"))?;
    }
    // `CREATE TABLE IF NOT EXISTS` 不会给已存在的用户数据库补列。工具续轮
    // 需要保存 tool_call_id/name，因此这里采用可重入的列级迁移。
    ensure_session_message_column(pool, "tool_call_id", "TEXT").await?;
    ensure_session_message_column(pool, "name", "TEXT").await?;
    ensure_session_message_column(pool, "content_json", "TEXT").await?;
    Ok(())
}

async fn ensure_session_message_column(
    pool: &SqlitePool,
    column: &str,
    definition: &str,
) -> anyhow::Result<()> {
    let columns = sqlx::query("PRAGMA table_info(session_messages)")
        .fetch_all(pool)
        .await?;
    let exists = columns.iter().any(|row| {
        use sqlx::Row;
        row.get::<String, _>("name") == column
    });
    if !exists {
        sqlx::query(&format!(
            "ALTER TABLE session_messages ADD COLUMN {column} {definition}"
        ))
        .execute(pool)
        .await?;
    }
    Ok(())
}
