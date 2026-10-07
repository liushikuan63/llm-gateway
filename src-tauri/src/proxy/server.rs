//! 本地网关 HTTP 服务 —— 整个方案的对外门面。
//!
//! 一次请求的完整生命周期：
//!
//!   鉴权 → 协议归一化(入) → 会话定位/上下文重建 → 粘性判定
//!        → 候选链解析与排序 → 降级执行 → 协议归一化(出)
//!        → 落库(上下文/用量/审计) → 返回
//!
//! 响应头里会带：
//!   X-Routed-Via        实际服务的 provider/model
//!   X-Fallback-Attempts 累计上游尝试次数（1 表示一次命中）
//!   X-Session-Id        会话 ID，客户端可回传来续接上下文

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Extension, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

use crate::config::{AppConfig, AuthFailureMode, RoutingStrategy};

use crate::context::{
    estimate_message_tokens, scoped_session_id, trim_to_budget, ContextStore, ExchangeWrite,
};
use crate::db::{self, repo};
use crate::domain::{
    ChatRequest, Content, FunctionCall, ImageUrl, Message, ModelType, Part, Provider,
    RemoteAccessKey, Role, ToolCall, Usage,
};
use crate::error::{GatewayError, Result};
use crate::proxy::health::HealthRegistry;
use crate::proxy::upstream::{PassthroughResponse, UpstreamClient, UpstreamEvent};
use crate::router::cascade::{CascadeRunRequest, CascadeStop, ConfidenceReading};
use crate::router::failover::{classify, AtomicFlag, AttemptOutcome, AttemptRecord, FailoverChain};

/// 鉴权失败复测确认之后的处理：按策略把这家供应商自动停用。
///
/// **只在这一刻动手**，而不是第一次 401 的时候——中转站的 401 未必等于凭据永久失效。
/// 写入是后台任务：降级链路不能被一次数据库写阻塞。
///
/// 豁免规则（用户「可以强制走用户选定的」）：主用供应商与配置里的豁免名单不自动停用，
/// 自动停用等于替用户改主意。
fn auth_confirm_handler(
    state: Arc<GatewayState>,
    cfg: AppConfig,
) -> impl FnMut(&crate::domain::Provider, &GatewayError) {
    move |provider, _error| {
        if cfg.auth_failure.mode != AuthFailureMode::SkipAndDisable || !provider.enabled {
            return;
        }
        let active = state.active.read().clone();
        if cfg.auth_failure.is_exempt(&provider.id, active.as_deref()) {
            tracing::info!(provider = %provider.id, "鉴权失败但命中豁免，保留供应商");
            return;
        }
        let state = state.clone();
        let provider_id = provider.id.clone();
        let provider_name = provider.name.clone();
        tokio::spawn(async move {
            auto_disable_provider(&state, &provider_id, &provider_name).await;
        });
    }
}

/// 把供应商标记为停用，并把原因**追加**到 `note`。
///
/// 追加而不是覆盖：备注里可能有用户自己写的内容，抹掉就再也找不回来。
async fn auto_disable_provider(state: &Arc<GatewayState>, provider_id: &str, provider_name: &str) {
    let Ok(list) = repo::list_providers(state.db.pool()).await else {
        return;
    };
    let Some(mut provider) = list.into_iter().find(|p| p.id == provider_id) else {
        return;
    };
    if !provider.enabled {
        return;
    }
    provider.enabled = false;
    let stamp = chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string();
    let reason = format!("上游连续返回 401/403 且复测确认凭据不可用（{provider_name}）");
    provider.note = Some(match provider.note.as_deref().map(str::trim) {
        Some(existing) if !existing.is_empty() => format!("{existing}｜{reason}（{stamp}）"),
        _ => format!("{reason}（{stamp}）"),
    });
    if repo::upsert_provider(state.db.pool(), &provider)
        .await
        .is_ok()
    {
        let _ = state.reload_providers().await;
        tracing::warn!(
            provider = %provider_id,
            "鉴权失败已确认，自动停用该供应商；恢复可在供应商页重新启用"
        );
    }
}
use crate::router::ratelimit::{Quota, RateLimiter};
use crate::router::Router as GatewayRouter;

/* ------------------------------ 共享状态 ------------------------------ */

pub struct GatewayState {
    pub db: db::Db,
    pub cfg: Arc<parking_lot::RwLock<AppConfig>>,
    pub health: Arc<HealthRegistry>,
    pub limiter: Arc<RateLimiter>,
    /// 远程访问 Key 的独立本地限流器，不与上游 Provider 配额混用。
    pub client_limiter: Arc<RateLimiter>,
    pub router: Arc<GatewayRouter>,
    pub upstream: Arc<UpstreamClient>,
    pub ctx: ContextStore,
    /// provider 列表缓存，避免每请求查库；由 UI 写入后主动 reload
    pub providers: Arc<parking_lot::RwLock<Vec<Provider>>>,
    /// 当前「主用」provider（CC Switch 式一键切换的目标）
    pub active: Arc<parking_lot::RwLock<Option<String>>>,
    /// 远程 Key 只缓存其哈希和配额，原始 Key 不会进入内存缓存或日志。
    pub remote_access_keys: Arc<parking_lot::RwLock<Vec<RemoteAccessKey>>>,
    /// 压缩会修改摘要和多条消息的 compacted 标记，必须串行化以避免较旧摘要覆盖
    /// 较新摘要。压缩是低频后台工作，使用全局锁可避免按会话锁表无限增长。
    compaction_lock: tokio::sync::Mutex<()>,
    /// 精确响应缓存。**默认关**：关着时 dispatch 不做哈希、不查表、不加头。
    pub cache: Arc<crate::cache::ResponseCache>,
    /// 只有实际 listener 位于回环地址时才会激活远程反代认证逻辑。
    listener_is_loopback: AtomicBool,
    /// 任务卡二 A5：账号型上游的适配器注册表。
    ///
    /// 放在 `GatewayState` 而不是只在 `AppState` 上：**分派点在代理这一侧**，
    /// 它拿到的只有 `GatewayState`。注册表是无状态只读的，`Arc` 共享，
    /// 两个 state 各持一份 `Arc` 即可。
    pub adapters: Arc<crate::agent_upstream::AdapterRegistry>,
    /// 任务卡二 B5 判据 5：**网关侧**的配额账本。
    ///
    /// 用 `parking_lot::Mutex` 而不是 `RwLock`：这个锁只在
    /// 「判定 + 记账」那一瞬间持有（几纳秒），没有读者可以并行的场景。
    /// 而且**判定与记账必须在同一个临界区**里 —— 分成两次加锁的话，
    /// 两个并发请求可能都判过、都记账，于是上限被突破一次。
    pub agent_quota: Arc<parking_lot::Mutex<crate::agent_upstream::QuotaBook>>,
    /// 任务卡二 B5 判据 4：**并发**闸门。
    ///
    /// 与 `agent_quota` 分开两把锁：它们的临界区互不相关，
    /// 合用一把会让「并发判定」被「配额记账」阻塞 —— 而并发闸门的
    /// 全部意义就是「立刻给出是与否」。
    pub agent_concurrency: Arc<parking_lot::Mutex<crate::agent_upstream::ConcurrencyGate>>,
}

impl GatewayState {
    pub fn new(db: db::Db, cfg: AppConfig) -> Self {
        let health = Arc::new(HealthRegistry::new());
        let limiter = Arc::new(RateLimiter::new());
        let router = Arc::new(GatewayRouter::with_custom_rules(
            limiter.clone(),
            health.clone(),
            cfg.custom_rules.clone(),
        ));
        let ctx = ContextStore::new(db.clone());
        let cache = Arc::new(crate::cache::ResponseCache::new(cfg.cache.clone()));

        Self {
            db,
            cfg: Arc::new(parking_lot::RwLock::new(cfg)),
            health,
            limiter,
            client_limiter: Arc::new(RateLimiter::new()),
            router,
            upstream: Arc::new(UpstreamClient::new()),
            ctx,
            cache,
            providers: Arc::new(parking_lot::RwLock::new(Vec::new())),
            active: Arc::new(parking_lot::RwLock::new(None)),
            remote_access_keys: Arc::new(parking_lot::RwLock::new(Vec::new())),
            compaction_lock: tokio::sync::Mutex::new(()),
            listener_is_loopback: AtomicBool::new(false),
            // A5：现在只有假适配器。`AppState` 那份由 lib.rs 各自构造 ——
            // 两处各建一份是**可以的**（适配器无状态），
            // 合成一个全局单例反而要多引入一层共享。
            adapters: Arc::new(crate::agent_upstream::AdapterRegistry::with_builtins()),
            agent_quota: Arc::new(parking_lot::Mutex::new(
                crate::agent_upstream::QuotaBook::new(),
            )),
            agent_concurrency: Arc::new(parking_lot::Mutex::new(
                crate::agent_upstream::ConcurrencyGate::new(),
            )),
        }
    }

    pub async fn reload_providers(&self) -> Result<()> {
        // 走 routable 变体：路由表里只放**已启用**的模型。
        //
        // 用的是 `list_routable_providers` 而不是 `list_providers` ——后者同时
        // 喂前端（需要看到已禁用的模型才能在界面上勾回来），需求与路由相反。
        // `models.enabled` 2026-10-05 才接进领域模型，此前SQL 里的
        // `AND enabled = 1` 只在读取时生效、INSERT 又不写这一列，禁用状态
        // 撑不过一次保存。
        let list = repo::list_routable_providers(self.db.pool()).await?;
        let remote_keys = repo::list_remote_access_keys(self.db.pool()).await?;
        *self.providers.write() = list;
        *self.remote_access_keys.write() = remote_keys;

        // 缓存失效挂在这里，而不是散在各个命令里。
        //
        // 理由：`reload_providers` 是供应商启停、模型增删改、价格刷新三类变更的
        // **唯一共同入口**（commands.rs 里 9 处 + pricing_refresh 都走它）。
        // 散着写就要维护一份「哪些命令会改路由」的清单，漏一个的症状是
        // 「改了配置却不生效」——缓存还在返回旧答案，而界面显示保存成功。
        //
        // 唯一不在它覆盖范围内的是 `update_config`（改的是 cfg 不是 providers），
        // 那里单独调一次。
        self.cache.invalidate_all();
        Ok(())
    }

    fn remote_mode_is_live(&self, cfg: &AppConfig) -> bool {
        cfg.remote_mode.enabled && self.listener_is_loopback.load(Ordering::Acquire)
    }

    /// 热切换：改的是内存里的路由目标，客户端 base_url 不变，因此无需重启 CLI
    pub fn set_active(&self, provider_id: &str) -> Result<()> {
        let exists = self
            .providers
            .read()
            .iter()
            .any(|p| p.id == provider_id && p.enabled);
        if !exists {
            return Err(GatewayError::ModelNotFound(format!(
                "provider `{provider_id}` 不存在或未启用"
            )));
        }
        *self.active.write() = Some(provider_id.to_string());
        tracing::info!("热切换生效 -> {provider_id}");
        Ok(())
    }

    pub fn cfg_snapshot(&self) -> AppConfig {
        self.cfg.read().clone()
    }

    /// 在同一条串行链路中重新读取会话、判定阈值并执行压缩。每个等待者都在拿锁
    /// 后重读数据库，因此不会基于陈旧摘要重复写入，也不会把已压缩的消息遗漏。
    async fn compact_session_if_needed(
        self: &Arc<Self>,
        session_id: &str,
        force: bool,
    ) -> Result<bool> {
        let _guard = self.compaction_lock.lock().await;
        let cfg = self.cfg_snapshot();
        let session = repo::get_or_create_session(self.db.pool(), session_id).await?;
        if !force && !self.ctx.needs_compaction(&session, &cfg).await {
            return Ok(false);
        }

        let previous_summary = session.summary.clone().unwrap_or_default();
        let summary_state = Arc::clone(self);
        let compacted = self
            .ctx
            .compact(&session, &cfg, move |messages| {
                let summary_state = summary_state.clone();
                let previous_summary = previous_summary.clone();
                async move { summary_state.summarize(messages, &previous_summary).await }
            })
            .await?;
        Ok(compacted.is_some())
    }

    /// 后台任务：日志清理 + Provider 快照 + 定价自动刷新
    pub async fn background_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        // 定价自动刷新按「天」节流：后台每 5 分钟醒一次，但只在距上次尝试超过
        // 24 小时后才真正发起网络请求，避免频繁打扰公共目录。
        let mut last_pricing_refresh: Option<std::time::Instant> = None;
        loop {
            tick.tick().await;
            let cfg = self.cfg_snapshot();
            if let Err(e) =
                repo::purge_old_requests(self.db.pool(), cfg.analytics_retention_days as i64).await
            {
                tracing::warn!("清理请求日志失败: {e}");
            }
            if let Err(e) = self.reload_providers().await {
                tracing::warn!("刷新 provider 失败: {e}");
            }
            if cfg.catalog_auto_update {
                let due = last_pricing_refresh
                    .map(|last| last.elapsed() >= Duration::from_secs(24 * 3600))
                    .unwrap_or(true);
                if due {
                    // 失败也要等下一个周期再试，避免目录不可用时反复重试。
                    last_pricing_refresh = Some(std::time::Instant::now());
                    if let Err(error) = crate::pricing_refresh::refresh(&self, false).await {
                        tracing::warn!("自动刷新定价失败: {error}");
                    }
                }
            } else {
                last_pricing_refresh = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{flattened_response_tool_name, restore_response_function_name};

    #[test]
    fn response_namespace_tool_names_round_trip() {
        let flat = flattened_response_tool_name("multi_agent_v1", "spawn_agent");
        assert_eq!(flat, "lgw__multi_agent_v1__spawn_agent");
        assert_eq!(
            restore_response_function_name(&flat),
            ("spawn_agent", Some("multi_agent_v1"))
        );
    }
}

/* ------------------------------ 服务启动 ------------------------------ */

pub async fn serve(state: Arc<GatewayState>) -> Result<()> {
    let cfg = state.cfg_snapshot();
    cfg.validate_remote_mode().map_err(GatewayError::Other)?;
    if cfg.remote_mode.enabled
        && !state
            .remote_access_keys
            .read()
            .iter()
            .any(|key| key.enabled)
    {
        return Err(GatewayError::Other(anyhow::anyhow!(
            "远程模式至少需要一个已启用的独立访问 Key"
        )));
    }
    let bind = cfg.bind.clone();
    let port = cfg.port;

    let app = Router::new()
        // OpenAI 兼容面（事实标准，绝大多数客户端走这里）
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(list_models))
        .route("/v1/responses", post(responses))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/images/generations", post(image_generations))
        .route("/v1/audio/speech", post(audio_speech))
        // Anthropic 原生面（Claude Code / Claude Desktop 走这里）
        .route("/v1/messages", post(anthropic_messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        // C2 Gemini 原生面（Gemini CLI / Gemini SDK 走这里）
        //
        // 路径里 `:generateContent` 之前那段是 `models/<模型名>`，
        // 而 `:` 不是路径分隔符，所以整段会作为一个路径参数进来，
        // 由 `gemini_inbound::parse_model_action` 拆开。
        // 这样写而不是用 `:model:action`：axum 只允许一个 `:` 参数前缀，
        // 写成两个会把 `:action` 当成字面量。
        .route("/v1beta/models", get(gemini_models))
        // 用**通配**而不是 `:model_action`：卡片刻意提醒「路径里的 {model}
        // 形如 models/gemini-2.5-pro」，也就是说前缀可能出现两次
        // （`/v1beta/models/models/gemini-2.5-pro:generateContent`）。
        // `:param` 只吃一段，第二种形态会 404；通配把余下整段都收进来，
        // 再由 `parse_model_action` 剥前缀。
        .route("/v1beta/models/*model_action", post(gemini_generate))
        // Ollama 仿真面（Zed / JetBrains AI 走这里）
        .route("/api/chat", post(ollama_chat))
        .route("/api/tags", get(ollama_tags))
        // 网关自面
        .route("/healthz", get(healthz))
        .route("/gw/stats", get(gw_stats))
        .route("/gw/health", get(gw_health))
        // 任务卡二 A8：Agent 型入口。**与 LLM 透传分开**（裁决之一）——
        // 它让外部 CLI 在本地干活并产出文件，不是一个「聊天补全」接口。
        .route("/gw/agent/run", post(gw_agent_run))
        // axum 0.7 推荐顺序：先 layer，最后 with_state
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth))
        // 非信任客户端的超大请求体会同时挤占内存和上游配额，按方案书限制为 8 MiB。
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(state.clone());

    let addr = format!("{bind}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| GatewayError::Other(e.into()))?;
    let is_loopback = listener
        .local_addr()
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or(false);
    state
        .listener_is_loopback
        .store(is_loopback, Ordering::Release);
    tracing::info!("LLM Gateway 已启动: http://{addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|e| GatewayError::Other(e.into()))?;
    Ok(())
}

/// 已认证调用方的审计标识。它不含原始 Key，允许后续分发链路安全落库。
#[derive(Debug, Clone, Default)]
struct AuthContext {
    client: Option<String>,
}

/// 鉴权中间件。
/// 放行 /healthz；本地模式使用统一 Key，远程 HTTPS 反代模式使用独立 Key。
async fn auth(
    State(state): State<Arc<GatewayState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Response {
    if req.uri().path() == "/healthz" {
        return next.run(req).await;
    }

    let cfg = state.cfg_snapshot();
    // 只信任来自回环地址的转发头。即使用户刚关闭远程模式，仍要识别旧
    // 反代送来的 `X-Forwarded-For`，否则它会在重启前被误判成“本机请求”。
    let proxied_client = peer.ip().is_loopback() && forwarded_client_ip(req.headers()).is_some();
    let external_client = !peer.ip().is_loopback() || proxied_client;

    // 即使允许局域网调用模型，管理接口也绝不暴露给外部或反代后的客户端。
    if req.uri().path().starts_with("/gw/") && external_client {
        return StatusCode::NOT_FOUND.into_response();
    }

    // 反代流量只能在远程模式已经实际生效时进入模型接口。这样配置切换
    // （尤其是由远程切回本地）不会留下一个可借本地统一 Key 访问的窗口。
    if proxied_client && !state.remote_mode_is_live(&cfg) {
        return remote_proxy_unavailable_response(
            "远程 HTTPS 反代模式未启用或尚未完成回环监听重启",
        );
    }
    // TLS 在受信回环反代处终止。只有该反代明确证明外层是 HTTPS 时，才接收
    // 远程 Key；缺失或伪造为 HTTP 的流量不能退化为本地统一 Key 路径。
    if proxied_client && state.remote_mode_is_live(&cfg) && !forwarded_proto_is_https(req.headers())
    {
        return remote_tls_required_response();
    }
    let provided = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .or_else(|| {
            req.headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        });

    // 配置已切到远程模式、但旧 listener 仍是 0.0.0.0 时，拒绝外部流量直到
    // 用户按 UI 提示重启。这样不会短暂用明文直连端口承接远程 Key。
    if cfg.remote_mode.enabled
        && external_client
        && !state.listener_is_loopback.load(Ordering::Acquire)
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "message": "远程模式配置已更新，请重启网关以启用回环反代监听",
                    "type": "server_error",
                    "code": null,
                }
            })),
        )
            .into_response();
    }

    let context = if state.remote_mode_is_live(&cfg) && external_client {
        let Some(provided) = provided else {
            return unauthorized_response();
        };
        let provided_hash = sha256_hex(&provided);
        let matched = state
            .remote_access_keys
            .read()
            .iter()
            .find(|key| key.enabled && constant_time_eq(&provided_hash, &key.key_hash))
            .cloned();
        let Some(key) = matched else {
            return unauthorized_response();
        };

        let limiter_key = format!("remote-key:{}", key.id);
        let quota = Quota {
            rpm: key.rpm_limit.max(1),
            ..Quota::default()
        };
        // 必须在同一个临界区检查并占用：分开的 allows + consume 会让并发远程
        // 请求同时观察到同一剩余额度，从而穿透单 Key RPM 上限。
        if !state.client_limiter.try_consume(&limiter_key, &quota, 0) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": {
                        "message": "该访问 Key 已达到本地请求速率上限",
                        "type": "rate_limit_exceeded",
                        "code": null,
                    }
                })),
            )
                .into_response();
        }

        // B2 预算闸门。放在这里（鉴权通过、占用 RPM 之后，路由打分之前）：
        // 前两步都不改数据，所以被预算拦下的请求不会留下任何痕迹。
        //
        // **被拦的请求不扣费、不进 requests 表** —— requests 只记真实发生的
        // 消费，把拒绝也写进去会让「本月花了多少」被自己的拒绝记录污染。
        if cfg.budget.enabled && key.monthly_budget_micros > 0 {
            let since = crate::budget::month_start_secs(chrono::Utc::now().timestamp());
            // 已知边界（卡片要求在注释里写明，不引数据库锁）：判定与记账之间
            // 有窗口，并发请求可能同时观察到同一份「未超」的累计值，
            // 于是极小概率超支一次。真要严格就用 requests 自增 id 做乐观扣减，
            // 那要事务；本卡按卡片口径接受这个窗口。
            let spent_rows = repo::sum_cost_by_currency_for_key(state.db.pool(), &key.id, since)
                .await
                .unwrap_or_default();
            let used = crate::budget::spent_in_currency(&spent_rows, &key.budget_currency);
            if let crate::budget::GateDecision::BudgetExceeded {
                used,
                limit,
                currency,
            } =
                crate::budget::check_budget(used, key.monthly_budget_micros, &key.budget_currency)
            {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(serde_json::json!({
                        "error": {
                            // 报具体的已用与上限，不返回笼统的「限流」——
                            // 用户看到 used/limit 才知道该充值还是该换 Key。
                            "message": format!(
                                "该访问 Key 的月度预算已用完：已用 {:.6} / 上限 {:.6} {currency}",
                                used as f64 / crate::budget::MICROS_PER_UNIT as f64,
                                limit as f64 / crate::budget::MICROS_PER_UNIT as f64,
                            ),
                            "type": "budget_exceeded",
                            "code": "budget_exceeded",
                            "used": used,
                            "limit": limit,
                            "currency": currency,
                        }
                    })),
                )
                    .into_response();
            }
        }

        AuthContext {
            client: Some(format!("remote-key:{}", key.id)),
        }
    } else {
        match provided {
            Some(key) if constant_time_eq(&key, &cfg.unified_key) => AuthContext {
                client: Some("local-unified-key".into()),
            },
            _ => return unauthorized_response(),
        }
    };

    req.extensions_mut().insert(context);
    next.run(req).await
}

fn remote_proxy_unavailable_response(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": {
                "message": message,
                "type": "server_error",
                "code": null,
            }
        })),
    )
        .into_response()
}

fn remote_tls_required_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": {
                "message": "远程反代请求必须携带受信的 X-Forwarded-Proto: https",
                "type": "forbidden",
                "code": null,
            }
        })),
    )
        .into_response()
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(GatewayError::Unauthorized("gateway key 不匹配".into()).to_openai_error()),
    )
        .into_response()
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .and_then(|value| value.parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_loopback())
}

fn forwarded_proto_is_https(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let mut protocols = value.split(',').map(str::trim);
    matches!(protocols.next(), Some(protocol) if protocol.eq_ignore_ascii_case("https"))
        && protocols.next().is_none()
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

/* ------------------------------ 处理器 ------------------------------ */

async fn healthz() -> &'static str {
    "ok"
}

async fn list_models(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    let models = GatewayRouter::public_models(&state.providers.read());
    Json(serde_json::json!({ "object": "list", "data": models }))
}

async fn gw_health(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "providers": state.health.snapshot(),
        "total_failovers": state.health.total_failovers(),
        "active": state.active.read().clone(),
    }))
}

async fn gw_stats(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    match repo::stats_overview(state.db.pool()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// 任务卡二 A8：Agent 型入口。
///
/// 请求体：`{ "runtime": "<runtime_id>", "model": "...", "prompt": "..." }`
///
/// ## 与 `/v1/chat/completions` 的区别（裁决之一）
///
/// 那个是 **LLM 透传**：发一轮对话、拿一段文本、不落任何文件。
/// 这个是 **Agent 型入口**：让外部 CLI 在隔离目录里干活、**产出文件**，
/// 并把产物清单报回来。两者刻意分开 —— 混在一起的话，
/// 「我这次会不会被写文件」就说不清了。
///
/// ## 状态码：一律 4xx，不用 500
///
/// 开关没开、运行时不存在、`runtime_id` 越界、适配器起不来 ——
/// 全是**用户能改的配置或环境问题**。给 500 会把他引向「服务端有 bug」
/// 这个错误方向。（与 A5 判据 3、A7 判据 2 同一条口径。）
#[derive(serde::Deserialize)]
struct AgentRunRequest {
    runtime: String,
    #[serde(default)]
    model: String,
    prompt: String,
}

async fn gw_agent_run(
    State(state): State<Arc<GatewayState>>,
    Extension(auth): Extension<AuthContext>,
    Json(req): Json<AgentRunRequest>,
) -> Response {
    let cfg = state.cfg_snapshot();
    let started = Instant::now();

    // ===== B5 判据 5：**网关自己的**配额，判定 + 记账同一个临界区 =====
    //
    // 卡片原文：「把日上限配成 2，第 3 次必须**被网关拒绝**，
    // 断言里的错误来源是**网关自己的配额器**，而不是上游返回的 429。」
    //
    // 顺序：**配额在跑之前判** —— 先跑再判的话，
    // 超限的那次已经在用户机器上跑完了（Agent 会写文件、花额度）。
    //
    // 判定与记账在同一个锁里：分成两次加锁的话，两个并发请求可能
    // 都判过、都记账，于是上限被突破一次。
    if let Err(rejection) =
        state
            .agent_quota
            .lock()
            .check_and_record(&req.runtime, cfg.agent.quota, chrono::Utc::now())
    {
        // **429**：与上游的 429 状态码相同，但错误体自报家门
        // （见 `QuotaRejection::message`）。用户能一眼看出是本地拦的。
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": rejection.message(),
                // 机器可读的来源标记 —— 让客户端不必解析自然语言。
                "source": "gateway_quota",
            })),
        )
            .into_response();
    }

    // ===== B5 判据 4：并发闸门。**不排队** —— 满了立刻拒 =====
    //
    // 顺序：配额 → 并发 → 执行。两道闸门都在**跑之前** ——
    // 超限的那次一旦跑起来就已经在用户机器上写了文件、花了额度。
    //
    // 用**独立的一把锁**（不与 `agent_quota` 合用）：并发闸门的全部意义
    // 就是「立刻给出是与否」，被配额记账阻塞就失去了意义。
    let limit = cfg.agent.max_concurrency;
    if let Err(rejection) = state.agent_concurrency.lock().try_enter(limit) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": rejection.message(),
                "source": "gateway_concurrency",
            })),
        )
            .into_response();
    }

    let outcome = crate::agent_upstream::run_agent_request(
        &cfg.agent,
        &state.adapters,
        &req.runtime,
        &req.model,
        &req.prompt,
    )
    .await;
    // **必须与上面的 `try_enter` 配对。** 漏掉这一句的后果是
    // 槽位永久占住 —— 跑过 N 次之后再也进不来（N = 上限）。
    // 它紧跟 `.await` 之后、在任何 `return` 之前，就是为了让「配对」
    // 这件事在代码里是**看得见的**。
    state.agent_concurrency.lock().leave();
    let latency_ms = started.elapsed().as_millis() as u64;

    // ===== 审计：`route_intent = "agent"` + 产物清单（卡片 A8 要求）=====
    //
    // 【为什么零 schema 变更】`attempts_json` 是**自由形态**的 TEXT 列，
    // 而 `route.intent` 已有独立列。语义上也对：`attempts_json` 是
    // 「这次请求逐次尝试的明细」，一次 Agent 执行正是一次尝试。
    //
    // 对一个「加列要改 11 处 `RequestLog` 字面量」的表来说，
    // 「不用加列」这件事很重要 —— 那笔账本会话连败两次，
    // 见 `docs/D批接续-交接单.md`。
    let detail = match &outcome {
        Ok(o) => serde_json::json!([{
            "runtime_kind": req.runtime,
            "transport": o.transport,
            "artifacts": o.artifacts,
            "ok": true,
        }]),
        Err(message) => serde_json::json!([{
            "runtime_kind": req.runtime,
            "error": message,
            "ok": false,
        }]),
    };
    let detail_json = serde_json::to_string(&detail).ok();
    let trace_id = crate::trace::new_trace_id();
    let status = if outcome.is_ok() { 200 } else { 400 };
    let error_text = outcome.as_ref().err().cloned();
    // 审计写失败**不该影响响应** —— 用户已经拿到结果了。
    // 但必须留一条日志，否则「审计丢了」这件事没人知道。
    if let Err(e) = repo::log_request(
        state.db.pool(),
        repo::RequestLog {
            session_id: None,
            client: auth.client.as_deref(),
            requested_model: &req.model,
            // 账号型上游里 `runtime_id` 就是「谁服务的」，
            // 与普通上游的 provider 同义。
            routed_provider: Some(&req.runtime),
            routed_model: Some(&req.model),
            status: Some(status),
            latency_ms: latency_ms as i64,
            prompt_tokens: 0,
            completion_tokens: 0,
            fallback_attempts: 0,
            error: error_text.as_deref(),
            cost: None,
            currency: None,
            rate_label: None,
            estimated_prompt_tokens: None,
            attempts_json: detail_json.as_deref(),
            access_key_id: crate::budget::access_key_id_of(auth.client.as_deref()),
            refined_prompt: None,
            trace_id: &trace_id,
            route: repo::RouteTrace {
                // 卡片原文要求新增 `route_intent=agent`。
                intent: Some("agent".to_string()),
                ..Default::default()
            },
        },
    )
    .await
    {
        tracing::warn!("Agent 执行的审计写入失败：{e}");
    }

    match outcome {
        Ok(outcome) => Json(serde_json::json!({
            "text": outcome.text,
            "transport": outcome.transport,
            "artifacts": outcome.artifacts,
        }))
        .into_response(),
        // 4xx：这些原因都在用户那一侧（配置 / 环境），不在服务端。
        Err(message) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": message,
            })),
        )
            .into_response(),
    }
}

async fn chat_completions(
    State(state): State<Arc<GatewayState>>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    match serde_json::from_value::<crate::protocol::openai::OaChatRequest>(body) {
        Ok(r) => match r.to_internal() {
            Ok(req) => dispatch(state, req, &headers, auth.client, Exit::OpenAI).await,
            Err(e) => error_response(&e),
        },
        Err(e) => error_response(&GatewayError::Protocol(format!("请求体解析失败: {e}"))),
    }
}

/// Anthropic 原生入口（Claude Code 走这里）
async fn anthropic_messages(
    State(state): State<Arc<GatewayState>>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    match serde_json::from_value::<crate::protocol::anthropic::AnthropicRequest>(body) {
        Ok(r) => match r.to_internal() {
            Ok(req) => dispatch(state, req, &headers, auth.client, Exit::Anthropic).await,
            Err(e) => error_response(&e),
        },
        Err(e) => error_response(&GatewayError::Protocol(format!("请求体解析失败: {e}"))),
    }
}

/// C2 `GET /v1beta/models`：Gemini 原生面的模型列表。
///
/// 返回的 `name` **带 `models/` 前缀** —— 客户端会把它原样拼回
/// `:generateContent` 的路径里，不带前缀会 404。
async fn gemini_models(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    let models: Vec<(String, String)> = GatewayRouter::public_models(&state.providers.read())
        .into_iter()
        .map(|m| (m.id.clone(), m.id))
        .collect();
    Json(crate::protocol::gemini_inbound::models_list(&models))
}

/// C2 `POST /v1beta/models/{model}:{action}`：Gemini 原生入站。
///
/// 走的是**同一条**已有 dispatch 路径（能力过滤、降级链、会话派生都在里面），
/// 没有为 Gemini 写旁路 —— 旁路意味着两套行为，测试成本翻倍且必然漂移。
async fn gemini_generate(
    State(state): State<Arc<GatewayState>>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    axum::extract::Path(model_action): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some((model, action)) = crate::protocol::gemini_inbound::parse_model_action(&model_action)
    else {
        // 路径不合法 ⇒ 明确的 404，而不是让 axum 回空响应
        return error_response(&GatewayError::ModelNotFound(format!(
            "路径 {model_action:?} 不是 <模型名>:<动作> 的形态"
        )));
    };
    let stream = match action.as_str() {
        "generateContent" => false,
        "streamGenerateContent" => true,
        other => {
            // 用 `ModelNotFound`（→404）而不是 `Protocol`（→500）：
            // 「你请求的动作不存在」是调用方的问题，不是网关坏了。
            return error_response(&GatewayError::ModelNotFound(format!(
                "动作 {other} 不存在（{model_action}）：Gemini 原生入站只提供 \
                 generateContent 与 streamGenerateContent"
            )));
        }
    };
    // `?alt=sse` 是 Gemini 的流式开关。**不认它也不报错**：有些客户端
    // 两个都发（路径动作 + alt=sse），而 `streamGenerateContent` 本身就隐含流式。
    // 但如果客户端**只**给了 `alt=sse` 而动作是 generateContent，那按非流式走 ——
    // 动作是权威，query 只是提示。
    let _ = query;

    let inbound: crate::protocol::gemini_inbound::GeminiInboundRequest =
        match serde_json::from_value(body) {
            Ok(v) => v,
            Err(e) => {
                return error_response(&GatewayError::Protocol(format!(
                    "Gemini 请求体解析失败: {e}"
                )))
            }
        };

    let req = match crate::protocol::gemini_inbound::to_internal(&model, &inbound, stream) {
        Ok(req) => req,
        Err(error) => return error_response(&error),
    };
    dispatch(state, req, &headers, auth.client, Exit::Gemini).await
}

/// Codex CLI 需要的 /v1/responses。
/// 结构上近似 chat/completions，本方案做最小适配：入参取 input（可以是字符串或消息数组），
/// 出参包一层 output。
async fn responses(
    State(state): State<Arc<GatewayState>>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("auto")
        .to_string();
    let stream = body
        .get("stream")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let compaction = responses_compaction_requested(body.get("input"));

    // `input` 既可能是纯字符串，也可能是 Responses 原生 item 数组。后者与
    // Chat Completions 的 content 类型不同（input_text/function_call_output 等），
    // 不能先强转成 OaMessage，否则会把真实文本和工具续轮静默清空。
    let messages = match responses_input_messages(body.get("input")) {
        Ok(messages) => messages,
        Err(error) => return error_response(&error),
    };

    let mut req = ChatRequest {
        model,
        messages,
        temperature: body
            .get("temperature")
            .and_then(|x| x.as_f64())
            .map(|x| x as f32),
        top_p: None,
        max_tokens: body
            .get("max_output_tokens")
            .and_then(|x| x.as_u64())
            .map(|x| x as u32),
        stop: None,
        stream,
        // Responses API 的 function tool 把 name/parameters 放在顶层，内部
        // 统一为 Chat Completions 风格，供 OpenAI/Gemini/Anthropic 复用转换层。
        tools: normalize_responses_tools(body.get("tools")),
        tool_choice: normalize_responses_tool_choice(body.get("tool_choice")),
        thinking: None,
        extra: Default::default(),
    };

    if compaction {
        // Codex Remote Compaction V2 把普通 /responses 请求末尾追加
        // compaction_trigger，并要求响应中恰好有一个 compaction item。上游若是
        // Chat Completions，不可能原生返回该协议项，因此先让模型生成结构化
        // 摘要，再由网关在出口包装成 Codex 能识别的协议项。
        req.tools = None;
        req.tool_choice = None;
        req.messages
            .insert(0, Message::system(crate::context::summarization_prompt("")));
        return dispatch_remote_compaction(state, req, &headers, auth.client, stream).await;
    }

    dispatch(state, req, &headers, auth.client, Exit::Responses).await
}

async fn embeddings(
    State(state): State<Arc<GatewayState>>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    passthrough_dispatch(
        state,
        &headers,
        auth.client,
        body,
        ModelType::Embedding,
        &["embeddings"],
        true,
    )
    .await
}

async fn image_generations(
    State(state): State<Arc<GatewayState>>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    passthrough_dispatch(
        state,
        &headers,
        auth.client,
        body,
        ModelType::Image,
        &["images", "generations"],
        true,
    )
    .await
}

async fn audio_speech(
    State(state): State<Arc<GatewayState>>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    passthrough_dispatch(
        state,
        &headers,
        auth.client,
        body,
        ModelType::Speech,
        &["audio", "speech"],
        false,
    )
    .await
}

const COMPACTION_PREFIX: &str = "llm-gateway-compaction-v1:";

fn responses_compaction_requested(input: Option<&serde_json::Value>) -> bool {
    input
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("compaction_trigger")
            })
        })
}

fn decode_compaction_content(value: &str) -> String {
    value
        .strip_prefix(COMPACTION_PREFIX)
        .unwrap_or(value)
        .to_string()
}

fn compaction_context(encrypted_content: &str) -> String {
    let summary = decode_compaction_content(encrypted_content);
    format!("【以下为远程压缩的上下文摘要，请据此保持上下文连贯】\n{summary}\n【摘要结束】")
}

fn compaction_item(response_id: &str, summary: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("cmp_{response_id}"),
        "type": "compaction",
        "encrypted_content": format!("{COMPACTION_PREFIX}{summary}"),
    })
}

fn passthrough_usage(value: &PassthroughResponse) -> (u32, u32, u32) {
    let PassthroughResponse::Json(value) = value else {
        return (0, 0, 0);
    };
    let usage = value.get("usage");
    let prompt = usage
        .and_then(|usage| usage.get("prompt_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let completion = usage
        .and_then(|usage| usage.get("completion_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let total = usage
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as u32)
        .unwrap_or(prompt.saturating_add(completion));
    (prompt, completion, total)
}

/// Embedding / 图片生成 / TTS 的统一非聊天派发。
///
/// 这些端点必须按 `ModelType` 过滤候选；不能把 embedding 模型降级成聊天模型，
/// 也不能把 TTS 请求发给不支持 `/audio/speech` 的 Anthropic/Gemini/Ollama 方言。
async fn passthrough_dispatch(
    state: Arc<GatewayState>,
    _headers: &HeaderMap,
    client: Option<String>,
    body: serde_json::Value,
    model_type: ModelType,
    path_segments: &[&str],
    expect_json: bool,
) -> Response {
    let requested_model = match body.get("model").and_then(serde_json::Value::as_str) {
        Some(model) if !model.trim().is_empty() => model.to_string(),
        _ => return error_response(&GatewayError::Protocol("非聊天请求缺少 model".into())),
    };
    let cfg = state.cfg_snapshot();
    let providers = state.providers.read().clone();
    let candidates = match state
        .router
        .resolve_typed(&requested_model, &providers, model_type)
    {
        Ok(candidates) => candidates,
        Err(error) => return error_response(&error),
    };
    if candidates.is_empty() {
        return error_response(&GatewayError::ModelNotFound(format!(
            "{requested_model}（类型 {}）",
            model_type.code()
        )));
    }

    let mut ranked = state
        .router
        .rank(candidates, &cfg, Default::default(), None);
    if let Some(active_id) = state.active.read().clone() {
        if let Some(position) = ranked
            .iter()
            .position(|candidate| candidate.provider.id == active_id)
        {
            let active = ranked.remove(position);
            ranked.insert(0, active);
        }
    }
    if ranked.is_empty() {
        return error_response(&GatewayError::CapabilityUnavailable {
            kind: format!("{} 模型当前没有可用候选", model_type.code()),
        });
    }

    let max_attempts = if cfg.failover_enabled {
        cfg.max_fallback_attempts
    } else {
        1
    };
    let flag = AtomicFlag::new();
    let chain = FailoverChain::new(&ranked, max_attempts, &flag);
    let chain = chain.with_auth_policy(cfg.auth_failure.mode, cfg.auth_failure.confirm_retries);
    let upstream = state.upstream.clone();
    let timeout = Duration::from_secs(cfg.upstream_timeout_secs);
    let body = Arc::new(body);
    let started = Instant::now();
    let mut attempt_records = Vec::new();
    let outcome = chain
        .run_with_auth_policy(
            &mut attempt_records,
            |provider, model| {
                let upstream = upstream.clone();
                let body = body.clone();
                async move {
                    upstream
                        .call_passthrough(
                            &provider,
                            &model,
                            path_segments,
                            &body,
                            timeout,
                            expect_json,
                        )
                        .await
                }
            },
            |provider, model, error| {
                if let GatewayError::Upstream { status: 429, .. } = error {
                    if let Some(model_ref) = provider
                        .models
                        .iter()
                        .find(|candidate| candidate.upstream == model)
                    {
                        state.router.mark_rate_limited(provider, model_ref);
                    }
                }
                state.health.record_failure(&provider.id, model, error);
            },
            auth_confirm_handler(state.clone(), cfg.clone()),
        )
        .await;

    match outcome {
        Ok(outcome) => {
            let latency = started.elapsed().as_millis() as u64;
            state
                .health
                .record_success(&outcome.provider_id, &outcome.model, latency as u32);
            let (prompt_tokens, completion_tokens, total_tokens) =
                passthrough_usage(&outcome.value);
            // D3：记一次实测吞吐样本。放在拿到 usage 之后 ——
            // 没有 `completion_tokens` 就算不出 tok/s，
            // 而在这里编一个 0 会把 EWMA 拉向 0，
            // 与「0 = 无样本」的约定撞车（`record_tps` 自己也会拒绝 0）。
            state.health.record_tps(
                &outcome.provider_id,
                &outcome.model,
                completion_tokens,
                latency as u32,
            );
            if let Some(provider) = ranked
                .iter()
                .find(|candidate| candidate.provider.id == outcome.provider_id)
            {
                if let Some(model) = provider
                    .provider
                    .models
                    .iter()
                    .find(|model| model.upstream == outcome.model)
                {
                    state
                        .router
                        .consume(&provider.provider, model, total_tokens);
                }
            }

            let audit_state = state.clone();
            let audit_client = client.clone();
            let audit_requested_model = requested_model.clone();
            let audit_provider = outcome.provider_id.clone();
            let audit_model = outcome.model.clone();
            let attempts = outcome.attempts;
            let attempts_json = attempts_json(&attempt_records);
            tokio::spawn(async move {
                let _ = repo::log_request(
                    audit_state.db.pool(),
                    repo::RequestLog {
                        session_id: None,
                        client: audit_client.as_deref(),
                        // 这次消费归属的远程 Key。本机统一 Key 的请求是 None。
                        // 与 client 同源派生，不另写一套前缀解析。
                        access_key_id: crate::budget::access_key_id_of(audit_client.as_deref()),
                        // 改写后的提示词默认不落库；只有开了 audit.store_refined_prompt
                        // 才会由 refined_prompt_to_store 返回脱敏后的文本。失败路径一律 None。
                        refined_prompt: None,
                        // 这条路径不在 dispatch 里（没有入口处的 traceId 可用），
                        // 就地生成一个：本地永远要有，不能因为路径不同就留空。
                        trace_id: &crate::trace::new_trace_id(),
                        requested_model: &audit_requested_model,
                        routed_provider: Some(&audit_provider),
                        routed_model: Some(&audit_model),
                        status: Some(200),
                        latency_ms: latency as i64,
                        prompt_tokens: prompt_tokens as i64,
                        completion_tokens: completion_tokens as i64,
                        fallback_attempts: fallback_count(attempts),
                        error: None,
                        cost: None,
                        currency: None,
                        rate_label: None,
                        estimated_prompt_tokens: None,
                        attempts_json: attempts_json.as_deref(),
                        route: Default::default(),
                    },
                )
                .await;
            });

            let mut response = match outcome.value {
                PassthroughResponse::Json(value) => Json(value).into_response(),
                PassthroughResponse::Bytes { body, content_type } => {
                    let mut response = Body::from(body).into_response();
                    response.headers_mut().insert(
                        "content-type",
                        parse_header(content_type.as_deref().unwrap_or("audio/mpeg")),
                    );
                    response
                }
            };
            response.headers_mut().insert(
                "x-routed-via",
                parse_header(&format!("{}/{}", outcome.provider_id, outcome.model)),
            );
            response.headers_mut().insert(
                "x-fallback-attempts",
                parse_header(&outcome.attempts.to_string()),
            );
            response
        }
        Err(error) => {
            let status = error.http_status().as_u16() as i64;
            let kind = classify(&error);
            // 用**尝试记录数**，不要从错误里反推：候选耗尽时 failover 返回的是
            // 最后一个上游错误（`Err(last_err.unwrap_or(AllProvidersFailed…))`），
            // `AllProvidersFailed` 成了走不到的死分支，反推恒得 1，于是审计里
            // `fallback_attempts` 恒为 0 —— 恰好抹掉「自动切换到底有没有生效」
            // 这个唯一的诊断信号。实测 2026-10-06 #94：链上试了 2 家（attempts_json
            // 有 2 条），fallback_attempts 却是 0。
            let attempts = attempt_records.len();
            let audit_state = state.clone();
            let audit_client = client.clone();
            let audit_requested_model = requested_model.clone();
            let attempts_json = attempts_json(&attempt_records);
            let latency = started.elapsed().as_millis() as i64;
            tokio::spawn(async move {
                let _ = repo::log_request(
                    audit_state.db.pool(),
                    repo::RequestLog {
                        session_id: None,
                        client: audit_client.as_deref(),
                        // 这次消费归属的远程 Key。本机统一 Key 的请求是 None。
                        // 与 client 同源派生，不另写一套前缀解析。
                        access_key_id: crate::budget::access_key_id_of(audit_client.as_deref()),
                        // 改写后的提示词默认不落库；只有开了 audit.store_refined_prompt
                        // 才会由 refined_prompt_to_store 返回脱敏后的文本。失败路径一律 None。
                        refined_prompt: None,
                        // 这条路径不在 dispatch 里（没有入口处的 traceId 可用），
                        // 就地生成一个：本地永远要有，不能因为路径不同就留空。
                        trace_id: &crate::trace::new_trace_id(),
                        requested_model: &audit_requested_model,
                        routed_provider: None,
                        routed_model: None,
                        status: Some(status),
                        latency_ms: latency,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        fallback_attempts: fallback_count(attempts),
                        error: Some(kind),
                        cost: None,
                        currency: None,
                        rate_label: None,
                        estimated_prompt_tokens: None,
                        attempts_json: attempts_json.as_deref(),
                        route: Default::default(),
                    },
                )
                .await;
            });
            error_response(&error)
        }
    }
}

/// Remote Compaction V2 专用出口。它不写回网关的会话历史：Codex 自己维护
/// compaction checkpoint，网关再把压缩摘要混入持久化上下文会造成双重摘要。
async fn dispatch_remote_compaction(
    state: Arc<GatewayState>,
    req: ChatRequest,
    headers: &HeaderMap,
    client: Option<String>,
    client_stream: bool,
) -> Response {
    let cfg = state.cfg_snapshot();
    let header_sid = headers
        .get("x-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let response_session_id = crate::context::derive_session_id(&req, header_sid.as_deref());

    let providers = state.providers.read().clone();
    let candidates = match state.router.resolve(&req.model, &providers) {
        Ok(candidates) => candidates,
        Err(error) => return error_response(&error),
    };

    let required = required_capabilities(&req);
    let media = crate::media::Media::of(&req);
    let mut ranked = state.router.rank(candidates, &cfg, required, None);
    let _media_rejections = filter_by_media_carry(&mut ranked, &media);
    if ranked.is_empty() {
        return error_response(&missing_capability_error(
            &required,
            &media,
            &_media_rejections,
        ));
    }
    if let Some(active_id) = state.active.read().clone() {
        if let Some(position) = ranked
            .iter()
            .position(|candidate| candidate.provider.id == active_id)
        {
            let active = ranked.remove(position);
            ranked.insert(0, active);
        }
    }

    let max_attempts = if cfg.failover_enabled {
        cfg.max_fallback_attempts
    } else {
        1
    };
    let flag = AtomicFlag::new();
    let chain = FailoverChain::new(&ranked, max_attempts, &flag);
    let chain = chain.with_auth_policy(cfg.auth_failure.mode, cfg.auth_failure.confirm_retries);
    let upstream = state.upstream.clone();
    let timeout = Duration::from_secs(cfg.upstream_timeout_secs);
    let upstream_req = Arc::new(ChatRequest {
        stream: false,
        ..req.clone()
    });
    let started = Instant::now();
    let mut attempt_records = Vec::new();
    let outcome = chain
        .run_with_auth_policy(
            &mut attempt_records,
            |provider, model| {
                let upstream = upstream.clone();
                let request = upstream_req.clone();
                let defaults = cfg.ollama_options.clone();
                async move {
                    upstream
                        .call(&provider, &request, &model, timeout, &defaults)
                        .await
                }
            },
            |provider, model, error| {
                if let GatewayError::Upstream { status: 429, .. } = error {
                    if let Some(model_ref) = provider
                        .models
                        .iter()
                        .find(|candidate| candidate.upstream == model)
                    {
                        state.router.mark_rate_limited(provider, model_ref);
                    }
                }
                state.health.record_failure(&provider.id, model, error);
            },
            auth_confirm_handler(state.clone(), cfg.clone()),
        )
        .await;

    match outcome {
        Ok(outcome) => {
            let summary = outcome.value.content.trim();
            if summary.is_empty() {
                return error_response(&GatewayError::Upstream {
                    provider: outcome.provider_id.clone(),
                    model: outcome.model.clone(),
                    status: StatusCode::BAD_GATEWAY.as_u16(),
                    body: "上游没有生成可用的压缩摘要".into(),
                });
            }

            let latency = started.elapsed().as_millis() as u64;
            state
                .health
                .record_success(&outcome.provider_id, &outcome.model, latency as u32);
            if let Some(provider) = ranked
                .iter()
                .find(|candidate| candidate.provider.id == outcome.provider_id)
            {
                if let Some(model) = provider
                    .provider
                    .models
                    .iter()
                    .find(|model| model.upstream == outcome.model)
                {
                    state.router.consume(
                        &provider.provider,
                        model,
                        outcome
                            .value
                            .usage
                            .as_ref()
                            .map(|usage| usage.total_tokens)
                            .unwrap_or(0),
                    );
                }
            }

            let response_id = format!("resp_compact_{}", uuid::Uuid::new_v4().simple());
            let item = compaction_item(&response_id, summary);
            let usage = outcome.value.usage.clone().unwrap_or_default();
            let audit_state = state.clone();
            let audit_session_id = response_session_id.clone();
            let audit_client = client.clone();
            let requested_model = req.model.clone();
            let routed_provider = outcome.provider_id.clone();
            let routed_model = outcome.model.clone();
            let attempts = outcome.attempts;
            let prompt_tokens = usage.prompt_tokens as i64;
            let completion_tokens = usage.completion_tokens as i64;
            let attempts_json = attempts_json(&attempt_records);
            tokio::spawn(async move {
                let _ = repo::log_request(
                    audit_state.db.pool(),
                    repo::RequestLog {
                        session_id: Some(&audit_session_id),
                        client: audit_client.as_deref(),
                        // 这次消费归属的远程 Key。本机统一 Key 的请求是 None。
                        // 与 client 同源派生，不另写一套前缀解析。
                        access_key_id: crate::budget::access_key_id_of(audit_client.as_deref()),
                        // 改写后的提示词默认不落库；只有开了 audit.store_refined_prompt
                        // 才会由 refined_prompt_to_store 返回脱敏后的文本。失败路径一律 None。
                        refined_prompt: None,
                        // 这条路径不在 dispatch 里（没有入口处的 traceId 可用），
                        // 就地生成一个：本地永远要有，不能因为路径不同就留空。
                        trace_id: &crate::trace::new_trace_id(),
                        requested_model: &requested_model,
                        routed_provider: Some(&routed_provider),
                        routed_model: Some(&routed_model),
                        status: Some(200),
                        latency_ms: latency as i64,
                        prompt_tokens,
                        completion_tokens,
                        fallback_attempts: fallback_count(attempts),
                        error: None,
                        cost: None,
                        currency: None,
                        rate_label: None,
                        estimated_prompt_tokens: None,
                        attempts_json: attempts_json.as_deref(),
                        route: Default::default(),
                    },
                )
                .await;
            });

            let payload = serde_json::json!({
                "id": response_id,
                "object": "response",
                "model": outcome.model,
                "status": "completed",
                "output": [item.clone()],
                "usage": responses_usage_json(&usage),
            });
            let mut response = if client_stream {
                let created = sse_event(
                    Some("response.created"),
                    serde_json::json!({
                        "type": "response.created",
                        "response": {
                            "id": response_id,
                            "object": "response",
                            "model": outcome.model,
                            "status": "in_progress",
                            "output": [],
                        },
                    })
                    .to_string(),
                );
                let item_done = sse_event(
                    Some("response.output_item.done"),
                    serde_json::json!({
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": item,
                    })
                    .to_string(),
                );
                let completed = sse_event(
                    Some("response.completed"),
                    serde_json::json!({
                        "type": "response.completed",
                        "response": payload,
                    })
                    .to_string(),
                );
                Body::from(format!("{created}{item_done}{completed}")).into_response()
            } else {
                Json(payload).into_response()
            };
            let response_headers = response.headers_mut();
            if client_stream {
                response_headers.insert(
                    "content-type",
                    parse_header("text/event-stream; charset=utf-8"),
                );
                response_headers.insert("cache-control", parse_header("no-cache"));
                response_headers.insert("x-accel-buffering", parse_header("no"));
            }
            response_headers.insert(
                "x-routed-via",
                parse_header(&format!("{}/{}", outcome.provider_id, outcome.model)),
            );
            response_headers.insert(
                "x-fallback-attempts",
                parse_header(&outcome.attempts.to_string()),
            );
            response_headers.insert("x-session-id", parse_header(&response_session_id));
            response
        }
        Err(error) => {
            let status = error.http_status().as_u16() as i64;
            let kind = classify(&error);
            // 用**尝试记录数**，不要从错误里反推：候选耗尽时 failover 返回的是
            // 最后一个上游错误（`Err(last_err.unwrap_or(AllProvidersFailed…))`），
            // `AllProvidersFailed` 成了走不到的死分支，反推恒得 1，于是审计里
            // `fallback_attempts` 恒为 0 —— 恰好抹掉「自动切换到底有没有生效」
            // 这个唯一的诊断信号。实测 2026-10-06 #94：链上试了 2 家（attempts_json
            // 有 2 条），fallback_attempts 却是 0。
            let attempts = attempt_records.len();
            let audit_state = state.clone();
            let audit_session_id = response_session_id.clone();
            let audit_client = client.clone();
            let requested_model = req.model.clone();
            let attempts_json = attempts_json(&attempt_records);
            let latency = started.elapsed().as_millis() as i64;
            tokio::spawn(async move {
                let _ = repo::log_request(
                    audit_state.db.pool(),
                    repo::RequestLog {
                        session_id: Some(&audit_session_id),
                        client: audit_client.as_deref(),
                        // 这次消费归属的远程 Key。本机统一 Key 的请求是 None。
                        // 与 client 同源派生，不另写一套前缀解析。
                        access_key_id: crate::budget::access_key_id_of(audit_client.as_deref()),
                        // 改写后的提示词默认不落库；只有开了 audit.store_refined_prompt
                        // 才会由 refined_prompt_to_store 返回脱敏后的文本。失败路径一律 None。
                        refined_prompt: None,
                        // 这条路径不在 dispatch 里（没有入口处的 traceId 可用），
                        // 就地生成一个：本地永远要有，不能因为路径不同就留空。
                        trace_id: &crate::trace::new_trace_id(),
                        requested_model: &requested_model,
                        routed_provider: None,
                        routed_model: None,
                        status: Some(status),
                        latency_ms: latency,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        fallback_attempts: fallback_count(attempts),
                        error: Some(kind),
                        cost: None,
                        currency: None,
                        rate_label: None,
                        estimated_prompt_tokens: None,
                        attempts_json: attempts_json.as_deref(),
                        route: Default::default(),
                    },
                )
                .await;
            });
            error_response(&error)
        }
    }
}

fn responses_input_messages(
    input: Option<&serde_json::Value>,
) -> std::result::Result<Vec<Message>, GatewayError> {
    match input {
        None => Ok(Vec::new()),
        Some(serde_json::Value::String(text)) => Ok(vec![Message::user(text)]),
        Some(serde_json::Value::Array(items)) => {
            let mut messages = Vec::with_capacity(items.len());
            for item in items {
                let kind = item.get("type").and_then(serde_json::Value::as_str);
                if kind == Some("compaction_trigger") {
                    // Remote Compaction V2 的请求控制项不是消息，不能送给上游。
                    continue;
                }
                messages.push(response_input_item(item)?);
            }
            Ok(messages)
        }
        Some(other) => Err(GatewayError::Protocol(format!(
            "Responses input 必须是字符串或 item 数组，实际为 {other}"
        ))),
    }
}

fn response_input_item(item: &serde_json::Value) -> std::result::Result<Message, GatewayError> {
    let kind = item.get("type").and_then(|value| value.as_str());
    match kind {
        Some("function_call") => {
            let call_id = response_required_string(item, "call_id")?;
            let name = response_function_name(item)?;
            let arguments = item
                .get("arguments")
                .map(json_value_as_text)
                .unwrap_or_else(|| "{}".into());
            Ok(Message {
                role: Role::Assistant,
                content: Content::Text(String::new()),
                tool_calls: Some(vec![ToolCall {
                    id: call_id,
                    kind: "function".into(),
                    function: FunctionCall { name, arguments },
                }]),
                tool_call_id: None,
                name: None,
            })
        }
        Some("function_call_output") => Ok(Message {
            role: Role::Tool,
            content: responses_content(item.get("output")),
            tool_calls: None,
            tool_call_id: Some(response_required_string(item, "call_id")?),
            name: None,
        }),
        Some("compaction") => {
            let encrypted_content = response_required_string(item, "encrypted_content")?;
            Ok(Message::system(compaction_context(&encrypted_content)))
        }
        Some("message") | None => {
            let role = match item.get("role").and_then(|value| value.as_str()) {
                Some("system") | Some("developer") => Role::System,
                Some("assistant") => Role::Assistant,
                Some("user") | None => Role::User,
                Some("tool") => Role::Tool,
                Some(other) => {
                    return Err(GatewayError::Protocol(format!(
                        "Responses input 不支持 role `{other}`"
                    )))
                }
            };
            Ok(Message {
                role,
                content: responses_content(item.get("content")),
                tool_calls: None,
                tool_call_id: item
                    .get("call_id")
                    .or_else(|| item.get("tool_call_id"))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                name: item
                    .get("name")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
            })
        }
        Some(other) => Err(GatewayError::Protocol(format!(
            "Responses input 不支持 item type `{other}`"
        ))),
    }
}

fn response_required_string(
    item: &serde_json::Value,
    key: &str,
) -> std::result::Result<String, GatewayError> {
    item.get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| GatewayError::Protocol(format!("Responses {key} 不能为空")))
}

fn responses_content(value: Option<&serde_json::Value>) -> Content {
    match value {
        None | Some(serde_json::Value::Null) => Content::Text(String::new()),
        Some(serde_json::Value::String(text)) => Content::Text(text.clone()),
        Some(serde_json::Value::Array(parts)) => {
            let mut content_parts = Vec::new();
            for part in parts {
                match part.get("type").and_then(|value| value.as_str()) {
                    Some("input_text") | Some("output_text") | Some("text") => {
                        content_parts.push(Part::Text {
                            text: part
                                .get("text")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default()
                                .to_owned(),
                        });
                    }
                    Some("input_image") | Some("image_url") => {
                        let image_url = part.get("image_url");
                        let url = image_url
                            .and_then(|value| value.as_str())
                            .or_else(|| {
                                image_url
                                    .and_then(|value| value.get("url"))
                                    .and_then(|value| value.as_str())
                            })
                            .unwrap_or_default()
                            .to_owned();
                        let detail = part
                            .get("detail")
                            .or_else(|| image_url.and_then(|value| value.get("detail")))
                            .and_then(|value| value.as_str())
                            .map(str::to_owned);
                        content_parts.push(Part::ImageUrl {
                            image_url: ImageUrl { url, detail },
                        });
                    }
                    Some("input_audio") => content_parts.push(Part::InputAudio {
                        input_audio: part
                            .get("input_audio")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({})),
                    }),
                    Some("input_video") | Some("video_url") => {
                        let video_url = part.get("video_url");
                        let url = video_url
                            .and_then(|value| value.as_str())
                            .or_else(|| {
                                video_url
                                    .and_then(|value| value.get("url"))
                                    .and_then(|value| value.as_str())
                            })
                            .or_else(|| part.get("url").and_then(|value| value.as_str()))
                            .unwrap_or_default()
                            .to_owned();
                        let detail = part
                            .get("detail")
                            .or_else(|| video_url.and_then(|value| value.get("detail")))
                            .and_then(|value| value.as_str())
                            .map(str::to_owned);
                        content_parts.push(Part::VideoUrl {
                            video_url: ImageUrl { url, detail },
                        });
                    }
                    // 未知 block 不应被静默丢弃；保留 JSON 文本至少让上游和日志
                    // 能看到内容，而不是错误地发送空消息。
                    Some(_) | None => content_parts.push(Part::Text {
                        text: part.to_string(),
                    }),
                }
            }
            if content_parts
                .iter()
                .all(|part| matches!(part, Part::Text { .. }))
            {
                Content::Text(
                    content_parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                )
            } else {
                Content::Parts(content_parts)
            }
        }
        Some(other) => Content::Text(other.to_string()),
    }
}

fn json_value_as_text(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn normalize_responses_tools(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let tools = value?.as_array()?;
    let mut normalized = Vec::new();
    for tool in tools {
        match tool.get("type").and_then(serde_json::Value::as_str) {
            Some("function") => {
                if let Some(function) = responses_function_tool(tool, None) {
                    normalized.push(function);
                }
            }
            Some("namespace") => {
                let Some(namespace) = tool
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .filter(|name| !name.is_empty())
                else {
                    continue;
                };
                let Some(children) = tool.get("tools").and_then(serde_json::Value::as_array) else {
                    continue;
                };
                for child in children {
                    if child.get("type").and_then(serde_json::Value::as_str) == Some("function") {
                        if let Some(function) = responses_function_tool(child, Some(namespace)) {
                            normalized.push(function);
                        }
                    }
                }
            }
            // Codex 还会发送 web_search、tool_search 等 ChatGPT 私有工具。
            // Chat Completions 没有对应声明，严格上游会直接 400；这里必须丢弃，
            // 不能原样透传，也不能伪造一个语义不同的 function。
            _ => {}
        }
    }
    (!normalized.is_empty()).then_some(serde_json::Value::Array(normalized))
}

fn responses_function_tool(
    tool: &serde_json::Value,
    namespace: Option<&str>,
) -> Option<serde_json::Value> {
    let function = tool.get("function").unwrap_or(tool);
    let name = function
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())?;
    let flat_name = namespace
        .map(|namespace| flattened_response_tool_name(namespace, name))
        .unwrap_or_else(|| name.to_string());
    let mut normalized = serde_json::Map::new();
    normalized.insert("name".into(), serde_json::Value::String(flat_name));
    for key in ["description", "parameters", "strict"] {
        if let Some(value) = function.get(key) {
            normalized.insert(key.to_owned(), value.clone());
        }
    }
    Some(serde_json::json!({
        "type": "function",
        "function": normalized,
    }))
}

const RESPONSE_TOOL_NAMESPACE_PREFIX: &str = "lgw__";

fn flattened_response_tool_name(namespace: &str, name: &str) -> String {
    format!("{RESPONSE_TOOL_NAMESPACE_PREFIX}{namespace}__{name}")
}

fn response_function_name(item: &serde_json::Value) -> Result<String> {
    let name = response_required_string(item, "name")?;
    Ok(item
        .get("namespace")
        .and_then(serde_json::Value::as_str)
        .filter(|namespace| !namespace.is_empty())
        .map(|namespace| flattened_response_tool_name(namespace, &name))
        .unwrap_or(name))
}

fn restore_response_function_name(name: &str) -> (&str, Option<&str>) {
    let Some(rest) = name.strip_prefix(RESPONSE_TOOL_NAMESPACE_PREFIX) else {
        return (name, None);
    };
    let Some((namespace, child)) = rest.split_once("__") else {
        return (name, None);
    };
    if namespace.is_empty() || child.is_empty() {
        (name, None)
    } else {
        (child, Some(namespace))
    }
}

fn normalize_responses_tool_choice(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let choice = value?;
    if choice.get("type").and_then(|value| value.as_str()) == Some("function")
        && choice.get("function").is_none()
    {
        if let Some(namespace) = choice
            .get("namespace")
            .and_then(serde_json::Value::as_str)
            .filter(|namespace| !namespace.is_empty())
        {
            let name = choice.get("name")?.as_str()?;
            return Some(serde_json::json!({
                "type": "function",
                "function": { "name": flattened_response_tool_name(namespace, name) },
            }));
        }
        let name = choice.get("name")?.clone();
        Some(serde_json::json!({
            "type": "function",
            "function": { "name": name },
        }))
    } else {
        Some(choice.clone())
    }
}

/// Ollama 仿真入口（让 Zed / JetBrains AI 这类只认 Ollama 的客户端也能接进来）
async fn ollama_chat(
    State(state): State<Arc<GatewayState>>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    match crate::protocol::ollama::ollama_request_to_internal(&body) {
        Ok(req) => dispatch(state, req, &headers, auth.client, Exit::Ollama).await,
        Err(error) => error_response(&error),
    }
}

async fn ollama_tags(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    let models = GatewayRouter::public_models(&state.providers.read());
    let data: Vec<serde_json::Value> = models
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "name": m.id,
                "model": m.id,
                "modified_at": chrono::Utc::now().to_rfc3339(),
                "size": 0,
                "digest": "",
                "details": { "family": "gateway", "parameter_size": "unknown" },
            })
        })
        .collect();
    Json(serde_json::json!({ "models": data }))
}

async fn count_tokens(Json(body): Json<serde_json::Value>) -> Response {
    // Claude Code 用此端点预估上下文。沿用内部统一消息的粗略 token 估算，
    // 保证估值与压缩触发条件一致，而不是固定返回一个误导性的常量。
    match serde_json::from_value::<crate::protocol::anthropic::AnthropicRequest>(body) {
        Ok(request) => match request.to_internal() {
            Ok(request) => {
                let input_tokens: u32 = request.messages.iter().map(Message::approx_tokens).sum();
                Json(serde_json::json!({ "input_tokens": input_tokens })).into_response()
            }
            Err(error) => error_response(&error),
        },
        Err(error) => error_response(&GatewayError::Protocol(format!("请求体解析失败: {error}"))),
    }
}

/* ---------------------------- 核心分发逻辑 ---------------------------- */

/// 出口方言：决定响应怎么编码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    OpenAI,
    Anthropic,
    Responses,
    Ollama,
    /// C2 Gemini 原生入站面。客户端是 Gemini CLI / Gemini SDK。
    Gemini,
}

/// 已完成会话定位、上下文重建和路由排序的一次内部派发。
/// 非流式与流式路径共享这组不可拆分的输入，避免两处函数签名继续膨胀。
struct DispatchInput {
    req: ChatRequest,
    new_messages: Vec<Message>,
    /// 客户端可见的逻辑会话 ID。远程模式下不能回显内部隔离后的存储键。
    response_session_id: String,
    session_id: String,
    client: Option<String>,
    ranked: Vec<crate::router::score::Candidate>,
    cfg: AppConfig,
    exit: Exit,
    /// 智能模式的判定结果。未开智能模式时为 `None`。
    intent: Option<crate::intellect::TaskIntent>,
    /// 联网搜索的可观测状态。未开搜索时为 `None`。
    search: Option<crate::search::executor::SearchOutcome>,
    /// 提示词预优化的结果。未开启或未触发时为 `None`。
    refine: Option<crate::intellect::RefineOutcome>,
    /// **客户端**提供的会话 id。没提供时是空串。
    ///
    /// 缓存键用它而不是 `session_id`：后者在客户端没提供时由网关随机生成，
    /// 每次请求都不同 —— 拿它做键会让每一条缓存都独一无二，
    /// 表现为「缓存永远不命中」，而功能看起来只是「没生效」。
    /// 卡片原文是「会话 id（**若有**）」，这个「若有」就是它。
    /// 实测证据：修之前两次完全相同的请求拿到
    /// `a-4ec77332-…` 与 `a-e4826059-…` 两个 session，键必然不同。
    cache_session: String,
    /// B4 贯穿本次请求的 traceId。入口处定一次，之后所有落库与响应头都用它。
    trace_id: String,
}

/// 智能模式 + 联网搜索在路由前的预处理结果。
///
/// 落点必须在**上下文重建之后、`resolve()` 之前**：注入的检索消息会改变 token
/// 估算，放晚了预算会算错；放早了上下文还不完整，分类器看到的不是真实请求。
struct RoutingPreflight {
    intent: Option<crate::intellect::TaskIntent>,
    search: Option<crate::search::executor::SearchOutcome>,
    /// 提示词预优化的结果。未开启或未触发时为 `None`。
    refine: Option<crate::intellect::RefineOutcome>,
}

/// 智能模式的**总开关**。
///
/// 这是唯一的权威口径：分类、联网搜索、以及虚拟模型名 `smart` 带来的排序权重
/// 全部由它决定。三者必须同进同退，否则会出现界面上无法解释的半吊子行为。
fn smart_mode_enabled(cfg: &AppConfig) -> bool {
    cfg.smart_routing.enabled
}

/// 判定本次请求要不要走智能模式。
///
/// 两条启用路径互不干扰：全局策略设为 `smart`，或客户端点名虚拟模型 `smart`。
/// **都不成立时返回 `false`，后续路径与改动前逐行等价。**
fn smart_mode_active(cfg: &AppConfig, requested_model: &str) -> bool {
    smart_mode_enabled(cfg)
        && (cfg.routing_strategy == RoutingStrategy::Smart || requested_model.trim() == "smart")
}

async fn run_routing_preflight(
    state: &Arc<GatewayState>,
    cfg: &AppConfig,
    req: &mut ChatRequest,
) -> RoutingPreflight {
    let mut preflight = RoutingPreflight {
        intent: None,
        search: None,
        refine: None,
    };
    if !smart_mode_active(cfg, &req.model) {
        return preflight;
    }

    let media = crate::media::Media::of(req);
    let user_text = {
        let input = crate::intellect::ClassifyInput {
            messages: &req.messages,
            media,
            has_tools: req.tools.is_some(),
            requested_model: &req.model,
        };
        // 决策端点不可达时按需拉起一次（默认关闭，开关与路径全由用户填）。
        //
        // **只在配置了端点时尝试**，且失败不阻断——拉不起来就是走启发式，
        // 请求照常发出。这一步每条请求都会走到，所以必须便宜：
        // `ensure_running` 内部先探 `/health`，端点在就立即返回。
        if cfg.smart_routing.jev.auto_start.enabled {
            match crate::intellect::autostart::ensure_running(
                &cfg.smart_routing.jev.auto_start,
                &cfg.smart_routing.jev.base_url,
            )
            .await
            {
                crate::intellect::SpawnOutcome::AlreadyRunning => {}
                crate::intellect::SpawnOutcome::Started { pid } => {
                    tracing::info!(pid, "已拉起本地决策服务，尚未就绪，本轮按弃权处理")
                }
                crate::intellect::SpawnOutcome::Refused(reason) => {
                    tracing::warn!("未拉起本地决策服务：{reason}")
                }
            }
        }

        // Jev 客户端按需构造：配置无效时只是拿不到决策信号，不是错误。
        let jev = crate::intellect::JevClient::new(
            &cfg.smart_routing.jev.base_url,
            &cfg.smart_routing.jev.model,
            cfg.smart_routing.jev.timeout_ms,
            cfg.smart_routing.jev.max_state_chars,
        )
        .ok();
        let intent = crate::intellect::classify(&input, &cfg.smart_routing, jev.as_ref()).await;
        tracing::info!(
            intent = intent.class.code(),
            classifier = intent.classifier.code(),
            complexity = intent.complexity,
            needs_web = intent.needs_web,
            jev_note = intent.jev_note.as_deref().unwrap_or(""),
            "智能模式分类完成"
        );
        preflight.intent = Some(intent);
        // 显式在这里结束 `input` 对 `req` 的不可变借用：
        // 下面预取要拿 `&mut req.messages`，两者不能重叠。
        input.last_user_text()
    };

    // 提示词预优化。排在联网搜索**之前**：改写后的提示词才是用户真正想问的东西，
    // 检索词应该从它身上取，否则会出现「按原文检索、按改写稿回答」的错位。
    //
    // 失败一律用原文，不阻断。
    let mut effective_text = user_text.clone();
    if cfg.smart_routing.prompt_refine.enabled {
        let wants_refine = preflight
            .intent
            .as_ref()
            .map(|intent| intent.needs_refine)
            .unwrap_or(false);
        if wants_refine {
            if let Some(outcome) = run_prompt_refine(state, cfg, &user_text).await {
                if outcome.applied {
                    effective_text = outcome.prompt.clone();
                    apply_refined_prompt(&mut req.messages, &outcome.prompt);
                }
                preflight.refine = Some(outcome);
            }
        }
    }

    // 联网搜索预取。失败不阻断请求——只把状态记下来回给客户端。
    if cfg.search.enabled {
        let needs_web = preflight
            .intent
            .as_ref()
            .map(|intent| intent.needs_web)
            .unwrap_or(false);
        if needs_web {
            let key = load_search_key(state).await;
            let outcome = crate::search::executor::prefetch(
                crate::search::executor::shared_client(),
                &cfg.search,
                key.as_deref(),
                &mut req.messages,
                &effective_text,
            )
            .await;
            if let Some(error) = outcome.error.as_deref() {
                tracing::warn!("联网搜索未生效：{error}");
            }
            preflight.search = Some(outcome);
        }
    }

    // 保留 `smart` 这个虚拟名，不改写成 `auto`：路由器认得它，会把本轮候选标记为
    // 虚拟策略覆盖，于是「客户端按请求点名 smart」在全局策略不是 smart 时也能生效，
    // 与 `fastest` / `smartest` 的既有行为一致。
    preflight
}

/// 跑一次提示词改写。返回 `None` 表示「压根没找到可用的改写目标」，
/// 调用方据此保持原文不变（连一次网络请求都不该发）。
async fn run_prompt_refine(
    state: &Arc<GatewayState>,
    cfg: &AppConfig,
    user_text: &str,
) -> Option<crate::intellect::RefineOutcome> {
    let providers = state.providers.read().clone();
    let required = crate::router::score::RequiredCapabilities::default();
    let ranked = state.router.rank(
        state.router.resolve("auto", &providers).ok()?,
        cfg,
        required,
        None,
    );
    let mut target =
        crate::intellect::refine::pick_target(&cfg.smart_routing.prompt_refine, &ranked)?;
    // 解密放在选目标之后：没选中就不用解密，避免白读一次密钥。
    let provider = providers
        .iter()
        .find(|p| p.base_url == target.base_url && p.dialect == target.dialect);
    let key = provider.and_then(|p| {
        if p.api_key_enc.trim().is_empty() {
            return None;
        }
        crate::crypto::decrypt(&p.api_key_enc).ok()
    });
    target.api_key = key;
    Some(
        crate::intellect::refine::refine(
            crate::search::executor::shared_client(),
            &target,
            target.api_key.clone(),
            user_text,
            &cfg.smart_routing.prompt_refine,
        )
        .await,
    )
}

/// 把改写后的文本替换进**最后一条 user 消息**。
///
/// 只替换那一条，不动历史：历史是已经发生的事实，改写它等于伪造上下文。
/// 找不到 user 消息时什么都不做——那说明请求只有 system 提示，改写无处可放。
fn apply_refined_prompt(messages: &mut [crate::domain::Message], refined: &str) {
    for message in messages.iter_mut().rev() {
        if message.role == crate::domain::Role::User {
            message.content = crate::domain::Content::Text(refined.to_owned());
            return;
        }
    }
}

/// 读取搜索后端 API Key 并解密。解密失败按「没有凭据」处理，
/// 不让一个坏密文阻断整个请求。
async fn load_search_key(state: &Arc<GatewayState>) -> Option<String> {
    let encoded = match repo::get_secret(state.db.pool(), repo::SECRET_SEARCH_API_KEY).await {
        Ok(Some(value)) => value,
        _ => return None,
    };
    match crate::crypto::decrypt(&encoded) {
        Ok(plain) => Some(plain),
        Err(error) => {
            tracing::warn!("搜索后端密钥解密失败，按未配置处理：{error}");
            None
        }
    }
}

impl DispatchInput {
    /// 智能模式 + 联网搜索的审计视图。关掉任一功能时对应字段为 `None`，
    /// 这与「判定为 false / 空结果」是两件事，界面上要分开显示。
    fn route_trace(&self) -> repo::RouteTrace {
        route_trace_of(
            self.intent.as_ref(),
            self.search.as_ref(),
            self.refine.as_ref(),
        )
    }
}

fn route_trace_of(
    intent: Option<&crate::intellect::TaskIntent>,
    search: Option<&crate::search::executor::SearchOutcome>,
    refine: Option<&crate::intellect::RefineOutcome>,
) -> repo::RouteTrace {
    repo::RouteTrace {
        intent: intent.map(|i| i.class.code().to_owned()),
        classifier: intent.map(|i| i.classifier.code().to_owned()),
        search: search.map(|outcome| match outcome.error {
            Some(_) => "failed".to_owned(),
            None => crate::search::backend_code(outcome.backend),
        }),
        search_hits: search.map(|outcome| outcome.hits as i64),
        refined: refine.map(|outcome| outcome.applied),
        refine_note: refine.map(|outcome| {
            outcome
                .reason
                .clone()
                .unwrap_or_else(|| format!("{}→{} 字", outcome.original_chars, outcome.final_chars))
        }),
    }
}

/// 把诊断头写进响应。**未开智能模式/搜索时一个都不写**——
/// 输出 `none` 这类空值会让客户端误以为网关做了判定但结论是「无」。
fn apply_route_headers(trace: &repo::RouteTrace, headers: &mut HeaderMap) {
    if let Some(intent) = trace.intent.as_deref() {
        headers.insert("x-route-intent", parse_header(intent));
    }
    if let Some(classifier) = trace.classifier.as_deref() {
        headers.insert("x-route-classifier", parse_header(classifier));
    }
    if let Some(search) = trace.search.as_deref() {
        headers.insert("x-route-search", parse_header(search));
        headers.insert(
            "x-route-search-hits",
            parse_header(&trace.search_hits.unwrap_or(0).to_string()),
        );
    }
    // 只在真的触发了改写链路时才发。没触发时一个头都不多，
    // 这样「客户端看到 refined 头」就等于「提示词确实被动过」。
    if let Some(refined) = trace.refined {
        headers.insert(
            "x-route-refined",
            parse_header(if refined { "1" } else { "0" }),
        );
        if let Some(note) = trace.refine_note.as_deref() {
            headers.insert("x-route-refine-note", parse_header(note));
        }
    }
}

/// Anthropic SSE 不是“把文本 delta 换个 event 名”即可：客户端会按 content
/// block 的开始、增量、关闭顺序组装消息。这里维护出口侧索引，绝不复用来自
/// OpenAI/Gemini/Ollama 的工具索引，避免文字块和工具块发生碰撞。
struct AnthropicStreamState {
    next_output_index: usize,
    active_text_index: Option<usize>,
    tools: BTreeMap<usize, AnthropicToolBlock>,
}

#[derive(Default)]
struct AnthropicToolBlock {
    output_index: Option<usize>,
    id: Option<String>,
    name: Option<String>,
    pending_arguments: String,
}

impl AnthropicStreamState {
    fn new(req_id: &str, model: &str, initial_usage: Option<&Usage>) -> (Self, String) {
        let usage = initial_usage.cloned().unwrap_or_default();
        let mut stream_usage = serde_json::json!({
            "input_tokens": usage.normal_input_tokens(),
            "output_tokens": 0,
        });
        if usage.cache_read_tokens > 0 {
            stream_usage["cache_read_input_tokens"] = serde_json::json!(usage.cache_read_tokens);
        }
        if usage.cache_creation_tokens > 0 {
            stream_usage["cache_creation_input_tokens"] =
                serde_json::json!(usage.cache_creation_tokens);
        }
        let start = sse_event(
            Some("message_start"),
            serde_json::json!({
                "type": "message_start",
                "message": {
                    "id": req_id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": stream_usage,
                },
            })
            .to_string(),
        );
        (
            Self {
                next_output_index: 0,
                active_text_index: None,
                tools: BTreeMap::new(),
            },
            start,
        )
    }

    fn text_delta(&mut self, text: &str) -> Vec<String> {
        let mut output = self.close_tools();
        let index = match self.active_text_index {
            Some(index) => index,
            None => {
                let index = self.allocate_output_index();
                self.active_text_index = Some(index);
                output.push(sse_event(
                    Some("content_block_start"),
                    serde_json::json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": { "type": "text", "text": "" },
                    })
                    .to_string(),
                ));
                index
            }
        };
        output.push(sse_event(
            Some("content_block_delta"),
            serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text },
            })
            .to_string(),
        ));
        output
    }

    fn tool_deltas(&mut self, calls: &serde_json::Value) -> Vec<String> {
        let mut output = self.close_text();
        for (position, call) in tool_call_values(calls).into_iter().enumerate() {
            let source_index = call
                .get("index")
                .and_then(|value| value.as_u64())
                .unwrap_or(position as u64) as usize;
            let id = call
                .get("id")
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            let name = call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            let arguments = call
                .get("function")
                .and_then(|function| function.get("arguments"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned();

            let mut start = None;
            let mut delta = None;
            {
                let block = self.tools.entry(source_index).or_default();
                if id.is_some() {
                    block.id = id;
                }
                if name.is_some() {
                    block.name = name;
                }
                if let Some(index) = block.output_index {
                    if !arguments.is_empty() {
                        delta = Some((index, arguments));
                    }
                } else {
                    block.pending_arguments.push_str(&arguments);
                    if let (Some(id), Some(name)) = (block.id.clone(), block.name.clone()) {
                        start = Some((id, name, std::mem::take(&mut block.pending_arguments)));
                    }
                }
            }

            if let Some((id, name, initial_arguments)) = start {
                let output_index = self.allocate_output_index();
                self.tools
                    .get_mut(&source_index)
                    .expect("tool block was inserted above")
                    .output_index = Some(output_index);
                output.push(sse_event(
                    Some("content_block_start"),
                    serde_json::json!({
                        "type": "content_block_start",
                        "index": output_index,
                        "content_block": {
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": {},
                        },
                    })
                    .to_string(),
                ));
                if !initial_arguments.is_empty() {
                    output.push(anthropic_input_json_delta(output_index, &initial_arguments));
                }
            } else if let Some((output_index, arguments)) = delta {
                output.push(anthropic_input_json_delta(output_index, &arguments));
            }
        }
        output
    }

    fn finish(&mut self) -> Vec<String> {
        let mut output = self.close_text();
        output.extend(self.close_tools());
        output
    }

    fn allocate_output_index(&mut self) -> usize {
        let index = self.next_output_index;
        self.next_output_index += 1;
        index
    }

    fn close_text(&mut self) -> Vec<String> {
        self.active_text_index
            .take()
            .map(anthropic_content_block_stop)
            .into_iter()
            .collect()
    }

    fn close_tools(&mut self) -> Vec<String> {
        let mut indexes: Vec<usize> = self
            .tools
            .values()
            .filter_map(|block| block.output_index)
            .collect();
        indexes.sort_unstable();
        self.tools.clear();
        indexes
            .into_iter()
            .map(anthropic_content_block_stop)
            .collect()
    }
}

/// Responses API 用独立 output item 表达文本和工具调用；其 SSE 客户端不会从
/// 裸 delta 猜测 item，因此完整生命周期在网关侧显式维护。
struct ResponsesStreamState {
    response_id: String,
    next_output_index: usize,
    text: Option<ResponseTextBlock>,
    tools: BTreeMap<usize, ResponseToolBlock>,
}

struct ResponseTextBlock {
    output_index: usize,
    item_id: String,
    text: String,
}

#[derive(Default)]
struct ResponseToolBlock {
    output_index: Option<usize>,
    item_id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ResponsesStreamState {
    fn new(req_id: &str, model: &str) -> (Self, String) {
        let state = Self {
            response_id: req_id.to_owned(),
            next_output_index: 0,
            text: None,
            tools: BTreeMap::new(),
        };
        let created = response_event(
            "response.created",
            serde_json::json!({
                "type": "response.created",
                "response": {
                    "id": req_id,
                    "object": "response",
                    "model": model,
                    "status": "in_progress",
                    "output": [],
                },
            }),
        );
        (state, created)
    }

    fn text_delta(&mut self, delta: &str) -> Vec<String> {
        let mut output = Vec::new();
        if self.text.is_none() {
            let output_index = self.allocate_output_index();
            let item_id = format!("msg_{}", self.response_id);
            output.push(response_event(
                "response.output_item.added",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "id": item_id,
                        "type": "message",
                        "status": "in_progress",
                        "role": "assistant",
                        "content": [],
                    },
                }),
            ));
            output.push(response_event(
                "response.content_part.added",
                serde_json::json!({
                    "type": "response.content_part.added",
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": { "type": "output_text", "text": "" },
                }),
            ));
            self.text = Some(ResponseTextBlock {
                output_index,
                item_id,
                text: String::new(),
            });
        }
        let text = self
            .text
            .as_mut()
            .expect("text block was initialized above");
        text.text.push_str(delta);
        output.push(response_event(
            "response.output_text.delta",
            serde_json::json!({
                "type": "response.output_text.delta",
                "item_id": text.item_id,
                "output_index": text.output_index,
                "content_index": 0,
                "delta": delta,
            }),
        ));
        output
    }

    fn tool_deltas(&mut self, calls: &serde_json::Value) -> Vec<String> {
        let mut output = Vec::new();
        for (position, call) in tool_call_values(calls).into_iter().enumerate() {
            let source_index = call
                .get("index")
                .and_then(|value| value.as_u64())
                .unwrap_or(position as u64) as usize;
            let call_id = call
                .get("id")
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            let name = call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            let arguments = call
                .get("function")
                .and_then(|function| function.get("arguments"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned();

            let mut start = None;
            let mut delta = None;
            {
                let block = self.tools.entry(source_index).or_default();
                if call_id.is_some() {
                    block.call_id = call_id;
                }
                if name.is_some() {
                    block.name = name;
                }
                if let Some(output_index) = block.output_index {
                    if !arguments.is_empty() {
                        block.arguments.push_str(&arguments);
                        delta = Some((
                            block
                                .item_id
                                .clone()
                                .expect("started tools have an item id"),
                            output_index,
                            block.arguments.clone(),
                            arguments,
                        ));
                    }
                } else {
                    block.arguments.push_str(&arguments);
                    if let (Some(call_id), Some(name)) = (block.call_id.clone(), block.name.clone())
                    {
                        start = Some((call_id, name, block.arguments.clone()));
                    }
                }
            }

            if let Some((call_id, name, initial_arguments)) = start {
                let output_index = self.allocate_output_index();
                let item_id = format!("fc_{}_{}", self.response_id, source_index);
                let (name, namespace) = restore_response_function_name(&name);
                let mut item = serde_json::json!({
                    "id": item_id,
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": call_id,
                    "name": name,
                    "arguments": "",
                });
                if let Some(namespace) = namespace {
                    item["namespace"] = serde_json::json!(namespace);
                }
                let block = self
                    .tools
                    .get_mut(&source_index)
                    .expect("tool block was inserted above");
                block.output_index = Some(output_index);
                block.item_id = Some(item_id.clone());
                output.push(response_event(
                    "response.output_item.added",
                    serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": item,
                    }),
                ));
                if !initial_arguments.is_empty() {
                    output.push(response_function_arguments_delta(
                        block.item_id.as_deref().unwrap_or_default(),
                        output_index,
                        &initial_arguments,
                    ));
                }
            } else if let Some((item_id, output_index, _all_arguments, arguments)) = delta {
                output.push(response_function_arguments_delta(
                    &item_id,
                    output_index,
                    &arguments,
                ));
            }
        }
        output
    }

    fn finish(&mut self) -> Vec<String> {
        let mut output = Vec::new();
        if let Some(text) = self.text.take() {
            output.push(response_event(
                "response.output_text.done",
                serde_json::json!({
                    "type": "response.output_text.done",
                    "item_id": text.item_id,
                    "output_index": text.output_index,
                    "content_index": 0,
                    "text": text.text,
                }),
            ));
            output.push(response_event(
                "response.content_part.done",
                serde_json::json!({
                    "type": "response.content_part.done",
                    "item_id": text.item_id,
                    "output_index": text.output_index,
                    "content_index": 0,
                    "part": { "type": "output_text", "text": text.text },
                }),
            ));
            output.push(response_event(
                "response.output_item.done",
                serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": text.output_index,
                    "item": {
                        "id": text.item_id,
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text.text }],
                    },
                }),
            ));
        }
        for block in self.tools.values() {
            let (Some(item_id), Some(output_index), Some(call_id), Some(name)) = (
                block.item_id.as_deref(),
                block.output_index,
                block.call_id.as_deref(),
                block.name.as_deref(),
            ) else {
                continue;
            };
            output.push(response_event(
                "response.function_call_arguments.done",
                serde_json::json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": item_id,
                    "output_index": output_index,
                    "call_id": call_id,
                    "name": restore_response_function_name(name).0,
                    "arguments": block.arguments,
                }),
            ));
            let (name, namespace) = restore_response_function_name(name);
            let mut item = serde_json::json!({
                "id": item_id,
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "name": name,
                "arguments": block.arguments,
            });
            if let Some(namespace) = namespace {
                item["namespace"] = serde_json::json!(namespace);
            }
            output.push(response_event(
                "response.output_item.done",
                serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": item,
                }),
            ));
        }
        output
    }

    fn allocate_output_index(&mut self) -> usize {
        let index = self.next_output_index;
        self.next_output_index += 1;
        index
    }
}

async fn dispatch(
    state: Arc<GatewayState>,
    mut req: ChatRequest,
    headers: &HeaderMap,
    client: Option<String>,
    exit: Exit,
) -> Response {
    let cfg = state.cfg_snapshot();

    // B4 traceId 在**入口处定一次**。之后所有落库、响应头、span 都用它。
    //
    // 客户端给了就透传（便于跨服务串联），但**必须清洗**：
    // 这个值会被写进响应头，原样回显等于开了一个 HTTP 头注入面
    // （客户端发 `X-Trace-Id: abc\r\nX-Injected: 1` 就能注入一个新头）。
    // 清洗是白名单（只留 hex 与短横线），不是黑名单。
    let trace_id =
        crate::trace::resolve_trace_id(headers.get("x-trace-id").and_then(|v| v.to_str().ok()));

    // B2 模型白名单闸门。放在**路由打分之前**，卡片点名的位置。
    //
    // 为什么预算在中间件判、白名单在这里判：白名单要比的是**请求的模型名**，
    // 而那在 body 里，中间件阶段还没解析 body。两处都在「鉴权之后、
    // 打分之前」，符合卡片的位置要求。
    //
    // 被拒的请求**不扣费、不进 requests 表** —— 与预算闸门同一口径。
    if cfg.budget.enabled {
        if let Some(key_id) = crate::budget::access_key_id_of(client.as_deref()) {
            let whitelist = state
                .remote_access_keys
                .read()
                .iter()
                .find(|k| k.id == key_id)
                .map(|k| k.allowed_models.clone())
                .unwrap_or_default();
            if !crate::budget::model_allowed(&whitelist, &req.model) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!(
                                "该访问 Key 不允许使用模型 {}（白名单：{}）",
                                req.model,
                                whitelist.join(", "),
                            ),
                            "type": "model_not_allowed",
                            "code": "model_not_allowed",
                            // 写明被拒的模型名，客户端才知道该换哪一个
                            "model": req.model,
                        }
                    })),
                )
                    .into_response();
            }
        }
    }

    // 1) 会话定位
    let header_sid = headers
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let response_session_id = crate::context::derive_session_id(&req, header_sid.as_deref());
    // `remote-key:<id>` 由认证中间件生成，不接受调用方输入；内部键只落数据库，
    // 对外继续回显逻辑 ID，避免泄露命名空间并保持客户端续接方式不变。
    let session_id = scoped_session_id(&response_session_id, client.as_deref());

    let first_user_text = req
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| m.content_text())
        .unwrap_or_default();

    let mut session = match state.ctx.touch(&session_id, &first_user_text).await {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };

    // 先处理已经超出常规阈值的会话。这样下一轮请求不会与刚刚完成的异步压缩
    // 竞争；拿到单飞锁后会重新读取会话，确保摘要不会被陈旧任务覆盖。
    if state.ctx.needs_compaction(&session, &cfg).await {
        match state.compact_session_if_needed(&session_id, false).await {
            Ok(true) => match repo::get_or_create_session(state.db.pool(), &session_id).await {
                Ok(refreshed) => session = refreshed,
                Err(error) => return error_response(&error),
            },
            Ok(false) => {}
            Err(error) => tracing::warn!("请求前自动压缩失败，将继续执行容量保护: {error}"),
        }
    }

    // 2) 上下文重建：把落库的历史拼回请求里，同时保留本轮真正新增的
    // 输入尾部，供响应成功后原子写回。不能从“最后一条 user”猜测：工具
    // 续轮可能只有 assistant tool_calls + tool result。
    let incoming_messages = req.messages.clone();
    let mut new_messages = match state
        .ctx
        .prepare_context(&session, &req.messages, CONTEXT_HISTORY_LIMIT)
        .await
    {
        Ok(prepared) => {
            req.messages = prepared.messages;
            prepared.new_messages
        }
        Err(e) => {
            tracing::warn!("上下文重建失败，使用原始请求: {e}");
            incoming_messages.clone()
        }
    };

    // 3) 粘性判定
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let sticky = match (
        session.sticky_provider_id.as_deref(),
        session.sticky_model.as_deref(),
        session.sticky_expires_at,
    ) {
        (Some(p), Some(m), Some(exp)) if exp > now => Some((p.to_string(), m.to_string())),
        _ => None,
    };

    // 4) 智能模式分类与联网搜索预取。必须夹在上下文重建之后、`resolve()` 之前：
    //    注入的检索消息会改变 token 估算，而分类器需要看到完整历史。
    let preflight = run_routing_preflight(&state, &cfg, &mut req).await;

    // 5) 候选链
    let providers = state.providers.read().clone();
    // 总开关关着时，虚拟模型名 `smart` 不产生任何效果——否则会出现
    // 「不分类、不搜索，但排序已经换成 Smart 权重」的半吊子状态。
    let smart_enabled = smart_mode_enabled(&cfg);
    let candidates = match state.router.resolve_typed_with(
        &req.model,
        &providers,
        ModelType::Chat,
        smart_enabled,
    ) {
        Ok(c) => c,
        Err(e) => return error_response(&e),
    };

    // 路由前按候选窗口扣除输出、工具 schema 和协议余量。先强制压缩可压缩的
    // 持久化历史，再在必要时按完整工具交换裁剪；仍放不下时明确报错，绝不把
    // 超窗 payload 交给小窗口备选 Provider。
    //
    // 估算校准：本地按字符估算，与上游真实 token 之间存在系统性偏差。用日志里
    // 「实际 / 估算」的 EWMA 比值保守换算，避免把超窗 payload 送进窗口。
    let calibration = state.calibration_snapshot().await;
    let fixed_reserve = fixed_context_reserve(&req, &candidates);
    if let Some(max_message_budget) = max_message_budget(&candidates, fixed_reserve) {
        let conservative_ratio = conservative_ratio(&candidates, &calibration);
        let scaled_budget = (max_message_budget as f64 / conservative_ratio).floor() as u32;
        if scaled(estimate_message_tokens(&req.messages), conservative_ratio) > max_message_budget {
            match state.compact_session_if_needed(&session_id, true).await {
                Ok(true) => {
                    session = match repo::get_or_create_session(state.db.pool(), &session_id).await
                    {
                        Ok(refreshed) => refreshed,
                        Err(error) => return error_response(&error),
                    };
                    match state
                        .ctx
                        .prepare_context(&session, &incoming_messages, CONTEXT_HISTORY_LIMIT)
                        .await
                    {
                        Ok(prepared) => {
                            req.messages = prepared.messages;
                            new_messages = prepared.new_messages;
                        }
                        Err(error) => {
                            tracing::warn!("压缩后上下文重建失败，保留压缩前上下文: {error}");
                        }
                    }
                }
                Ok(false) => {}
                Err(error) => return error_response(&error),
            }
        }

        let before_trim = estimate_message_tokens(&req.messages);
        if scaled(before_trim, conservative_ratio) > max_message_budget {
            req.messages = trim_to_budget(req.messages, scaled_budget);
            let after_trim = estimate_message_tokens(&req.messages);
            if scaled(after_trim, conservative_ratio) > max_message_budget {
                return error_response(&GatewayError::ContextLengthExceeded {
                    required: scaled(before_trim, conservative_ratio).saturating_add(fixed_reserve),
                    available: max_message_budget.saturating_add(fixed_reserve),
                });
            }
            tracing::warn!(
                "会话 {} 在所有候选窗口内无法完整容纳，已从 {} tokens 裁剪至 {} tokens",
                session_id,
                before_trim,
                after_trim
            );
        }
    }

    // 多模态需求：带图片/音频/视频的请求只能交给确实接受该模态、且协议链路能真正
    // 承载该媒体的模型，否则不是「慢一点」，而是内容被上游静默丢弃。
    let required = required_capabilities(&req);
    let media = crate::media::Media::of(&req);

    // D3：长 prompt 型代价需要这个数，而它原本在**下面**才算出来，
    // 所以上提到 rank 之前。重复计算一次没有意义，直接用同一个值。
    let request_message_tokens = estimate_message_tokens(&req.messages);
    let mut ranked = state.router.rank_with_intent(
        candidates,
        &cfg,
        required,
        sticky.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
        preflight.intent.as_ref().map(|intent| intent.class),
        request_message_tokens,
        // D4：领域从请求内容判定。**与 `intent` 同源**（都在这一层），
        // 因为两者都是分类的产物；分开算会让「难度说 reasoning、
        // 领域说 vision」这种不一致有机会出现。
        crate::intellect::detect_domain(&crate::intellect::ClassifyInput {
            messages: &req.messages,
            media,
            // 与 L1913 那处**同款判定**。两处口径不同会让
            // 「分类说有工具、领域判定说没有」这种不一致有机会出现。
            has_tools: req.tools.is_some(),
            requested_model: &req.model,
        }),
    );
    // 方言承载过滤要看到被剔除前的候选，才能区分「模型没勾选能力」与
    // 「该方言承载不了这种媒体」两种情况，给出可操作的错误。
    let media_rejections = filter_by_media_carry(&mut ranked, &media);
    ranked.retain(|candidate| {
        candidate_supports_context(
            candidate,
            request_message_tokens,
            fixed_reserve,
            &calibration,
        )
    });

    if ranked.is_empty() {
        return error_response(&missing_capability_error(
            &required,
            &media,
            &media_rejections,
        ));
    }

    // UI 的“热切换”不应只是写一份状态。把用户指定的 provider 提到候选链首位，
    // 仍保留其余健康候选作为故障转移后备。
    if let Some(active_id) = state.active.read().clone() {
        if let Some(pos) = ranked.iter().position(|c| c.provider.id == active_id) {
            let active = ranked.remove(pos);
            ranked.insert(0, active);
        }
    }

    // 6) 执行（区分流式/非流式）
    let dispatch_input = DispatchInput {
        req,
        new_messages,
        response_session_id,
        session_id: session_id.clone(),
        client,
        ranked,
        cfg,
        exit,
        intent: preflight.intent,
        search: preflight.search,
        refine: preflight.refine,
        cache_session: header_sid.clone().unwrap_or_default(),
        // traceId 在**入口处定一次**：客户端给了合法值就透传（便于跨服务串联），
        // 否则生成新的。清洗在 `resolve_trace_id` 里做 —— 这个值会被写进
        // 响应头，不清洗就是 HTTP 头注入面。
        trace_id: trace_id.clone(),
    };
    if dispatch_input.req.stream {
        stream_dispatch(state.clone(), dispatch_input).await
    } else {
        normal_dispatch(state.clone(), dispatch_input).await
    }
}

const CONTEXT_HISTORY_LIMIT: usize = 1_000;
const DEFAULT_OUTPUT_RESERVE: u32 = 1_024;
const PROTOCOL_CONTEXT_RESERVE: u32 = 256;

/// 请求实际包含的模态需求。检测对象是重建后的完整上下文——历史里带过的图片
/// 同样要求后续模型能看懂，否则上下文与客户端所见会分叉。
fn required_capabilities(req: &ChatRequest) -> crate::router::score::RequiredCapabilities {
    let media = crate::media::Media::of(req);
    crate::router::score::RequiredCapabilities {
        tools: req.tools.is_some(),
        vision: media.image,
        audio: media.audio,
        video: media.video,
    }
}

/// 没有任何候选能满足需求时的错误。优先级：先报「模型缺能力」（用户可自行勾选），
/// 再报「方言承载不了」（需要换上游或改客户端），最后回落到通用失败。
fn missing_capability_error(
    required: &crate::router::score::RequiredCapabilities,
    media: &crate::media::Media,
    rejections: &[String],
) -> GatewayError {
    let missing: Vec<&str> = [
        (required.vision, "图片输入（视觉）"),
        (required.audio, "音频输入"),
        (required.video, "视频输入"),
    ]
    .iter()
    .filter_map(|(needed, label)| needed.then_some(*label))
    .collect();
    if missing.is_empty() {
        return GatewayError::AllProvidersFailed { attempts: 0 };
    }
    let mut kind = missing.join("、");
    if required.tools {
        kind = format!("{kind}（含工具调用）");
    }
    if media.any() && !rejections.is_empty() {
        // 能力都勾了但链路承载不了：把第一条具体原因带上，避免用户反复试错。
        kind = format!("{kind}；{}", rejections[0]);
    }
    GatewayError::CapabilityUnavailable { kind }
}

/// 按方言的实际承载能力剔除候选，收集原因文本用于错误提示。
fn filter_by_media_carry(
    ranked: &mut Vec<crate::router::score::Candidate>,
    media: &crate::media::Media,
) -> Vec<String> {
    if !media.any() {
        return Vec::new();
    }
    let mut reasons: Vec<String> = Vec::new();
    ranked.retain(
        |candidate| match crate::media::carry(candidate.provider.dialect, media) {
            Ok(()) => true,
            Err(reason) => {
                let text = format!(
                    "{}（{}）无法承载：{}",
                    candidate.provider.name, candidate.model.upstream, reason
                );
                if !reasons.contains(&text) {
                    reasons.push(text);
                }
                false
            }
        },
    );
    reasons
}

/// 估算校准快照：provider::model → 「实际/估算」EWMA 比值。表很小（每个配置模型
/// 一行），每请求读取一次比维护带失效逻辑的内存缓存更不容易出错。
type CalibrationMap = std::collections::HashMap<String, f64>;

/// 保守取候选中的最大比值：宁可多裁一点，也不能把真实超窗的 payload 发出去。
fn conservative_ratio(
    candidates: &[crate::router::score::Candidate],
    calibration: &CalibrationMap,
) -> f64 {
    candidates
        .iter()
        .filter_map(|candidate| {
            calibration
                .get(&calibration_key(
                    &candidate.provider.id,
                    &candidate.model.upstream,
                ))
                .copied()
        })
        .fold(1.0_f64, f64::max)
        .clamp(0.5, 3.0)
}

fn calibration_key(provider_id: &str, model: &str) -> String {
    format!("{provider_id}::{model}")
}

/// 把本地估算换算成上游口径的保守值。
fn scaled(estimated: u32, ratio: f64) -> u32 {
    (estimated as f64 * ratio).ceil().min(u32::MAX as f64) as u32
}

fn fixed_context_reserve(req: &ChatRequest, candidates: &[crate::router::score::Candidate]) -> u32 {
    let tool_schema_tokens = req
        .tools
        .as_ref()
        .and_then(|tools| serde_json::to_string(tools).ok())
        .map(|json| (json.chars().count() as u32).div_ceil(3))
        .unwrap_or(0);
    // 模型级 max_tokens 覆盖会改变真实输出上限，预留必须按可能的最大值计算，
    // 否则请求本身放得下、输出却把窗口顶爆。
    let requested = req.max_tokens.unwrap_or(DEFAULT_OUTPUT_RESERVE);
    let override_max = candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .model
                .overrides
                .as_ref()
                .and_then(|overrides| overrides.max_tokens)
        })
        .max()
        .unwrap_or(0);
    requested
        .max(override_max)
        .saturating_add(tool_schema_tokens)
        .saturating_add(PROTOCOL_CONTEXT_RESERVE)
}

fn max_message_budget(
    candidates: &[crate::router::score::Candidate],
    fixed_reserve: u32,
) -> Option<u32> {
    candidates
        .iter()
        .filter_map(|candidate| {
            (candidate.model.context_window > 0).then_some(candidate.model.context_window as u32)
        })
        .map(|window| window.saturating_sub(fixed_reserve))
        .max()
}

fn candidate_supports_context(
    candidate: &crate::router::score::Candidate,
    message_tokens: u32,
    fixed_reserve: u32,
    calibration: &CalibrationMap,
) -> bool {
    if candidate.model.context_window <= 0 {
        return true;
    }
    let ratio = calibration
        .get(&calibration_key(
            &candidate.provider.id,
            &candidate.model.upstream,
        ))
        .copied()
        .unwrap_or(1.0)
        .clamp(0.5, 3.0);
    scaled(message_tokens, ratio).saturating_add(fixed_reserve)
        <= candidate.model.context_window as u32
}

fn schedule_compaction(state: Arc<GatewayState>, session_id: String) {
    tokio::spawn(async move {
        if let Err(error) = state.compact_session_if_needed(&session_id, false).await {
            tracing::warn!("响应后自动压缩失败: {error}");
        }
    });
}

/// 一次级联尝试的产物。
///
/// 【为什么不让 `send` 直接返回 `Err`】失败时那一跳的 `AttemptRecord`
/// 仍然要进审计 —— 「上游拒了」恰恰是最需要看得见的一类记录。
/// 让 `send` 返回 `Err` 的话，`run_cascade` 会把整轮结束掉，
/// 而失败之前已经发生过的那几跳的记录也就一起没了。
type CascadeAttemptPayload = Result<AttemptOutcome<crate::domain::ChatResponse>>;

/// D4：级联发送闭包返回的 future。
///
/// **必须装箱**：闭包每次调用都要 clone 出一份独立的现场（`ranked` / `cfg` /
/// `state`），而 Rust 的闭包不能返回 `impl Trait`。装箱的代价是一次堆分配，
/// 相对一次 LLM 上游请求（秒级）可以忽略。
type CascadeSendFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<CascadeAttemptPayload>> + Send>>;

/// 级联各次尝试共用的逐跳明细收集器。
///
/// 【为什么需要它】每次尝试各有自己的 `Vec<AttemptRecord>`，而
/// `CascadeRun` 只保留**最后**那次的值。不共享一个 sink 的话，
/// 「第一跳发给了便宜的模型、被升级掉了」这件事会在审计里消失 ——
/// 而那正是级联唯一需要向用户解释清楚的东西。
type AttemptRecordSink = Arc<parking_lot::Mutex<Vec<AttemptRecord>>>;

/// D4：级联置信度闭包返回的 future。装箱理由同上。
type CascadeAskFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = ConfidenceReading> + Send>>;

/// D4：向 Jev 问一次「这个候选回答够不够好」。
///
/// 走的是**已有的** Jev 通道（`/v1/systemone`），不为此新起一个模型
/// （卡片硬约束 2）。
///
/// ## `None` 与 `Some(0.0)` 是两件事
///
/// - `None` = **通道不可用**：服务没启动 / 超时 / 返回结构非法。
///   三种情况一视同仁 —— 对「这个置信度可不可信」而言它们没有区别。
///   调用方据此**不升级**（卡片硬约束 3）。
/// - `Some(0.0)` = 通道好用，但它明确说这个回答不行 ——
///   那正是该升级的情形。
///
/// 把两者合并成 `0.0` 会让「Jev 没起来」变成「每次都升级到最贵那档」，
/// 那与实测量到的「edgeJev 会高置信度判错」叠加起来是双倍损失。
async fn cascade_confidence(cfg: &AppConfig, question: &str, answer: &str) -> Option<f32> {
    let jev = crate::intellect::JevClient::new(
        &cfg.smart_routing.jev.base_url,
        &cfg.smart_routing.jev.model,
        cfg.smart_routing.jev.timeout_ms,
        cfg.smart_routing.jev.max_state_chars,
    )
    .ok()?;
    // state 是「问题 + 候选回答」。截断由 `truncate_state` 负责（保留尾部），
    // 与分类那条通道同一个函数 —— 两处各写一套截断迟早不一致。
    let state = format!("【用户的问题】\n{question}\n\n【候选回答】\n{answer}");
    let body = serde_json::json!({
        "model": jev.model_name(),
        "state": { "prompt": jev.truncate_state(&state) },
        "questions": {
            "adequate": {
                "type": "choice",
                "instructions": "这个候选回答是否已经足以交付给用户，不需要换成更强的模型重答",
                "criteria": {
                    "adequate": "已经答到点上，内容完整，没有明显的事实或推理错误",
                    "inadequate": "答偏了、明显不完整，或存在事实/推理错误"
                }
            }
        }
    });
    let result = jev.decide(body).await.ok()?;
    let answer = result.get("adequate")?;
    Some(match answer.choice() {
        // 选「够好」时取模型自报的确定程度：它自己对所选项都不确定时，
        // 不该被当成「可以收手」。
        Some("adequate") => answer.confidence(),
        // 选了别的选项（含 inadequate）⇒ 明确「不够好」。
        // 这里**不能**用 `answer.confidence()` —— 那会让「很确定地说不行」
        // 拿到高置信度，语义正好反过来。
        Some(_) => 0.0,
        // 不是 choice（`noul` / `score`）时它的值本来就落在 0..=1。
        None => answer.confidence(),
    })
}

/// 级联要拿「用户这次问的是什么」去问 Jev，取最后一条 user 消息。
/// 与 `intellect::ClassifyInput::last_user_text` **同口径**。
fn last_user_text(req: &ChatRequest) -> String {
    req.messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content_text())
        .unwrap_or_default()
}

/// 非流式
async fn normal_dispatch(state: Arc<GatewayState>, input: DispatchInput) -> Response {
    let route = input.route_trace();
    // `tokio::spawn(async move { … })` 是**按值**捕获用到的变量，
    // 所以下面的后台任务要用的是一份克隆，原变量留给响应头。
    let audit_route = route.clone();
    let DispatchInput {
        cache_session,
        trace_id,
        req,
        new_messages,
        response_session_id,
        session_id,
        client,
        mut ranked,
        cfg,
        exit,
        intent: _intent,
        search: _search,
        // B3：改写后的提示词要不要落库，由这个值决定。
        // 原来是 `_refine`（明确丢弃），现在真的要读它。
        refine,
    } = input;
    let started = Instant::now();
    let max_attempts = if cfg.failover_enabled {
        cfg.max_fallback_attempts
    } else {
        1
    };

    // ===== D4：级联档的执行层 =====
    //
    // 这是 `RoutingStrategy::Cascade` 唯一的生产消费点。判定用
    // [`Router::effective_strategy`] 而不是在本地重写一遍降级口径 ——
    // 两处各判一次的话，表现会是「界面上设了 cascade、实际却没级联」，
    // 而那种错误不报错，只是行为不对。
    //
    // **只在非流式路径上**（这里就是）：流式的换家在 `stream_dispatch` 里
    // 由「首个字节之后不换家」那条铁律挡着，级联压根不参与。
    let (strategy, _) = state.router.effective_strategy(&ranked, &cfg);
    let cascade_on = matches!(strategy, RoutingStrategy::Cascade) && cfg.cascade.enabled();
    // 级联档的**执行顺序**是「便宜优先」，与 `Weights` 排序刻意分开
    // （级联档的权重与 Balanced 逐位相同，见 tests/cascade.rs）。
    //
    // 【模式隔离】重排只发生在真的走级联时 —— 其余七档的候选顺序
    // 必须逐位不变。判断因此放在重排**之前**。
    //
    // `order_by_cost` 返回 false = 币种不可比或一个价格都没有，
    // 此时保持原顺序：那不算错误，只是「便宜优先」这个前提本轮不成立。
    let cascade_ordered = if cascade_on {
        crate::router::order_by_cost(
            &mut ranked,
            estimate_message_tokens(&req.messages) as i64,
            utc_minute_of_day(),
        )
    } else {
        false
    };

    // 缓存键的 provider/model 用**本轮路由的首选**（ranked[0]）。
    //
    // 为什么不用实际命中的那家：查表必须发生在跑上游**之前**，那时还不知道
    // 会不会失败转移。用首选做键是确定性的（同样的请求 + 同样的配置 ⇒ 同样的首选），
    // 代价是「发生了失败转移」的那次响应不会被回填 —— 见下面回填处的路由一致性检查。
    // 宁可少缓存，也不要让键与内容不符。
    let intended_route = ranked
        .first()
        .map(|c| (c.provider.id.clone(), c.model.upstream.clone()));
    let cache_cfg = cfg.cache.clone();
    let cache_miss_key = match (&intended_route, cache_cfg.enabled) {
        (Some((pid, mid)), true) => {
            let request_value = serde_json::to_value(&req).unwrap_or(serde_json::Value::Null);
            let key_input = crate::cache::CacheKeyInput {
                provider_id: pid,
                routed_model: mid,
                request: &request_value,
                // 用客户端提供的会话 id，不是网关自动生成的那个 ——
                // 后者每次请求都不同，会让每一条缓存都独一无二。
                session_id: &cache_session,
                stream: false,
                // 多模态看重建后的完整上下文，与 required_capabilities 同源
                multimodal: crate::media::Media::of(&req).any(),
                // 触发过搜索预取就不缓存：把当时的检索结果固化成「模型的记忆」
                // 是错误的信息来源，今天查到的和昨天不一样。
                search_injected: _search.is_some(),
                upstream_status: None,
                response_has_tool_calls: false,
            };
            match state.cache.lookup(&cache_cfg, &key_input) {
                crate::cache::Lookup::Hit(value) => {
                    // 存的是协议中立的 `ChatResponse`，命中时按本次 exit 重新编码 ——
                    // 这样 OpenAI / Anthropic / Responses 三个出口能共用一份缓存。
                    // 若直接存编码后的 body，同一个逻辑请求来自不同客户端就会串格式。
                    let cached: crate::domain::ChatResponse =
                        serde_json::from_value(value).unwrap_or_default();
                    let body = encode_response(&cached, mid, exit);
                    let mut resp = Json(body).into_response();
                    apply_route_headers(&route, resp.headers_mut());
                    let h = resp.headers_mut();
                    h.insert(
                        "x-cache",
                        parse_header(crate::cache::CacheOutcome::Hit.code()),
                    );
                    h.insert(
                        "x-cache-key",
                        parse_header(&crate::cache::key_prefix(&crate::cache::cache_key(
                            &key_input,
                        ))),
                    );
                    h.insert("x-session-id", parse_header(&response_session_id));
                    // B4：traceId **永远**回给客户端，即使 OTLP 导出关着。
                    // 否则「导不出」会退化成「查不到」——排查时手里一个可追的标识都没有。
                    h.insert("x-trace-id", parse_header(&trace_id));
                    return resp;
                }
                crate::cache::Lookup::Miss(key) => Some(key),
                crate::cache::Lookup::Bypass(_) => None,
            }
        }
        _ => None,
    };
    // 绕过原因：缓存开着、但这次请求落在明确不缓存的场景里。
    // 关闭时是 None —— 一个头都不加（铁律 2）。
    let cache_bypass = if cache_cfg.enabled && cache_miss_key.is_none() {
        match &intended_route {
            Some((pid, mid)) => {
                let request_value = serde_json::to_value(&req).unwrap_or(serde_json::Value::Null);
                let probe = crate::cache::CacheKeyInput {
                    provider_id: pid,
                    routed_model: mid,
                    request: &request_value,
                    session_id: &cache_session,
                    stream: false,
                    multimodal: crate::media::Media::of(&req).any(),
                    search_injected: _search.is_some(),
                    upstream_status: None,
                    response_has_tool_calls: false,
                };
                probe.bypass_reason_code().map(|r| r.code())
            }
            None => None,
        }
    } else {
        None
    };

    let upstream = state.upstream.clone();
    let req_arc = Arc::new(req.clone());

    let health = state.health.clone();
    let router = state.router.clone();
    // 级联各跳的逐跳明细共用这一个收集器：不共享的话，
    // 「第一跳发给了便宜的模型、被升级掉了」这件事在审计里会消失。
    let records_sink: AttemptRecordSink = Arc::new(parking_lot::Mutex::new(Vec::new()));

    // ===== D4：全仓唯一的发送路径 =====
    //
    // 级联**关着**时 `run_cascade` 立刻短路成「发一次」（连置信度都不读），
    // 所以非级联路径也走这里：发送与「要不要升级」只有一份实现，
    // 不会出现「生产路径与用例跑的不是同一段代码」那种假验证。
    let cascade_run = {
        let question = last_user_text(&req);
        crate::router::cascade::run_cascade(
            &cfg.cascade,
            CascadeRunRequest {
                candidates: ranked.len(),
                streaming: false,
            },
            |start: usize| -> CascadeSendFuture {
                // 每次尝试 clone 出一份独立现场：闭包不能返回借用外层的 future
                // （`Pin<Box<dyn Future + Send>>` 默认是 `'static`）。
                let state = Arc::clone(&state);
                let ranked = ranked.clone();
                let cfg = cfg.clone();
                let upstream = upstream.clone();
                let req_arc = Arc::clone(&req_arc);
                let health = Arc::clone(&health);
                let router = Arc::clone(&router);
                let sink = Arc::clone(&records_sink);
                Box::pin(async move {
                    // `AtomicFlag` 只被流式路径 `set`，而这里是**非流式**入口；
                    // 每次尝试各起一个，等价于原来共享一个（它永远是 0）。
                    let flag = AtomicFlag::new();
                    let chain = FailoverChain::new(&ranked[start..], max_attempts, &flag);
                    let chain = chain
                        .with_auth_policy(cfg.auth_failure.mode, cfg.auth_failure.confirm_retries);
                    let timeout = Duration::from_secs(cfg.upstream_timeout_secs);
                    let mut records = Vec::new();
                    let outcome = chain
                        .run_with_auth_policy(
                            &mut records,
                            |provider, model| {
                                let up = upstream.clone();
                                let r = Arc::clone(&req_arc);
                                let defaults = state.cfg_snapshot().ollama_options;
                                let adapters = state.adapters.clone();
                                async move {
                                    // ===== 任务卡二 A5：**唯一分派点** =====
                                    //
                                    // 这是全仓唯一一处「按 Provider 的上游形态选路径」的地方。
                                    //
                                    // 【模式隔离】`runtime_id` 为 `None`（默认，也是绝大多数
                                    // Provider 的现状）时，这个 `if let` **落空后直接落到
                                    // 原有那一行**，中间不经过任何新代码 —— 所以「老 Provider
                                    // 的响应体逐字节不变」是**结构性保证**，
                                    // 而不是「小心写出来的」。
                                    //
                                    // 放在**链的闭包里**而不是函数顶部：`ranked` 是一批候选，
                                    // 「要发给谁」是链逐候选决定的。放在顶部就只能对
                                    // `ranked[0]` 生效，那会**丢掉失败转移** ——
                                    // 而那种缺陷只在首选真失败时才暴露。
                                    // 放在这里还意味着账号型与 API 型 Provider 混在一批候选里
                                    // 也能各自走对路。
                                    if let Some(runtime_id) = provider.runtime_id.as_deref() {
                                        return crate::agent_upstream::call_agent(
                                            &adapters,
                                            runtime_id,
                                            &model,
                                            &r.messages,
                                            timeout.as_millis() as u64,
                                        )
                                        .await;
                                    }
                                    up.call(&provider, &r, &model, timeout, &defaults).await
                                }
                            },
                            |provider, model, err| {
                                // 429 额外打满本地额度窗口，避免连续撞墙
                                if let GatewayError::Upstream { status: 429, .. } = err {
                                    if let Some(m) =
                                        provider.models.iter().find(|m| m.upstream == model)
                                    {
                                        router.mark_rate_limited(provider, m);
                                    }
                                }
                                health.record_failure(&provider.id, model, err);
                            },
                            auth_confirm_handler(state.clone(), cfg.clone()),
                        )
                        .await;
                    // 链要 `&mut Vec`，所以明细先落在本地，链跑完再并进共享 sink
                    // ——共享锁不能跨 await 持有。
                    sink.lock().append(&mut records);
                    Ok::<_, GatewayError>(outcome)
                })
            },
            |payload: &CascadeAttemptPayload| -> CascadeAskFuture {
                // 失败的那一跳不问 Jev：没有回答可评估。
                // 返回「通道不可用」而不是 `Some(0.0)` —— 后者会触发升级，
                // 而这里根本没有可升级的东西（错误由外层如实报出）。
                let Ok(outcome) = payload.as_ref() else {
                    return Box::pin(async { ConfidenceReading::unavailable() });
                };
                let cfg = cfg.clone();
                let question = question.clone();
                let content = outcome.value.content.clone();
                Box::pin(async move {
                    match cascade_confidence(&cfg, &question, &content).await {
                        Some(value) => ConfidenceReading::available(value),
                        None => ConfidenceReading::unavailable(),
                    }
                })
            },
        )
        .await
    };

    // 各次尝试的逐跳明细都在这个 sink 里（级联升级过的前几跳也在）。
    let mut attempt_records: Vec<AttemptRecord> = std::mem::take(&mut *records_sink.lock());
    let mut cascade_stop: Option<CascadeStop> = None;
    let mut cascade_attempts = 0usize;
    let outcome: Result<AttemptOutcome<crate::domain::ChatResponse>> = match cascade_run {
        Ok(run) => {
            cascade_stop = Some(run.stop);
            cascade_attempts = run.attempts;
            run.value
        }
        // `run_cascade` 只在 `send` 自己返回 `Err` 时失败，而这里的 `send`
        // 把结果包在 payload 里（失败也要保住那一跳的审计记录），
        // 所以这条是防御分支 —— 真走到的话按「没有可用候选」处理。
        Err(error) => Err(error),
    };

    // 「为什么这次没升级」是级联唯一会被问到的问题，而它没有别的落点：
    // `requests` 表那几列已冻结（加列那笔账连着失败两次），
    // 逐跳明细则由 `attempts_json` 如实承载。所以停因先进日志。
    if let Some(stop) = cascade_stop {
        tracing::info!(
            attempts = cascade_attempts,
            escalations = cascade_attempts.saturating_sub(1),
            stop = crate::router::cascade::stop_label(stop),
            cheap_first = cascade_ordered,
            "级联路由执行完毕"
        );
    }

    match outcome {
        Ok(o) => {
            let latency = started.elapsed().as_millis() as u64;
            // 任务卡二 A6 判据 1：把适配器如实回报的**传输**回填进逐跳明细。
            //
            // 【为什么在这里回填，而不是在 `AttemptRecord::success` 里传】
            // 失败转移链对响应类型是**泛型**的（`run_with_auth_policy<T>`），
            // 链内部读不到 `ChatResponse.transport`；硬加 trait 约束会把
            // 泛型复杂度传染给每个调用方。而这里 `o.value` 是有具体类型的。
            //
            // 回填**最后一条**是准确的：`o` 就是链最后成功的那一跳。
            // 普通 API 上游的 `transport` 是 `None`，回填等于没改。
            if let Some(last) = attempt_records.last_mut() {
                last.transport = o.value.transport.clone();
            }
            health.record_success(&o.provider_id, &o.model, latency as u32);
            if let Some(p) = ranked.iter().find(|c| c.provider.id == o.provider_id) {
                if let Some(m) = p.provider.models.iter().find(|m| m.upstream == o.model) {
                    router.consume(
                        &p.provider,
                        m,
                        o.value.usage.as_ref().map(|u| u.total_tokens).unwrap_or(0),
                    );
                }
            }

            // 上下文和粘性是下一次同会话请求的前置条件，必须在 HTTP 成功响应
            // 之前完成；仅审计日志允许异步，避免紧随其后的请求读到旧历史。
            let (pt, ct, cache_read, cache_creation) = o
                .value
                .usage
                .as_ref()
                .map(|u| {
                    (
                        u.prompt_tokens,
                        u.completion_tokens,
                        u.cache_read_tokens,
                        u.cache_creation_tokens,
                    )
                })
                .unwrap_or((0, 0, 0, 0));
            // D3：流式路径的实测吞吐。**与上一个记账点（非流式）同一口径** ——
            // 流式的 usage 往往在最后一个 chunk 才出现，所以必须等到
            // `o.value.usage` 拿到之后再记，不能挪到 `record_success` 旁边。
            // `record_tps` 自己会拒绝 0 token（多数流式中间帧没有 usage）。
            health.record_tps(&o.provider_id, &o.model, ct, latency as u32);
            let pid = o.provider_id.clone();
            let mid = o.model.clone();
            let assistant = Message {
                role: Role::Assistant,
                content: Content::Text(o.value.content.clone()),
                tool_calls: o.value.tool_calls.clone(),
                tool_call_id: None,
                name: None,
            };
            if let Err(error) = state
                .ctx
                .append_exchange_with(
                    &session_id,
                    ExchangeWrite {
                        incoming: &new_messages,
                        assistant: &assistant,
                        routed_provider: Some(&pid),
                        routed_model: Some(&mid),
                        prompt_tokens: pt as i64,
                        completion_tokens: ct as i64,
                    },
                )
                .await
            {
                tracing::error!("响应成功但上下文持久化失败: {error}");
                return error_response(&error);
            }
            if let Err(error) = repo::update_sticky(
                state.db.pool(),
                &session_id,
                &pid,
                &mid,
                now_secs() + state.cfg_snapshot().sticky_ttl_secs,
            )
            .await
            {
                tracing::warn!("更新会话粘性路由失败: {error}");
            }

            // exchange 已持久化后再触发后台压缩。若下一请求先到达，它会通过请求
            // 前的同一把锁同步完成压缩，不会读到尚未压缩的陈旧上下文。
            //
            // 校准写入必须在触发压缩之前完成：压缩阈值要按同一份「实际/估算」口径
            // 判断，否则刚观测到的偏差要等到下一轮才生效。
            let estimated_prompt = estimate_message_tokens(&req.messages) as i64;
            if let Ok(calibration) = repo::record_token_calibration(
                state.db.pool(),
                &pid,
                &mid,
                estimated_prompt,
                pt as i64,
            )
            .await
            {
                if let Err(error) = repo::update_session_token_ratio(
                    state.db.pool(),
                    &session_id,
                    calibration.ratio,
                )
                .await
                {
                    tracing::warn!("更新会话校准比值失败: {error}");
                }
            }
            schedule_compaction(state.clone(), session_id.clone());

            let audit_state = state.clone();
            let audit_session_id = session_id.clone();
            let audit_requested_model = req.model.clone();
            let audit_client = client.clone();
            let audit_pid = pid.clone();
            let audit_mid = mid.clone();
            let audit_fallback_attempts = fallback_count(o.attempts);
            let audit_price = ranked
                .iter()
                .find(|c| c.provider.id == pid && c.model.upstream == mid)
                .and_then(|c| c.model.price.as_ref());
            let (audit_cost, audit_currency, audit_rate_label) = price_charge(
                audit_price,
                pt as i64,
                ct as i64,
                cache_read as i64,
                cache_creation as i64,
            );
            let estimated_prompt = estimate_message_tokens(&req.messages) as i64;
            // traceId 要被三个地方用：响应头（借用）、审计落库（移进 spawn）、
            // 以及每一条降级明细。先克隆一份，后面都用它。
            let audit_trace_id = trace_id.clone();
            let audit_attempts = attempts_json_traced(&attempt_records, Some(&audit_trace_id));
            // 必须在 `tokio::spawn` **之前**算好：spawn 的闭包按值捕获，
            // 而 `cfg` 与 `refine` 在里面拿不到。走 `refined_prompt_to_store`
            // 而不是手写 `if`，是为了让「开关」与「脱敏」永远同进同退。
            //
            // 【B3 补正】这段接线在 B3 那一笔里**从未生效** ——
            // 当时的 `String.Replace` 锚点没匹配上、静默返回原文，
            // 而 B3 的测试全过，因为它们只测 `refined_prompt_to_store` 这个
            // 纯函数，没有一条走 dispatch。现在补上，并加了端到端用例
            // （tests/trace.rs 的 `开启提示词留存后_改写结果真的落库`）。
            let audit_refined_prompt = crate::audit::refined_prompt_to_store(
                &cfg.audit,
                refine.as_ref().map(|r| r.prompt.as_str()),
            );
            // traceId 要被两个地方用：响应头（借用）与审计落库（移进 spawn）。
            // 先克隆一份给 spawn，原值留给响应头。
            let audit_trace_id = trace_id.clone();
            tokio::spawn(async move {
                let _ = repo::log_request(
                    audit_state.db.pool(),
                    repo::RequestLog {
                        session_id: Some(&audit_session_id),
                        client: audit_client.as_deref(),
                        // 这次消费归属的远程 Key。本机统一 Key 的请求是 None。
                        // 与 client 同源派生，不另写一套前缀解析。
                        access_key_id: crate::budget::access_key_id_of(audit_client.as_deref()),
                        // 已脱敏（开关关着时是 None）。
                        refined_prompt: audit_refined_prompt.as_deref(),
                        trace_id: &audit_trace_id,
                        requested_model: &audit_requested_model,
                        routed_provider: Some(&audit_pid),
                        routed_model: Some(&audit_mid),
                        status: Some(200),
                        latency_ms: latency as i64,
                        prompt_tokens: pt as i64,
                        completion_tokens: ct as i64,
                        fallback_attempts: audit_fallback_attempts,
                        error: None,
                        cost: audit_cost,
                        currency: audit_currency,
                        rate_label: audit_rate_label.as_deref(),
                        estimated_prompt_tokens: Some(estimated_prompt),
                        attempts_json: audit_attempts.as_deref(),
                        route: audit_route,
                    },
                )
                .await;
            });

            let body = encode_response(&o.value, &o.model, exit);
            let mut resp = Json(body).into_response();
            apply_route_headers(&route, resp.headers_mut());
            let h = resp.headers_mut();
            h.insert(
                "x-routed-via",
                parse_header(&format!("{}/{}", o.provider_id, o.model)),
            );
            h.insert("x-fallback-attempts", parse_header(&o.attempts.to_string()));
            h.insert("x-session-id", parse_header(&response_session_id));
            // B4：traceId **永远**回给客户端，即使 OTLP 导出关着。
            // 否则「导不出」会退化成「查不到」——排查时手里一个可追的标识都没有。
            h.insert("x-trace-id", parse_header(&trace_id));

            // 回填。三条前置：拿到过键、路由与首选一致、响应不含 tool_calls。
            //
            // 「路由与首选一致」这条是关键：键是按 ranked[0] 算的，
            // 若这次实际是失败转移到第二家拿到的答案，用它回填会让
            // 「provider_id=首选」这个键指向第二家的输出 —— 键与内容不符。
            // 宁可少缓存一次。
            if let Some(key) = cache_miss_key.as_deref() {
                let routed_as_intended = intended_route
                    .as_ref()
                    .is_some_and(|(pid, mid)| *pid == o.provider_id && *mid == o.model);
                let has_tool_calls = o.value.tool_calls.as_ref().is_some_and(|t| !t.is_empty());
                if routed_as_intended && !has_tool_calls {
                    if let Ok(serialized) = serde_json::to_value(&o.value) {
                        state.cache.store(key, serialized);
                    }
                    h.insert(
                        "x-cache",
                        parse_header(crate::cache::CacheOutcome::Miss.code()),
                    );
                    h.insert("x-cache-key", parse_header(&crate::cache::key_prefix(key)));
                } else if has_tool_calls {
                    // 响应带工具调用：从「本来要缓存」降级为绕过，并说明原因
                    h.insert(
                        "x-cache",
                        parse_header(crate::cache::CacheOutcome::Bypass.code()),
                    );
                    h.insert(
                        "x-cache-reason",
                        parse_header(crate::cache::BypassReason::ResponseHasToolCalls.code()),
                    );
                }
            } else if let Some(reason) = cache_bypass {
                // 缓存开着但这次场景明确不缓存。关闭时 cache_bypass 是 None，
                // 一个头都不加。
                h.insert(
                    "x-cache",
                    parse_header(crate::cache::CacheOutcome::Bypass.code()),
                );
                h.insert("x-cache-reason", parse_header(reason));
            }
            resp
        }
        Err(e) => {
            let st = state.clone();
            let sid = session_id.clone();
            let kind = classify(&e);
            let status = e.http_status().as_u16() as i64;
            let requested_model = req.model.clone();
            let audit_client = client.clone();
            // 同 963 行：尝试次数取真实记录数，不从错误里反推。
            let audit_fallback_attempts = fallback_count(attempt_records.len());
            // 失败路径也要带上 traceId：本地永远要有，不能因为路径不同就留空。
            // 克隆一份给 spawn，原值留给响应头。
            let audit_trace_id = trace_id.clone();
            let audit_attempts = attempts_json_traced(&attempt_records, Some(&audit_trace_id));
            tokio::spawn(async move {
                let _ = repo::log_request(
                    st.db.pool(),
                    repo::RequestLog {
                        session_id: Some(&sid),
                        client: audit_client.as_deref(),
                        // 这次消费归属的远程 Key。本机统一 Key 的请求是 None。
                        // 与 client 同源派生，不另写一套前缀解析。
                        access_key_id: crate::budget::access_key_id_of(audit_client.as_deref()),
                        // 改写后的提示词默认不落库；只有开了 audit.store_refined_prompt
                        // 才会由 refined_prompt_to_store 返回脱敏后的文本。失败路径一律 None。
                        refined_prompt: None,
                        trace_id: &audit_trace_id,
                        requested_model: &requested_model,
                        routed_provider: None,
                        routed_model: None,
                        status: Some(status),
                        latency_ms: started.elapsed().as_millis() as i64,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        fallback_attempts: audit_fallback_attempts,
                        error: Some(kind),
                        cost: None,
                        currency: None,
                        rate_label: None,
                        estimated_prompt_tokens: None,
                        attempts_json: audit_attempts.as_deref(),
                        route: Default::default(),
                    },
                )
                .await;
            });
            error_response(&e)
        }
    }
}

/// 流式。核心难点是「降级时机」：
/// 一旦已经向客户端吐出第一个 delta，就不再换家 —— 否则用户会看到两截拼起来的回答。
/// 因此只有在拿到首个 delta 之前的失败才允许切换。
async fn stream_dispatch(state: Arc<GatewayState>, input: DispatchInput) -> Response {
    use async_stream::stream;

    let route = input.route_trace();
    let DispatchInput {
        // 流式一律 BYPASS，用不到缓存键里的会话 id。
        cache_session: _cache_session,
        trace_id,
        req,
        new_messages,
        response_session_id,
        session_id,
        client,
        ranked,
        cfg,
        exit,
        intent: _intent,
        search: _search,
        refine: _refine,
    } = input;

    // `async_stream::stream!` 生成的是协程，它**会拿走**在里面用到的变量。
    // `trace_id` 既要在协程里落库、又要在协程外的响应头上用，
    // 所以先克隆一份给协程，原值留给响应头。
    let stream_trace_id = trace_id.clone();

    let req_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let started = Instant::now();
    let max_attempts = if cfg.failover_enabled {
        cfg.max_fallback_attempts
    } else {
        1
    };

    // 响应头一经发送就无法改写。先完成连接与首个有效事件的选择，才能让
    // X-Routed-Via / X-Fallback-Attempts 反映实际结果，同时仍能在未输出前降级。
    let mut attempts = 0usize;
    let mut last_error: Option<GatewayError> = None;
    let mut selected = None;
    let mut attempt_records: Vec<crate::router::failover::AttemptRecord> = Vec::new();

    'select: for candidate in ranked.iter().take(max_attempts) {
        attempts += 1;
        let attempt_started = Instant::now();
        let timeout = Duration::from_secs(cfg.upstream_timeout_secs);
        let mut upstream_stream = match state
            .upstream
            .call_stream(
                &candidate.provider,
                &req,
                &candidate.model.upstream,
                timeout,
                &cfg.ollama_options,
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                record_candidate_failure(&state, candidate, &error);
                attempt_records.push(crate::router::failover::AttemptRecord::failure(
                    &candidate.provider,
                    &candidate.model.upstream,
                    &error,
                    attempt_started.elapsed().as_millis() as u64,
                ));
                let retryable = error.retryable();
                last_error = Some(error);
                if !retryable {
                    break;
                }
                continue;
            }
        };

        let mut initial_usage = None;
        let mut initial_finish_reason = None;
        loop {
            match upstream_stream.next().await {
                Some(Ok(UpstreamEvent::Ping)) => continue,
                Some(Ok(UpstreamEvent::Usage(value))) => {
                    merge_usage(&mut initial_usage, value);
                    continue;
                }
                Some(Ok(UpstreamEvent::Finish { finish_reason })) => {
                    if finish_reason.is_some() {
                        initial_finish_reason = finish_reason;
                    }
                    continue;
                }
                Some(Ok(event)) => {
                    attempt_records.push(crate::router::failover::AttemptRecord::success(
                        &candidate.provider,
                        &candidate.model.upstream,
                        attempt_started.elapsed().as_millis() as u64,
                    ));
                    selected = Some((
                        candidate.clone(),
                        upstream_stream,
                        event,
                        initial_usage,
                        initial_finish_reason,
                    ));
                    break 'select;
                }
                Some(Err(error)) => {
                    record_candidate_failure(&state, candidate, &error);
                    attempt_records.push(crate::router::failover::AttemptRecord::failure(
                        &candidate.provider,
                        &candidate.model.upstream,
                        &error,
                        attempt_started.elapsed().as_millis() as u64,
                    ));
                    let retryable = error.retryable();
                    last_error = Some(error);
                    if !retryable {
                        break 'select;
                    }
                    break;
                }
                None => {
                    let error = GatewayError::Protocol(format!(
                        "上游 {}/{} 在输出首个事件前结束",
                        candidate.provider.name, candidate.model.upstream
                    ));
                    record_candidate_failure(&state, candidate, &error);
                    attempt_records.push(crate::router::failover::AttemptRecord::failure(
                        &candidate.provider,
                        &candidate.model.upstream,
                        &error,
                        attempt_started.elapsed().as_millis() as u64,
                    ));
                    last_error = Some(error);
                    break;
                }
            }
        }
    }

    let (candidate, mut upstream_stream, first_event, initial_usage, initial_finish_reason) =
        match selected {
            Some(selected) => selected,
            None => {
                let error = last_error.unwrap_or(GatewayError::AllProvidersFailed { attempts });
                spawn_failed_stream_audit(
                    state,
                    FailedStreamAudit {
                        session_id: &session_id,
                        requested_model: &req.model,
                        client: client.as_deref(),
                        attempts,
                        started,
                        error: &error,
                        records: &attempt_records,
                    },
                );
                return error_response(&error);
            }
        };

    let provider_id = candidate.provider.id.clone();
    let model = candidate.model.upstream.clone();
    let routed_via = format!("{provider_id}/{model}");
    let requested_model = req.model.clone();
    let stream_state = state.clone();
    let stream_session_id = session_id.clone();
    let stream_attempts = attempts;
    let stream_client = client.clone();
    // 同上：`stream!` 展开成 `async move`，会按值捕获。审计里那份另存，
    // 响应头那一份留在外层函数里。
    let stream_route = route.clone();

    let s = stream! {
        let mut full = String::new();
        let mut streamed_tool_call_deltas = Vec::new();
        let mut usage = initial_usage;
        let mut final_finish_reason = initial_finish_reason;
        let mut openai_role_sent = false;
        let mut anthropic_stream = None;
        let mut responses_stream = None;
        match exit {
            Exit::Anthropic => {
                let (stream, start) = AnthropicStreamState::new(&req_id, &model, usage.as_ref());
                anthropic_stream = Some(stream);
                yield Ok::<String, std::convert::Infallible>(start);
            }
            Exit::Responses => {
                let (stream, created) = ResponsesStreamState::new(&req_id, &model);
                responses_stream = Some(stream);
                yield Ok::<String, std::convert::Infallible>(created);
            }
            Exit::OpenAI | Exit::Ollama => {}
            // C2：Gemini 的 `alt=sse` **没有**独立的 start 事件 ——
            // 第一帧就是第一个增量。造一个 start 会让客户端多收到一个空 parts。
            Exit::Gemini => {}
        }
        let mut next_event = Some(first_event);

        let upstream_completed = loop {
            let event = match next_event.take() {
                Some(event) => Some(Ok(event)),
                None => upstream_stream.next().await,
            };

            match event {
                Some(Ok(UpstreamEvent::Delta(text))) => {
                    full.push_str(&text);
                    match exit {
                        Exit::Anthropic => {
                            for encoded in anthropic_stream
                                .as_mut()
                                .expect("Anthropic stream state was initialized")
                                .text_delta(&text)
                            {
                                yield Ok::<String, std::convert::Infallible>(encoded);
                            }
                        }
                        Exit::Responses => {
                            for encoded in responses_stream
                                .as_mut()
                                .expect("Responses stream state was initialized")
                                .text_delta(&text)
                            {
                                yield Ok::<String, std::convert::Infallible>(encoded);
                            }
                        }
                        // C2：Gemini 也走 `encode_delta` —— 编码只放一处。
                        // 两套编码逻辑必然漂移，而漂移的表现是「某个协议的流式
                        // 少了几帧」，不会有任何报错。
                        Exit::OpenAI | Exit::Ollama | Exit::Gemini => {
                            let include_openai_role = exit == Exit::OpenAI && !openai_role_sent;
                            if include_openai_role {
                                openai_role_sent = true;
                            }
                            yield Ok::<String, std::convert::Infallible>(encode_delta(&req_id, &model, &text, include_openai_role, exit));
                        }
                    }
                }
                Some(Ok(UpstreamEvent::ToolCalls(tool_calls))) => {
                    merge_tool_call_deltas(&mut streamed_tool_call_deltas, &tool_calls);
                    match exit {
                        Exit::Anthropic => {
                            for encoded in anthropic_stream
                                .as_mut()
                                .expect("Anthropic stream state was initialized")
                                .tool_deltas(&tool_calls)
                            {
                                yield Ok::<String, std::convert::Infallible>(encoded);
                            }
                        }
                        Exit::Responses => {
                            for encoded in responses_stream
                                .as_mut()
                                .expect("Responses stream state was initialized")
                                .tool_deltas(&tool_calls)
                            {
                                yield Ok::<String, std::convert::Infallible>(encoded);
                            }
                        }
                        Exit::OpenAI | Exit::Ollama | Exit::Gemini => {
                            let include_openai_role = exit == Exit::OpenAI && !openai_role_sent;
                            if include_openai_role {
                                openai_role_sent = true;
                            }
                            yield Ok::<String, std::convert::Infallible>(encode_tool_delta(&req_id, &model, &tool_calls, include_openai_role, exit));
                        }
                    }
                }
                Some(Ok(UpstreamEvent::Finish { finish_reason: upstream_finish_reason })) => {
                    if upstream_finish_reason.is_some() {
                        final_finish_reason = upstream_finish_reason;
                    }
                }
                Some(Ok(UpstreamEvent::Usage(value))) => {
                    merge_usage(&mut usage, value);
                }
                Some(Ok(UpstreamEvent::Ping)) => {
                    if matches!(exit, Exit::OpenAI | Exit::Anthropic) {
                        yield Ok::<String, std::convert::Infallible>(sse_event(Some("ping"), "{}"));
                    }
                }
                Some(Ok(UpstreamEvent::Done { finish_reason: terminal_finish_reason, usage: final_usage })) => {
                    if let Some(value) = final_usage {
                        merge_usage(&mut usage, value);
                    }
                    if final_finish_reason.is_none() {
                        final_finish_reason = terminal_finish_reason;
                    }
                    break true;
                }
                Some(Err(error)) => {
                    record_candidate_failure(&stream_state, &candidate, &error);
                    persist_incomplete_stream_exchange(
                        &stream_state,
                        IncompleteStreamExchange {
                            session_id: &stream_session_id,
                            incoming: &new_messages,
                            provider_id: &provider_id,
                            model: &model,
                            content: &full,
                            tool_call_deltas: &streamed_tool_call_deltas,
                            usage: usage.as_ref(),
                        },
                    )
                    .await;
                    schedule_compaction(stream_state.clone(), stream_session_id.clone());
                    spawn_failed_stream_audit(
                        stream_state.clone(),
                        FailedStreamAudit {
                            session_id: &stream_session_id,
                            requested_model: &requested_model,
                            client: stream_client.as_deref(),
                            attempts: stream_attempts,
                            started,
                            error: &error,
                            records: &attempt_records,
                        },
                    );
                    yield Ok::<String, std::convert::Infallible>(encode_error_chunk(&req_id, &error, exit));
                    return;
                }
                None => {
                    let error = GatewayError::Upstream {
                        provider: provider_id.clone(),
                        model: model.clone(),
                        status: StatusCode::BAD_GATEWAY.as_u16(),
                        body: "流式响应在完成事件前意外结束".into(),
                    };
                    record_candidate_failure(&stream_state, &candidate, &error);
                    persist_incomplete_stream_exchange(
                        &stream_state,
                        IncompleteStreamExchange {
                            session_id: &stream_session_id,
                            incoming: &new_messages,
                            provider_id: &provider_id,
                            model: &model,
                            content: &full,
                            tool_call_deltas: &streamed_tool_call_deltas,
                            usage: usage.as_ref(),
                        },
                    )
                    .await;
                    schedule_compaction(stream_state.clone(), stream_session_id.clone());
                    spawn_failed_stream_audit(
                        stream_state.clone(),
                        FailedStreamAudit {
                            session_id: &stream_session_id,
                            requested_model: &requested_model,
                            client: stream_client.as_deref(),
                            attempts: stream_attempts,
                            started,
                            error: &error,
                            records: &attempt_records,
                        },
                    );
                    yield Ok::<String, std::convert::Infallible>(encode_error_chunk(&req_id, &error, exit));
                    return;
                }
            }
        };

        debug_assert!(upstream_completed, "正常流式完成路径必须收到上游 Done 事件");
        // 已收到明确 Done 后才更新成功率、粘性路由并发送完成帧。首事件后的异常
        // 不会换家，也不会伪装成成功；已经输出的部分会作为 incomplete exchange
        // 落库，下一轮仍能保持与客户端所见一致的上下文。
        let final_usage = usage.unwrap_or_default();
        let prompt_tokens = final_usage.prompt_tokens;
        let completion_tokens = final_usage.completion_tokens;
        let total_tokens = final_usage.total_tokens;
        let latency = started.elapsed().as_millis() as u64;
        // 记账收敛到共享函数：**删不掉那行吞吐记录**
        // （单独删时没有用例会红，见 `record_success_with_throughput` 的注释）。
        crate::proxy::health::record_success_with_throughput(
            stream_state.health.as_ref(),
            &provider_id,
            &model,
            latency as u32,
            completion_tokens,
        );
        stream_state
            .router
            .consume(&candidate.provider, &candidate.model, total_tokens);

        let assistant = Message {
            role: Role::Assistant,
            content: Content::Text(full),
            tool_calls: crate::protocol::convert::tool_calls_from_openai(&streamed_tool_call_deltas),
            tool_call_id: None,
            name: None,
        };
        if let Err(error) = stream_state
            .ctx
            .append_exchange_with(
                &stream_session_id,
                ExchangeWrite {
                    incoming: &new_messages,
                    assistant: &assistant,
                    routed_provider: Some(&provider_id),
                    routed_model: Some(&model),
                    prompt_tokens: prompt_tokens as i64,
                    completion_tokens: completion_tokens as i64,
                },
            )
            .await
        {
            tracing::error!("流式响应成功但上下文持久化失败: {error}");
            yield Ok::<String, std::convert::Infallible>(encode_error_chunk(&req_id, &error, exit));
            return;
        }
        if let Err(error) = repo::update_sticky(
            stream_state.db.pool(),
            &stream_session_id,
            &provider_id,
            &model,
            now_secs() + stream_state.cfg_snapshot().sticky_ttl_secs,
        )
        .await
        {
            tracing::warn!("更新流式会话粘性路由失败: {error}");
        }
        schedule_compaction(stream_state.clone(), stream_session_id.clone());

        if let Some(stream) = anthropic_stream.as_mut() {
            for encoded in stream.finish() {
                yield Ok::<String, std::convert::Infallible>(encoded);
            }
        }
        if let Some(stream) = responses_stream.as_mut() {
            for encoded in stream.finish() {
                yield Ok::<String, std::convert::Infallible>(encoded);
            }
        }

        yield Ok::<String, std::convert::Infallible>(encode_done(
            &req_id,
            &model,
            final_finish_reason.as_deref(),
            &final_usage,
            exit,
        ));
        if let Some(end) = encode_stream_end(&req_id, exit) {
            yield Ok::<String, std::convert::Infallible>(end);
        }

        let audit_state = stream_state.clone();
        let audit_session_id = stream_session_id.clone();
        let audit_provider_id = provider_id.clone();
        let audit_model = model.clone();
        let audit_client = stream_client.clone();
        let audit_requested_model = requested_model.clone();
        let (audit_cost, audit_currency, audit_rate_label) = price_charge(
            candidate.model.price.as_ref(),
            prompt_tokens as i64,
            completion_tokens as i64,
            final_usage.cache_read_tokens as i64,
            final_usage.cache_creation_tokens as i64,
        );
        let estimated_prompt = estimate_message_tokens(&req.messages) as i64;
        // 用协程外面备好的那份（见 `stream_trace_id` 的说明）。
        let audit_trace_id = stream_trace_id;
        let audit_attempts =
            attempts_json_traced(&attempt_records, Some(&audit_trace_id));
        tokio::spawn(async move {
            let _ = repo::log_request(
                audit_state.db.pool(),
                repo::RequestLog {
                    session_id: Some(&audit_session_id),
                    client: audit_client.as_deref(),
                    // 这次消费归属的远程 Key。本机统一 Key 的请求是 None。
                    // 与 client 同源派生，不另写一套前缀解析。
                    access_key_id: crate::budget::access_key_id_of(audit_client.as_deref()),
                    // 改写后的提示词默认不落库；只有开了 audit.store_refined_prompt
                    // 才会由 refined_prompt_to_store 返回脱敏后的文本。失败路径一律 None。
                    refined_prompt: None,
                    trace_id: &audit_trace_id,
                    requested_model: &audit_requested_model,
                    routed_provider: Some(&audit_provider_id),
                    routed_model: Some(&audit_model),
                    status: Some(200),
                    latency_ms: latency as i64,
                    prompt_tokens: prompt_tokens as i64,
                    completion_tokens: completion_tokens as i64,
                    fallback_attempts: fallback_count(stream_attempts),
                    error: None,
                    cost: audit_cost,
                    currency: audit_currency,
                    rate_label: audit_rate_label.as_deref(),
                    estimated_prompt_tokens: Some(estimated_prompt),
                    attempts_json: audit_attempts.as_deref(),
                    route: stream_route,
                },
            )
            .await;
        });
    };

    let body = Body::from_stream(s.map(|chunk| {
        let data = match chunk {
            Ok(data) => data,
            Err(never) => match never {},
        };
        Ok::<Bytes, std::convert::Infallible>(Bytes::from(data))
    }));
    let mut resp = body.into_response();
    apply_route_headers(&route, resp.headers_mut());
    let h = resp.headers_mut();
    h.insert("x-session-id", parse_header(&response_session_id));
    // B4：traceId **永远**回给客户端，即使 OTLP 导出关着。
    // 否则「导不出」会退化成「查不到」——排查时手里一个可追的标识都没有。
    h.insert("x-trace-id", parse_header(&trace_id));
    h.insert("x-routed-via", parse_header(&routed_via));
    h.insert("x-fallback-attempts", parse_header(&attempts.to_string()));
    h.insert("x-accel-buffering", parse_header("no"));
    // 流式请求明确不缓存，并说明原因。
    //
    // 为什么要在流式路径也发这个头：卡片要求「流式请求返回 BYPASS」。
    // 流式走的是另一条分支、根本不碰缓存，所以如果不在这儿显式发一个头，
    // 客户端看到的就是「什么都没有」——那与「缓存功能没生效」无法区分。
    // 缓存关着时一个头都不加（铁律 2）。
    if cfg.cache.enabled {
        h.insert(
            "x-cache",
            parse_header(crate::cache::CacheOutcome::Bypass.code()),
        );
        h.insert(
            "x-cache-reason",
            parse_header(crate::cache::BypassReason::Streaming.code()),
        );
    }
    h.insert(
        "content-type",
        parse_header(match exit {
            Exit::Ollama => "application/x-ndjson; charset=utf-8",
            _ => "text/event-stream; charset=utf-8",
        }),
    );
    h.insert("cache-control", parse_header("no-cache"));
    resp
}

/// 各方言会分段或在结尾重复报告用量；按维度取较完整的值，并保证 total
/// 不会小于输入与输出之和。这样 Anthropic 的 input/output 分离事件和 OpenAI
/// 的 usage-only chunk 都能落到同一条审计记录。
fn merge_usage(current: &mut Option<Usage>, next: Usage) {
    let usage = current.get_or_insert_with(Usage::default);
    usage.prompt_tokens = usage.prompt_tokens.max(next.prompt_tokens);
    usage.completion_tokens = usage.completion_tokens.max(next.completion_tokens);
    usage.cache_read_tokens = usage.cache_read_tokens.max(next.cache_read_tokens);
    usage.cache_creation_tokens = usage.cache_creation_tokens.max(next.cache_creation_tokens);
    usage.total_tokens = usage
        .total_tokens
        .max(next.total_tokens)
        .max(usage.prompt_tokens.saturating_add(usage.completion_tokens));
}

/// OpenAI 会把一个 function call 分散到多个 streaming delta 中。持久化层需要
/// 完整的 assistant tool_calls 才能让下一轮 tool result 与原始调用重新关联。
fn merge_tool_call_deltas(merged: &mut Vec<serde_json::Value>, delta: &serde_json::Value) {
    let calls: Vec<&serde_json::Value> = delta
        .as_array()
        .map(|calls| calls.iter().collect())
        .unwrap_or_else(|| vec![delta]);

    for (position, call) in calls.into_iter().enumerate() {
        let index = call
            .get("index")
            .and_then(|index| index.as_u64())
            .map(|index| index as usize)
            .unwrap_or(position);
        while merged.len() <= index {
            let next_index = merged.len();
            merged.push(serde_json::json!({
                "index": next_index,
                "type": "function",
                "function": { "arguments": "" },
            }));
        }
        let target = &mut merged[index];
        if let Some(id) = call.get("id").and_then(|value| value.as_str()) {
            target["id"] = serde_json::json!(id);
        }
        if let Some(kind) = call
            .get("type")
            .or_else(|| call.get("kind"))
            .and_then(|value| value.as_str())
        {
            target["type"] = serde_json::json!(kind);
        }
        if let Some(name) = call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(|value| value.as_str())
        {
            target["function"]["name"] = serde_json::json!(name);
        }
        if let Some(arguments) = call
            .get("function")
            .and_then(|function| function.get("arguments"))
            .and_then(|value| value.as_str())
        {
            let prior = target["function"]["arguments"].as_str().unwrap_or_default();
            target["function"]["arguments"] = serde_json::json!(format!("{prior}{arguments}"));
        }
    }
}

fn record_candidate_failure(
    state: &GatewayState,
    candidate: &crate::router::score::Candidate,
    error: &GatewayError,
) {
    if let GatewayError::Upstream { status: 429, .. } = error {
        state
            .router
            .mark_rate_limited(&candidate.provider, &candidate.model);
    }
    state
        .health
        .record_failure(&candidate.provider.id, &candidate.model.upstream, error);
}

/// 首个事件已经发给客户端后，上游异常不能切换 Provider；否则回答会混入两家
/// 模型的内容。为保证下一轮上下文不与客户端所见分叉，把已经发出的文本和工具
/// 调用作为不完整交换持久化，但不更新成功率、配额或 sticky 路由。
struct IncompleteStreamExchange<'a> {
    session_id: &'a str,
    incoming: &'a [Message],
    provider_id: &'a str,
    model: &'a str,
    content: &'a str,
    tool_call_deltas: &'a [serde_json::Value],
    usage: Option<&'a Usage>,
}

async fn persist_incomplete_stream_exchange(
    state: &GatewayState,
    exchange: IncompleteStreamExchange<'_>,
) {
    if exchange.content.is_empty() && exchange.tool_call_deltas.is_empty() {
        return;
    }
    let usage = exchange.usage.cloned().unwrap_or_default();
    let assistant = Message {
        role: Role::Assistant,
        content: Content::Text(exchange.content.to_owned()),
        tool_calls: crate::protocol::convert::tool_calls_from_openai(exchange.tool_call_deltas),
        tool_call_id: None,
        name: None,
    };
    if let Err(error) = state
        .ctx
        .append_exchange_with(
            exchange.session_id,
            ExchangeWrite {
                incoming: exchange.incoming,
                assistant: &assistant,
                routed_provider: Some(exchange.provider_id),
                routed_model: Some(exchange.model),
                prompt_tokens: usage.prompt_tokens as i64,
                completion_tokens: usage.completion_tokens as i64,
            },
        )
        .await
    {
        tracing::error!("流式响应中断且不完整上下文持久化失败: {error}");
    }
}

fn fallback_count(upstream_attempts: usize) -> i64 {
    upstream_attempts.saturating_sub(1) as i64
}

/// UTC 当日分钟数，用于匹配时段价规则。
fn utc_minute_of_day() -> u16 {
    use chrono::Timelike;
    let now = chrono::Utc::now();
    (now.hour() * 60 + now.minute()) as u16
}

/// 按「实际路由到的模型价格」计价：先按输入长度选档，再乘时段倍率。
/// 缺价格时必须保持 `None`，不能写成 0；档位说明一并返回供审计对账。
fn price_charge(
    price: Option<&crate::domain::ModelPrice>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
) -> (Option<f64>, Option<&'static str>, Option<String>) {
    match price {
        Some(price) => {
            let charge = price.charge_with_cache(
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                utc_minute_of_day(),
            );
            (
                Some(charge.cost),
                Some(charge.currency.code()),
                charge.label,
            )
        }
        None => (None, None, None),
    }
}

/// 把降级链的每一跳序列化成审计用的 JSON 数组。
///
/// **B4：每一跳都补上 `trace_id` 与 `attempt` 序号。**
/// 同一个请求的所有尝试共用一个 traceId，但 attempt 从 0 递增 ——
/// 这样「降级 3 次分别打到了哪」才能从**一条记录**里看出来，
/// 而不必去翻三条日志再自己按时间对齐。
///
/// `attempt` 由**数组下标**决定，不是从记录里读的：记录本身没有这个字段，
/// 而按遍历顺序编号正是调用方看到的顺序，不会与排序后的下标不一致。
fn attempts_json(records: &[crate::router::failover::AttemptRecord]) -> Option<String> {
    attempts_json_traced(records, None)
}

fn attempts_json_traced(
    records: &[crate::router::failover::AttemptRecord],
    trace_id: Option<&str>,
) -> Option<String> {
    if records.is_empty() {
        return None;
    }
    let Some(trace_id) = trace_id else {
        // 没有 traceId 时保持旧形状（那几条不在 dispatch 里的路径）。
        return serde_json::to_string(records).ok();
    };
    let enriched: Vec<serde_json::Value> = records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let mut value = serde_json::to_value(record).unwrap_or(serde_json::Value::Null);
            if let Some(obj) = value.as_object_mut() {
                obj.insert("trace_id".into(), serde_json::json!(trace_id));
                obj.insert("attempt".into(), serde_json::json!(index));
            }
            value
        })
        .collect();
    serde_json::to_string(&enriched).ok()
}

/// 首个流式事件之前整体失败的请求（所有候选都试过或不可重试）的审计上下文。
struct FailedStreamAudit<'a> {
    session_id: &'a str,
    requested_model: &'a str,
    client: Option<&'a str>,
    attempts: usize,
    started: Instant,
    error: &'a GatewayError,
    records: &'a [crate::router::failover::AttemptRecord],
}

fn spawn_failed_stream_audit(state: Arc<GatewayState>, audit: FailedStreamAudit<'_>) {
    let FailedStreamAudit {
        session_id,
        requested_model,
        client,
        attempts,
        started,
        error,
        records,
    } = audit;
    let session_id = session_id.to_string();
    let requested_model = requested_model.to_string();
    let client = client.map(str::to_owned);
    let status = error.http_status().as_u16() as i64;
    let kind = classify(error);
    let attempts_json = attempts_json(records);
    tokio::spawn(async move {
        let _ = repo::log_request(
            state.db.pool(),
            repo::RequestLog {
                session_id: Some(&session_id),
                client: client.as_deref(),
                // 这次消费归属的远程 Key。本机统一 Key 的请求是 None。
                // 与 client 同源派生，不另写一套前缀解析。
                access_key_id: crate::budget::access_key_id_of(client.as_deref()),
                // 改写后的提示词默认不落库；只有开了 audit.store_refined_prompt
                // 才会由 refined_prompt_to_store 返回脱敏后的文本。失败路径一律 None。
                refined_prompt: None,
                // 这条路径不在 dispatch 里（没有入口处的 traceId 可用），
                // 就地生成一个：本地永远要有，不能因为路径不同就留空。
                trace_id: &crate::trace::new_trace_id(),
                requested_model: &requested_model,
                routed_provider: None,
                routed_model: None,
                status: Some(status),
                latency_ms: started.elapsed().as_millis() as i64,
                prompt_tokens: 0,
                completion_tokens: 0,
                fallback_attempts: fallback_count(attempts),
                error: Some(kind),
                cost: None,
                currency: None,
                rate_label: None,
                estimated_prompt_tokens: None,
                attempts_json: attempts_json.as_deref(),
                route: Default::default(),
            },
        )
        .await;
    });
}

/* ---------------------------- 响应编码 ---------------------------- */

fn responses_usage_json(usage: &Usage) -> serde_json::Value {
    let mut usage_json = serde_json::json!({
        "input_tokens": usage.prompt_tokens,
        "output_tokens": usage.completion_tokens,
        "total_tokens": usage.total_tokens,
    });
    if usage.cache_read_tokens > 0 || usage.cache_creation_tokens > 0 {
        let mut details = serde_json::Map::new();
        if usage.cache_read_tokens > 0 {
            details.insert(
                "cached_tokens".into(),
                serde_json::json!(usage.cache_read_tokens),
            );
        }
        if usage.cache_creation_tokens > 0 {
            details.insert(
                "cache_write_tokens".into(),
                serde_json::json!(usage.cache_creation_tokens),
            );
        }
        usage_json["input_tokens_details"] = serde_json::Value::Object(details);
    }
    usage_json
}

fn encode_response(
    resp: &crate::domain::ChatResponse,
    model: &str,
    exit: Exit,
) -> serde_json::Value {
    match exit {
        Exit::OpenAI => crate::protocol::openai::chat_completion_response(&resp.id, model, resp),
        Exit::Anthropic => crate::protocol::anthropic::messages_response(resp, model),
        // C2：Gemini 原生的形状是 `candidates[].content.parts[]`，不是 OpenAI 的
        // `choices` —— 客户端按同名形状读，返回错形状会得到「响应为空」而不是报错。
        Exit::Gemini => crate::protocol::gemini_inbound::from_internal_response(resp),
        Exit::Responses => {
            let mut output = Vec::new();
            let has_tool_calls = resp
                .tool_calls
                .as_ref()
                .map(|calls| !calls.is_empty())
                .unwrap_or(false);

            // Responses 将文本消息和 function call 表示为两种 output item。不能只
            // 编码文本，否则非流式工具调用会在协议出口被静默丢弃，调用方也无法续轮。
            if !resp.content.is_empty() || !has_tool_calls {
                output.push(serde_json::json!({
                    "id": format!("msg_{}", resp.id),
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": resp.content }],
                }));
            }
            if let Some(tool_calls) = &resp.tool_calls {
                for (index, call) in tool_calls.iter().enumerate() {
                    let (name, namespace) = restore_response_function_name(&call.function.name);
                    let mut item = serde_json::json!({
                        "id": format!("fc_{}_{}", resp.id, index),
                        "type": "function_call",
                        "status": "completed",
                        "call_id": call.id,
                        "name": name,
                        "arguments": call.function.arguments,
                    });
                    if let Some(namespace) = namespace {
                        item["namespace"] = serde_json::json!(namespace);
                    }
                    output.push(item);
                }
            }

            serde_json::json!({
                "id": resp.id,
                "object": "response",
                "model": model,
                "status": "completed",
                "output": output,
                "usage": responses_usage_json(&resp.usage.clone().unwrap_or_default()),
            })
        }
        Exit::Ollama => crate::protocol::ollama::ollama_response(resp, model),
    }
}

fn encode_delta(
    req_id: &str,
    model: &str,
    text: &str,
    include_openai_role: bool,
    exit: Exit,
) -> String {
    match exit {
        Exit::OpenAI => {
            let mut delta = serde_json::json!({ "content": text });
            if include_openai_role {
                delta["role"] = serde_json::json!("assistant");
            }
            sse_event(
                None,
                crate::protocol::openai::sse_chunk(req_id, model, &delta, None),
            )
        }
        Exit::Anthropic => sse_event(
            Some("content_block_delta"),
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": text },
            })
            .to_string(),
        ),
        Exit::Responses => sse_event(
            Some("response.output_text.delta"),
            serde_json::json!({ "type": "response.output_text.delta", "delta": text }).to_string(),
        ),
        Exit::Ollama => ndjson_line(&serde_json::json!({
            "model": model,
            "message": { "role": "assistant", "content": text },
            "done": false,
        })),
        // C2：Gemini 的增量帧是 `candidates[].content.parts[].text`，
        // 且**没有** `[DONE]` 哨兵（结束靠 finishReason + 连接关闭）。
        Exit::Gemini => crate::protocol::gemini_inbound::sse_frame(
            &crate::protocol::gemini_inbound::sse_delta_chunk(text, model),
        ),
    }
}

fn encode_tool_delta(
    req_id: &str,
    model: &str,
    tc: &serde_json::Value,
    include_openai_role: bool,
    exit: Exit,
) -> String {
    match exit {
        Exit::OpenAI => {
            let mut delta = serde_json::json!({ "tool_calls": tc });
            if include_openai_role {
                delta["role"] = serde_json::json!("assistant");
            }
            sse_event(
                None,
                crate::protocol::openai::sse_chunk(req_id, model, &delta, None),
            )
        }
        Exit::Anthropic => {
            let mut encoded = String::new();
            for (position, call) in tool_call_values(tc).into_iter().enumerate() {
                let index = call
                    .get("index")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(position as u64);
                let function = call.get("function");
                if let (Some(id), Some(name)) = (
                    call.get("id").and_then(|value| value.as_str()),
                    function
                        .and_then(|value| value.get("name"))
                        .and_then(|value| value.as_str()),
                ) {
                    encoded.push_str(&sse_event(
                        Some("content_block_start"),
                        serde_json::json!({
                            "type": "content_block_start",
                            "index": index,
                            "content_block": {
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": {},
                            },
                        })
                        .to_string(),
                    ));
                }
                if let Some(arguments) = function
                    .and_then(|value| value.get("arguments"))
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                {
                    encoded.push_str(&sse_event(
                        Some("content_block_delta"),
                        serde_json::json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "input_json_delta", "partial_json": arguments },
                        })
                        .to_string(),
                    ));
                }
            }
            encoded
        }
        Exit::Responses => {
            let mut encoded = String::new();
            for (position, call) in tool_call_values(tc).into_iter().enumerate() {
                let index = call
                    .get("index")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(position as u64);
                let id = call
                    .get("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("tool-call");
                let arguments = call
                    .get("function")
                    .and_then(|value| value.get("arguments"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                encoded.push_str(&sse_event(
                    Some("response.function_call_arguments.delta"),
                    serde_json::json!({
                        "type": "response.function_call_arguments.delta",
                        "item_id": id,
                        "output_index": index,
                        "delta": arguments,
                    })
                    .to_string(),
                ));
            }
            encoded
        }
        Exit::Ollama => ndjson_line(&serde_json::json!({
            "model": model,
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": tc,
            },
            "done": false,
        })),
        // C2：Gemini 用 `functionCall` part 表示工具调用，
        // 与 OpenAI 的 `tool_calls[].function` 不是一种形状。
        Exit::Gemini => {
            let mut encoded = String::new();
            for (position, call) in tool_call_values(tc).into_iter().enumerate() {
                let _ = position;
                let Some(name) = call
                    .get("function")
                    .and_then(|value| value.get("name"))
                    .and_then(|value| value.as_str())
                else {
                    continue;
                };
                let args = call
                    .get("function")
                    .and_then(|value| value.get("arguments"))
                    .and_then(|value| value.as_str())
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                encoded.push_str(&crate::protocol::gemini_inbound::sse_frame(
                    &serde_json::json!({
                        "candidates": [{
                            "content": {
                                "role": "model",
                                "parts": [{"functionCall": {"name": name, "args": args}}],
                            },
                            "index": 0,
                        }],
                        "modelVersion": model,
                    }),
                ));
            }
            encoded
        }
    }
}

fn encode_done(
    req_id: &str,
    model: &str,
    finish: Option<&str>,
    usage: &Usage,
    exit: Exit,
) -> String {
    match exit {
        Exit::OpenAI => sse_event(
            None,
            crate::protocol::openai::sse_chunk(
                req_id,
                model,
                &serde_json::json!({}),
                Some(finish.unwrap_or("stop")),
            ),
        ),
        Exit::Anthropic => sse_event(
            Some("message_delta"),
            serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": map_stop_reason(finish), "stop_sequence": null },
                "usage": { "output_tokens": usage.completion_tokens },
            })
            .to_string(),
        ),
        // C2：Gemini 的收尾帧是一个**普通的 data 帧**，带 finishReason 与用量，
        // 而不是一个具名事件。
        Exit::Gemini => crate::protocol::gemini_inbound::sse_frame(&serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": []},
                "finishReason":
                    crate::protocol::gemini_inbound::gemini_finish_reason(finish),
                "index": 0,
            }],
            "modelVersion": model,
            "usageMetadata": {
                "promptTokenCount": usage.prompt_tokens,
                "candidatesTokenCount": usage.completion_tokens,
                "totalTokenCount": usage.total_tokens,
            },
        })),
        Exit::Responses => sse_event(
            Some("response.completed"),
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": req_id,
                    "object": "response",
                    "model": model,
                    "status": "completed",
                    "output": [],
                    "usage": {
                        "input_tokens": usage.prompt_tokens,
                        "output_tokens": usage.completion_tokens,
                        "total_tokens": usage.total_tokens,
                        "input_tokens_details": {
                            "cached_tokens": usage.cache_read_tokens,
                            "cache_write_tokens": usage.cache_creation_tokens,
                        },
                    },
                },
            })
            .to_string(),
        ),
        Exit::Ollama => ndjson_line(&serde_json::json!({
            "model": model,
            "message": { "role": "assistant", "content": "" },
            "done": true,
        })),
    }
}

fn encode_error_chunk(_req_id: &str, e: &GatewayError, exit: Exit) -> String {
    match exit {
        Exit::OpenAI => sse_event(
            None,
            serde_json::json!({ "error": { "message": e.public_message() } }).to_string(),
        ),
        Exit::Ollama => ndjson_line(&serde_json::json!({
            "error": e.public_message(),
            "done": true,
        })),
        _ => sse_event(
            Some("error"),
            serde_json::json!({ "type": "error", "error": { "message": e.public_message() } })
                .to_string(),
        ),
    }
}

fn encode_stream_end(_req_id: &str, exit: Exit) -> Option<String> {
    match exit {
        Exit::OpenAI => Some(sse_event(None, "[DONE]")),
        Exit::Anthropic => Some(sse_event(
            Some("message_stop"),
            serde_json::json!({ "type": "message_stop" }).to_string(),
        )),
        Exit::Responses | Exit::Ollama => None,
        // C2：Gemini 的 `alt=sse` **没有** `[DONE]` 哨兵 ——
        // 结束靠最后一帧的 `finishReason` 与连接关闭。
        // 加一个会让严格按协议解析的客户端（Gemini SDK）报错。
        Exit::Gemini => None,
    }
}

fn sse_event(event: Option<&str>, data: impl AsRef<str>) -> String {
    let data = data.as_ref();
    let mut encoded = String::new();
    if let Some(event) = event {
        encoded.push_str("event: ");
        encoded.push_str(event);
        encoded.push('\n');
    }
    if data.is_empty() {
        encoded.push_str("data:\n\n");
        return encoded;
    }
    for line in data.lines() {
        encoded.push_str("data: ");
        encoded.push_str(line);
        encoded.push('\n');
    }
    encoded.push('\n');
    encoded
}

fn ndjson_line(value: &serde_json::Value) -> String {
    format!("{value}\n")
}

fn tool_call_values(value: &serde_json::Value) -> Vec<&serde_json::Value> {
    value
        .as_array()
        .map(|calls| calls.iter().collect())
        .unwrap_or_else(|| vec![value])
}

fn anthropic_input_json_delta(index: usize, partial_json: &str) -> String {
    sse_event(
        Some("content_block_delta"),
        serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": { "type": "input_json_delta", "partial_json": partial_json },
        })
        .to_string(),
    )
}

fn anthropic_content_block_stop(index: usize) -> String {
    sse_event(
        Some("content_block_stop"),
        serde_json::json!({ "type": "content_block_stop", "index": index }).to_string(),
    )
}

fn response_event(event: &str, value: serde_json::Value) -> String {
    sse_event(Some(event), value.to_string())
}

fn response_function_arguments_delta(item_id: &str, output_index: usize, delta: &str) -> String {
    response_event(
        "response.function_call_arguments.delta",
        serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "item_id": item_id,
            "output_index": output_index,
            "delta": delta,
        }),
    )
}

fn map_stop_reason(finish: Option<&str>) -> &'static str {
    match finish {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        _ => "end_turn",
    }
}

/* -------------------------------- 工具 -------------------------------- */

fn error_response(e: &GatewayError) -> Response {
    let status = e.http_status();
    let body = e.to_openai_error();
    (status, Json(body)).into_response()
}

fn parse_header(s: &str) -> axum::http::HeaderValue {
    // **无条件**编码，而不是只在 `from_str` 报错时才编码。
    //
    // `HeaderValue::from_str` 会放行 0x80–0xFF 的原始字节，所以中文能写进去；
    // 但客户端的 `HeaderValue::to_str()` 在遇到非 ASCII 时返回 `Err`——
    // 也就是说头「存在」却**读不出来**，看起来和没发一样。
    // 这个坑是端到端测试逼出来的：`tests/route_headers.rs` 里
    // 「改写失败要写明原因」一条长期报「头是空的」。
    axum::http::HeaderValue::from_str(&encode_header_value(s))
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static(""))
}

/// 把任意文本转成可以安全放进 HTTP 头的形式。
///
/// 纯 ASCII 原样返回（含 `:` `/` `-` 等），其余字节转成 UTF-8 的 `%XX`。
/// 抽成纯函数是为了能单独打测——「中文会不会被吞掉」在端到端测试里
/// 只表现为「头读不出来」，很容易被当成测试写错而放过。
fn encode_header_value(s: &str) -> String {
    if s.bytes()
        .all(|byte| (0x20..=0x7E).contains(&byte) && byte != b'"' && byte != b'\\')
    {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        let byte = *byte;
        match byte {
            0x20..=0x7E if byte != b'"' && byte != b'\\' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

impl GatewayState {
    /// 估算校准快照。表很小（每个配置模型一行），每请求读取一次比维护带失效
    /// 逻辑的内存缓存更不容易出错；读取失败时按「未校准」处理，绝不因此拦请求。
    pub async fn calibration_snapshot(&self) -> CalibrationMap {
        match repo::list_calibrations(self.db.pool()).await {
            Ok(rows) => rows
                .into_iter()
                .map(|row| (calibration_key(&row.provider_id, &row.model), row.ratio))
                .collect(),
            Err(error) => {
                tracing::warn!("读取 token 校准表失败，本次按未校准处理: {error}");
                CalibrationMap::new()
            }
        }
    }

    /// 用最便宜的可用模型生成摘要（压缩上下文时调用）
    pub async fn summarize(&self, msgs: Vec<Message>, prev: &str) -> Result<String> {
        use crate::context::summarization_prompt;

        let mut all: Vec<Message> = vec![Message::system(summarization_prompt(prev))];
        all.extend(msgs);

        let req = ChatRequest {
            model: "auto".into(),
            messages: all,
            temperature: Some(0.2),
            top_p: None,
            max_tokens: Some(800),
            stop: None,
            stream: false,
            tools: None,
            tool_choice: None,
            thinking: None,
            extra: Default::default(),
        };

        let providers = self.providers.read().clone();
        let candidates = self.router.resolve("auto", &providers)?;
        let mut cfg = self.cfg_snapshot();
        // 摘要任务挑便宜快的：用 Fastest 策略，且只需要 2 次尝试
        cfg.routing_strategy = RoutingStrategy::Fastest;
        // 摘要调用是纯文本任务，不需要任何模态能力。
        let ranked = self.router.rank(candidates, &cfg, Default::default(), None);

        let flag = AtomicFlag::new();
        let chain = FailoverChain::new(&ranked, 2, &flag);
        let up = self.upstream.clone();
        let timeout = Duration::from_secs(60);
        let r = Arc::new(req);
        let defaults = self.cfg_snapshot().ollama_options;

        // 摘要调用同样走候选链，但它的尝试明细不写审计：这不是用户请求。
        let mut records = Vec::new();
        let o = chain
            .run(
                &mut records,
                |p, m| {
                    let up = up.clone();
                    let r = r.clone();
                    let defaults = defaults.clone();
                    async move { up.call(&p, &r, &m, timeout, &defaults).await }
                },
                |p, m, e| self.health.record_failure(&p.id, m, e),
            )
            .await?;

        Ok(o.value.content)
    }
}
