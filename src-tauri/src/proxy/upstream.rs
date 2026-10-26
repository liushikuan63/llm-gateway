//! 上游转发器。网关里唯一真正「出网」的地方。
//!
//! 设计要点：
//!  1) 单一 reqwest::Client 复用连接池；超时按 provider 可配（免费层普遍慢）。
//!  2) 流式与非流式走同一套适配器，但**流式解析成统一事件流**，
//!     出口再按客户端方言重新编码 —— 这样「入 OpenAI / 出 Anthropic」这种
//!     跨方言组合才是真转换，而不是把上游的 SSE 原样透传。
//!  3) 上游返回的 4xx/5xx 原样带上 body，方便上层判定 429 还是 401。

use axum::body::Bytes;
use futures_util::Stream;
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Url;
use std::net::{IpAddr, Ipv6Addr};
use std::pin::Pin;
use std::time::Duration;

use super::timing::{
    observation::{HttpObservationGuard, ObservedStream},
    TimingMode,
};
use crate::crypto;
use crate::domain::{ChatRequest, ChatResponse, Dialect, ModelOverrides, Provider, Usage};
use crate::error::{GatewayError, Result};

// Owned Windows TLS fixture trust is task-scoped and absent from product builds.
// It extends the normal factory's trust roots; no certificate/hostname checks
// are disabled, and no machine trust store or global client is changed.
#[cfg(all(test, windows))]
tokio::task_local! {static OWNED_NETWORK_TEST_ROOT: reqwest::Certificate;}
#[cfg(all(test, windows))]
tokio::task_local! {static OWNED_NETWORK_TEST_RESOLVE: (String, std::net::SocketAddr);}
#[cfg(all(test, windows))]
#[path = "upstream/network_tests.rs"]
mod network_tests;

// reqwest 0.12.28 has no resolved charset feature: text() uses UTF-8
// replacement on its decoded body. Retain that decoding and error behavior.
async fn observed_text(
    response: reqwest::Response,
    observation: Option<&HttpObservationGuard>,
) -> std::result::Result<String, reqwest::Error> {
    use futures_util::StreamExt;
    let Some(observation) = observation else {
        return response.text().await;
    };
    let observer = observation.body_observer();
    let mut chunks = response.bytes_stream();
    let mut full = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        observer.received(chunk.len());
        full.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&full).into_owned())
}

/// Standard HTTP uses the exact failover candidate. Direct callers without a
/// dispatch scope retain their legacy first-match lookup. A selected None is
/// authoritative: never borrow a sibling alias's body, headers or capabilities.
pub(crate) fn selected_model(
    model: &str,
    p: &Provider,
) -> Option<std::sync::Arc<crate::domain::ModelRef>> {
    super::dispatch_target::selected(&p.id, model).or_else(|| {
        p.models
            .iter()
            .find(|candidate| candidate.upstream == model)
            .cloned()
            .map(std::sync::Arc::new)
    })
}
fn overrides(model: &str, p: &Provider) -> Option<ModelOverrides> {
    selected_model(model, p)
        .and_then(|candidate| candidate.overrides.clone())
        .filter(|overrides| !overrides.is_empty())
}

fn apply_thinking_capability(body: &mut serde_json::Value, model: &str, p: &Provider) {
    if !selected_model(model, p).is_some_and(|model| model.supports_thinking) {
        crate::protocol::convert::strip_thinking(body);
    }
}

/// Private transport policy; never serialized into ChatRequest or provider extras.
#[derive(Clone, Copy, Default)]
pub(crate) struct RequestWireOptions {
    pub entitlement_purpose: &'static str,
    pub preserve_instruction: bool,
    pub cost_policy: crate::cost_policy::RequestCostPolicy,
    pub normalized_ingress: Option<super::control_modes::ControlSet>,
    pub caller: Option<super::control_modes::CallerControls>,
}

/// Response decoding and inherited cost policy stay together at passthrough's
/// private wire boundary; neither option can be supplied through provider JSON.
#[derive(Clone, Copy)]
pub(crate) struct PassthroughWireOptions {
    pub expect_json: bool,
    pub cost_policy: crate::cost_policy::RequestCostPolicy,
}

const INSTRUCTION_OVERRIDE_CONFLICT: &str =
    "extra_body.instructions conflicts with current request instructions";

fn instruction_override_conflict() -> GatewayError {
    GatewayError::CapabilityUnavailable {
        kind: INSTRUCTION_OVERRIDE_CONFLICT.into(),
    }
}

/// This is a local request-policy rejection, not an upstream health failure.
/// Require both private strict provenance and the exact typed error identity.
pub(crate) fn is_instruction_override_conflict(
    error: &GatewayError,
    options: RequestWireOptions,
) -> bool {
    options.preserve_instruction
        && matches!(error, GatewayError::CapabilityUnavailable { kind } if kind == INSTRUCTION_OVERRIDE_CONFLICT)
}

pub(crate) fn is_local_request_policy_rejection(
    error: &GatewayError,
    options: RequestWireOptions,
) -> bool {
    matches!(error, GatewayError::RequestControlUnavailable { .. })
        || is_instruction_override_conflict(error, options)
}

/// The actual Responses HTTP transport can replace the encoded
/// instruction through this model override. Use the same effective override
/// lookup as serialization/application, including duplicate upstream mappings.
/// Key presence is a conflict even for null, blank or identical configured text.
pub(crate) fn validate_instruction_override(
    provider: &Provider,
    model: &str,
    options: RequestWireOptions,
) -> Result<()> {
    validate_model_instruction_override(
        provider.dialect,
        overrides(model, provider).as_ref(),
        options,
    )
}

/// Shared pure validation for the pre-dispatch exact candidate and actual wire.
pub(crate) fn validate_model_instruction_override(
    dialect: Dialect,
    overrides: Option<&ModelOverrides>,
    options: RequestWireOptions,
) -> Result<()> {
    if options.preserve_instruction
        && dialect == Dialect::Responses
        && overrides
            .and_then(|value| value.extra_body.as_ref())
            .and_then(|value| {
                value
                    .as_object()
                    .map(|object| object.contains_key("instructions"))
            })
            .unwrap_or(false)
    {
        return Err(instruction_override_conflict());
    }
    Ok(())
}

pub struct UpstreamClient {
    http: parking_lot::RwLock<std::result::Result<reqwest::Client, String>>,
    cost_gate: parking_lot::RwLock<Option<std::sync::Arc<crate::cost_policy::CostGate>>>,
}

pub enum PassthroughResponse {
    Json(serde_json::Value),
    Bytes {
        body: Bytes,
        content_type: Option<String>,
    },
}

impl Default for UpstreamClient {
    fn default() -> Self {
        Self::new()
    }
}

impl UpstreamClient {
    pub fn new() -> Self {
        Self {
            http: parking_lot::RwLock::new(Self::build_http(None)),
            cost_gate: parking_lot::RwLock::new(None),
        }
    }

    pub(crate) fn set_cost_gate(&self, gate: std::sync::Arc<crate::cost_policy::CostGate>) {
        *self.cost_gate.write() = Some(gate);
    }

    async fn authorize_wire(
        &self,
        p: &Provider,
        model: &str,
        body: &serde_json::Value,
        policy: crate::cost_policy::RequestCostPolicy,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>> {
        let gate = { self.cost_gate.read().clone() };
        match gate {
            Some(gate) => gate.before_wire(p, model, body, policy).await,
            None if policy.is_free() => Err(crate::cost_policy::rejected("free_guard_unavailable")),
            None => Ok(None),
        }
    }

    async fn refresh_budget_admission(
        &self,
        p: &Provider,
        model: &str,
        body: &serde_json::Value,
        policy: crate::cost_policy::RequestCostPolicy,
        budget: &Option<crate::budget::holds::Permit>,
        free_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>> {
        let Some(budget) = budget else {
            return Ok(free_permit);
        };
        // Budget persistence added an await after the original free admission.
        // Release that semaphore before reusing the authoritative gate; do not
        // acquire two permits or turn FreeOnly into Unrestricted.
        drop(free_permit);
        let permit = match self.authorize_wire(p, model, body, policy).await {
            Ok(permit) => permit,
            Err(error) => {
                budget.release_known_not_sent().await?;
                return Err(error);
            }
        };
        if let Err(error) = budget.check_final(p, policy, permit.is_some()) {
            budget.release_known_not_sent().await?;
            return Err(error);
        }
        Ok(permit)
    }

    fn build_http(proxy: Option<&str>) -> std::result::Result<reqwest::Client, String> {
        let builder = reqwest::Client::builder()
            .pool_max_idle_per_host(16)
            .connect_timeout(Duration::from_secs(10))
            // 总超时在每次请求上按 provider 覆盖
            .timeout(Duration::from_secs(300))
            // Provider URL 是本地管理员可配的输入。禁止自动跟随 3xx，避免经过
            // 未校验的 Location 跳转到其他网络目标。
            .redirect(reqwest::redirect::Policy::none())
            .gzip(true)
            .brotli(true);
        let builder = crate::outbound::apply_proxy(builder, proxy)?;
        #[cfg(all(test, windows))]
        let builder = match OWNED_NETWORK_TEST_ROOT.try_with(Clone::clone) {
            Ok(root) => builder.add_root_certificate(root),
            Err(_) => builder,
        };
        #[cfg(all(test, windows))]
        let builder = match OWNED_NETWORK_TEST_RESOLVE.try_with(Clone::clone) {
            Ok((host, addr)) => builder.resolve(&host, addr),
            Err(_) => builder,
        };
        builder
            .build()
            .map_err(|_| "无法创建上游代理连接".to_string())
    }

    /// Hot reload the client pool. Existing requests keep their own client clone.
    pub fn set_proxy(&self, proxy: Option<&str>) {
        *self.http.write() = Self::build_http(proxy);
    }

    fn client(&self) -> Result<reqwest::Client> {
        self.http.read().clone().map_err(GatewayError::Protocol)
    }

    fn headers_for(p: &Provider, api_key: &str) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, "application/json".parse().unwrap());
        if api_key.is_empty() {
            return Ok(h);
        }
        match p.dialect {
            Dialect::OpenAI | Dialect::Ollama | Dialect::Responses => {
                h.insert(
                    AUTHORIZATION,
                    format!("Bearer {api_key}")
                        .parse()
                        .map_err(|_| GatewayError::Protocol("bad api key header".into()))?,
                );
            }
            Dialect::Anthropic => {
                h.insert(
                    "x-api-key",
                    api_key
                        .parse()
                        .map_err(|_| GatewayError::Protocol("bad api key header".into()))?,
                );
                h.insert("anthropic-version", "2023-06-01".parse().unwrap());
            }
            Dialect::Gemini => {
                // Gemini 用 ?key= 查询参数，见下方 url()
            }
        }
        Ok(h)
    }

    /// 在鉴权头之后叠加模型级额外请求头。受保护的头在保存期已被拒绝，
    /// 这里再校验一次解析结果，坏值只影响该请求而不会 panic。
    fn apply_extra_headers(h: &mut HeaderMap, overrides: Option<&ModelOverrides>) -> Result<()> {
        let Some(headers) = overrides.and_then(|overrides| overrides.extra_headers.as_ref()) else {
            return Ok(());
        };
        for header in headers {
            let name = reqwest::header::HeaderName::from_bytes(header.name.trim().as_bytes())
                .map_err(|_| GatewayError::Protocol("bad extra header name".into()))?;
            let value = reqwest::header::HeaderValue::from_str(&header.value)
                .map_err(|_| GatewayError::Protocol("bad extra header value".into()))?;
            h.insert(name, value);
        }
        Ok(())
    }

    /// 把上游的非成功响应归类成错误。三条调用路径（`call` / `call_passthrough`
    /// / `call_stream`）共用它，避免某一路径漏掉判断。
    ///
    /// 上下文超限必须单独识别成 [`GatewayError::ContextLengthExceeded`]：
    /// 它在 `error.rs` 的 `retryable()` 里返回 **true**，因为候选链里往往还有
    /// 窗口更大的模型。漏掉这一步，400 会被当成不可重试的客户端错误，
    /// 整条候选链在第一个候选上就死掉，用户看到的现象是「自动切换完全没生效」。
    ///
    /// 2026-10-05 实测：`call` 与 `call_stream` 两条路径都缺这段判断，只有
    /// `call_passthrough` 有。DSH 走 `call_stream`，因此 263152 token 的请求
    /// 打到上限 256000 的 free 模型后**整轮失败**，而候选链里明明还有
    /// 922000 窗口的 gpt-6-astra 能接住。
    fn classify_upstream_error(p: &Provider, model: &str, status: u16, text: &str) -> GatewayError {
        if looks_like_context_length_error(text) {
            let (required, available) = parse_context_length_error(text).unwrap_or((0, 0));
            return GatewayError::ContextLengthExceeded {
                required,
                available,
            };
        }
        GatewayError::Upstream {
            provider: p.name.clone(),
            model: model.to_string(),
            status,
            body: truncate(text, 800),
        }
    }

    /// 规范化参数已在 build_body 中按候选应用；这里仅合并方言专属额外字段。
    /// 额外请求体在保存期已校验为对象且不含受保护键，这里的合并是浅合并。
    fn apply_overrides(body: &mut serde_json::Value, overrides: Option<&ModelOverrides>) {
        let Some(overrides) = overrides else {
            return;
        };
        if let (Some(serde_json::Value::Object(extra)), Some(target)) =
            (&overrides.extra_body, body.as_object_mut())
        {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
    }

    fn build_url(p: &Provider, model: &str, stream: bool) -> Result<Url> {
        if let Some(path) = custom_upstream_path(p, model)? {
            let mut url = build_url_from_path(p, &path, model)?;
            if p.dialect == Dialect::Gemini && stream {
                url.query_pairs_mut().append_pair("alt", "sse");
            }
            return Ok(url);
        }
        let mut url = validate_upstream_base_url(&p.base_url)?;
        match p.dialect {
            Dialect::OpenAI => append_url_path(&mut url, &["chat", "completions"])?,
            Dialect::Anthropic => append_url_path(&mut url, &["messages"])?,
            Dialect::Gemini => {
                let action = if stream {
                    "streamGenerateContent"
                } else {
                    "generateContent"
                };
                let model_action = format!("{model}:{action}");
                append_url_path(&mut url, &["models", &model_action])?;
                if stream {
                    url.query_pairs_mut().append_pair("alt", "sse");
                }
            }
            Dialect::Ollama => append_url_path(&mut url, &["api", "chat"])?,
            Dialect::Responses => append_url_path(&mut url, &["responses"])?,
        }
        Ok(url)
    }

    /// 组装上游请求体（按方言）
    ///
    /// `defaults` 是网关侧要注入的方言专属旋钮（目前只有 Ollama 的
    /// `num_ctx`）。**必须由网关注入而不是靠客户端传**：`num_ctx` 是
    /// Ollama 专属字段，Anthropic / Responses 协议里没有地方放它，
    /// Claude Code 与 Codex CLI 不可能知道要发。缺了它的症状是
    /// 「回答到一半被截断且看不出原因」——Ollama 默认 num_ctx 只有 4096，
    /// prompt 与输出共用这一份，推理模型的 thinking 吃光预算后正文为 0。
    pub fn build_body(
        p: &Provider,
        req: &ChatRequest,
        model: &str,
        defaults: &crate::config::OllamaOptionsConfig,
    ) -> serde_json::Value {
        Self::build_body_with_options(p, req, model, defaults, RequestWireOptions::default())
    }

    fn build_body_with_options(
        p: &Provider,
        req: &ChatRequest,
        model: &str,
        defaults: &crate::config::OllamaOptionsConfig,
        options: RequestWireOptions,
    ) -> serde_json::Value {
        // Apply canonical overrides before converting: each dialect stores these
        // fields in a different location. The shared failover request stays intact.
        let mut effective;
        let req = if let Some(options) = overrides(model, p)
            .filter(|options| options.temperature.is_some() || options.max_tokens.is_some())
        {
            effective = req.clone();
            if let Some(temperature) = options.temperature {
                effective.temperature = Some(temperature);
            }
            if let Some(max_tokens) = options.max_tokens {
                effective.max_tokens = Some(max_tokens);
            }
            &effective
        } else {
            req
        };
        match p.dialect {
            Dialect::OpenAI => crate::protocol::openai::to_upstream_body(req, model),
            Dialect::Anthropic => {
                crate::protocol::anthropic::internal_to_anthropic_body(req, model)
            }
            Dialect::Gemini => crate::protocol::gemini::to_gemini_body(req),
            Dialect::Ollama => crate::protocol::ollama::to_ollama_body(req, model, Some(defaults)),
            Dialect::Responses => crate::protocol::responses::to_responses_body_with_instruction(
                req,
                model,
                options.preserve_instruction,
            ),
        }
    }

    /// 非流式请求
    pub async fn call(
        &self,
        p: &Provider,
        req: &ChatRequest,
        model: &str,
        timeout: Duration,
        defaults: &crate::config::OllamaOptionsConfig,
    ) -> Result<ChatResponse> {
        self.call_with_options(
            p,
            req,
            model,
            timeout,
            defaults,
            RequestWireOptions::default(),
        )
        .await
    }

    pub(crate) async fn call_with_options(
        &self,
        p: &Provider,
        req: &ChatRequest,
        model: &str,
        timeout: Duration,
        defaults: &crate::config::OllamaOptionsConfig,
        options: RequestWireOptions,
    ) -> Result<ChatResponse> {
        let mut observation = None;
        let result: Result<ChatResponse> = async {
            validate_instruction_override(p, model, options)?;
            crate::protocol::reasoning::validate(
                req,
                p.dialect,
                selected_model(model, p).is_some_and(|m| m.supports_thinking),
            )?;
            let url = Self::build_url(p, model, false)?;
            let key = decrypt_provider_key(p)?;
            let mut body = Self::build_body_with_options(p, req, model, defaults, options);
            if p.dialect != Dialect::Gemini && body.get("stream").is_none() {
                body["stream"] = serde_json::Value::Bool(false);
            }
            Self::apply_overrides(&mut body, overrides(model, p).as_ref());
            // Unknown/false remains unsupported even after explicit extra-body merging.
            apply_thinking_capability(&mut body, model, p);

            let url = with_gemini_key(url, p.dialect, &key)?;

            let mut headers = Self::headers_for(p, &key)?;
            Self::apply_extra_headers(&mut headers, overrides(model, p).as_ref())?;

            let _free_permit = self
                .authorize_wire(p, model, &body, options.cost_policy)
                .await?;
            let observation_context = super::observation_context::Snapshot::capture(p, &url, &body);
            let outgoing = self
                .client()?
                .post(url)
                .timeout(timeout)
                .headers(headers)
                .json(&body);
            let budget = crate::budget::holds::before_http(p, model, &body).await?;
            let _free_permit = self
                .refresh_budget_admission(
                    p,
                    model,
                    &body,
                    options.cost_policy,
                    &budget,
                    _free_permit,
                )
                .await?;
            observation = HttpObservationGuard::begin_with_projection(
                TimingMode::Buffered,
                Some(super::control_modes::WireModeSnapshot::observe(
                    p.dialect,
                    selected_model(model, p).map(|m| m.supports_thinking),
                    options.caller,
                    options.normalized_ingress,
                    req,
                    &body,
                )),
                observation_context,
            );
            let resp = outgoing
                .send()
                .await
                .map_err(|e| map_reqwest_err(&p.name, model, e))?;
            if let Some(observation) = &observation {
                observation.headers();
            }

            let status = resp.status();
            let text = match observed_text(resp, observation.as_ref()).await {
                Ok(text) => text,
                // A successful response whose body hits the deadline is a
                // transport timeout, not empty/invalid JSON. A known HTTP error
                // remains authoritative even if reading its diagnostic fails.
                Err(error) if status.is_success() && error.is_timeout() => {
                    return Err(map_reqwest_err(&p.name, model, error));
                }
                // Preserve the prior bad-body/HTTP-error classification for
                // other read failures; do not introduce new replay eligibility.
                Err(_) => String::new(),
            };

            if !status.is_success() {
                return Err(Self::classify_upstream_error(
                    p,
                    model,
                    status.as_u16(),
                    &text,
                ));
            }

            // HTTP 200 并不等于上游真的成功：反向代理、WAF 和过载节点常会返回
            // HTML 或截断 JSON。它属于可换 Provider 的上游坏响应，而不是客户端
            // 请求的协议错误，因此映射为可重试的 502。
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|_| GatewayError::Upstream {
                    provider: p.name.clone(),
                    model: model.into(),
                    status: 502,
                    body: truncate(&text, 800),
                })?;
            ensure_successful_response(&v, p, model, &text)?;
            if let Some(generation) = observation
                .as_ref()
                .and_then(HttpObservationGuard::generation_observer)
            {
                generation.observe(&v);
            }

            let mut response = match p.dialect {
                Dialect::OpenAI => crate::protocol::convert::openai_response_to_internal(&v),
                Dialect::Anthropic => crate::protocol::anthropic::anthropic_to_internal(&v),
                Dialect::Gemini => crate::protocol::gemini::from_gemini_response(&v),
                Dialect::Ollama => crate::protocol::ollama::from_ollama_response(&v),
                Dialect::Responses => {
                    crate::protocol::responses::from_responses_response(&v, model)
                }
            };
            response.reported_usage_evidence =
                super::reported_usage::observe(&v, p.dialect, response.usage.as_ref());
            if let Some(budget) = budget {
                budget.settle(response.reference_cost_usage()).await?;
            }
            Ok(response)
        }
        .await;
        if let Some(observation) = &observation {
            observation.finish_result(result.is_ok());
        }
        if crate::budget::holds::active() {
            result.map_err(GatewayError::no_replay)
        } else {
            result
        }
    }

    /// 非聊天端点（Embedding / 图片 / TTS）的原生转发。
    ///
    /// 这些端点没有共同 IR，先只对接 OpenAI 兼容厂商；不能用聊天模型顶替，
    /// 也不能在协议边界悄悄改成 `/chat/completions`。
    pub async fn call_passthrough(
        &self,
        p: &Provider,
        model: &str,
        path_segments: &[&str],
        body: &serde_json::Value,
        timeout: Duration,
        expect_json: bool,
    ) -> Result<PassthroughResponse> {
        self.call_passthrough_with_policy(
            p,
            model,
            path_segments,
            body,
            timeout,
            PassthroughWireOptions {
                expect_json,
                cost_policy: Default::default(),
            },
        )
        .await
    }

    pub(crate) async fn call_passthrough_with_policy(
        &self,
        p: &Provider,
        model: &str,
        path_segments: &[&str],
        body: &serde_json::Value,
        timeout: Duration,
        options: PassthroughWireOptions,
    ) -> Result<PassthroughResponse> {
        // Enforce the existing typed bound contract at the common wire entry:
        // the Gateway calls this policy-aware path directly, not the public shim.
        if crate::budget::holds::active() {
            return Err(crate::budget::holds::rejected(
                "passthrough_reference_bound_unverified",
            ));
        }
        // Responses 也放行：它是 OpenAI 系，只是请求体结构不同
        // （见 protocol::responses）。拦掉它会让自定义上游路径对
        // Responses 方言失效。
        if p.dialect != Dialect::OpenAI && p.dialect != Dialect::Responses {
            return Err(GatewayError::CapabilityUnavailable {
                kind: format!("{} 当前仅支持 OpenAI 兼容上游", path_segments.join("/")),
            });
        }

        let url = if let Some(path) = custom_upstream_path(p, model)? {
            build_url_from_path(p, &path, model)?
        } else {
            let mut url = validate_upstream_base_url(&p.base_url)?;
            append_url_path(&mut url, path_segments)?;
            url
        };
        let key = decrypt_provider_key(p)?;
        let mut headers = Self::headers_for(p, &key)?;
        let overrides = overrides(model, p);
        Self::apply_extra_headers(&mut headers, overrides.as_ref())?;

        let mut body = body.clone();
        if let Some(object) = body.as_object_mut() {
            object.insert("model".into(), serde_json::json!(model));
            if let Some(extra) = overrides
                .as_ref()
                .and_then(|overrides| overrides.extra_body.as_ref())
                .and_then(serde_json::Value::as_object)
            {
                for (key, value) in extra {
                    object.insert(key.clone(), value.clone());
                }
            }
        }

        let _free_permit = self
            .authorize_wire(p, model, &body, options.cost_policy)
            .await?;
        let resp = self
            .client()?
            .post(url)
            .timeout(timeout)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|error| map_reqwest_err(&p.name, model, error))?;
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(Self::classify_upstream_error(
                p,
                model,
                status.as_u16(),
                &text,
            ));
        }

        if options.expect_json {
            let bytes = resp
                .bytes()
                .await
                .map_err(|error| map_typed_body_error(&p.name, model, error))?;
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| GatewayError::Upstream {
                    provider: p.name.clone(),
                    model: model.into(),
                    status: 502,
                    body: truncate(&String::from_utf8_lossy(&bytes), 800),
                })?;
            Ok(PassthroughResponse::Json(value))
        } else {
            let body = resp
                .bytes()
                .await
                .map_err(|error| map_typed_body_error(&p.name, model, error))?;
            Ok(PassthroughResponse::Bytes { body, content_type })
        }
    }

    /// 流式请求：返回统一的事件流
    pub async fn call_stream(
        &self,
        p: &Provider,
        req: &ChatRequest,
        model: &str,
        timeout: Duration,
        defaults: &crate::config::OllamaOptionsConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<UpstreamEvent>> + Send>>> {
        self.call_stream_with_options(
            p,
            req,
            model,
            timeout,
            defaults,
            RequestWireOptions::default(),
        )
        .await
    }

    pub(crate) async fn call_stream_with_options(
        &self,
        p: &Provider,
        req: &ChatRequest,
        model: &str,
        timeout: Duration,
        defaults: &crate::config::OllamaOptionsConfig,
        options: RequestWireOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<UpstreamEvent>> + Send>>> {
        use futures_util::StreamExt;

        let mut observation = None;
        let result: Result<Pin<Box<dyn Stream<Item = Result<UpstreamEvent>> + Send>>> = async {
        validate_instruction_override(p, model, options)?;
        crate::protocol::reasoning::validate(req, p.dialect, selected_model(model, p).is_some_and(|m| m.supports_thinking))?;
        let url = Self::build_url(p, model, true)?;
        let key = decrypt_provider_key(p)?;
        let mut body = Self::build_body_with_options(p, req, model, defaults, options);
        if p.dialect != Dialect::Gemini {
            body["stream"] = serde_json::Value::Bool(true);
        }
        Self::apply_overrides(&mut body, overrides(model, p).as_ref());
        apply_thinking_capability(&mut body, model, p);

        let url = with_gemini_key(url, p.dialect, &key)?;

        let mut headers = Self::headers_for(p, &key)?;
        Self::apply_extra_headers(&mut headers, overrides(model, p).as_ref())?;

        let free_permit = self
            .authorize_wire(p, model, &body, options.cost_policy)
            .await?;
        let observation_context = super::observation_context::Snapshot::capture(p, &url, &body);
        let outgoing = self.client()?.post(url).timeout(timeout).headers(headers).json(&body);
        // Only the private raw report, not legacy normalized stream usage, can
        // release a reference hold after a protocol terminal event.
        let budget = crate::budget::holds::before_http(p, model, &body).await?;
        let free_permit=self.refresh_budget_admission(p,model,&body,options.cost_policy,&budget,free_permit).await?;
        observation = HttpObservationGuard::begin_with_projection(TimingMode::Streaming, Some(
                super::control_modes::WireModeSnapshot::observe(
                    p.dialect, selected_model(model, p).map(|m|m.supports_thinking),
                    options.caller, options.normalized_ingress, req, &body,
                ),
            ), observation_context);
        let resp = outgoing.send().await.map_err(|e| map_reqwest_err(&p.name, model, e))?;
        if let Some(observation) = &observation { observation.headers(); }

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = observed_text(resp, observation.as_ref()).await.unwrap_or_default();
            return Err(Self::classify_upstream_error(p, model, status, &text));
        }

        let dialect = p.dialect;
        let byte_observer = observation.as_ref().map(HttpObservationGuard::body_observer);
        let generation = observation.as_ref().and_then(HttpObservationGuard::generation_observer);
        let mut bytes = resp.bytes_stream();
        // Body deadlines must keep the same typed error as header deadlines.
        // Capture this actual invocation's identity before returning the stream;
        // polling must never consult a later candidate or task-local scope.
        let body_provider = p.name.clone();
        let body_model = model.to_owned();
        let stream = async_stream::stream! {
            let _free_permit=free_permit;
            let mut budget_report=crate::budget::holds::StreamReport::default();
            let mut buffer = Vec::new();

            while let Some(chunk) = bytes.next().await {
                match chunk {
                    Ok(chunk) => {
                        if budget.is_some() { budget_report.ingest(&chunk, dialect); }
                        if let Some(observer) = &byte_observer { observer.received(chunk.len()); }
                        buffer.extend_from_slice(&chunk);
                        let events = match dialect {
                            Dialect::Ollama => drain_ollama_lines(&mut buffer, generation.as_ref()),
                            _ => drain_sse_blocks(dialect, &mut buffer),
                        };
                        for event in events {
                            if matches!(&event, Ok(UpstreamEvent::Done { .. })) {
                                if let Some(budget)=&budget { if let Err(error)=budget.settle(budget_report.usage()).await { yield Err(error); return; } }
                            }
                            let terminal = event.is_err() || (dialect == Dialect::Responses
                                && matches!(&event, Ok(UpstreamEvent::Done { .. })));
                            yield event;
                            if terminal {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(map_reqwest_err(&body_provider, &body_model, e));
                        return;
                    }
                }
            }

            // 有些实现不会在最后一个 SSE 事件后补空行；EOF 时仍需消费完整残留。
            if !buffer.is_empty() {
                let events = match dialect {
                    Dialect::Ollama => parse_ollama_line(&buffer, generation.as_ref()),
                    _ => parse_sse_block(dialect, &buffer),
                };
                for event in events {
                    if matches!(&event, Ok(UpstreamEvent::Done { .. })) {
                        if let Some(budget)=&budget { if let Err(error)=budget.settle(budget_report.usage()).await { yield Err(error); return; } }
                    }
                        let terminal = event.is_err() || (dialect == Dialect::Responses
                            && matches!(&event, Ok(UpstreamEvent::Done { .. })));
                    yield event;
                        if terminal {
                            return;
                        }
                }
            }
        };

        Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = Result<UpstreamEvent>> + Send>>)
        }.await;
        match result {
            Ok(stream) => {
                let protected = crate::budget::holds::active();
                let stream = stream.map(move |event| {
                    if protected {
                        event.map_err(GatewayError::no_replay)
                    } else {
                        event
                    }
                });
                Ok(ObservedStream::wrap(Box::pin(stream), observation))
            }
            Err(error) => {
                if let Some(observation) = &observation {
                    observation.finish_result(false);
                }
                if crate::budget::holds::active() {
                    Err(error.no_replay())
                } else {
                    Err(error)
                }
            }
        }
    }
}

/* --------------------------- 统一流式事件模型 --------------------------- */

#[derive(Debug, Clone)]
pub enum UpstreamEvent {
    /// 一段文本增量
    Delta(String),
    ReasoningDelta(String),
    RefusalDelta(String),
    /// 工具调用增量（简化：整体下发，够 Claude Code / Codex 用）
    ToolCalls(serde_json::Value),
    /// 上游宣告生成已完成，但传输流尚未结束。OpenAI 的 usage-only chunk
    /// 正常会出现在该事件之后、`[DONE]` 之前，不能在这里提前关闭下游流。
    Finish {
        finish_reason: Option<String>,
    },
    /// 独立用量事件。某些上游（特别是 OpenAI `include_usage`）会以
    /// `choices: []` 的最后一个 chunk 单独发送它。
    Usage(Usage),
    /// Private numeric field-presence evidence; ignored by legacy consumers.
    UsageObservation(serde_json::Value),
    /// 结束：携带 usage 与 finish_reason
    Done {
        finish_reason: Option<String>,
        usage: Option<Usage>,
    },
    /// 心跳，透传给客户端保活
    Ping,
}

/// 从跨网络分块的缓存中取出所有完整 SSE 事件，残留半包继续留给下一次读取。
fn drain_sse_blocks(dialect: Dialect, buffer: &mut Vec<u8>) -> Vec<Result<UpstreamEvent>> {
    let mut out = Vec::new();

    while let Some((end, delimiter_len)) = find_sse_delimiter(buffer) {
        let event: Vec<u8> = buffer.drain(..end + delimiter_len).collect();
        out.extend(parse_sse_block(dialect, &event[..end]));
    }
    out
}

/// Ollama 使用 newline-delimited JSON，而不是 SSE。每一行都是独立事件。
fn drain_ollama_lines(
    buffer: &mut Vec<u8>,
    generation: Option<&super::generation::SharedGeneration>,
) -> Vec<Result<UpstreamEvent>> {
    let mut out = Vec::new();
    while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = buffer.drain(..=end).collect();
        out.extend(parse_ollama_line(&line, generation));
    }
    out
}

fn find_sse_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    for i in 0..buffer.len() {
        if buffer.get(i..i + 2) == Some(b"\n\n") {
            return Some((i, 2));
        }
        if buffer.get(i..i + 4) == Some(b"\r\n\r\n") {
            return Some((i, 4));
        }
    }
    None
}

/// 解析一段完整 SSE 事件。JSON 解码失败时不 panic；上游偶发的非 JSON 心跳直接忽略。
pub fn parse_sse_block(dialect: Dialect, bytes: &[u8]) -> Vec<Result<UpstreamEvent>> {
    let text = String::from_utf8_lossy(bytes);
    let mut event_name: Option<&str> = None;
    let mut data = String::new();

    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("event:") {
            event_name = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.trim());
        }
    }

    if data.is_empty() {
        return Vec::new();
    }
    if data == "[DONE]" {
        return vec![Ok(UpstreamEvent::Done {
            // A transport terminator is not a reported generation reason.
            // Public protocol encoders keep their compatibility defaults.
            finish_reason: None,
            usage: None,
        })];
    }

    let v: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    parse_event_value(dialect, event_name, v)
}

fn parse_ollama_line(
    bytes: &[u8],
    generation: Option<&super::generation::SharedGeneration>,
) -> Vec<Result<UpstreamEvent>> {
    let text = String::from_utf8_lossy(bytes);
    let line = text.trim();
    if line.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) => {
            if let Some(generation) = generation {
                generation.observe(&v);
            }
            parse_event_value(Dialect::Ollama, None, v)
        }
        // 截断或厂商自定义诊断行不应导致代理 panic；后续完整行仍可继续输出。
        Err(_) => Vec::new(),
    }
}

fn parse_event_value(
    dialect: Dialect,
    event_name: Option<&str>,
    v: serde_json::Value,
) -> Vec<Result<UpstreamEvent>> {
    // A successful HTTP header is not a successful model result. Native
    // error envelopes must not disappear behind a later DONE/stop/done:true.
    // Do not reflect arbitrary upstream messages or interpret their code as
    // verified HTTP status, zero usage or a refund/replay entitlement. This
    // POST already happened, so fail closed before or after visible content.
    if dialect != Dialect::Responses
        && (event_name == Some("error") || v.get("error").is_some_and(|error| !error.is_null()))
    {
        let provider = match dialect {
            Dialect::OpenAI => "OpenAI",
            Dialect::Anthropic => "Anthropic",
            Dialect::Gemini => "Gemini",
            Dialect::Ollama => "Ollama",
            Dialect::Responses => unreachable!(),
        };
        return vec![Err(GatewayError::Upstream {
            provider: provider.into(),
            model: String::new(),
            status: 502,
            body: format!("{provider} stream reported an inline error"),
        }
        .no_replay())];
    }
    let mut out = Vec::new();
    match dialect {
        Dialect::OpenAI => parse_openai_chunk(v, &mut out),
        Dialect::Responses => parse_responses_chunk(event_name, v, &mut out),
        Dialect::Anthropic => parse_anthropic_chunk(event_name, v, &mut out),
        Dialect::Gemini => parse_gemini_chunk(v, &mut out),
        Dialect::Ollama => parse_ollama_chunk(v, &mut out),
    }
    out
}

fn parse_openai_chunk(v: serde_json::Value, out: &mut Vec<Result<UpstreamEvent>>) {
    let usage = openai_usage(&v);
    let choice = v.get("choices").and_then(|c| c.get(0));
    if let Some(c) = choice {
        // Preserve existing normalized reasoning/refusal channels for compatible
        // HTTP streams; neither channel is a user-visible text token sample.
        if let Some(text) = c
            .pointer("/delta/reasoning_content")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
        {
            out.push(Ok(UpstreamEvent::ReasoningDelta(text.to_owned())));
        }
        if let Some(text) = c
            .pointer("/delta/refusal")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
        {
            out.push(Ok(UpstreamEvent::RefusalDelta(text.to_owned())));
        }
        if let Some(d) = c
            .get("delta")
            .and_then(|d| d.get("content"))
            .and_then(|x| x.as_str())
        {
            if !d.is_empty() {
                out.push(Ok(UpstreamEvent::Delta(d.to_string())));
            }
        }
        if let Some(tc) = c.get("delta").and_then(|d| d.get("tool_calls")) {
            out.push(Ok(UpstreamEvent::ToolCalls(tc.clone())));
        }
        if let Some(fr) = c.get("finish_reason").and_then(|x| x.as_str()) {
            // 这不是传输流结束。OpenAI 在 finish_reason 后仍可能发送一个
            // choices 为空的 usage-only chunk，随后才是 [DONE]。
            out.push(Ok(UpstreamEvent::Finish {
                finish_reason: Some(fr.to_string()),
            }));
        }
    }
    // 个别 OpenAI 兼容上游会把 finish_reason 和 usage 放在同一个 chunk；
    // 结束标记不是丢弃用量的理由，后续还有 usage-only chunk 时由 server 合并。
    if let Some(usage) = usage {
        out.push(Ok(UpstreamEvent::Usage(usage)));
    }
    if let Some(raw) = v.get("usage") {
        out.push(Ok(UpstreamEvent::UsageObservation(desktop_openai_usage(
            raw,
        ))));
    }
}

/// Optional display evidence, never a billing input. Only allowlisted numeric
/// usage fields cross the desktop boundary; arbitrary upstream error/secret
/// extensions are not reflected. Missing/invalid counters are not filled in.
fn desktop_openai_usage(raw: &serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    let Some(raw) = raw.as_object() else {
        return Value::Null;
    };
    let mut output = Map::new();
    let count = |value: &Value| value.as_u64().is_some_and(|n| n <= u64::from(u32::MAX));
    for name in [
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        "prompt_cache_hit_tokens",
        "prompt_cache_write_tokens",
    ] {
        if let Some(value) = raw.get(name).filter(|value| count(value)) {
            output.insert(name.into(), value.clone());
        }
    }
    if let Some(value) = raw
        .get("credit")
        .filter(|value| value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0))
    {
        output.insert("credit".into(), value.clone());
    }
    for name in [
        "prompt_tokens_details",
        "completion_tokens_details",
        "input_tokens_details",
        "output_tokens_details",
    ] {
        if let Some(details) = raw.get(name).and_then(Value::as_object) {
            let mut kept = Map::new();
            for field in [
                "cached_tokens",
                "cache_write_tokens",
                "cache_creation_tokens",
                "reasoning_tokens",
            ] {
                if let Some(value) = details.get(field).filter(|value| count(value)) {
                    kept.insert(field.into(), value.clone());
                }
            }
            if !kept.is_empty() {
                output.insert(name.into(), Value::Object(kept));
            }
        }
    }
    Value::Object(output)
}

fn openai_usage(v: &serde_json::Value) -> Option<Usage> {
    let usage = v.get("usage")?;
    Some(Usage::from_openai(usage))
}

fn responses_envelope_error() -> GatewayError {
    GatewayError::Upstream {
        provider: "Responses".into(),
        model: String::new(),
        status: 502,
        body: "Responses stream event envelope is invalid".into(),
    }
    .no_replay()
}

fn responses_terminal_valid(v: &serde_json::Value, kind: &str) -> bool {
    let expected = if kind == "response.completed" {
        "completed"
    } else {
        "incomplete"
    };
    let Some(response) = v.get("response").filter(|r| r.is_object()) else {
        return false;
    };
    v.get("type").map_or(true, |t| t.as_str() == Some(kind))
        && v.get("error").map_or(true, serde_json::Value::is_null)
        && response.get("status").and_then(|s| s.as_str()) == Some(expected)
        && response
            .get("error")
            .map_or(true, serde_json::Value::is_null)
        && response
            .get("output")
            .and_then(|o| o.as_array())
            .is_some_and(|items| {
                items.iter().all(|item| {
                    item.is_object()
                        && item
                            .get("type")
                            .and_then(|t| t.as_str())
                            .is_some_and(|t| !t.is_empty())
                })
            })
        && if expected == "completed" {
            response
                .get("incomplete_details")
                .map_or(true, serde_json::Value::is_null)
        } else {
            response
                .get("incomplete_details")
                .and_then(|d| d.get("reason"))
                .and_then(|r| r.as_str())
                .is_some_and(|r| !r.trim().is_empty())
        }
}

// Stream events can express wholly unknown usage without inventing scalar0.
// Keep the existing converter and its buffered/ordinary compatibility intact.
fn responses_stream_usage_known(response: &serde_json::Value) -> bool {
    let Some(usage) = response.get("usage").filter(|u| u.is_object()) else {
        return false;
    };
    let Some((input, output)) = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .zip(usage.get("output_tokens").and_then(|v| v.as_u64()))
    else {
        return false;
    };
    let limit = u64::from(u32::MAX);
    input <= limit
        && output <= limit
        && input + output <= limit
        && usage
            .get("total_tokens")
            .map_or(true, |v| v.as_u64() == Some(input + output))
}

fn parse_responses_chunk(
    event_name: Option<&str>,
    v: serde_json::Value,
    out: &mut Vec<Result<UpstreamEvent>>,
) {
    let kind = v
        .get("type")
        .and_then(|value| value.as_str())
        .or(event_name)
        .unwrap_or("");
    if event_name
        .filter(|name| name.starts_with("response.") || *name == "error")
        .zip(v.get("type").and_then(|t| t.as_str()))
        .is_some_and(|(declared, actual)| declared != actual)
    {
        out.push(Err(responses_envelope_error()));
        return;
    }
    match kind {
        "response.output_text.delta" => {
            if let Some(text) = v
                .get("delta")
                .and_then(|value| value.as_str())
                .filter(|text| !text.is_empty())
            {
                out.push(Ok(UpstreamEvent::Delta(text.to_owned())));
            }
        }
        "response.output_item.added" => {
            if let Some(item) = v.get("item").filter(|item| {
                item.get("type").and_then(|kind| kind.as_str()) == Some("function_call")
            }) {
                out.push(Ok(UpstreamEvent::ToolCalls(serde_json::json!([
                    openai_tool_call(
                        v.get("output_index")
                            .and_then(|index| index.as_u64())
                            .unwrap_or(0) as usize,
                        item.get("call_id").and_then(|id| id.as_str()),
                        item.get("name").and_then(|name| name.as_str()),
                        item.get("arguments")
                            .and_then(|args| args.as_str())
                            .unwrap_or_default()
                            .to_owned(),
                    )
                ]))));
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(delta) = v.get("delta").and_then(|value| value.as_str()) {
                out.push(Ok(UpstreamEvent::ToolCalls(serde_json::json!([
                    openai_tool_call_arguments(
                        v.get("output_index")
                            .and_then(|index| index.as_u64())
                            .unwrap_or(0) as usize,
                        delta,
                    )
                ]))));
            }
        }
        "response.completed" | "response.incomplete" => {
            if responses_terminal_valid(&v, kind) {
                let payload = &v["response"];
                let mut response = crate::protocol::responses::from_responses_response(payload, "");
                if !responses_stream_usage_known(payload) {
                    response.usage = None;
                }
                out.push(Ok(UpstreamEvent::Done {
                    finish_reason: response.finish_reason,
                    usage: response.usage,
                }));
            } else {
                out.push(Err(responses_envelope_error()));
            }
        }
        "response.failed" | "error" => {
            // Same already-dispatched unknown boundary as native inline errors.
            // A failed Responses event is not a zero-cost/replay receipt. Keep
            // arbitrary response.model/message out of every parser consumer;
            // actual candidate identity remains in the dispatch/attempt trace.
            out.push(Err(GatewayError::Upstream {
                provider: "Responses".into(),
                model: String::new(),
                status: 502,
                body: "Responses stream reported an inline error".into(),
            }
            .no_replay()));
        }
        // Item/argument done events repeat the complete content. Deltas already
        // carry it, so replaying these snapshots would duplicate text/arguments.
        _ => {}
    }
}

fn parse_anthropic_chunk(
    event: Option<&str>,
    v: serde_json::Value,
    out: &mut Vec<Result<UpstreamEvent>>,
) {
    match event.unwrap_or("") {
        "message_start" => {
            if let Some(usage) = anthropic_usage(v.get("message").and_then(|m| m.get("usage"))) {
                out.push(Ok(UpstreamEvent::Usage(usage)));
            }
        }
        "content_block_start" => {
            let block = v.get("content_block");
            let is_tool_use = block
                .and_then(|block| block.get("type"))
                .and_then(|kind| kind.as_str())
                == Some("tool_use");
            if !is_tool_use {
                return;
            }

            let index = stream_tool_index(&v);
            let id = block
                .and_then(|block| block.get("id"))
                .and_then(|id| id.as_str());
            let name = block
                .and_then(|block| block.get("name"))
                .and_then(|name| name.as_str());
            if let (Some(id), Some(name)) = (id, name) {
                // Anthropic 在 start 事件里通常给出空 input，随后再用
                // input_json_delta 传完整参数。空对象不能提前写成 "{}"，否则
                // 下游按增量拼接时会得到无效 JSON。
                let arguments =
                    anthropic_start_arguments(block.and_then(|block| block.get("input")));
                out.push(Ok(UpstreamEvent::ToolCalls(serde_json::Value::Array(
                    vec![openai_tool_call(index, Some(id), Some(name), arguments)],
                ))));
            }
        }
        "content_block_delta" => {
            if let Some(t) = v
                .get("delta")
                .and_then(|d| d.get("text"))
                .and_then(|x| x.as_str())
            {
                out.push(Ok(UpstreamEvent::Delta(t.to_string())));
            }
            if let Some(partial_json) = v
                .get("delta")
                .filter(|delta| {
                    delta.get("type").and_then(|kind| kind.as_str()) == Some("input_json_delta")
                })
                .and_then(|delta| delta.get("partial_json"))
                .and_then(|partial_json| partial_json.as_str())
            {
                if !partial_json.is_empty() {
                    // 只携带当前分片及原 content block 的索引。既有
                    // merge_tool_call_deltas 会按 index 拼接 arguments。
                    out.push(Ok(UpstreamEvent::ToolCalls(serde_json::Value::Array(
                        vec![openai_tool_call_arguments(
                            stream_tool_index(&v),
                            partial_json,
                        )],
                    ))));
                }
            }
        }
        "message_delta" => {
            let usage = anthropic_usage(v.get("usage"));
            let fr = v
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|x| x.as_str());
            // Message-level updates precede message_stop. They can carry the
            // final reason/usage without proving a complete transport stream.
            // Retain them just like OpenAI Finish + Usage; EOF or an error
            // before message_stop must still take the incomplete-stream path.
            if let Some(reason) = fr {
                out.push(Ok(UpstreamEvent::Finish {
                    finish_reason: Some(normalize_anthropic_finish_reason(reason)),
                }));
            }
            if let Some(usage) = usage {
                out.push(Ok(UpstreamEvent::Usage(usage)));
            }
        }
        "message_stop" => {
            out.push(Ok(UpstreamEvent::Done {
                // message_delta owns the reported stop_reason, if any.
                finish_reason: None,
                usage: None,
            }));
        }
        "ping" => out.push(Ok(UpstreamEvent::Ping)),
        _ => {}
    }
}

fn anthropic_usage(usage: Option<&serde_json::Value>) -> Option<Usage> {
    let usage = usage?;
    Some(Usage::from_anthropic(usage))
}

fn parse_gemini_chunk(v: serde_json::Value, out: &mut Vec<Result<UpstreamEvent>>) {
    let text = crate::protocol::gemini::extract_stream_text(&v);
    if !text.is_empty() {
        out.push(Ok(UpstreamEvent::Delta(text)));
    }
    let has_tool_calls = push_gemini_tool_calls(&v, out);
    let usage = gemini_usage(&v);
    let finish_reason = v
        .get("candidates")
        .and_then(|candidates| candidates.get(0))
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(|reason| reason.as_str())
        .map(|reason| match reason {
            "STOP" => "stop".to_string(),
            "MAX_TOKENS" => "length".to_string(),
            other => other.to_lowercase(),
        });
    if let Some(finish_reason) = finish_reason {
        out.push(Ok(UpstreamEvent::Done {
            finish_reason: Some(if has_tool_calls {
                "tool_calls".into()
            } else {
                finish_reason
            }),
            usage,
        }));
    } else {
        if has_tool_calls {
            // Gemini 的 functionCall 有时先于最终 STOP chunk。提前标记能让
            // 既有流式出口在最终 chunk 缺少 functionCall 时仍输出 tool_calls。
            out.push(Ok(UpstreamEvent::Finish {
                finish_reason: Some("tool_calls".into()),
            }));
        }
        if let Some(usage) = usage {
            out.push(Ok(UpstreamEvent::Usage(usage)));
        }
    }
}

fn gemini_usage(v: &serde_json::Value) -> Option<Usage> {
    let usage = v.get("usageMetadata")?;
    Some(Usage::from_gemini(usage))
}

fn parse_ollama_chunk(v: serde_json::Value, out: &mut Vec<Result<UpstreamEvent>>) {
    if let Some(t) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|x| x.as_str())
        .or_else(|| v.get("response").and_then(|x| x.as_str()))
    {
        if !t.is_empty() {
            out.push(Ok(UpstreamEvent::Delta(t.to_string())));
        }
    }
    let done = v.get("done").and_then(|x| x.as_bool()).unwrap_or(false);
    let has_tool_calls = push_ollama_tool_calls(&v, out);
    if has_tool_calls && !done {
        out.push(Ok(UpstreamEvent::Finish {
            finish_reason: Some("tool_calls".into()),
        }));
    }
    if done {
        let prompt_tokens = v
            .get("prompt_eval_count")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32;
        let completion_tokens = v.get("eval_count").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        let usage = (prompt_tokens > 0 || completion_tokens > 0).then_some(Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            ..Default::default()
        });
        out.push(Ok(UpstreamEvent::Done {
            finish_reason: if has_tool_calls {
                Some("tool_calls".into())
            } else {
                normalize_ollama_finish_reason(
                    v.get("done_reason").and_then(|reason| reason.as_str()),
                )
            },
            usage,
        }));
    }
}

/// 将各上游的工具调用增量归一化为 OpenAI streaming delta 形态。下游出口和
/// 持久化层已经依赖这一形态，因此所有方言都应提供 index/function.arguments。
fn openai_tool_call(
    index: usize,
    id: Option<&str>,
    name: Option<&str>,
    arguments: String,
) -> serde_json::Value {
    let mut call = serde_json::json!({
        "index": index,
        "type": "function",
        "function": { "arguments": arguments },
    });
    if let Some(id) = id {
        call["id"] = serde_json::json!(id);
    }
    if let Some(name) = name {
        call["function"]["name"] = serde_json::json!(name);
    }
    call
}

/// 后续参数分片必须不重复发送 id/name，否则下游客户端可能把每一片误认为新调用。
fn openai_tool_call_arguments(index: usize, partial_json: &str) -> serde_json::Value {
    serde_json::json!({
        "index": index,
        "function": { "arguments": partial_json },
    })
}

fn stream_tool_index(v: &serde_json::Value) -> usize {
    v.get("index")
        .and_then(|index| index.as_u64())
        .map(|index| index as usize)
        .unwrap_or(0)
}

fn tool_arguments(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(value)) => value.clone(),
        Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "{}".into()),
        None => "{}".into(),
    }
}

fn anthropic_start_arguments(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::Object(value)) if value.is_empty() => String::new(),
        value => tool_arguments(value),
    }
}

fn normalize_anthropic_finish_reason(reason: &str) -> String {
    match reason {
        "tool_use" => "tool_calls".into(),
        "max_tokens" => "length".into(),
        "end_turn" | "stop_sequence" => "stop".into(),
        other => other.into(),
    }
}

fn normalize_ollama_finish_reason(reason: Option<&str>) -> Option<String> {
    // Keep absence and unknown reported reasons; done=true alone is not stop.
    reason
        .filter(|reason| !reason.trim().is_empty())
        .map(str::to_owned)
}

fn push_gemini_tool_calls(v: &serde_json::Value, out: &mut Vec<Result<UpstreamEvent>>) -> bool {
    let Some(parts) = v
        .get("candidates")
        .and_then(|candidates| candidates.get(0))
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(|parts| parts.as_array())
    else {
        return false;
    };

    let calls = parts
        .iter()
        .filter_map(|part| {
            part.get("functionCall")
                .or_else(|| part.get("function_call"))
        })
        .filter_map(|call| {
            let name = call.get("name").and_then(|name| name.as_str())?;
            Some((name, call))
        })
        .enumerate()
        .map(|(index, (name, call))| {
            let id = call
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{index}"));
            openai_tool_call(
                index,
                Some(&id),
                Some(name),
                tool_arguments(call.get("args").or_else(|| call.get("arguments"))),
            )
        })
        .collect::<Vec<_>>();

    if calls.is_empty() {
        false
    } else {
        out.push(Ok(UpstreamEvent::ToolCalls(serde_json::Value::Array(
            calls,
        ))));
        true
    }
}

fn push_ollama_tool_calls(v: &serde_json::Value, out: &mut Vec<Result<UpstreamEvent>>) -> bool {
    let Some(calls) = v
        .get("message")
        .and_then(|message| message.get("tool_calls"))
        .or_else(|| v.get("tool_calls"))
        .and_then(|calls| calls.as_array())
    else {
        return false;
    };

    let calls = calls
        .iter()
        .filter_map(|call| {
            let function = call.get("function").unwrap_or(call);
            let name = function
                .get("name")
                .or_else(|| call.get("name"))
                .and_then(|name| name.as_str())?;
            Some((call, function, name))
        })
        .enumerate()
        .map(|(index, (call, function, name))| {
            let id = call
                .get("id")
                .or_else(|| function.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{index}"));
            openai_tool_call(
                index,
                Some(&id),
                Some(name),
                tool_arguments(function.get("arguments").or_else(|| call.get("arguments"))),
            )
        })
        .collect::<Vec<_>>();

    if calls.is_empty() {
        false
    } else {
        out.push(Ok(UpstreamEvent::ToolCalls(serde_json::Value::Array(
            calls,
        ))));
        true
    }
}

/* --------------------------------- 工具 --------------------------------- */

fn decrypt_provider_key(p: &Provider) -> Result<String> {
    if p.api_key_enc.trim().is_empty() {
        // 本地 Ollama / 无鉴权 vLLM 可以不填 Key；不要因此在网关侧提前失败。
        Ok(String::new())
    } else {
        crypto::decrypt(&p.api_key_enc)
    }
}

pub(crate) fn validate_upstream_base_url(base_url: &str) -> Result<Url> {
    let url = Url::parse(base_url.trim())
        .map_err(|error| GatewayError::Protocol(format!("上游 Base URL 无效: {error}")))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(GatewayError::Protocol(
            "上游 Base URL 仅支持 http 或 https".into(),
        ));
    }
    if url.host_str().is_none() {
        return Err(GatewayError::Protocol(
            "上游 Base URL 必须包含主机名".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(GatewayError::Protocol(
            "上游 Base URL 不允许包含用户名或密码".into(),
        ));
    }
    if url.fragment().is_some() {
        return Err(GatewayError::Protocol(
            "上游 Base URL 不允许包含片段标识".into(),
        ));
    }

    if url.scheme() == "http" && !allows_insecure_local_http(&url) {
        return Err(GatewayError::Protocol(
            "公网上游 Base URL 必须使用 https；http 仅允许回环或私网 LAN 服务".into(),
        ));
    }

    // 不阻断 localhost、回环或 RFC1918 私网：它们是 Ollama、vLLM 和 LAN
    // 部署的正常使用场景。仅拦截已知云元数据目标。
    let host = url.host_str().expect("host already verified");
    let normalized_host = host.trim_end_matches('.').to_ascii_lowercase();
    if matches!(
        normalized_host.as_str(),
        "169.254.169.254" | "metadata.google.internal"
    ) {
        return Err(GatewayError::Protocol(
            "上游 Base URL 指向受保护的云元数据地址".into(),
        ));
    }

    Ok(url)
}

fn allows_insecure_local_http(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    // `Url::host_str()` renders an IPv6 literal with brackets. Remove only that URI syntax
    // before parsing the address; domain names and IPv4 values remain untouched.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => address.is_loopback() || address.is_private(),
        Ok(IpAddr::V6(address)) => address.is_loopback() || is_ipv6_unique_local(address),
        Err(_) => false,
    }
}

fn is_ipv6_unique_local(address: Ipv6Addr) -> bool {
    (address.segments()[0] & 0xfe00) == 0xfc00
}

fn custom_upstream_path(p: &Provider, model: &str) -> Result<Option<String>> {
    let Some(model_ref) = selected_model(model, p) else {
        return Ok(None);
    };
    model_ref
        .validate_upstream_path()
        .map_err(GatewayError::Protocol)?;
    Ok(model_ref
        .upstream_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned))
}

fn build_url_from_path(p: &Provider, path: &str, model: &str) -> Result<Url> {
    let mut url = validate_upstream_base_url(&p.base_url)?;
    let resolved = path.replace("{model}", model);
    url.set_path(&resolved);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn append_url_path(url: &mut Url, segments: &[&str]) -> Result<()> {
    let mut path = url
        .path_segments_mut()
        .map_err(|_| GatewayError::Protocol("上游 Base URL 不能用于追加 API 路径".into()))?;
    path.pop_if_empty();
    for segment in segments {
        path.push(segment);
    }
    Ok(())
}

fn with_gemini_key(mut url: Url, dialect: Dialect, key: &str) -> Result<Url> {
    if dialect != Dialect::Gemini || key.is_empty() {
        return Ok(url);
    }
    url.query_pairs_mut().append_pair("key", key);
    Ok(url)
}

/// 许多兼容层会以 HTTP 200 包裹 `{ success: false }` 或 `{ error: ... }`。
/// 这些响应若被当作空助手消息，会错误地被标记为一次成功，进而阻断降级链。
fn ensure_successful_response(
    value: &serde_json::Value,
    provider: &Provider,
    model: &str,
    raw_body: &str,
) -> Result<()> {
    let application_error = value.get("error").is_some_and(|error| !error.is_null())
        || value.get("success").and_then(|success| success.as_bool()) == Some(false);
    let expected_payload = match provider.dialect {
        Dialect::OpenAI => value
            .get("choices")
            .and_then(|choices| choices.as_array())
            .is_some_and(|choices| !choices.is_empty()),
        Dialect::Anthropic => value
            .get("content")
            .and_then(|content| content.as_array())
            .is_some(),
        Dialect::Gemini => value
            .get("candidates")
            .and_then(|candidates| candidates.as_array())
            .is_some_and(|candidates| !candidates.is_empty()),
        Dialect::Ollama => value
            .get("message")
            .is_some_and(|message| message.is_object()),
        // Responses 成功时必有 `output` 数组（空数组也算成功 ——
        // 内容被上限截断时就是这样）。这里判「字段在不在」而不是「非空」，
        // 否则空输出会被误判成上游错误。
        Dialect::Responses => value.get("output").is_some_and(|o| o.is_array()),
    };

    if application_error || !expected_payload {
        return Err(GatewayError::Upstream {
            provider: provider.name.clone(),
            model: model.into(),
            status: 502,
            body: truncate(raw_body, 800),
        });
    }
    Ok(())
}

/// 上游错误体是否在说「上下文超了」。
///
/// 不能按 HTTP 码判：实测同一件事各家给 400/413/422 都有，所以只认文案。
/// 匹配的都是上游原文里的固定说法（中英文各覆盖几种），并且都要求**同时**
/// 出现「上下文/长度」与「超过/超限」两个语义，避免把别的 400 误判成超限
/// —— 误判的代价是本该失败的请求被静默重试到下一家。
pub fn looks_like_context_length_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let mentions_context = lower.contains("context length")
        || lower.contains("context_length")
        || lower.contains("maximum context")
        || lower.contains("context window")
        || lower.contains("too many tokens")
        // Anthropic 口径：它不说 context，说 "prompt is too long"。
        || lower.contains("prompt is too long")
        || lower.contains("input is too long")
        || body.contains("上下文");
    let mentions_exceeded = lower.contains("exceed")
        || lower.contains("maximum")
        || lower.contains("too long")
        || lower.contains("reduce the length")
        || body.contains("超")
        || body.contains("过长");
    mentions_context && mentions_exceeded
}

/// 从错误体里抠出「实际要了多少 / 只允许多少」。
///
/// 上游文案实测形如：`This endpoint's maximum context length is 256000 tokens.
/// However, you requested about 258091 tokens (...)`。
/// 抠不出来就返回 None —— 调用方会退化成 0/0，只影响错误文案，
/// 不影响「要不要换一家」这个判定（那由`looks_like_context_length_error` 决定）。
pub fn parse_context_length_error(body: &str) -> Option<(u32, u32)> {
    let mut required = None;
    let mut available = None;
    let chars: Vec<char> = body.chars().collect();

    // 关键词必须**紧邻**数字，不能只看「同一段文字里有没有」。
    // 实测踩过：报文 `...maximum context length is 256000 tokens. However,
    // you requested about 258091 tokens...` 里，用 64 字符窗口时两个关键词
    // 会同时落进每个数字的窗口，于是 256000 被误判成「实际请求量」。
    // 改成只看数字**前面** 48 个字符：「上限」一定写在数字之前，
    // 「请求量」则既可能在前（`requested: 258091`）也在后（`requested about
    // 258091`），所以两边都看但要求更近。
    let before_of = |i: usize| -> String {
        let from = i.saturating_sub(48);
        chars[from..i]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase()
    };
    let after_of = |j: usize| -> String {
        let to = (j + 32).min(chars.len());
        chars[j..to].iter().collect::<String>().to_ascii_lowercase()
    };

    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let mut digits = String::new();
        let mut j = i;
        while j < chars.len() && chars[j].is_ascii_digit() {
            digits.push(chars[j]);
            j += 1;
        }
        // 解析失败就跳过：错误文案里出现年份、版本号很常见，
        // 不能因此让整段返回 None。
        if let Ok(value) = digits.parse::<u32>() {
            let before = before_of(i);
            let after = after_of(j);
            let is_limit = before.contains("maximum context")
                || before.contains("maximum number of tokens")
                || before.contains("context length is")
                || before.contains("context window")
                || before.contains("最大");
            let is_requested = before.contains("requested")
                || before.contains("需要")
                || before.contains("prompt is too long")
                || after.starts_with(" tokens")
                || after.contains(" requested");
            // 先判上限：「上限」的前置词更明确，命中就不要再当成请求量。
            if is_limit {
                available.get_or_insert(value);
            } else if is_requested {
                required.get_or_insert(value);
            }
        }
        i = j.max(i + 1);
    }

    match (required, available) {
        (Some(r), Some(a)) => Some((r, a)),
        _ => None,
    }
}

// A typed successful response can hit its deadline while reading the body.
// Preserve the existing non-timeout classification/retry contract verbatim.
fn map_typed_body_error(provider: &str, model: &str, error: reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        map_reqwest_err(provider, model, error)
    } else {
        GatewayError::Other(anyhow::anyhow!(error.to_string()))
    }
}

fn map_reqwest_err(provider: &str, model: &str, e: reqwest::Error) -> GatewayError {
    if e.is_timeout() {
        GatewayError::Timeout(format!("{provider}/{model} 请求超时"))
    } else if e.is_connect() {
        GatewayError::Upstream {
            provider: provider.into(),
            model: model.into(),
            status: 503,
            body: e.to_string(),
        }
    } else {
        GatewayError::Other(anyhow::anyhow!(e.to_string()))
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod url_validation_tests {
    use super::validate_upstream_base_url;
    use crate::error::GatewayError;

    #[test]
    fn allows_local_and_lan_upstreams() {
        for base_url in [
            "http://localhost:11434",
            "http://127.0.0.1:11434/v1",
            "http://[::1]:11434",
            "http://10.0.0.8:8000/v1",
            "http://172.16.1.8:8000/v1",
            "http://192.168.1.16:8080/v1",
            "http://[fd00::8]:8000/v1",
            "https://api.example.test/v1",
        ] {
            assert!(
                validate_upstream_base_url(base_url).is_ok(),
                "expected {base_url} to remain usable"
            );
        }
    }

    #[test]
    fn rejects_unsafe_or_malformed_upstreams() {
        for base_url in [
            "file:///etc/passwd",
            "ftp://example.test/v1",
            "http://api.example.test/v1",
            "https://user:password@example.test/v1",
            "https://example.test/v1#fragment",
            "http://169.254.169.254/latest/meta-data",
            "https://metadata.google.internal/computeMetadata/v1",
        ] {
            let error = validate_upstream_base_url(base_url)
                .expect_err("unsafe Base URL must be rejected before a request is sent");
            assert!(matches!(error, GatewayError::Protocol(_)));
            assert!(!error.retryable());
        }
    }
}

#[cfg(test)]
mod instruction_override_transport_tests {
    use super::*;
    use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
    use futures_util::StreamExt;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    fn provider(base_url: String) -> Provider {
        let now = chrono::Utc::now();
        serde_json::from_value(json!({"id":"owned","name":"owned","dialect":"responses","base_url":base_url,"api_key_enc":"","enabled":true,"priority":1,"rpm_limit":0,"intelligence":50,"created_at":now,"updated_at":now,
            "models":[{"alias":"fixture","upstream":"fixture","context_window":16384,"supports_tools":true,"supports_vision":false,"supports_thinking":false,"supports_stream":true,"overrides":{"extra_body":{"instructions":"configured-B"}}}]})).unwrap()
    }
    async fn receive(
        State(capture): State<Arc<Mutex<Vec<Value>>>>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        capture.lock().unwrap().push(body.clone());
        let value = json!({"id":"direct","model":"fixture","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"direct answer"}]}]});
        if body["stream"] == true {
            let delta = json!({"type":"response.output_text.delta","delta":"direct answer"});
            let done = json!({"type":"response.completed","response":value});
            return (
                [("content-type", "text/event-stream")],
                format!("data: {delta}\n\ndata: {done}\n\n"),
            )
                .into_response();
        }
        Json(value).into_response()
    }
    fn assert_conflict(error: GatewayError) {
        assert!(matches!(error, GatewayError::CapabilityUnavailable { .. }));
        assert_eq!(error.http_status(), 400);
        assert!(!error.retryable());
        let text = error.public_message();
        assert!(text.contains("extra_body.instructions"));
        assert!(!text.contains("configured-B"));
        assert!(!text.contains("current-A"));
    }
    #[tokio::test]
    async fn responses_instructions_r3_direct_http_normal_and_stream_defend_before_post() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let capture = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = Router::new()
            .route("/responses", post(receive))
            .with_state(capture.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        struct Abort(tokio::task::JoinHandle<()>);
        impl Drop for Abort {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _guard = Abort(task);
        let mut p = provider(origin);
        let req:ChatRequest=serde_json::from_value(json!({"model":"fixture","messages":[{"role":"system","content":"current-A"},{"role":"user","content":"U"}],"stream":false,"max_tokens":42})).unwrap();
        let client = UpstreamClient::new();
        let defaults = crate::config::OllamaOptionsConfig::default();
        let strict = RequestWireOptions {
            preserve_instruction: true,
            ..Default::default()
        };
        for (index, value) in [
            Value::Null,
            json!(""),
            json!("configured-B"),
            json!("current-A"),
            json!({}),
            json!([]),
            json!(true),
            json!(17),
        ]
        .into_iter()
        .enumerate()
        {
            p.models[0].overrides.as_mut().unwrap().extra_body =
                Some(json!({"instructions":value}));
            p.models[0].overrides.as_ref().unwrap().validate().unwrap();
            // The HTTP transport defends itself independently of server metadata.
            p.runtime_id = Some("metadata-must-not-disable-http-defense".into());
            match client
                .call_with_options(
                    &p,
                    &req,
                    "fixture",
                    Duration::from_secs(2),
                    &defaults,
                    strict,
                )
                .await
            {
                Err(e) => assert_conflict(e),
                Ok(_) => panic!("normal transport accepted conflict"),
            }
            match client
                .call_stream_with_options(
                    &p,
                    &req,
                    "fixture",
                    Duration::from_secs(2),
                    &defaults,
                    strict,
                )
                .await
            {
                Err(e) => assert_conflict(e),
                Ok(_) => panic!("stream transport accepted conflict"),
            }
            assert_eq!(capture.lock().unwrap().len(), index * 2);
            p.runtime_id = None;
            client
                .call(&p, &req, "fixture", Duration::from_secs(2), &defaults)
                .await
                .unwrap();
            let events: Vec<_> = client
                .call_stream(&p, &req, "fixture", Duration::from_secs(2), &defaults)
                .await
                .unwrap()
                .collect()
                .await;
            assert!(events.iter().all(|v| v.is_ok()));
            assert_eq!(capture.lock().unwrap().len(), (index + 1) * 2);
            for body in &capture.lock().unwrap()[index * 2..] {
                assert_eq!(body["instructions"], value);
            }
        }
    }
    #[test]
    fn responses_instructions_r3_health_exemption_is_exact_and_private() {
        let strict = RequestWireOptions {
            preserve_instruction: true,
            ..Default::default()
        };
        let local = instruction_override_conflict();
        assert!(is_instruction_override_conflict(&local, strict));
        assert!(!is_instruction_override_conflict(
            &local,
            RequestWireOptions::default()
        ));
        for error in [
            GatewayError::CapabilityUnavailable {
                kind: "other local capability rejection".into(),
            },
            GatewayError::CapabilityUnavailable {
                kind: format!("prefix {INSTRUCTION_OVERRIDE_CONFLICT}"),
            },
            GatewayError::Protocol(INSTRUCTION_OVERRIDE_CONFLICT.into()),
            GatewayError::Upstream {
                provider: "owned".into(),
                model: "fixture".into(),
                status: 500,
                body: INSTRUCTION_OVERRIDE_CONFLICT.into(),
            },
        ] {
            assert!(!is_instruction_override_conflict(&error, strict));
        }
    }

    #[test]
    fn responses_instructions_r3_effective_override_lookup_and_dialect_scope() {
        let mut p = provider("http://127.0.0.1:1".into());
        let strict = RequestWireOptions {
            preserve_instruction: true,
            ..Default::default()
        };
        assert!(validate_instruction_override(&p, "fixture", strict).is_err());
        assert!(validate_instruction_override(&p, "missing-model", strict).is_ok());
        for dialect in [
            Dialect::OpenAI,
            Dialect::Anthropic,
            Dialect::Gemini,
            Dialect::Ollama,
        ] {
            p.dialect = dialect;
            assert!(validate_instruction_override(&p, "fixture", strict).is_ok());
        }
        p.dialect = Dialect::Responses;
        let conflict = p.models[0].clone();
        p.models[0].overrides = None;
        p.models.push(conflict);
        assert!(
            validate_instruction_override(&p, "fixture", strict).is_ok(),
            "same first matching upstream override as serializer/application"
        );
        p.models.swap(0, 1);
        assert!(validate_instruction_override(&p, "fixture", strict).is_err());
        for extra in [
            json!({"Instructions":"configured-B"}),
            json!({"other":{"instructions":"configured-B"}}),
        ] {
            p.models[0].overrides.as_mut().unwrap().extra_body = Some(extra);
            assert!(validate_instruction_override(&p, "fixture", strict).is_ok());
        }
    }
}

#[cfg(test)]
mod desktop_usage_observation_tests {
    use super::*;
    #[test]
    fn numeric_allowlist_retains_zero_partial_and_drops_secret_or_invalid_values() {
        use serde_json::json;
        for raw in [
            serde_json::Value::Null,
            json!(42),
            json!("PRIVATE_SYNTHETIC"),
        ] {
            assert!(desktop_openai_usage(&raw).is_null());
        }
        let raw = json!({"completion_tokens":0,"prompt_tokens":-1,"total_tokens":"PRIVATE_SYNTHETIC","credit":0.0,"debug_authorization":"PRIVATE_SYNTHETIC","prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":"PRIVATE_SYNTHETIC","unsafe":"PRIVATE_SYNTHETIC"}});
        let value = desktop_openai_usage(&raw);
        assert_eq!(
            value,
            json!({"completion_tokens":0,"credit":0.0,"prompt_tokens_details":{"cached_tokens":0}})
        );
        assert!(!value.to_string().contains("PRIVATE_SYNTHETIC"));
        assert_eq!(
            desktop_openai_usage(&json!({"completion_tokens":2,"credit":0.5})),
            json!({"completion_tokens":2,"credit":0.5})
        );
    }
}
