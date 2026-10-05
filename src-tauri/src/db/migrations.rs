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
    supports_audio   INTEGER NOT NULL DEFAULT 0,
    supports_video   INTEGER NOT NULL DEFAULT 0,
    supports_stream  INTEGER NOT NULL DEFAULT 1,
    model_type       TEXT NOT NULL DEFAULT 'chat',
    upstream_path    TEXT,
    enabled          INTEGER NOT NULL DEFAULT 1,
    price_json       TEXT,
    overrides_json   TEXT
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
    token_ratio        REAL,
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
    error             TEXT,
    cost              REAL,
    currency          TEXT,
    rate_label        TEXT,
    estimated_prompt_tokens INTEGER,
    attempts_json     TEXT
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
    currency          TEXT NOT NULL DEFAULT '',
    requests          INTEGER NOT NULL DEFAULT 0,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    errors            INTEGER NOT NULL DEFAULT 0,
    cost              REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (day, provider_id, model, currency)
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
    (
        "token_calibration",
        r#"
CREATE TABLE IF NOT EXISTS token_calibration (
    provider_id TEXT NOT NULL,
    model       TEXT NOT NULL,
    samples     INTEGER NOT NULL DEFAULT 0,
    ratio       REAL NOT NULL DEFAULT 1.0,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (provider_id, model)
);
"#,
    ),
    (
        "meta",
        r#"
CREATE TABLE IF NOT EXISTS meta (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
"#,
    ),
    (
        "app_secrets",
        r#"
-- 应用级密钥（当前只有搜索后端 API Key）。值是 AES-256-GCM 密文：
-- config.toml 是明文落盘且会随项目快照传播，密钥绝不能进那里。
CREATE TABLE IF NOT EXISTS app_secrets (
    name       TEXT PRIMARY KEY,
    value_enc  TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
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
    // `CREATE TABLE IF NOT EXISTS` 不会给已存在的用户数据库补列。以下迁移必须可重入：
    // 旧库补列、新库无操作，且不依赖任何一次性的版本标记。
    ensure_column(pool, "session_messages", "tool_call_id", "TEXT").await?;
    ensure_column(pool, "session_messages", "name", "TEXT").await?;
    ensure_column(pool, "session_messages", "content_json", "TEXT").await?;
    ensure_column(pool, "models", "price_json", "TEXT").await?;
    ensure_column(pool, "models", "overrides_json", "TEXT").await?;
    ensure_column(
        pool,
        "models",
        "supports_audio",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(
        pool,
        "models",
        "supports_video",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(pool, "models", "model_type", "TEXT NOT NULL DEFAULT 'chat'").await?;
    ensure_column(pool, "models", "upstream_path", "TEXT").await?;
    ensure_column(pool, "models", "local_json", "TEXT").await?;
    ensure_column(
        pool,
        "models",
        "supports_thinking",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(pool, "requests", "cost", "REAL").await?;
    ensure_column(pool, "requests", "currency", "TEXT").await?;
    ensure_column(pool, "requests", "rate_label", "TEXT").await?;
    ensure_column(pool, "requests", "estimated_prompt_tokens", "INTEGER").await?;
    ensure_column(pool, "requests", "attempts_json", "TEXT").await?;
    ensure_column(pool, "requests", "route_intent", "TEXT").await?;
    ensure_column(pool, "requests", "route_classifier", "TEXT").await?;
    ensure_column(pool, "requests", "route_search", "TEXT").await?;
    ensure_column(pool, "requests", "route_search_hits", "INTEGER").await?;
    ensure_column(pool, "requests", "route_refined", "INTEGER").await?;
    ensure_column(pool, "requests", "route_refine_note", "TEXT").await?;
    // B2 预算闸门：把一次消费归因到某个远程 Key。
    //
    // 历史行一律 NULL，语义是「这次消费不属于任何远程 Key，是本机统一 Key 发的」。
    // **不要按 client 字段猜着回填** —— client 是客户端自报的字符串
    // （`remote-key:<id>` 只是网关自己写的格式），不是 Key 身份；
    // 猜错了会把别人的钱算到这个 Key 头上，而且算错了没有任何报错。
    ensure_column(pool, "requests", "access_key_id", "TEXT").await?;
    // B3 审计存储：改写后的最终提示词。**默认不写** ——
    // 由 `audit.store_refined_prompt` 显式打开，且写入前必须脱敏。
    // 开这个列而不是建新表：它与一次请求一一对应，且审计查询要按行返回。
    ensure_column(pool, "requests", "refined_prompt", "TEXT").await?;
    ensure_column(
        pool,
        "remote_access_keys",
        "monthly_budget_micros",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    // 币种与额度必须成对：多币种的数字加在一起是没有意义的。
    ensure_column(
        pool,
        "remote_access_keys",
        "budget_currency",
        "TEXT NOT NULL DEFAULT ''",
    )
    .await?;
    // JSON 数组字符串。空 = 不限。存 TEXT 而不是关联表：
    // 一个 Key 的模型白名单只有几十条，建表反而要多一次 join 与一套增删改。
    ensure_column(
        pool,
        "remote_access_keys",
        "allowed_models",
        "TEXT NOT NULL DEFAULT ''",
    )
    .await?;
    ensure_column(pool, "sessions", "token_ratio", "REAL").await?;
    migrate_usage_daily_currency(pool).await?;
    Ok(())
}

async fn table_columns(pool: &SqlitePool, table: &str) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            use sqlx::Row;
            row.get::<String, _>("name")
        })
        .collect())
}

async fn ensure_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    definition: &str,
) -> anyhow::Result<()> {
    if table_columns(pool, table)
        .await?
        .iter()
        .any(|c| c == column)
    {
        return Ok(());
    }
    sqlx::query(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {definition}"
    ))
    .execute(pool)
    .await?;
    Ok(())
}

/// `usage_daily` 的旧主键是 (day, provider_id, model)，不含币种。SQLite 不能直接
/// 修改主键，只能重建；旧行没有计价币种与花费，保留 token 计数并写明币种未知。
async fn migrate_usage_daily_currency(pool: &SqlitePool) -> anyhow::Result<()> {
    if table_columns(pool, "usage_daily")
        .await?
        .iter()
        .any(|c| c == "currency")
    {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(
        r#"
ALTER TABLE usage_daily RENAME TO usage_daily_legacy;
CREATE TABLE usage_daily (
    day               TEXT NOT NULL,
    provider_id       TEXT NOT NULL,
    model             TEXT NOT NULL,
    currency          TEXT NOT NULL DEFAULT '',
    requests          INTEGER NOT NULL DEFAULT 0,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    errors            INTEGER NOT NULL DEFAULT 0,
    cost              REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (day, provider_id, model, currency)
);
INSERT INTO usage_daily (day, provider_id, model, currency, requests, prompt_tokens, completion_tokens, errors, cost)
    SELECT day, provider_id, model, '', requests, prompt_tokens, completion_tokens, errors, 0
    FROM usage_daily_legacy;
DROP TABLE usage_daily_legacy;
"#,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
