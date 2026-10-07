//! Tauri 命令层：前端 UI 与 Rust 内核之间的唯一通道。
//! 命令粒度按「一次用户操作」划分，不做细碎 CRUD，减少 IPC 往返。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::{Emitter, Manager, State};

use crate::config::AppConfig;
use crate::crypto;
use crate::db::repo;
use crate::domain::{
    Dialect, ModelRef, Provider, PublicModel, RemoteAccessKey, Session, SessionMessage,
};
use crate::model_catalog;
use crate::proxy::server::GatewayState;
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
    /// 任务卡二 A5：这个 Provider 走的是**账号型上游**（非 `None` 时指向
    /// `agent_runtimes.id`）还是 HTTP 直连（`None`）。
    ///
    /// 必须回给前端：不显示的话，用户在同一张卡片上看到「API Key：已保存」
    /// 却完全不知道请求其实**没走 HTTP**，那个 Key 是摆设。
    /// 账号型上游的登录态由各家 CLI 自己管，网关不读不存（卡片红线）。
    pub runtime_id: Option<String>,
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
            runtime_id: p.runtime_id,
        }
    }
}

/// 启动状态查询：不依赖 AppState，供前端在后端初始化完成前轮询。
#[tauri::command]
pub fn get_boot_state(
    boot: tauri::State<'_, crate::boot::BootState>,
) -> crate::boot::BootStateView {
    boot.view()
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

/// 读取当前 API 地址可用的模型目录。目录发现不会落库；前端明确选择模型并保存
/// Provider 后才会更新数据库。空 Key 只会在当前目标与保存记录严格一致时复用。
#[tauri::command]
pub async fn discover_provider_models(
    state: State<'_, AppState>,
    input: model_catalog::DiscoveryInput,
) -> Result<model_catalog::DiscoveryResponse, String> {
    let saved_provider = if let Some(provider_id) = input.provider_id.as_deref() {
        repo::list_providers(state.db.pool())
            .await
            .map_err(|_| "无法读取已保存的 Provider".to_string())?
            .into_iter()
            .find(|provider| provider.id == provider_id)
    } else {
        None
    };
    let proxy_url = state.config.read().http_proxy.clone();

    model_catalog::discover(&input, saved_provider.as_ref(), proxy_url.as_deref()).await
}

#[tauri::command]
pub async fn upsert_provider(
    state: State<'_, AppState>,
    input: ProviderInput,
) -> Result<String, String> {
    // 保存期和目录发现期使用同一规则：公网 HTTP 绝不能携带上游 Key；本地
    // Ollama/vLLM 与 RFC1918 LAN 服务仍可用 HTTP。完整接口 URL 会在此收敛为
    // 可由转发器安全追加路径的基础地址。
    let base_url = model_catalog::normalize_base_url(input.dialect, &input.base_url)?;
    let enc = if input.api_key.trim().is_empty() {
        // 未填密钥只允许在协议和规范化基础地址均未改变时保留。否则旧 Key
        // 可能被无意转发给另一个服务端，必须要求用户重新确认并提交。
        if let Some(id) = &input.id {
            let existing = repo::list_providers(state.db.pool())
                .await
                .map_err(|_| "无法读取已保存的 Provider".to_string())?
                .into_iter()
                .find(|provider| &provider.id == id);
            match existing {
                Some(provider)
                    if provider.api_key_enc.trim().is_empty()
                        || model_catalog::matches_saved_provider_target(
                            &provider,
                            input.dialect,
                            &base_url,
                        ) =>
                {
                    provider.api_key_enc
                }
                Some(_) => {
                    return Err("API 地址或协议已变更，请重新输入 API Key".into());
                }
                None => String::new(),
            }
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

    // 价格只用于本地花费估算。非法值必须在这里拒绝，而不是落库后再被静默忽略。
    for model in &input.models {
        if let Some(price) = &model.price {
            if !price.is_valid() {
                return Err(format!(
                    "模型 {} 的价格必须是 0 或更大的有限数值",
                    model.alias.trim()
                ));
            }
        }
    }
    // 模型级覆盖会直接改写发往上游的请求，必须在保存期整体校验。
    for model in &input.models {
        model
            .validate_upstream_path()
            .map_err(|error| format!("模型 {} 的上游请求路径无效：{error}", model.alias.trim()))?;
        if let Some(overrides) = &model.overrides {
            overrides
                .validate()
                .map_err(|error| format!("模型 {} 的参数覆盖无效：{error}", model.alias.trim()))?;
        }
    }

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
        runtime_id: None,
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
    let defaults = state.config.read().clone().ollama_options;
    let res = state
        .gateway
        .upstream
        .call(
            &p,
            &req,
            &model,
            std::time::Duration::from_secs(30),
            &defaults,
        )
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
    // 配置变更必须清缓存。`reload_providers` 覆盖供应商/模型/价格三类变更，
    // 但那条路走不到这里（本命令改的是 cfg，不是 providers 表）。
    // 漏掉它的症状是「改了策略但答案还是旧的」，而界面显示保存成功。
    state.gateway.cache.invalidate_all();
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
    /// 月度预算，micros。0 = 不限。
    pub monthly_budget_micros: i64,
    /// 预算币种。空 = 没设预算。
    pub budget_currency: String,
    /// 模型白名单。空数组 = 不限。
    pub allowed_models: Vec<String>,
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
            monthly_budget_micros: key.monthly_budget_micros,
            budget_currency: key.budget_currency,
            allowed_models: key.allowed_models,
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
    /// 下面三项都是 `Option`：**不传表示「不改」**。
    /// 用非 Option 的话，前端只想改 label 就必须把预算一起回传，
    /// 漏传一次就把用户设的预算清空了 —— 而界面上看不出任何异常。
    #[serde(default)]
    pub monthly_budget_micros: Option<i64>,
    #[serde(default)]
    pub budget_currency: Option<String>,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
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
        // 新建的 Key 默认不限预算、不限模型：闸门是**opt-in** 的，
        // 新建时不带任何限制，用户设了才生效。
        monthly_budget_micros: 0,
        budget_currency: String::new(),
        allowed_models: Vec::new(),
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

    // 预算三件套从输入进来；没传时沿用现值，避免「只想改 label」把预算清空。
    let budget = (
        input
            .monthly_budget_micros
            .unwrap_or(current.monthly_budget_micros)
            .max(0),
        input
            .budget_currency
            .clone()
            .unwrap_or_else(|| current.budget_currency.clone()),
        input
            .allowed_models
            .clone()
            .unwrap_or_else(|| current.allowed_models.clone()),
    );

    if !repo::update_remote_access_key(
        state.db.pool(),
        &input.id,
        &label,
        input.enabled,
        rpm_limit,
        (budget.0, &budget.1, &budget.2),
    )
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
    // 会话列表 SQL 收敛在 repo 层：那里有真实数据库的集成测试，能挡住
    // 「新增列后漏改 SELECT」这类只在运行时才炸的错。本进程 panic=abort，
    // 一次列名不匹配就会让整个应用退出，因此这个查询必须有测试覆盖。
    let rows = repo::list_sessions_with_counts(state.db.pool(), limit.unwrap_or(50).clamp(1, 200))
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(session, message_count)| SessionView {
            session,
            message_count,
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
    /// 任务卡二 A5：快照**必须带上运行时 id**。
    ///
    /// 漏掉它的后果是：回滚一次快照会把 Provider 的账号型配置
    /// **悄悄清成 NULL** —— 于是它退回 HTTP 直连，带着空 Key
    /// 去连真实上游。而「回滚」是用户为了**恢复**才做的动作，
    /// 出一个「越回滚越坏」的结果最难归因。
    ///
    /// `#[serde(default)]` 让**旧的快照文件**（没有这个键）仍能读 ——
    /// 那时它读出来是 `None`，与旧快照当时的语义一致。
    #[serde(default)]
    runtime_id: Option<String>,
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
            runtime_id: provider.runtime_id.clone(),
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
            runtime_id: None,
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
            Dialect::Responses => "responses",
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
            let price = model
                .price
                .as_ref()
                .filter(|price| price.is_valid())
                .and_then(|price| serde_json::to_string(price).ok());
            let overrides = model
                .overrides
                .as_ref()
                .filter(|overrides| !overrides.is_empty())
                .and_then(|overrides| serde_json::to_string(overrides).ok());
            sqlx::query(
                r#"INSERT INTO models
                     (id, provider_id, alias, upstream, context_window, supports_tools,
                      supports_vision, supports_audio, supports_video, supports_stream,
                      model_type, upstream_path, price_json, overrides_json)
                   VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
            )
            .bind(format!("{}:{}", provider.id, model.alias))
            .bind(&provider.id)
            .bind(&model.alias)
            .bind(&model.upstream)
            .bind(model.context_window)
            .bind(model.supports_tools as i64)
            .bind(model.supports_vision as i64)
            .bind(model.supports_audio as i64)
            .bind(model.supports_video as i64)
            .bind(model.supports_stream as i64)
            .bind(model.model_type.code())
            .bind(&model.upstream_path)
            .bind(price)
            .bind(overrides)
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
    pub spend: SpendOverview,
}

/// 本地花费估算。数据来自 `usage_daily`（按 UTC 自然日聚合），只统计配置了价格的
/// 模型；未配置价格的请求单独计数，不折算成 0 混进合计。
#[derive(Debug, Clone, Serialize)]
pub struct SpendOverview {
    pub today: Vec<SpendBucketView>,
    pub days7: Vec<SpendBucketView>,
    pub days30: Vec<SpendBucketView>,
    /// 近 30 天有 token 消耗但没有价格的请求数
    pub unpriced_requests_30d: i64,
    /// 近 14 天逐日花费
    pub daily: Vec<serde_json::Value>,
    /// 近 30 天按供应商
    pub by_provider: Vec<serde_json::Value>,
    /// 近 30 天按模型
    pub by_model: Vec<serde_json::Value>,
    /// 统计口径说明，供界面原样展示，避免把估算说成账单。
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpendBucketView {
    pub currency: String,
    pub cost: f64,
    pub requests: i64,
}

/// UTC 自然日粒度，与 `usage_daily` 的按天聚合一致。
fn utc_day_cutoff(days_ago: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(days_ago))
        .format("%Y-%m-%d")
        .to_string()
}

async fn spend_overview(pool: &sqlx::SqlitePool) -> Result<SpendOverview, String> {
    let bucket_view = |buckets: Vec<repo::SpendBucket>| {
        buckets
            .into_iter()
            .map(|bucket| SpendBucketView {
                currency: bucket.currency,
                cost: bucket.cost,
                requests: bucket.requests,
            })
            .collect::<Vec<_>>()
    };

    let today = repo::spend_buckets(pool, &utc_day_cutoff(0))
        .await
        .map_err(|e| e.to_string())?;
    let days7 = repo::spend_buckets(pool, &utc_day_cutoff(6))
        .await
        .map_err(|e| e.to_string())?;
    let days30 = repo::spend_buckets(pool, &utc_day_cutoff(29))
        .await
        .map_err(|e| e.to_string())?;
    let unpriced_requests_30d = repo::unpriced_request_count(pool, &utc_day_cutoff(29))
        .await
        .map_err(|e| e.to_string())?;
    let daily = repo::spend_daily(pool, &utc_day_cutoff(13))
        .await
        .map_err(|e| e.to_string())?;
    let by_provider = repo::spend_by_dimension(pool, &utc_day_cutoff(29), false)
        .await
        .map_err(|e| e.to_string())?;
    let by_model = repo::spend_by_dimension(pool, &utc_day_cutoff(29), true)
        .await
        .map_err(|e| e.to_string())?;

    Ok(SpendOverview {
        today: bucket_view(today),
        days7: bucket_view(days7),
        days30: bucket_view(days30),
        unpriced_requests_30d,
        daily,
        by_provider,
        by_model,
        note: "按模型配置的价格 × 实际 token 本地估算，按 UTC 自然日聚合；不是上游账单，请以厂商账单为准。".into(),
    })
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
        spend: spend_overview(state.db.pool()).await?,
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

/* --------------------------- B3 审计检索与导出 --------------------------- */

/// 一页审计记录。`total` 是**命中总数**（不是本页条数），
/// `truncated` 表示「还有更多页」。
#[derive(Debug, Serialize)]
pub struct AuditPage {
    pub rows: Vec<crate::audit::AuditRow>,
    pub total: u64,
    pub truncated: bool,
}

#[tauri::command]
pub async fn query_requests(
    state: State<'_, AppState>,
    filter: crate::audit::RequestFilter,
) -> Result<AuditPage, String> {
    let (rows, total) = repo::query_requests(state.db.pool(), &filter)
        .await
        .map_err(|e| e.to_string())?;
    // 本页最后一条在全集里的位置 < total ⇒ 还有下一页
    // 括号不能省：`rows.len() as u64 < total` 会被解析成泛型实参
    let truncated = u64::from(filter.page_offset()) + (rows.len() as u64) < total;
    Ok(AuditPage {
        rows,
        total,
        truncated,
    })
}

#[derive(Debug, Serialize)]
pub struct AuditExportResult {
    pub written: u64,
    pub path: String,
    /// 伴随的列说明文件路径（只有 CSV 会生成）。
    pub columns_doc: Option<String>,
}

/// 导出审计记录。
///
/// `format` 只认 `jsonl` / `csv`；其余一律报错而不是「默认按 jsonl」——
/// 用户写错格式却拿到一个能打开的文件，会以为导出的就是他要的格式。
#[tauri::command]
pub async fn export_requests(
    state: State<'_, AppState>,
    filter: crate::audit::RequestFilter,
    format: String,
    dest_path: String,
) -> Result<AuditExportResult, String> {
    let fmt = format.trim().to_ascii_lowercase();
    if fmt != "jsonl" && fmt != "csv" {
        return Err(format!("不支持的导出格式 {format:?}，只支持 jsonl 与 csv"));
    }
    let path = std::path::PathBuf::from(dest_path.trim());
    if path.as_os_str().is_empty() {
        return Err("导出路径不能为空".into());
    }

    let rows = repo::query_requests_for_export(state.db.pool(), &filter)
        .await
        .map_err(|e| e.to_string())?;

    let (content, columns_doc_path) = if fmt == "jsonl" {
        (crate::audit::to_jsonl(&rows), None)
    } else {
        let max_attempts = rows.iter().map(|r| r.attempts.len()).max().unwrap_or(0);
        // CSV 没有注释标准，列名来源写成伴随文件而不是塞进注释行
        // （注释行会被解析器当成数据）。
        let mut doc_path = path.clone();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "export".into());
        doc_path.set_file_name(format!("{stem}_columns.md"));
        if let Some(parent) = doc_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        std::fs::write(&doc_path, crate::audit::columns_doc(max_attempts))
            .map_err(|e| format!("写列说明失败：{e}"))?;
        (
            crate::audit::to_csv(&rows),
            Some(doc_path.to_string_lossy().into_owned()),
        )
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    std::fs::write(&path, content).map_err(|e| format!("写导出文件失败：{e}"))?;

    Ok(AuditExportResult {
        written: rows.len() as u64,
        path: path.to_string_lossy().into_owned(),
        columns_doc: columns_doc_path,
    })
}

/* ------------------- D5 可解释性与 Pareto 前沿 ------------------- */

/// D5：质量 × 速度 × 价格的三维前沿视图。
///
/// 三个维度的取值口径**全部在 `Router::pareto_view` 里**（与路由同源），
/// 这里只负责取配置与供应商列表。界面**不重算**任何一维 ——
/// 重算等于把口径复制一份，改了一处另一处就悄悄漂移。
///
/// 时段用 `server.rs` 的那一份实现（峰谷价要生效，就必须与真实计费同一分钟）。
#[tauri::command]
pub async fn capability_pareto(
    state: State<'_, AppState>,
) -> Result<crate::router::pareto::ParetoView, String> {
    let cfg = state.gateway.cfg_snapshot();
    let providers = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    Ok(state.gateway.router.pareto_view(
        &providers,
        &cfg,
        crate::proxy::server::utc_minute_of_day(),
    ))
}

/* ------------------- 任务卡二 A5：账号型上游运行时 ------------------- */

/// 列出全部运行时。
///
/// 返回体里**不带任何凭据** —— 运行时表里现在也没有凭据列，
/// 登录态由各家 CLI 自己管（`~/.codex/auth.json` 那类），
/// 网关**不读也不存**。这是任务卡二的红线之一。
#[tauri::command]
pub async fn list_agent_runtimes(
    state: State<'_, AppState>,
) -> Result<Vec<crate::domain::AgentRuntime>, String> {
    repo::list_agent_runtimes(state.db.pool())
        .await
        .map_err(|e| e.to_string())
}

/// 已注册的适配器。供前端在「新建运行时」时列出可选 kind。
///
/// 返回 `(id, label)` 二元组：`id` 是写进库的值，`label` 是给人看的。
/// 界面上只显示 `label` 会让用户配不出正确的 `kind`，
/// 只显示 `id` 则屏幕上全是 `codex` / `qoder` 这种没有上下文的词。
#[tauri::command]
pub async fn list_agent_adapters(
    state: State<'_, AppState>,
) -> Result<Vec<(String, String)>, String> {
    Ok(state
        .adapters
        .ids()
        .into_iter()
        .filter_map(|id| {
            state
                .adapters
                .get(id)
                .map(|a| (a.id().to_string(), a.label().to_string()))
        })
        .collect())
}

/// 新建或更新一个运行时。
///
/// 校验在 `repo::upsert_agent_runtime` 里（写库前跑），这里只做一层
/// 前置：**`kind` 必须能在注册表里解析出适配器**。
/// 不查的话用户能存下一个 `kind = "codexx"` 的运行时，
/// 而那个错误要等到**请求时**才爆 —— 离操作已经很远。
#[tauri::command]
pub async fn save_agent_runtime(
    state: State<'_, AppState>,
    runtime: crate::domain::AgentRuntime,
) -> Result<(), String> {
    if state.adapters.get(runtime.kind.trim()).is_none() {
        // 错误里带上「有哪些可用的」—— 只说「不认识」用户还得自己去翻
        return Err(format!(
            "未知账号运行时类型：{}（可用的有：{}）",
            runtime.kind,
            state.adapters.ids().join("、")
        ));
    }
    repo::upsert_agent_runtime(state.db.pool(), &runtime)
        .await
        .map_err(|e| e.to_string())
}

/// 删除运行时。
///
/// **被引用时拒绝并报出是谁在用** —— 不做级联删除：
/// 删掉一个还被引用的运行时会留下指向空气的 `provider.runtime_id`，
/// 而那要等到请求时才报「未知账号运行时」，离操作已经很远。
///
/// 也不「顺手把引用它的 Provider 也删掉」：用户点的是「删运行时」，
/// 不是「删那几个供应商」。多删的东西不会自己回来。
#[tauri::command]
pub async fn delete_agent_runtime(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let users = repo::providers_using_runtime(state.db.pool(), &id)
        .await
        .map_err(|e| e.to_string())?;
    if !users.is_empty() {
        return Err(format!(
            "还有 {} 个供应商在用它：{}。请先把它们改成别的上游，或换掉它们的运行时。",
            users.len(),
            users.join("、")
        ));
    }
    let hit = repo::delete_agent_runtime(state.db.pool(), &id)
        .await
        .map_err(|e| e.to_string())?;
    if !hit {
        return Err(format!("没有这个运行时：{id}"));
    }
    Ok(())
}

/* --------------------------- D2 能力集导出 / 导入 --------------------------- */

/// 导出**全部**模型的多来源能力账本。
///
/// 形态：`{ "<provider_id>/<alias>": { ...CapabilitySet... }, ... }`
///
/// 为什么要能带走（卡片原文）：「用户整理好一套能力数据后要能带走，
/// 也能在另一台机器上复用。这条不做好，30 个模型逐个手填就没人愿意用了」。
///
/// 用 `BTreeMap` 语义（按 key 排序）而不是哈希序：同样数据导两次要逐字节相同，
/// 否则 diff 与快照都没法用。
#[tauri::command]
pub async fn export_capabilities(state: State<'_, AppState>) -> Result<String, String> {
    let providers = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    let mut out: std::collections::BTreeMap<String, crate::capability::CapabilitySet> =
        std::collections::BTreeMap::new();
    for provider in providers {
        for model in &provider.models {
            if let Some(ledger) =
                repo::read_capability_ledger(state.db.pool(), &provider.id, &model.alias)
                    .await
                    .map_err(|e| e.to_string())?
            {
                out.insert(format!("{}/{}", provider.id, model.alias), ledger);
            }
        }
    }
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// 导入结果。**把跳过的东西回给用户看** ——
/// 静默跳过会让他以为数据齐了。
#[derive(Debug, Serialize)]
pub struct ImportCapabilitiesResult {
    /// 实际写进库的模型数。
    pub written: usize,
    /// 本机没有、因而**没有**写入的键。
    pub skipped: Vec<String>,
    /// 认不出来的维度名（新版本写的文件在老版本里读）。
    pub skipped_dimensions: Vec<String>,
    /// 认不出来的来源名。
    pub skipped_sources: Vec<String>,
}

/// 导入能力账本。返回写入与跳过的明细。
///
/// **只写能力，不动别的字段** —— 不碰价格、不碰启用状态。
/// 导入一份别人整理的能力集不该顺手改掉本机的定价。
#[tauri::command]
pub async fn import_capabilities(
    state: State<'_, AppState>,
    payload: String,
) -> Result<ImportCapabilitiesResult, String> {
    let value: serde_json::Value =
        serde_json::from_str(&payload).map_err(|e| format!("不是合法 JSON：{e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| "顶层必须是一个对象（形如 {\"provider/alias\": {...}}）".to_string())?;

    let providers = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| e.to_string())?;
    // 先把「这台机器上有哪些模型」查出来 —— 导入一份来自别的机器的能力集时，
    // 里面必然有本机没有的模型，那些要如实报出来而不是静默丢弃。
    let known: std::collections::BTreeSet<String> = providers
        .iter()
        .flat_map(|p| {
            p.models
                .iter()
                .map(move |m| format!("{}/{}", p.id, m.alias))
        })
        .collect();

    let mut written = 0usize;
    let mut skipped = Vec::new();
    let mut skipped_dimensions = Vec::new();
    let mut skipped_sources = Vec::new();

    for (key, raw) in obj {
        if !known.contains(key) {
            skipped.push(key.clone());
            continue;
        }
        let Some((provider_id, alias)) = key.split_once('/') else {
            skipped.push(key.clone());
            continue;
        };
        let (ledger, report) = crate::capability::CapabilitySet::import_json(&raw.to_string())?;
        skipped_dimensions.extend(report.skipped_dimensions);
        skipped_sources.extend(report.skipped_sources);
        let hit = repo::write_capability_ledger(state.db.pool(), provider_id, alias, &ledger)
            .await
            .map_err(|e| e.to_string())?;
        if hit {
            written += 1;
        } else {
            skipped.push(key.clone());
        }
    }

    // 导入改了**路由实际用的定值** ⇒ 两把锁都要刷（CLAUDE.md 铁律 7：
    // 只改 AppState.config 会让界面显示「保存成功」而运行时毫无变化）。
    state
        .gateway
        .reload_providers()
        .await
        .map_err(|e| e.to_string())?;

    Ok(ImportCapabilitiesResult {
        written,
        skipped,
        skipped_dimensions,
        skipped_sources,
    })
}

/* --------------------------- CLI 工具接管（可选） --------------------------- */

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileAttributesW, ReplaceFileW, FILE_ATTRIBUTE_ENCRYPTED, REPLACEFILE_WRITE_THROUGH,
};

/// 单个 CLI 配置写入的可审计结果。已有文件必定给出已验证的备份路径；
/// 新建文件没有原始内容，因此 `backup_path` 为 `None`。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverStatus {
    Updated,
    Created,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TakeoverResult {
    pub client: String,
    pub path: String,
    pub backup_path: Option<String>,
    pub status: TakeoverStatus,
}

struct PreparedTakeover {
    client: &'static str,
    path: std::path::PathBuf,
    original: Option<Vec<u8>>,
    updated: Vec<u8>,
    backup_path: Option<std::path::PathBuf>,
}

impl PreparedTakeover {
    fn result(&self) -> TakeoverResult {
        TakeoverResult {
            client: self.client.to_string(),
            path: self.path.display().to_string(),
            backup_path: self
                .backup_path
                .as_ref()
                .map(|path| path.display().to_string()),
            status: if self.original.is_some() {
                TakeoverStatus::Updated
            } else {
                TakeoverStatus::Created
            },
        }
    }
}

/* --------------------------- CLI 工具检测与更新 --------------------------- */

/// 一次检测结果 + 最新版本信息。界面只消费这一种结构，检测与查更新共用。
#[derive(Debug, Clone, Serialize)]
pub struct CliToolReport {
    #[serde(flatten)]
    pub tool: crate::cli_tools::CliToolStatus,
    pub latest_version: Option<String>,
    /// 已安装且版本与 latest 不同（含预发布），界面据此提示「可更新」。
    pub update_available: bool,
    /// 查询最新版本失败时的原因；不影响已检测到的本机信息。
    pub check_error: Option<String>,
}

/// 检测本机 CLI。只读，不修改任何文件；`None` 表示不联网查询最新版本。
#[tauri::command]
pub async fn detect_cli_tools() -> Result<Vec<CliToolReport>, String> {
    Ok(detect_reports(None).await)
}

/// 检测 + 查询最新版本。registry 查询失败只记录原因，不把整个检测判为失败。
#[tauri::command]
pub async fn detect_cli_tools_with_updates(
    state: State<'_, AppState>,
) -> Result<Vec<CliToolReport>, String> {
    let proxy = state.config.read().http_proxy.clone();
    Ok(detect_reports(Some(proxy.as_deref())).await)
}

/// `updates` 为 `None` 时只做本机检测（不发起任何网络请求）；为 `Some` 时按给定的
/// 代理设置查询 npm registry。两个按钮的行为必须与标签一致：真机验证时发现
/// 「检测本机 CLI」也会联网，那属于标签与行为不符。
///
/// npm 类工具无论本机是否已安装都查询最新版本：未安装时要让用户看到「将安装哪个版本」，
/// 已安装时用于提示可更新。官方脚本始终安装最新版，因此不查询、不臆断版本。
async fn detect_reports(updates: Option<Option<&str>>) -> Vec<CliToolReport> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    let extra = crate::cli_tools::well_known_dirs();
    let mut reports = Vec::new();
    for spec in crate::cli_tools::TOOLS {
        let tool = crate::cli_tools::detect(spec, &path_env, &extra).await;
        let (latest_version, check_error) = match (updates, spec.source.package()) {
            (Some(proxy), Some(package)) => {
                match crate::cli_tools::latest_version(package, proxy).await {
                    Ok(version) => (Some(version), None),
                    Err(error) => (None, Some(error)),
                }
            }
            _ => (None, None),
        };
        let update_available = match (&tool.version, &latest_version) {
            (Some(current), Some(latest)) => current != latest,
            _ => false,
        };
        reports.push(CliToolReport {
            tool,
            latest_version,
            update_available,
            check_error,
        });
    }
    reports
}

/// 安装或更新指定的 CLI。命令来自内置常量（不接受用户输入），执行前界面会展示确切命令。
#[tauri::command]
pub async fn install_cli_tool(id: String) -> Result<String, String> {
    let spec = crate::cli_tools::TOOLS
        .iter()
        .find(|spec| spec.id == id)
        .ok_or_else(|| format!("不支持安装 {id}"))?;
    crate::cli_tools::install(spec).await
}

/* --------------------------- 桌宠（Petdex）与 AI 进程监控 --------------------------- */

/// 桌宠状态：由网关最近活动 + 本机 AI 软件进程 + 工具任务日志共同决定。
#[derive(Debug, Clone, Serialize)]
pub struct PetStatus {
    /// 综合状态（网关与任务取最高优先级）："idle" | "working" | "error"
    pub status: String,
    /// 状态原因（供界面与悬浮提示展示，不猜测未观测到的信息）。
    pub reason: String,
    /// 网关自身的状态，单独保留便于界面区分"网关出错"与"任务出错"。
    pub gateway_status: String,
    pub requests_last_minute: i64,
    pub failed_last_minute: i64,
    pub installed_pets: Vec<crate::petdex::InstalledPet>,
    pub ai_processes: Vec<crate::petdex::AiProcess>,
    /// 工具会话日志中检测到的最近任务（错误优先、其次最近活动）。
    pub active_tasks: Vec<crate::petdex::DetectedTask>,
    pub pet_window_open: bool,
}

/// 判定窗口：最近 60 秒内的事件参与状态判定。
const PET_ACTIVITY_WINDOW_SECS: i64 = 60;

#[tauri::command]
pub async fn get_pet_status(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<PetStatus, String> {
    let now = chrono::Utc::now().timestamp();
    let since = now - PET_ACTIVITY_WINDOW_SECS;
    let summary = sqlx::query(
        r#"SELECT COUNT(*) AS total,
                  SUM(CASE WHEN status IS NULL OR status >= 400 THEN 1 ELSE 0 END) AS failed
           FROM requests WHERE ts >= ?"#,
    )
    .bind(since)
    .fetch_one(state.db.pool())
    .await
    .map_err(|error| format!("读取请求活动失败：{error}"))?;
    let total: i64 = summary.get("total");
    let failed: i64 = summary.get::<Option<i64>, _>("failed").unwrap_or(0);

    // 最近一条请求决定 error 优先：只有失败之后还没有成功，才展示错误状态。
    let last = sqlx::query("SELECT ts, status, error FROM requests ORDER BY id DESC LIMIT 1")
        .fetch_optional(state.db.pool())
        .await
        .map_err(|error| format!("读取最近请求失败：{error}"))?;
    let last_request = last.map(|row| {
        (
            row.get::<i64, _>("ts"),
            row.get::<Option<i64>, _>("status"),
            row.get::<Option<String>, _>("error"),
        )
    });
    let (status, mut reason) =
        crate::petdex::derive_activity_status(total, last_request, since, PET_ACTIVITY_WINDOW_SECS);

    let ai_processes = crate::petdex::list_ai_processes().await;
    if status == "idle" && !ai_processes.is_empty() {
        // 同一软件常有多个进程（Electron 多进程），按软件数报更有意义。
        let distinct_tools = ai_processes
            .iter()
            .map(|process| process.tool_id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len();
        reason = format!(
            "{distinct_tools} 个 AI 软件正在运行（共 {} 个进程）",
            ai_processes.len()
        );
    }

    // 任务日志优先：出错的任务压过"空闲"说明，运行中的任务让宠物进入工作态。
    let active_tasks = crate::petdex::list_tasks();
    let gateway_status = status.clone();
    let (status, reason) = crate::petdex::combine_pet_status(&status, &reason, &active_tasks);

    Ok(PetStatus {
        status,
        reason,
        gateway_status,
        requests_last_minute: total,
        failed_last_minute: failed,
        installed_pets: crate::petdex::list_installed(),
        ai_processes,
        active_tasks,
        pet_window_open: app.get_webview_window("pet").is_some(),
    })
}

/// 读取宠物资源（精灵图以 data URL 返回）。
#[tauri::command]
pub async fn get_pet_asset(slug: String) -> Result<crate::petdex::PetAsset, String> {
    crate::petdex::load_asset(&slug)
}

/// 显示桌宠窗口；已存在时直接显示并置顶。
#[tauri::command]
pub async fn open_pet_window(app: tauri::AppHandle) -> Result<(), String> {
    crate::pet_window::ensure_pet_window(&app)
}

#[tauri::command]
pub async fn close_pet_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("pet") {
        window
            .close()
            .map_err(|error| format!("关闭桌宠窗口失败：{error}"))?;
    }
    Ok(())
}

/// 调整宠物本体大小（1.00× = 120×130，最大 3.00×）。返回窗口实际布局。
#[tauri::command]
pub async fn set_pet_window_size(
    app: tauri::AppHandle,
    scale: f64,
) -> Result<crate::pet_window::PetWindowLayout, String> {
    crate::pet_window::set_pet_window_size(&app, scale)
}

/// 展开 / 收起当前任务与 AI 软件信息面板。
#[tauri::command]
pub async fn set_pet_window_expanded(
    app: tauri::AppHandle,
    expanded: bool,
) -> Result<crate::pet_window::PetWindowLayout, String> {
    crate::pet_window::set_pet_window_expanded(&app, expanded)
}

/// 隐藏 / 恢复宠物旁边的任务气泡；隐藏时窗口缩回宠物本体，避免透明区域挡住桌面点击。
#[tauri::command]
pub async fn set_pet_window_bubble_hidden(
    app: tauri::AppHandle,
    hidden: bool,
) -> Result<crate::pet_window::PetWindowLayout, String> {
    crate::pet_window::set_pet_window_bubble_hidden(&app, hidden)
}

/// 读取桌宠窗口当前布局（缩放比例、面板展开状态、逻辑尺寸）。
#[tauri::command]
pub async fn get_pet_window_layout(
    app: tauri::AppHandle,
) -> Result<crate::pet_window::PetWindowLayout, String> {
    Ok(crate::pet_window::pet_window_layout(&app))
}

/// 任务数量变化后重算桌宠窗口布局：气泡堆叠高度必须跟着任务条数变化。
#[tauri::command]
pub async fn refresh_pet_window_layout(
    app: tauri::AppHandle,
) -> Result<crate::pet_window::PetWindowLayout, String> {
    crate::pet_window::refresh_pet_window(&app)
}

/// 前端上报可见气泡数量与堆叠状态：关闭单个气泡 / 悬浮展开时窗口高度随之变化。
#[tauri::command]
pub async fn set_pet_bubbles(
    app: tauri::AppHandle,
    bubble_count: usize,
    bubbles_expanded: bool,
) -> Result<crate::pet_window::PetWindowLayout, String> {
    crate::pet_window::set_pet_bubbles(&app, bubble_count, bubbles_expanded)
}

/// 弹出桌宠的原生右键菜单（宠物列表 + 面板 / 主窗口 / 暂停 / 隐藏）。
/// 用系统菜单而不是页面内菜单：即使面板收起，菜单仍然可用。
#[tauri::command]
pub async fn show_pet_menu(
    app: tauri::AppHandle,
    current_slug: Option<String>,
    paused: bool,
    expanded: bool,
) -> Result<(), String> {
    let pets = crate::petdex::list_installed();
    crate::pet_window::show_pet_menu(&app, current_slug.as_deref(), &pets, paused, expanded)
}

/// 打开并聚焦主窗口（桌宠点击跳转）。`section` 由前端映射到具体页面。
#[tauri::command]
pub async fn focus_main_window(
    app: tauri::AppHandle,
    section: Option<String>,
) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口不存在".to_string())?;
    window.show().map_err(|error| error.to_string())?;
    window.unminimize().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())?;
    if let Some(section) = section.filter(|value| !value.is_empty()) {
        // 前端订阅该事件后切换到对应页面；此处只在窗口就绪后发送。
        let _ = app.emit("llm-gateway-navigate", section);
    }
    Ok(())
}

/// 打开指定的 AI 任务。
/// Codex 使用官方 codex://threads/<id> 深链；Qoder 没有任务级协议，退化为聚焦 Qoder IDE。
#[tauri::command]
pub async fn open_ai_task(tool_id: String, session_id: String) -> Result<String, String> {
    let tasks = crate::petdex::list_tasks();
    let task = tasks
        .iter()
        .find(|task| task.tool_id == tool_id && task.session_id == session_id)
        .ok_or_else(|| "任务已不在当前监控窗口内；请刷新后重试".to_string())?;

    if let Some(link) = task.deep_link.as_deref() {
        tauri_plugin_opener::open_url(link, None::<&str>)
            .map_err(|error| format!("打开任务失败：{error}"))?;
        return Ok(format!("已打开 [{}] {}。", task.source_label, task.title));
    }

    let focus_result = if tool_id.starts_with("qoder") {
        match crate::petdex::focus_tool_window("qoder_ide").await {
            Ok(message) => Ok(message),
            Err(_) => crate::petdex::focus_tool_window("qoder").await,
        }
    } else {
        crate::petdex::focus_tool_window(&tool_id).await
    };
    focus_result
        .map(|message| {
            format!(
                "{message}。当前任务：[{}] {}",
                task.source_label, task.title
            )
        })
        .map_err(|error| {
            format!(
                "无法打开任务“{}”：{error}。可使用「项目」打开任务目录。",
                task.title
            )
        })
}

/// 将某个 AI 软件的顶层窗口切到前台（工具标识必须来自监控结果）。
#[tauri::command]
pub async fn focus_ai_tool(tool_id: String) -> Result<String, String> {
    crate::petdex::focus_tool_window(&tool_id).await
}

/// 打开检测到任务的项目目录。只接受真实存在的目录，不执行路径中的内容。
#[tauri::command]
pub async fn open_task_project(path: String) -> Result<String, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("该任务没有可用的项目路径".to_string());
    }
    let directory = std::path::PathBuf::from(trimmed);
    let metadata =
        std::fs::metadata(&directory).map_err(|error| format!("项目目录不可访问：{error}"))?;
    if !metadata.is_dir() {
        return Err(format!("项目路径不是目录：{}", directory.display()));
    }
    tauri_plugin_opener::open_path(&directory, None::<&str>)
        .map_err(|error| format!("打开项目目录失败：{error}"))?;
    Ok(format!("已打开项目目录：{}", directory.display()))
}

/// 结束某个 AI 软件当前检测到的全部进程（工具标识必须来自监控结果）。
#[tauri::command]
pub async fn stop_ai_tool(tool_id: String) -> Result<String, String> {
    crate::petdex::stop_tool_processes(&tool_id).await
}

/// 查询 Petdex 商店列表（返回 CLI 原始输出，界面按原样展示）。
#[tauri::command]
pub async fn petdex_catalog() -> Result<String, String> {
    crate::petdex::petdex_list().await
}

/// 通过官方 Petdex CLI 安装宠物。
#[tauri::command]
pub async fn petdex_install_pet(slug: String) -> Result<String, String> {
    crate::petdex::petdex_install(&slug).await
}

/// 网关连通性自检：确认服务在监听，并用统一 Key 发一次最小请求走通端到端链路。
/// 结果用于「一键配置后确保可用」，因此失败必须给出可操作的原因。
#[derive(Debug, Clone, Serialize)]
pub struct SelfCheckResult {
    pub healthy: bool,
    pub base_url: String,
    pub routed_via: Option<String>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn run_gateway_self_check(state: State<'_, AppState>) -> Result<SelfCheckResult, String> {
    let cfg = state.config.read().clone();
    let base_url = cfg.base_url();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    let health = client.get(format!("{base_url}/healthz")).send().await;
    match health {
        Ok(response) if response.status().is_success() => {}
        Ok(response) => {
            return Ok(SelfCheckResult {
                healthy: false,
                base_url,
                routed_via: None,
                latency_ms: 0,
                error: Some(format!("健康检查返回 HTTP {}", response.status().as_u16())),
            })
        }
        Err(error) => {
            return Ok(SelfCheckResult {
                healthy: false,
                base_url,
                routed_via: None,
                latency_ms: 0,
                error: Some(format!("无法连接网关服务：{error}")),
            })
        }
    }

    let started = std::time::Instant::now();
    let body = serde_json::json!({
        "model": "auto",
        "messages": [{ "role": "user", "content": "回复「ok」即可，不要展开。" }],
        "max_tokens": 8,
        "stream": false,
    });
    let response = client
        .post(format!("{base_url}/v1/chat/completions"))
        .bearer_auth(&cfg.unified_key)
        .json(&body)
        .send()
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match response {
        Ok(response) => {
            let routed_via = response
                .headers()
                .get("x-routed-via")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if response.status().is_success() {
                Ok(SelfCheckResult {
                    healthy: true,
                    base_url,
                    routed_via,
                    latency_ms,
                    error: None,
                })
            } else {
                let status = response.status().as_u16();
                let detail = response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|value| {
                        value
                            .get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| "上游未返回错误详情".into());
                Ok(SelfCheckResult {
                    healthy: false,
                    base_url,
                    routed_via,
                    latency_ms,
                    error: Some(format!("网关返回 HTTP {status}：{detail}")),
                })
            }
        }
        Err(error) => Ok(SelfCheckResult {
            healthy: false,
            base_url,
            routed_via: None,
            latency_ms,
            error: Some(format!("请求网关失败：{error}")),
        }),
    }
}

/// 将已支持的 CLI 指向本地网关；不支持的 CLI 会在读取任何配置前拒绝。
/// 已有配置先创建并验证独立备份，失败时绝不改写原文件。
#[tauri::command]
pub async fn apply_takeover(state: State<'_, AppState>) -> Result<Vec<TakeoverResult>, String> {
    let cfg = state.config.read().clone();
    ensure_supported_takeover_selection(cfg.takeover.gemini_cli)?;
    if !cfg.takeover.claude_code
        && !cfg.takeover.codex
        && !cfg.takeover.gemini_cli
        && !cfg.takeover.opencode
        && !cfg.takeover.crush
    {
        return Ok(Vec::new());
    }

    let base = cfg.base_url();
    let home = dirs::home_dir().ok_or_else(|| "找不到用户目录".to_string())?;
    let mut prepared = Vec::new();

    if cfg.takeover.claude_code {
        prepared.push(prepare_claude_takeover(
            &home.join(".claude").join("settings.json"),
            &base,
            &cfg.unified_key,
        )?);
    }

    if cfg.takeover.codex {
        prepared.push(prepare_codex_takeover(
            &home.join(".codex").join("config.toml"),
            &base,
            &cfg.unified_key,
        )?);
    }

    if cfg.takeover.opencode {
        prepared.push(prepare_opencode_takeover(
            &home.join(".config").join("opencode").join("opencode.json"),
            &format!("{base}/v1"),
            &cfg.unified_key,
        )?);
    }

    if cfg.takeover.gemini_cli {
        // 写 `.env` 而不是 `settings.json` —— 理由见 `prepare_gemini_takeover`。
        prepared.push(prepare_gemini_takeover(
            &home.join(".gemini").join(".env"),
            &base,
            &cfg.unified_key,
        )?);
    }

    if cfg.takeover.crush {
        prepared.push(prepare_crush_takeover(
            &home.join(".config").join("crush").join("crushrc"),
            &format!("{base}/v1"),
            &cfg.unified_key,
        )?);
    }

    apply_prepared_takeovers(prepared)
}

fn ensure_supported_takeover_selection(gemini_cli: bool) -> Result<(), String> {
    let _ = gemini_cli;
    // 【2026-10-06】Gemini CLI 原先在这里被拒，理由是「网关尚未提供 Gemini 入站协议」。
    // 那条已经解开（C2 落了 `/v1beta/models/{m}:generateContent` 等）。
    //
    // 现在走 `~/.gemini/.env` —— **官方文档承认的环境文件机制**
    // （configuration.html：「Variables from `.gemini/.env` files are never excluded」）。
    // 为什么不是 `settings.json`：逐条核对过官方 schema，分类是
    // general / output / ui / ide / privacy / model / tools / mcp / security /
    // advanced / mcpServers / telemetry，**没有 baseUrl 这一项**；
    // 网上流传的 `{"baseUrl": ...}` 骨架写进去会被静默忽略。
    //
    // 【已如实告知、仍然选择这条】`CODE_ASSIST_ENDPOINT` 在官方文档里的描述是
    // 「the endpoint for the code assist server」，指向的是 **Cloud Code Assist
    // 后端**，而本网关提供的是公开 Gemini API 的 `generateContent` 形状。
    // 两者**不保证握手成功** —— 这一点写进下面的注释与前端文案，
    // 不让用户以为开了就一定通。
    Ok(())
}

fn prepare_claude_takeover(
    path: &std::path::Path,
    base_url: &str,
    unified_key: &str,
) -> Result<PreparedTakeover, String> {
    prepare_takeover_file("Claude Code", path, |contents| {
        let mut value: serde_json::Value = match contents {
            Some(contents) => serde_json::from_str(contents)
                .map_err(|_| "Claude Code settings.json 不是有效 JSON；未改写原文件".to_string())?,
            None => serde_json::json!({}),
        };
        let root = value.as_object_mut().ok_or_else(|| {
            "Claude Code settings.json 根节点必须是对象；未改写原文件".to_string()
        })?;
        let env = root
            .entry("env")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| {
                "Claude Code settings.json 的 env 必须是对象；未改写原文件".to_string()
            })?;
        env.insert("ANTHROPIC_BASE_URL".into(), serde_json::json!(base_url));
        env.insert(
            "ANTHROPIC_AUTH_TOKEN".into(),
            serde_json::json!(unified_key),
        );
        // 固定 Claude Code 使用 Bearer；网关仍会正常校验其它合法鉴权形式。
        env.insert("ANTHROPIC_API_KEY".into(), serde_json::json!(""));
        serde_json::to_string_pretty(&value)
            .map_err(|_| "Claude Code settings.json 序列化失败；未改写原文件".to_string())
    })
}

fn prepare_codex_takeover(
    path: &std::path::Path,
    base_url: &str,
    unified_key: &str,
) -> Result<PreparedTakeover, String> {
    prepare_takeover_file("Codex CLI", path, |contents| {
        merge_codex_takeover_config(
            contents.unwrap_or(""),
            &format!("{base_url}/v1"),
            unified_key,
        )
    })
}

fn prepare_opencode_takeover(
    path: &std::path::Path,
    gateway_url: &str,
    unified_key: &str,
) -> Result<PreparedTakeover, String> {
    prepare_takeover_file("OpenCode", path, |contents| {
        let mut value: serde_json::Value = match contents {
            Some(contents) => serde_json::from_str(contents)
                .map_err(|_| "OpenCode opencode.json 不是有效 JSON；未改写原文件".to_string())?,
            None => serde_json::json!({}),
        };
        let root = value
            .as_object_mut()
            .ok_or_else(|| "OpenCode opencode.json 根节点必须是对象；未改写原文件".to_string())?;
        root.entry("$schema")
            .or_insert_with(|| serde_json::json!("https://opencode.ai/config.json"));
        let providers = root
            .entry("provider")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| {
                "OpenCode opencode.json 的 provider 必须是对象；未改写原文件".to_string()
            })?;
        providers.insert(
            "llm-gateway".into(),
            serde_json::json!({
                "npm": "@ai-sdk/openai-compatible",
                "name": "LLM Gateway",
                "options": {
                    "baseURL": gateway_url,
                    "apiKey": unified_key,
                },
                "models": {
                    "auto": { "name": "LLM Gateway auto" },
                },
            }),
        );
        root.insert("model".into(), serde_json::json!("llm-gateway/auto"));
        root.insert("small_model".into(), serde_json::json!("llm-gateway/auto"));
        serde_json::to_string_pretty(&value)
            .map_err(|_| "OpenCode opencode.json 序列化失败；未改写原文件".to_string())
    })
}

fn prepare_crush_takeover(
    path: &std::path::Path,
    gateway_url: &str,
    unified_key: &str,
) -> Result<PreparedTakeover, String> {
    prepare_takeover_file("Crush", path, |contents| {
        merge_crush_takeover_config(contents.unwrap_or(""), gateway_url, unified_key)
    })
}

const CRUSH_TAKEOVER_BEGIN: &str = "# >>> llm-gateway takeover >>>";
const CRUSH_TAKEOVER_END: &str = "# <<< llm-gateway takeover <<<";
fn merge_crush_takeover_config(
    contents: &str,
    gateway_url: &str,
    unified_key: &str,
) -> Result<String, String> {
    let block = format!(
        "{CRUSH_TAKEOVER_BEGIN}\nprovider add llm-gateway --name {} --type openai-compat --base-url {} --api-key {}\nmodel add llm-gateway/auto --name {}\nmodel large llm-gateway/auto\nmodel small llm-gateway/auto\n{CRUSH_TAKEOVER_END}",
        shell_single_quote("LLM Gateway"),
        shell_single_quote(gateway_url),
        shell_single_quote(unified_key),
        shell_single_quote("LLM Gateway auto"),
    );

    let start = contents.find(CRUSH_TAKEOVER_BEGIN);
    let end = contents.find(CRUSH_TAKEOVER_END);
    match (start, end) {
        (None, None) => Ok({
            if contents.trim().is_empty() {
                format!("{block}\n")
            } else {
                format!("{}\n\n{block}\n", contents.trim_end())
            }
        }),
        (Some(start), Some(end)) if end >= start => Ok({
            let end = end + CRUSH_TAKEOVER_END.len();
            format!("{}{}{}", &contents[..start], block, &contents[end..])
        }),
        _ => Err("Crush crushrc 的接管标记不完整；未改写原文件".into()),
    }
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// 只改 Codex 选择的 provider 与 `llm_gateway` 专用表，保留其它 provider 的语义字段。
fn merge_codex_takeover_config(
    contents: &str,
    gateway_url: &str,
    unified_key: &str,
) -> Result<String, String> {
    let mut original: toml::Value = toml::from_str(contents)
        .map_err(|_| "Codex config.toml 不是有效 TOML；未改写原文件".to_string())?;
    let root = original
        .as_table_mut()
        .ok_or_else(|| "Codex config.toml 根节点必须是表；未改写原文件".to_string())?;
    // 旧接管版本曾写过无效的顶层 base_url；自定义 provider 使用表内 base_url。
    root.remove("base_url");
    root.insert("model".into(), toml::Value::String("auto".into()));
    root.insert(
        "model_provider".into(),
        toml::Value::String("llm_gateway".into()),
    );
    let providers = root
        .entry("model_providers")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| "Codex config.toml 的 model_providers 必须是表；未改写原文件".to_string())?;
    let gateway = providers
        .entry("llm_gateway")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .ok_or_else(|| {
            "Codex config.toml 的 model_providers.llm_gateway 必须是表；未改写原文件".to_string()
        })?;
    remove_codex_gateway_auth_conflicts(gateway)?;
    gateway.insert("name".into(), toml::Value::String("LLM Gateway".into()));
    gateway.insert("base_url".into(), toml::Value::String(gateway_url.into()));
    gateway.insert("wire_api".into(), toml::Value::String("responses".into()));
    gateway.insert(
        "experimental_bearer_token".into(),
        toml::Value::String(unified_key.into()),
    );
    gateway.insert("requires_openai_auth".into(), toml::Value::Boolean(false));

    toml::to_string_pretty(&original)
        .map_err(|_| "Codex config.toml 序列化失败；未改写原文件".to_string())
}

/// 本地统一 Key 与旧 provider 鉴权来源不能并存；只处理接管专用的表。
fn remove_codex_gateway_auth_conflicts(gateway: &mut toml::Table) -> Result<(), String> {
    gateway.remove("auth");
    gateway.remove("env_key");
    gateway.remove("env_key_instructions");

    for header_table_name in ["http_headers", "env_http_headers"] {
        let Some(headers) = gateway.get_mut(header_table_name) else {
            continue;
        };
        let headers = headers.as_table_mut().ok_or_else(|| {
            format!(
                "Codex config.toml 的 model_providers.llm_gateway.{header_table_name} 必须是表；未改写原文件"
            )
        })?;
        let authorization_keys: Vec<String> = headers
            .keys()
            .filter(|name| name.eq_ignore_ascii_case("authorization"))
            .cloned()
            .collect();
        for name in authorization_keys {
            headers.remove(&name);
        }
    }
    Ok(())
}

/// Gemini CLI 的接管写进 `~/.gemini/.env`（环境文件），不是 `settings.json`。
///
/// 【为什么不是 settings.json】逐条核对过官方 schema
/// （google-gemini.github.io/gemini-cli/docs/get-started/configuration.html）：
/// 分类是 general / output / ui / ide / privacy / model / tools / mcp /
/// security / advanced / mcpServers / telemetry，**没有端点配置项**。
/// 写了会被静默忽略 —— 正是 CLAUDE.md 第 9 条禁止的「开着没反应的开关」。
/// 而 `.gemini/.env` 是文档明确承认的（「Variables from `.gemini/.env` files
/// are never excluded」）。
///
/// 【两个地址键都写，且标明来路】
/// - `CODE_ASSIST_ENDPOINT` —— 官方文档环境变量表里列出的那个，
///   描述为「the endpoint for the code assist server」
/// - `GOOGLE_GEMINI_BASE_URL` —— 本仓库先前那版 `#[cfg(test)]` 实现用的键，
///   官方文档的环境变量表里**没有**它
///
/// 两个都写不等于两个都对：它们是互斥的猜测，其中必然有一个不生效。
/// 之所以都留着，是因为**实测哪条通需要真装一次 Gemini CLI**，
/// 而那不在本卡的验证预算里。等实测出结论后删掉错的那个 ——
/// 在那之前宁可冗余，也不要因为选错而让用户看到「配置写了但没反应」。
fn prepare_gemini_takeover(
    path: &std::path::Path,
    base_url: &str,
    unified_key: &str,
) -> Result<PreparedTakeover, String> {
    for value in [base_url, unified_key] {
        if value.contains('\n') || value.contains('\r') {
            // 环境文件是行导向的，值里带换行会注入一个新变量 ——
            // 与 HTTP 头注入同一类问题。统一 key 由网关生成、base_url 由配置来，
            // 两者都不该有换行，所以这里是「不可能发生」的兜底断言而不是清洗。
            return Err("Gemini CLI 接管的值不能包含换行".to_string());
        }
    }
    prepare_takeover_file("Gemini CLI", path, |contents| {
        let base = contents.unwrap_or("");
        let base = replace_dotenv_value(base, "CODE_ASSIST_ENDPOINT", base_url);
        let base = replace_dotenv_value(&base, "GOOGLE_GEMINI_BASE_URL", base_url);
        Ok(replace_dotenv_value(&base, "GEMINI_API_KEY", unified_key))
    })
}

fn prepare_takeover_file<F>(
    client: &'static str,
    path: &std::path::Path,
    transform: F,
) -> Result<PreparedTakeover, String>
where
    F: FnOnce(Option<&str>) -> Result<String, String>,
{
    let original = read_takeover_source(path, client)?;
    let contents = original
        .as_deref()
        .map(|bytes| {
            std::str::from_utf8(bytes).map_err(|_| {
                format!(
                    "{client} 配置不是 UTF-8 文本；未改写原文件：{}",
                    path.display()
                )
            })
        })
        .transpose()?;
    let updated = transform(contents)?.into_bytes();
    Ok(PreparedTakeover {
        client,
        path: path.to_path_buf(),
        original,
        updated,
        backup_path: None,
    })
}

fn read_takeover_source(path: &std::path::Path, client: &str) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(format!(
                    "{client} 配置不是普通文件，未写入：{}",
                    path.display()
                ));
            }
            std::fs::read(path).map(Some).map_err(|error| {
                format!(
                    "读取 {client} 配置失败，未写入：{}: {error}",
                    path.display()
                )
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "检查 {client} 配置失败，未写入：{}: {error}",
            path.display()
        )),
    }
}

/// 先为所有已有文件建立并验证备份；只有全部成功后才开始写入。
fn apply_prepared_takeovers(
    mut prepared: Vec<PreparedTakeover>,
) -> Result<Vec<TakeoverResult>, String> {
    for index in 0..prepared.len() {
        let (path, client, original) = {
            let item = &prepared[index];
            (item.path.clone(), item.client, item.original.clone())
        };
        if let Err(error) = ensure_takeover_parent(&path, client) {
            return Err(with_takeover_backup_paths(&prepared, error));
        }
        if let Some(original) = original.as_deref() {
            match create_verified_takeover_backup(&path, original, client) {
                Ok(backup) => prepared[index].backup_path = Some(backup),
                Err(error) => return Err(with_takeover_backup_paths(&prepared, error)),
            }
        }
    }

    let mut results = Vec::with_capacity(prepared.len());
    for item in &prepared {
        if let Err(error) = write_prepared_takeover(item) {
            return Err(with_takeover_backup_paths(&prepared, error));
        }
        results.push(item.result());
    }
    Ok(results)
}

fn ensure_takeover_parent(path: &std::path::Path, client: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{client} 配置路径无父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "创建 {client} 配置目录失败，未写入：{}: {error}",
            parent.display()
        )
    })
}

fn create_verified_takeover_backup(
    path: &std::path::Path,
    original: &[u8],
    client: &str,
) -> Result<std::path::PathBuf, String> {
    create_verified_takeover_backup_with(path, original, client, write_new_synced_file)
}

fn create_verified_takeover_backup_with<F>(
    path: &std::path::Path,
    original: &[u8],
    client: &str,
    write_backup: F,
) -> Result<std::path::PathBuf, String>
where
    F: FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
{
    let current = read_takeover_source(path, client)?
        .ok_or_else(|| format!("{client} 配置在备份前消失，未写入：{}", path.display()))?;
    if current != original {
        return Err(format!(
            "{client} 配置在备份前已被其它进程修改，未写入：{}",
            path.display()
        ));
    }

    let file_name = path
        .file_name()
        .ok_or_else(|| format!("{client} 配置路径没有文件名: {}", path.display()))?
        .to_string_lossy();
    let backup = path.with_file_name(format!(
        "{file_name}.llm-gateway-backup-{}.bak",
        uuid::Uuid::new_v4().simple()
    ));
    if let Err(error) = write_backup(&backup, original) {
        return Err(format!(
            "创建 {client} 配置备份失败，未写入原文件：{}: {error}",
            backup.display()
        ));
    }

    match std::fs::read(&backup) {
        Ok(contents) if contents == original => Ok(backup),
        Ok(_) => {
            let _ = std::fs::remove_file(&backup);
            Err(format!(
                "验证 {client} 配置备份失败，未写入原文件：{}",
                backup.display()
            ))
        }
        Err(error) => {
            let _ = std::fs::remove_file(&backup);
            Err(format!(
                "读取 {client} 配置备份失败，未写入原文件：{}: {error}",
                backup.display()
            ))
        }
    }
}

fn write_new_synced_file(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let result = file.write_all(contents).and_then(|()| file.sync_all());
    drop(file);
    if result.is_err() {
        // 只有 `create_new` 成功后才会走到这里，因此不会删除别的进程已有文件。
        let _ = std::fs::remove_file(path);
    }
    result
}

fn write_prepared_takeover(item: &PreparedTakeover) -> Result<(), String> {
    match item.original.as_deref() {
        Some(original) => {
            let backup = item.backup_path.as_deref().ok_or_else(|| {
                format!(
                    "{} 缺少已验证备份，拒绝改写：{}",
                    item.client,
                    item.path.display()
                )
            })?;
            write_existing_takeover_file(&item.path, original, &item.updated, backup, item.client)
        }
        None => write_new_takeover_file(&item.path, &item.updated, item.client),
    }
}

fn write_new_takeover_file(
    path: &std::path::Path,
    updated: &[u8],
    client: &str,
) -> Result<(), String> {
    write_new_synced_file(path, updated).map_err(|error| {
        format!(
            "新建 {client} 配置失败，未覆盖已有文件：{}: {error}",
            path.display()
        )
    })?;
    verify_takeover_contents(path, updated, client, "新建")
}

fn write_existing_takeover_file(
    path: &std::path::Path,
    original: &[u8],
    updated: &[u8],
    backup_path: &std::path::Path,
    client: &str,
) -> Result<(), String> {
    let current = read_takeover_source(path, client)?.ok_or_else(|| {
        format!(
            "{client} 配置在写入前消失，未改写；可用备份恢复：{}",
            backup_path.display()
        )
    })?;
    if current != original {
        return Err(format!(
            "{client} 配置在写入前已被其它进程修改，未改写；可用备份恢复：{}",
            backup_path.display()
        ));
    }

    let temp_path = write_takeover_temp_file(path, updated, client)?;
    #[cfg(windows)]
    {
        let uses_efs = match takeover_file_uses_efs(path) {
            Ok(uses_efs) => uses_efs,
            Err(error) => {
                let _ = std::fs::remove_file(&temp_path);
                return Err(error);
            }
        };
        if uses_efs {
            let _ = std::fs::remove_file(&temp_path);
            return write_efs_takeover_file_in_place(path, original, updated, backup_path, client);
        }
        if let Err(error) = replace_takeover_file_windows(&temp_path, path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(format!(
                "原子替换 {client} 配置失败，原文件保持不变；可用备份恢复：{}: {error}",
                backup_path.display()
            ));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (original, backup_path);
        if let Err(error) = std::fs::rename(&temp_path, path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(format!(
                "原子替换 {client} 配置失败，原文件保持不变；可用备份恢复：{}: {error}",
                backup_path.display()
            ));
        }
    }

    verify_takeover_contents(path, updated, client, "写入")
        .map_err(|error| format!("{error}；可用备份恢复：{}", backup_path.display()))
}

fn write_takeover_temp_file(
    path: &std::path::Path,
    updated: &[u8],
    client: &str,
) -> Result<std::path::PathBuf, String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("{client} 配置路径没有文件名: {}", path.display()))?
        .to_string_lossy();
    let temp_path = path.with_file_name(format!(
        ".{file_name}.llm-gateway-takeover-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    write_new_synced_file(&temp_path, updated).map_err(|error| {
        format!(
            "写入 {client} 配置临时文件失败，未改写原文件：{}: {error}",
            temp_path.display()
        )
    })?;
    Ok(temp_path)
}

fn verify_takeover_contents(
    path: &std::path::Path,
    expected: &[u8],
    client: &str,
    action: &str,
) -> Result<(), String> {
    let actual = std::fs::read(path).map_err(|error| {
        format!(
            "{action}后读取 {client} 配置失败：{}: {error}",
            path.display()
        )
    })?;
    if actual != expected {
        return Err(format!(
            "{action}后校验 {client} 配置失败：{}",
            path.display()
        ));
    }
    Ok(())
}

fn with_takeover_backup_paths(prepared: &[PreparedTakeover], error: String) -> String {
    let backups: Vec<String> = prepared
        .iter()
        .filter_map(|item| item.backup_path.as_ref())
        .map(|path| path.display().to_string())
        .collect();
    if backups.is_empty() {
        error
    } else {
        format!(
            "{error}。本次已验证的备份路径：{}；请关闭对应 CLI 后再使用备份恢复。",
            backups.join("；")
        )
    }
}

#[cfg(windows)]
fn takeover_wide_path(path: &std::path::Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn takeover_file_uses_efs(path: &std::path::Path) -> Result<bool, String> {
    let wide = takeover_wide_path(path);
    let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attributes == u32::MAX {
        return Err(format!(
            "检查 EFS 属性失败，未改写原文件：{}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(attributes & FILE_ATTRIBUTE_ENCRYPTED != 0)
}

#[cfg(windows)]
fn replace_takeover_file_windows(
    temp_path: &std::path::Path,
    path: &std::path::Path,
) -> Result<(), String> {
    let temp_wide = takeover_wide_path(temp_path);
    let path_wide = takeover_wide_path(path);
    let ok = unsafe {
        ReplaceFileW(
            path_wide.as_ptr(),
            temp_wide.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn write_efs_takeover_file_in_place(
    path: &std::path::Path,
    original: &[u8],
    updated: &[u8],
    backup_path: &std::path::Path,
    client: &str,
) -> Result<(), String> {
    match write_takeover_file_in_place(path, updated) {
        Ok(()) => match verify_takeover_contents(path, updated, client, "EFS 写入") {
            Ok(()) => Ok(()),
            Err(error) => Err(format!(
                "{error}；{}",
                restore_efs_takeover_file(path, original, backup_path, client)
            )),
        },
        Err(error) => Err(format!(
            "EFS 原位写入 {client} 配置失败：{}: {error}；{}",
            path.display(),
            restore_efs_takeover_file(path, original, backup_path, client)
        )),
    }
}

#[cfg(windows)]
fn write_takeover_file_in_place(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(windows)]
fn restore_efs_takeover_file(
    path: &std::path::Path,
    original: &[u8],
    backup_path: &std::path::Path,
    client: &str,
) -> String {
    let backup = match std::fs::read(backup_path) {
        Ok(contents) if contents == original => contents,
        Ok(_) => return format!("备份校验失败，请手动恢复：{}", backup_path.display()),
        Err(error) => {
            return format!(
                "读取备份失败，请手动恢复：{}: {error}",
                backup_path.display()
            )
        }
    };
    match write_takeover_file_in_place(path, &backup)
        .and_then(|()| std::fs::read(path).map(|contents| contents == original))
    {
        Ok(true) => format!("已自动从已验证备份恢复 {client} 原文件"),
        Ok(false) => format!(
            "自动恢复 {client} 配置的校验失败，请手动恢复：{}",
            backup_path.display()
        ),
        Err(error) => format!(
            "自动恢复 {client} 配置失败，请手动恢复：{}: {error}",
            backup_path.display()
        ),
    }
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
    use super::{
        apply_prepared_takeovers, create_verified_takeover_backup_with,
        ensure_supported_takeover_selection, prepare_claude_takeover, prepare_codex_takeover,
        prepare_crush_takeover, prepare_gemini_takeover, prepare_opencode_takeover, TakeoverStatus,
        CRUSH_TAKEOVER_BEGIN, CRUSH_TAKEOVER_END,
    };
    use std::{io, path::PathBuf};

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
    fn gemini_takeover_is_now_accepted() {
        // 【契约变更 2026-10-06】这条原先叫
        // `gemini_takeover_is_rejected_before_any_file_operation`，
        // 断言「Gemini 接管必须在碰任何文件之前被拒」。
        //
        // C2 落了 Gemini 原生入站之后，那条契约不再成立：
        // 现在走官方承认的 `~/.gemini/.env` 机制，写前同样备份。
        // 测试跟着契约改，而不是把旧断言删掉了事 ——
        // 「被拒」这条曾经挡住了一个真实的功能缺口，值得留下一句说明。
        assert!(ensure_supported_takeover_selection(false).is_ok());
        assert!(
            ensure_supported_takeover_selection(true).is_ok(),
            "Gemini 接管现在应当被接受"
        );

        // 反向：接管真的会写文件（不是「放行了但什么都没干」）
        let temp = TempDir::new();
        let path = temp.0.join(".gemini").join(".env");
        let written = apply_prepared_takeovers(vec![prepare_gemini_takeover(
            &path,
            "http://127.0.0.1:15721",
            "test-token",
        )
        .expect("prepare Gemini")])
        .expect("apply takeover");
        assert_eq!(written[0].status, TakeoverStatus::Created);
        let contents = std::fs::read_to_string(&path).expect("read Gemini env");
        assert!(contents.contains("GEMINI_API_KEY=test-token"), "{contents}");
    }

    #[test]
    fn gemini_env_merge_preserves_unrelated_variables() {
        // `.gemini/.env` 是**用户自己的**文件，可能还放着别的变量。
        // 整份覆盖会把它们删掉，而且用户看不出是网关干的。
        let temp = TempDir::new();
        let path = temp.0.join(".gemini").join(".env");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# 我自己注释掉的 key\n# GEMINI_API_KEY=disabled\nMY_THEME=dark\nCODE_ASSIST_ENDPOINT=https://old.invalid\n",
        )
        .unwrap();

        apply_prepared_takeovers(vec![prepare_gemini_takeover(
            &path,
            "http://127.0.0.1:15721",
            "unified-token",
        )
        .expect("prepare Gemini")])
        .expect("apply takeover");

        let got = std::fs::read_to_string(&path).expect("read");
        // 无关变量与注释原样保留
        assert!(got.contains("MY_THEME=dark"), "无关变量被删了：{got}");
        assert!(
            got.contains("# GEMINI_API_KEY=disabled"),
            "注释行是用户主动禁用的意图，不该被删：{got}"
        );
        // 我们自己的键被更新（不是重复追加）
        assert!(got.contains("CODE_ASSIST_ENDPOINT=http://127.0.0.1:15721"));
        assert!(!got.contains("https://old.invalid"), "旧值没被替换：{got}");
        assert!(got.contains("GEMINI_API_KEY=unified-token"));
        assert!(got.contains("GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:15721"));
        // 每个键只出现一次（注释掉的那行不算）
        let uncommented = |key: &str| {
            got.lines()
                .filter(|l| !l.trim_start().starts_with('#') && l.starts_with(key))
                .count()
        };
        assert_eq!(uncommented("CODE_ASSIST_ENDPOINT"), 1, "{got}");
        assert_eq!(uncommented("GEMINI_API_KEY"), 1, "{got}");
        assert_eq!(uncommented("GOOGLE_GEMINI_BASE_URL"), 1, "{got}");
    }

    #[test]
    fn takeover_preserves_unrelated_settings_and_writes_codex_provider_contract() {
        let temp = TempDir::new();
        let claude_path = temp.0.join(".claude").join("settings.json");
        let codex_path = temp.0.join(".codex").join("config.toml");
        let gemini_path = temp.0.join(".gemini").join(".env");
        for path in [&claude_path, &codex_path, &gemini_path] {
            std::fs::create_dir_all(path.parent().expect("config parent"))
                .expect("create config directory");
        }

        let claude_original = r#"{
  "permissions": { "allow": ["Read"] },
  "env": {
    "KEEP_ME": "yes",
    "ANTHROPIC_BASE_URL": "https://old.invalid"
  },
  "ui": { "theme": "dark" }
}"#;
        let codex_original = r#"# retain this comment
model = "gpt-5"
model_provider = "example"
approval_policy = "on-request"
base_url = "http://old-invalid-root.example/v1"

[model_providers.example]
name = "Example"
base_url = "https://provider.invalid/v1"
wire_api = "responses"
experimental_bearer_token = "unrelated-token"
requires_openai_auth = true

[model_providers.example.http_headers]
Authorization = "Bearer unrelated"
X-Example-Header = "preserve-example-header"

[model_providers.llm_gateway]
custom_setting = "preserve-me"
env_key = "LLM_GATEWAY_OLD_KEY"
env_key_instructions = "use the old gateway key"

[model_providers.llm_gateway.auth]
command = "old-gateway-token-command"

[model_providers.llm_gateway.http_headers]
Authorization = "Bearer stale"
X-Gateway-Header = "preserve-gateway-header"

[model_providers.llm_gateway.env_http_headers]
authorization = "LLM_GATEWAY_OLD_AUTHORIZATION"
X-Gateway-Env-Header = "LLM_GATEWAY_PRESERVE_HEADER"
"#;
        let gemini_original = "# keep this comment\r\nGOOGLE_API_KEY=test-key\r\nGOOGLE_GEMINI_BASE_URL=https://old.invalid\r\nCUSTOM_VALUE=keep\r\n";
        std::fs::write(&claude_path, claude_original).expect("write Claude config");
        std::fs::write(&codex_path, codex_original).expect("write Codex config");
        std::fs::write(&gemini_path, gemini_original).expect("write Gemini config");

        let results = apply_prepared_takeovers(vec![
            prepare_claude_takeover(&claude_path, "http://127.0.0.1:15721", "test-token")
                .expect("prepare Claude"),
            prepare_codex_takeover(&codex_path, "http://127.0.0.1:15721", "test-token")
                .expect("prepare Codex"),
            prepare_gemini_takeover(&gemini_path, "http://127.0.0.1:15721", "test-token")
                .expect("prepare Gemini"),
        ])
        .expect("apply takeover");

        assert_eq!(results.len(), 3);
        for result in &results {
            assert_eq!(result.status, TakeoverStatus::Updated);
            assert!(
                result.backup_path.is_some(),
                "{} needs a backup",
                result.client
            );
        }
        let claude_backup =
            PathBuf::from(results[0].backup_path.as_ref().expect("Claude backup path"));
        let codex_backup =
            PathBuf::from(results[1].backup_path.as_ref().expect("Codex backup path"));
        let gemini_backup =
            PathBuf::from(results[2].backup_path.as_ref().expect("Gemini backup path"));
        assert_eq!(
            std::fs::read(&claude_backup).expect("read Claude backup"),
            claude_original.as_bytes()
        );
        assert_eq!(
            std::fs::read(&codex_backup).expect("read Codex backup"),
            codex_original.as_bytes()
        );
        assert_eq!(
            std::fs::read(&gemini_backup).expect("read Gemini backup"),
            gemini_original.as_bytes()
        );

        let claude_updated: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&claude_path).expect("read Claude config"),
        )
        .expect("parse updated Claude config");
        assert_eq!(claude_updated["permissions"]["allow"][0], "Read");
        assert_eq!(claude_updated["ui"]["theme"], "dark");
        assert_eq!(claude_updated["env"]["KEEP_ME"], "yes");
        assert_eq!(
            claude_updated["env"]["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:15721"
        );

        let codex_updated: toml::Value =
            toml::from_str(&std::fs::read_to_string(&codex_path).expect("read Codex config"))
                .expect("parse updated Codex config");
        assert_eq!(codex_updated["model"].as_str(), Some("auto"));
        assert_eq!(
            codex_updated["model_provider"].as_str(),
            Some("llm_gateway")
        );
        assert_eq!(
            codex_updated["approval_policy"].as_str(),
            Some("on-request")
        );
        assert_eq!(
            codex_updated["model_providers"]["example"]["base_url"].as_str(),
            Some("https://provider.invalid/v1")
        );
        assert_eq!(
            codex_updated["model_providers"]["example"]["wire_api"].as_str(),
            Some("responses")
        );
        assert_eq!(
            codex_updated["model_providers"]["example"]["experimental_bearer_token"].as_str(),
            Some("unrelated-token")
        );
        assert_eq!(
            codex_updated["model_providers"]["example"]["requires_openai_auth"].as_bool(),
            Some(true)
        );
        assert_eq!(
            codex_updated["model_providers"]["example"]["http_headers"]["Authorization"].as_str(),
            Some("Bearer unrelated")
        );
        assert_eq!(
            codex_updated["model_providers"]["example"]["http_headers"]["X-Example-Header"]
                .as_str(),
            Some("preserve-example-header")
        );
        assert!(codex_updated.get("base_url").is_none());
        let gateway = &codex_updated["model_providers"]["llm_gateway"];
        assert_eq!(gateway["custom_setting"].as_str(), Some("preserve-me"));
        assert_eq!(gateway["name"].as_str(), Some("LLM Gateway"));
        assert_eq!(
            gateway["base_url"].as_str(),
            Some("http://127.0.0.1:15721/v1")
        );
        assert_eq!(gateway["wire_api"].as_str(), Some("responses"));
        assert_eq!(
            gateway["experimental_bearer_token"].as_str(),
            Some("test-token")
        );
        assert_eq!(gateway["requires_openai_auth"].as_bool(), Some(false));
        assert!(gateway.get("auth").is_none());
        assert!(gateway.get("env_key").is_none());
        assert!(gateway.get("env_key_instructions").is_none());
        assert!(gateway["http_headers"].get("Authorization").is_none());
        assert_eq!(
            gateway["http_headers"]["X-Gateway-Header"].as_str(),
            Some("preserve-gateway-header")
        );
        assert!(gateway["env_http_headers"].get("authorization").is_none());
        assert_eq!(
            gateway["env_http_headers"]["X-Gateway-Env-Header"].as_str(),
            Some("LLM_GATEWAY_PRESERVE_HEADER")
        );
        assert_eq!(
            codex_updated["model_providers"]["example"]["name"].as_str(),
            Some("Example")
        );

        let gemini_updated = std::fs::read_to_string(&gemini_path).expect("read Gemini config");
        assert!(gemini_updated.contains("# keep this comment\r\n"));
        assert!(gemini_updated.contains("GOOGLE_API_KEY=test-key\r\n"));
        assert!(gemini_updated.contains("CUSTOM_VALUE=keep\r\n"));
        assert!(gemini_updated.contains("GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:15721\r\n"));
    }

    #[test]
    fn takeover_creates_missing_files_without_unnecessary_backups() {
        let temp = TempDir::new();
        let claude_path = temp.0.join(".claude").join("settings.json");
        let codex_path = temp.0.join(".codex").join("config.toml");
        let gemini_path = temp.0.join(".gemini").join(".env");

        let results = apply_prepared_takeovers(vec![
            prepare_claude_takeover(&claude_path, "http://127.0.0.1:15721", "test-token")
                .expect("prepare Claude"),
            prepare_codex_takeover(&codex_path, "http://127.0.0.1:15721", "test-token")
                .expect("prepare Codex"),
            prepare_gemini_takeover(&gemini_path, "http://127.0.0.1:15721", "test-token")
                .expect("prepare Gemini"),
        ])
        .expect("apply takeover");

        for result in &results {
            assert_eq!(result.status, TakeoverStatus::Created);
            assert!(
                result.backup_path.is_none(),
                "{} should be created",
                result.client
            );
        }
        assert!(std::fs::read_to_string(&claude_path)
            .expect("read Claude config")
            .contains("ANTHROPIC_BASE_URL"));
        // 【契约变更 2026-10-06】原先只断言 `GOOGLE_GEMINI_BASE_URL` 一行。
        // 现在两个地址键都写（哪个真生效要真装一次 Gemini CLI 才能实测，
        // 见 `prepare_gemini_takeover` 的注释），加上统一 Key。
        assert_eq!(
            std::fs::read_to_string(&gemini_path).expect("read Gemini config"),
            "CODE_ASSIST_ENDPOINT=http://127.0.0.1:15721\n\
             GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:15721\n\
             GEMINI_API_KEY=test-token\n"
        );
        let codex_updated: toml::Value =
            toml::from_str(&std::fs::read_to_string(&codex_path).expect("read Codex config"))
                .expect("parse new Codex config");
        assert_eq!(
            codex_updated["model_provider"].as_str(),
            Some("llm_gateway")
        );
        assert_eq!(
            codex_updated["model_providers"]["llm_gateway"]["base_url"].as_str(),
            Some("http://127.0.0.1:15721/v1")
        );
    }

    #[test]
    fn repeated_backups_are_unique_without_waiting_for_the_clock() {
        let temp = TempDir::new();
        let path = temp.0.join(".gemini").join(".env");
        std::fs::create_dir_all(path.parent().expect("Gemini config parent"))
            .expect("create Gemini config directory");
        let original = "CUSTOM_VALUE=keep\n";
        std::fs::write(&path, original).expect("write original Gemini config");

        let first = apply_prepared_takeovers(vec![prepare_gemini_takeover(
            &path,
            "http://127.0.0.1:15721",
            "test-token",
        )
        .expect("prepare first")])
        .expect("apply first");
        let first_backup = PathBuf::from(first[0].backup_path.as_ref().expect("first backup path"));
        let after_first = std::fs::read(&path).expect("read first update");

        let second = apply_prepared_takeovers(vec![prepare_gemini_takeover(
            &path,
            "http://127.0.0.1:16721",
            "test-token-2",
        )
        .expect("prepare second")])
        .expect("apply second");
        let second_backup =
            PathBuf::from(second[0].backup_path.as_ref().expect("second backup path"));

        assert_ne!(first_backup, second_backup);
        assert_eq!(
            std::fs::read(&first_backup).expect("read first backup"),
            original.as_bytes()
        );
        assert_eq!(
            std::fs::read(&second_backup).expect("read second backup"),
            after_first
        );
    }

    #[test]
    fn opencode_and_crush_takeover_preserve_user_configuration() {
        let temp = TempDir::new();
        let opencode_path = temp
            .0
            .join(".config")
            .join("opencode")
            .join("opencode.json");
        let crush_path = temp.0.join(".config").join("crush").join("crushrc");
        std::fs::create_dir_all(opencode_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(crush_path.parent().unwrap()).unwrap();
        let opencode_original = r#"{
  "provider": { "existing": { "name": "Keep me" } },
  "theme": "dark"
}"#;
        let crush_original =
            "option debug true\nprovider add existing --type openai --base-url 'https://old.invalid/v1'\n";
        std::fs::write(&opencode_path, opencode_original).unwrap();
        std::fs::write(&crush_path, crush_original).unwrap();

        let results = apply_prepared_takeovers(vec![
            prepare_opencode_takeover(&opencode_path, "http://127.0.0.1:15721/v1", "test-token")
                .unwrap(),
            prepare_crush_takeover(&crush_path, "http://127.0.0.1:15721/v1", "test-token").unwrap(),
        ])
        .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.backup_path.is_some()));

        let opencode: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&opencode_path).unwrap()).unwrap();
        assert_eq!(opencode["theme"], "dark");
        assert_eq!(opencode["provider"]["existing"]["name"], "Keep me");
        assert_eq!(opencode["model"], "llm-gateway/auto");
        assert_eq!(
            opencode["provider"]["llm-gateway"]["options"]["baseURL"],
            "http://127.0.0.1:15721/v1"
        );

        let crush = std::fs::read_to_string(&crush_path).unwrap();
        assert!(crush.contains("option debug true"));
        assert!(crush.contains("provider add existing"));
        assert!(crush.contains(CRUSH_TAKEOVER_BEGIN));
        assert!(crush.contains("provider add llm-gateway"));
        assert!(crush.contains("model large llm-gateway/auto"));

        // 再次接管必须替换托管块，而不是每执行一次就追加一份。
        apply_prepared_takeovers(vec![prepare_crush_takeover(
            &crush_path,
            "http://127.0.0.1:16721/v1",
            "next-token",
        )
        .unwrap()])
        .unwrap();
        let replay = std::fs::read_to_string(&crush_path).unwrap();
        assert_eq!(replay.matches(CRUSH_TAKEOVER_BEGIN).count(), 1);
        assert_eq!(replay.matches(CRUSH_TAKEOVER_END).count(), 1);
        assert!(replay.contains("http://127.0.0.1:16721/v1"));
    }

    #[test]
    fn backup_failure_stops_before_the_original_file_is_changed() {
        let temp = TempDir::new();
        let path = temp.0.join(".gemini").join(".env");
        std::fs::create_dir_all(path.parent().expect("Gemini config parent"))
            .expect("create Gemini config directory");
        let original = b"CUSTOM_VALUE=keep\n";
        std::fs::write(&path, original).expect("write original Gemini config");

        let error = create_verified_takeover_backup_with(
            &path,
            original,
            "Gemini CLI",
            |_backup, _contents| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "simulated backup failure",
                ))
            },
        )
        .expect_err("backup failure should stop takeover");

        assert!(error.contains("未写入原文件"));
        assert_eq!(std::fs::read(&path).expect("read original"), original);
    }

    #[test]
    fn malformed_json_or_toml_is_never_silently_replaced() {
        let temp = TempDir::new();
        let claude_path = temp.0.join(".claude").join("settings.json");
        let codex_path = temp.0.join(".codex").join("config.toml");
        std::fs::create_dir_all(claude_path.parent().expect("Claude config parent"))
            .expect("create Claude config directory");
        std::fs::create_dir_all(codex_path.parent().expect("Codex config parent"))
            .expect("create Codex config directory");
        let malformed_json = "{ not-json";
        let malformed_toml = "base_url = [";
        std::fs::write(&claude_path, malformed_json).expect("write malformed JSON");
        std::fs::write(&codex_path, malformed_toml).expect("write malformed TOML");

        assert!(
            prepare_claude_takeover(&claude_path, "http://127.0.0.1:15721", "test-token")
                .err()
                .expect("malformed JSON must fail")
                .contains("未改写原文件")
        );
        assert!(
            prepare_codex_takeover(&codex_path, "http://127.0.0.1:15721", "test-token")
                .err()
                .expect("malformed TOML must fail")
                .contains("未改写原文件")
        );
        assert_eq!(
            std::fs::read_to_string(&claude_path).expect("read malformed JSON"),
            malformed_json
        );
        assert_eq!(
            std::fs::read_to_string(&codex_path).expect("read malformed TOML"),
            malformed_toml
        );
    }
}

/* --------------------- 本地模型 / 智能模式 / 联网搜索 --------------------- */

/// 扫描本机全部推理运行时。并发探测，一个端点不可达不影响其它端点。
#[tauri::command]
pub async fn list_local_runtimes(
    state: State<'_, AppState>,
) -> Result<Vec<crate::local_models::ProbeOutcome>, String> {
    let cfg = state.config.read().clone();
    let http = crate::local_models::runtime::default_client();
    let outcomes = crate::local_models::probe_all(
        &http,
        &cfg.local_models.endpoints,
        cfg.local_models.probe_timeout_ms,
    )
    .await;
    Ok(outcomes)
}

/// 某个端点上的已装模型。端点不可达时返回明确的错误文案，不返回空列表假装「没模型」。
#[tauri::command]
pub async fn list_local_models(
    state: State<'_, AppState>,
    endpoint_id: String,
) -> Result<Vec<crate::local_models::LocalModelInfo>, String> {
    let cfg = state.config.read().clone();
    let endpoint = crate::local_models::find_endpoint(&cfg.local_models.endpoints, &endpoint_id)
        .ok_or_else(|| format!("找不到本地端点 {endpoint_id}"))?;
    let http = crate::local_models::runtime::default_client();
    crate::local_models::runtime::fetch_models(&http, endpoint, cfg.local_models.probe_timeout_ms)
        .await
        .map_err(|e| e.to_string())
}

/// 登记一个本地模型为供应商。
///
/// 登记后它与云端模型**完全平权**：同样进候选链、同样打分降级、同样审计。
/// 重复登记同一个 `endpoint_id + upstream` 会并入已有 Provider，而不是新建。
#[tauri::command]
pub async fn register_local_model(
    state: State<'_, AppState>,
    input: crate::local_models::RegisterLocalInput,
) -> Result<crate::local_models::RegisterLocalOutcome, String> {
    if input.upstream.trim().is_empty() {
        return Err("本地模型名不能为空".into());
    }
    let (http, endpoint, probe_timeout, providers, now) = {
        let cfg = state.config.read().clone();
        let endpoint =
            crate::local_models::find_endpoint(&cfg.local_models.endpoints, &input.endpoint_id)
                .cloned()
                .ok_or_else(|| format!("找不到本地端点 {}", input.endpoint_id))?;
        (
            crate::local_models::runtime::default_client(),
            endpoint,
            cfg.local_models.probe_timeout_ms,
            repo::list_providers(state.db.pool())
                .await
                .map_err(|e| e.to_string())?,
            chrono::Utc::now(),
        )
    };

    // 每次登记都重新扫一次目录：用户可能在登记前又拉了新模型，
    // 一次性全部收进来比让他逐个点更省事。
    let scanned = crate::local_models::runtime::fetch_models(&http, &endpoint, probe_timeout)
        .await
        .map_err(|e| e.to_string())?;
    let target = crate::local_models::find_by_upstream(&scanned, input.upstream.trim())
        .cloned()
        .ok_or_else(|| {
            format!(
                "{} 上没有名为 {} 的模型，请先在运行时里下载",
                endpoint.label, input.upstream
            )
        })?;

    let alias = input
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&target.alias)
        .to_owned();

    // 同一端点的模型共用一个 Provider；重复登记并入它。
    let mut provider = providers
        .iter()
        .find(|p| {
            p.base_url == endpoint.base_url
                && p.dialect == crate::local_models::dialect_of(endpoint.kind)
        })
        .cloned();
    if provider.is_none() {
        provider = Some(Provider {
            id: format!("lp-{}", uuid::Uuid::new_v4().simple()),
            name: input
                .provider_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("本地 · {}", endpoint.label)),
            dialect: crate::local_models::dialect_of(endpoint.kind),
            base_url: endpoint.base_url.clone(),
            api_key_enc: String::new(),
            enabled: input.enabled.unwrap_or(true),
            priority: 0,
            models: Vec::new(),
            rpm_limit: 0,
            intelligence: 50,
            note: Some(format!("由 {} 自动登记的本地模型", endpoint.label)),
            runtime_id: None,
            created_at: now,
            updated_at: now,
        });
    }
    let mut provider = provider.expect("上一步已确保存在");

    // 端点地址被改过之后，同一个端点会留下一份**永远 404 的僵尸供应商**：
    // 上一步按 `base_url == endpoint.base_url` 匹配，地址变了就匹配不到、
    // 于是新建一份，旧的原封不动留着。真机踩到过（把 `…:11434` 改成
    // `…:11434/v1` 登记一次），表现是路由一直挑中那个失效项、
    // 每次请求都 `404 page not found`，界面上完全看不出有两个。
    //
    // 清理条件刻意收得很紧：只删「同一 dialect + 同名自动登记 + 标记是本地登记的
    // + 声明的 host 与当前端点相同，只是路径不同」。绝不动用户手工建的供应商。
    let stale: Vec<String> = providers
        .iter()
        .filter(|old| {
            old.id != provider.id
                && old.dialect == provider.dialect
                && old.base_url != endpoint.base_url
                && crate::local_models::same_host(&endpoint.base_url, &old.base_url)
                && old
                    .note
                    .as_deref()
                    .map(|n| n.starts_with("由 ") && n.ends_with(" 自动登记的本地模型"))
                    .unwrap_or(false)
        })
        .map(|old| old.id.clone())
        .collect();
    for id in stale {
        repo::delete_provider(state.db.pool(), &id)
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(provider = %id, "端点地址已变更，清理自动登记的失效供应商");
    }

    // 把该端点扫到的全部模型并进去（已存在的按 upstream 跳过）。
    let mut added = 0usize;
    for info in &scanned {
        if info.upstream == target.upstream {
            continue;
        }
        if provider.models.iter().any(|m| m.upstream == info.upstream) {
            continue;
        }
        provider
            .models
            .push(crate::local_models::to_model_ref(info));
        added += 1;
    }
    let target_ref = crate::local_models::to_model_ref(&target);
    let already = provider
        .models
        .iter()
        .position(|m| m.upstream == target.upstream);
    match already {
        Some(index) => {
            // 保留用户改过的 alias，其余元数据跟着运行时刷新。
            let mut merged = target_ref.clone();
            merged.alias = provider.models[index].alias.clone();
            provider.models[index] = merged;
        }
        None => {
            provider.models.push(target_ref);
            added += 1;
        }
    }
    let provider_id = provider.id.clone();
    repo::upsert_provider(state.db.pool(), &provider)
        .await
        .map_err(|e| e.to_string())?;
    state
        .gateway
        .reload_providers()
        .await
        .map_err(|e| e.to_string())?;

    Ok(crate::local_models::RegisterLocalOutcome {
        provider_id,
        alias,
        added_models: added,
        all_models: scanned,
    })
}

/// 拉取本地模型（仅 Ollama）。逐行进度通过 Tauri 事件推给界面。
#[tauri::command]
pub async fn pull_local_model(
    app: tauri::AppHandle,
    endpoint_id: String,
    model: String,
) -> Result<(), String> {
    let endpoint = state_config_endpoint(&app, &endpoint_id)?;
    if endpoint.kind != crate::config::LocalRuntimeKind::Ollama {
        return Err(format!(
            "{} 没有统一的模型下载接口，请用它自带的客户端下载",
            endpoint.label
        ));
    }
    let http = crate::local_models::runtime::default_client();
    let progress_app = app.clone();
    crate::local_models::pull_ollama_model(
        &http,
        &endpoint.base_url,
        &model,
        600_000,
        move |event| {
            let _ = progress_app.emit("local-model://pull-progress", &event);
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 读配置里的端点。抽成函数是因为命令签名里拿不到 `State`，只能走 AppHandle。
fn state_config_endpoint(
    app: &tauri::AppHandle,
    endpoint_id: &str,
) -> Result<crate::config::LocalEndpoint, String> {
    let cfg = app.state::<AppState>().config.read().clone();
    crate::local_models::find_endpoint(&cfg.local_models.endpoints, endpoint_id)
        .cloned()
        .ok_or_else(|| format!("找不到本地端点 {endpoint_id}"))
}

/// 搜索设置（Key 只回掩码）。
#[tauri::command]
pub async fn get_search_settings(
    state: State<'_, AppState>,
) -> Result<crate::search::SearchSettingsView, String> {
    let cfg = state.config.read().clone();
    let api_key_masked = match repo::get_secret(state.db.pool(), repo::SECRET_SEARCH_API_KEY).await
    {
        Ok(Some(encoded)) => crypto::decrypt(&encoded)
            .ok()
            .and_then(|key| crate::search::mask_key(&key)),
        _ => None,
    };
    Ok(crate::search::SearchSettingsView {
        enabled: cfg.search.enabled,
        backend: cfg.search.backend,
        // 先算再挪：`searxng_url` 是 `Option<String>`，移走之后
        // 再借用 `cfg.search` 属于部分移动后的借用。
        max_results: cfg.search.normalized_max_results(),
        searxng_url: cfg.search.searxng_url,
        timeout_ms: cfg.search.timeout_ms,
        inject_as: cfg.search.inject_as,
        api_key_masked,
        backend_needs_key: matches!(
            cfg.search.backend,
            crate::config::SearchBackendKind::Tavily | crate::config::SearchBackendKind::Brave
        ),
    })
}

/// 更新搜索设置。`api_key` 留空表示不改；`clear_api_key` 才真的删。
#[tauri::command]
pub async fn update_search_settings(
    state: State<'_, AppState>,
    input: crate::search::SearchSettingsInput,
) -> Result<crate::search::SearchSettingsView, String> {
    // 必须先克隆再提交，不能就地改 `state.config`：
    //   ① 代理读的是 `state.gateway.cfg`（`cfg_snapshot()`），和 `state.config`
    //      是**两把独立的锁**。只写前者的话，开关/后端/条数在重启前完全不生效，
    //      而 `get_search_settings` 读的是刚被改过的 `state.config`，
    //      界面上看起来「保存成功了」——这种假成功最难查。
    //   ② 就地改会在校验或落盘失败时留下一个被拒绝的内存值。
    {
        let guard = state.config.read();
        let mut next = guard.clone();
        drop(guard);
        next.search.enabled = input.enabled;
        next.search.backend = input.backend;
        next.search.searxng_url = input.searxng_url.clone();
        next.search.max_results = input.max_results;
        next.search.timeout_ms = input.timeout_ms;
        next.search.inject_as = input.inject_as;
        next.normalize_local();
        crate::search::validate(&next.search).map_err(|e| e.to_string())?;
        next.save().map_err(|e| e.to_string())?;
        *state.gateway.cfg.write() = next.clone();
        *state.config.write() = next;
    }
    if input.clear_api_key {
        repo::delete_secret(state.db.pool(), repo::SECRET_SEARCH_API_KEY)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(key) = input
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        let encoded = crypto::encrypt(key).map_err(|e| e.to_string())?;
        repo::set_secret(state.db.pool(), repo::SECRET_SEARCH_API_KEY, &encoded)
            .await
            .map_err(|e| e.to_string())?;
    }
    get_search_settings(state).await
}

/// 实跑一次搜索后端，不注入上下文。用于「测试后端」按钮。
#[tauri::command]
pub async fn test_search_backend(
    state: State<'_, AppState>,
    text: String,
) -> Result<crate::search::executor::SearchOutcome, String> {
    let cfg = state.config.read().clone();
    if !cfg.search.enabled {
        return Err("联网搜索尚未启用".into());
    }
    let key = match repo::get_secret(state.db.pool(), repo::SECRET_SEARCH_API_KEY).await {
        Ok(Some(encoded)) => crypto::decrypt(&encoded).ok(),
        _ => None,
    };
    let http = reqwest::Client::new();
    Ok(crate::search::test_backend(&http, &cfg.search, key.as_deref(), &text).await)
}

/// 分类器试跑。走**完整链路**（硬规则 → Jev → 启发式），与线上同一份代码。
#[tauri::command]
pub async fn classify_preview(
    state: State<'_, AppState>,
    text: String,
    has_image: bool,
    has_tools: bool,
) -> Result<crate::intellect::TaskIntent, String> {
    let cfg = state.config.read().clone();
    let messages = vec![crate::domain::Message::user(text)];
    let media = crate::media::Media {
        image: has_image,
        ..Default::default()
    };
    let input = crate::intellect::ClassifyInput {
        messages: &messages,
        media,
        has_tools,
        requested_model: "auto",
    };
    let jev = crate::intellect::JevClient::new(
        &cfg.smart_routing.jev.base_url,
        &cfg.smart_routing.jev.model,
        cfg.smart_routing.jev.timeout_ms,
        cfg.smart_routing.jev.max_state_chars,
    )
    .ok();
    Ok(crate::intellect::classify(&input, &cfg.smart_routing, jev.as_ref()).await)
}

/// 决策端点健康检查 + 一道题的原始分布。界面上用它解释「为什么没走 Jev」。
#[tauri::command]
pub async fn jev_probe(
    state: State<'_, AppState>,
    text: String,
) -> Result<serde_json::Value, String> {
    let cfg = state.config.read().clone();
    let client = crate::intellect::JevClient::new(
        &cfg.smart_routing.jev.base_url,
        &cfg.smart_routing.jev.model,
        cfg.smart_routing.jev.timeout_ms,
        cfg.smart_routing.jev.max_state_chars,
    )
    .map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "model": client.model_name(),
        "state": { "prompt": client.truncate_state(&text) },
        "questions": crate::intellect::jev::preview_questions(),
    });
    match client.decide(body).await {
        Ok(result) => {
            let rows = crate::intellect::jev::preview(
                &result,
                cfg.smart_routing.min_confidence,
                cfg.smart_routing.min_margin,
            );
            Ok(serde_json::json!({
                "ok": true,
                "endpoint": client.endpoint(),
                "rows": rows,
            }))
        }
        Err(error) => Ok(serde_json::json!({
            "ok": false,
            "endpoint": client.endpoint(),
            "error": error.to_string(),
        })),
    }
}

/// 批量校分类器：算混淆矩阵，回答「该不该更信任 Jev」。
///
/// **只跑样本，不改任何配置。** 校准的价值在于让人看完数据自己决定阈值，
/// 命令擅自调阈值就越权了。
///
/// 逐条串行：每条都要打一次决策端点，并发打过去只会挤占它本来就紧张的
/// 单核推理（edgeJev 是 CPU int8）。样本量是几十条量级，串行的总耗时可接受。
#[tauri::command]
pub async fn calibrate_classifier(
    state: State<'_, AppState>,
    samples: Vec<crate::intellect::LabeledSample>,
) -> Result<crate::intellect::CalibrationReport, String> {
    if samples.is_empty() {
        return Err("校准至少需要一条样本".into());
    }
    if samples.len() > 200 {
        // 上限不是为了防滥用，是因为每条都是一次真实推理，
        // 200 条 × 20ms 已经接近 5 秒，再多就会卡住界面。
        return Err(format!("一次最多校准 200 条，收到 {}", samples.len()));
    }
    let cfg = state.config.read().clone();
    // 配置无效时仍然可以校准——结果会全部落到「弃权」，
    // 那本身就是有用信息（告诉用户端点还没配好）。
    let jev = crate::intellect::JevClient::new(
        &cfg.smart_routing.jev.base_url,
        &cfg.smart_routing.jev.model,
        cfg.smart_routing.jev.timeout_ms,
        cfg.smart_routing.jev.max_state_chars,
    )
    .ok();
    let smart = cfg.smart_routing.clone();
    Ok(crate::intellect::calibrate::calibrate(samples, smart, jev.as_ref()).await)
}

/// 校准用的默认样本集。
///
/// **这些不是拍脑袋写的**，而是本机 edgeJev 实测的五条（`docs/0.3.0验证记录.md` §2.6）。
/// 预置它们是为了让用户点一下就能看到「它到底行不行」，而不是面对空白输入框。
/// 其中「线上排查根因」是已知的**错判样本**，刻意保留在集里——
/// 删掉它报告就会显示成一切正常，那种样本集没有诊断价值。
#[tauri::command]
pub async fn calibrate_default_samples(
    state: State<'_, AppState>,
) -> Result<crate::intellect::CalibrationReport, String> {
    calibrate_classifier(state, default_calibration_samples()).await
}

fn default_calibration_samples() -> Vec<crate::intellect::LabeledSample> {
    use crate::intellect::calibrate::LabeledSample as S;
    vec![
        S {
            text: "把变量名 x 改成 userName".into(),
            expected: crate::intellect::TaskClass::Simple,
            has_image: false,
            has_tools: false,
        },
        S {
            text: "写一个快速排序算法".into(),
            expected: crate::intellect::TaskClass::Simple,
            has_image: false,
            has_tools: false,
        },
        S {
            text: "帮我设计一个分布式限流器，需要考虑故障转移和一致性".into(),
            expected: crate::intellect::TaskClass::Reasoning,
            has_image: false,
            has_tools: false,
        },
        // 已知错判样本：edgeJev 给 0.747 置信度判成 simple，而正确答案是 reasoning。
        S {
            text: "线上服务 500 白屏，帮我定位根因".into(),
            expected: crate::intellect::TaskClass::Reasoning,
            has_image: false,
            has_tools: false,
        },
        S {
            text: "你好".into(),
            expected: crate::intellect::TaskClass::Simple,
            has_image: false,
            has_tools: false,
        },
    ]
}

/* --------------------------- 定价与校准 --------------------------- */

/// 立即刷新定价。手工填写的价格永不被覆盖；结果逐项返回，便于界面解释。
#[tauri::command]
pub async fn refresh_pricing(
    state: State<'_, AppState>,
) -> Result<crate::pricing::RefreshOutcome, String> {
    crate::pricing_refresh::refresh(&state.gateway, true).await
}

/// 最近一次定价刷新摘要；从未刷新过时为 null。
#[tauri::command]
pub async fn pricing_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    crate::pricing_refresh::status(&state.gateway).await
}

/// token 估算校准表。ratio 是「上游实际 / 本地估算」的 EWMA 比值。
#[tauri::command]
pub async fn list_token_calibrations(
    state: State<'_, AppState>,
) -> Result<Vec<repo::Calibration>, String> {
    repo::list_calibrations(state.db.pool())
        .await
        .map_err(|e| e.to_string())
}

/// 清空校准样本。只影响本机估算口径，不改动任何请求或会话数据。
#[tauri::command]
pub async fn clear_token_calibrations(state: State<'_, AppState>) -> Result<u64, String> {
    repo::clear_calibrations(state.db.pool())
        .await
        .map_err(|e| e.to_string())
}

/// 导出/导入整个数据目录（多设备同步用，等价于 CC Switch 的配置目录同步）
#[tauri::command]
pub async fn export_bundle(_state: State<'_, AppState>, dest: String) -> Result<(), String> {
    crate::bundle::export_bundle(&crate::config::app_data_dir(), std::path::Path::new(&dest)).await
}

/// 导入结果。逐项报告，避免把「部分成功」说成整体成功。
#[derive(Debug, Clone, Serialize)]
pub struct ImportBundleResult {
    pub providers_imported: usize,
    pub models_imported: usize,
    /// 需要用当前设备的主密钥才能解密的条目数；密钥不匹配时该 Provider 会保留
    /// 但必须重新填写 Key，绝不静默把无法解密的密文当成可用凭据。
    pub providers_missing_key: Vec<String>,
    pub config_imported: bool,
    /// 统一 Key 与远程模式属于本机安全边界，不随包覆盖。
    pub preserved_security_fields: Vec<String>,
}

/// 导入之前的数据目录备份路径，便于用户回退。
#[derive(Debug, Clone, Serialize)]
pub struct ImportBundleOutcome {
    pub result: ImportBundleResult,
    pub backup_dir: String,
}

/// 从导出目录读取配置包并应用到本机。
///
/// 与 `export_bundle` 对称：读取同目录的 `config.toml` 与 `gateway.db`。为避免
/// 在运行中替换 SQLite 文件本身，这里打开源库只读、把 Provider/模型读出来后在
/// 本机库的单个事务内替换；配置同样经过与「设置页」相同的校验再落盘。统一 Key
/// 与远程模式属于本机安全边界，永远不从包里覆盖。
#[tauri::command]
pub async fn import_bundle(
    state: State<'_, AppState>,
    src: String,
) -> Result<ImportBundleOutcome, String> {
    let src_dir = std::path::Path::new(&src);
    // 1) 先只读解析，任何一步失败都在改动本机数据之前返回。
    let contents = crate::bundle::read_bundle(src_dir).await?;
    let imported_config = match contents.config_toml {
        Some(raw) => Some(
            toml::from_str::<AppConfig>(&raw).map_err(|e| format!("config.toml 解析失败：{e}"))?,
        ),
        None => None,
    };
    let usable = contents.providers;

    // 2) 备份现有数据目录，备份失败就中止，避免「改了但无法回退」。
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = crate::bundle::backup_data_dir(&crate::config::app_data_dir(), &timestamp)
        .await
        .map_err(|e| format!("备份当前数据目录失败，未导入：{e}"))?;

    let models_imported = usable.iter().map(|provider| provider.models.len()).sum();
    let providers_imported = usable.len();
    if !usable.is_empty() {
        replace_snapshot_providers(state.db.pool(), &usable).await?;
        state
            .gateway
            .reload_providers()
            .await
            .map_err(|e| e.to_string())?;
        *state.gateway.active.write() = None;
    }

    let mut preserved_security_fields = Vec::new();
    let mut config_imported = false;
    if let Some(mut cfg) = imported_config {
        let previous = state.config.read().clone();
        // 统一 Key 与远程模式属于本机安全边界，永远不从包里覆盖。
        preserved_security_fields.push("统一访问 Key".to_string());
        preserved_security_fields.push("远程 HTTPS 模式".to_string());
        cfg.unified_key = previous.unified_key.clone();
        cfg.remote_mode = previous.remote_mode.clone();
        cfg.normalize_custom_rules();
        cfg.validate_custom_rules().map_err(|e| e.to_string())?;
        cfg.normalize_listener();
        cfg.validate_remote_mode().map_err(|e| e.to_string())?;
        cfg.save().map_err(|e| e.to_string())?;
        *state.config.write() = cfg.clone();
        *state.gateway.cfg.write() = cfg.clone();
        state
            .gateway
            .router
            .set_custom_rules(cfg.custom_rules.clone());
        config_imported = true;
    }

    Ok(ImportBundleOutcome {
        result: ImportBundleResult {
            providers_imported,
            models_imported,
            providers_missing_key: contents.providers_missing_key,
            config_imported,
            preserved_security_fields,
        },
        backup_dir: backup.display().to_string(),
    })
}

/// 开机自启状态：是否已开启 + 已注册的命令行（用于界面回显）。
#[derive(serde::Serialize)]
pub struct AutostartView {
    /// 已注册的命令行；`None` 表示未开启
    pub command: Option<String>,
}

#[tauri::command]
pub fn get_autostart_state() -> AutostartView {
    // 读失败时返回未开启而不是报错：界面上的开关不该因为一个只读操作
    // 而变成红色错误条，用户会以为出事了。
    AutostartView {
        command: crate::autostart::status().unwrap_or(None),
    }
}

#[tauri::command]
pub fn set_autostart(enabled: bool) -> Result<AutostartView, String> {
    if enabled {
        crate::autostart::enable(None)?;
    } else {
        crate::autostart::disable()?;
    }
    Ok(AutostartView {
        command: crate::autostart::status().unwrap_or(None),
    })
}

/// 扫描失效模型。**只读**，不改任何数据。
///
/// 判定口径见 `stale_models`：先与上游目录做差集，拉不到目录的一律判
/// 「无法判定」而不是失效 —— 否则一次网络抖动就能让用户误删整家供应商。
#[tauri::command]
pub async fn scan_stale_models(
    state: State<'_, AppState>,
) -> Result<crate::stale_models::StaleScanResult, String> {
    let providers = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| format!("无法读取已保存的 Provider：{e}"))?;

    let mut entries = Vec::new();
    let mut catalog_unavailable = Vec::new();

    for provider in providers {
        if provider.models.is_empty() {
            continue;
        }
        // 拿上游目录。拿不到就整家标为「无法判定」。
        let catalog = fetch_catalog(&provider).await;
        if catalog.is_none() {
            catalog_unavailable.push(provider.name.clone());
        }
        entries.extend(crate::stale_models::scan_by_catalog(
            &provider,
            catalog.as_ref(),
        ));
    }

    let removable = entries.iter().filter(|e| e.is_removable()).count();
    tracing::info!(
        扫描总数 = entries.len(),
        可删除 = removable,
        目录不可用 = catalog_unavailable.len(),
        "失效模型扫描完成"
    );

    Ok(crate::stale_models::StaleScanResult {
        entries,
        catalog_unavailable,
        probed: false,
    })
}

/// 拉取某供应商的上游模型目录。失败返回 `None`（不是 `Err`）——
/// 调用方要把「拉不到」与「拉到但为空」区分开。
async fn fetch_catalog(provider: &Provider) -> Option<std::collections::BTreeSet<String>> {
    // 只有目录可信的方言才走这条路；Anthropic 的 /v1/models 只列自家模型，
    // 拿它判别家聚合站会把所有模型误判成失效。
    if !crate::stale_models::catalog_is_authoritative(provider.dialect) {
        return None;
    }
    let input = crate::model_catalog::DiscoveryInput {
        provider_id: Some(provider.id.clone()),
        dialect: provider.dialect,
        base_url: provider.base_url.clone(),
        api_key: String::new(),
    };
    let saved = provider.clone();
    crate::model_catalog::discover(&input, Some(&saved), None)
        .await
        .ok()
        .map(|response| response.models.into_iter().map(|m| m.id).collect())
}

/// 删除指定供应商下的若干模型（按 alias 精确匹配）。
///
/// 走 `upsert_provider` 整体回写而不是新加一条 DELETE —— providers 与 models
/// 本来就是整体替换的语义（见 `repo::upsert_provider` 的注释），另开一条
/// 删除路径只会让「删模型」与「改模型」有两套行为。
#[tauri::command]
pub async fn delete_models(
    state: State<'_, AppState>,
    provider_id: String,
    aliases: Vec<String>,
) -> Result<usize, String> {
    if aliases.is_empty() {
        return Ok(0);
    }
    let mut providers = repo::list_providers(state.db.pool())
        .await
        .map_err(|e| format!("无法读取已保存的 Provider：{e}"))?;
    let provider = providers
        .iter_mut()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| "供应商不存在".to_string())?;

    let before = provider.models.len();
    let targets: std::collections::HashSet<&str> = aliases.iter().map(|a| a.trim()).collect();
    provider
        .models
        .retain(|m| !targets.contains(m.alias.trim()));
    let removed = before - provider.models.len();
    if removed == 0 {
        return Ok(0);
    }

    let updated = provider.clone();
    repo::upsert_provider(state.db.pool(), &updated)
        .await
        .map_err(|e| format!("删除模型失败：{e}"))?;
    state
        .gateway
        .reload_providers()
        .await
        .map_err(|e| format!("删除模型失败：{e}"))?;
    tracing::info!(供应商 = %provider_id, 删除数量 = removed, "已删除失效模型");
    Ok(removed)
}
