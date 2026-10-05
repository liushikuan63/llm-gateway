//! 数据访问。所有 SQL 集中在此，便于后续替换存储后端。

use chrono::Utc;
use serde::Serialize;
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
        Dialect::Responses => "responses",
    }
}

pub async fn list_models_of(pool: &SqlitePool, provider_id: &str) -> Result<Vec<ModelRef>> {
    let rows = sqlx::query(
        r#"SELECT alias, upstream, context_window, supports_tools, supports_vision,
                  supports_audio, supports_video, supports_thinking, supports_stream, model_type,
                  upstream_path, price_json, overrides_json, local_json, enabled
           FROM models WHERE provider_id = ?"#,
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
            supports_audio: r.get::<i64, _>("supports_audio") == 1,
            supports_video: r.get::<i64, _>("supports_video") == 1,
            supports_thinking: r.get::<i64, _>("supports_thinking") == 1,
            supports_stream: r.get::<i64, _>("supports_stream") == 1,
            model_type: ModelType::parse(&r.get::<String, _>("model_type")).unwrap_or_default(),
            upstream_path: r.get::<Option<String>, _>("upstream_path"),
            price: read_model_price(&r),
            overrides: read_model_overrides(&r),
            local: read_local_meta(&r),
            enabled: r.get::<i64, _>("enabled") == 1,
        })
        .collect())
}

/// 价格整包存 JSON：档位与时段规则是可变结构，拆成定宽列会让两处定义漂移。
/// 解析失败或数值非法时返回 `None`，绝不让坏数据参与计价。
fn read_model_price(row: &sqlx::sqlite::SqliteRow) -> Option<ModelPrice> {
    let raw = row.get::<Option<String>, _>("price_json")?;
    let price: ModelPrice = serde_json::from_str(&raw).ok()?;
    price.is_valid().then_some(price)
}

/// 覆盖配置解析失败时返回 `None`，绝不让坏 JSON 影响请求路由；保存期已经拒绝过
/// 非法值，能到这里的大多是手工改库。
fn read_model_overrides(row: &sqlx::sqlite::SqliteRow) -> Option<ModelOverrides> {
    let raw = row.get::<Option<String>, _>("overrides_json")?;
    let parsed: ModelOverrides = serde_json::from_str(&raw).ok()?;
    (!parsed.is_empty()).then_some(parsed)
}

/// 本地元数据只用于界面展示与排查，坏 JSON 不该挡住一次正常路由，
/// 因此解析失败一律降级为 `None` 而不是报错。
fn read_local_meta(row: &sqlx::sqlite::SqliteRow) -> Option<LocalMeta> {
    let raw = row.get::<Option<String>, _>("local_json")?;
    let parsed: LocalMeta = serde_json::from_str(&raw).ok()?;
    (!parsed.runtime.trim().is_empty()).then_some(parsed)
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
        let price = m
            .price
            .as_ref()
            .filter(|price| price.is_valid())
            .and_then(|price| serde_json::to_string(price).ok());
        let overrides = m
            .overrides
            .as_ref()
            .filter(|overrides| !overrides.is_empty())
            .and_then(|overrides| serde_json::to_string(overrides).ok());
        let local = m
            .local
            .as_ref()
            .filter(|local| !local.runtime.trim().is_empty())
            .and_then(|local| serde_json::to_string(local).ok());
        sqlx::query(
            r#"INSERT OR REPLACE INTO models
                 (id, provider_id, alias, upstream, context_window, supports_tools, supports_vision,
                  supports_audio, supports_video, supports_thinking, supports_stream, model_type,
                   upstream_path, price_json, overrides_json, local_json, enabled)
                VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(format!("{}:{}", p.id, m.alias))
        .bind(&p.id)
        .bind(&m.alias)
        .bind(&m.upstream)
        .bind(m.context_window)
        .bind(m.supports_tools as i64)
        .bind(m.supports_vision as i64)
        .bind(m.supports_audio as i64)
        .bind(m.supports_video as i64)
        .bind(m.supports_thinking as i64)
        .bind(m.supports_stream as i64)
        .bind(m.model_type.code())
        .bind(&m.upstream_path)
        .bind(price)
        .bind(overrides)
        .bind(local)
        .bind(m.enabled as i64)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// 仅更新一个模型的价格 JSON，供「自动获取最新定价」使用。返回是否命中了记录。
pub async fn update_model_price(
    pool: &SqlitePool,
    provider_id: &str,
    alias: &str,
    price: &ModelPrice,
) -> Result<bool> {
    let encoded =
        serde_json::to_string(price).map_err(|e| crate::error::GatewayError::Other(e.into()))?;
    let result =
        sqlx::query("UPDATE models SET price_json = ? WHERE provider_id = ? AND alias = ?")
            .bind(encoded)
            .bind(provider_id)
            .bind(alias)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
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
        model_type: String,
        pname: String,
    }
    let rows = sqlx::query_as::<_, Row1>(
        r#"SELECT m.alias, m.upstream, m.context_window, m.supports_tools, m.supports_vision,
                  m.model_type, p.name AS pname
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
            model_type: ModelType::parse(&r.model_type).unwrap_or_default(),
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
        token_ratio: r.get("token_ratio"),
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
                total_tokens, compact_count, token_ratio, summary, created_at, updated_at
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
        if looks_like_encoding_loss(&title) {
            tracing::warn!(
                session = %session_id,
                "会话标题疑似编码丢失（连续问号）：{title:?}"
            );
        }
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
            // 标题若是「连续问号」，说明**客户端在发过来之前就已把非 ASCII 字符
            // 替换成了 `?`**（GBK/Latin-1 客户端常见）。库里存下去就永久是问号，
            // 界面上只看到 `???????,???`，完全无从判断是编码问题还是用户真输入的问号。
            //
            // 刻意**不改写标题**：擅自猜回原文比留问号更危险。
            // 只记一条警告，让它可被检索到。
            if looks_like_encoding_loss(&title) {
                tracing::warn!(
                    session = %session_id,
                    "会话标题疑似编码丢失（连续问号）：{:?}",
                    title
                );
            }
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

/// 文本是否疑似**编码丢失**（非 ASCII 字符在到达网关前已被替换成 `?`）。
///
/// 判据是**连续 3 个以上**的 `?`：
/// - 单个问号极可能是用户真的在提问（"为什么报错?"），不能误报；
/// - `???????,???` 这种成片出现，只可能来自编码转换（GBK/Latin-1 客户端、
///   或某些 HTTP 库在序列化时用了 ASCII 编码）。
///
/// 只用于**告警与诊断**，绝不用来改写内容：擅自猜回原文比留问号更危险。
pub fn looks_like_encoding_loss(text: &str) -> bool {
    let mut run = 0usize;
    for ch in text.chars() {
        if ch == '?' {
            run += 1;
            if run >= 3 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
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
                total_tokens, compact_count, token_ratio, summary, created_at, updated_at
         FROM sessions ORDER BY updated_at DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(row_to_session).collect())
}

/// 会话列表 + 每条会话的消息数（管理页用）。查询集中在这里而不是命令层，
/// 是为了让它落进真实数据库的集成测试覆盖范围：本进程 `panic = "abort"`，
/// 一旦 SELECT 与结构体的字段不同步，应用会直接退出而不是返回错误。
pub async fn list_sessions_with_counts(
    pool: &SqlitePool,
    limit: i64,
) -> Result<Vec<(Session, i64)>> {
    let rows = sqlx::query(
        r#"SELECT s.id, s.snapshot_id, s.title, s.sticky_provider_id, s.sticky_model,
                  s.sticky_expires_at, s.total_tokens, s.compact_count, s.token_ratio, s.summary,
                  s.created_at, s.updated_at, COUNT(m.id) AS message_count
           FROM sessions s
           LEFT JOIN session_messages m ON m.session_id = s.id
           GROUP BY s.id
           ORDER BY s.updated_at DESC
           LIMIT ?"#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| (row_to_session(row), row.get("message_count")))
        .collect())
}

/// 记录该会话最近一次「实际 / 估算」token 比值，供压缩阈值按真实口径换算。
pub async fn update_session_token_ratio(
    pool: &SqlitePool,
    session_id: &str,
    ratio: f64,
) -> Result<()> {
    sqlx::query("UPDATE sessions SET token_ratio = ? WHERE id = ?")
        .bind(ratio)
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
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
    /// 按模型价格估算的花费；未配置价格时为 `None`。
    pub cost: Option<f64>,
    pub currency: Option<&'a str>,
    /// 生效的计价档位说明（时段价 / 输入长度分档）；全是默认档时为 `None`。
    pub rate_label: Option<&'a str>,
    /// 本地估算的输入 token；用于与上游实际用量对照做校准。
    pub estimated_prompt_tokens: Option<i64>,
    /// 逐跳降级明细（JSON 数组）。没有尝试明细时为 `None`。
    pub attempts_json: Option<&'a str>,
    /// 智能模式与联网搜索的可观测信息。未启用时整块为默认值。
    pub route: RouteTrace,
}

/// 智能模式 + 联网搜索在审计里的落点。
///
/// 合成一个子结构而不是往 `RequestLog` 上再摊四个字段：审计写入点有八九处，
/// 每处多四行既难读也容易漏。默认全空代表「这两项功能没开」。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouteTrace {
    /// simple / vision / reasoning
    pub intent: Option<String>,
    /// rule / jev / heuristic
    pub classifier: Option<String>,
    /// 实际生效的搜索后端；`failed` 表示所有后端都不可用。
    pub search: Option<String>,
    /// 注入上下文的检索结果条数。
    pub search_hits: Option<i64>,
    /// 提示词是否被改写过。`Some(true)` 才表示真的替换了提示词；
    /// `Some(false)` 表示触发了但用原文（失败、过短、超限等）。
    pub refined: Option<bool>,
    /// 改写前后的字符数，写成 `原文→新文` 的形式便于人工核对改写幅度。
    pub refine_note: Option<String>,
}

pub async fn log_request(pool: &SqlitePool, log: RequestLog<'_>) -> Result<()> {
    let ts = Utc::now().timestamp();
    sqlx::query(
        r#"INSERT INTO requests
             (ts, session_id, client, requested_model, routed_provider, routed_model, status,
              latency_ms, prompt_tokens, completion_tokens, fallback_attempts, error,
              cost, currency, rate_label, estimated_prompt_tokens, attempts_json,
              route_intent, route_classifier, route_search, route_search_hits,
              route_refined, route_refine_note)
           VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
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
    .bind(log.cost)
    .bind(log.currency)
    .bind(log.rate_label)
    .bind(log.estimated_prompt_tokens)
    .bind(log.attempts_json)
    .bind(log.route.intent.as_deref())
    .bind(log.route.classifier.as_deref())
    .bind(log.route.search.as_deref())
    .bind(log.route.search_hits)
    .bind(log.route.refined)
    .bind(log.route.refine_note.as_deref())
    .execute(pool)
    .await?;

    // 日粒度聚合，供用量看板直接查，不用每次扫全表
    let day = Utc::now().format("%Y-%m-%d").to_string();
    let pid = log.routed_provider.unwrap_or("unknown");
    let mid = log.routed_model.unwrap_or("unknown");
    let currency = log.currency.unwrap_or("");
    sqlx::query(
        r#"INSERT INTO usage_daily (day, provider_id, model, currency, requests, prompt_tokens, completion_tokens, errors, cost)
           VALUES (?,?,?,?,1,?,?,?,?)
           ON CONFLICT(day, provider_id, model, currency) DO UPDATE SET
             requests = requests + 1,
             prompt_tokens = prompt_tokens + excluded.prompt_tokens,
             completion_tokens = completion_tokens + excluded.completion_tokens,
             errors = errors + excluded.errors,
             cost = cost + excluded.cost"#,
    )
    .bind(day)
    .bind(pid)
    .bind(mid)
    .bind(currency)
    .bind(log.prompt_tokens)
    .bind(log.completion_tokens)
    .bind(if log.status.unwrap_or(200) >= 400 { 1 } else { 0 })
    .bind(log.cost.unwrap_or(0.0))
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
                  prompt_tokens, completion_tokens, fallback_attempts, error,
                  cost, currency, rate_label, estimated_prompt_tokens, attempts_json,
                  route_intent, route_classifier, route_search, route_search_hits,
                  route_refined, route_refine_note
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
                "cost": r.get::<Option<f64>, _>("cost"),
                "currency": r.get::<Option<String>, _>("currency"),
                "rate_label": r.get::<Option<String>, _>("rate_label"),
                "estimated_prompt_tokens": r.get::<Option<i64>, _>("estimated_prompt_tokens"),
                "attempts": r
                    .get::<Option<String>, _>("attempts_json")
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok()),
                "route_intent": r.get::<Option<String>, _>("route_intent"),
                "route_classifier": r.get::<Option<String>, _>("route_classifier"),
                "route_search": r.get::<Option<String>, _>("route_search"),
                "route_search_hits": r.get::<Option<i64>, _>("route_search_hits"),
                "route_refined": r.get::<Option<i64>, _>("route_refined").map(|v| v != 0),
                "route_refine_note": r.get::<Option<String>, _>("route_refine_note"),
            })
        })
        .collect())
}

/// 分币种花费汇总。`currency = ''` 表示没有配置价格，这些请求只计 token 不计花费。
pub async fn spend_buckets(pool: &SqlitePool, since_day: &str) -> Result<Vec<SpendBucket>> {
    let rows = sqlx::query(
        r#"SELECT currency, SUM(cost) AS cost, SUM(requests) AS requests
           FROM usage_daily WHERE day >= ? AND currency <> ''
           GROUP BY currency ORDER BY cost DESC, currency ASC"#,
    )
    .bind(since_day)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| SpendBucket {
            currency: r.get("currency"),
            cost: r.get::<Option<f64>, _>("cost").unwrap_or(0.0),
            requests: r.get::<Option<i64>, _>("requests").unwrap_or(0),
        })
        .collect())
}

/// 有 token 消耗但没有价格的请求数。没有 token 的失败请求不计入，避免把
/// 「没花钱」误报成「价格缺失」。
pub async fn unpriced_request_count(pool: &SqlitePool, since_day: &str) -> Result<i64> {
    let row = sqlx::query(
        r#"SELECT COALESCE(SUM(requests), 0) AS total FROM usage_daily
           WHERE day >= ? AND currency = '' AND (prompt_tokens + completion_tokens) > 0"#,
    )
    .bind(since_day)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<Option<i64>, _>("total").unwrap_or(0))
}

/// 近 N 天按天分币种花费，供趋势表使用。
pub async fn spend_daily(pool: &SqlitePool, since_day: &str) -> Result<Vec<serde_json::Value>> {
    let rows = sqlx::query(
        r#"SELECT day, currency, SUM(cost) AS cost, SUM(requests) AS requests
           FROM usage_daily WHERE day >= ? AND currency <> ''
           GROUP BY day, currency ORDER BY day DESC, currency ASC"#,
    )
    .bind(since_day)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "day": r.get::<String, _>("day"),
                "currency": r.get::<String, _>("currency"),
                "cost": r.get::<Option<f64>, _>("cost").unwrap_or(0.0),
                "requests": r.get::<Option<i64>, _>("requests").unwrap_or(0),
            })
        })
        .collect())
}

/// 近 N 天按供应商 / 模型分币种花费。`dimension` 只接受内部固定的两个值。
pub async fn spend_by_dimension(
    pool: &SqlitePool,
    since_day: &str,
    by_model: bool,
) -> Result<Vec<serde_json::Value>> {
    let dimension = if by_model { "u.model" } else { "u.provider_id" };
    let rows = sqlx::query(&format!(
        r#"SELECT u.provider_id,
                  COALESCE(p.name, u.provider_id) AS provider,
                  {dimension} AS dimension,
                  u.currency AS currency,
                  SUM(u.cost) AS cost,
                  SUM(u.requests) AS requests,
                  SUM(u.prompt_tokens) AS prompt_tokens,
                  SUM(u.completion_tokens) AS completion_tokens
           FROM usage_daily u
           LEFT JOIN providers p ON p.id = u.provider_id
           WHERE u.day >= ? AND u.currency <> ''
           GROUP BY u.provider_id, provider, dimension, u.currency
           ORDER BY cost DESC, requests DESC"#
    ))
    .bind(since_day)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "provider_id": r.get::<String, _>("provider_id"),
                "provider": r.get::<String, _>("provider"),
                "model": r.get::<Option<String>, _>("dimension"),
                "currency": r.get::<String, _>("currency"),
                "cost": r.get::<Option<f64>, _>("cost").unwrap_or(0.0),
                "requests": r.get::<Option<i64>, _>("requests").unwrap_or(0),
                "prompt_tokens": r.get::<Option<i64>, _>("prompt_tokens").unwrap_or(0),
                "completion_tokens": r.get::<Option<i64>, _>("completion_tokens").unwrap_or(0),
            })
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct SpendBucket {
    pub currency: String,
    pub cost: f64,
    pub requests: i64,
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

/* ---------------------- Token calibration / Meta KV ---------------------- */

/// 每个 provider+model 的估算校准记录。比值 = 上游实际 prompt token / 本地估算。
#[derive(Debug, Clone, Serialize)]
pub struct Calibration {
    pub provider_id: String,
    pub model: String,
    pub samples: i64,
    pub ratio: f64,
    pub updated_at: chrono::DateTime<Utc>,
}

/// 单次观测的权重：样本越多越稳定，避免一次异常请求把系数带偏。
const CALIBRATION_MIN_RATIO: f64 = 0.5;
const CALIBRATION_MAX_RATIO: f64 = 3.0;

/// 用一次「实际 / 估算」观测更新 EWMA 校准系数，并返回更新后的记录。
/// 比值超出合理区间时按边界截断；样本数为 0 时直接采用首个观测值。
pub async fn record_token_calibration(
    pool: &SqlitePool,
    provider_id: &str,
    model: &str,
    estimated_prompt_tokens: i64,
    actual_prompt_tokens: i64,
) -> Result<Calibration> {
    // 估算或实际任一侧为 0 都说明这次观测不可用于校准（例如上游未回传用量）。
    let observed = if estimated_prompt_tokens <= 0 || actual_prompt_tokens <= 0 {
        None
    } else {
        Some(
            (actual_prompt_tokens as f64 / estimated_prompt_tokens as f64)
                .clamp(CALIBRATION_MIN_RATIO, CALIBRATION_MAX_RATIO),
        )
    };

    let existing = sqlx::query(
        "SELECT samples, ratio FROM token_calibration WHERE provider_id = ? AND model = ?",
    )
    .bind(provider_id)
    .bind(model)
    .fetch_optional(pool)
    .await?;

    let (samples, ratio) = match (&existing, observed) {
        (Some(row), Some(observed)) => {
            let samples: i64 = row.get("samples");
            let previous: f64 = row.get("ratio");
            let weight = 1.0 / (samples as f64 + 1.0).min(20.0);
            (samples + 1, previous * (1.0 - weight) + observed * weight)
        }
        (Some(row), None) => (row.get("samples"), row.get("ratio")),
        (None, Some(observed)) => (1, observed),
        (None, None) => (0, 1.0),
    };

    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO token_calibration (provider_id, model, samples, ratio, updated_at)
           VALUES (?,?,?,?,?)
           ON CONFLICT(provider_id, model) DO UPDATE SET
             samples = excluded.samples, ratio = excluded.ratio, updated_at = excluded.updated_at"#,
    )
    .bind(provider_id)
    .bind(model)
    .bind(samples)
    .bind(ratio)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(Calibration {
        provider_id: provider_id.to_owned(),
        model: model.to_owned(),
        samples,
        ratio,
        updated_at: now,
    })
}

pub async fn list_calibrations(pool: &SqlitePool) -> Result<Vec<Calibration>> {
    let rows = sqlx::query(
        "SELECT provider_id, model, samples, ratio, updated_at FROM token_calibration
         ORDER BY samples DESC, provider_id ASC, model ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| Calibration {
            provider_id: row.get("provider_id"),
            model: row.get("model"),
            samples: row.get("samples"),
            ratio: row.get("ratio"),
            updated_at: row.get("updated_at"),
        })
        .collect())
}

/// 清空校准样本，回到未校准状态。只影响本机统计口径，不影响任何请求数据。
pub async fn clear_calibrations(pool: &SqlitePool) -> Result<u64> {
    let result = sqlx::query("DELETE FROM token_calibration")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn meta_get(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT value FROM meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|row| row.get("value")))
}

pub async fn meta_set(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO meta (key, value, updated_at) VALUES (?,?,?)
           ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at"#,
    )
    .bind(key)
    .bind(value)
    .bind(Utc::now())
    .execute(pool)
    .await?;
    Ok(())
}

/* ----------------------------- App secrets ----------------------------- */

/// 搜索后端 API Key 的存放位置。集中成一个常量，避免各处硬编码字符串写错。
pub const SECRET_SEARCH_API_KEY: &str = "search.api_key";

/// 读取应用级密钥的**密文**。解密由调用方用 `crypto.rs` 完成 ——
/// 存储层不碰密钥学，出错面越小越好。
pub async fn get_secret(pool: &SqlitePool, name: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT value_enc FROM app_secrets WHERE name = ?")
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|row| row.get("value_enc")))
}

pub async fn set_secret(pool: &SqlitePool, name: &str, value_enc: &str) -> Result<()> {
    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO app_secrets (name, value_enc, created_at, updated_at) VALUES (?,?,?,?)
           ON CONFLICT(name) DO UPDATE SET value_enc = excluded.value_enc,
                                           updated_at = excluded.updated_at"#,
    )
    .bind(name)
    .bind(value_enc)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// 返回是否真的删掉了一行，供调用方区分「本来就没有」与「删掉了」。
pub async fn delete_secret(pool: &SqlitePool, name: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM app_secrets WHERE name = ?")
        .bind(name)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// 取**只有已启用模型**的供应商列表，供路由打分使用。
///
/// 为什么不能直接在 SQL 里 `WHERE enabled = 1`：`list_providers` 同时喂给
/// 前端（要看到已禁用的模型才能重新勾上）和路由（只该看已启用的）。两边
/// 需求相反，所以拆成两个入口—— SQL 层过滤会把「已禁用」这个状态对前端
/// 藏起来，用户就没法在界面上把它改回来了。
pub async fn list_routable_providers(pool: &SqlitePool) -> Result<Vec<Provider>> {
    Ok(list_providers(pool)
        .await?
        .into_iter()
        .map(|mut p| {
            p.models.retain(|m| m.enabled);
            p
        })
        .collect())
}

/// 某供应商下**只有已启用模型**的列表，供精确点名模型时查。
pub async fn list_routable_models_of(
    pool: &SqlitePool,
    provider_id: &str,
) -> Result<Vec<ModelRef>> {
    Ok(list_models_of(pool, provider_id)
        .await?
        .into_iter()
        .filter(|m| m.enabled)
        .collect())
}
