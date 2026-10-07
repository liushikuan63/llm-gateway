//! Anthropic Messages API 兼容层。
//!
//! 这一层是「接入 Claude Code」的关键。CC Switch 的做法正是把
//! ANTHROPIC_BASE_URL 指向本地代理，让 Claude Code 以为自己在跟官方 API 说话。
//! 要点（实测血泪）：
//!   1. system 是顶层字段，不是 messages 里的 role=system；
//!   2. 响应里 content 是数组 [{type:"text", text:"..."}]，不是字符串；
//!   3. 流式用 event: xxx 的 SSE，事件类型有
//!      message_start / content_block_start / content_block_delta /
//!      content_block_stop / message_delta / message_stop；
//!   4. Claude Code 会发 thinking 预算，上游不支持时必须整流掉，否则 400。

use serde_json::json;

use crate::domain::{ChatRequest, ChatResponse, Content, Message, Role};

#[derive(Debug, serde::Deserialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<AnMessage>,
    pub system: Option<serde_json::Value>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stop_sequences: Option<Vec<String>>,
    pub stream: Option<bool>,
    pub tools: Option<serde_json::Value>,
    pub tool_choice: Option<serde_json::Value>,
    pub thinking: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AnMessage {
    pub role: String,
    pub content: serde_json::Value,
}

impl AnthropicRequest {
    pub fn to_internal(self) -> Result<ChatRequest, crate::error::GatewayError> {
        let mut msgs = Vec::new();

        if let Some(sys) = self.system {
            let text = match sys {
                serde_json::Value::String(s) => s,
                serde_json::Value::Array(blocks) => blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => other.to_string(),
            };
            if !text.is_empty() {
                msgs.push(Message::system(text));
            }
        }

        for m in self.messages {
            let role = match m.role.as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                other => {
                    return Err(crate::error::GatewayError::Protocol(format!(
                        "anthropic: unknown role `{other}`"
                    )))
                }
            };

            if matches!(role, Role::User) {
                // 一条 Anthropic user 消息可以同时携带多条 tool_result，以及文本或
                // 图片块。IR 需要把 tool_result 拆成专用 Tool 消息，但块的顺序不能
                // 丢失，否则并行工具调用回填时会缺结果或改变上下文语义。
                msgs.extend(user_content_to_messages(&m.content)?);
                continue;
            }

            msgs.push(Message {
                role,
                content: parse_an_content(&m.content),
                tool_calls: extract_tool_use(&m.content),
                tool_call_id: None,
                name: None,
            });
        }

        Ok(ChatRequest {
            model: self.model,
            messages: msgs,
            temperature: self.temperature,
            top_p: self.top_p,
            max_tokens: self.max_tokens,
            stop: self.stop_sequences,
            stream: self.stream.unwrap_or(false),
            tools: self.tools,
            tool_choice: self.tool_choice,
            thinking: self.thinking,
            extra: self.extra,
        })
    }
}

fn user_content_to_messages(
    content: &serde_json::Value,
) -> Result<Vec<Message>, crate::error::GatewayError> {
    let Some(blocks) = content.as_array() else {
        return Ok(vec![user_message_from_content(content)]);
    };

    if !blocks
        .iter()
        .any(|block| block.get("type").and_then(|kind| kind.as_str()) == Some("tool_result"))
    {
        return Ok(vec![user_message_from_content(content)]);
    }

    let mut messages = Vec::new();
    let mut regular_blocks = Vec::new();

    for block in blocks {
        match block.get("type").and_then(|kind| kind.as_str()) {
            Some("tool_result") => {
                flush_regular_user_blocks(&mut messages, &mut regular_blocks);
                messages.push(tool_result_to_message(block)?);
            }
            Some("text") | Some("image") => regular_blocks.push(block.clone()),
            Some(kind) => {
                return Err(crate::error::GatewayError::Protocol(format!(
                    "anthropic: unsupported user content block `{kind}` alongside tool_result"
                )))
            }
            None => {
                return Err(crate::error::GatewayError::Protocol(
                    "anthropic: user content block is missing type alongside tool_result".into(),
                ))
            }
        }
    }
    flush_regular_user_blocks(&mut messages, &mut regular_blocks);

    Ok(messages)
}

fn flush_regular_user_blocks(messages: &mut Vec<Message>, blocks: &mut Vec<serde_json::Value>) {
    if blocks.is_empty() {
        return;
    }
    let content = serde_json::Value::Array(std::mem::take(blocks));
    messages.push(user_message_from_content(&content));
}

fn user_message_from_content(content: &serde_json::Value) -> Message {
    Message {
        role: Role::User,
        content: parse_an_content(content),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

fn tool_result_to_message(
    block: &serde_json::Value,
) -> Result<Message, crate::error::GatewayError> {
    let tool_call_id = block
        .get("tool_use_id")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            crate::error::GatewayError::Protocol(
                "anthropic: tool_result is missing tool_use_id".into(),
            )
        })?
        .to_string();
    let content = match block.get("content") {
        Some(serde_json::Value::Null) | None => Content::Text(String::new()),
        Some(content) => parse_an_content(content),
    };

    Ok(Message {
        role: Role::Tool,
        content,
        tool_calls: None,
        tool_call_id: Some(tool_call_id),
        name: None,
    })
}

fn parse_an_content(v: &serde_json::Value) -> Content {
    match v {
        serde_json::Value::String(s) => Content::Text(s.clone()),
        serde_json::Value::Array(blocks) => {
            let mut texts = Vec::new();
            let mut parts = Vec::new();
            for b in blocks {
                match b.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                            texts.push(t.to_string());
                        }
                    }
                    Some("image") => {
                        if let Some(src) = b.get("source") {
                            let data = src.get("data").and_then(|d| d.as_str()).unwrap_or("");
                            let mt = src
                                .get("media_type")
                                .and_then(|d| d.as_str())
                                .unwrap_or("image/png");
                            parts.push(crate::domain::Part::ImageUrl {
                                image_url: crate::domain::ImageUrl {
                                    url: format!("data:{mt};base64,{data}"),
                                    detail: None,
                                },
                            });
                        }
                    }
                    _ => {}
                }
            }
            if parts.is_empty() {
                Content::Text(texts.join(""))
            } else {
                parts.insert(
                    0,
                    crate::domain::Part::Text {
                        text: texts.join(""),
                    },
                );
                Content::Parts(parts)
            }
        }
        other => Content::Text(other.to_string()),
    }
}

fn extract_tool_use(v: &serde_json::Value) -> Option<Vec<crate::domain::ToolCall>> {
    let arr = v.as_array()?;
    let out: Vec<crate::domain::ToolCall> = arr
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|b| {
            Some(crate::domain::ToolCall {
                id: b.get("id")?.as_str()?.to_string(),
                kind: "function".into(),
                function: crate::domain::FunctionCall {
                    name: b.get("name")?.as_str()?.to_string(),
                    arguments: serde_json::to_string(b.get("input").unwrap_or(&json!({}))).ok()?,
                },
            })
        })
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 内部统一响应 -> Anthropic 响应体
pub fn messages_response(resp: &ChatResponse, model: &str) -> serde_json::Value {
    let mut content = Vec::new();
    if !resp.content.is_empty() {
        content.push(json!({ "type": "text", "text": resp.content }));
    }
    if let Some(tcs) = &resp.tool_calls {
        for tc in tcs {
            let input: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.function.name,
                "input": input,
            }));
        }
    }
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "" }));
    }

    let stop_reason = match resp.finish_reason.as_deref() {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        _ => "end_turn",
    };
    let u = resp.usage.clone().unwrap_or_default();
    let mut usage = json!({
        "input_tokens": u.normal_input_tokens(),
        "output_tokens": u.completion_tokens,
    });
    if u.cache_read_tokens > 0 {
        usage["cache_read_input_tokens"] = json!(u.cache_read_tokens);
    }
    if u.cache_creation_tokens > 0 {
        usage["cache_creation_input_tokens"] = json!(u.cache_creation_tokens);
    }

    json!({
        "id": resp.id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage,
    })
}

/// Anthropic 上游响应 -> 内部统一响应
pub fn anthropic_to_internal(v: &serde_json::Value) -> crate::domain::ChatResponse {
    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    let mut text = String::new();
    let mut tool_calls: Vec<crate::domain::ToolCall> = Vec::new();

    for b in content {
        match b.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                if let (Some(id), Some(name)) = (
                    b.get("id").and_then(|x| x.as_str()),
                    b.get("name").and_then(|x| x.as_str()),
                ) {
                    let args = serde_json::to_string(b.get("input").unwrap_or(&json!({})))
                        .unwrap_or_else(|_| "{}".into());
                    tool_calls.push(crate::domain::ToolCall {
                        id: id.to_string(),
                        kind: "function".into(),
                        function: crate::domain::FunctionCall {
                            name: name.to_string(),
                            arguments: args,
                        },
                    });
                }
            }
            _ => {}
        }
    }

    let u = v.get("usage").cloned().unwrap_or(json!({}));

    crate::domain::ChatResponse {
        // 普通 API 上游没有「传输」这个概念（A6 判据 1 只对账号型上游有意义）。
        transport: None,
        id: v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        model: v
            .get("model")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        content: text,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        finish_reason: match v.get("stop_reason").and_then(|x| x.as_str()) {
            Some("tool_use") => Some("tool_calls".into()),
            Some("max_tokens") => Some("length".into()),
            Some(_) => Some("stop".into()),
            None => None,
        },
        usage: Some(crate::domain::Usage::from_anthropic(&u)),
    }
}

/// 内部消息 -> OpenAI 格式（用于把 Anthropic 客户端请求转成 OpenAI 上游格式）
pub fn internal_to_openai_messages(req: &ChatRequest) -> Vec<serde_json::Value> {
    use crate::protocol::convert::message_to_openai;
    req.messages.iter().map(message_to_openai).collect()
}

pub fn internal_to_anthropic_body(req: &ChatRequest, model: &str) -> serde_json::Value {
    use crate::protocol::convert::message_to_anthropic;
    let system = req
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::System))
        .map(Message::content_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut messages = Vec::new();
    for message in req
        .messages
        .iter()
        .filter(|message| !matches!(message.role, Role::System))
    {
        let next = if matches!(message.role, Role::Tool) {
            tool_result_to_anthropic_message(message)
        } else {
            message_to_anthropic(message)
        };
        push_or_merge_anthropic_message(&mut messages, next);
    }

    // Anthropic 要求会话从 user 开始。客户端可能只回传上一轮 assistant，
    // 此时补一个最小占位 user 消息，和 Node 金标保持一致。
    if messages
        .first()
        .and_then(|message| message.get("role"))
        .and_then(|role| role.as_str())
        == Some("assistant")
    {
        messages.insert(
            0,
            json!({ "role": "user", "content": [{ "type": "text", "text": "(continue)" }] }),
        );
    }

    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": req.max_tokens.unwrap_or(4096),
    });
    if !system.is_empty() {
        // Anthropic 接受字符串或 block 数组；使用字符串可与入站的 IR 语义严格往返，
        // 也避免此前把整个 JSON block 数组序列化进文本字段的错误。
        body["system"] = json!(system);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    if let Some(s) = &req.stop {
        body["stop_sequences"] = json!(s);
    }
    if let Some(t) = &req.tools {
        // OpenAI tools -> Anthropic tools
        let tools = t
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|t| {
                if t.get("type").and_then(|kind| kind.as_str()) != Some("function") {
                    return t;
                }
                let f = t.get("function").cloned().unwrap_or(json!({}));
                json!({
                    "name": f.get("name").cloned().unwrap_or(json!("")),
                    "description": f.get("description").cloned().unwrap_or(json!("")),
                    "input_schema": f.get("parameters").cloned().unwrap_or(json!({"type":"object"})),
                })
            })
            .collect::<Vec<_>>();
        body["tools"] = json!(tools);
    }
    if let Some(tool_choice) = &req.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }
    if let Some(thinking) = &req.thinking {
        body["thinking"] = thinking.clone();
    }
    body
}

/// Anthropic 要求消息角色交替。连续的 OpenAI `tool` 消息在其协议中必须合并
/// 到同一个 `role: user` 的 content 数组，而不能生成连续的 user 消息。
fn push_or_merge_anthropic_message(messages: &mut Vec<serde_json::Value>, next: serde_json::Value) {
    let next_role = next.get("role").and_then(|role| role.as_str());
    let next_content = next
        .get("content")
        .and_then(|content| content.as_array())
        .cloned();

    if let (Some(last), Some(next_role), Some(next_content)) =
        (messages.last_mut(), next_role, next_content)
    {
        if last.get("role").and_then(|role| role.as_str()) == Some(next_role) {
            if let Some(last_content) = last
                .get_mut("content")
                .and_then(serde_json::Value::as_array_mut)
            {
                last_content.extend(next_content);
                return;
            }
        }
    }
    messages.push(next);
}

/// `convert::message_to_anthropic` 的普通工具结果是文本化路径；这里保留 IR 中
/// 的图片块，避免从 Anthropic 入站后又转回 Anthropic 上游时丢失内容。
fn tool_result_to_anthropic_message(message: &Message) -> serde_json::Value {
    let Some(tool_use_id) = message.tool_call_id.as_deref() else {
        return crate::protocol::convert::message_to_anthropic(message);
    };

    let content = match &message.content {
        Content::Text(text) => json!(text),
        Content::Parts(parts) => json!(parts
            .iter()
            .filter_map(|part| match part {
                crate::domain::Part::Text { text } => {
                    Some(json!({ "type": "text", "text": text }))
                }
                crate::domain::Part::ImageUrl { image_url } => {
                    if let Some(rest) = image_url.url.strip_prefix("data:") {
                        let (media_type, data) =
                            rest.split_once(";base64,").unwrap_or(("image/png", ""));
                        Some(json!({
                            "type": "image",
                            "source": { "type": "base64", "media_type": media_type, "data": data }
                        }))
                    } else {
                        Some(json!({
                            "type": "image",
                            "source": { "type": "url", "url": image_url.url }
                        }))
                    }
                }
                // Anthropic tool_result 不支持 OpenAI 的 input_audio / video_url 块；
                // 路由层已按方言承载能力拦截这类请求，这里只保留协议边界标记。
                crate::domain::Part::InputAudio { .. } => None,
                crate::domain::Part::VideoUrl { .. } => None,
            })
            .collect::<Vec<_>>()),
    };

    json!({
        "role": "user",
        "content": [{
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
        }]
    })
}
