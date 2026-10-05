//! 模型目录：从各运行时的响应里读出模型与能力。
//!
//! 这一层全是**纯函数**——输入 JSON、输出结构体，不碰网络。能力映射的全部规则
//! 集中在这里，才能被单测逐条打穿。
//!
//! ## 保守优先
//!
//! 取不到能力信息时一律 `false`。宁可让一个能看图的模型不参与图片路由（用户会看到
//! 明确的「没有可用模型」错误），也不要标错导致把图片发给看不懂的模型——后者是
//! 静默丢内容，比报错严重得多。

use serde::{Deserialize, Serialize};

use crate::domain::{LocalMeta, ModelRef, ModelType};

/// 默认上下文窗口。取不到元数据时用它，并让界面明确标记「待确认」。
pub const DEFAULT_LOCAL_CONTEXT: i32 = 32_768;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalModelInfo {
    /// 上游真实模型名，含 tag。Ollama 的 `gemma4:12b-it-q4_K_M` 整体保留。
    pub upstream: String,
    /// 对外模型名，默认与 upstream 相同，可由用户改写
    pub alias: String,
    pub context_window: i32,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_audio: bool,
    pub supports_video: bool,
    pub supports_thinking: bool,
    pub supports_stream: bool,
    pub model_type: ModelType,
    pub meta: LocalMeta,
}

fn cap(capabilities: &[String], name: &str) -> bool {
    capabilities.iter().any(|c| c.eq_ignore_ascii_case(name))
}

#[derive(Debug, Deserialize)]
struct OllamaTag {
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    size: Option<i64>,
    #[serde(default)]
    details: Option<OllamaDetails>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct OllamaDetails {
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    parameter_size: Option<String>,
    #[serde(default)]
    quantization_level: Option<String>,
    #[serde(default)]
    context_length: Option<i64>,
}

/// 解析 Ollama `/api/tags`。
///
/// 实测样本（Ollama 0.35.1）：
/// ```json
/// { "models": [ { "name": "gemma4:12b-it-q4_K_M", "size": 8021618941,
///     "details": { "family": "gemma4", "parameter_size": "11.9B",
///                  "quantization_level": "Q4_K_M", "context_length": 262144 },
///     "capabilities": ["completion","vision","audio","tools","thinking"] } ] }
/// ```
pub fn ollama_models_from_tags(json: &serde_json::Value) -> Vec<LocalModelInfo> {
    let Some(models) = json.get("models").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(models.len());
    for raw in models {
        let Ok(tag) = serde_json::from_value::<OllamaTag>(raw.clone()) else {
            continue;
        };
        // `name` 是完整名（含 tag），`model` 通常是 digest。两者都在时优先 name。
        let upstream = if !tag.name.trim().is_empty() {
            tag.name.trim().to_owned()
        } else {
            tag.model.trim().to_owned()
        };
        if upstream.is_empty() {
            continue;
        }
        let details = tag.details.unwrap_or_default();
        let capabilities = tag.capabilities.unwrap_or_default();
        out.push(LocalModelInfo {
            alias: upstream.clone(),
            upstream,
            context_window: details
                .context_length
                .filter(|len| *len > 0)
                .map(|len| len.min(i32::MAX as i64) as i32)
                .unwrap_or(DEFAULT_LOCAL_CONTEXT),
            supports_tools: cap(&capabilities, "tools"),
            supports_vision: cap(&capabilities, "vision"),
            supports_audio: cap(&capabilities, "audio"),
            // 本地运行时目前都没有视频输入面，一律 false。
            supports_video: false,
            supports_thinking: cap(&capabilities, "thinking"),
            supports_stream: cap(&capabilities, "completion") || cap(&capabilities, "chat"),
            model_type: if cap(&capabilities, "embedding") {
                ModelType::Embedding
            } else {
                ModelType::Chat
            },
            // int8 量化的大模型尺寸会超过 i32，用 i64 装；负数或缺失直接丢掉，
            // 不让一个坏数字进 UI。
            meta: LocalMeta {
                runtime: "ollama".into(),
                family: details.family,
                parameter_size: details.parameter_size,
                quantization: details.quantization_level,
                disk_bytes: tag.size.filter(|size| *size >= 0),
                capabilities,
            },
        });
    }
    out
}

/// 解析 OpenAI 兼容 `/v1/models`：`{"data":[{"id":"...","object":"model"}]}`。
///
/// 这类面不暴露能力元数据，所以**所有能力位都是 false**。这不是偷懒，是刻意的：
/// 猜错能力的后果是静默丢内容，比明确报「没有支持该模态的模型」糟得多。
pub fn openai_models_from_list(json: &serde_json::Value) -> Vec<LocalModelInfo> {
    let Some(data) = json.get("data").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(data.len());
    for raw in data {
        let id = raw
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| raw.get("name").and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim()
            .to_owned();
        if id.is_empty() {
            continue;
        }
        let context_window = raw
            .get("context_length")
            .or_else(|| raw.get("max_context_length"))
            .and_then(|v| v.as_i64())
            .filter(|len| *len > 0)
            .map(|len| len.min(i32::MAX as i64) as i32)
            .unwrap_or(DEFAULT_LOCAL_CONTEXT);
        out.push(LocalModelInfo {
            alias: id.clone(),
            upstream: id.clone(),
            context_window,
            supports_tools: false,
            supports_vision: false,
            supports_audio: false,
            supports_video: false,
            supports_thinking: false,
            // OpenAI 兼容面都支持流式；不支持的话退化成非流式也能跑，只是慢。
            supports_stream: true,
            model_type: ModelType::Chat,
            meta: LocalMeta {
                runtime: String::new(),
                family: None,
                parameter_size: None,
                quantization: None,
                disk_bytes: None,
                capabilities: Vec::new(),
            },
        });
    }
    out
}

/// 转成网关内部的模型记录。
pub fn to_model_ref(info: &LocalModelInfo) -> ModelRef {
    ModelRef {
        alias: info.alias.clone(),
        upstream: info.upstream.clone(),
        context_window: info.context_window,
        supports_tools: info.supports_tools,
        supports_vision: info.supports_vision,
        supports_audio: info.supports_audio,
        supports_video: info.supports_video,
        supports_thinking: info.supports_thinking,
        supports_stream: info.supports_stream,
        model_type: info.model_type,
        upstream_path: None,
        price: None,
        overrides: None,
        local: Some(info.meta.clone()),
        enabled: true,
    }
}

/// 按 upstream 找出目录里的一条。
pub fn find_by_upstream<'a>(
    models: &'a [LocalModelInfo],
    upstream: &str,
) -> Option<&'a LocalModelInfo> {
    models.iter().find(|m| m.upstream == upstream)
}
