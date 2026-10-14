//! Desktop chat uses the authenticated gateway listener, including routing, budgets and audit.
use crate::config::AppConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatInput {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub session_id: String,
    pub max_tokens: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatResult {
    pub content: String,
    pub reasoning: Option<String>,
    pub model: String,
    pub routed_via: Option<String>,
    pub session_id: String,
    pub trace_id: Option<String>,
    pub fallback_attempts: u32,
    pub latency_ms: u64,
    pub usage: Option<Value>,
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
            .map(|m| m.content.len())
            .sum::<usize>()
            > 262_144
        || input
            .messages
            .last()
            .map_or(true, |m| m.role != "user" || m.content.trim().is_empty())
        || input
            .messages
            .iter()
            .any(|m| !matches!(m.role.as_str(), "system" | "user" | "assistant"))
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
    let mut response = client
        .post(format!(
            "http://{host}:{}/v1/chat/completions",
            listener.port()
        ))
        .bearer_auth(&cfg.unified_key)
        .header("x-session-id", &input.session_id)
        .header("x-client-name", "desktop-chat")
        .json(
            &serde_json::json!({ "model": input.model, "messages": input.messages,
            "max_tokens": input.max_tokens, "stream": false }),
        )
        .send()
        .await
        .map_err(|e| {
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
    if content.is_empty() && reasoning.is_none() {
        return Err("模型未返回文本回复，请查看审计与供应商设置".into());
    }
    Ok(ChatResult {
        content,
        reasoning,
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
        usage: body.get("usage").filter(|v| !v.is_null()).cloned(),
        cached,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoutingStrategy;

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
