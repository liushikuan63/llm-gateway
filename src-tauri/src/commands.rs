//! Tauri 命令层：前端 UI 与 Rust 内核之间的唯一通道。
//! 命令粒度按「一次用户操作」划分，不做细碎 CRUD，减少 IPC 往返。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::State;

use crate::config::AppConfig;
use crate::crypto;
use crate::db::repo;
use crate::domain::{
    Dialect, ModelRef, Provider, PublicModel, RemoteAccessKey, Session, SessionMessage,
};
use crate::proxy::server::GatewayState;
use crate::proxy::upstream::validate_upstream_base_url;
use crate::AppState;

/* ------------------------------- Provider ------------------------------- */

/// 前端提交的 provider 表单。api_key 是明文，落库前加密。
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInput {
    pub id: Option<String>,
    pub name: String,
    pub dialect: Dialect,
    pub base_url: String,
    pub api_key: String,
    pub enabled: bool,
    pub priority: i32,
    pub models: Vec<ModelRef>,
    pub rpm_limit: i32,
    pub intelligence: i32,
    pub note: Option<String>,
}

/// 返回给前端的 provider（密钥只给掩码，永不外传明文）
#[derive(Debug, Clone, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub dialect: Dialect,
    pub base_url: String,
    pub api_key_masked: String,
    pub enabled: bool,
    pub priority: i32,
    pub models: Vec<ModelRef>,
    pub rpm_limit: i32,
    pub intelligence: i32,
    pub note: Option<String>,
    pub health: Option<crate::domain::ProviderHealth>,
    /// 当前主用 Provider 仅在内存中保存；前端据此避免把“设为主用”误当成启停。
    pub is_active: bool,
}

impl ProviderView {
    fn from(
        p: Provider,
        health: Option<crate::domain::ProviderHealth>,
        active_id: Option<&str>,
    ) -> Self {
        let masked = if p.api_key_enc.is_empty() {
            String::new()
        } else {
            // api_key_enc 是随机 nonce + 密文的 Base64，直接脱敏会展示毫无意义的
            // 密文尾部。仅在进程内解密后再掩码，明文不会进入 IPC 返回值。
            crypto::decrypt(&p.api_key_enc)
                .map(|key| crypto::mask(&key))
                .unwrap_or_else(|_| "已保存（无法解密）".into())
        };
        let is_active = active_id == Some(p.id.as_str());
        Self {
            id: p.id,
            name: p.name,
            dialect: p.dialect,
            base_url: p.base_url,
            api_key_masked: masked,
            enabled: p.enabled,
            priority: p.priority,
            models: p.models,
            rpm_limit: p.rpm_limit,
            intelligence: p.intelligence,
            note: p.note,
            health,
            is_active,
        }
    }
}

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> Result<Vec<ProviderView>, String> {
    let list = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    let gw = state.gateway.clone();
    let active_id = gw.active.read().clone();
    Ok(list
        .into_iter()
        .map(|p| {
            let h = p.models.first().map(|m| gw.health.get(&p.id, &m.upstream));
            ProviderView::from(p, h, active_id.as_deref())
        })
        .collect())
}

#[tauri::command]
pub async fn upsert_provider(
    state: State<'_, AppState>,
    input: ProviderInput,
) -> Result<String, String> {
    // 保存期和出站期使用同一规则：公网 HTTP 绝不能携带上游 Key；本地
    // Ollama/vLLM 与 RFC1918 LAN 服务仍可用 HTTP。
    let base_url = validate_upstream_base_url(&input.base_url)
        .map_err(|error| error.to_string())?
        .as_str()
        .trim_end_matches('/')
        .to_string();
    let enc = if input.api_key.trim().is_empty() {
        // 未填密钥：保留原值（编辑场景下用户往往只改别的字段）
        if let Some(id) = &input.id {
            repo::list_providers(state.db.pool())
                .await
                .ok()
                .and_then(|l| l.into_iter().find(|p| &p.id == id))
                .map(|p| p.api_key_enc)
                .unwrap_or_default()
        } else {
            String::new()
        }
    } else {
        crypto::encrypt(input.api_key.trim()).map_err(|e| e.to_string())?
    };

    let id = input
        .id
        .clone()
        .unwrap_or_else(|| format!("p-{}", uuid::Uuid::new_v4().simple()));
    let enabled = input.enabled;
    let now = chrono::Utc::now();

    let p = Provider {
        id: id.clone(),
        name: input.name,
        dialect: input.dialect,
        base_url,
        api_key_enc: enc,
        enabled: input.enabled,
        priority: input.priority,
        models: input.models,
        rpm_limit: input.rpm_limit,
        intelligence: input.intelligence,
        note: input.note,
        created_at: now,
        updated_at: now,
    };

    repo::upsert_provider(state.db.pool(), &p)
        .await
        .map_err(|e| e.to_string())?;
    // 内核缓存要同步刷新，否则新加的 provider 不会立刻进路由表
    state
        .gateway
        .reload_providers()
        .await
        .map_err(|e| e.to_string())?;
    // 停用当前主用项后，不能让内存中继续保留一个不可路由的 active id。
    if !enabled && state.gateway.active.read().as_deref() == Some(id.as_str()) {
        *state.gateway.active.write() = None;
    }
    Ok(id)
}

#[tauri::command]
pub async fn delete_provider(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let was_active = state.gateway.active.read().as_deref() == Some(id.as_str());
    repo::delete_provider(state.db.pool(), &id)
        .await
        .map_err(|e| e.to_string())?;
    state
        .gateway
        .reload_providers()
        .await
        .map_err(|e| e.to_string())?;
    if was_active {
        *state.gateway.active.write() = None;
    }
    Ok(())
}

/// 连通性测试：发一条极短请求，返回 (延迟ms, 模型, 错误信息)
#[tauri::command]
pub async fn test_provider(
    state: State<'_, AppState>,
    id: String,
) -> Result<serde_json::Value, String> {
    let p = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| "provider 不存在".to_string())?;

    let model = p
        .models
        .first()
        .map(|m| m.upstream.clone())
        .unwrap_or_else(|| "auto".into());

    let req = crate::domain::ChatRequest {
        model: model.clone(),
        messages: vec![crate::domain::Message::user("hi")],
        temperature: Some(0.0),
        top_p: None,
        max_tokens: Some(16),
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        thinking: None,
        extra: Default::default(),
    };

    let started = std::time::Instant::now();
    let res = state
        .gateway
        .upstream
        .call(&p, &req, &model, std::time::Duration::from_secs(30))
        .await;
    let ms = started.elapsed().as_millis() as u64;

    Ok(match res {
        Ok(r) => {
            serde_json::json!({ "ok": true, "latency_ms": ms, "model": r.model, "reply": r.content })
        }
        Err(e) => serde_json::json!({ "ok": false, "latency_ms": ms, "error": e.to_string() }),
    })
}

/// 一键切换（CC Switch 的核心交互）。热生效，客户端无需重启。
#[tauri::command]
pub async fn set_active_provider(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.gateway.set_active(&id).map_err(|e| e.to_string())?;
    Ok(())
}

/* -------------------------------- 配置 -------------------------------- */

#[derive(Debug, Clone, Serialize)]
pub struct ConfigUpdateResult {
    pub config: AppConfig,
    /// axum listener 在启动时绑定，监听地址和端口变更只能重启后生效。
    pub restart_required: bool,
    pub restart_reasons: Vec<String>,
}

fn listener_restart_reasons(previous: &AppConfig, next: &AppConfig) -> Vec<String> {
    let mut reasons = Vec::new();
    if previous.bind != next.bind {
        reasons.push("监听地址".into());
    }
    if previous.port != next.port {
        reasons.push("监听端口".into());
    }
    if previous.remote_mode.enabled != next.remote_mode.enabled {
        reasons.push("远程 HTTPS 反代模式".into());
    }
    if previous.remote_mode.public_url != next.remote_mode.public_url {
        reasons.push("远程公开地址".into());
    }
    reasons
}

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    Ok(state.config.read().clone())
}

#[tauri::command]
pub async fn update_config(
    state: State<'_, AppState>,
    cfg: AppConfig,
) -> Result<ConfigUpdateResult, String> {
    let previous = state.config.read().clone();
    let mut cfg = cfg;
    // 远程反代与局域网直连是互斥的暴露模型。配置层会把二者规范化为
    // 0.0.0.0 或 127.0.0.1，不能信任前端或手工编辑传来的 bind 值。
    cfg.normalize_custom_rules();
    cfg.validate_custom_rules().map_err(|e| e.to_string())?;
    cfg.normalize_listener();
    cfg.validate_remote_mode().map_err(|e| e.to_string())?;
    if cfg.remote_mode.enabled
        && repo::enabled_remote_access_key_count(state.db.pool())
            .await
            .map_err(|e| e.to_string())?
            == 0
    {
        return Err("启用远程 HTTPS 反代模式前，必须先创建并启用至少一个独立访问 Key".into());
    }
    let restart_reasons = listener_restart_reasons(&previous, &cfg);
    cfg.save().map_err(|e| e.to_string())?;
    *state.config.write() = cfg.clone();
    *state.gateway.cfg.write() = cfg.clone();
    // Router 持有本轮不可变快照；配置热更新后必须同步替换它，才能让下一次
    // custom 策略请求立即使用 UI 中保存的新规则。
    state
        .gateway
        .router
        .set_custom_rules(cfg.custom_rules.clone());
    Ok(ConfigUpdateResult {
        config: cfg,
        restart_required: !restart_reasons.is_empty(),
        restart_reasons,
    })
}

#[tauri::command]
pub async fn list_models(state: State<'_, AppState>) -> Result<Vec<PublicModel>, String> {
    let providers = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    Ok(crate::router::Router::public_models(&providers))
}

#[tauri::command]
pub async fn get_unified_key(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let cfg = state.config.read().clone();
    Ok(serde_json::json!({
        "key": cfg.unified_key,
        "base_url": cfg.base_url(),
        "openai_endpoint": format!("{}/v1", cfg.base_url()),
        "anthropic_endpoint": cfg.base_url(),
        "ollama_endpoint": cfg.base_url(),
    }))
}

#[tauri::command]
pub async fn rotate_unified_key(state: State<'_, AppState>) -> Result<String, String> {
    let mut cfg = state.config.read().clone();
    cfg.unified_key = crypto::new_unified_key();
    cfg.save().map_err(|e| e.to_string())?;
    *state.config.write() = cfg.clone();
    *state.gateway.cfg.write() = cfg.clone();
    Ok(cfg.unified_key)
}

/* ------------------------ Remote HTTPS access keys ----------------------- */

/// 不向 IPC 返回哈希或原始 Key。原始 `secret` 只存在于创建命令的单次响应里。
#[derive(Debug, Clone, Serialize)]
pub struct RemoteAccessKeyView {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub rpm_limit: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<RemoteAccessKey> for RemoteAccessKeyView {
    fn from(key: RemoteAccessKey) -> Self {
        Self {
            id: key.id,
            label: key.label,
            enabled: key.enabled,
            rpm_limit: key.rpm_limit,
            created_at: key.created_at,
            updated_at: key.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateRemoteAccessKeyInput {
    pub label: String,
    pub rpm_limit: u32,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRemoteAccessKeyInput {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub rpm_limit: u32,
}

#[derive(Debug, Serialize)]
pub struct CreatedRemoteAccessKey {
    pub key: RemoteAccessKeyView,
    pub secret: String,
}

fn validate_remote_key_label(label: &str) -> Result<String, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("访问 Key 名称不能为空".into());
    }
    if label.chars().count() > 64 {
        return Err("访问 Key 名称不能超过 64 个字符".into());
    }
    if label.chars().any(char::is_control) {
        return Err("访问 Key 名称不能包含控制字符".into());
    }
    Ok(label.to_owned())
}

fn validate_remote_key_rpm(rpm_limit: u32) -> Result<u32, String> {
    if !(1..=100_000).contains(&rpm_limit) {
        return Err("每个访问 Key 的 RPM 必须在 1 到 100000 之间".into());
    }
    Ok(rpm_limit)
}

fn sha256_hex(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tauri::command]
pub async fn list_remote_access_keys(
    state: State<'_, AppState>,
) -> Result<Vec<RemoteAccessKeyView>, String> {
    repo::list_remote_access_keys(state.db.pool())
        .await
        .map_err(|e| e.to_string())
        .map(|keys| keys.into_iter().map(RemoteAccessKeyView::from).collect())
}

#[tauri::command]
pub async fn create_remote_access_key(
    state: State<'_, AppState>,
    input: CreateRemoteAccessKeyInput,
) -> Result<CreatedRemoteAccessKey, String> {
    let label = validate_remote_key_label(&input.label)?;
    let rpm_limit = validate_remote_key_rpm(input.rpm_limit)?;
    let existing = repo::list_remote_access_keys(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    if existing.len() >= 100 {
        return Err("最多可创建 100 个远程访问 Key".into());
    }

    let secret = crypto::new_unified_key();
    let now = chrono::Utc::now();
    let key = RemoteAccessKey {
        id: format!("rk-{}", uuid::Uuid::new_v4().simple()),
        label,
        key_hash: sha256_hex(&secret),
        enabled: true,
        rpm_limit,
        created_at: now,
        updated_at: now,
    };
    repo::create_remote_access_key(state.db.pool(), &key)
        .await
        .map_err(|e| e.to_string())?;
    state
        .gateway
        .reload_providers()
        .await
        .map_err(|e| e.to_string())?;

    Ok(CreatedRemoteAccessKey {
        key: key.into(),
        secret,
    })
}

#[tauri::command]
pub async fn update_remote_access_key(
    state: State<'_, AppState>,
    input: UpdateRemoteAccessKeyInput,
) -> Result<RemoteAccessKeyView, String> {
    let label = validate_remote_key_label(&input.label)?;
    let rpm_limit = validate_remote_key_rpm(input.rpm_limit)?;
    let keys = repo::list_remote_access_keys(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    let current = keys
        .iter()
        .find(|key| key.id == input.id)
        .ok_or_else(|| "远程访问 Key 不存在".to_string())?;
    if current.enabled
        && !input.enabled
        && state.config.read().remote_mode.enabled
        && keys.iter().filter(|key| key.enabled).count() <= 1
    {
        return Err("远程 HTTPS 反代模式已启用，不能停用最后一个访问 Key".into());
    }

    if !repo::update_remote_access_key(state.db.pool(), &input.id, &label, input.enabled, rpm_limit)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err("远程访问 Key 不存在".into());
    }
    state
        .gateway
        .reload_providers()
        .await
        .map_err(|e| e.to_string())?;
    repo::list_remote_access_keys(state.db.pool())
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|key| key.id == input.id)
        .map(RemoteAccessKeyView::from)
        .ok_or_else(|| "远程访问 Key 更新后无法读取".to_string())
}

#[tauri::command]
pub async fn delete_remote_access_key(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let keys = repo::list_remote_access_keys(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    let current = keys
        .iter()
        .find(|key| key.id == id)
        .ok_or_else(|| "远程访问 Key 不存在".to_string())?;
    if current.enabled
        && state.config.read().remote_mode.enabled
        && keys.iter().filter(|key| key.enabled).count() <= 1
    {
        return Err("远程 HTTPS 反代模式已启用，不能删除最后一个访问 Key".into());
    }
    if !repo::delete_remote_access_key(state.db.pool(), &id)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err("远程访问 Key 不存在".into());
    }
    state
        .gateway
        .reload_providers()
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/* -------------------------------- 会话 -------------------------------- */

#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub session: Session,
    pub message_count: i64,
}

#[tauri::command]
pub async fn list_sessions(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<SessionView>, String> {
    let rows = sqlx::query(
        r#"SELECT s.id, s.snapshot_id, s.title, s.sticky_provider_id, s.sticky_model,
                  s.sticky_expires_at, s.total_tokens, s.compact_count, s.summary,
                  s.created_at, s.updated_at, COUNT(m.id) AS message_count
           FROM sessions s
           LEFT JOIN session_messages m ON m.session_id = s.id
           GROUP BY s.id
           ORDER BY s.updated_at DESC
           LIMIT ?"#,
    )
    .bind(limit.unwrap_or(50).clamp(1, 200))
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .iter()
        .map(|r| SessionView {
            session: Session {
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
            },
            message_count: r.get("message_count"),
        })
        .collect())
}

#[tauri::command]
pub async fn get_session_messages(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<SessionMessage>, String> {
    // 管理页需要审计完整历史，而 recent_messages 是给转发链路重建上下文用的，
    // 会有意过滤 compacted 消息；两者不能复用。
    let rows = sqlx::query(
        r#"SELECT id, session_id, role, content, tool_calls, tool_call_id, name,
                  routed_provider, routed_model,
                  compacted, prompt_tokens, completion_tokens, created_at
           FROM session_messages
           WHERE session_id = ?
           ORDER BY id ASC
           LIMIT 500"#,
    )
    .bind(id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .iter()
        .map(|r| SessionMessage {
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
        })
        .collect())
}

#[tauri::command]
pub async fn delete_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    repo::delete_session(state.db.pool(), &id)
        .await
        .map_err(|e| e.to_string())
}

/// 手动触发压缩
#[tauri::command]
pub async fn compact_session(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let session = repo::get_or_create_session(state.db.pool(), &id)
        .await
        .map_err(|e| e.to_string())?;
    let cfg = state.config.read().clone();
    let gw: Arc<GatewayState> = state.gateway.clone();

    let summary = gw
        .ctx
        .compact(&session, &cfg, |msgs| {
            let gw = gw.clone();
            let prev = session.summary.clone().unwrap_or_default();
            async move { gw.summarize(msgs, &prev).await }
        })
        .await
        .map_err(|e| e.to_string())?;

    Ok(summary.unwrap_or_else(|| "无需压缩".into()))
}

/* -------------------------------- 快照 -------------------------------- */

/// 快照只保存可切换的 Provider 配置，刻意不保存 API Key 密文。应用同 ID
/// Provider 时保留当前库中的密文，这样快照不会成为另一份可导出的凭据副本。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotProvider {
    id: String,
    name: String,
    dialect: Dialect,
    base_url: String,
    enabled: bool,
    priority: i32,
    models: Vec<ModelRef>,
    rpm_limit: i32,
    intelligence: i32,
    note: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<&Provider> for SnapshotProvider {
    fn from(provider: &Provider) -> Self {
        Self {
            id: provider.id.clone(),
            name: provider.name.clone(),
            dialect: provider.dialect,
            base_url: provider.base_url.clone(),
            enabled: provider.enabled,
            priority: provider.priority,
            models: provider.models.clone(),
            rpm_limit: provider.rpm_limit,
            intelligence: provider.intelligence,
            note: provider.note.clone(),
            created_at: provider.created_at,
            updated_at: provider.updated_at,
        }
    }
}

impl SnapshotProvider {
    fn into_provider(self, api_key_enc: String) -> Provider {
        Provider {
            id: self.id,
            name: self.name,
            dialect: self.dialect,
            base_url: self.base_url,
            api_key_enc,
            enabled: self.enabled,
            priority: self.priority,
            models: self.models,
            rpm_limit: self.rpm_limit,
            intelligence: self.intelligence,
            note: self.note,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotPayload {
    version: u8,
    config: AppConfig,
    providers: Vec<SnapshotProvider>,
    #[serde(default)]
    active: Option<String>,
}

/// 快照列表只能展示元数据，避免把密文（或完整配置）经 IPC 发送给 UI。
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotView {
    pub id: String,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub provider_count: usize,
    pub active_provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotApplyResult {
    pub config: AppConfig,
    pub active_provider_id: Option<String>,
    pub restart_required: bool,
    pub restart_reasons: Vec<String>,
}

async fn replace_snapshot_providers(
    pool: &SqlitePool,
    providers: &[Provider],
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

    // 不使用 upsert 叠加旧数据。快照代表完整项目配置，列表中不存在的
    // Provider/模型必须同步移除，二者放在同一个事务中避免半成品状态。
    sqlx::query("DELETE FROM models")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM providers")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    for provider in providers {
        sqlx::query(
            r#"INSERT INTO providers
                 (id, name, dialect, base_url, api_key_enc, enabled, priority, rpm_limit,
                  intelligence, note, created_at, updated_at)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(&provider.id)
        .bind(&provider.name)
        .bind(match provider.dialect {
            Dialect::OpenAI => "openai",
            Dialect::Anthropic => "anthropic",
            Dialect::Gemini => "gemini",
            Dialect::Ollama => "ollama",
        })
        .bind(&provider.base_url)
        .bind(&provider.api_key_enc)
        .bind(provider.enabled as i64)
        .bind(provider.priority)
        .bind(provider.rpm_limit)
        .bind(provider.intelligence)
        .bind(&provider.note)
        .bind(provider.created_at)
        .bind(provider.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

        for model in &provider.models {
            sqlx::query(
                r#"INSERT INTO models
                     (id, provider_id, alias, upstream, context_window, supports_tools,
                      supports_vision, supports_stream)
                   VALUES (?,?,?,?,?,?,?,?)"#,
            )
            .bind(format!("{}:{}", provider.id, model.alias))
            .bind(&provider.id)
            .bind(&model.alias)
            .bind(&model.upstream)
            .bind(model.context_window)
            .bind(model.supports_tools as i64)
            .bind(model.supports_vision as i64)
            .bind(model.supports_stream as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }
    }

    tx.commit().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_snapshots(state: State<'_, AppState>) -> Result<Vec<SnapshotView>, String> {
    let snapshots = repo::list_snapshots(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    Ok(snapshots
        .into_iter()
        .map(|snapshot| SnapshotView {
            provider_count: snapshot
                .payload
                .get("providers")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len),
            active_provider_id: snapshot
                .payload
                .get("active")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            id: snapshot.id,
            name: snapshot.name,
            created_at: snapshot.created_at,
        })
        .collect())
}

#[tauri::command]
pub async fn create_snapshot(state: State<'_, AppState>, name: String) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("快照名称不能为空".into());
    }
    let providers = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    // 快照用于切换项目配置，而不是复制凭据或改变暴露边界。统一 Key 与远程
    // HTTPS 模式都在当前设备保留；远程访问 Key 更不会进入快照载荷。
    let mut snapshot_config = state.config.read().clone();
    snapshot_config.unified_key.clear();
    snapshot_config.remote_mode = Default::default();
    let payload = serde_json::to_value(SnapshotPayload {
        version: 3,
        config: snapshot_config,
        providers: providers.iter().map(SnapshotProvider::from).collect(),
        active: state.gateway.active.read().clone(),
    })
    .map_err(|e| e.to_string())?;
    repo::create_snapshot(state.db.pool(), name, &payload)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn apply_snapshot(
    state: State<'_, AppState>,
    id: String,
) -> Result<SnapshotApplyResult, String> {
    let snap = repo::get_snapshot(state.db.pool(), &id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "快照不存在".to_string())?;

    let SnapshotPayload {
        config: mut cfg,
        providers,
        active,
        ..
    } = serde_json::from_value(snap.payload)
        .map_err(|_| "快照格式不完整，未应用任何更改".to_string())?;
    let existing_keys: HashMap<String, String> = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|provider| (provider.id, provider.api_key_enc))
        .collect();
    let providers: Vec<Provider> = providers
        .into_iter()
        .map(|provider| {
            let api_key_enc = existing_keys.get(&provider.id).cloned().unwrap_or_default();
            provider.into_provider(api_key_enc)
        })
        .collect();
    let mut provider_ids = HashSet::new();
    for provider in &providers {
        if !provider_ids.insert(provider.id.as_str()) {
            return Err(format!("快照包含重复的 Provider ID：{}", provider.id));
        }
    }
    if let Some(active_id) = &active {
        if !providers
            .iter()
            .any(|provider| provider.id == *active_id && provider.enabled)
        {
            return Err("快照中的主用 Provider 不存在或已停用，未应用任何更改".into());
        }
    }

    let previous = state.config.read().clone();
    // 兼容旧快照：它们可能还包含统一 Key 或远程公开地址。两个值都是本机
    // 安全边界，不允许通过项目快照覆盖。
    cfg.unified_key = previous.unified_key.clone();
    cfg.remote_mode = previous.remote_mode.clone();
    cfg.normalize_custom_rules();
    cfg.validate_custom_rules().map_err(|e| e.to_string())?;
    cfg.normalize_listener();
    cfg.validate_remote_mode().map_err(|e| e.to_string())?;
    if cfg.remote_mode.enabled
        && repo::enabled_remote_access_key_count(state.db.pool())
            .await
            .map_err(|e| e.to_string())?
            == 0
    {
        return Err("本机远程 HTTPS 反代模式缺少已启用的独立访问 Key，未应用快照".into());
    }
    let restart_reasons = listener_restart_reasons(&previous, &cfg);

    replace_snapshot_providers(state.db.pool(), &providers).await?;
    cfg.save().map_err(|e| e.to_string())?;
    *state.config.write() = cfg.clone();
    *state.gateway.cfg.write() = cfg.clone();
    state
        .gateway
        .router
        .set_custom_rules(cfg.custom_rules.clone());
    state
        .gateway
        .reload_providers()
        .await
        .map_err(|e| e.to_string())?;
    *state.gateway.active.write() = None;
    if let Some(active_id) = &active {
        state
            .gateway
            .set_active(active_id)
            .map_err(|e| e.to_string())?;
    }

    Ok(SnapshotApplyResult {
        config: cfg,
        active_provider_id: active,
        restart_required: !restart_reasons.is_empty(),
        restart_reasons,
    })
}

/* -------------------------------- 统计 -------------------------------- */

#[derive(Debug, Clone, Serialize)]
pub struct ProviderUsage {
    pub provider_id: Option<String>,
    pub provider: String,
    pub requests: i64,
    pub successful_requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub fallback_attempts: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsOverview {
    /// 与 Node Store.statsOverview 一致，统计滚动最近 24 小时。
    pub window: String,
    pub total_requests: i64,
    pub today_requests: i64,
    pub success_rate: f64,
    pub avg_latency_ms: i64,
    pub total_fallbacks: i64,
    pub fallback_request_count: i64,
    pub fallback_rate: f64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub provider_distribution: Vec<ProviderUsage>,
}

#[tauri::command]
pub async fn stats_overview(state: State<'_, AppState>) -> Result<StatsOverview, String> {
    let since = chrono::Utc::now().timestamp() - 24 * 60 * 60;
    let total_row = sqlx::query("SELECT COUNT(*) AS total_requests FROM requests")
        .fetch_one(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    let overview_row = sqlx::query(
        r#"SELECT COUNT(*) AS today_requests,
                  SUM(CASE WHEN COALESCE(status, 0) < 400 THEN 1 ELSE 0 END) AS successful_requests,
                  AVG(COALESCE(latency_ms, 0)) AS avg_latency_ms,
                  SUM(COALESCE(fallback_attempts, 0)) AS total_fallbacks,
                  SUM(CASE WHEN COALESCE(fallback_attempts, 0) > 0 THEN 1 ELSE 0 END) AS fallback_request_count,
                  SUM(COALESCE(prompt_tokens, 0)) AS total_prompt_tokens,
                  SUM(COALESCE(completion_tokens, 0)) AS total_completion_tokens
           FROM requests
           WHERE ts >= ?"#,
    )
    .bind(since)
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| e.to_string())?;

    let today_requests = overview_row.get::<i64, _>("today_requests");
    let successful_requests = overview_row
        .get::<Option<i64>, _>("successful_requests")
        .unwrap_or(0);
    let fallback_request_count = overview_row
        .get::<Option<i64>, _>("fallback_request_count")
        .unwrap_or(0);
    let distribution_rows = sqlx::query(
        r#"SELECT r.routed_provider AS provider_id,
                  COALESCE(p.name, r.routed_provider, '未路由') AS provider,
                  COUNT(*) AS requests,
                  SUM(CASE WHEN COALESCE(r.status, 0) < 400 THEN 1 ELSE 0 END) AS successful_requests,
                  SUM(COALESCE(r.prompt_tokens, 0)) AS prompt_tokens,
                  SUM(COALESCE(r.completion_tokens, 0)) AS completion_tokens,
                  SUM(COALESCE(r.fallback_attempts, 0)) AS fallback_attempts
           FROM requests r
           LEFT JOIN providers p ON p.id = r.routed_provider
           WHERE r.ts >= ?
           GROUP BY r.routed_provider, p.name
           ORDER BY requests DESC, provider ASC"#,
    )
    .bind(since)
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| e.to_string())?;

    let provider_distribution = distribution_rows
        .iter()
        .map(|row| ProviderUsage {
            provider_id: row.get("provider_id"),
            provider: row.get("provider"),
            requests: row.get("requests"),
            successful_requests: row
                .get::<Option<i64>, _>("successful_requests")
                .unwrap_or(0),
            prompt_tokens: row.get::<Option<i64>, _>("prompt_tokens").unwrap_or(0),
            completion_tokens: row.get::<Option<i64>, _>("completion_tokens").unwrap_or(0),
            fallback_attempts: row.get::<Option<i64>, _>("fallback_attempts").unwrap_or(0),
        })
        .collect();

    Ok(StatsOverview {
        window: "24h".into(),
        total_requests: total_row.get("total_requests"),
        today_requests,
        success_rate: if today_requests == 0 {
            1.0
        } else {
            successful_requests as f64 / today_requests as f64
        },
        avg_latency_ms: overview_row
            .get::<Option<f64>, _>("avg_latency_ms")
            .unwrap_or(0.0)
            .round() as i64,
        total_fallbacks: overview_row
            .get::<Option<i64>, _>("total_fallbacks")
            .unwrap_or(0),
        fallback_request_count,
        fallback_rate: if today_requests == 0 {
            0.0
        } else {
            fallback_request_count as f64 / today_requests as f64
        },
        total_prompt_tokens: overview_row
            .get::<Option<i64>, _>("total_prompt_tokens")
            .unwrap_or(0),
        total_completion_tokens: overview_row
            .get::<Option<i64>, _>("total_completion_tokens")
            .unwrap_or(0),
        provider_distribution,
    })
}

#[tauri::command]
pub async fn recent_requests(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<serde_json::Value>, String> {
    repo::recent_requests(state.db.pool(), limit.unwrap_or(100).clamp(1, 500))
        .await
        .map_err(|e| e.to_string())
}

/* --------------------------- CLI 工具接管（可选） --------------------------- */

/// 把 Claude Code / Codex / Gemini CLI 的 base_url 指向本地网关。
/// 这是 CC Switch "接管模式" 的实现：配置文件只写一次，
/// 之后切换 provider 只改网关内部路由，客户端无感。
#[tauri::command]
pub async fn apply_takeover(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let cfg = state.config.read().clone();
    let base = cfg.base_url();
    let mut changed = Vec::new();

    let home = dirs::home_dir().ok_or_else(|| "找不到用户目录".to_string())?;

    if cfg.takeover.claude_code {
        let dir = home.join(".claude");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("settings.json");
        let mut v: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        v["env"]["ANTHROPIC_BASE_URL"] = serde_json::json!(base);
        v["env"]["ANTHROPIC_AUTH_TOKEN"] = serde_json::json!(cfg.unified_key);
        // 关键：置空 API_KEY，否则 Claude Code 会发 x-api-key 而非 Bearer，
        // 自建网关普遍只认 Bearer，会直接 401
        v["env"]["ANTHROPIC_API_KEY"] = serde_json::json!("");
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap())
            .map_err(|e| e.to_string())?;
        changed.push(path.display().to_string());
    }

    if cfg.takeover.codex {
        let dir = home.join(".codex");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("config.toml");
        let mut s = std::fs::read_to_string(&path).unwrap_or_default();
        // 粗暴但可靠：整段替换 base_url 行
        let lines: Vec<String> = s
            .lines()
            .filter(|l| !l.trim_start().starts_with("base_url"))
            .map(|l| l.to_string())
            .collect();
        s = lines.join("\n");
        s.push_str(&format!("\nbase_url = \"{base}/v1\"\n"));
        std::fs::write(&path, s).map_err(|e| e.to_string())?;
        changed.push(path.display().to_string());
    }

    if cfg.takeover.gemini_cli {
        let dir = home.join(".gemini");
        let path = dir.join(".env");
        let backup = write_gemini_takeover_env(&path, &base)?;
        let display = match backup {
            Some(backup) => format!("{}（已备份至 {}）", path.display(), backup.display()),
            None => path.display().to_string(),
        };
        changed.push(display);
    }

    Ok(changed)
}

/// 合并 Gemini CLI 的网关地址，避免覆盖用户的其它环境变量或注释。
/// 已有文件会在写入前保留一份同目录的独立备份，以便用户手动恢复。
fn write_gemini_takeover_env(
    path: &std::path::Path,
    base_url: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Gemini 配置路径无父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;

    let existing = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(format!(
                "读取 Gemini 配置失败（未写入，原文件保持不变）: {error}"
            ));
        }
    };

    let updated = replace_dotenv_value(&existing, "GOOGLE_GEMINI_BASE_URL", base_url);
    let backup = backup_before_overwrite(path)?;
    std::fs::write(path, updated).map_err(|error| {
        format!(
            "写入 Gemini 配置失败（可从备份恢复）: {}: {error}",
            path.display()
        )
    })?;
    Ok(backup)
}

fn backup_before_overwrite(path: &std::path::Path) -> Result<Option<std::path::PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    if !path.is_file() {
        return Err(format!("Gemini 配置路径不是普通文件: {}", path.display()));
    }

    let file_name = path
        .file_name()
        .ok_or_else(|| format!("Gemini 配置路径没有文件名: {}", path.display()))?
        .to_string_lossy();
    let backup = path.with_file_name(format!(
        "{file_name}.llm-gateway-backup-{}.bak",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::copy(path, &backup).map_err(|error| {
        format!(
            "创建 Gemini 配置备份失败（未写入原文件）: {}: {error}",
            backup.display()
        )
    })?;
    Ok(Some(backup))
}

fn replace_dotenv_value(contents: &str, key: &str, value: &str) -> String {
    let mut output = String::with_capacity(contents.len() + key.len() + value.len() + 2);
    let mut found = false;

    for chunk in contents.split_inclusive('\n') {
        let (line, ending) = split_line_ending(chunk);
        if is_dotenv_assignment(line, key) {
            output.push_str(key);
            output.push('=');
            output.push_str(value);
            output.push_str(ending);
            found = true;
        } else {
            output.push_str(chunk);
        }
    }

    if !found {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(key);
        output.push('=');
        output.push_str(value);
        output.push('\n');
    }
    output
}

fn split_line_ending(chunk: &str) -> (&str, &str) {
    if let Some(line) = chunk.strip_suffix("\r\n") {
        (line, "\r\n")
    } else if let Some(line) = chunk.strip_suffix('\n') {
        (line, "\n")
    } else {
        (chunk, "")
    }
}

fn is_dotenv_assignment(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    let assignment = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    assignment
        .split_once('=')
        .is_some_and(|(name, _)| name.trim() == key)
}

#[cfg(test)]
mod takeover_tests {
    use super::write_gemini_takeover_env;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "llm-gateway-takeover-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&path).expect("create temporary takeover directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn gemini_takeover_preserves_existing_env_and_creates_recoverable_backup() {
        let temp = TempDir::new();
        let path = temp.0.join(".gemini").join(".env");
        std::fs::create_dir_all(path.parent().expect("Gemini config parent"))
            .expect("create Gemini config directory");
        let original = "# keep this comment\r\nGOOGLE_API_KEY=user-key\r\nGOOGLE_GEMINI_BASE_URL=https://old.example\r\nCUSTOM_VALUE=keep\r\n";
        std::fs::write(&path, original).expect("write original Gemini config");

        let backup = write_gemini_takeover_env(&path, "http://127.0.0.1:15721")
            .expect("write takeover config")
            .expect("existing config should be backed up");

        assert_eq!(
            std::fs::read_to_string(&backup).expect("read backup"),
            original
        );
        let updated = std::fs::read_to_string(&path).expect("read updated config");
        assert!(updated.contains("# keep this comment\r\n"));
        assert!(updated.contains("GOOGLE_API_KEY=user-key\r\n"));
        assert!(updated.contains("CUSTOM_VALUE=keep\r\n"));
        assert!(updated.contains("GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:15721\r\n"));
        assert!(!updated.contains("https://old.example"));
    }

    #[test]
    fn gemini_takeover_creates_new_env_without_unnecessary_backup() {
        let temp = TempDir::new();
        let path = temp.0.join(".gemini").join(".env");

        let backup = write_gemini_takeover_env(&path, "http://127.0.0.1:15721")
            .expect("write new takeover config");

        assert!(backup.is_none());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read new config"),
            "GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:15721\n"
        );
    }
}

/// 导出/导入整个数据目录（多设备同步用，等价于 CC Switch 的配置目录同步）
#[tauri::command]
pub async fn export_bundle(_state: State<'_, AppState>, dest: String) -> Result<(), String> {
    let src = crate::config::app_data_dir();
    let dest = std::path::PathBuf::from(dest);
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    for name in ["config.toml", "gateway.db"] {
        let _ = std::fs::copy(src.join(name), dest.join(name));
    }
    Ok(())
}
