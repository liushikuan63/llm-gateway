//! 降级执行链。
//!
//! 与「重试同一个上游」不同，本链路每次换的是 **provider + model** 组合，
//! 因此能真正跨厂商容错：DeepSeek 限流 → GLM 顶上 → Kimi 兜底。
//!
//! 关键细节：
//!  1) 只在**请求体未发出前**或**上游未产出任何字节**时才安全重试。
//!     流式响应已经吐了一半再换家，客户端会收到两截拼起来的回答 —— 必须避免。
//!  2) 429 与 401 要区别对待：429 进冷却（会自愈），401 标记 Invalid（要人来处理）。
//!  3) 每次降级都记进 X-Fallback-Attempts 响应头，排查时一眼看到底换了几家。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use serde::Serialize;

use crate::domain::{Provider, ProviderHealth};
use crate::error::{GatewayError, Result};
use crate::router::score::Candidate;

/// 一次降级尝试的结果
pub struct AttemptOutcome<T> {
    pub value: T,
    pub provider_id: String,
    pub model: String,
    pub attempts: usize,
    pub latency_ms: u64,
}

/// 逐跳降级明细。只记录排查所需的最小信息：谁被尝试、结果、耗时与归类原因。
/// 上游响应体不进入该结构，避免把可能回显请求内容的数据写进审计。
#[derive(Debug, Clone, Serialize)]
pub struct AttemptRecord {
    pub provider_id: String,
    pub provider: String,
    pub model: String,
    /// 上游 HTTP 状态码；网络层失败时为 `None`。
    pub status: Option<u16>,
    /// 失败归类（如 `rate_limited`）；成功时为 `None`。
    pub reason: Option<String>,
    pub latency_ms: u64,
    pub ok: bool,
    /// 该次失败是否允许继续降级；成功时为 `false`。
    pub retryable: bool,
}

const MAX_ATTEMPT_ERROR_CHARS: usize = 300;

impl AttemptRecord {
    pub fn failure(
        provider: &Provider,
        model: &str,
        error: &GatewayError,
        latency_ms: u64,
    ) -> Self {
        Self {
            provider_id: provider.id.clone(),
            provider: provider.name.clone(),
            model: model.to_owned(),
            status: match error {
                GatewayError::Upstream { status, .. } => Some(*status),
                _ => None,
            },
            reason: Some(truncate_reason(&error.to_string(), classify(error))),
            latency_ms,
            ok: false,
            retryable: error.retryable(),
        }
    }

    pub fn success(provider: &Provider, model: &str, latency_ms: u64) -> Self {
        Self {
            provider_id: provider.id.clone(),
            provider: provider.name.clone(),
            model: model.to_owned(),
            status: None,
            reason: None,
            latency_ms,
            ok: true,
            retryable: false,
        }
    }
}

/// 记录原因时保留归类前缀，便于 UI 同时展示「哪一类」与「具体是什么」。
fn truncate_reason(message: &str, kind: &str) -> String {
    let text = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return kind.to_string();
    }
    if text.chars().count() <= MAX_ATTEMPT_ERROR_CHARS {
        return text;
    }
    let head: String = text.chars().take(MAX_ATTEMPT_ERROR_CHARS).collect();
    format!("{head}…")
}

/// 执行上下文：每个候选尝试一次，返回 Err 则换下一个
pub struct FailoverChain<'a> {
    candidates: &'a [Candidate],
    max_attempts: usize,
    /// 是否已经吐出过字节（流式）。一旦为 true，后续失败不再换家。
    stream_started: &'a AtomicFlag,
}

/// 轻量标志位：流式写出前向代理层汇报
pub struct AtomicFlag(AtomicUsize);

impl AtomicFlag {
    pub fn new() -> Self {
        Self(AtomicUsize::new(0))
    }
    pub fn set(&self) {
        self.0.store(1, Ordering::SeqCst);
    }
    pub fn get(&self) -> bool {
        self.0.load(Ordering::SeqCst) == 1
    }
}

impl Default for AtomicFlag {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> FailoverChain<'a> {
    pub fn new(
        candidates: &'a [Candidate],
        max_attempts: usize,
        stream_started: &'a AtomicFlag,
    ) -> Self {
        Self {
            candidates,
            max_attempts,
            stream_started,
        }
    }

    /// 逐个候选尝试。`f` 拿到 (provider, model)，返回业务结果。
    ///
    /// `on_failure` 用于让上层记录健康度/冷却，保持本结构不依赖 HealthRegistry。
    /// `records` 由调用方提供，函数把每次尝试追加进去供审计，成功与失败都记录。
    pub async fn run<T, F, Fut, E>(
        &self,
        records: &mut Vec<AttemptRecord>,
        mut f: F,
        mut on_failure: E,
    ) -> Result<AttemptOutcome<T>>
    where
        F: FnMut(Provider, String) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
        E: FnMut(&Provider, &str, &GatewayError),
    {
        if self.candidates.is_empty() {
            return Err(GatewayError::ModelNotFound(
                "没有可用的 provider/model 候选".into(),
            ));
        }

        let limit = self.max_attempts.min(self.candidates.len());
        let mut attempts = 0usize;
        let mut last_err: Option<GatewayError> = None;

        for c in self.candidates.iter().take(limit) {
            attempts += 1;
            let started = Instant::now();

            match f(c.provider.clone(), c.model.upstream.clone()).await {
                Ok(v) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    records.push(AttemptRecord::success(
                        &c.provider,
                        &c.model.upstream,
                        latency_ms,
                    ));
                    return Ok(AttemptOutcome {
                        value: v,
                        provider_id: c.provider.id.clone(),
                        model: c.model.upstream.clone(),
                        attempts,
                        latency_ms,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "尝试 {}/{} 失败 [{} / {}]: {}",
                        attempts,
                        limit,
                        c.provider.name,
                        c.model.upstream,
                        e
                    );
                    records.push(AttemptRecord::failure(
                        &c.provider,
                        &c.model.upstream,
                        &e,
                        started.elapsed().as_millis() as u64,
                    ));
                    on_failure(&c.provider, &c.model.upstream, &e);

                    // 已经吐出字节的流式请求，不能再换家
                    if self.stream_started.get() {
                        return Err(e);
                    }
                    if !e.retryable() {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or(GatewayError::AllProvidersFailed { attempts }))
    }
}

/// 把一次失败归类成人类可读的原因，写进请求日志的 error 字段
pub fn classify(err: &GatewayError) -> &'static str {
    match err {
        GatewayError::Upstream { status, .. } => match *status {
            401 | 403 => "auth_failed",
            408 => "timeout",
            429 => "rate_limited",
            500..=599 => "upstream_5xx",
            _ => "upstream_error",
        },
        GatewayError::Timeout(_) => "timeout",
        GatewayError::Protocol(_) => "protocol",
        GatewayError::Unauthorized(_) => "bad_gateway_key",
        _ => "unknown",
    }
}

/// 是否应该把该 provider 直接标记为 Invalid（需要用户换 Key），而非临时冷却
pub fn is_fatal_auth(err: &GatewayError) -> bool {
    matches!(
        err,
        GatewayError::Upstream { status: 401, .. } | GatewayError::Upstream { status: 403, .. }
    )
}

/// 供 UI 展示：把候选链的排序结果渲染成一行文字
pub fn describe_chain(
    candidates: &[Candidate],
    health: &dyn Fn(&str, &str) -> Option<ProviderHealth>,
) -> String {
    candidates
        .iter()
        .take(5)
        .map(|c| {
            let h = health(&c.provider.id, &c.model.upstream)
                .map(|x| format!("{:?}", x.health))
                .unwrap_or_else(|| "Unknown".into());
            format!("{}:{} ({})", c.provider.name, c.model.upstream, h)
        })
        .collect::<Vec<_>>()
        .join(" → ")
}
