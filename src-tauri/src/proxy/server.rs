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

use crate::config::{AppConfig, RoutingStrategy};
use crate::context::{
    estimate_message_tokens, scoped_session_id, trim_to_budget, ContextStore, ExchangeWrite,
};
use crate::db::{self, repo};
use crate::domain::{
    ChatRequest, Content, FunctionCall, ImageUrl, Message, Part, Provider, RemoteAccessKey, Role,
    ToolCall, Usage,
};
use crate::error::{GatewayError, Result};
use crate::proxy::health::HealthRegistry;
use crate::proxy::upstream::{UpstreamClient, UpstreamEvent};
use crate::router::failover::{classify, AtomicFlag, FailoverChain};
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
    /// 只有实际 listener 位于回环地址时才会激活远程反代认证逻辑。
    listener_is_loopback: AtomicBool,
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

        Self {
            db,
            cfg: Arc::new(parking_lot::RwLock::new(cfg)),
            health,
            limiter,
            client_limiter: Arc::new(RateLimiter::new()),
            router,
            upstream: Arc::new(UpstreamClient::new()),
            ctx,
            providers: Arc::new(parking_lot::RwLock::new(Vec::new())),
            active: Arc::new(parking_lot::RwLock::new(None)),
            remote_access_keys: Arc::new(parking_lot::RwLock::new(Vec::new())),
            compaction_lock: tokio::sync::Mutex::new(()),
            listener_is_loopback: AtomicBool::new(false),
        }
    }

    pub async fn reload_providers(&self) -> Result<()> {
        let list = repo::list_providers(self.db.pool()).await?;
        let remote_keys = repo::list_remote_access_keys(self.db.pool()).await?;
        *self.providers.write() = list;
        *self.remote_access_keys.write() = remote_keys;
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

    /// 后台任务：日志清理 + 健康快照
    pub async fn background_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
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
        }
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
        // Anthropic 原生面（Claude Code / Claude Desktop 走这里）
        .route("/v1/messages", post(anthropic_messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        // Ollama 仿真面（Zed / JetBrains AI 走这里）
        .route("/api/chat", post(ollama_chat))
        .route("/api/tags", get(ollama_tags))
        // 网关自面
        .route("/healthz", get(healthz))
        .route("/gw/stats", get(gw_stats))
        .route("/gw/health", get(gw_health))
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

/// OpenAI 兼容入口
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

    // `input` 既可能是纯字符串，也可能是 Responses 原生 item 数组。后者与
    // Chat Completions 的 content 类型不同（input_text/function_call_output 等），
    // 不能先强转成 OaMessage，否则会把真实文本和工具续轮静默清空。
    let messages = match responses_input_messages(body.get("input")) {
        Ok(messages) => messages,
        Err(error) => return error_response(&error),
    };

    let req = ChatRequest {
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

    dispatch(state, req, &headers, auth.client, Exit::Responses).await
}

fn responses_input_messages(
    input: Option<&serde_json::Value>,
) -> std::result::Result<Vec<Message>, GatewayError> {
    match input {
        None => Ok(Vec::new()),
        Some(serde_json::Value::String(text)) => Ok(vec![Message::user(text)]),
        Some(serde_json::Value::Array(items)) => items.iter().map(response_input_item).collect(),
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
            let name = response_required_string(item, "name")?;
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
    Some(serde_json::Value::Array(
        tools
            .iter()
            .map(|tool| {
                if tool.get("type").and_then(|value| value.as_str()) == Some("function")
                    && tool.get("function").is_none()
                {
                    let mut function = serde_json::Map::new();
                    for key in ["name", "description", "parameters", "strict"] {
                        if let Some(value) = tool.get(key) {
                            function.insert(key.to_owned(), value.clone());
                        }
                    }
                    serde_json::json!({ "type": "function", "function": function })
                } else {
                    tool.clone()
                }
            })
            .collect(),
    ))
}

fn normalize_responses_tool_choice(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let choice = value?;
    if choice.get("type").and_then(|value| value.as_str()) == Some("function")
        && choice.get("function").is_none()
    {
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
        let input_tokens = initial_usage.map(|usage| usage.prompt_tokens).unwrap_or(0);
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
                    "usage": { "input_tokens": input_tokens, "output_tokens": 0 },
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
                        "item": {
                            "id": item_id,
                            "type": "function_call",
                            "status": "in_progress",
                            "call_id": call_id,
                            "name": name,
                            "arguments": "",
                        },
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
                    "name": name,
                    "arguments": block.arguments,
                }),
            ));
            output.push(response_event(
                "response.output_item.done",
                serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": {
                        "id": item_id,
                        "type": "function_call",
                        "status": "completed",
                        "call_id": call_id,
                        "name": name,
                        "arguments": block.arguments,
                    },
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

    // 4) 候选链
    let providers = state.providers.read().clone();
    let candidates = match state.router.resolve(&req.model, &providers) {
        Ok(c) => c,
        Err(e) => return error_response(&e),
    };

    // 路由前按候选窗口扣除输出、工具 schema 和协议余量。先强制压缩可压缩的
    // 持久化历史，再在必要时按完整工具交换裁剪；仍放不下时明确报错，绝不把
    // 超窗 payload 交给小窗口备选 Provider。
    let fixed_reserve = fixed_context_reserve(&req);
    if let Some(max_message_budget) = max_message_budget(&candidates, fixed_reserve) {
        if estimate_message_tokens(&req.messages) > max_message_budget {
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
        if before_trim > max_message_budget {
            req.messages = trim_to_budget(req.messages, max_message_budget);
            let after_trim = estimate_message_tokens(&req.messages);
            if after_trim > max_message_budget {
                return error_response(&GatewayError::ContextLengthExceeded {
                    required: before_trim.saturating_add(fixed_reserve),
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

    let needs_tools = req.tools.is_some();
    let needs_vision = req
        .messages
        .iter()
        .any(|m| matches!(&m.content, crate::domain::Content::Parts(ps) if ps.iter().any(|p| matches!(p, crate::domain::Part::ImageUrl { .. }))));

    let mut ranked = state.router.rank(
        candidates,
        &cfg,
        needs_tools,
        needs_vision,
        sticky.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
    );
    let request_message_tokens = estimate_message_tokens(&req.messages);
    ranked.retain(|candidate| {
        candidate_supports_context(candidate, request_message_tokens, fixed_reserve)
    });

    // UI 的“热切换”不应只是写一份状态。把用户指定的 provider 提到候选链首位，
    // 仍保留其余健康候选作为故障转移后备。
    if let Some(active_id) = state.active.read().clone() {
        if let Some(pos) = ranked.iter().position(|c| c.provider.id == active_id) {
            let active = ranked.remove(pos);
            ranked.insert(0, active);
        }
    }

    if ranked.is_empty() {
        return error_response(&GatewayError::AllProvidersFailed { attempts: 0 });
    }

    // 5) 执行（区分流式/非流式）
    let dispatch_input = DispatchInput {
        req,
        new_messages,
        response_session_id,
        session_id: session_id.clone(),
        client,
        ranked,
        cfg,
        exit,
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

fn fixed_context_reserve(req: &ChatRequest) -> u32 {
    let tool_schema_tokens = req
        .tools
        .as_ref()
        .and_then(|tools| serde_json::to_string(tools).ok())
        .map(|json| (json.chars().count() as u32).div_ceil(3))
        .unwrap_or(0);
    req.max_tokens
        .unwrap_or(DEFAULT_OUTPUT_RESERVE)
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
) -> bool {
    candidate.model.context_window <= 0
        || message_tokens.saturating_add(fixed_reserve) <= candidate.model.context_window as u32
}

fn schedule_compaction(state: Arc<GatewayState>, session_id: String) {
    tokio::spawn(async move {
        if let Err(error) = state.compact_session_if_needed(&session_id, false).await {
            tracing::warn!("响应后自动压缩失败: {error}");
        }
    });
}

/// 非流式
async fn normal_dispatch(state: Arc<GatewayState>, input: DispatchInput) -> Response {
    let DispatchInput {
        req,
        new_messages,
        response_session_id,
        session_id,
        client,
        ranked,
        cfg,
        exit,
    } = input;
    let started = Instant::now();
    let flag = AtomicFlag::new();
    let max_attempts = if cfg.failover_enabled {
        cfg.max_fallback_attempts
    } else {
        1
    };
    let chain = FailoverChain::new(&ranked, max_attempts, &flag);

    let upstream = state.upstream.clone();
    let timeout = Duration::from_secs(cfg.upstream_timeout_secs);
    let req_arc = Arc::new(req.clone());

    let health = state.health.clone();
    let router = state.router.clone();

    let outcome = chain
        .run(
            |provider, model| {
                let up = upstream.clone();
                let r = req_arc.clone();
                async move { up.call(&provider, &r, &model, timeout).await }
            },
            |provider, model, err| {
                // 429 额外打满本地额度窗口，避免连续撞墙
                if let GatewayError::Upstream { status: 429, .. } = err {
                    if let Some(m) = provider.models.iter().find(|m| m.upstream == model) {
                        router.mark_rate_limited(provider, m);
                    }
                }
                health.record_failure(&provider.id, model, err);
            },
        )
        .await;

    match outcome {
        Ok(o) => {
            let latency = started.elapsed().as_millis() as u64;
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
            let (pt, ct) = o
                .value
                .usage
                .as_ref()
                .map(|u| (u.prompt_tokens, u.completion_tokens))
                .unwrap_or((0, 0));
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
            schedule_compaction(state.clone(), session_id.clone());

            let audit_state = state.clone();
            let audit_session_id = session_id.clone();
            let audit_requested_model = req.model.clone();
            let audit_client = client.clone();
            let audit_pid = pid.clone();
            let audit_mid = mid.clone();
            let audit_fallback_attempts = fallback_count(o.attempts);
            tokio::spawn(async move {
                let _ = repo::log_request(
                    audit_state.db.pool(),
                    repo::RequestLog {
                        session_id: Some(&audit_session_id),
                        client: audit_client.as_deref(),
                        requested_model: &audit_requested_model,
                        routed_provider: Some(&audit_pid),
                        routed_model: Some(&audit_mid),
                        status: Some(200),
                        latency_ms: latency as i64,
                        prompt_tokens: pt as i64,
                        completion_tokens: ct as i64,
                        fallback_attempts: audit_fallback_attempts,
                        error: None,
                    },
                )
                .await;
            });

            let body = encode_response(&o.value, &o.model, exit);
            let mut resp = Json(body).into_response();
            let h = resp.headers_mut();
            h.insert(
                "x-routed-via",
                parse_header(&format!("{}/{}", o.provider_id, o.model)),
            );
            h.insert("x-fallback-attempts", parse_header(&o.attempts.to_string()));
            h.insert("x-session-id", parse_header(&response_session_id));
            resp
        }
        Err(e) => {
            let st = state.clone();
            let sid = session_id.clone();
            let kind = classify(&e);
            let status = e.http_status().as_u16() as i64;
            let requested_model = req.model.clone();
            let audit_client = client.clone();
            let audit_fallback_attempts = fallback_count(attempt_count_from_error(&e));
            tokio::spawn(async move {
                let _ = repo::log_request(
                    st.db.pool(),
                    repo::RequestLog {
                        session_id: Some(&sid),
                        client: audit_client.as_deref(),
                        requested_model: &requested_model,
                        routed_provider: None,
                        routed_model: None,
                        status: Some(status),
                        latency_ms: started.elapsed().as_millis() as i64,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        fallback_attempts: audit_fallback_attempts,
                        error: Some(kind),
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

    let DispatchInput {
        req,
        new_messages,
        response_session_id,
        session_id,
        client,
        ranked,
        cfg,
        exit,
    } = input;

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

    'select: for candidate in ranked.iter().take(max_attempts) {
        attempts += 1;
        let timeout = Duration::from_secs(cfg.upstream_timeout_secs);
        let mut upstream_stream = match state
            .upstream
            .call_stream(
                &candidate.provider,
                &req,
                &candidate.model.upstream,
                timeout,
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                record_candidate_failure(&state, candidate, &error);
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
                    &session_id,
                    &req.model,
                    client.as_deref(),
                    attempts,
                    started,
                    &error,
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
                        Exit::OpenAI | Exit::Ollama => {
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
                        Exit::OpenAI | Exit::Ollama => {
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
                        &stream_session_id,
                        &requested_model,
                        stream_client.as_deref(),
                        stream_attempts,
                        started,
                        &error,
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
                        &stream_session_id,
                        &requested_model,
                        stream_client.as_deref(),
                        stream_attempts,
                        started,
                        &error,
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
        stream_state
            .health
            .record_success(&provider_id, &model, latency as u32);
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
        tokio::spawn(async move {
            let _ = repo::log_request(
                audit_state.db.pool(),
                repo::RequestLog {
                    session_id: Some(&audit_session_id),
                    client: audit_client.as_deref(),
                    requested_model: &audit_requested_model,
                    routed_provider: Some(&audit_provider_id),
                    routed_model: Some(&audit_model),
                    status: Some(200),
                    latency_ms: latency as i64,
                    prompt_tokens: prompt_tokens as i64,
                    completion_tokens: completion_tokens as i64,
                    fallback_attempts: fallback_count(stream_attempts),
                    error: None,
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
    let h = resp.headers_mut();
    h.insert("x-session-id", parse_header(&response_session_id));
    h.insert("x-routed-via", parse_header(&routed_via));
    h.insert("x-fallback-attempts", parse_header(&attempts.to_string()));
    h.insert("x-accel-buffering", parse_header("no"));
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

fn attempt_count_from_error(error: &GatewayError) -> usize {
    match error {
        GatewayError::AllProvidersFailed { attempts } => *attempts,
        _ => 1,
    }
}

fn spawn_failed_stream_audit(
    state: Arc<GatewayState>,
    session_id: &str,
    requested_model: &str,
    client: Option<&str>,
    attempts: usize,
    started: Instant,
    error: &GatewayError,
) {
    let session_id = session_id.to_string();
    let requested_model = requested_model.to_string();
    let client = client.map(str::to_owned);
    let status = error.http_status().as_u16() as i64;
    let kind = classify(error);
    tokio::spawn(async move {
        let _ = repo::log_request(
            state.db.pool(),
            repo::RequestLog {
                session_id: Some(&session_id),
                client: client.as_deref(),
                requested_model: &requested_model,
                routed_provider: None,
                routed_model: None,
                status: Some(status),
                latency_ms: started.elapsed().as_millis() as i64,
                prompt_tokens: 0,
                completion_tokens: 0,
                fallback_attempts: fallback_count(attempts),
                error: Some(kind),
            },
        )
        .await;
    });
}

/* ---------------------------- 响应编码 ---------------------------- */

fn encode_response(
    resp: &crate::domain::ChatResponse,
    model: &str,
    exit: Exit,
) -> serde_json::Value {
    match exit {
        Exit::OpenAI => crate::protocol::openai::chat_completion_response(&resp.id, model, resp),
        Exit::Anthropic => crate::protocol::anthropic::messages_response(resp, model),
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
                    output.push(serde_json::json!({
                        "id": format!("fc_{}_{}", resp.id, index),
                        "type": "function_call",
                        "status": "completed",
                        "call_id": call.id,
                        "name": call.function.name,
                        "arguments": call.function.arguments,
                    }));
                }
            }

            let usage = resp.usage.clone().unwrap_or_default();
            serde_json::json!({
                "id": resp.id,
                "object": "response",
                "model": model,
                "status": "completed",
                "output": output,
                "usage": {
                    "input_tokens": usage.prompt_tokens,
                    "output_tokens": usage.completion_tokens,
                    "total_tokens": usage.total_tokens,
                },
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
    axum::http::HeaderValue::from_str(s)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static(""))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

impl GatewayState {
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
        let ranked = self.router.rank(candidates, &cfg, false, false, None);

        let flag = AtomicFlag::new();
        let chain = FailoverChain::new(&ranked, 2, &flag);
        let up = self.upstream.clone();
        let timeout = Duration::from_secs(60);
        let r = Arc::new(req);

        let o = chain
            .run(
                |p, m| {
                    let up = up.clone();
                    let r = r.clone();
                    async move { up.call(&p, &r, &m, timeout).await }
                },
                |p, m, e| self.health.record_failure(&p.id, m, e),
            )
            .await?;

        Ok(o.value.content)
    }
}
