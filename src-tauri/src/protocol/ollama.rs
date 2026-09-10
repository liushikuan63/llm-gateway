//! Ollama 原生协议（/api/chat）。
//! 存在意义：Zed、JetBrains AI 等工具只认 Ollama 线，
//! 提供一层 Ollama 仿真即可零改造接入（对齐 FreeLLMAPI 的 opt-in Ollama emulation）。

use crate::domain::{ChatRequest, ChatResponse, Content, FunctionCall, ToolCall, Usage};
use crate::error::GatewayError;
use serde_json::json;

/// Ollama `/api/chat` 请求 -> 内部请求。
///
/// Ollama 将生成参数放在 `options`，而工具定义在顶层。入口若只复用 messages
/// 会让 tools 在进入路由器前静默消失，也无法按 `supports_tools` 做硬约束过滤。
pub fn ollama_request_to_internal(body: &serde_json::Value) -> Result<ChatRequest, GatewayError> {
    let options = body.get("options").and_then(serde_json::Value::as_object);
    let mut openai = json!({
        "model": body.get("model").and_then(serde_json::Value::as_str).unwrap_or("auto"),
        "messages": normalize_ollama_messages(body.get("messages")),
        "stream": body.get("stream").cloned().unwrap_or(serde_json::Value::Bool(false)),
    });

    copy_option(
        &mut openai,
        "temperature",
        body.get("temperature")
            .or_else(|| options.and_then(|options| options.get("temperature"))),
    );
    copy_option(
        &mut openai,
        "top_p",
        body.get("top_p")
            .or_else(|| options.and_then(|options| options.get("top_p"))),
    );
    copy_option(
        &mut openai,
        "max_tokens",
        body.get("num_predict")
            .or_else(|| options.and_then(|options| options.get("num_predict"))),
    );
    copy_option(
        &mut openai,
        "stop",
        body.get("stop")
            .or_else(|| options.and_then(|options| options.get("stop"))),
    );
    copy_option(&mut openai, "tools", body.get("tools"));
    copy_option(&mut openai, "tool_choice", body.get("tool_choice"));

    let request: crate::protocol::openai::OaChatRequest = serde_json::from_value(openai)
        .map_err(|error| GatewayError::Protocol(format!("Ollama 请求体解析失败: {error}")))?;
    request.to_internal()
}

pub fn to_ollama_body(req: &ChatRequest, model: &str) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = req
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                crate::domain::Role::System => "system",
                crate::domain::Role::User => "user",
                crate::domain::Role::Assistant => "assistant",
                crate::domain::Role::Tool => "tool",
            };
            let text = match &m.content {
                Content::Text(t) => t.clone(),
                Content::Parts(ps) => ps
                    .iter()
                    .filter_map(|p| match p {
                        crate::domain::Part::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            };
            let mut message = json!({ "role": role, "content": text });
            if let Some(tool_calls) = &m.tool_calls {
                message["tool_calls"] = json!(tool_calls_to_ollama(tool_calls));
            }
            message
        })
        .collect();

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": req.stream,
    });
    let mut options = serde_json::Map::new();
    if let Some(temperature) = req.temperature {
        options.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = req.top_p {
        options.insert("top_p".into(), json!(top_p));
    }
    if let Some(max_tokens) = req.max_tokens {
        options.insert("num_predict".into(), json!(max_tokens));
    }
    if let Some(stop) = &req.stop {
        if !stop.is_empty() {
            options.insert("stop".into(), json!(stop));
        }
    }
    if !options.is_empty() {
        body["options"] = serde_json::Value::Object(options);
    }
    if let Some(tools) = &req.tools {
        body["tools"] = tools.clone();
    }
    body
}

pub fn from_ollama_response(v: &serde_json::Value) -> ChatResponse {
    let msg = v.get("message").cloned().unwrap_or(json!({}));
    let tool_calls = tool_calls_from_ollama(msg.get("tool_calls"));
    let has_tool_calls = tool_calls.is_some();
    ChatResponse {
        id: uuid::Uuid::new_v4().to_string(),
        model: v.get("model").and_then(|x| x.as_str()).unwrap_or("").into(),
        content: msg
            .get("content")
            .and_then(|x| x.as_str())
            .or_else(|| v.get("response").and_then(|x| x.as_str()))
            .unwrap_or("")
            .into(),
        tool_calls,
        finish_reason: if has_tool_calls {
            Some("tool_calls".into())
        } else if v.get("done").and_then(|x| x.as_bool()).unwrap_or(false) {
            Some("stop".into())
        } else {
            None
        },
        usage: Some(usage_from_ollama(v)),
    }
}

/// 内部工具调用 -> Ollama 原生 `message.tool_calls`。Ollama 的 arguments 是 JSON
/// 值而非 OpenAI 的 JSON 字符串，出口必须转换，否则客户端无法直接执行函数。
pub fn tool_calls_to_ollama(tool_calls: &[ToolCall]) -> Vec<serde_json::Value> {
    tool_calls
        .iter()
        .map(|tool_call| {
            let arguments =
                serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                    .unwrap_or_else(|_| json!({}));
            json!({
                "function": {
                    "name": tool_call.function.name,
                    "arguments": arguments,
                }
            })
        })
        .collect()
}

/// 内部响应 -> Ollama 非流式响应。与 `to_ollama_body` 配对，确保跨方言上游
/// 返回的 function call 不会在 `/api/chat` 出口被丢弃。
pub fn ollama_response(resp: &ChatResponse, model: &str) -> serde_json::Value {
    let mut message = json!({ "role": "assistant", "content": resp.content });
    if let Some(tool_calls) = &resp.tool_calls {
        message["tool_calls"] = json!(tool_calls_to_ollama(tool_calls));
    }
    let usage = resp.usage.clone().unwrap_or_default();
    json!({
        "model": model,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "message": message,
        "done": true,
        "done_reason": if resp.finish_reason.as_deref() == Some("tool_calls") { "tool_calls" } else { "stop" },
        "prompt_eval_count": usage.prompt_tokens,
        "eval_count": usage.completion_tokens,
    })
}

fn copy_option(target: &mut serde_json::Value, key: &str, value: Option<&serde_json::Value>) {
    if let Some(value) = value.filter(|value| !value.is_null()) {
        target[key] = value.clone();
    }
}

fn normalize_ollama_messages(messages: Option<&serde_json::Value>) -> serde_json::Value {
    let mut messages = messages
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Ollama 的 tool call ID 是可选的，OpenAI IR 中却是关联 tool result 的必要字段。
    // 为缺失 ID 的历史调用生成稳定的请求内 ID，避免转换时整条调用被过滤。
    for (message_index, message) in messages.iter_mut().enumerate() {
        let Some(tool_calls) = message
            .get_mut("tool_calls")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for (call_index, call) in tool_calls.iter_mut().enumerate() {
            if call.get("id").is_none() {
                call["id"] = json!(format!("ollama-{message_index}-{call_index}"));
            }
            if call.get("type").is_none() {
                call["type"] = json!("function");
            }
        }
    }

    serde_json::Value::Array(messages)
}

fn tool_calls_from_ollama(value: Option<&serde_json::Value>) -> Option<Vec<ToolCall>> {
    let tool_calls = value?.as_array()?;
    let calls: Vec<ToolCall> = tool_calls
        .iter()
        .enumerate()
        .filter_map(|(index, call)| {
            let function = call.get("function")?;
            let name = function.get("name")?.as_str()?.to_string();
            let arguments = match function.get("arguments") {
                Some(serde_json::Value::String(arguments)) => arguments.clone(),
                Some(arguments) => serde_json::to_string(arguments).ok()?,
                None => "{}".into(),
            };
            Some(ToolCall {
                id: call
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("ollama-call-{index}")),
                kind: "function".into(),
                function: FunctionCall { name, arguments },
            })
        })
        .collect();
    (!calls.is_empty()).then_some(calls)
}

fn usage_from_ollama(value: &serde_json::Value) -> Usage {
    let prompt_tokens = value
        .get("prompt_eval_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let completion_tokens = value
        .get("eval_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
    }
}
