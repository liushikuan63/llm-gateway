//! Desktop chat uses the authenticated gateway listener, including routing, budgets and audit.
//! Design and replay invariant: [request flow](../../docs/wiki/request-flows.md).
use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

pub const CANCELLED: &str = "desktop_chat_cancelled";
#[derive(Debug, Serialize)]
pub struct PersonalFailure {
    pub message: String,
    pub outcome: String,
}
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CommandError {
    Ordinary(String),
    Personal(PersonalFailure),
}
impl From<String> for CommandError {
    fn from(value: String) -> Self {
        Self::Ordinary(value)
    }
}
impl From<PersonalFailure> for CommandError {
    fn from(value: PersonalFailure) -> Self {
        Self::Personal(value)
    }
}
impl CommandError {
    pub fn before_dispatch(personal: bool, message: String) -> Self {
        if personal {
            Self::Personal(PersonalFailure {
                message,
                outcome: "not_dispatched".into(),
            })
        } else {
            Self::Ordinary(message)
        }
    }
}
const MAX_CONTROLLED_REQUESTS: usize = 32;

struct ControlledJob {
    owner: String,
    cancel: Option<oneshot::Sender<()>>,
}

/// Process-local UI request ownership only. No sessions, credentials or model
/// requests live here; dropping a request never guesses a usage refund.
#[derive(Default)]
pub struct RequestControls {
    jobs: parking_lot::Mutex<HashMap<uuid::Uuid, ControlledJob>>,
}

pub struct ControlledRequest {
    id: uuid::Uuid,
    controls: Arc<RequestControls>,
    cancelled: oneshot::Receiver<()>,
}

#[derive(Clone, Serialize)]
pub struct RequestReady {
    pub request_id: String,
    pub trace_id: String,
}

impl RequestControls {
    pub fn register(
        self: &Arc<Self>,
        owner: &str,
        request_id: &str,
    ) -> Result<ControlledRequest, String> {
        let id = parse_request_id(request_id)?;
        if owner.is_empty() || owner.len() > 128 {
            return Err("聊天窗口标识无效".into());
        }
        let mut jobs = self.jobs.lock();
        if jobs.contains_key(&id) {
            return Err("该聊天请求仍在处理中".into());
        }
        if jobs.len() >= MAX_CONTROLLED_REQUESTS {
            return Err("并行聊天请求过多，请等待已有请求结束".into());
        }
        let (cancel, cancelled) = oneshot::channel();
        jobs.insert(
            id,
            ControlledJob {
                owner: owner.into(),
                cancel: Some(cancel),
            },
        );
        Ok(ControlledRequest {
            id,
            controls: self.clone(),
            cancelled,
        })
    }

    pub fn cancel(&self, owner: &str, request_id: &str) -> Result<bool, String> {
        let id = parse_request_id(request_id)?;
        let sender = {
            let mut jobs = self.jobs.lock();
            jobs.get_mut(&id)
                .filter(|job| job.owner == owner)
                .and_then(|job| job.cancel.take())
        };
        // Keep the entry until its guard drops: an accepted cancellation cannot
        // allow an old guard to remove a newer request reusing the same id.
        Ok(sender.is_some_and(|sender| sender.send(()).is_ok()))
    }
}

fn parse_request_id(value: &str) -> Result<uuid::Uuid, String> {
    if value.len() > 64 {
        return Err("聊天请求标识无效".into());
    }
    uuid::Uuid::parse_str(value).map_err(|_| "聊天请求标识无效".into())
}

impl ControlledRequest {
    pub fn id(&self) -> uuid::Uuid {
        self.id
    }
    pub fn ready(&self) -> RequestReady {
        RequestReady {
            request_id: self.id.to_string(),
            trace_id: self.id.simple().to_string(),
        }
    }
    pub async fn run<T>(
        mut self,
        operation: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        match self.cancelled.try_recv() {
            Ok(()) | Err(oneshot::error::TryRecvError::Closed) => return Err(CANCELLED.into()),
            Err(oneshot::error::TryRecvError::Empty) => (),
        }
        tokio::pin!(operation);
        tokio::select! {
            biased;
            // A response already ready in this poll retains its known outcome.
            result = &mut operation => result,
            _ = &mut self.cancelled => Err(CANCELLED.into()),
        }
    }
}

impl Drop for ControlledRequest {
    fn drop(&mut self) {
        self.controls.jobs.lock().remove(&self.id);
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Preserve a completed assistant's separate reasoning channel on follow-up.
    /// This is conversational history, not usage or entitlement evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatInput {
    #[serde(default)]
    pub personal_test: bool,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub session_id: String,
    pub max_tokens: u32,
}

/// Observed field shape only; reported does not certify supplier settlement.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyFieldState {
    Reported,
    Unavailable,
    Invalid,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyUsageEvidence {
    Unobserved,
    Unknown,
    Complete,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ReplyMetadata {
    pub finish_reason: ReplyFieldState,
    pub usage: ReplyFieldState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_evidence: Option<ReplyUsageEvidence>,
}
#[derive(Deserialize)]
struct GatewayReplyObservation {
    version: u8,
    finish_reason: Option<Value>,
    usage_present: bool,
    usage_evidence: ReplyUsageEvidence,
}

/// Opt-in desktop metadata is captured before OpenAI's compatibility defaults.
/// Ordinary clients and stored cache JSON retain their existing wire contracts.
pub(crate) fn attach_gateway_observation(
    body: &mut Value,
    response: &crate::domain::ChatResponse,
    requested: bool,
    personal: bool,
) {
    if !requested {
        return;
    }
    let evidence = if personal {
        "unknown"
    } else {
        match response.reported_usage_evidence {
            crate::domain::ReportedUsageEvidence::Unobserved => "unobserved",
            crate::domain::ReportedUsageEvidence::Unknown => "unknown",
            crate::domain::ReportedUsageEvidence::Complete(_) => "complete",
        }
    };
    body["llmgw_reply_observation"] = serde_json::json!({
        "version": 1, "finish_reason": response.finish_reason,
        "usage_present": !personal && response.usage.is_some(), "usage_evidence": evidence,
    });
}

fn reply_observations(body: &Value) -> (Option<String>, Option<Value>, ReplyMetadata) {
    let mut finish = body.pointer("/choices/0/finish_reason").cloned();
    let mut usage = body.get("usage").filter(|v| !v.is_null()).cloned();
    let evidence = match body.get("llmgw_reply_observation") {
        None => None, // Older gateways expose declarations, not pre-normalization evidence.
        Some(value) => match serde_json::from_value::<GatewayReplyObservation>(value.clone()) {
            Ok(observed) if observed.version == 1 => {
                finish = observed.finish_reason;
                if !observed.usage_present {
                    usage = None;
                }
                Some(observed.usage_evidence)
            }
            _ => {
                return (
                    None,
                    None,
                    ReplyMetadata {
                        finish_reason: ReplyFieldState::Invalid,
                        usage: ReplyFieldState::Invalid,
                        usage_evidence: None,
                    },
                )
            }
        },
    };
    let finish_state = match finish.as_ref() {
        None | Some(Value::Null) => ReplyFieldState::Unavailable,
        Some(Value::String(value)) if value.trim().is_empty() => ReplyFieldState::Unavailable,
        Some(Value::String(_)) => ReplyFieldState::Reported,
        Some(_) => ReplyFieldState::Invalid,
    };
    let finish = match finish {
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value),
        _ => None,
    };
    let usage_state = match usage.as_ref() {
        None => ReplyFieldState::Unavailable,
        Some(Value::Object(_)) => ReplyFieldState::Reported,
        Some(_) => ReplyFieldState::Invalid,
    };
    // Preserve the original non-null JSON, including partial/extension fields.
    // State is about shape only; never fill unknown counts, prices or currency.
    (
        finish,
        usage,
        ReplyMetadata {
            finish_reason: finish_state,
            usage: usage_state,
            usage_evidence: evidence,
        },
    )
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personal_test_outcome: Option<String>,
    pub content: String,
    pub reasoning: Option<String>,
    /// Keep refusal separate from content so replay matches persisted history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    pub model: String,
    pub routed_via: Option<String>,
    pub session_id: String,
    pub trace_id: Option<String>,
    pub fallback_attempts: u32,
    pub latency_ms: u64,
    pub usage: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_metadata: Option<ReplyMetadata>,
    pub cached: bool,
}

pub fn validate(input: &ChatInput) -> Result<(), String> {
    if input.model.trim().is_empty() || input.model.len() > 512 {
        return Err("请选择模型或自动路由".into());
    }
    if uuid::Uuid::parse_str(&input.session_id).is_err() {
        return Err("聊天会话标识无效".into());
    }
    if !(1..=8192).contains(&input.max_tokens) {
        return Err("最大回复 token 数须为 1–8192".into());
    }
    if input.messages.is_empty()
        || input.messages.len() > 128
        || input
            .messages
            .iter()
            .map(|m| {
                m.content
                    .len()
                    .saturating_add(m.reasoning_content.as_deref().map_or(0, str::len))
            })
            .sum::<usize>()
            > 262_144
        || input
            .messages
            .last()
            .map_or(true, |m| m.role != "user" || m.content.trim().is_empty())
        || input.messages.iter().any(|m| {
            !matches!(m.role.as_str(), "system" | "user" | "assistant")
                || m.reasoning_content.is_some() && m.role != "assistant"
        })
    {
        return Err("聊天需以非空用户消息结束，最多 128 条、256 KiB 文本".into());
    }
    Ok(())
}

/// Allow the configured routing work to finish before the desktop gives up.
/// This is a conservative budget for known bounded stages, not a strict upper
/// bound on shared compaction/plugin lock queues, process cleanup or local I/O.
fn request_timeout(cfg: &AppConfig, requested_model: &str) -> Result<Duration, String> {
    use crate::config::RoutingStrategy;

    let excessive = || "聊天等待预算过大，请降低上游超时或降级次数".to_string();
    // Confirmation retries already consume the chain's attempt limit.
    let attempts = u64::try_from(if cfg.failover_enabled {
        cfg.max_fallback_attempts
    } else {
        1
    })
    .map_err(|_| excessive())?;
    // A virtual strategy can reduce this configured cascade bound, never raise it.
    let cascade = cfg.routing_strategy == RoutingStrategy::Cascade && cfg.cascade.enabled();
    let rounds = if cascade {
        u64::from(cfg.cascade.sanitized().max_escalations) + 1
    } else {
        1
    };
    // ProcessTransport gives stdin writes and response reads separate T budgets.
    // HTTP and built-in CLI adapters fit within that conservative two-phase bound.
    let phase_ms = cfg
        .upstream_timeout_secs
        .checked_mul(1000)
        .ok_or_else(excessive)?
        .max(1);
    let main_ms = phase_ms
        .checked_mul(2)
        .and_then(|v| v.checked_mul(attempts))
        .and_then(|v| v.checked_mul(rounds))
        .ok_or_else(excessive)?;
    // server::summarize: two attempts at 60s each, at most two request-time
    // compactions; retain the same plugin two-phase allowance for those calls.
    let compaction_ms = 2 * 2 * 2 * 60_000;
    // Keep the previous 60s allowance for local dispatch, persistence and delivery.
    let mut total_ms = main_ms
        .checked_add(compaction_ms)
        .and_then(|v| v.checked_add(60_000))
        .ok_or_else(excessive)?;
    let mut add = |millis: u64| -> Result<(), String> {
        total_ms = total_ms.checked_add(millis).ok_or_else(excessive)?;
        Ok(())
    };
    if cascade {
        add(cfg
            .smart_routing
            .jev
            .timeout_ms
            .max(100)
            .checked_mul(rounds)
            .ok_or_else(excessive)?)?;
    }
    if cfg.smart_routing.enabled
        && (cfg.routing_strategy == RoutingStrategy::Smart || requested_model.trim() == "smart")
    {
        // Bounds match autostart::ensure_running and the classifier consumer.
        if cfg.smart_routing.jev.auto_start.enabled {
            add(cfg
                .smart_routing
                .jev
                .auto_start
                .boot_wait_ms
                .clamp(100, 3000))?;
        }
        add(cfg.smart_routing.timeout_ms.max(100))?;
        if cfg.smart_routing.prompt_refine.enabled {
            add(cfg
                .smart_routing
                .prompt_refine
                .timeout_ms
                .clamp(200, 30_000))?;
        }
        if cfg.search.enabled {
            add(cfg.search.timeout_ms.clamp(500, 60_000))?;
        }
    }
    let timeout = Duration::from_millis(total_ms);
    // Avoid overflow in the timer used by reqwest/Tokio for hand-edited extremes.
    Instant::now().checked_add(timeout).ok_or_else(excessive)?;
    Ok(timeout)
}

pub async fn send(
    cfg: &AppConfig,
    listener: std::net::SocketAddr,
    input: ChatInput,
) -> Result<ChatResult, String> {
    send_with_trace(cfg, listener, input, None).await
}

pub async fn send_with_trace(
    cfg: &AppConfig,
    listener: std::net::SocketAddr,
    input: ChatInput,
    trace: Option<uuid::Uuid>,
) -> Result<ChatResult, String> {
    if input.personal_test {
        return Err("个人测试默认关闭；必须通过已归属的桌面会话及可信来源发送".into());
    }
    send_inner(cfg, listener, input, trace, None).await
}

pub(crate) async fn send_personal(
    cfg: &AppConfig,
    listener: std::net::SocketAddr,
    input: ChatInput,
    trace: uuid::Uuid,
    ticket: crate::personal_test::Ticket,
) -> Result<ChatResult, PersonalFailure> {
    let mut result = send_inner(cfg, listener, input, Some(trace), Some(&ticket.token))
        .await
        .map_err(|message| PersonalFailure {
            message,
            outcome: ticket.ctx.outcome().into(),
        })?;
    result.personal_test_outcome = Some(ticket.ctx.outcome().into());
    Ok(result)
}

pub async fn send_controlled_personal(
    gateway: &crate::proxy::server::GatewayState,
    owner: &str,
    request: ControlledRequest,
    input: ChatInput,
) -> Result<ChatResult, PersonalFailure> {
    let trace = request.id();
    let progress = Arc::new(crate::personal_test::Progress::default());
    request
        .run(async {
            let cfg = gateway.cfg_snapshot();
            let listener = gateway.listener_snapshot().ok_or("LLM 网关服务未开启")?;
            let ticket = gateway
                .personal_tests
                .issue(owner, &input, trace, gateway, progress.clone())
                .await?;
            send_personal(&cfg, listener.bound_addr, input, trace, ticket)
                .await
                .map_err(|e| e.message)
        })
        .await
        .map_err(|message| PersonalFailure {
            message,
            outcome: progress.outcome().into(),
        })
}

async fn send_inner(
    cfg: &AppConfig,
    listener: std::net::SocketAddr,
    input: ChatInput,
    trace: Option<uuid::Uuid>,
    personal_ticket: Option<&str>,
) -> Result<ChatResult, String> {
    validate(&input)?;
    // Never send the unified key through a system/VPN proxy or to a caller-supplied URL.
    let host = if listener.is_ipv6() {
        "[::1]"
    } else {
        "127.0.0.1"
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(request_timeout(cfg, &input.model)?)
        .build()
        .map_err(|_| "无法初始化聊天连接".to_string())?;
    let started = Instant::now();
    let request = client
        .post(format!(
            "http://{host}:{}/v1/chat/completions",
            listener.port()
        ))
        .bearer_auth(&cfg.unified_key)
        .header("x-session-id", &input.session_id)
        .header("x-llmgw-desktop-observation", "1")
        .header("x-client-name", "desktop-chat")
        .json(
            &serde_json::json!({ "model": input.model, "messages": input.messages,
            "max_tokens": input.max_tokens, "stream": false }),
        );
    let request = if let Some(trace) = trace {
        request.header("x-trace-id", trace.simple().to_string())
    } else {
        request
    };
    let request = if let Some(token) = personal_ticket {
        request.header(crate::personal_test::HEADER, token)
    } else {
        request
    };
    let mut response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            "聊天请求超时；请核对用量与审计，确认上游是否已生成回复后再重试".to_string()
        } else {
            "无法连接本机网关，请检查诊断页与监听状态".to_string()
        }
    })?;
    let status = response.status();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned)
    };
    let routed_via = header("x-routed-via");
    let session_id = header("x-session-id").unwrap_or(input.session_id);
    let trace_id = header("x-trace-id");
    let fallback_attempts = header("x-fallback-attempts")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
        .saturating_sub(1);
    let cached = header("x-cache").is_some_and(|s| s.eq_ignore_ascii_case("hit"));
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "聊天响应读取失败，请在审计页核对请求".to_string())?
    {
        if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
            return Err("聊天回复超过 8 MiB，请减少回复长度".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| format!("网关返回非 JSON 响应（HTTP {status}）"))?;
    if !status.is_success() {
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("请求失败");
        return Err(format!(
            "HTTP {status}：{}",
            message.chars().take(2000).collect::<String>()
        ));
    }
    let message = body
        .pointer("/choices/0/message")
        .ok_or_else(|| "网关响应缺少聊天消息".to_string())?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let reasoning = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let refusal = message
        .get("refusal")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned);
    if content.is_empty() && reasoning.is_none() && refusal.is_none() {
        return Err("模型未返回文本回复，请查看审计与供应商设置".into());
    }
    // Existing RUST_LOG debug level only. These are provider declarations and
    // channel-presence facts, never a verified terminal/settlement or a reason
    // to retry. Do not log response values, identities, model IDs or token counts.
    if tracing::enabled!(target: "llm_gateway::desktop_chat_reply", tracing::Level::DEBUG) {
        let declared_finish = match body.pointer("/choices/0/finish_reason") {
            None | Some(Value::Null) => "missing",
            Some(Value::String(value)) => match value.as_str() {
                "stop" => "stop",
                "length" => "length",
                "tool_calls" => "tool_calls",
                "function_call" => "function_call",
                "content_filter" => "content_filter",
                _ => "other",
            },
            Some(_) => "invalid",
        };
        tracing::debug!(target: "llm_gateway::desktop_chat_reply", parent: None,
            declared_finish,
            has_main_text = !content.is_empty(),
            has_reasoning = reasoning.is_some(),
            has_refusal = refusal.is_some(),
            "desktop_chat_reply_metadata_v1");
    }
    let (finish_reason, usage, response_metadata) = reply_observations(&body);
    Ok(ChatResult {
        personal_test_outcome: None,
        content,
        reasoning,
        refusal,
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("未知")
            .into(),
        routed_via,
        session_id,
        trace_id,
        fallback_attempts,
        latency_ms: started.elapsed().as_millis() as u64,
        usage,
        finish_reason,
        response_metadata: Some(response_metadata),
        cached,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoutingStrategy;

    #[test]
    fn actual_desktop_http_reply_metadata_is_closed_and_debug_level_controlled() {
        use axum::{routing::post, Json, Router};
        use std::{
            io::Write,
            sync::atomic::{AtomicUsize, Ordering},
        };
        #[derive(Clone)]
        struct Sink(Arc<parking_lot::Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (enabled, finish, class, main, reasoning, refusal) in [
            (
                true,
                Some(serde_json::json!("length")),
                "length",
                "PRIVATE_MAIN_CANARY",
                Some("PRIVATE_REASON_CANARY"),
                None,
            ),
            (
                true,
                Some(serde_json::json!("stop")),
                "stop",
                "PRIVATE_MAIN_CANARY",
                None,
                None,
            ),
            (
                true,
                Some(serde_json::json!("PRIVATE_FINISH_CANARY")),
                "other",
                "PRIVATE_MAIN_CANARY",
                None,
                None,
            ),
            (
                true,
                Some(serde_json::json!({"PRIVATE_FIELD_CANARY":"PRIVATE_VALUE_CANARY"})),
                "invalid",
                "PRIVATE_MAIN_CANARY",
                None,
                None,
            ),
            (true, None, "missing", "PRIVATE_MAIN_CANARY", None, None),
            (
                true,
                Some(serde_json::Value::Null),
                "missing",
                "PRIVATE_MAIN_CANARY",
                None,
                None,
            ),
            (
                true,
                Some(serde_json::json!("content_filter")),
                "content_filter",
                "",
                None,
                Some("PRIVATE_REFUSAL_CANARY"),
            ),
            (
                true,
                Some(serde_json::json!("stop")),
                "stop",
                "",
                Some("PRIVATE_REASON_CANARY"),
                None,
            ),
            (
                false,
                Some(serde_json::json!("length")),
                "length",
                "PRIVATE_MAIN_CANARY",
                None,
                None,
            ),
        ] {
            let bytes = Arc::new(parking_lot::Mutex::new(Vec::new()));
            let writer = Sink(bytes.clone());
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_env_filter(format!(
                    "off,llm_gateway::desktop_chat_reply={}",
                    if enabled { "debug" } else { "info" }
                ))
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                let parent = tracing::debug_span!(
                    target: "llm_gateway::desktop_chat_reply",
                    "owned_parent_context",
                    private = "PRIVATE_PARENT_CANARY"
                );
                let _entered = parent.enter();
                rt.block_on(async {
                let mut packet=serde_json::json!({"model":"PRIVATE_MODEL_CANARY","choices":[{"message":{"role":"assistant","content":main,"reasoning_content":reasoning,"refusal":refusal}}]});
                if let Some(finish)=finish {packet["choices"][0]["finish_reason"]=finish;}
                let hits=Arc::new(AtomicUsize::new(0));let seen=hits.clone();
                let app=Router::new().route("/v1/chat/completions",post(move || {let packet=packet.clone();let seen=seen.clone();async move {seen.fetch_add(1,Ordering::SeqCst);Json(packet)}}));
                let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let addr=listener.local_addr().unwrap();
                let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
                let result=send(&AppConfig::default(),addr,ChatInput {personal_test:false,model:"owned-model".into(),session_id:uuid::Uuid::new_v4().to_string(),max_tokens:128,messages:vec![ChatMessage{role:"user".into(),content:"owned diagnostic request".into(),reasoning_content:None}]}).await;
                server.abort();let _=server.await;
                let result=result.expect("positive HTTP decoder must succeed before log assertions");
                assert_eq!(hits.load(Ordering::SeqCst),1,"one explicit owned HTTP request, no replay");
                assert_eq!(result.content,main);assert_eq!(result.reasoning.as_deref(),reasoning);assert_eq!(result.refusal.as_deref(),refusal);
                assert!(result.usage.is_none(),"unknown usage must stay unknown");
                assert!(serde_json::to_value(result).unwrap().get("declared_finish").is_none(),"no new IPC/export contract");
            })
            });
            let log = String::from_utf8(bytes.lock().clone()).unwrap();
            for canary in [
                "PRIVATE_MAIN_CANARY",
                "PRIVATE_REASON_CANARY",
                "PRIVATE_REFUSAL_CANARY",
                "PRIVATE_MODEL_CANARY",
                "PRIVATE_FINISH_CANARY",
                "PRIVATE_FIELD_CANARY",
                "PRIVATE_VALUE_CANARY",
                "PRIVATE_PARENT_CANARY",
            ] {
                assert!(
                    !log.contains(canary),
                    "closed metadata must not expose values"
                );
            }
            assert_eq!(
                log.matches("desktop_chat_reply_metadata_v1").count(),
                usize::from(enabled)
            );
            if enabled {
                assert!(
                    log.contains(&format!("declared_finish=\"{class}\""))
                        || log.contains(&format!("declared_finish={class}")),
                    "fixed stop classification required"
                );
                assert!(log.contains(&format!("has_main_text={}", !main.is_empty())));
                assert!(log.contains(&format!("has_reasoning={}", reasoning.is_some())));
                assert!(log.contains(&format!("has_refusal={}", refusal.is_some())));
            }
        }
    }

    #[tokio::test]
    async fn controlled_cancel_before_first_poll_never_dispatches() {
        let controls = Arc::new(RequestControls::default());
        let id = uuid::Uuid::new_v4().to_string();
        let request = controls.register("main", &id).unwrap();
        assert!(!controls.cancel("another-window", &id).unwrap());
        assert!(controls.cancel("main", &id).unwrap());
        assert!(!controls.cancel("main", &id).unwrap());
        let polled = std::sync::atomic::AtomicBool::new(false);
        let result = request
            .run(async {
                polled.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .await;
        assert_eq!(result.unwrap_err(), CANCELLED);
        assert!(!polled.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!controls.cancel("main", &id).unwrap());
        assert!(controls.jobs.lock().is_empty());
    }

    #[tokio::test]
    async fn controlled_inflight_cancel_drops_only_target_and_keeps_other_request() {
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let controls = Arc::new(RequestControls::default());
        let id = uuid::Uuid::new_v4().to_string();
        let other_id = uuid::Uuid::new_v4().to_string();
        let request = controls.register("main", &id).unwrap();
        let other = controls.register("main", &other_id).unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let job = tokio::spawn({
            let entered = entered.clone();
            let dropped = dropped.clone();
            async move {
                request
                    .run(async move {
                        let _guard = Dropped(dropped);
                        entered.notify_one();
                        std::future::pending::<Result<(), String>>().await
                    })
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        assert!(controls.cancel("main", &id).unwrap());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), job)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err(),
            CANCELLED
        );
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            other
                .run(async { Ok::<_, String>("other success") })
                .await
                .unwrap(),
            "other success"
        );
        assert!(controls.jobs.lock().is_empty());
    }

    #[tokio::test]
    async fn controlled_known_completion_can_win_accepted_cancel_without_false_cancel() {
        let controls = Arc::new(RequestControls::default());
        let id = uuid::Uuid::new_v4().to_string();
        let request = controls.register("main", &id).unwrap();
        assert_eq!(request.ready().trace_id, request.id().simple().to_string());
        let result = request
            .run(async {
                assert!(controls.cancel("main", &id).unwrap());
                Ok::<_, String>("known success")
            })
            .await
            .unwrap();
        assert_eq!(result, "known success");
        assert!(!controls.cancel("main", &id).unwrap());
    }

    #[test]
    fn controlled_registration_rejects_duplicate_invalid_and_overflow_and_cleans_on_drop() {
        let controls = Arc::new(RequestControls::default());
        assert!(controls.register("main", "not-a-uuid").is_err());
        assert!(controls
            .register("", &uuid::Uuid::new_v4().to_string())
            .is_err());
        let id = uuid::Uuid::new_v4().to_string();
        let first = controls.register("main", &id).unwrap();
        assert!(controls.register("another-window", &id).is_err());
        let mut held = vec![first];
        for _ in 1..MAX_CONTROLLED_REQUESTS {
            held.push(
                controls
                    .register("main", &uuid::Uuid::new_v4().to_string())
                    .unwrap(),
            );
        }
        assert!(controls
            .register("main", &uuid::Uuid::new_v4().to_string())
            .is_err());
        drop(held);
        assert!(controls.jobs.lock().is_empty());
        assert!(controls.register("main", &id).is_ok());
    }

    #[test]
    fn timeout_covers_serial_fallback_and_two_compactions() {
        let mut cfg = AppConfig {
            upstream_timeout_secs: 100,
            max_fallback_attempts: 3,
            failover_enabled: true,
            ..Default::default()
        };
        // Three plugin attempts can each spend 100s writing and 100s reading.
        // Two compactions can each use two such attempts with a 60s phase budget.
        let with_fallback = request_timeout(&cfg, "auto").unwrap();
        assert!(with_fallback > Duration::from_secs(1080));
        cfg.failover_enabled = false;
        let without_fallback = request_timeout(&cfg, "auto").unwrap();
        assert!(without_fallback > Duration::from_secs(680));
        assert!(without_fallback < with_fallback);
    }

    #[test]
    fn timeout_covers_sanitized_cascade_and_shared_confirmation_attempts() {
        let mut cfg = AppConfig {
            upstream_timeout_secs: 10,
            max_fallback_attempts: 2,
            failover_enabled: true,
            routing_strategy: RoutingStrategy::Cascade,
            ..Default::default()
        };
        cfg.cascade.max_escalations = 3;
        cfg.smart_routing.jev.timeout_ms = 1000;
        let three_escalations = request_timeout(&cfg, "auto").unwrap();
        // Four rounds, two 20s plugin attempts per round, four 1s Jev decisions,
        // plus 480s for the two possible request-time compactions.
        assert!(three_escalations > Duration::from_secs(644));
        cfg.cascade.max_escalations = u8::MAX;
        assert_eq!(request_timeout(&cfg, "auto").unwrap(), three_escalations);
        cfg.auth_failure.confirm_retries = u32::MAX;
        assert_eq!(request_timeout(&cfg, "auto").unwrap(), three_escalations);
        cfg.cascade.max_escalations = 0;
        assert!(request_timeout(&cfg, "auto").unwrap() < three_escalations);
    }

    #[test]
    fn timeout_covers_enabled_preflight_and_preserves_disabled_mode() {
        let mut cfg = AppConfig {
            upstream_timeout_secs: 1,
            failover_enabled: false,
            routing_strategy: RoutingStrategy::Smart,
            ..Default::default()
        };
        cfg.smart_routing.enabled = false;
        cfg.smart_routing.timeout_ms = 1000;
        cfg.smart_routing.jev.auto_start.enabled = true;
        cfg.smart_routing.jev.auto_start.boot_wait_ms = 20_000;
        cfg.smart_routing.prompt_refine.enabled = true;
        cfg.smart_routing.prompt_refine.timeout_ms = u64::MAX;
        cfg.search.enabled = true;
        cfg.search.timeout_ms = u64::MAX;
        let disabled = request_timeout(&cfg, "auto").unwrap();
        cfg.smart_routing.enabled = true;
        let enabled = request_timeout(&cfg, "auto").unwrap();
        // 3s startup probe + 1s classification + 30s refinement + 60s search.
        assert!(enabled >= disabled + Duration::from_secs(94));
        cfg.routing_strategy = RoutingStrategy::Priority;
        assert_eq!(request_timeout(&cfg, "auto").unwrap(), disabled);
        assert!(request_timeout(&cfg, "smart").unwrap() >= enabled);
    }

    #[test]
    fn timeout_rejects_unrepresentable_deadlines_without_panicking() {
        let mut cfg = AppConfig {
            upstream_timeout_secs: u64::MAX,
            ..Default::default()
        };
        assert!(request_timeout(&cfg, "auto").is_err());
        cfg.upstream_timeout_secs = 600;
        cfg.max_fallback_attempts = usize::MAX;
        cfg.failover_enabled = true;
        assert!(request_timeout(&cfg, "auto").is_err());
    }
}

/// Optional true incremental delivery; buffered desktop consumers stay unchanged.
pub mod stream;
