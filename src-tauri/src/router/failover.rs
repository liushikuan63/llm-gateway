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

use crate::config::AuthFailureMode;
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
    /// 任务卡二 A5/A6：这一跳走的是账号型上游时，它的运行时种类
    /// （`codex` / `qoder`…）。`None` = 普通 API 上游。
    ///
    /// 【为什么放在这里而不是给 `requests` 表加列】
    /// 卡片 A6 判据 1 要求审计能看出「这次走的是账号型上游」，
    /// 而逐跳明细经 `attempts_json` 列**已经**进审计了。
    /// 加一列要改 11 处 `RequestLog` 字面量（我连试两次都失败了，
    /// 失败轨迹见 `docs/D批接续-交接单.md`）；
    /// 而这里只需改 `AttemptRecord` 自己的构造点，**编译器会列全**。
    ///
    /// 更重要的是**粒度更对**：一次请求可能先撞账号型、再降级到 API 型，
    /// 「这次是什么上游」在请求级别本来就说不清 —— 逐跳才说得清。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_kind: Option<String>,
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
            // 账号型上游的一跳：记下它的运行时种类。
            // 从 `provider.runtime_id` 取 —— 那正是分派点用来选路的同一个值，
            // 不另存一份状态（两份状态迟早不一致）。
            runtime_kind: provider.runtime_id.clone(),
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
            runtime_kind: provider.runtime_id.clone(),
        }
    }

    /// 复测**成功**的记录。必须有标记：否则审计里只剩「一次失败 + 一次成功」，
    /// 看不出这次成功是复测得来的——而这正是「不凭一次失败就判死」的全部证据。
    pub fn confirm_success(provider: &Provider, model: &str, latency_ms: u64) -> Self {
        let mut record = Self::success(provider, model, latency_ms);
        record.reason = Some("确认复测：本次成功，此前那次鉴权失败判定为暂态".into());
        record
    }

    /// 复测记录。`reason` 带 `确认复测：` 前缀——排查时必须一眼看出
    /// 「这次失败是判定前的复测」，否则会误以为是两次独立的业务失败。
    pub fn confirm_failure(
        provider: &Provider,
        model: &str,
        error: &GatewayError,
        latency_ms: u64,
    ) -> Self {
        let mut record = Self::failure(provider, model, error, latency_ms);
        record.reason = Some(format!(
            "确认复测：{}",
            record.reason.unwrap_or_else(|| "unknown".into())
        ));
        record
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
    /// 上游鉴权失败的处理策略。默认 `Strict`，即本文件引入该字段之前的行为。
    auth_mode: AuthFailureMode,
    /// 判定「真实不可用」之前的独立复测次数。
    auth_confirm_retries: u32,
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
            auth_mode: AuthFailureMode::Strict,
            auth_confirm_retries: 0,
        }
    }

    /// 套用鉴权失败策略。**不改这里的调用方保持旧行为**，因为默认档位是 `Strict`。
    pub fn with_auth_policy(mut self, mode: AuthFailureMode, confirm_retries: u32) -> Self {
        self.auth_mode = mode;
        self.auth_confirm_retries = confirm_retries.min(3);
        self
    }

    /// 逐个候选尝试。`f` 拿到 (provider, model)，返回业务结果。
    ///
    /// `on_failure` 用于让上层记录健康度/冷却，保持本结构不依赖 HealthRegistry。
    /// `records` 由调用方提供，函数把每次尝试追加进去供审计，成功与失败都记录。
    pub async fn run<T, F, Fut, E>(
        &self,
        records: &mut Vec<AttemptRecord>,
        f: F,
        on_failure: E,
    ) -> Result<AttemptOutcome<T>>
    where
        F: FnMut(Provider, String) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
        E: FnMut(&Provider, &str, &GatewayError),
    {
        // 默认策略是 `Strict`，因此这里等价于改动前的行为；既有调用方无需改动。
        self.run_with_auth_policy(records, f, on_failure, |_provider, _error| {})
            .await
    }

    /// 同 [`Self::run`]，但额外拿到「鉴权失败已复测确认」这个时机，
    /// 让上层按策略把该供应商自动停用。
    pub async fn run_with_auth_policy<T, F, Fut, E, C>(
        &self,
        records: &mut Vec<AttemptRecord>,
        mut f: F,
        mut on_failure: E,
        mut on_auth_confirmed: C,
    ) -> Result<AttemptOutcome<T>>
    where
        F: FnMut(Provider, String) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
        E: FnMut(&Provider, &str, &GatewayError),
        C: FnMut(&Provider, &GatewayError),
    {
        if self.candidates.is_empty() {
            return Err(GatewayError::ModelNotFound(
                "没有可用的 provider/model 候选".into(),
            ));
        }

        let limit = self.max_attempts.min(self.candidates.len());
        let mut attempts = 0usize;
        let mut last_err: Option<GatewayError> = None;
        // 一旦发生过鉴权失败，后续候选**不得落到免 Key 后端**（本地 Ollama、
        // 无鉴权的自建服务）。硬约束「401 不回落」：不能因为上游拒了 Key 就
        // 悄悄换一家不需要凭据的服务——那既可能泄露上下文，也让错误更难追。
        let mut no_keyless_fallback = false;

        for c in self.candidates.iter().take(limit) {
            if no_keyless_fallback && c.provider.api_key_enc.trim().is_empty() {
                tracing::warn!(
                    "鉴权失败后跳过免 Key 后端 [{} / {}]",
                    c.provider.name,
                    c.model.upstream
                );
                continue;
            }
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
                        // 上游鉴权失败：先确认「真实不可用」，再决定要不要放弃这家。
                        // 旧行为（Strict）是一次 401/403 立刻终止整条链，
                        // 于是一家中转站被拒就能让「自动分流」整体失败。
                        if !is_fatal_auth(&e) || self.auth_mode == AuthFailureMode::Strict {
                            return Err(e);
                        }
                        match self
                            .confirm_auth_failure(c, &mut f, records, &mut attempts)
                            .await
                        {
                            ConfirmOutcome::Recovered(v) => {
                                let latency_ms = started.elapsed().as_millis() as u64;
                                return Ok(AttemptOutcome {
                                    value: v,
                                    provider_id: c.provider.id.clone(),
                                    model: c.model.upstream.clone(),
                                    attempts,
                                    latency_ms,
                                });
                            }
                            ConfirmOutcome::StillRefused => {
                                // 已确认不可用：调用方的 on_failure 按策略自动停用
                                // 这家供应商，这里换下一家继续。
                                // 已确认不可用：上层按策略把这家供应商自动停用，这里换下一家继续。
                                no_keyless_fallback = true;
                                on_auth_confirmed(&c.provider, &e);
                                last_err = Some(e);
                                continue;
                            }
                            ConfirmOutcome::Undecidable(seen) => {
                                last_err = Some(seen.unwrap_or(e));
                                continue;
                            }
                        }
                    }
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or(GatewayError::AllProvidersFailed { attempts }))
    }

    /// 鉴权失败的确认复测。
    ///
    /// 为什么要有这一步：中转站的 401 未必等于「Key 永久失效」——可能是某个
    /// 路径被风控、或瞬时拒流。不复测就判死，会把**仍然可用**的供应商踢出路由；
    /// 而复测通过时直接把它当成功结果用，等于承认「这次能用」。
    ///
    /// 复测复用同一个闭包 `f`，因此发的是**同一个请求**，不是另造探针：
    /// 探针成功只能证明「探针能过」，证明不了「这个请求能过」。
    async fn confirm_auth_failure<T, F, Fut>(
        &self,
        c: &Candidate,
        f: &mut F,
        records: &mut Vec<AttemptRecord>,
        attempts: &mut usize,
    ) -> ConfirmOutcome<T>
    where
        F: FnMut(Provider, String) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        for _ in 0..self.auth_confirm_retries {
            if *attempts >= self.max_attempts {
                return ConfirmOutcome::Undecidable(None);
            }
            *attempts += 1;
            let started = Instant::now();
            match f(c.provider.clone(), c.model.upstream.clone()).await {
                Ok(v) => {
                    records.push(AttemptRecord::confirm_success(
                        &c.provider,
                        &c.model.upstream,
                        started.elapsed().as_millis() as u64,
                    ));
                    return ConfirmOutcome::Recovered(v);
                }
                Err(e) => {
                    let fatal = is_fatal_auth(&e);
                    records.push(AttemptRecord::confirm_failure(
                        &c.provider,
                        &c.model.upstream,
                        &e,
                        started.elapsed().as_millis() as u64,
                    ));
                    if !fatal {
                        return ConfirmOutcome::Undecidable(Some(e));
                    }
                }
            }
        }
        ConfirmOutcome::StillRefused
    }
}

enum ConfirmOutcome<T> {
    /// 复测通过：这家的模型其实能用，直接用它。
    Recovered(T),
    /// 复测仍然被拒：已确认真实不可用（原始错误由调用方保留）。
    StillRefused,
    /// 复测给出的是另一种错误（限流等），不足以判定凭据失效。
    Undecidable(Option<GatewayError>),
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
