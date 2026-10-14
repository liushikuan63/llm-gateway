//! WorkBuddy's account-scoped model transport. The production origin is fixed;
//! neither provider configuration nor request extras choose an authentication target.
use super::upstream::UpstreamEvent;
use crate::domain::{ChatRequest, ChatResponse, FunctionCall, ToolCall, Usage};
use crate::error::{GatewayError, Result};
use crate::model_catalog::{ContextSource, DiscoveredModel, DiscoveryResponse};
use crate::workbuddy_auth::{ModelAuthorization, WorkBuddyAuth};
use futures_util::{Stream, StreamExt};
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const ORIGIN: &str = "https://www.workbuddy.ai";
const DESKTOP_UA: &str = "WorkBuddy/5.7.6";
const JSON_LIMIT: usize = 512 * 1024;
const FRAME_LIMIT: usize = 1024 * 1024;
// Local guards, not claims about the upstream service's limits.
const OUTPUT_LIMIT: usize = 64 * 1024 * 1024;
const CATALOG_TTL: Duration = Duration::from_secs(300);
const CATALOG_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TOOLS: usize = 64;
// A protocol placeholder only; retain supplied system instructions and every
// original message. The international API requires the first role to be system.
const FALLBACK_SYSTEM: &str = "You are a helpful assistant.";

type EventStream = Pin<Box<dyn Stream<Item = Result<UpstreamEvent>> + Send>>;
type CatalogSlot = Arc<Mutex<Option<CachedCatalog>>>;

pub(crate) struct WorkBuddyModelClient {
    auth: parking_lot::RwLock<WorkBuddyAuth>,
    http: parking_lot::RwLock<std::result::Result<reqwest::Client, String>>,
    catalogs: parking_lot::Mutex<BTreeMap<String, CatalogSlot>>,
    #[cfg(test)]
    test_origin: Option<String>,
}

#[derive(Clone)]
struct Catalog {
    response: DiscoveryResponse,
    entries: BTreeMap<String, Value>,
}

struct CachedCatalog {
    loaded: Instant,
    catalog: Option<Catalog>,
    failure_status: u16,
}

impl WorkBuddyModelClient {
    pub(crate) fn new(auth: WorkBuddyAuth) -> Self {
        Self {
            auth: parking_lot::RwLock::new(auth),
            http: parking_lot::RwLock::new(Self::build_http(None)),
            catalogs: parking_lot::Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            test_origin: None,
        }
    }

    fn build_http(proxy: Option<&str>) -> std::result::Result<reqwest::Client, String> {
        crate::outbound::apply_proxy(
            reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .pool_max_idle_per_host(16),
            proxy,
        )?
        .build()
        .map_err(|_| "无法创建 WorkBuddy 模型连接".into())
    }

    pub(crate) fn set_proxy(&self, proxy: Option<&str>) {
        let rebuilt_auth = self.auth.read().with_proxy(proxy);
        let rebuilt_http = Self::build_http(proxy);
        // A failed rebuild disables the model transport rather than silently using
        // the old proxy. with_proxy preserves the authorization service's gate.
        match rebuilt_auth {
            Ok(auth) => {
                *self.auth.write() = auth;
                *self.http.write() = rebuilt_http;
            }
            Err(error) => *self.http.write() = Err(error),
        }
    }

    fn client(&self) -> Result<reqwest::Client> {
        self.http.read().clone().map_err(GatewayError::Protocol)
    }

    fn origin(&self) -> &str {
        #[cfg(test)]
        if let Some(origin) = &self.test_origin {
            return origin;
        }
        ORIGIN
    }

    /// Only crate-internal tests may replace the fixed production origin.
    #[cfg(test)]
    pub(crate) fn with_test_origin(auth: WorkBuddyAuth, origin: String) -> Self {
        let mut client = Self::new(auth);
        client.test_origin = Some(origin);
        client
    }

    async fn authorize(&self, account_id: &str) -> Result<ModelAuthorization> {
        let auth = self.auth.read().clone();
        auth.prepare_model_authorization(account_id)
            .await
            .map_err(GatewayError::Unauthorized)
    }

    async fn catalog(&self, account_id: &str, auth: &ModelAuthorization) -> Result<Catalog> {
        let key = format!(
            "{account_id}\0{}\0{}",
            auth.identity_hash, auth.credential_fingerprint
        );
        let slot = {
            let mut slots = self.catalogs.lock();
            if !slots.contains_key(&key) {
                // Keep generations separate; obsolete credential generations never
                // authorize a request with a new credential. Bound retained accounts.
                let prefix = format!("{account_id}\0");
                slots.retain(|candidate, _| !candidate.starts_with(&prefix));
                if slots.len() >= 64 {
                    if let Some(oldest_key) = slots.keys().next().cloned() {
                        slots.remove(&oldest_key);
                    }
                }
            }
            slots
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        // Per account/generation single-flight. Cancellation drops the guard and
        // leaves no poisoned cached result, so the next caller can discover again.
        let mut cached = slot.lock().await;
        if let Some(value) = cached.as_ref().filter(|value| {
            value.loaded.elapsed()
                < if value.catalog.is_some() {
                    CATALOG_TTL
                } else {
                    Duration::from_secs(1)
                }
        }) {
            return value.catalog.clone().ok_or_else(|| GatewayError::Upstream {
                provider: "WorkBuddy".into(),
                model: "catalog".into(),
                status: value.failure_status,
                body: "WorkBuddy 目录最近发现失败，请稍后重试".into(),
            });
        }
        let result = self.load_catalog(auth).await;
        *cached = Some(CachedCatalog {
            loaded: Instant::now(),
            catalog: result.as_ref().ok().cloned(),
            failure_status: result
                .as_ref()
                .err()
                .map_or(502, |error| error.http_status().as_u16()),
        });
        result
    }

    async fn load_catalog(&self, auth: &ModelAuthorization) -> Result<Catalog> {
        let response = self
            .client()?
            .get(format!("{}/v3/config", self.origin()))
            .headers(auth.headers.clone())
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, DESKTOP_UA)
            .timeout(CATALOG_TIMEOUT)
            .send()
            .await
            .map_err(|error| network_error(error, "catalog"))?;
        let status = response.status().as_u16();
        let bytes = read_bounded(response, JSON_LIMIT).await?;
        if !(200..300).contains(&status) {
            return Err(response_error(status, "catalog", &bytes, auth));
        }
        if auth.contains_credential(&bytes) {
            return Err(protocol("目录响应包含认证凭据"));
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| protocol("目录不是有效 JSON"))?;
        let catalog = parse_catalog(&value)?;
        Ok(catalog)
    }

    pub(crate) async fn discover_models(
        &self,
        account_id: &str,
    ) -> std::result::Result<DiscoveryResponse, String> {
        let auth = self
            .authorize(account_id)
            .await
            .map_err(|error| error.to_string())?;
        self.catalog(account_id, &auth)
            .await
            .map(|catalog| catalog.response)
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn call_stream(
        &self,
        account_id: &str,
        req: &ChatRequest,
        upstream_model: &str,
        timeout: Duration,
    ) -> Result<EventStream> {
        let started = tokio::time::Instant::now();
        let preparation = async {
            let auth = self.authorize(account_id).await?;
            let catalog = self.catalog(account_id, &auth).await?;
            let metadata = catalog.entries.get(upstream_model).ok_or_else(|| {
                GatewayError::ModelNotFound("WorkBuddy 账号目录中没有请求的模型".into())
            })?;
            let body = request_body(req, upstream_model, metadata)?;
            let response = self
                .client()?
                .post(format!("{}/v2/chat/completions", self.origin()))
                .headers(auth.headers.clone())
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "text/event-stream")
                .header(USER_AGENT, DESKTOP_UA)
                .timeout(timeout)
                .json(&body)
                .send()
                .await
                .map_err(|error| network_error(error, "model"))?;
            let status = response.status().as_u16();
            let is_sse = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(';')
                        .next()
                        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
                });
            if !(200..300).contains(&status) || !is_sse {
                let bytes = read_bounded(response, JSON_LIMIT).await?;
                return Err(response_error(
                    if (200..300).contains(&status) {
                        502
                    } else {
                        status
                    },
                    upstream_model,
                    &bytes,
                    &auth,
                ));
            }
            Ok((response, auth))
        };
        let (response, auth) = tokio::time::timeout(timeout, preparation)
            .await
            .map_err(|_| GatewayError::Timeout("WorkBuddy 请求准备超时".into()))??;
        let deadline = started + timeout;
        let model = upstream_model.to_owned();
        let output_limit = req
            .max_tokens
            .map(|tokens| (tokens as usize).saturating_mul(128))
            .unwrap_or(OUTPUT_LIMIT)
            .clamp(FRAME_LIMIT, OUTPUT_LIMIT);
        Ok(Box::pin(async_stream::try_stream! {
            let mut bytes = response.bytes_stream();
            let mut parser = SseParser::new(output_limit);
            while let Some(chunk) = tokio::time::timeout_at(deadline, bytes.next()).await
                .map_err(|_| GatewayError::Timeout("WorkBuddy 响应超时".into()))? {
                let chunk = chunk.map_err(|error| network_error(error, &model))?;
                for event in parser.feed(&chunk, false, &auth)? { yield event; }
                if parser.done { return; }
            }
            for event in parser.feed(&[], true, &auth)? { yield event; }
        }))
    }

    pub(crate) async fn call(
        &self,
        account_id: &str,
        req: &ChatRequest,
        upstream_model: &str,
        timeout: Duration,
    ) -> Result<ChatResponse> {
        collect(
            self.call_stream(account_id, req, upstream_model, timeout)
                .await?,
            upstream_model,
        )
        .await
    }
}

fn protocol(message: &str) -> GatewayError {
    GatewayError::Upstream {
        provider: "WorkBuddy".into(),
        model: "model".into(),
        status: 502,
        body: format!("WorkBuddy 协议错误：{message}"),
    }
}

fn network_error(error: reqwest::Error, model: &str) -> GatewayError {
    if error.is_timeout() {
        GatewayError::Timeout("WorkBuddy 网络请求超时".into())
    } else {
        GatewayError::Upstream {
            provider: "WorkBuddy".into(),
            model: model.into(),
            status: 502,
            body: "WorkBuddy 网络请求失败".into(),
        }
    }
}

async fn read_bounded(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(protocol("JSON 响应超过本地大小限制"));
    }
    let mut bytes = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk.map_err(|error| network_error(error, "response"))?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(protocol("JSON 响应超过本地大小限制"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn validate_target_request(
    req: &ChatRequest,
    body: &Value,
    metadata: &Value,
    added_system: bool,
) -> Result<()> {
    for key in ["tools", "functions"] {
        if let Some(value) = body.get(key) {
            let tools = value
                .as_array()
                .ok_or_else(|| protocol("工具声明必须为数组"))?;
            if !tools.is_empty()
                && metadata.get("supportsToolCall").and_then(Value::as_bool) != Some(true)
            {
                return Err(protocol("目标模型目录没有声明工具调用能力"));
            }
        }
    }
    for message in &req.messages {
        if let crate::domain::Content::Parts(parts) = &message.content {
            for part in parts {
                let accepted = match part {
                    crate::domain::Part::Text { .. } => true,
                    crate::domain::Part::ImageUrl { .. } => {
                        metadata.get("disabledMultimodal").and_then(Value::as_bool) != Some(true)
                            && metadata.get("supportsImages").and_then(Value::as_bool) == Some(true)
                    }
                    crate::domain::Part::InputAudio { .. } => {
                        metadata.get("supportsAudio").and_then(Value::as_bool) == Some(true)
                    }
                    crate::domain::Part::VideoUrl { .. } => {
                        metadata.get("supportsVideo").and_then(Value::as_bool) == Some(true)
                    }
                };
                if !accepted {
                    return Err(protocol("目标模型目录没有声明请求的多模态能力"));
                }
            }
        }
    }
    let output_max = metadata
        .get("maxOutputTokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol("目录缺少输出上限"))?;
    for key in ["max_tokens", "max_completion_tokens"] {
        if let Some(value) = body.get(key) {
            let requested = value
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| protocol("请求的输出上限无效"))?;
            if requested > output_max {
                return Err(protocol("请求超过目标模型输出上限"));
            }
        }
    }
    let input_max = metadata
        .get("maxInputTokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol("目录缺少输入上限"))?;
    let schema_tokens: u64 = ["tools", "functions"]
        .iter()
        .filter_map(|key| body.get(*key))
        .map(|value| (value.to_string().chars().count() as u64).div_ceil(3))
        .sum();
    let implicit_tokens = if added_system {
        u64::from(crate::context::estimate_message_tokens(&[
            crate::domain::Message::system(FALLBACK_SYSTEM),
        ]))
    } else {
        0
    };
    if u64::from(crate::context::estimate_message_tokens(&req.messages))
        .saturating_add(implicit_tokens)
        .saturating_add(schema_tokens)
        > input_max
    {
        return Err(protocol(
            "估算上下文超过目标模型输入上限，请缩短历史或选择其他模型",
        ));
    }
    Ok(())
}

// Only bounded code and request-id metadata are retained. Messages/body are
// deliberately not reflected, including when a service echoes a credential.
fn response_error(
    status: u16,
    model: &str,
    bytes: &[u8],
    auth: &ModelAuthorization,
) -> GatewayError {
    let mut body = "WorkBuddy 拒绝请求或返回无效响应".to_string();
    if !auth.contains_credential(bytes) {
        if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
            let error = value.get("error").unwrap_or(&value);
            for (label, field) in [
                ("code", "code"),
                ("request_id", "request_id"),
                ("request_id", "requestId"),
            ] {
                if let Some(value) = error
                    .get(field)
                    .or_else(|| error.get("data").and_then(|data| data.get(field)))
                    .or_else(|| value.get(field))
                    .or_else(|| value.get("data").and_then(|data| data.get(field)))
                {
                    let text = match value {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        _ => continue,
                    };
                    if !text.is_empty()
                        && text.len() <= 64
                        && text
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_-.:".contains(&c))
                    {
                        body.push_str(&format!("; {label}={text}"));
                    }
                }
            }
        }
    }
    GatewayError::Upstream {
        provider: "WorkBuddy".into(),
        model: model.into(),
        status,
        body,
    }
}

fn business_error(value: &Value) -> bool {
    value.get("error").is_some_and(|error| !error.is_null())
        || [Some(value), value.get("data"), value.pointer("/error/data")]
            .into_iter()
            .flatten()
            .any(|object| {
                object
                    .get("code")
                    .is_some_and(|code| !(code.as_i64() == Some(0) || code.as_str() == Some("0")))
            })
}

fn parse_catalog(value: &Value) -> Result<Catalog> {
    if value.get("code").and_then(Value::as_i64) != Some(0) || business_error(value) {
        return Err(protocol("目录业务响应失败"));
    }
    let data = value.get("data").ok_or_else(|| protocol("目录缺少 data"))?;
    let raw_models = data
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol("目录缺少 models 数组"))?;
    let mut entries = BTreeMap::new();
    let mut models = BTreeMap::new();
    let mut duplicate_ids = HashSet::new();
    for raw in raw_models {
        let Some(id) = raw
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty() && id.len() <= 256)
        else {
            continue;
        };
        if models.contains_key(id) || duplicate_ids.contains(id) {
            models.remove(id);
            entries.remove(id);
            duplicate_ids.insert(id.to_owned());
            continue;
        }
        if raw.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(input) = raw
            .get("maxInputTokens")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0 && *n <= i32::MAX as u64)
        else {
            continue;
        };
        let Some(_output) = raw
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0 && *n <= u32::MAX as u64)
        else {
            continue;
        };
        let model = DiscoveredModel {
            id: id.into(),
            name: raw
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(id)
                .into(),
            context_window: input as i32,
            context_source: ContextSource::Provider,
            supports_tools: raw.get("supportsToolCall").and_then(Value::as_bool),
            supports_vision: if raw.get("disabledMultimodal").and_then(Value::as_bool) == Some(true)
            {
                Some(false)
            } else {
                raw.get("supportsImages").and_then(Value::as_bool)
            },
            supports_audio: raw.get("supportsAudio").and_then(Value::as_bool),
            supports_video: raw.get("supportsVideo").and_then(Value::as_bool),
            supports_stream: Some(true),
            supports_thinking: raw
                .get("supportsThinking")
                .and_then(Value::as_bool)
                .or_else(|| {
                    raw.get("reasoning")
                        .filter(|reasoning| reasoning.is_object())
                        .and_then(|reasoning| {
                            (reasoning
                                .get("effort")
                                .and_then(Value::as_str)
                                .is_some_and(|effort| !effort.is_empty())
                                || reasoning
                                    .get("defaultEffort")
                                    .and_then(Value::as_str)
                                    .is_some_and(|effort| !effort.is_empty())
                                || reasoning
                                    .get("supportedEfforts")
                                    .and_then(Value::as_array)
                                    .is_some_and(|efforts| {
                                        efforts.iter().any(|effort| {
                                            effort.as_str().is_some_and(|effort| !effort.is_empty())
                                        })
                                    })
                                || reasoning
                                    .get("canDisableThinking")
                                    .and_then(Value::as_bool)
                                    .is_some())
                            .then_some(true)
                        })
                }),
            model_type: Some(crate::domain::ModelType::Chat),
            is_free: None,
            price: None,
            capabilities: None,
        };
        models.insert(id.to_owned(), model);
        entries.insert(id.to_owned(), raw.clone());
    }
    if let Some(roster) = data
        .get("agents")
        .and_then(Value::as_array)
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent.get("name").and_then(Value::as_str) == Some("cli"))
        })
        .and_then(|agent| agent.get("models"))
        .and_then(Value::as_array)
        .filter(|roster| !roster.is_empty())
    {
        let ids: HashSet<&str> = roster.iter().filter_map(Value::as_str).collect();
        models.retain(|id, _| ids.contains(id.as_str()));
        entries.retain(|id, _| ids.contains(id.as_str()));
    }
    if models.is_empty() {
        return Err(protocol("目录没有有效模型"));
    }
    Ok(Catalog {
        response: DiscoveryResponse {
            base_url: ORIGIN.into(),
            models: models.into_values().collect(),
            warnings: vec![
                "WorkBuddy credit 为积分单位，目录倍率不代表货币价格或免费承诺。".into(),
            ],
        },
        entries,
    })
}

fn request_body(req: &ChatRequest, model: &str, metadata: &Value) -> Result<Value> {
    if req.max_tokens.is_some_and(|requested| {
        metadata
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| u64::from(requested) > maximum)
    }) {
        return Err(protocol("max_tokens 超过目录声明的模型输出上限"));
    }
    let mut body = crate::protocol::openai::to_upstream_body(req, model);
    body["model"] = json!(model);
    body["stream"] = json!(true);
    let messages = body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .filter(|messages| !messages.is_empty())
        .ok_or_else(|| protocol("请求缺少有效消息"))?;
    let added_system = messages[0].get("role").and_then(Value::as_str) != Some("system");
    if added_system {
        messages.insert(0, json!({"role":"system", "content":FALLBACK_SYSTEM}));
    }
    let mut options = body
        .get("stream_options")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    options.insert("include_usage".into(), json!(true));
    body["stream_options"] = Value::Object(options);
    // No extra can add credentials or change the authenticated request target.
    for key in [
        "authorization",
        "Authorization",
        "headers",
        "extra_headers",
        "api_key",
        "access_token",
        "refresh_token",
        "base_url",
        "upstream_path",
    ] {
        body.as_object_mut()
            .ok_or_else(|| protocol("请求体无效"))?
            .remove(key);
    }
    if let Some(choice) = body.get("tool_choice").cloned() {
        let normalized = match &choice {
            Value::String(_) => choice,
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("function") {
                    let name = object
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| protocol("tool_choice 缺少函数名"))?;
                    json!(name)
                } else {
                    match object.get("type").and_then(Value::as_str) {
                        Some("auto") => json!("auto"),
                        Some("any") | Some("required") => json!("required"),
                        Some("none") => json!("none"),
                        _ => return Err(protocol("tool_choice 无效")),
                    }
                }
            }
            _ => return Err(protocol("tool_choice 无效")),
        };
        if normalized.as_str() == Some("none") {
            let object = body.as_object_mut().ok_or_else(|| protocol("请求体无效"))?;
            object.remove("tools");
            object.remove("functions");
        }
        body["tool_choice"] = normalized;
    }
    if let Some(thinking) = &req.thinking {
        if let Some(effort) = thinking
            .get("effort")
            .or_else(|| thinking.get("reasoning_effort"))
            .and_then(Value::as_str)
        {
            let reasoning = metadata.get("reasoning");
            let supported = reasoning
                .and_then(|reasoning| reasoning.get("supportedEfforts"))
                .and_then(Value::as_array)
                .is_some_and(|efforts| {
                    efforts
                        .iter()
                        .any(|candidate| candidate.as_str() == Some(effort))
                })
                || reasoning
                    .and_then(|reasoning| reasoning.get("effort"))
                    .and_then(Value::as_str)
                    == Some(effort);
            if !supported {
                return Err(protocol("目录没有声明请求的 reasoning effort"));
            }
            body["reasoning_effort"] = json!(effort);
            body.as_object_mut()
                .ok_or_else(|| protocol("请求体无效"))?
                .remove("thinking");
        }
    }
    validate_target_request(req, &body, metadata, added_system)?;
    Ok(body)
}

#[derive(Default)]
struct ToolState {
    id: Option<String>,
    kind: Option<String>,
    name: String,
    arguments: String,
}

#[derive(Default)]
pub(crate) struct ToolAssembler {
    tools: BTreeMap<usize, ToolState>,
    ids: BTreeMap<String, usize>,
}

impl ToolAssembler {
    pub(crate) fn append(&mut self, calls: &Value) -> Result<Value> {
        let calls = calls
            .as_array()
            .ok_or_else(|| protocol("工具增量不是数组"))?;
        let mut normalized = Vec::new();
        for call in calls {
            let id = call
                .get("id")
                .map(|value| {
                    value
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| protocol("工具 id 无效"))
                })
                .transpose()?;
            let explicit_index = call
                .get("index")
                .map(|value| {
                    value
                        .as_u64()
                        .filter(|index| *index < MAX_TOOLS as u64)
                        .map(|index| index as usize)
                        .ok_or_else(|| protocol("工具 index 超出本地限制"))
                })
                .transpose()?;
            let index = if let Some(index) = explicit_index {
                index
            } else if let Some(index) = id.and_then(|id| self.ids.get(id)).copied() {
                index
            } else if id.is_some() {
                (0..MAX_TOOLS)
                    .find(|index| !self.tools.contains_key(index))
                    .ok_or_else(|| protocol("工具数量超出本地限制"))?
            } else if self.tools.len() == 1 && calls.len() == 1 {
                self.tools
                    .keys()
                    .next()
                    .copied()
                    .ok_or_else(|| protocol("工具增量无法关联"))?
            } else {
                return Err(protocol("工具增量没有可唯一关联的 index/id"));
            };
            if let Some(id) = id {
                if self.ids.get(id).is_some_and(|previous| *previous != index) {
                    return Err(protocol("工具 id/index 不稳定"));
                }
            }
            let tool = self.tools.entry(index).or_default();
            if let Some(id) = id {
                if tool.id.as_deref().is_some_and(|previous| previous != id) {
                    return Err(protocol("工具 id 不稳定"));
                }
                tool.id = Some(id.into());
                self.ids.insert(id.into(), index);
            }
            if let Some(kind) = call.get("type") {
                if kind.as_str() != Some("function") {
                    return Err(protocol("工具 type 无效"));
                }
                tool.kind = Some("function".into());
            }
            let function = call
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| protocol("工具 function 无效"))?;
            let mut delta = json!({"index": index, "type": "function", "function": {}});
            if let Some(id) = id {
                delta["id"] = json!(id);
            }
            for (key, accumulator) in [("name", &mut tool.name), ("arguments", &mut tool.arguments)]
            {
                if let Some(value) = function.get(key) {
                    let text = value
                        .as_str()
                        .ok_or_else(|| protocol("工具函数增量必须为字符串"))?;
                    if accumulator.len().saturating_add(text.len()) > FRAME_LIMIT {
                        return Err(protocol("工具内容超过本地大小限制"));
                    }
                    accumulator.push_str(text);
                    delta["function"][key] = json!(text);
                }
            }
            normalized.push(delta);
        }
        Ok(Value::Array(normalized))
    }

    pub(crate) fn complete(&self) -> Result<Vec<ToolCall>> {
        self.tools
            .values()
            .map(|tool| {
                let id = tool
                    .id
                    .as_ref()
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| protocol("结束时工具 id 缺失"))?;
                if tool.name.trim().is_empty() || tool.kind.as_deref() != Some("function") {
                    return Err(protocol("结束时工具 name/type 缺失"));
                }
                serde_json::from_str::<Value>(&tool.arguments)
                    .map_err(|_| protocol("结束时工具 arguments 不是完整 JSON"))?;
                Ok(ToolCall {
                    id: id.clone(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: tool.name.clone(),
                        arguments: tool.arguments.clone(),
                    },
                })
            })
            .collect()
    }

    /// Emit the completed name once, before Finish. Arguments remain ordinary
    /// increments. This satisfies both OpenAI clients that append name deltas
    /// and the gateway's existing persistence merger that replaces names.
    fn final_names(&self) -> Result<Value> {
        self.complete()?;
        Ok(Value::Array(
            self.tools
                .iter()
                .map(|(index, tool)| {
                    json!({
                        "index":index,"id":tool.id,"type":"function",
                        "function":{"name":tool.name,"arguments":""}
                    })
                })
                .collect(),
        ))
    }
}

fn merge_usage(target: &mut Option<Usage>, incoming: Usage) {
    if let Some(previous) = target {
        previous.prompt_tokens = previous.prompt_tokens.max(incoming.prompt_tokens);
        previous.completion_tokens = previous.completion_tokens.max(incoming.completion_tokens);
        previous.total_tokens = previous.total_tokens.max(incoming.total_tokens);
        previous.cache_read_tokens = previous.cache_read_tokens.max(incoming.cache_read_tokens);
        previous.cache_creation_tokens = previous
            .cache_creation_tokens
            .max(incoming.cache_creation_tokens);
        if let Some(tokens) = incoming.reasoning_tokens {
            previous.reasoning_tokens = Some(previous.reasoning_tokens.unwrap_or(0).max(tokens));
        }
        if incoming.credit.is_some() {
            previous.credit = incoming.credit;
        }
    } else {
        *target = Some(incoming);
    }
}

struct SseParser {
    line: Vec<u8>,
    data: Vec<u8>,
    event_error: bool,
    frame_bytes: usize,
    tools: ToolAssembler,
    finish: Option<String>,
    usage: Option<Usage>,
    output_bytes: usize,
    output_limit: usize,
    has_output: bool,
    done: bool,
}

impl SseParser {
    fn new(output_limit: usize) -> Self {
        Self {
            line: Vec::new(),
            data: Vec::new(),
            event_error: false,
            frame_bytes: 0,
            tools: ToolAssembler::default(),
            finish: None,
            usage: None,
            output_bytes: 0,
            output_limit,
            has_output: false,
            done: false,
        }
    }

    fn feed(
        &mut self,
        bytes: &[u8],
        eof: bool,
        auth: &ModelAuthorization,
    ) -> Result<Vec<UpstreamEvent>> {
        let mut events = Vec::new();
        for byte in bytes {
            if self.done {
                break;
            }
            self.frame_bytes += 1;
            if self.frame_bytes > FRAME_LIMIT {
                return Err(protocol("SSE frame 超过本地大小限制"));
            }
            if *byte == b'\n' {
                self.line(auth, &mut events)?;
            } else {
                self.line.push(*byte);
            }
        }
        if eof && !self.done {
            if !self.line.is_empty() {
                self.line(auth, &mut events)?;
            }
            if !self.data.is_empty() || self.event_error {
                self.frame(auth, &mut events)?;
            }
            if !self.done {
                return Err(protocol("SSE 在 [DONE] 之前结束"));
            }
        }
        Ok(events)
    }

    fn line(&mut self, auth: &ModelAuthorization, events: &mut Vec<UpstreamEvent>) -> Result<()> {
        let mut line = std::mem::take(&mut self.line);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            return self.frame(auth, events);
        }
        if line.starts_with(b":") {
            return Ok(());
        }
        let (field, value) = match line.iter().position(|byte| *byte == b':') {
            Some(index) => {
                let mut value = &line[index + 1..];
                if value.first() == Some(&b' ') {
                    value = &value[1..];
                }
                (&line[..index], value)
            }
            None => (line.as_slice(), &[][..]),
        };
        if field == b"data" {
            if !self.data.is_empty() {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(value);
        } else if field == b"event" && value == b"error" {
            self.event_error = true;
        }
        Ok(())
    }

    fn frame(&mut self, auth: &ModelAuthorization, events: &mut Vec<UpstreamEvent>) -> Result<()> {
        self.frame_bytes = 0;
        let data = std::mem::take(&mut self.data);
        if self.event_error {
            self.event_error = false;
            return Err(response_error(502, "model", &data, auth));
        }
        if data.is_empty() {
            return Ok(());
        }
        let text = std::str::from_utf8(&data).map_err(|_| protocol("SSE 不是有效 UTF-8"))?;
        if text.trim() == "[DONE]" {
            self.tools.complete()?;
            if !self.has_output {
                return Err(protocol("模型流没有正文、工具或 refusal"));
            }
            if self.finish.is_none() {
                return Err(protocol("模型流没有 finish_reason"));
            }
            self.done = true;
            events.push(UpstreamEvent::Done {
                finish_reason: self.finish.clone(),
                usage: self.usage.clone(),
            });
            return Ok(());
        }
        let value: Value =
            serde_json::from_str(text).map_err(|_| protocol("SSE data 不是有效 JSON"))?;
        if !value.is_object() {
            return Err(protocol("SSE data 必须为对象"));
        }
        if business_error(&value) {
            return Err(response_error(502, "model", &data, auth));
        }
        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            if !usage.is_object() {
                return Err(protocol("usage 必须为对象"));
            }
            let incoming = Usage::from_openai(usage);
            merge_usage(&mut self.usage, incoming.clone());
            events.push(UpstreamEvent::Usage(incoming));
        }
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| protocol("SSE 缺少 choices 数组"))?;
        if choices.len() > 1 {
            return Err(protocol("WorkBuddy 多 choice 响应尚不支持"));
        }
        for choice in choices {
            if choice
                .get("index")
                .is_some_and(|index| index.as_u64() != Some(0))
            {
                return Err(protocol("choice index 必须为 0"));
            }
            let delta = choice.get("delta").or_else(|| choice.get("message"));
            if let Some(delta) = delta {
                if !delta.is_object() {
                    return Err(protocol("choice delta 必须为对象"));
                }
                for field in ["content", "reasoning_content", "refusal"] {
                    let source = if field == "reasoning_content" {
                        delta.get(field).or_else(|| delta.get("reasoning"))
                    } else {
                        delta.get(field)
                    };
                    if let Some(source) = source.filter(|source| !source.is_null()) {
                        let text = source
                            .as_str()
                            .ok_or_else(|| protocol("文本增量必须为字符串"))?;
                        if text.is_empty() {
                            continue;
                        }
                        if self.finish.is_some() {
                            return Err(protocol("finish_reason 之后仍有文本增量"));
                        }
                        self.output_bytes = self.output_bytes.saturating_add(text.len());
                        if self.output_bytes > self.output_limit {
                            return Err(protocol("模型输出超过本地大小限制"));
                        }
                        if field != "reasoning_content" {
                            self.has_output = true;
                        }
                        events.push(match field {
                            "content" => UpstreamEvent::Delta(text.into()),
                            "refusal" => UpstreamEvent::RefusalDelta(text.into()),
                            _ => UpstreamEvent::ReasoningDelta(text.into()),
                        });
                    }
                }
                if let Some(calls) = delta.get("tool_calls").filter(|calls| !calls.is_null()) {
                    if self.finish.is_some() {
                        return Err(protocol("finish_reason 之后仍有工具增量"));
                    }
                    let mut normalized = self.tools.append(calls)?;
                    let tool_bytes = serde_json::to_vec(&normalized)
                        .map_err(|_| protocol("工具增量无效"))?
                        .len();
                    // Upstream name fragments are accumulated internally. Sending
                    // cumulative names repeatedly would duplicate the final name
                    // in standard clients; sending fragments would lose prefixes
                    // in the existing gateway persistence merger.
                    if let Some(calls) = normalized.as_array_mut() {
                        for call in calls {
                            if let Some(function) =
                                call.get_mut("function").and_then(Value::as_object_mut)
                            {
                                function.remove("name");
                            }
                        }
                    }
                    self.output_bytes = self.output_bytes.saturating_add(tool_bytes);
                    if self.output_bytes > self.output_limit {
                        return Err(protocol("模型输出超过本地大小限制"));
                    }
                    if normalized.as_array().is_some_and(|calls| !calls.is_empty()) {
                        self.has_output = true;
                        events.push(UpstreamEvent::ToolCalls(normalized));
                    }
                }
            }
            if let Some(reason) = choice
                .get("finish_reason")
                .filter(|reason| !reason.is_null())
            {
                let reason = reason
                    .as_str()
                    .ok_or_else(|| protocol("finish_reason 必须是字符串或 null"))?;
                // Some compatible providers emit an empty string on ordinary
                // deltas. It carries no finish signal; DONE still requires an
                // actual finish reason, so this never promotes EOF to success.
                if reason.is_empty() {
                    continue;
                }
                if reason == "error" {
                    return Err(response_error(502, "model", &data, auth));
                }
                if self.finish.as_deref().is_some_and(|old| old != reason) {
                    return Err(protocol("finish_reason 不稳定"));
                }
                if self.finish.is_none() && !self.tools.tools.is_empty() {
                    events.push(UpstreamEvent::ToolCalls(self.tools.final_names()?));
                }
                self.finish = Some(reason.into());
                events.push(UpstreamEvent::Finish {
                    finish_reason: self.finish.clone(),
                });
            }
        }
        Ok(())
    }
}

async fn collect(mut stream: EventStream, model: &str) -> Result<ChatResponse> {
    let mut response = ChatResponse {
        id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        model: model.into(),
        transport: Some("workbuddy-http".into()),
        ..Default::default()
    };
    let mut tools = ToolAssembler::default();
    let mut done = false;
    while let Some(event) = stream.next().await {
        match event? {
            UpstreamEvent::Delta(text) => response.content.push_str(&text),
            UpstreamEvent::ReasoningDelta(text) => response
                .reasoning_content
                .get_or_insert_with(String::new)
                .push_str(&text),
            UpstreamEvent::RefusalDelta(text) => response
                .refusal
                .get_or_insert_with(String::new)
                .push_str(&text),
            UpstreamEvent::ToolCalls(calls) => {
                tools.append(&calls)?;
            }
            UpstreamEvent::Finish { finish_reason } => response.finish_reason = finish_reason,
            UpstreamEvent::Usage(usage) => merge_usage(&mut response.usage, usage),
            UpstreamEvent::Done {
                finish_reason,
                usage,
            } => {
                response.finish_reason = finish_reason;
                if let Some(usage) = usage {
                    merge_usage(&mut response.usage, usage);
                }
                done = true;
            }
            UpstreamEvent::Ping => {}
        }
    }
    if !done {
        return Err(protocol("模型流没有成功终结"));
    }
    let tools = tools.complete()?;
    if !tools.is_empty() {
        response.tool_calls = Some(tools);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_TOKEN: &str = "fixture-wb-model-access";

    fn authorization() -> ModelAuthorization {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut bearer =
            reqwest::header::HeaderValue::from_static("Bearer fixture-wb-model-access");
        bearer.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, bearer);
        ModelAuthorization {
            headers,
            identity_hash: "fixture-identity-sha256".into(),
            credential_fingerprint: "fixture-generation-sha256".into(),
        }
    }

    fn frame(value: Value) -> String {
        format!("data: {value}\r\n\r\n")
    }

    fn chunk(delta: Value, finish: Option<&str>) -> Value {
        json!({"id":"fixture-completion", "choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    }

    #[test]
    fn empty_intermediate_finish_reason_is_not_a_terminal_event() {
        let text = frame(chunk(json!({"content":"O"}), Some("")))
            + &frame(chunk(json!({"content":"K"}), Some("stop")))
            + "data: [DONE]\n\n";
        let events = parse(text.as_bytes()).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, UpstreamEvent::Finish { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, UpstreamEvent::Done { .. }))
                .count(),
            1
        );
        let incomplete = frame(chunk(json!({"content":"O"}), Some(""))) + "data: [DONE]\n\n";
        assert!(parse(incomplete.as_bytes()).is_err());
        let invalid = frame(chunk(json!({"content":"O"}), None))
            .replace("\"finish_reason\":null", "\"finish_reason\":7")
            + &frame(chunk(json!({"content":"K"}), Some("stop")))
            + "data: [DONE]\n\n";
        assert!(parse(invalid.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("finish_reason 必须是字符串"));
    }

    fn parse(text: &[u8]) -> Result<Vec<UpstreamEvent>> {
        SseParser::new(OUTPUT_LIMIT).feed(text, true, &authorization())
    }

    fn model(id: &str) -> Value {
        json!({"id":id,"name":format!("Name {id}"),"maxInputTokens":32000,"maxOutputTokens":8192,
            "supportsToolCall":true,"supportsImages":true,"supportsAudio":true,"supportsVideo":true,"credits":"x0.00 credits",
            "reasoning":{"supportedEfforts":["low","high"]}})
    }

    fn request() -> ChatRequest {
        serde_json::from_value(json!({"model":"alias", "stream":false,
            "messages":[{"role":"system","content":"fixture system"},
                {"role":"assistant","content":"","tool_calls":[{"id":"old-tool","type":"function","function":{"name":"past","arguments":"{}"}}]},
                {"role":"tool","content":"past result","tool_call_id":"old-tool"},
                {"role":"user","content":[{"type":"text","text":"fixture question"},
                    {"type":"image_url","image_url":{"url":"data:image/png;base64,fixture-image","detail":"low"}},
                    {"type":"input_audio","input_audio":{"format":"wav","data":"fixture-audio"}},
                    {"type":"video_url","video_url":{"url":"https://example.invalid/fixture-video"}}]}],
            "tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object","properties":{}}}}],
            "tool_choice":{"type":"function","function":{"name":"lookup"}},
            "response_format":{"type":"json_object"}, "stream_options":{"include_usage":false,"fixture_option":true},
            "headers":{"Authorization":"fixture-must-not-forward"},"base_url":"https://example.invalid"}))
            .unwrap()
    }

    #[test]
    fn workbuddy_body_preserves_complete_ir_and_forces_official_wire() {
        let req = request();
        let expected = crate::protocol::openai::to_upstream_body(&req, "dynamic-model");
        let body = request_body(&req, "dynamic-model", &model("dynamic-model")).unwrap();
        assert_eq!(body["messages"], expected["messages"]);
        assert_eq!(body["tools"], expected["tools"]);
        assert_eq!(body["response_format"], expected["response_format"]);
        assert_eq!(body["model"], "dynamic-model");
        assert_eq!(body["tool_choice"], "lookup");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["stream_options"]["fixture_option"], true);
        assert!(body.get("headers").is_none());
        assert!(body.get("base_url").is_none());
        assert_eq!(ORIGIN, "https://www.workbuddy.ai");
        assert_eq!(DESKTOP_UA, "WorkBuddy/5.7.6");
    }

    #[test]
    fn workbuddy_user_first_request_requires_a_neutral_system_head_and_counts_its_budget() {
        let mut req = request();
        req.messages = vec![crate::domain::Message::user("只回复 OK")];
        req.tools = None;
        req.tool_choice = None;
        req.extra.clear();
        let body = request_body(&req, "m", &model("m")).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][0]["content"],
            "You are a helpful assistant."
        );
        assert_eq!(body["messages"][1]["content"], "只回复 OK");
        let mut exact = model("m");
        exact["maxInputTokens"] = json!(crate::context::estimate_message_tokens(&req.messages));
        assert!(
            request_body(&req, "m", &exact).is_err(),
            "implicit system tokens count toward the upstream input bound"
        );
        let mut history = request();
        history.messages.remove(0);
        let original = crate::protocol::openai::to_upstream_body(&history, "m");
        let converted = request_body(&history, "m", &model("m")).unwrap();
        assert_eq!(
            converted["messages"].as_array().unwrap()[1..],
            original["messages"].as_array().unwrap()[..],
            "existing history, tool IDs and media are preserved in order"
        );
    }

    #[test]
    fn workbuddy_none_removes_tools_but_auto_keeps_them() {
        let mut req = request();
        req.extra
            .insert("functions".into(), json!([{"name":"legacy"}]));
        req.tool_choice = Some(json!("none"));
        let disabled = request_body(&req, "m", &model("m")).unwrap();
        assert!(disabled.get("tools").is_none());
        assert!(disabled.get("functions").is_none());
        req.tool_choice = Some(json!({"type":"auto"}));
        let enabled = request_body(&req, "m", &model("m")).unwrap();
        assert!(enabled.get("tools").is_some());
        assert!(enabled.get("functions").is_some());
        assert_eq!(enabled["tool_choice"], "auto");
        req.tool_choice = Some(json!({"type":"function","function":{}}));
        assert!(request_body(&req, "m", &model("m")).is_err());
    }

    #[test]
    fn workbuddy_declared_reasoning_and_output_limits_are_checked() {
        let mut req = request();
        req.thinking = Some(json!({"effort":"high"}));
        let body = request_body(&req, "m", &model("m")).unwrap();
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking").is_none());
        req.thinking = Some(json!({"effort":"invented"}));
        assert!(request_body(&req, "m", &model("m")).is_err());
        req.thinking = None;
        req.max_tokens = Some(8192);
        assert!(request_body(&req, "m", &model("m")).is_ok());
        req.max_tokens = Some(8193);
        assert!(request_body(&req, "m", &model("m")).is_err());
    }

    #[test]
    fn workbuddy_catalog_is_dynamic_conservative_and_filters_invalid_entries() {
        let mut disabled = model("disabled");
        disabled["disabled"] = json!(true);
        let mut bad = model("bad");
        bad["maxOutputTokens"] = json!(0);
        let mut veto = model("veto");
        veto["disabledMultimodal"] = json!(true);
        let mut unknown = model("unknown");
        unknown.as_object_mut().unwrap().remove("supportsToolCall");
        unknown.as_object_mut().unwrap().remove("reasoning");
        let catalog = parse_catalog(
            &json!({"code":0,"data":{"models":[model("first"),model("new-model"),
            disabled,bad,veto,unknown,model("duplicate"),model("duplicate")]}}),
        )
        .unwrap();
        assert_eq!(catalog.response.models.len(), 4);
        assert!(catalog.entries.contains_key("new-model"));
        assert!(!catalog.entries.contains_key("disabled"));
        assert!(!catalog.entries.contains_key("bad"));
        assert!(!catalog.entries.contains_key("duplicate"));
        let veto = catalog
            .response
            .models
            .iter()
            .find(|m| m.id == "veto")
            .unwrap();
        assert_eq!(veto.supports_vision, Some(false));
        let unknown = catalog
            .response
            .models
            .iter()
            .find(|m| m.id == "unknown")
            .unwrap();
        assert_eq!(unknown.supports_tools, None);
        assert_eq!(unknown.supports_thinking, None);
        assert_eq!(veto.supports_thinking, Some(true));
        for entry in catalog.response.models {
            assert_eq!(entry.is_free, None);
            assert_eq!(entry.price, None);
        }
        assert!(parse_catalog(&json!({"code":1,"data":{"models":[model("m")]}})).is_err());
        assert!(parse_catalog(&json!({"code":0,"data":{"models":[]}})).is_err());
    }

    #[test]
    fn workbuddy_catalog_uses_declared_cli_roster_without_default_model() {
        let catalog = parse_catalog(&json!({"code":0,"data":{"models":[model("a"),model("b")],
            "agents":[{"name":"cli","models":["b"]}]}}))
        .unwrap();
        assert_eq!(catalog.response.models.len(), 1);
        assert_eq!(catalog.response.models[0].id, "b");
        assert!(
            parse_catalog(&json!({"code":0,"data":{"models":[model("a")],
            "agents":[{"name":"cli","models":["gone"]}]}}))
            .is_err()
        );
    }

    fn complete_fixture() -> String {
        let mut text = frame(chunk(json!({"reasoning_content":"思考中文"}), None));
        text += &frame(chunk(json!({"content":"中文回答"}), None));
        text += &frame(chunk(
            json!({"tool_calls":[
            {"index":7,"id":"tool-seven","type":"function","function":{"name":"look","arguments":"{\"城市\":"}},
            {"index":2,"id":"tool-two","type":"function","function":{"name":"we","arguments":"{\"n\":"}}]}),
            None,
        ));
        text += &frame(chunk(
            json!({"tool_calls":[
            {"id":"tool-two","function":{"name":"ather","arguments":"2}"}},
            {"index":7,"function":{"name":"up","arguments":"\"上海\"}"}}]}),
            None,
        ));
        text += &frame(chunk(json!({}), Some("tool_calls")));
        text += &frame(
            json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30,
            "prompt_tokens_details":{"cached_tokens":8,"cache_write_tokens":3},
            "completion_tokens_details":{"reasoning_tokens":4},"credit":2.5}}),
        );
        text += &frame(
            json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30,"credit":0}}),
        );
        text += "data: [DONE]";
        text
    }

    #[tokio::test]
    async fn workbuddy_every_byte_utf8_crlf_tool_fragments_tail_usage_and_credit_zero() {
        let auth = authorization();
        let mut parser = SseParser::new(OUTPUT_LIMIT);
        let mut events = Vec::new();
        for byte in complete_fixture().as_bytes() {
            events.extend(parser.feed(&[*byte], false, &auth).unwrap());
        }
        events.extend(parser.feed(&[], true, &auth).unwrap());
        assert!(parser.done);
        let response = collect(
            Box::pin(futures_util::stream::iter(events.into_iter().map(Ok))),
            "fixture-model",
        )
        .await
        .unwrap();
        assert_eq!(response.content, "中文回答");
        assert_eq!(response.reasoning_content.as_deref(), Some("思考中文"));
        assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(response.transport.as_deref(), Some("workbuddy-http"));
        let tools = response.tool_calls.unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].id, "tool-two");
        assert_eq!(tools[0].function.name, "weather");
        assert_eq!(tools[0].function.arguments, "{\"n\":2}");
        assert_eq!(tools[1].id, "tool-seven");
        assert_eq!(tools[1].function.name, "lookup");
        assert_eq!(tools[1].function.arguments, "{\"城市\":\"上海\"}");
        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 20);
        assert_eq!(usage.completion_tokens, 10);
        assert_eq!(usage.total_tokens, 30);
        assert_eq!(usage.cache_read_tokens, 8);
        assert_eq!(usage.cache_creation_tokens, 3);
        assert_eq!(usage.reasoning_tokens, Some(4));
        assert_eq!(usage.credit, Some(0.0));
    }

    #[test]
    fn workbuddy_multiline_data_and_final_frame_without_blank_line() {
        let text="data: {\"choices\":[{\"index\":0,\"delta\":\ndata: {\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n: ping\n\ndata: [DONE]\r\n";
        let events = parse(text.as_bytes()).unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event,UpstreamEvent::Delta(text) if text=="ok")));
        assert!(matches!(events.last(), Some(UpstreamEvent::Done { .. })));
    }

    #[test]
    fn workbuddy_error_empty_truncated_invalid_frames_are_failures() {
        let valid = frame(chunk(json!({"content":"partial"}), None));
        for text in [
            String::new(),
            "data: [DONE]\n\n".into(),
            "data: invalid-json\n\n".into(),
            frame(json!({"choices":{},"usage":null})),
            format!("{valid}{}", frame(chunk(json!({}), Some("stop")))),
            format!(
                "{valid}{}data: [DONE]\n\n",
                frame(chunk(json!({}), Some("error")))
            ),
            format!(
                "{}data: [DONE]\n\n",
                frame(chunk(json!({"reasoning":"only thinking"}), Some("error")))
            ),
            format!(
                "{valid}{}",
                frame(json!({"error":{"message":"fixture failure","code":14018}}))
            ),
            "event: error\ndata: {\"code\":42}\n\n".into(),
            "data: {\"code\":42,\"choices\":[]}\n\n".into(),
            "data: {\"data\":{\"code\":42},\"choices\":[]}\n\n".into(),
        ] {
            assert!(
                parse(text.as_bytes()).is_err(),
                "expected failure for synthetic frame"
            );
        }
        assert!(parse(b"data: \xff\n\n").is_err());
        assert!(parse(format!("data: {}\n\n", "a".repeat(FRAME_LIMIT)).as_bytes()).is_err());
    }

    #[tokio::test]
    async fn workbuddy_refusal_is_preserved_and_error_never_returns_partial_success() {
        let text = format!(
            "{}data: [DONE]\n\n",
            frame(chunk(json!({"refusal":"fixture refusal"}), Some("stop")))
        );
        let response = collect(
            Box::pin(futures_util::stream::iter(
                parse(text.as_bytes()).unwrap().into_iter().map(Ok),
            )),
            "m",
        )
        .await
        .unwrap();
        assert_eq!(response.refusal.as_deref(), Some("fixture refusal"));
        assert!(response.content.is_empty());
        let stream = Box::pin(futures_util::stream::iter(vec![
            Ok(UpstreamEvent::Delta("partial".into())),
            Err(protocol("fixture failure")),
        ]));
        assert!(collect(stream, "m").await.is_err());
    }

    #[test]
    fn workbuddy_tools_require_unambiguous_bounded_stable_complete_calls() {
        let mut tools = ToolAssembler::default();
        assert!(tools.append(&json!([{"index":64,"id":"fixture","type":"function","function":{"name":"f","arguments":"{}"}}])).is_err());
        assert!(tools
            .append(&json!([{"function":{"arguments":"{}"}}]))
            .is_err());
        tools.append(&json!([{"index":63,"id":"fixture-63","type":"function","function":{"name":"f","arguments":"{"}}])).unwrap();
        assert!(tools.complete().is_err());
        tools
            .append(&json!([{"id":"fixture-63","function":{"arguments":"}"}}]))
            .unwrap();
        assert_eq!(tools.complete().unwrap()[0].function.arguments, "{}");
        assert!(tools
            .append(&json!([{"index":63,"id":"other-id","function":{"arguments":""}}]))
            .is_err());
        assert!(tools
            .append(&json!([{"index":62,"id":"fixture-63","function":{"arguments":""}}]))
            .is_err());
        tools.append(&json!([{"index":1,"id":"second","type":"function","function":{"name":"g","arguments":"{}"}}])).unwrap();
        assert!(tools
            .append(&json!([{"function":{"arguments":""}}]))
            .is_err());
        assert!(tools
            .append(&json!([{"index":1,"type":"unrecognized","function":{"arguments":""}}]))
            .is_err());
        let mut full = ToolAssembler::default();
        for index in 0..MAX_TOOLS {
            full.append(&json!([{"id":format!("fixture-{index}"),"type":"function","function":{"name":"f","arguments":"{}"}}])).unwrap();
        }
        assert_eq!(full.complete().unwrap().len(), MAX_TOOLS);
        assert!(full.append(&json!([{"id":"overflow","type":"function","function":{"name":"f","arguments":"{}"}}])).is_err());
    }

    #[test]
    fn workbuddy_finish_and_output_guards_are_strict() {
        let text = format!(
            "{}{}data: [DONE]\n\n",
            frame(chunk(json!({"content":"ok"}), Some("stop"))),
            frame(chunk(json!({"content":"after finish"}), None))
        );
        assert!(parse(text.as_bytes()).is_err());
        let text = format!(
            "{}data: [DONE]\n\n",
            frame(chunk(json!({"content":"ok"}), None))
        );
        assert!(parse(text.as_bytes()).is_err());
        let mut parser = SseParser::new(2);
        assert!(parser
            .feed(
                frame(chunk(json!({"content":"long"}), None)).as_bytes(),
                false,
                &authorization()
            )
            .is_err());
        let text = frame(
            json!({"choices":[{"index":0,"delta":{"content":"a"}},{"index":1,"delta":{"content":"b"}}]}),
        );
        assert!(parse(text.as_bytes()).is_err());
    }

    #[test]
    fn workbuddy_error_metadata_does_not_reflect_plain_or_escaped_credential() {
        let auth = authorization();
        let bytes=serde_json::to_vec(&json!({"error":{"message":FIXTURE_TOKEN,"code":FIXTURE_TOKEN,"request_id":FIXTURE_TOKEN}})).unwrap();
        let error = response_error(401, "fixture-model", &bytes, &auth).to_string();
        assert!(!error.contains(FIXTURE_TOKEN));
        let escaped =
            br#"{"error":{"code":"fixture-wb-model-\u0061ccess","request_id":"safe-id"}}"#;
        let error = response_error(403, "fixture-model", escaped, &auth).to_string();
        assert!(!error.contains("safe-id"));
        assert!(!error.contains(FIXTURE_TOKEN));
        let error = response_error(
            429,
            "fixture-model",
            br#"{"error":{"code":14018,"request_id":"fixture-request"}}"#,
            &auth,
        )
        .to_string();
        assert!(error.contains("14018"));
        assert!(error.contains("fixture-request"));
    }

    #[derive(Clone)]
    struct HttpFixture {
        captured: Arc<parking_lot::Mutex<Vec<CapturedRequest>>>,
        catalog_models: Arc<parking_lot::Mutex<Vec<Value>>>,
    }

    // Deliberately no Debug: captured headers and bodies remain in fixture memory.
    struct CapturedRequest {
        method: &'static str,
        headers: reqwest::header::HeaderMap,
        body: Option<Value>,
    }

    struct HttpServer {
        origin: String,
        fixture: HttpFixture,
        client: WorkBuddyModelClient,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for HttpServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn http_catalog(
        axum::extract::State(fixture): axum::extract::State<HttpFixture>,
        headers: axum::http::HeaderMap,
    ) -> axum::Json<Value> {
        fixture.captured.lock().push(CapturedRequest {
            method: "GET",
            headers,
            body: None,
        });
        // Give concurrent callers a deterministic opportunity to reach single-flight.
        tokio::time::sleep(Duration::from_millis(10)).await;
        axum::Json(json!({"code":0,"data":{"models":fixture.catalog_models.lock().clone()}}))
    }

    async fn http_model(
        axum::extract::State(fixture): axum::extract::State<HttpFixture>,
        headers: axum::http::HeaderMap,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let token = headers
            .get("authorization")
            .and_then(|header| header.to_str().ok())
            .unwrap_or_default()
            .strip_prefix("Bearer ")
            .unwrap_or_default()
            .to_owned();
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        fixture.captured.lock().push(CapturedRequest {
            method: "POST",
            headers,
            body: Some(body),
        });
        match model.as_str() {
            "fixture-refusal" => {
                let text = format!(
                    "{}{}data: [DONE]\n\n",
                    frame(chunk(
                        json!({"reasoning_content":"fixture refusal reasoning","refusal":"fixture refusal"}),
                        Some("stop")
                    )),
                    frame(
                        json!({"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3,"credit":0}})
                    )
                );
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    text,
                )
                    .into_response()
            }
            "fixture-json-error" => axum::Json(
                json!({"error":{"message":token,"code":14018,"request_id":"fixture-error"}}),
            )
            .into_response(),
            "fixture-http-401" => (
                axum::http::StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error":{"message":token,"code":"fixture-unauthorized"}})),
            )
                .into_response(),
            "fixture-stream-error" | "fixture-truncated" => {
                let first = frame(chunk(json!({"content":"fixture partial"}), None));
                let tail = if model == "fixture-truncated" {
                    frame(chunk(json!({}), Some("stop")))
                } else {
                    frame(json!({"error":{"message":token,"code":14018}}))
                };
                let stream = async_stream::stream! {
                    yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from(first));
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from(tail));
                };
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    axum::body::Body::from_stream(stream),
                )
                    .into_response()
            }
            _ => (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                complete_fixture(),
            )
                .into_response(),
        }
    }

    async fn http_server() -> HttpServer {
        use base64::Engine;
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            if std::env::var_os("LLMGW_MASTER_KEY").is_none() {
                std::env::set_var(
                    "LLMGW_MASTER_KEY",
                    base64::engine::general_purpose::STANDARD.encode([73u8; 32]),
                );
            }
        });
        let fixture = HttpFixture {
            captured: Arc::new(parking_lot::Mutex::new(Vec::new())),
            catalog_models: Arc::new(parking_lot::Mutex::new(
                [
                    "fixture-good",
                    "fixture-refusal",
                    "fixture-stream-error",
                    "fixture-truncated",
                    "fixture-json-error",
                    "fixture-http-401",
                ]
                .into_iter()
                .map(model)
                .collect(),
            )),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = axum::Router::new()
            .route("/v3/config", axum::routing::get(http_catalog))
            .route("/v2/chat/completions", axum::routing::post(http_model))
            .with_state(fixture.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let auth = WorkBuddyAuth::with_test_origin(db.pool().clone(), origin.clone());
        auth.test_seed_account(
            "fixture-account-a",
            "1001",
            "fixture-a-access",
            "fixture-a-refresh",
        )
        .await;
        auth.test_seed_account(
            "fixture-account-b",
            "1002",
            "fixture-b-access",
            "fixture-b-refresh",
        )
        .await;
        let client = WorkBuddyModelClient::with_test_origin(auth, origin.clone());
        *client.http.write() = Ok(reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap());
        HttpServer {
            origin,
            fixture,
            client,
            task,
        }
    }

    #[tokio::test]
    async fn actual_target_capabilities_and_final_body_limits_reject_before_post() {
        for (key, value) in [
            ("supportsToolCall", json!(false)),
            ("supportsImages", json!(false)),
            ("disabledMultimodal", json!(true)),
            ("supportsAudio", json!(false)),
            ("supportsVideo", json!(false)),
            ("maxInputTokens", json!(1)),
        ] {
            let server = http_server().await;
            let mut entry = model("fixture-good");
            entry[key] = value;
            *server.fixture.catalog_models.lock() = vec![entry];
            assert!(
                server
                    .client
                    .call(
                        "fixture-account-a",
                        &request(),
                        "fixture-good",
                        Duration::from_secs(5)
                    )
                    .await
                    .is_err(),
                "accepted {key}"
            );
            assert_eq!(
                server
                    .fixture
                    .captured
                    .lock()
                    .iter()
                    .filter(|entry| entry.method == "POST")
                    .count(),
                0
            );
        }
        for key in ["max_tokens", "max_completion_tokens"] {
            let server = http_server().await;
            let mut req = request();
            req.max_tokens = None;
            req.extra.insert(key.into(), json!(1000000));
            assert!(server
                .client
                .call(
                    "fixture-account-a",
                    &req,
                    "fixture-good",
                    Duration::from_secs(5)
                )
                .await
                .is_err());
            assert_eq!(
                server
                    .fixture
                    .captured
                    .lock()
                    .iter()
                    .filter(|entry| entry.method == "POST")
                    .count(),
                0
            );
        }
        let small = http_server().await;
        let mut entry = model("fixture-good");
        entry["maxInputTokens"] = json!(32);
        *small.fixture.catalog_models.lock() = vec![entry];
        let mut req:ChatRequest=serde_json::from_value(json!({"model":"alias","stream":false,"messages":[{"role":"user","content":"short"}],
            "tools":[{"type":"function","function":{"name":"large","description":"schema detail ".repeat(200),"parameters":{"type":"object"}}}]})).unwrap();
        assert!(small
            .client
            .call(
                "fixture-account-a",
                &req,
                "fixture-good",
                Duration::from_secs(5)
            )
            .await
            .is_err());
        assert_eq!(
            small
                .fixture
                .captured
                .lock()
                .iter()
                .filter(|entry| entry.method == "POST")
                .count(),
            0
        );
        let legacy = http_server().await;
        let mut entry = model("fixture-good");
        entry["supportsToolCall"] = json!(false);
        *legacy.fixture.catalog_models.lock() = vec![entry];
        req.tools = None;
        req.extra.insert(
            "functions".into(),
            json!([{"name":"legacy","parameters":{"type":"object"}}]),
        );
        assert!(legacy
            .client
            .call(
                "fixture-account-a",
                &req,
                "fixture-good",
                Duration::from_secs(5)
            )
            .await
            .is_err());
        assert_eq!(
            legacy
                .fixture
                .captured
                .lock()
                .iter()
                .filter(|entry| entry.method == "POST")
                .count(),
            0
        );
        let server = http_server().await;
        assert!(
            server
                .client
                .call(
                    "fixture-account-a",
                    &request(),
                    "fixture-good",
                    Duration::from_secs(5)
                )
                .await
                .is_ok(),
            "declared capabilities control"
        );
        assert_eq!(
            server
                .fixture
                .captured
                .lock()
                .iter()
                .filter(|entry| entry.method == "POST")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn workbuddy_http_nonstream_complete_body_accounts_and_auth_headers() {
        let server = http_server().await;
        let req = request();
        let a = server
            .client
            .call(
                "fixture-account-a",
                &req,
                "fixture-good",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        let b = server
            .client
            .call(
                "fixture-account-b",
                &req,
                "fixture-good",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        for response in [&a, &b] {
            assert_eq!(response.content, "中文回答");
            assert_eq!(response.reasoning_content.as_deref(), Some("思考中文"));
            assert_eq!(
                response.tool_calls.as_ref().unwrap()[0].function.name,
                "weather"
            );
            assert_eq!(
                response.tool_calls.as_ref().unwrap()[1].function.name,
                "lookup"
            );
            let usage = response.usage.as_ref().unwrap();
            assert_eq!(usage.total_tokens, 30);
            assert_eq!(usage.reasoning_tokens, Some(4));
            assert_eq!(usage.cache_read_tokens, 8);
            assert_eq!(usage.cache_creation_tokens, 3);
            assert_eq!(usage.credit, Some(0.0));
        }
        let capture = server.fixture.captured.lock();
        assert_eq!(
            capture.iter().filter(|entry| entry.method == "GET").count(),
            2
        );
        assert_eq!(
            capture
                .iter()
                .filter(|entry| entry.method == "POST")
                .count(),
            2
        );
        let expected = crate::protocol::openai::to_upstream_body(&req, "fixture-good");
        for (uid, token) in [
            ("1001", "Bearer fixture-a-access"),
            ("1002", "Bearer fixture-b-access"),
        ] {
            let account_requests: Vec<_> = capture
                .iter()
                .filter(|entry| {
                    entry.headers.get("x-user-id").and_then(|v| v.to_str().ok()) == Some(uid)
                })
                .collect();
            assert_eq!(account_requests.len(), 2);
            for entry in account_requests {
                assert_eq!(
                    entry
                        .headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok()),
                    Some(token)
                );
                assert_eq!(
                    entry.headers.get("x-domain").and_then(|v| v.to_str().ok()),
                    Some("https://www.workbuddy.ai")
                );
                assert_eq!(
                    entry.headers.get("x-product").and_then(|v| v.to_str().ok()),
                    Some("SaaS")
                );
                assert_eq!(
                    entry
                        .headers
                        .get("user-agent")
                        .and_then(|v| v.to_str().ok()),
                    Some(DESKTOP_UA)
                );
                assert!(!entry.headers.contains_key("x-refresh-token"));
                if let Some(body) = &entry.body {
                    assert_eq!(body["messages"], expected["messages"]);
                    assert_eq!(body["tools"], expected["tools"]);
                    assert_eq!(body["tool_choice"], "lookup");
                    assert_eq!(body["stream"], true);
                    assert_eq!(body["stream_options"]["include_usage"], true);
                    assert!(body.get("headers").is_none());
                    assert!(body.get("base_url").is_none());
                }
            }
        }
        assert!(server.origin.starts_with("http://127.0.0.1:"));
    }

    #[tokio::test]
    async fn workbuddy_http_stream_emits_complete_name_once_and_preserves_refusal_usage() {
        let server = http_server().await;
        let mut req = request();
        req.stream = true;
        let stream = server
            .client
            .call_stream(
                "fixture-account-a",
                &req,
                "fixture-good",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        let events: Vec<_> = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let mut names = BTreeMap::<u64, Vec<String>>::new();
        let mut finish_seen = false;
        for event in &events {
            match event {
                UpstreamEvent::Finish { .. } => finish_seen = true,
                UpstreamEvent::ToolCalls(calls) => {
                    for call in calls.as_array().unwrap() {
                        if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                            assert!(!finish_seen, "complete name must precede Finish");
                            names
                                .entry(call["index"].as_u64().unwrap())
                                .or_default()
                                .push(name.into());
                        }
                    }
                }
                _ => {}
            }
        }
        assert_eq!(names.get(&2), Some(&vec!["weather".to_string()]));
        assert_eq!(names.get(&7), Some(&vec!["lookup".to_string()]));
        let response = collect(
            Box::pin(futures_util::stream::iter(events.into_iter().map(Ok))),
            "fixture-good",
        )
        .await
        .unwrap();
        assert_eq!(
            response.tool_calls.as_ref().unwrap()[0].function.name,
            "weather"
        );
        assert_eq!(
            response.tool_calls.as_ref().unwrap()[1].function.name,
            "lookup"
        );
        let refusal = server
            .client
            .call(
                "fixture-account-a",
                &req,
                "fixture-refusal",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(refusal.refusal.as_deref(), Some("fixture refusal"));
        assert_eq!(
            refusal.reasoning_content.as_deref(),
            Some("fixture refusal reasoning")
        );
        assert_eq!(refusal.usage.unwrap().credit, Some(0.0));
        let refusal_events = server
            .client
            .call_stream(
                "fixture-account-a",
                &req,
                "fixture-refusal",
                Duration::from_secs(5),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(refusal_events.iter().any(
            |event| matches!(event, UpstreamEvent::RefusalDelta(text) if text=="fixture refusal")
        ));
        assert!(refusal_events.iter().any(|event| matches!(event, UpstreamEvent::ReasoningDelta(text) if text=="fixture refusal reasoning")));
        assert!(refusal_events
            .iter()
            .any(|event| matches!(event, UpstreamEvent::Usage(usage) if usage.credit==Some(0.0))));
        assert!(matches!(
            refusal_events.last(),
            Some(UpstreamEvent::Done { .. })
        ));
        assert_eq!(
            server
                .fixture
                .captured
                .lock()
                .iter()
                .filter(|entry| entry.method == "GET")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn workbuddy_http_unknown_model_and_concurrent_catalog_send_zero_model_posts() {
        let server = http_server().await;
        let (first, second) = tokio::join!(
            server.client.discover_models("fixture-account-a"),
            server.client.discover_models("fixture-account-a")
        );
        assert!(first.is_ok());
        assert!(second.is_ok());
        assert!(server
            .client
            .call(
                "fixture-account-a",
                &request(),
                "fixture-not-in-catalog",
                Duration::from_secs(5)
            )
            .await
            .is_err());
        let capture = server.fixture.captured.lock();
        assert_eq!(
            capture.iter().filter(|entry| entry.method == "GET").count(),
            1
        );
        assert_eq!(
            capture
                .iter()
                .filter(|entry| entry.method == "POST")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn workbuddy_http_errors_are_not_success_and_never_retried() {
        let server = http_server().await;
        let req = request();
        for name in [
            "fixture-stream-error",
            "fixture-truncated",
            "fixture-json-error",
            "fixture-http-401",
        ] {
            let error = server
                .client
                .call("fixture-account-a", &req, name, Duration::from_secs(5))
                .await
                .unwrap_err();
            assert!(!error.to_string().contains("fixture-a-access"));
            assert!(!error.to_string().contains("fixture-a-refresh"));
        }
        let mut stream = server
            .client
            .call_stream(
                "fixture-account-a",
                &req,
                "fixture-stream-error",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        let mut errors = 0;
        let mut done = 0;
        while let Some(event) = stream.next().await {
            match event {
                Err(_) => errors += 1,
                Ok(UpstreamEvent::Done { .. }) => done += 1,
                _ => {}
            }
        }
        assert_eq!(errors, 1);
        assert_eq!(done, 0);
        let capture = server.fixture.captured.lock();
        assert_eq!(
            capture.iter().filter(|entry| entry.method == "GET").count(),
            1
        );
        assert_eq!(
            capture
                .iter()
                .filter(|entry| entry.method == "POST")
                .count(),
            5
        );
        for name in [
            "fixture-truncated",
            "fixture-json-error",
            "fixture-http-401",
        ] {
            assert_eq!(
                capture
                    .iter()
                    .filter(|entry| entry
                        .body
                        .as_ref()
                        .and_then(|b| b.get("model"))
                        .and_then(Value::as_str)
                        == Some(name))
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn workbuddy_http_dynamic_models_disabled_entries_and_failed_discovery_singleflight() {
        let server = http_server().await;
        let mut disabled = model("fixture-disabled");
        disabled["disabled"] = json!(true);
        server
            .fixture
            .catalog_models
            .lock()
            .extend([model("fixture-new-from-response"), disabled]);
        let response = server
            .client
            .call(
                "fixture-account-a",
                &request(),
                "fixture-new-from-response",
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(response.model, "fixture-new-from-response");
        assert!(server
            .client
            .call(
                "fixture-account-a",
                &request(),
                "fixture-disabled",
                Duration::from_secs(5)
            )
            .await
            .is_err());
        assert_eq!(
            server
                .fixture
                .captured
                .lock()
                .iter()
                .filter(|entry| entry.method == "POST")
                .count(),
            1
        );

        let failing = http_server().await;
        failing.fixture.catalog_models.lock().clear();
        let (first, second) = tokio::join!(
            failing.client.discover_models("fixture-account-a"),
            failing.client.discover_models("fixture-account-a")
        );
        assert!(first.is_err());
        assert!(second.is_err());
        assert!(failing
            .client
            .call(
                "fixture-account-a",
                &request(),
                "fixture-good",
                Duration::from_secs(5)
            )
            .await
            .is_err());
        let capture = failing.fixture.captured.lock();
        assert_eq!(
            capture.iter().filter(|entry| entry.method == "GET").count(),
            1
        );
        assert_eq!(
            capture
                .iter()
                .filter(|entry| entry.method == "POST")
                .count(),
            0
        );
    }
}
