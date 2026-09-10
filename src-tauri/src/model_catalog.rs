//! Provider 模型目录发现。
//!
//! 这个模块只读取上游公开的模型目录，不会把 API Key 写入日志、错误消息或 URL
//! 查询参数。保存 Provider 和目录发现共用同一份基础地址规范化逻辑，避免用户把
//! `/chat/completions`、`/messages` 等完整接口地址保存为基础地址后被重复拼接。

use futures_util::{stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use crate::crypto;
use crate::domain::{Dialect, Provider};
use crate::proxy::upstream::validate_upstream_base_url;

pub const DEFAULT_CONTEXT_WINDOW: i32 = 32_768;

const DISCOVERY_DEADLINE: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PAGES: usize = 10;
const MAX_OLLAMA_MODELS: usize = 96;
const OLLAMA_SHOW_CONCURRENCY: usize = 4;

/// 前端请求的目录发现参数。`api_key` 仅在本次调用的内存中使用。
#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryInput {
    pub provider_id: Option<String>,
    pub dialect: Dialect,
    pub base_url: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContextSource {
    Provider,
    Default,
}

/// 从上游目录读取的一条模型记录。没有可靠元数据时，能力字段保持 `None`，
/// 上下文窗口使用明确标记过的保守默认值，而不是从名称推断。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: String,
    pub context_window: i32,
    pub context_source: ContextSource,
    pub supports_tools: Option<bool>,
    pub supports_vision: Option<bool>,
    pub supports_stream: Option<bool>,
    pub is_free: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveryResponse {
    pub base_url: String,
    pub models: Vec<DiscoveredModel>,
    pub warnings: Vec<String>,
}

impl DiscoveredModel {
    fn default_for(id: String, name: String) -> Self {
        Self {
            id,
            name,
            context_window: DEFAULT_CONTEXT_WINDOW,
            context_source: ContextSource::Default,
            supports_tools: None,
            supports_vision: None,
            supports_stream: None,
            is_free: None,
        }
    }

    fn with_context(mut self, context_window: Option<i32>) -> Self {
        if let Some(context_window) = context_window {
            self.context_window = context_window;
            self.context_source = ContextSource::Provider;
        }
        self
    }
}

/// 将可填写的 API 地址转换为网关保存和请求时共同使用的基础地址。
///
/// 支持用户直接粘贴各方言的完整调用 URL，例如 OpenAI 的
/// `/v1/chat/completions`、Anthropic 的 `/v1/messages`、Gemini 的
/// `/v1beta/models` 和 Ollama 的 `/api/chat`。查询参数一律拒绝，避免把
/// 凭据或其他一次性参数持久化并在目录请求中转发。
pub fn normalize_base_url(dialect: Dialect, raw: &str) -> Result<String, String> {
    let mut url =
        validate_upstream_base_url(raw).map_err(|_| "API 地址无效或不被允许".to_string())?;
    if url.query().is_some() {
        return Err("API 地址不得包含查询参数".into());
    }

    let segments = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let keep = normalized_path_len(dialect, &segments);
    let path = if keep == 0 {
        String::new()
    } else {
        format!("/{}", segments[..keep].join("/"))
    };

    url.set_path(&path);
    url.set_query(None);
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// `normalized_base_url` 必须来自 [`normalize_base_url`]。该比较用于决定编辑
/// Provider 时能否继续使用已加密的旧 Key；地址或方言任一变化都不能复用。
pub fn matches_saved_provider_target(
    provider: &Provider,
    dialect: Dialect,
    normalized_base_url: &str,
) -> bool {
    provider.dialect == dialect
        && normalize_base_url(provider.dialect, &provider.base_url)
            .is_ok_and(|stored_base_url| stored_base_url == normalized_base_url)
}

/// 发现目录。调用方可传入同 ID 的已保存 Provider；只有调用方未填写 Key 且目标
/// 的协议和规范地址与已保存记录完全一致时，才会在进程内解密并复用旧 Key。
pub async fn discover(
    input: &DiscoveryInput,
    saved_provider: Option<&Provider>,
    proxy_url: Option<&str>,
) -> Result<DiscoveryResponse, String> {
    let base_url = normalize_base_url(input.dialect, &input.base_url)?;
    let api_key = resolve_api_key(input, saved_provider, &base_url)?;
    let client = build_client(proxy_url)?;

    tokio::time::timeout(
        DISCOVERY_DEADLINE,
        discover_inner(&client, input.dialect, &base_url, &api_key),
    )
    .await
    .map_err(|_| "模型目录发现超时".to_string())?
}

fn normalized_path_len(dialect: Dialect, segments: &[&str]) -> usize {
    let len = segments.len();
    match dialect {
        Dialect::OpenAI => {
            if ends_with(segments, &["chat", "completions"]) {
                len.saturating_sub(2)
            } else if segments
                .last()
                .is_some_and(|segment| matches!(*segment, "completions" | "models"))
            {
                len.saturating_sub(1)
            } else {
                len
            }
        }
        Dialect::Anthropic => {
            if segments
                .last()
                .is_some_and(|segment| matches!(*segment, "messages" | "models"))
            {
                len.saturating_sub(1)
            } else {
                len
            }
        }
        Dialect::Gemini => {
            if segments.last() == Some(&"models") {
                len.saturating_sub(1)
            } else {
                len
            }
        }
        Dialect::Ollama => {
            if ends_with(segments, &["api", "chat"])
                || ends_with(segments, &["api", "tags"])
                || ends_with(segments, &["api", "show"])
            {
                len.saturating_sub(2)
            } else if segments.last() == Some(&"api") {
                len.saturating_sub(1)
            } else {
                len
            }
        }
    }
}

fn ends_with(segments: &[&str], suffix: &[&str]) -> bool {
    segments.len() >= suffix.len()
        && segments
            .get(segments.len() - suffix.len()..)
            .is_some_and(|tail| tail == suffix)
}

fn resolve_api_key(
    input: &DiscoveryInput,
    saved_provider: Option<&Provider>,
    normalized_base_url: &str,
) -> Result<String, String> {
    let entered_key = input.api_key.trim();
    if !entered_key.is_empty() {
        return Ok(entered_key.to_string());
    }

    let Some(saved_provider) = saved_provider else {
        return if input.provider_id.is_some() {
            Err("已保存的 Provider 不存在，请重新输入 API Key".into())
        } else {
            // 本地 Ollama、无鉴权 vLLM 和部分企业网关允许空 Key；让服务端实际
            // 响应决定是否需要鉴权，而不是在 UI 命令层错误阻断。
            Ok(String::new())
        };
    };

    if saved_provider.api_key_enc.trim().is_empty() {
        return Ok(String::new());
    }
    if !matches_saved_provider_target(saved_provider, input.dialect, normalized_base_url) {
        return Err("API 地址或协议已变更，请重新输入 API Key".into());
    }
    crypto::decrypt(&saved_provider.api_key_enc)
        .map_err(|_| "已保存的 API Key 无法读取，请重新输入".to_string())
}

fn build_client(proxy_url: Option<&str>) -> Result<Client, String> {
    let mut builder = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(4)
        .redirect(reqwest::redirect::Policy::none())
        .gzip(true)
        .brotli(true);

    if let Some(proxy_url) = proxy_url.map(str::trim).filter(|proxy| !proxy.is_empty()) {
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|_| "系统代理配置无效，无法获取模型目录".to_string())?;
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|_| "无法初始化模型目录 HTTP 客户端".to_string())
}

async fn discover_inner(
    client: &Client,
    dialect: Dialect,
    base_url: &str,
    api_key: &str,
) -> Result<DiscoveryResponse, String> {
    let (models, warnings) = match dialect {
        Dialect::OpenAI => discover_openai(client, base_url, api_key).await?,
        Dialect::Anthropic => discover_anthropic(client, base_url, api_key).await?,
        Dialect::Gemini => discover_gemini(client, base_url, api_key).await?,
        Dialect::Ollama => discover_ollama(client, base_url, api_key).await?,
    };

    Ok(DiscoveryResponse {
        base_url: base_url.to_string(),
        models: dedupe_models(models),
        warnings,
    })
}

async fn discover_openai(
    client: &Client,
    base_url: &str,
    api_key: &str,
) -> Result<(Vec<DiscoveredModel>, Vec<String>), String> {
    let response = get_json(
        client,
        endpoint(base_url, &["models"])?,
        auth_headers(Dialect::OpenAI, api_key)?,
    )
    .await?;
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "上游模型目录响应格式无效".to_string())?;

    Ok((data.iter().filter_map(openai_model).collect(), Vec::new()))
}

async fn discover_anthropic(
    client: &Client,
    base_url: &str,
    api_key: &str,
) -> Result<(Vec<DiscoveredModel>, Vec<String>), String> {
    let headers = auth_headers(Dialect::Anthropic, api_key)?;
    let mut models = Vec::new();
    let mut warnings = Vec::new();
    let mut after_id: Option<String> = None;
    let mut seen_ids = HashSet::new();

    for page_index in 0..MAX_PAGES {
        let mut url = endpoint(base_url, &["models"])?;
        if let Some(after_id) = &after_id {
            url.query_pairs_mut().append_pair("after_id", after_id);
        }
        let response = get_json(client, url, headers.clone()).await?;
        let data = response
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "上游模型目录响应格式无效".to_string())?;

        models.extend(data.iter().filter_map(|entry| {
            let id = non_empty_string(entry.get("id"))?;
            let name = non_empty_string(entry.get("display_name")).unwrap_or_else(|| id.clone());
            let mut model = DiscoveredModel::default_for(id, name).with_context(first_context(
                entry,
                &["context_window", "context_length", "max_input_tokens"],
            ));
            // Anthropic 的目录会在 `capabilities.image_input.supported` 明确
            // 报告视觉输入能力；没有该字段时不能根据模型名补猜。
            model.supports_vision = nested_supported(entry, "capabilities", "image_input");
            Some(model)
        }));

        if response.get("has_more").and_then(Value::as_bool) != Some(true) {
            return Ok((models, warnings));
        }
        let Some(last_id) = non_empty_string(response.get("last_id")) else {
            warnings.push("模型目录分页缺少继续读取所需的游标，结果可能不完整".into());
            return Ok((models, warnings));
        };
        if !seen_ids.insert(last_id.clone()) {
            warnings.push("模型目录分页游标重复，已停止继续读取".into());
            return Ok((models, warnings));
        }
        if page_index + 1 == MAX_PAGES {
            warnings.push("模型目录分页达到安全上限，结果可能不完整".into());
            return Ok((models, warnings));
        }
        after_id = Some(last_id);
    }

    Ok((models, warnings))
}

async fn discover_gemini(
    client: &Client,
    base_url: &str,
    api_key: &str,
) -> Result<(Vec<DiscoveredModel>, Vec<String>), String> {
    let headers = auth_headers(Dialect::Gemini, api_key)?;
    let mut models = Vec::new();
    let mut warnings = Vec::new();
    let mut page_token: Option<String> = None;
    let mut seen_tokens = HashSet::new();

    for page_index in 0..MAX_PAGES {
        let mut url = endpoint(base_url, &["models"])?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("pageSize", "1000");
            if let Some(token) = &page_token {
                query.append_pair("pageToken", token);
            }
        }

        let response = get_json(client, url, headers.clone()).await?;
        let page_models = response
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| "上游模型目录响应格式无效".to_string())?;
        models.extend(page_models.iter().filter_map(gemini_model));

        let next_page_token = non_empty_string(response.get("nextPageToken"));
        let Some(next_page_token) = next_page_token else {
            return Ok((models, warnings));
        };
        if !seen_tokens.insert(next_page_token.clone()) {
            warnings.push("模型目录分页令牌重复，已停止继续读取".into());
            return Ok((models, warnings));
        }
        if page_index + 1 == MAX_PAGES {
            warnings.push("模型目录分页达到安全上限，结果可能不完整".into());
            return Ok((models, warnings));
        }
        page_token = Some(next_page_token);
    }

    Ok((models, warnings))
}

async fn discover_ollama(
    client: &Client,
    base_url: &str,
    api_key: &str,
) -> Result<(Vec<DiscoveredModel>, Vec<String>), String> {
    let headers = auth_headers(Dialect::Ollama, api_key)?;
    let tags_response = get_json(
        client,
        endpoint(base_url, &["api", "tags"])?,
        headers.clone(),
    )
    .await?;
    let tags = tags_response
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "上游模型目录响应格式无效".to_string())?;

    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    let mut warnings = Vec::new();
    for tag in tags {
        let Some(id) =
            non_empty_string(tag.get("name")).or_else(|| non_empty_string(tag.get("model")))
        else {
            continue;
        };
        if seen.insert(id.clone()) {
            let name = non_empty_string(tag.get("model")).unwrap_or_else(|| id.clone());
            candidates.push((id, name));
        }
    }
    if candidates.len() > MAX_OLLAMA_MODELS {
        candidates.truncate(MAX_OLLAMA_MODELS);
        warnings.push("Ollama 模型数量超过安全上限，未读取全部模型详情".into());
    }

    let show_url = endpoint(base_url, &["api", "show"])?;
    let results = stream::iter(
        candidates
            .into_iter()
            .enumerate()
            .map(|(index, (id, name))| {
                let client = client.clone();
                let headers = headers.clone();
                let show_url = show_url.clone();
                async move {
                    let response = post_json(
                        &client,
                        show_url,
                        headers,
                        json!({ "model": id, "verbose": false }),
                    )
                    .await;
                    (index, id, name, response)
                }
            }),
    )
    .buffer_unordered(OLLAMA_SHOW_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut ordered = Vec::with_capacity(results.len());
    let mut failed_details = 0usize;
    for (index, id, name, response) in results {
        let mut model = DiscoveredModel::default_for(id, name);
        match response {
            Ok(response) => apply_ollama_details(&mut model, &response),
            Err(_) => failed_details += 1,
        }
        ordered.push((index, model));
    }
    ordered.sort_by_key(|(index, _)| *index);
    if failed_details > 0 {
        warnings.push(format!(
            "{} 个 Ollama 模型的详情不可用，已保留为待确认默认值",
            failed_details
        ));
    }

    Ok((
        ordered.into_iter().map(|(_, model)| model).collect(),
        warnings,
    ))
}

fn openai_model(entry: &Value) -> Option<DiscoveredModel> {
    let id = non_empty_string(entry.get("id"))?;
    let name = non_empty_string(entry.get("name")).unwrap_or_else(|| id.clone());
    let mut model = DiscoveredModel::default_for(id, name).with_context(first_context(
        entry,
        &["context_length", "context_window", "max_input_tokens"],
    ));

    if let Some(parameters) = entry.get("supported_parameters").and_then(string_list) {
        model.supports_tools = Some(parameters.iter().any(|parameter| parameter == "tools"));
        if parameters.iter().any(|parameter| parameter == "stream") {
            model.supports_stream = Some(true);
        }
    }
    if let Some(modalities) = entry
        .get("architecture")
        .and_then(|architecture| architecture.get("input_modalities"))
        .and_then(string_list)
    {
        model.supports_vision = Some(
            modalities
                .iter()
                .any(|modality| modality.eq_ignore_ascii_case("image")),
        );
    }
    model.is_free = pricing_is_free(entry.get("pricing"));
    Some(model)
}

fn gemini_model(entry: &Value) -> Option<DiscoveredModel> {
    let id = non_empty_string(entry.get("name"))
        .map(|name| name.strip_prefix("models/").unwrap_or(&name).to_string())
        .or_else(|| non_empty_string(entry.get("baseModelId")))?;
    let name = non_empty_string(entry.get("displayName")).unwrap_or_else(|| id.clone());
    let mut model = DiscoveredModel::default_for(id, name)
        .with_context(context_value(entry.get("inputTokenLimit")));

    if let Some(methods) = entry
        .get("supportedGenerationMethods")
        .and_then(string_list)
    {
        if !methods
            .iter()
            .any(|method| method.eq_ignore_ascii_case("generateContent"))
        {
            // `models.list` 同时会返回 embed、token-count 等资源；它们不能作为
            // 当前聊天 Provider 的候选模型，且不能靠名称猜测用途。
            return None;
        }
        if methods
            .iter()
            .any(|method| method.eq_ignore_ascii_case("streamGenerateContent"))
        {
            model.supports_stream = Some(true);
        }
    }
    Some(model)
}

fn apply_ollama_details(model: &mut DiscoveredModel, response: &Value) {
    if let Some(context_window) = ollama_context(response) {
        model.context_window = context_window;
        model.context_source = ContextSource::Provider;
    }
    if let Some(capabilities) = response.get("capabilities").and_then(string_list) {
        model.supports_tools = Some(
            capabilities
                .iter()
                .any(|capability| capability.eq_ignore_ascii_case("tools")),
        );
        model.supports_vision = Some(
            capabilities
                .iter()
                .any(|capability| capability.eq_ignore_ascii_case("vision")),
        );
    }
}

fn ollama_context(response: &Value) -> Option<i32> {
    response
        .get("parameters")
        .and_then(Value::as_str)
        .and_then(|parameters| {
            parameters.lines().find_map(|line| {
                let mut words = line.split_whitespace();
                (words.next() == Some("num_ctx"))
                    .then(|| words.next())
                    .flatten()
                    .and_then(|value| value.parse::<i64>().ok())
                    .and_then(context_from_i64)
            })
        })
        .or_else(|| {
            response
                .get("model_info")
                .and_then(Value::as_object)
                .and_then(|model_info| {
                    model_info.iter().find_map(|(key, value)| {
                        (key == "context_length" || key.ends_with(".context_length"))
                            .then(|| context_value(Some(value)))
                            .flatten()
                    })
                })
        })
}

fn pricing_is_free(pricing: Option<&Value>) -> Option<bool> {
    let pricing = pricing?.as_object()?;
    // OpenRouter 的免费与否至少需要完整的 prompt/completion 定价。只看到其中
    // 一项为零无法证明另一个方向免费；任何已给出的其他计费项也必须是合法的
    // 非负数，不能在解析时悄悄丢弃。
    let mut values = vec![
        nonnegative_price(pricing.get("prompt")?)?,
        nonnegative_price(pricing.get("completion")?)?,
    ];
    for field in [
        "request",
        "image",
        "web_search",
        "internal_reasoning",
        "input_cache_read",
        "input_cache_write",
    ] {
        if let Some(value) = pricing.get(field) {
            values.push(nonnegative_price(value)?);
        }
    }
    Some(values.into_iter().all(|value| value == 0.0))
}

fn nonnegative_price(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(number) => number.parse::<f64>().ok(),
        _ => None,
    }
    .filter(|number| number.is_finite() && *number >= 0.0)
}

fn context_value(value: Option<&Value>) -> Option<i32> {
    value
        .and_then(Value::as_i64)
        .and_then(context_from_i64)
        .or_else(|| {
            value
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<i64>().ok())
                .and_then(context_from_i64)
        })
}

fn first_context(entry: &Value, fields: &[&str]) -> Option<i32> {
    fields
        .iter()
        .find_map(|field| context_value(entry.get(*field)))
}

fn nested_supported(entry: &Value, parent: &str, field: &str) -> Option<bool> {
    entry
        .get(parent)
        .and_then(|parent| parent.get(field))
        .and_then(|capability| capability.get("supported"))
        .and_then(Value::as_bool)
}

fn context_from_i64(value: i64) -> Option<i32> {
    (1..=i32::MAX as i64)
        .contains(&value)
        .then_some(value as i32)
}

fn string_list(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|values| {
        values
            .iter()
            .filter_map(|value| non_empty_string(Some(value)))
            .collect::<Vec<_>>()
    })
}

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn dedupe_models(models: Vec<DiscoveredModel>) -> Vec<DiscoveredModel> {
    let mut unique = BTreeMap::new();
    for model in models {
        unique
            .entry(model.id.clone())
            .and_modify(|existing: &mut DiscoveredModel| merge_model(existing, &model))
            .or_insert(model);
    }
    unique.into_values().collect()
}

fn merge_model(existing: &mut DiscoveredModel, incoming: &DiscoveredModel) {
    if existing.context_source == ContextSource::Default
        && incoming.context_source == ContextSource::Provider
    {
        existing.context_window = incoming.context_window;
        existing.context_source = incoming.context_source;
    }
    if existing.name == existing.id && incoming.name != incoming.id {
        existing.name = incoming.name.clone();
    }
    if existing.supports_tools.is_none() {
        existing.supports_tools = incoming.supports_tools;
    }
    if existing.supports_vision.is_none() {
        existing.supports_vision = incoming.supports_vision;
    }
    if existing.supports_stream.is_none() {
        existing.supports_stream = incoming.supports_stream;
    }
    if existing.is_free.is_none() {
        existing.is_free = incoming.is_free;
    }
}

fn endpoint(base_url: &str, segments: &[&str]) -> Result<Url, String> {
    let mut url = Url::parse(base_url).map_err(|_| "API 地址无效或不被允许".to_string())?;
    let mut path = url
        .path_segments_mut()
        .map_err(|_| "API 地址不能用于模型目录请求".to_string())?;
    path.pop_if_empty();
    for segment in segments {
        path.push(segment);
    }
    drop(path);
    Ok(url)
}

fn auth_headers(dialect: Dialect, api_key: &str) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

    if dialect == Dialect::Anthropic {
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    }
    if api_key.is_empty() {
        return Ok(headers);
    }

    let header_value = match dialect {
        Dialect::OpenAI | Dialect::Ollama => HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| "API Key 格式无效".to_string())?,
        Dialect::Anthropic | Dialect::Gemini => {
            HeaderValue::from_str(api_key).map_err(|_| "API Key 格式无效".to_string())?
        }
    };
    match dialect {
        Dialect::OpenAI | Dialect::Ollama => {
            headers.insert(AUTHORIZATION, header_value);
        }
        Dialect::Anthropic => {
            headers.insert("x-api-key", header_value);
        }
        Dialect::Gemini => {
            // 使用 API Key 请求头而不是 `?key=`，这样请求 URL、重定向目标和
            // 错误消息都不会携带凭据。
            headers.insert("x-goog-api-key", header_value);
        }
    }
    Ok(headers)
}

async fn get_json(client: &Client, url: Url, headers: HeaderMap) -> Result<Value, String> {
    let response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(request_error)?;
    response_json(response).await
}

async fn post_json(
    client: &Client,
    url: Url,
    headers: HeaderMap,
    body: Value,
) -> Result<Value, String> {
    let response = client
        .post(url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(request_error)?;
    response_json(response).await
}

fn request_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "模型目录请求超时".into()
    } else if error.is_connect() {
        "无法连接模型目录服务".into()
    } else {
        "模型目录请求失败".into()
    }
}

async fn response_json(mut response: reqwest::Response) -> Result<Value, String> {
    if !response.status().is_success() {
        return Err(format!(
            "模型目录服务返回 HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("模型目录响应超过安全大小上限".into());
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "读取模型目录响应失败".to_string())?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("模型目录响应超过安全大小上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "上游模型目录不是有效 JSON".to_string())
}
