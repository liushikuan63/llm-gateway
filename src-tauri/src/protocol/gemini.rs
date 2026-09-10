//! Gemini 原生协议（/v1beta/models/{model}:generateContent | :streamGenerateContent）
//! 覆盖文本、工具声明和函数调用；Gemini 的 functionDeclarations 与 OpenAI 的
//! `tools[].function` 之间在这里转换。
//! 需要它的原因：Gemini CLI 与部分 Android/前端工具直接说这条线。

use crate::domain::{ChatRequest, ChatResponse, Content, FunctionCall, Role, ToolCall};
use serde_json::json;

pub fn to_gemini_body(req: &ChatRequest) -> serde_json::Value {
    let mut system: Option<String> = None;
    let mut contents: Vec<serde_json::Value> = Vec::new();

    for m in &req.messages {
        if matches!(m.role, Role::System) {
            let t = match &m.content {
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
            system = Some(match system {
                Some(prev) => format!("{prev}\n{t}"),
                None => t,
            });
            continue;
        }
        let role = match m.role {
            Role::Assistant => "model",
            _ => "user",
        };
        let mut parts = Vec::new();
        let text = content_text(&m.content);
        if !text.is_empty() {
            parts.push(json!({ "text": text }));
        }

        if matches!(m.role, Role::Tool) {
            let response = serde_json::from_str::<serde_json::Value>(&text)
                .unwrap_or_else(|_| json!({ "result": text }));
            parts.push(json!({
                "functionResponse": {
                    "name": m.name.as_deref().unwrap_or("tool"),
                    "response": response,
                }
            }));
        }

        if let Some(tool_calls) = &m.tool_calls {
            for tool_call in tool_calls {
                let args = serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments)
                    .unwrap_or_else(|_| json!({}));
                parts.push(json!({
                    "functionCall": {
                        "name": tool_call.function.name,
                        "args": args,
                    }
                }));
            }
        }

        if parts.is_empty() {
            parts.push(json!({ "text": "" }));
        }
        contents.push(json!({ "role": role, "parts": parts }));
    }

    let mut body = json!({
        "contents": contents,
        "generationConfig": {
            "temperature": req.temperature.unwrap_or(0.7),
            "maxOutputTokens": req.max_tokens.unwrap_or(4096),
        }
    });
    if let Some(s) = system {
        body["systemInstruction"] = json!({ "parts": [{ "text": s }] });
    }
    if let Some(tools) = &req.tools {
        let declarations = gemini_function_declarations(tools);
        if !declarations.is_empty() {
            body["tools"] = json!([{ "functionDeclarations": declarations }]);
        }
    }
    if let Some(tool_choice) = &req.tool_choice {
        if let Some(tool_config) = gemini_tool_config(tool_choice) {
            body["toolConfig"] = tool_config;
        }
    }
    body
}

fn content_text(content: &Content) -> String {
    match content {
        Content::Text(text) => text.clone(),
        Content::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                crate::domain::Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

fn gemini_function_declarations(tools: &serde_json::Value) -> Vec<serde_json::Value> {
    tools
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            let function = tool.get("function").unwrap_or(tool);
            let name = function.get("name")?.as_str()?;
            let mut declaration = json!({ "name": name });
            if let Some(description) = function.get("description") {
                declaration["description"] = description.clone();
            }
            if let Some(parameters) = function
                .get("parameters")
                .or_else(|| function.get("input_schema"))
            {
                declaration["parameters"] = parameters.clone();
            }
            Some(declaration)
        })
        .collect()
}

fn gemini_tool_config(tool_choice: &serde_json::Value) -> Option<serde_json::Value> {
    let mode = match tool_choice {
        serde_json::Value::String(value) => match value.as_str() {
            "none" => "NONE",
            "required" => "ANY",
            _ => "AUTO",
        },
        serde_json::Value::Object(value) => {
            match value.get("type").and_then(|kind| kind.as_str()) {
                Some("none") => "NONE",
                Some("any") | Some("required") | Some("function") | Some("tool") => "ANY",
                _ => "AUTO",
            }
        }
        _ => return None,
    };

    let mut config = json!({ "functionCallingConfig": { "mode": mode } });
    let name = tool_choice
        .get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool_choice.get("name"))
        .and_then(|name| name.as_str());
    if let Some(name) = name {
        config["functionCallingConfig"]["allowedFunctionNames"] = json!([name]);
    }
    Some(config)
}

pub fn from_gemini_response(v: &serde_json::Value) -> ChatResponse {
    let cand = v
        .get("candidates")
        .and_then(|c| c.get(0))
        .cloned()
        .unwrap_or(json!({}));
    let parts = cand
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
        .collect::<Vec<_>>()
        .join("");
    let tool_calls: Vec<ToolCall> = parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| {
            let call = part.get("functionCall")?;
            let name = call.get("name")?.as_str()?.to_string();
            let arguments = serde_json::to_string(call.get("args").unwrap_or(&json!({}))).ok()?;
            Some(ToolCall {
                id: call
                    .get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("call_{index}")),
                kind: "function".into(),
                function: FunctionCall { name, arguments },
            })
        })
        .collect();

    let u = v.get("usageMetadata").cloned().unwrap_or(json!({}));
    ChatResponse {
        id: uuid::Uuid::new_v4().to_string(),
        model: v
            .get("modelVersion")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .into(),
        content: text,
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        finish_reason: cand.get("finishReason").and_then(|x| x.as_str()).map(
            |reason| match reason {
                "STOP" => "stop".into(),
                "MAX_TOKENS" => "length".into(),
                other => other.to_lowercase(),
            },
        ),
        usage: Some(crate::domain::Usage {
            prompt_tokens: u
                .get("promptTokenCount")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32,
            completion_tokens: u
                .get("candidatesTokenCount")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32,
            total_tokens: u
                .get("totalTokenCount")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32,
        }),
    }
}

/// 从 Gemini 流式分片里取文本
pub fn extract_stream_text(v: &serde_json::Value) -> String {
    v.get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}
