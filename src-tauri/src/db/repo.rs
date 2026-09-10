//! 数据访问。所有 SQL 集中在此，便于后续替换存储后端。

use chrono::Utc;
use sqlx::{Row, SqlitePool};

use crate::domain::*;
use crate::error::Result;

/* ------------------------------- Providers ------------------------------- */

pub async fn list_providers(pool: &SqlitePool) -> Result<Vec<Provider>> {
    let rows = sqlx::query(
        r#"SELECT id, name, dialect, base_url, api_key_enc, enabled, priority,
                  rpm_limit, intelligence, note, created_at, updated_at
           FROM providers ORDER BY priority ASC, name ASC"#,
    )
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let pid: String = r.get("id");
        let models = list_models_of(pool, &pid).await?;
        out.push(Provider {
            id: pid,
            name: r.get("name"),
            dialect: parse_dialect(&r.get::<String, _>("dialect")),
            base_url: r.get("base_url"),
            api_key_enc: r.get("api_key_enc"),
            enabled: r.get::<i64, _>("enabled") == 1,
            priority: r.get("priority"),
            models,
            rpm_limit: r.get("rpm_limit"),
            intelligence: r.get("intelligence"),
            note: r.get("note"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        });
    }
    Ok(out)
}

fn parse_dialect(s: &str) -> Dialect {
    match s {
        "anthropic" => Dialect::Anthropic,
        "gemini" => Dialect::Gemini,
        "ollama" => Dialect::Ollama,
        _ => Dialect::OpenAI,
    }
}

fn dialect_str(d: Dialect) -> &'static str {
    match d {
        Dialect::OpenAI => "openai",
        Dialect::Anthropic => "anthropic",
        Dialect::Gemini => "gemini",
        Dialect::Ollama => "ollama",
    }
}

pub async fn list_models_of(pool: &SqlitePool, provider_id: &str) -> Result<Vec<ModelRef>> {
    let rows = sqlx::query(
        r#"SELECT alias, upstream, context_window, supports_tools, supports_vision, supports_stream
           FROM models WHERE provider_id = ? AND enabled = 1"#,
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ModelRef {
            alias: r.get("alias"),
            upstream: r.get("upstream"),
            context_window: r.get("context_window"),
            supports_tools: r.get::<i64, _>("supports_tools") == 1,
            supports_vision: r.get::<i64, _>("supports_vision") == 1,
            supports_stream: r.get::<i64, _>("supports_stream") == 1,
        })
        .collect())
}

pub async fn upsert_provider(pool: &SqlitePool, p: &Provider) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO providers
             (id, name, dialect, base_url, api_key_enc, enabled, priority, rpm_limit, intelligence, note, created_at, updated_at)
           VALUES (?,?,?,?,?,?,?,?,?,?,?,?)
           ON CONFLICT(id) DO UPDATE SET
             name=excluded.name, dialect=excluded.dialect, base_url=excluded.base_url,
             api_key_enc=excluded.api_key_enc, enabled=excluded.enabled, priority=excluded.priority,
             rpm_limit=excluded.rpm_limit, intelligence=excluded.intelligence, note=excluded.note,
             updated_at=excluded.updated_at"#,
    )
    .bind(&p.id)
    .bind(&p.name)
    .bind(dialect_str(p.dialect))
    .bind(&p.base_url)
    .bind(&p.api_key_enc)
    .bind(p.enabled as i64)
    .bind(p.priority)
    .bind(p.rpm_limit)
    .bind(p.intelligence)
    .bind(&p.note)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    // 模型列表整体替换，简单且不会留下孤儿
    sqlx::query("DELETE FROM models WHERE provider_id = ?")
        .bind(&p.id)
        .execute(pool)
        .await?;
    for m in &p.models {
        sqlx::query(
            r#"INSERT OR REPLACE INTO models
                 (id, provider_id, alias, upstream, context_window, supports_tools, supports_vision, supports_stream)
               VALUES (?,?,?,?,?,?,?,?)"#,
        )
        .bind(format!("{}:{}", p.id, m.alias))
        .bind(&p.id)
        .bind(&m.alias)
        .bind(&m.upstream)
        .bind(m.context_window)
        .bind(m.supports_tools as i64)
        .bind(m.supports_vision as i64)
        .bind(m.supports_stream as i64)
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn delete_provider(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM providers WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/* -------------------------- Remote access keys -------------------------- */

pub async fn list_remote_access_keys(pool: &SqlitePool) -> Result<Vec<RemoteAccessKey>> {
    let rows = sqlx::query(
        "SELECT id, label, key_hash, enabled, rpm_limit, created_at, updated_at
         FROM remote_access_keys ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|row| RemoteAccessKey {
            id: row.get("id"),
            label: row.get("label"),
            key_hash: row.get("key_hash"),
            enabled: row.get::<i64, _>("enabled") == 1,
            rpm_limit: row.get::<i64, _>("rpm_limit").max(1) as u32,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect())
}

pub async fn create_remote_access_key(pool: &SqlitePool, key: &RemoteAccessKey) -> Result<()> {
    sqlx::query(
        "INSERT INTO remote_access_keys (id, label, key_hash, enabled, rpm_limit, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&key.id)
    .bind(&key.label)
    .bind(&key.key_hash)
    .bind(key.enabled as i64)
    .bind(key.rpm_limit as i64)
    .bind(key.created_at)
    .bind(key.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_remote_access_key(
    pool: &SqlitePool,
    id: &str,
    label: &str,
    enabled: bool,
    rpm_limit: u32,
) -> Result<bool> {
    let changed = sqlx::query(
        "UPDATE remote_access_keys
         SET label = ?, enabled = ?, rpm_limit = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(label)
    .bind(enabled as i64)
    .bind(rpm_limit as i64)
    .bind(Utc::now())
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected()
        == 1;
    Ok(changed)
}

pub async fn delete_remote_access_key(pool: &SqlitePool, id: &str) -> Result<bool> {
    Ok(sqlx::query("DELETE FROM remote_access_keys WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected()
        == 1)
}

pub async fn enabled_remote_access_key_count(pool: &SqlitePool) -> Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM remote_access_keys WHERE enabled = 1")
        .fetch_one(pool)
        .await
        .map_err(Into::into)
}

/// 列出所有已启用模型，聚合成对外 /v1/models 的视图
pub async fn list_public_models(pool: &SqlitePool) -> Result<Vec<PublicModel>> {
    #[derive(sqlx::FromRow)]
    struct Row1 {
        alias: String,
        upstream: String,
        context_window: i32,
        supports_tools: i64,
        supports_vision: i64,
        pname: String,
    }
    let rows = sqlx::query_as::<_, Row1>(
        r#"SELECT m.alias, m.upstream, m.context_window, m.supports_tools, m.supports_vision, p.name AS pname
           FROM models m JOIN providers p ON p.id = m.provider_id
           WHERE p.enabled = 1 AND m.enabled = 1
           ORDER BY m.alias"#,
    )
    .fetch_all(pool)
    .await?;

    let mut map: std::collections::BTreeMap<String, PublicModel> =
        std::collections::BTreeMap::new();
    for r in rows {
        let e = map.entry(r.alias.clone()).or_insert_with(|| PublicModel {
            id: r.alias.clone(),
            object: "model".into(),
            owned_by: r.pname.clone(),
            context_window: r.context_window,
            supports_tools: r.supports_tools == 1,
            supports_vision: r.supports_vision == 1,
            backed_by: 0,
        });
        e.backed_by += 1;
        e.context_window = e.context_window.max(r.context_window);
        e.supports_tools |= r.supports_tools == 1;
        e.supports_vision |= r.supports_vision == 1;
        let _ = &r.upstream;
    }
    Ok(map.into_values().collect())
}

/* -------------------------------- Sessions ------------------------------- */

fn row_to_session(r: &sqlx::sqlite::SqliteRow) -> Session {
    Session {
        id: r.get("id"),
        snapshot_id: r.get("snapshot_id"),
        title: r.get("title"),
        sticky_provider_id: r.get("sticky_provider_id"),
        sticky_model: r.get("sticky_model"),
        sticky_expires_at: r.get("sticky_expires_at"),
        total_tokens: r.get("total_tokens"),
        compact_count: r.get("compact_count"),
        summary: r.get("summary"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

pub async fn get_or_create_session(pool: &SqlitePool, id: &str) -> Result<Session> {
    // 使用 INSERT OR IGNORE 避免同一无显式 ID 的会话并发首请求时出现唯一键竞态。
    let now = Utc::now();
    sqlx::query(
        "INSERT OR IGNORE INTO sessions (id, title, created_at, updated_at) VALUES (?, '', ?, ?)",
    )
    .bind(id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    let row = sqlx::query(
        "SELECT id, snapshot_id, title, sticky_provider_id, sticky_model, sticky_expires_at,
                total_tokens, compact_count, summary, created_at, updated_at
         FROM sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(row_to_session(&row))
}

/// 仅在会话还没有标题时写入首条用户消息生成的标题。
pub async fn set_session_title_if_empty(
    pool: &SqlitePool,
    session_id: &str,
    title: &str,
) -> Result<Option<chrono::DateTime<Utc>>> {
    let now = Utc::now();
    let result = sqlx::query(
        "UPDATE sessions SET title = ?, updated_at = ?
         WHERE id = ? AND (title IS NULL OR title = '')",
    )
    .bind(title)
    .bind(now)
    .bind(session_id)
    .execute(pool)
    .await?;

    Ok((result.rows_affected() == 1).then_some(now))
}

/// 取最近 N 条未被压缩的消息（按 id 升序），用于拼装上游请求
pub async fn recent_messages(
    pool: &SqlitePool,
    session_id: &str,
    limit: i64,
) -> Result<Vec<SessionMessage>> {
    let rows = sqlx::query(
        r#"SELECT * FROM (
                SELECT id, session_id, role, content, tool_calls, tool_call_id, name,
                       routed_provider, routed_model,
                       compacted, prompt_tokens, completion_tokens, created_at
                FROM session_messages
                WHERE session_id = ? AND compacted = 0
                ORDER BY id DESC LIMIT ?
             ) ORDER BY id ASC"#,
    )
    .bind(session_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(row_to_msg).collect())
}

/// 读取最近消息并优先还原完整的多模态内容。
///
/// 旧数据库在列级迁移后，历史行的 `content_json` 为 NULL；损坏的序列化内容
/// 也不能阻断会话恢复，因此两种情况都退回既有文本投影。
pub async fn recent_messages_with_content(
    pool: &SqlitePool,
    session_id: &str,
    limit: i64,
) -> Result<Vec<RehydratedSessionMessage>> {
    recent_messages_with_content_by_compaction(pool, session_id, limit, true).await
}

/// 读取最近 N 条消息（包括已压缩记录），只用于识别客户端重放历史中的真实新增
/// 尾部。压缩后的消息不会进入上游上下文，但仍需要参与去重，避免重复落库。
pub async fn recent_all_messages_with_content(
    pool: &SqlitePool,
    session_id: &str,
    limit: i64,
) -> Result<Vec<RehydratedSessionMessage>> {
    recent_messages_with_content_by_compaction(pool, session_id, limit, false).await
}

async fn recent_messages_with_content_by_compaction(
    pool: &SqlitePool,
    session_id: &str,
    limit: i64,
    only_uncompacted: bool,
) -> Result<Vec<RehydratedSessionMessage>> {
    let rows = sqlx::query(
        r#"SELECT * FROM (
                SELECT id, session_id, role, content, content_json, tool_calls, tool_call_id, name,
                       routed_provider, routed_model,
                       compacted, prompt_tokens, completion_tokens, created_at
                FROM session_messages
                WHERE session_id = ? AND (? = 0 OR compacted = 0)
                ORDER BY id DESC LIMIT ?
             ) ORDER BY id ASC"#,
    )
    .bind(session_id)
    .bind(only_uncompacted as i64)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(row_to_rehydrated_msg).collect())
}

fn row_to_msg(r: &sqlx::sqlite::SqliteRow) -> SessionMessage {
    SessionMessage {
        id: r.get("id"),
        session_id: r.get("session_id"),
        role: r.get("role"),
        content: r.get("content"),
        tool_calls: r.get("tool_calls"),
        tool_call_id: r.get("tool_call_id"),
        name: r.get("name"),
        routed_provider: r.get("routed_provider"),
        routed_model: r.get("routed_model"),
        compacted: r.get::<i64, _>("compacted") == 1,
        prompt_tokens: r.get("prompt_tokens"),
        completion_tokens: r.get("completion_tokens"),
        created_at: r.get("created_at"),
    }
}

fn row_to_rehydrated_msg(r: &sqlx::sqlite::SqliteRow) -> RehydratedSessionMessage {
    let message = row_to_msg(r);
    let serialized_content: Option<String> = r.get("content_json");
    let content = serialized_content
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_else(|| Content::Text(message.content.clone()));
    RehydratedSessionMessage { message, content }
}

#[allow(clippy::too_many_arguments)]
pub async fn append_message(
    pool: &SqlitePool,
    session_id: &str,
    role: &str,
    content: &str,
    tool_calls: Option<&str>,
    routed_provider: Option<&str>,
    routed_model: Option<&str>,
    prompt_tokens: i64,
    completion_tokens: i64,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO session_messages
             (session_id, role, content, tool_calls, tool_call_id, name, routed_provider,
              routed_model, compacted, prompt_tokens, completion_tokens, created_at)
           VALUES (?,?,?,?,?,?,?,?,0,?,?,?)"#,
    )
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(tool_calls)
    .bind(None::<&str>)
    .bind(None::<&str>)
    .bind(routed_provider)
    .bind(routed_model)
    .bind(prompt_tokens)
    .bind(completion_tokens)
    .bind(Utc::now())
    .execute(pool)
    .await?;

    sqlx::query("UPDATE sessions SET updated_at = ?, total_tokens = total_tokens + ? WHERE id = ?")
        .bind(Utc::now())
        .bind(prompt_tokens + completion_tokens)
        .bind(session_id)
        .execute(pool)
        .await?;

    // 标题用首条 user 消息自动生成，方便 UI 列出会话
    if role == "user" {
        let _ = set_session_title_if_empty(
            pool,
            session_id,
            &content.chars().take(40).collect::<String>(),
        )
        .await;
    }
    Ok(())
}

/// 原子写入一轮对话（用户消息 + 助手响应）。
///
/// 流式响应的所有 chunk 先在内存聚合，结束后仅调用本函数一次；事务保证进程
/// 在两条消息之间退出时，不会留下无法重建上下文的半轮记录。
#[allow(clippy::too_many_arguments)]
pub async fn append_turn(
    pool: &SqlitePool,
    session_id: &str,
    user_content: &str,
    assistant_content: &str,
    assistant_tool_calls: Option<&str>,
    routed_provider: Option<&str>,
    routed_model: Option<&str>,
    prompt_tokens: i64,
    completion_tokens: i64,
) -> Result<()> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;
    let messages = [
        ("user", user_content, None, None, None, prompt_tokens, 0),
        (
            "assistant",
            assistant_content,
            assistant_tool_calls,
            routed_provider,
            routed_model,
            0,
            completion_tokens,
        ),
    ];

    for (role, content, tool_calls, provider, model, prompt, completion) in messages {
        sqlx::query(
            r#"INSERT INTO session_messages
                 (session_id, role, content, tool_calls, routed_provider, routed_model, compacted,
                  prompt_tokens, completion_tokens, created_at)
               VALUES (?,?,?,?,?,?,0,?,?,?)"#,
        )
        .bind(session_id)
        .bind(role)
        .bind(content)
        .bind(tool_calls)
        .bind(provider)
        .bind(model)
        .bind(prompt)
        .bind(completion)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query("UPDATE sessions SET updated_at = ?, total_tokens = total_tokens + ? WHERE id = ?")
        .bind(now)
        .bind(prompt_tokens + completion_tokens)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;

    let title: String = user_content.trim().chars().take(40).collect();
    if !title.is_empty() {
        sqlx::query("UPDATE sessions SET title = ? WHERE id = ? AND (title IS NULL OR title = '')")
            .bind(title)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// 原子保存一整次网关交互。
///
/// 不能把一次输入假定为“最后一条 user 消息”：工具续轮的输入通常是
/// assistant(tool_calls) + tool(tool_call_id)，两者都必须完整落库，才能在
/// 进程重启后继续组成合法的上游请求。`assistant` 是本次上游实际生成的响应。
#[allow(clippy::too_many_arguments)]
pub async fn append_exchange(
    pool: &SqlitePool,
    session_id: &str,
    incoming: &[Message],
    assistant: &Message,
    routed_provider: Option<&str>,
    routed_model: Option<&str>,
    prompt_tokens: i64,
    completion_tokens: i64,
) -> Result<()> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;

    for (index, message) in incoming.iter().enumerate() {
        insert_message_in_transaction(
            &mut tx,
            MessageInsert {
                session_id,
                message,
                routed_provider: None,
                routed_model: None,
                prompt_tokens: if index == 0 { prompt_tokens } else { 0 },
                completion_tokens: 0,
                created_at: now,
            },
        )
        .await?;
    }
    insert_message_in_transaction(
        &mut tx,
        MessageInsert {
            session_id,
            message: assistant,
            routed_provider,
            routed_model,
            prompt_tokens: 0,
            completion_tokens,
            created_at: now,
        },
    )
    .await?;

    sqlx::query("UPDATE sessions SET updated_at = ?, total_tokens = total_tokens + ? WHERE id = ?")
        .bind(now)
        .bind(prompt_tokens + completion_tokens)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;

    if let Some(title_source) = incoming
        .iter()
        .find(|message| message.role == Role::User)
        .map(Message::content_text)
    {
        let title: String = title_source.trim().chars().take(40).collect();
        if !title.is_empty() {
            sqlx::query(
                "UPDATE sessions SET title = ? WHERE id = ? AND (title IS NULL OR title = '')",
            )
            .bind(title)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await?;
    Ok(())
}

struct MessageInsert<'a> {
    session_id: &'a str,
    message: &'a Message,
    routed_provider: Option<&'a str>,
    routed_model: Option<&'a str>,
    prompt_tokens: i64,
    completion_tokens: i64,
    created_at: chrono::DateTime<Utc>,
}

async fn insert_message_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    insert: MessageInsert<'_>,
) -> Result<()> {
    let MessageInsert {
        session_id,
        message,
        routed_provider,
        routed_model,
        prompt_tokens,
        completion_tokens,
        created_at,
    } = insert;
    let tool_calls = message
        .tool_calls
        .as_ref()
        .map(|calls| {
            serde_json::to_string(calls)
                .map_err(|error| crate::error::GatewayError::Protocol(error.to_string()))
        })
        .transpose()?;
    let content_json = serde_json::to_string(&message.content)
        .map_err(|error| crate::error::GatewayError::Protocol(error.to_string()))?;
    sqlx::query(
        r#"INSERT INTO session_messages
             (session_id, role, content, content_json, tool_calls, tool_call_id, name, routed_provider,
              routed_model, compacted, prompt_tokens, completion_tokens, created_at)
           VALUES (?,?,?,?,?,?,?,?,?,0,?,?,?)"#,
    )
    .bind(session_id)
    .bind(role_str(message.role))
    .bind(message.content_text())
    .bind(content_json)
    .bind(tool_calls.as_deref())
    .bind(message.tool_call_id.as_deref())
    .bind(message.name.as_deref())
    .bind(routed_provider)
    .bind(routed_model)
    .bind(prompt_tokens)
    .bind(completion_tokens)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

pub async fn update_sticky(
    pool: &SqlitePool,
    session_id: &str,
    provider_id: &str,
    model: &str,
    expires_at: i64,
) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        "UPDATE sessions SET sticky_provider_id = ?, sticky_model = ?, sticky_expires_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(provider_id)
    .bind(model)
    .bind(expires_at)
    .bind(now)
    .bind(session_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn apply_compaction(
    pool: &SqlitePool,
    session_id: &str,
    upto_msg_id: i64,
    summary: &str,
) -> Result<()> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE session_messages SET compacted = 1 WHERE session_id = ? AND id <= ?")
        .bind(session_id)
        .bind(upto_msg_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE sessions SET summary = ?, compact_count = compact_count + 1, updated_at = ? WHERE id = ?",
    )
    .bind(summary)
    .bind(now)
    .bind(session_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn list_sessions(pool: &SqlitePool, limit: i64) -> Result<Vec<Session>> {
    let rows = sqlx::query(
        "SELECT id, snapshot_id, title, sticky_provider_id, sticky_model, sticky_expires_at,
                total_tokens, compact_count, summary, created_at, updated_at
         FROM sessions ORDER BY updated_at DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Session {
            id: r.get("id"),
            snapshot_id: r.get("snapshot_id"),
            title: r.get("title"),
            sticky_provider_id: r.get("sticky_provider_id"),
            sticky_model: r.get("sticky_model"),
            sticky_expires_at: r.get("sticky_expires_at"),
            total_tokens: r.get("total_tokens"),
            compact_count: r.get("compact_count"),
            summary: r.get("summary"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        })
        .collect())
}

pub async fn delete_session(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/* ---------------------------- Requests / Stats ---------------------------- */

pub struct RequestLog<'a> {
    pub session_id: Option<&'a str>,
    pub client: Option<&'a str>,
    pub requested_model: &'a str,
    pub routed_provider: Option<&'a str>,
    pub routed_model: Option<&'a str>,
    pub status: Option<i64>,
    pub latency_ms: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub fallback_attempts: i64,
    pub error: Option<&'a str>,
}

pub async fn log_request(pool: &SqlitePool, log: RequestLog<'_>) -> Result<()> {
    let ts = Utc::now().timestamp();
    sqlx::query(
        r#"INSERT INTO requests
             (ts, session_id, client, requested_model, routed_provider, routed_model, status,
              latency_ms, prompt_tokens, completion_tokens, fallback_attempts, error)
           VALUES (?,?,?,?,?,?,?,?,?,?,?,?)"#,
    )
    .bind(ts)
    .bind(log.session_id)
    .bind(log.client)
    .bind(log.requested_model)
    .bind(log.routed_provider)
    .bind(log.routed_model)
    .bind(log.status)
    .bind(log.latency_ms)
    .bind(log.prompt_tokens)
    .bind(log.completion_tokens)
    .bind(log.fallback_attempts)
    .bind(log.error)
    .execute(pool)
    .await?;

    // 日粒度聚合，供用量看板直接查，不用每次扫全表
    let day = Utc::now().format("%Y-%m-%d").to_string();
    let pid = log.routed_provider.unwrap_or("unknown");
    let mid = log.routed_model.unwrap_or("unknown");
    sqlx::query(
        r#"INSERT INTO usage_daily (day, provider_id, model, requests, prompt_tokens, completion_tokens, errors)
           VALUES (?,?,?,1,?,?,?)
           ON CONFLICT(day, provider_id, model) DO UPDATE SET
             requests = requests + 1,
             prompt_tokens = prompt_tokens + excluded.prompt_tokens,
             completion_tokens = completion_tokens + excluded.completion_tokens,
             errors = errors + excluded.errors"#,
    )
    .bind(day)
    .bind(pid)
    .bind(mid)
    .bind(log.prompt_tokens)
    .bind(log.completion_tokens)
    .bind(if log.status.unwrap_or(200) >= 400 { 1 } else { 0 })
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn stats_overview(pool: &SqlitePool) -> Result<serde_json::Value> {
    let since = Utc::now().timestamp() - 24 * 3600;
    let r = sqlx::query(
        r#"SELECT COUNT(*) AS total,
                  SUM(CASE WHEN status >= 200 AND status < 400 THEN 1 ELSE 0 END) AS ok,
                  AVG(latency_ms) AS avg_latency,
                  SUM(prompt_tokens) AS pt,
                  SUM(completion_tokens) AS ct,
                  SUM(fallback_attempts) AS fb
           FROM requests WHERE ts >= ?"#,
    )
    .bind(since)
    .fetch_one(pool)
    .await?;

    Ok(serde_json::json!({
        "window": "24h",
        "total": r.get::<i64, _>("total"),
        "success": r.get::<Option<i64>, _>("ok").unwrap_or(0),
        "avg_latency_ms": r.get::<Option<f64>, _>("avg_latency").unwrap_or(0.0).round() as i64,
        "prompt_tokens": r.get::<Option<i64>, _>("pt").unwrap_or(0),
        "completion_tokens": r.get::<Option<i64>, _>("ct").unwrap_or(0),
        "fallback_attempts": r.get::<Option<i64>, _>("fb").unwrap_or(0),
    }))
}

pub async fn recent_requests(pool: &SqlitePool, limit: i64) -> Result<Vec<serde_json::Value>> {
    let rows = sqlx::query(
        r#"SELECT ts, client, requested_model, routed_provider, routed_model, status, latency_ms,
                  prompt_tokens, completion_tokens, fallback_attempts, error
           FROM requests ORDER BY id DESC LIMIT ?"#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "ts": r.get::<i64, _>("ts"),
                "client": r.get::<Option<String>, _>("client"),
                "requested_model": r.get::<String, _>("requested_model"),
                "routed_provider": r.get::<Option<String>, _>("routed_provider"),
                "routed_model": r.get::<Option<String>, _>("routed_model"),
                "status": r.get::<Option<i64>, _>("status"),
                "latency_ms": r.get::<Option<i64>, _>("latency_ms"),
                "prompt_tokens": r.get::<i64, _>("prompt_tokens"),
                "completion_tokens": r.get::<i64, _>("completion_tokens"),
                "fallback_attempts": r.get::<i64, _>("fallback_attempts"),
                "error": r.get::<Option<String>, _>("error"),
            })
        })
        .collect())
}

pub async fn purge_old_requests(pool: &SqlitePool, retention_days: i64) -> Result<()> {
    let cutoff = Utc::now().timestamp() - retention_days * 86400;
    sqlx::query("DELETE FROM requests WHERE ts < ?")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(())
}

/* -------------------------------- Snapshots ------------------------------- */

pub async fn list_snapshots(pool: &SqlitePool) -> Result<Vec<Snapshot>> {
    let rows =
        sqlx::query("SELECT id, name, payload, created_at FROM snapshots ORDER BY created_at DESC")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .iter()
        .map(|r| Snapshot {
            id: r.get("id"),
            name: r.get("name"),
            payload: serde_json::from_str(r.get::<String, _>("payload").as_str())
                .unwrap_or(serde_json::Value::Null),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn create_snapshot(
    pool: &SqlitePool,
    name: &str,
    payload: &serde_json::Value,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO snapshots (id, name, payload, created_at) VALUES (?,?,?,?)")
        .bind(&id)
        .bind(name)
        .bind(payload.to_string())
        .bind(Utc::now())
        .execute(pool)
        .await?;
    Ok(id)
}

pub async fn get_snapshot(pool: &SqlitePool, id: &str) -> Result<Option<Snapshot>> {
    let r = sqlx::query("SELECT id, name, payload, created_at FROM snapshots WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(r.map(|r| Snapshot {
        id: r.get("id"),
        name: r.get("name"),
        payload: serde_json::from_str(r.get::<String, _>("payload").as_str())
            .unwrap_or(serde_json::Value::Null),
        created_at: r.get("created_at"),
    }))
}
