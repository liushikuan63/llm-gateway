//! Raw OpenAI ingress for an explicitly selected Trae native route.
//!
//! Validate before the permissive OpenAI converter can discard information.
//! These are local resource limits, not claims about upstream capabilities.
use std::collections::HashSet;
use std::io::Write;

use base64::Engine;
use serde_json::{Map, Value};

use crate::domain::{ChatRequest, Content, FunctionCall, ImageUrl, Message, Part, Role, ToolCall};
use crate::error::{GatewayError, Result};

const REQUEST_BYTES: usize = 8 * 1024 * 1024;
const ARGUMENT_BYTES: usize = 1024 * 1024;
const SCHEMA_BYTES: usize = 256 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_CALLS: usize = 64;
const MAX_MESSAGES: usize = 4096;
const MAX_PARTS: usize = 4096;
const IMAGE_BYTES: usize = 4 * 1024 * 1024;

fn fail(code: &str, path: &str) -> GatewayError {
    GatewayError::Protocol(format!("native_request_{code} at {path}"))
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>> {
    value.as_object().ok_or_else(|| fail("invalid_type", path))
}

fn keys(value: &Map<String, Value>, allowed: &[&str], path: &str) -> Result<()> {
    if value.keys().any(|key| !allowed.contains(&key.as_str())) {
        // Never interpolate an untrusted key (which may itself contain secrets).
        return Err(fail("unsupported_field", path));
    }
    Ok(())
}

fn required<'a>(value: &'a Map<String, Value>, key: &str, path: &str) -> Result<&'a Value> {
    value.get(key).ok_or_else(|| fail("missing_field", path))
}

fn string<'a>(value: &'a Value, path: &str) -> Result<&'a str> {
    value.as_str().ok_or_else(|| fail("invalid_type", path))
}

fn identifier(value: &Value, path: &str, function_name: bool) -> Result<String> {
    let text = string(value, path)?;
    let valid = if function_name {
        text.len() <= 64
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    } else {
        text.len() <= 256 && text.trim() == text && !text.chars().any(char::is_control)
    };
    if text.is_empty() || !valid {
        return Err(fail("invalid_value", path));
    }
    Ok(text.to_owned())
}

fn depth(value: &Value, level: usize, path: &str) -> Result<()> {
    if level > MAX_DEPTH {
        return Err(fail("limit_exceeded", path));
    }
    match value {
        Value::Array(values) => {
            for value in values {
                depth(value, level + 1, path)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                depth(value, level + 1, path)?;
            }
        }
        Value::Number(number) if number.as_f64().map_or(true, |number| !number.is_finite()) => {
            return Err(fail("invalid_value", path));
        }
        _ => {}
    }
    Ok(())
}

struct ByteLimit {
    remaining: usize,
}
impl Write for ByteLimit {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "JSON limit",
            ));
        }
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn bounded_json(value: &Value, limit: usize, path: &str) -> Result<usize> {
    depth(value, 0, path)?;
    let mut writer = ByteLimit { remaining: limit };
    serde_json::to_writer(&mut writer, value).map_err(|_| fail("limit_exceeded", path))?;
    Ok(limit - writer.remaining)
}

fn image(value: &Value, path: &str) -> Result<ImageUrl> {
    let map = object(value, path)?;
    keys(map, &["url", "detail"], path)?;
    let url_path = format!("{path}.url");
    let raw = string(required(map, "url", &url_path)?, &url_path)?;
    if raw.starts_with("data:") {
        let payload = ["image/png", "image/jpeg", "image/gif", "image/webp"]
            .iter()
            .find_map(|mime| raw.strip_prefix(&format!("data:{mime};base64,")))
            .ok_or_else(|| fail("invalid_image", &url_path))?;
        if payload.is_empty() || payload.len() > IMAGE_BYTES.div_ceil(3) * 4 {
            return Err(fail("limit_exceeded", &url_path));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|_| fail("invalid_image", &url_path))?;
        if bytes.is_empty() || bytes.len() > IMAGE_BYTES {
            return Err(fail("limit_exceeded", &url_path));
        }
    } else {
        if raw.len() > 8192 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(fail("invalid_image", &url_path));
        }
        let parsed = reqwest::Url::parse(raw).map_err(|_| fail("invalid_image", &url_path))?;
        // Check raw authority too: URL parsers normalize an empty userinfo away.
        let authority = raw
            .split_once("://")
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""));
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
            || authority.is_some_and(|a| a.contains('@'))
            || !raw.starts_with("https://")
            || raw.contains('\\')
        {
            return Err(fail("invalid_image", &url_path));
        }
    }
    let detail = map
        .get("detail")
        .map(|value| {
            let path = format!("{path}.detail");
            let text = string(value, &path)?;
            if !matches!(text, "auto" | "low" | "high") {
                return Err(fail("invalid_value", &path));
            }
            Ok(text.to_owned())
        })
        .transpose()?;
    Ok(ImageUrl {
        url: raw.to_owned(),
        detail,
    })
}

/// Reuse the same image contract for typed callers without rewriting their payload.
pub(crate) fn validate_native_image_url(value: &ImageUrl) -> std::result::Result<(), &'static str> {
    let raw = serde_json::to_value(value).map_err(|_| "native_image_invalid")?;
    image(&raw, "image_url")
        .map(|_| ())
        .map_err(|_| "native_image_invalid")
}

fn content(value: &Value, path: &str) -> Result<Content> {
    if let Some(text) = value.as_str() {
        return Ok(Content::Text(text.to_owned()));
    }
    let parts = value.as_array().ok_or_else(|| fail("invalid_type", path))?;
    if parts.is_empty() || parts.len() > MAX_PARTS {
        return Err(fail("limit_exceeded", path));
    }
    let mut output = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        let path = format!("{path}[{index}]");
        let map = object(part, &path)?;
        let kind_path = format!("{path}.type");
        let kind = string(required(map, "type", &kind_path)?, &kind_path)?;
        match kind {
            "text" => {
                keys(map, &["type", "text"], &path)?;
                let text_path = format!("{path}.text");
                output.push(Part::Text {
                    text: string(required(map, "text", &text_path)?, &text_path)?.to_owned(),
                });
            }
            "image_url" => {
                keys(map, &["type", "image_url"], &path)?;
                let image_path = format!("{path}.image_url");
                output.push(Part::ImageUrl {
                    image_url: image(required(map, "image_url", &image_path)?, &image_path)?,
                });
            }
            _ => return Err(fail("unsupported_part", &kind_path)),
        }
    }
    Ok(Content::Parts(output))
}

fn calls(
    value: &Value,
    path: &str,
    seen: &mut HashSet<String>,
    pending: &mut HashSet<String>,
) -> Result<Vec<ToolCall>> {
    let values = value.as_array().ok_or_else(|| fail("invalid_type", path))?;
    if values.is_empty() || values.len() > MAX_CALLS || seen.len() + values.len() > MAX_CALLS {
        return Err(fail("limit_exceeded", path));
    }
    let mut output = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let path = format!("{path}[{index}]");
        let map = object(value, &path)?;
        keys(map, &["id", "type", "function"], &path)?;
        let id_path = format!("{path}.id");
        let id = identifier(required(map, "id", &id_path)?, &id_path, false)?;
        if !seen.insert(id.clone()) {
            return Err(fail("duplicate_tool_call", &id_path));
        }
        let type_path = format!("{path}.type");
        if string(required(map, "type", &type_path)?, &type_path)? != "function" {
            return Err(fail("unsupported_field", &type_path));
        }
        let function_path = format!("{path}.function");
        let function = object(required(map, "function", &function_path)?, &function_path)?;
        keys(function, &["name", "arguments"], &function_path)?;
        let name_path = format!("{function_path}.name");
        let name = identifier(required(function, "name", &name_path)?, &name_path, true)?;
        let arguments_path = format!("{function_path}.arguments");
        let arguments = string(
            required(function, "arguments", &arguments_path)?,
            &arguments_path,
        )?;
        if arguments.len() > ARGUMENT_BYTES {
            return Err(fail("limit_exceeded", &arguments_path));
        }
        let parsed: Value = serde_json::from_str(arguments)
            .map_err(|_| fail("invalid_arguments", &arguments_path))?;
        if !parsed.is_object() {
            return Err(fail("invalid_arguments", &arguments_path));
        }
        depth(&parsed, 0, &arguments_path)?;
        pending.insert(id.clone());
        output.push(ToolCall {
            id,
            kind: "function".into(),
            function: FunctionCall {
                name,
                arguments: arguments.to_owned(),
            },
        });
    }
    Ok(output)
}

fn tools(value: &Value) -> Result<HashSet<String>> {
    let values = value
        .as_array()
        .ok_or_else(|| fail("invalid_type", "$.tools"))?;
    if values.len() > MAX_CALLS {
        return Err(fail("limit_exceeded", "$.tools"));
    }
    let mut names = HashSet::new();
    let mut schema_bytes = 0;
    for (index, value) in values.iter().enumerate() {
        let path = format!("$.tools[{index}]");
        let map = object(value, &path)?;
        keys(map, &["type", "function"], &path)?;
        let type_path = format!("{path}.type");
        if string(required(map, "type", &type_path)?, &type_path)? != "function" {
            return Err(fail("unsupported_field", &type_path));
        }
        let path = format!("{path}.function");
        let function = object(required(map, "function", &path)?, &path)?;
        keys(function, &["name", "description", "parameters"], &path)?;
        let name_path = format!("{path}.name");
        let name = identifier(required(function, "name", &name_path)?, &name_path, true)?;
        if !names.insert(name) {
            return Err(fail("duplicate_tool", &name_path));
        }
        if let Some(description) = function.get("description") {
            string(description, &format!("{path}.description"))?;
        }
        let path = format!("{path}.parameters");
        let parameters = required(function, "parameters", &path)?;
        object(parameters, &path)?;
        schema_bytes += bounded_json(parameters, SCHEMA_BYTES, &path)?;
        if schema_bytes > SCHEMA_BYTES {
            return Err(fail("limit_exceeded", "$.tools"));
        }
    }
    Ok(names)
}

fn tool_choice(value: &Value, names: &HashSet<String>) -> Result<()> {
    let path = "$.tool_choice";
    if let Some(text) = value.as_str() {
        if !matches!(text, "auto" | "required" | "none") {
            return Err(fail("invalid_value", path));
        }
        if text != "none" && names.is_empty() {
            return Err(fail("missing_tools", path));
        }
        return Ok(());
    }
    let map = object(value, path)?;
    keys(map, &["type", "function"], path)?;
    if string(
        required(map, "type", "$.tool_choice.type")?,
        "$.tool_choice.type",
    )? != "function"
    {
        return Err(fail("unsupported_field", "$.tool_choice.type"));
    }
    let function = object(
        required(map, "function", "$.tool_choice.function")?,
        "$.tool_choice.function",
    )?;
    keys(function, &["name"], "$.tool_choice.function")?;
    let name = identifier(
        required(function, "name", "$.tool_choice.function.name")?,
        "$.tool_choice.function.name",
        true,
    )?;
    if !names.contains(&name) {
        return Err(fail("unknown_tool", "$.tool_choice.function.name"));
    }
    Ok(())
}

/// Native-only immutable history contract; generic Message remains unchanged.
/// Assistant reasoning preserves absent/null/string distinctly. Other optional
/// fields reject null; assistant content may be absent/null with function calls.
/// Empty tools is preserved; empty messages/content-parts/tool-calls are rejected.
#[derive(Debug)]
pub(crate) struct NativeChatRequest {
    request: ChatRequest,
    reasoning: Vec<Option<Option<String>>>,
}
impl std::ops::Deref for NativeChatRequest {
    type Target = ChatRequest;
    fn deref(&self) -> &ChatRequest {
        &self.request
    }
}
impl NativeChatRequest {
    #[cfg(test)]
    fn into_request(self) -> ChatRequest {
        self.request
    }
    /// Position-aligned with the validated messages; None is absent, Some(None) is JSON null.
    pub(crate) fn reasoning(&self) -> &[Option<Option<String>>] {
        &self.reasoning
    }
}
pub(crate) fn parse_trae_openai(value: &Value) -> Result<NativeChatRequest> {
    let map = object(value, "$")?;
    const CONSTRAINTS: &[&str] = &[
        "temperature",
        "top_p",
        "stop",
        "seed",
        "n",
        "logprobs",
        "top_logprobs",
        "presence_penalty",
        "frequency_penalty",
        "response_format",
        "thinking",
        "reasoning",
        "logit_bias",
    ];
    for key in CONSTRAINTS {
        if map.contains_key(*key) {
            return Err(fail("unsupported_constraint", &format!("$.{key}")));
        }
    }
    keys(
        map,
        &[
            "model",
            "messages",
            "max_tokens",
            "max_completion_tokens",
            "stream",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "reasoning_effort",
            "stream_options",
            "user",
        ],
        "$",
    )?;
    bounded_json(value, REQUEST_BYTES, "$")?;
    let model = identifier(required(map, "model", "$.model")?, "$.model", false)?;
    let tools_value = map.get("tools");
    let names = tools_value.map(tools).transpose()?.unwrap_or_default();
    if let Some(value) = map.get("tool_choice") {
        tool_choice(value, &names)?;
    }
    let mut extra = Map::new();
    if let Some(value) = map.get("parallel_tool_calls") {
        if !value.is_boolean() {
            return Err(fail("invalid_type", "$.parallel_tool_calls"));
        }
        if names.is_empty() {
            return Err(fail("missing_tools", "$.parallel_tool_calls"));
        }
        extra.insert("parallel_tool_calls".into(), value.clone());
    }
    for (key, max) in [("reasoning_effort", 64), ("user", 256)] {
        if let Some(value) = map.get(key) {
            let path = format!("$.{key}");
            let text = string(value, &path)?;
            if text.len() > max
                || (key == "reasoning_effort" && text.is_empty())
                || text.chars().any(char::is_control)
            {
                return Err(fail("invalid_value", &path));
            }
            extra.insert(key.into(), value.clone());
        }
    }
    if let Some(value) = map.get("stream_options") {
        let options = object(value, "$.stream_options")?;
        keys(options, &["include_usage"], "$.stream_options")?;
        if options
            .get("include_usage")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(fail("invalid_type", "$.stream_options.include_usage"));
        }
        extra.insert("stream_options".into(), value.clone());
    }
    let stream = map
        .get("stream")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| fail("invalid_type", "$.stream"))
        })
        .transpose()?
        .unwrap_or(false);
    let max = |key: &str| -> Result<Option<u32>> {
        map.get(key)
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| fail("invalid_value", &format!("$.{key}")))
            })
            .transpose()
    };
    let max_tokens = max("max_tokens")?;
    let max_completion_tokens = max("max_completion_tokens")?;
    if matches!((max_tokens, max_completion_tokens), (Some(a), Some(b)) if a != b) {
        return Err(fail("conflicting_constraint", "$.max_completion_tokens"));
    }
    let values = required(map, "messages", "$.messages")?
        .as_array()
        .ok_or_else(|| fail("invalid_type", "$.messages"))?;
    if values.is_empty() || values.len() > MAX_MESSAGES {
        return Err(fail("limit_exceeded", "$.messages"));
    }
    let mut messages = Vec::with_capacity(values.len());
    let mut reasoning = Vec::with_capacity(values.len());
    let mut seen = HashSet::new();
    let mut pending = HashSet::new();
    for (index, value) in values.iter().enumerate() {
        let path = format!("$.messages[{index}]");
        let map = object(value, &path)?;
        keys(
            map,
            &[
                "role",
                "content",
                "name",
                "tool_calls",
                "tool_call_id",
                "reasoning_content",
            ],
            &path,
        )?;
        let role_path = format!("{path}.role");
        let role = match string(required(map, "role", &role_path)?, &role_path)? {
            "system" => Role::System,
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            _ => return Err(fail("unsupported_role", &role_path)),
        };
        let reasoning_path = format!("{path}.reasoning_content");
        let history = match map.get("reasoning_content") {
            Some(_) if role != Role::Assistant => {
                return Err(fail("invalid_role_field", &reasoning_path))
            }
            Some(Value::Null) => Some(None),
            Some(value) => Some(Some(string(value, &reasoning_path)?.to_owned())),
            None => None,
        };
        reasoning.push(history);
        let calls_path = format!("{path}.tool_calls");
        if role != Role::Assistant && map.contains_key("tool_calls") {
            return Err(fail("invalid_role_field", &calls_path));
        }
        let tool_calls = map
            .get("tool_calls")
            .map(|value| calls(value, &calls_path, &mut seen, &mut pending))
            .transpose()?;
        let content_path = format!("{path}.content");
        let content = match map.get("content") {
            None | Some(Value::Null) if tool_calls.is_some() => Content::Text(String::new()),
            Some(value) => content(value, &content_path)?,
            None => return Err(fail("missing_field", &content_path)),
        };
        let id_path = format!("{path}.tool_call_id");
        let tool_call_id = if role == Role::Tool {
            let id = identifier(required(map, "tool_call_id", &id_path)?, &id_path, false)?;
            if !pending.remove(&id) {
                return Err(fail("orphan_tool_result", &id_path));
            }
            Some(id)
        } else {
            if map.contains_key("tool_call_id") {
                return Err(fail("invalid_role_field", &id_path));
            }
            None
        };
        let name_path = format!("{path}.name");
        let name = map
            .get("name")
            .map(|value| identifier(value, &name_path, true))
            .transpose()?;
        messages.push(Message {
            role,
            content,
            tool_calls,
            tool_call_id,
            name,
        });
    }
    Ok(NativeChatRequest {
        reasoning,
        request: ChatRequest {
            model,
            messages,
            temperature: None,
            top_p: None,
            max_tokens: max_tokens.or(max_completion_tokens),
            stop: None,
            stream,
            tools: tools_value.cloned(),
            tool_choice: map.get("tool_choice").cloned(),
            thinking: None,
            extra,
        },
    })
}

#[cfg(test)]
mod tests;
