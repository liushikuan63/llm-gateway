use serde::{Deserialize, Serialize};

/// 一条消息。网关内部统一用这个结构；
/// OpenAI / Anthropic / Gemini 三种方言都在 protocol/convert.rs 里与它互转。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Content,
    /// 工具调用（OpenAI 风格，作为中间表示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 多模态内容：文本 + 图片/音频/视频。FreeLLMAPI 早期只支持纯文本，本方案一开始就留好位。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Part>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    ImageUrl {
        image_url: ImageUrl,
    },
    InputAudio {
        input_audio: serde_json::Value,
    },
    /// 视频输入。OpenAI 兼容阵营用 `video_url` 表达，结构同图片。
    VideoUrl {
        video_url: ImageUrl,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

impl Message {
    pub fn system(s: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Content::Text(s.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Content::Text(s.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Text(s.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// 取纯文本表示（多模态内容只保留文本部分）。
    /// 落库、指纹计算、摘要输入都用它 —— 这些场景不需要图片二进制。
    pub fn content_text(&self) -> String {
        match &self.content {
            Content::Text(t) => t.clone(),
            Content::Parts(ps) => ps
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// 粗略 token 估算。真计费用上游返回的 usage；
    /// 这里只用于「是否触发压缩」的判定，误差可接受。
    /// 图片/音频/视频按保守的固定值计入，避免把多模态上下文当纯文本低估。
    pub fn approx_tokens(&self) -> u32 {
        let n = match &self.content {
            Content::Text(t) => t.chars().count(),
            Content::Parts(ps) => ps
                .iter()
                .map(|p| match p {
                    Part::Text { text } => text.chars().count(),
                    Part::ImageUrl { .. } | Part::VideoUrl { .. } => 800,
                    // 音频按 1 秒 ≈ 32 token 的保守口径计，base64 数据不按字符数算。
                    Part::InputAudio { .. } => 3_200,
                })
                .sum(),
        };
        let extra = self
            .tool_calls
            .as_ref()
            .map(|ts| {
                ts.iter()
                    .map(|t| t.function.arguments.chars().count() + 20)
                    .sum::<usize>()
            })
            .unwrap_or(0);
        // 中文按 1 char ≈ 0.7 token，英文按 4 char ≈ 1 token，取折中 3
        ((n + extra) as f32 / 3.0).ceil() as u32 + 4
    }
}

/// 一次对外请求的内部统一表示（协议无关）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    /// "auto" 表示交给路由器挑
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// 高阶：thinking / reasoning 预算（Claude Code v2.1.97+ 会发）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
    /// 其他未识别字段原样透传，保证新特性不会因为网关丢字段而失效
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// 一次上游响应的内部统一表示
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// 已包含在 `prompt_tokens` 内的缓存命中 token。
    #[serde(default)]
    pub cache_read_tokens: u32,
    /// 已包含在 `prompt_tokens` 内的缓存创建 token。
    #[serde(default)]
    pub cache_creation_tokens: u32,
}

impl Usage {
    /// Anthropic 把普通输入、缓存命中与缓存创建拆开返回；其它方言通常直接给出
    /// 包含缓存的总输入。对外输出 Anthropic 口径时需要恢复普通输入部分。
    pub fn normal_input_tokens(&self) -> u32 {
        let cached = self
            .cache_read_tokens
            .saturating_add(self.cache_creation_tokens)
            .min(self.prompt_tokens);
        self.prompt_tokens.saturating_sub(cached)
    }

    pub fn from_openai(value: &serde_json::Value) -> Self {
        let prompt_tokens = json_u32(value.get("prompt_tokens"));
        let completion_tokens = json_u32(value.get("completion_tokens"));
        let details = value
            .get("prompt_tokens_details")
            .or_else(|| value.get("input_tokens_details"));
        let cache_read_tokens = details
            .and_then(|details| details.get("cached_tokens"))
            .or_else(|| value.get("cache_read_input_tokens"))
            .or_else(|| value.get("prompt_cache_hit_tokens"))
            .map(|value| json_u32(Some(value)))
            .unwrap_or(0);
        let cache_creation_tokens = details
            .and_then(|details| details.get("cache_write_tokens"))
            .or_else(|| details.and_then(|details| details.get("cache_creation_tokens")))
            .or_else(|| value.get("cache_creation_input_tokens"))
            .or_else(|| value.get("prompt_cache_write_tokens"))
            .map(|value| json_u32(Some(value)))
            .unwrap_or(0);
        let total_tokens = json_u32(value.get("total_tokens"))
            .max(prompt_tokens.saturating_add(completion_tokens));
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        }
    }

    pub fn from_anthropic(value: &serde_json::Value) -> Self {
        let input_tokens = json_u32(value.get("input_tokens"));
        let cache_read_tokens = json_u32(value.get("cache_read_input_tokens"));
        let cache_creation_tokens = json_u32(value.get("cache_creation_input_tokens"));
        let prompt_tokens = input_tokens
            .saturating_add(cache_read_tokens)
            .saturating_add(cache_creation_tokens);
        let completion_tokens = json_u32(value.get("output_tokens"));
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
            cache_read_tokens,
            cache_creation_tokens,
        }
    }

    pub fn from_gemini(value: &serde_json::Value) -> Self {
        let prompt_tokens = json_u32(value.get("promptTokenCount"));
        let completion_tokens = json_u32(value.get("candidatesTokenCount"));
        let cache_read_tokens = json_u32(value.get("cachedContentTokenCount"));
        let cache_creation_tokens = json_u32(value.get("cacheCreationTokenCount"));
        let total_tokens = json_u32(value.get("totalTokenCount"))
            .max(prompt_tokens.saturating_add(completion_tokens));
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        }
    }
}

fn json_u32(value: Option<&serde_json::Value>) -> u32 {
    value.and_then(serde_json::Value::as_u64).unwrap_or(0) as u32
}
