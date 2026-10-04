//! OpenAI 兼容层。对外我们完整实现 /v1/* 表面，
//! 因为它是事实标准：OpenAI SDK、LangChain、LlamaIndex、Continue、
//! Cursor、Codex CLI、绝大多数自建应用都先认这套。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::domain::{ChatRequest, ChatResponse, Content, Message, Part, Role, Usage};

/* -------------------------- 对外请求（入站解析） -------------------------- */

/// 宽松解析：未知字段保留在 extra，避免客户端新特性被网关吃掉。
/// 这是网关最容易踩的坑之一 —— 严格 struct 会让新增参数静默丢失。
#[derive(Debug, Deserialize)]
pub struct OaChatRequest {
    pub model: String,
    pub messages: Vec<OaMessage>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    pub max_completion_tokens: Option<u32>,
    pub stop: Option<serde_json::Value>,
    pub stream: Option<bool>,
    pub tools: Option<serde_json::Value>,
    pub tool_choice: Option<serde_json::Value>,
    pub thinking: Option<serde_json::Value>,
    pub reasoning: Option<serde_json::Value>,
    pub user: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OaMessage {
    pub role: String,
    pub content: Option<serde_json::Value>,
    pub name: Option<String>,
    pub tool_calls: Option<Vec<serde_json::Value>>,
    pub tool_call_id: Option<String>,
}

impl OaChatRequest {
    pub fn to_internal(self) -> Result<ChatRequest, crate::error::GatewayError> {
        let mut msgs = Vec::with_capacity(self.messages.len());
        for m in self.messages {
            msgs.push(convert_message(m)?);
        }

        // `user` 是 OpenAI 用于标识终端用户/租户的标准字段。内部会话层以
        // 它作为 X-Session-Id 之后的第二优先级，因此必须保留在 IR 的 extra，
        // 而不是因其已被显式反序列化就悄悄丢弃。
        let mut extra = self.extra;
        if let Some(user) = self.user {
            extra.insert("user".into(), serde_json::Value::String(user));
        }

        let max_tokens = self.max_tokens.or(self.max_completion_tokens);
        let stop = match self.stop {
            Some(serde_json::Value::String(s)) => Some(vec![s]),
            Some(serde_json::Value::Array(a)) => Some(
                a.into_iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
            ),
            _ => None,
        };

        Ok(ChatRequest {
            model: self.model,
            messages: msgs,
            temperature: self.temperature,
            top_p: self.top_p,
            max_tokens,
            stop,
            stream: self.stream.unwrap_or(false),
            tools: self.tools,
            tool_choice: self.tool_choice,
            thinking: self.thinking.or(self.reasoning),
            extra,
        })
    }
}

fn convert_message(m: OaMessage) -> Result<Message, crate::error::GatewayError> {
    let role = match m.role.as_str() {
        "system" | "developer" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" | "function" => Role::Tool,
        other => {
            return Err(crate::error::GatewayError::Protocol(format!(
                "unknown role `{other}`"
            )))
        }
    };

    let content = match m.content {
        None => Content::Text(String::new()),
        Some(serde_json::Value::String(s)) => Content::Text(s),
        Some(serde_json::Value::Array(parts)) => {
            let mut ps = Vec::new();
            for p in parts {
                let t = p.get("type").and_then(|v| v.as_str()).unwrap_or("text");
                match t {
                    "text" => ps.push(Part::Text {
                        text: p.get("text").and_then(|v| v.as_str()).unwrap_or("").into(),
                    }),
                    "image_url" => ps.push(Part::ImageUrl {
                        image_url: serde_json::from_value(
                            p.get("image_url").cloned().unwrap_or(json!({})),
                        )
                        .unwrap_or(crate::domain::ImageUrl {
                            url: String::new(),
                            detail: None,
                        }),
                    }),
                    "input_audio" => ps.push(Part::InputAudio {
                        input_audio: p.get("input_audio").cloned().unwrap_or(json!({})),
                    }),
                    "video_url" => ps.push(Part::VideoUrl {
                        video_url: serde_json::from_value(
                            p.get("video_url").cloned().unwrap_or(json!({})),
                        )
                        .unwrap_or(crate::domain::ImageUrl {
                            url: String::new(),
                            detail: None,
                        }),
                    }),
                    _ => {}
                }
            }
            Content::Parts(ps)
        }
        Some(other) => Content::Text(other.to_string()),
    };

    let tool_calls = m
        .tool_calls
        .as_deref()
        .and_then(crate::protocol::convert::tool_calls_from_openai);

    Ok(Message {
        role,
        content,
        tool_calls,
        tool_call_id: m.tool_call_id,
        name: m.name,
    })
}

/* ------------------------- 上游请求（出站构造） ------------------------- */

/// 内部请求 -> OpenAI 上游请求体。与入站的 `to_internal` 对称。
///
/// 两个坑：
///  1) max_tokens / max_completion_tokens 新旧字段差异：新模型只认后者，
///     老模型只认前者。两个都发最稳，但 o1 系列会因收到 max_tokens 报错，
///     所以对 o1/o3 前缀模型只发 max_completion_tokens。
///  2) stream_options.include_usage：流式场景下不带这个参数，
///     OpenAI 兼容上游普遍不返回 usage，网关就无法统计 token。
pub fn to_upstream_body(req: &ChatRequest, model: &str) -> serde_json::Value {
    use crate::protocol::convert::message_to_openai;

    let messages: Vec<serde_json::Value> = req.messages.iter().map(message_to_openai).collect();
    let is_o_series = model.starts_with("o1") || model.starts_with("o3");

    let mut body = json!({
        "model": model,
        "messages": messages,
    });

    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    if let Some(mt) = req.max_tokens {
        if is_o_series {
            body["max_completion_tokens"] = json!(mt);
        } else {
            body["max_tokens"] = json!(mt);
        }
    }
    if let Some(stop) = &req.stop {
        body["stop"] = json!(stop);
    }
    if let Some(tools) = &req.tools {
        body["tools"] = tools.clone();
    }
    if let Some(tc) = &req.tool_choice {
        body["tool_choice"] = tc.clone();
    }
    if let Some(th) = &req.thinking {
        body["thinking"] = th.clone();
    }
    if req.stream {
        body["stream_options"] = json!({ "include_usage": true });
    }

    // 未识别字段原样透传：网关不该成为新特性的瓶颈
    for (k, v) in &req.extra {
        if !body.as_object().unwrap().contains_key(k) {
            body[k] = v.clone();
        }
    }

    body
}

/* ------------------------- 对外响应（出站构造） ------------------------- */

pub fn chat_completion_response(
    req_id: &str,
    model: &str,
    resp: &ChatResponse,
) -> serde_json::Value {
    let response_id = if req_id.trim().is_empty() {
        format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
    } else {
        req_id.to_string()
    };
    let mut msg = json!({
        "role": "assistant",
        "content": resp.content,
    });
    if let Some(tool_calls) = &resp.tool_calls {
        msg["tool_calls"] = json!(crate::protocol::convert::tool_calls_to_openai(tool_calls));
    }
    json!({
        "id": response_id,
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": msg,
            "finish_reason": resp.finish_reason.clone().unwrap_or_else(|| "stop".into()),
        }],
        "usage": usage_json(&resp.usage.clone().unwrap_or_default()),
    })
}

pub fn usage_json(u: &Usage) -> serde_json::Value {
    let mut usage = json!({
        "prompt_tokens": u.prompt_tokens,
        "completion_tokens": u.completion_tokens,
        "total_tokens": u.total_tokens,
    });
    if u.cache_read_tokens > 0 || u.cache_creation_tokens > 0 {
        let mut details = serde_json::Map::new();
        if u.cache_read_tokens > 0 {
            details.insert("cached_tokens".into(), json!(u.cache_read_tokens));
        }
        if u.cache_creation_tokens > 0 {
            details.insert("cache_write_tokens".into(), json!(u.cache_creation_tokens));
        }
        usage["prompt_tokens_details"] = serde_json::Value::Object(details);
    }
    usage
}

/// 流式 chunk 的 JSON payload。
///
/// 这里**不**写入 `data: ` 或空行。调用方 `proxy/server.rs` 统一完成 SSE
/// framing；若这里提前包一层会产生 `data: data: ...` 并让 OpenAI 客户端无法
/// 解析 JSON。
pub fn sse_chunk(
    req_id: &str,
    model: &str,
    delta: &serde_json::Value,
    finish: Option<&str>,
) -> String {
    let mut choice = json!({ "index": 0, "delta": delta });
    if let Some(f) = finish {
        choice["finish_reason"] = json!(f);
    }
    let payload = json!({
        "id": req_id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [choice],
    });
    payload.to_string()
}

/// Anthropic 流式事件（供 /v1/messages 使用）
pub fn anthropic_sse(event: &str, data: &serde_json::Value) -> String {
    format!("event: {}\ndata: {}\n\n", event, data)
}

pub const SSE_DONE: &str = "data: [DONE]\n\n";
