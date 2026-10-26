//! Strict, lossless candidate native wire mapping. No network, defaults, or context rewriting.
use super::{catalog::TraeNativeModelView, profile::SourceFunction};
use crate::domain::{ChatRequest, Content, Part, Role};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const REQUEST_LIMIT: usize = 8 * 1024 * 1024;
pub(crate) const SCHEMA_LIMIT: usize = 256 * 1024;
pub(crate) const CALL_LIMIT: usize = 64;
pub(crate) const ARGUMENT_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeToolChoice {
    Auto,
    Required,
    None,
    Named(String),
}
#[derive(Clone, Debug)]
pub(crate) struct NativeConstraints {
    pub config_name: String,
    pub declared_tools: BTreeSet<String>,
    pub tool_choice: NativeToolChoice,
    pub parallel_tool_calls: Option<bool>,
}
#[derive(Debug)]
pub(crate) struct PreparedNativeRequest {
    pub body: Value,
    pub constraints: NativeConstraints,
}
/// Explicit local candidate policy, never a model capability declaration.
/// The default rejects images when the native catalog has no vision evidence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct NativeRequestOptions {
    pub allow_undeclared_images: bool,
}
/// Only the validated Native DTO can supply history; generic extra fields remain closed.
pub(crate) fn build_native_history_request(
    req: &crate::protocol::strict_account::NativeChatRequest,
    target: &TraeNativeModelView,
    function: SourceFunction,
    options: NativeRequestOptions,
) -> Result<PreparedNativeRequest, String> {
    let mut prepared = build_native_request_with_options(req, target, function, options)?;
    for (index, history) in req.reasoning().iter().enumerate() {
        if let Some(reasoning) = history {
            prepared.body["messages"][index]["reasoning_content"] = json!(reasoning);
        }
    }
    if serde_json::to_vec(&prepared.body)
        .map_err(|_| "native_request_invalid")?
        .len()
        > REQUEST_LIMIT
    {
        return Err("native_request_limit".into());
    }
    Ok(prepared)
}
pub(super) fn exact_keys(object: &Map<String, Value>, keys: &[&str]) -> Result<(), String> {
    if object.keys().any(|k| !keys.contains(&k.as_str())) {
        return Err("native_request_unknown_field".into());
    }
    Ok(())
}
pub(super) fn valid_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
pub(super) fn json_object_arguments(arguments: &str) -> Result<(), String> {
    if arguments.len() > ARGUMENT_LIMIT || !super::catalog::nesting_valid(arguments.as_bytes()) {
        return Err("native_tool_arguments_limit_or_nesting".into());
    }
    let parsed: Value =
        serde_json::from_str(arguments).map_err(|_| "native_tool_arguments_invalid")?;
    if !parsed.is_object() {
        return Err("native_tool_arguments_not_object".into());
    }
    Ok(())
}

fn tools(value: Option<&Value>) -> Result<(Option<Value>, BTreeSet<String>), String> {
    let mut names = BTreeSet::new();
    let Some(value) = value else {
        return Ok((None, names));
    };
    let entries = value
        .as_array()
        .filter(|v| v.len() <= CALL_LIMIT)
        .ok_or("native_tools_invalid")?;
    let mut output = Vec::with_capacity(entries.len());
    let mut schema_bytes = 0usize;
    for entry in entries {
        let object = entry.as_object().ok_or("native_tools_invalid")?;
        exact_keys(object, &["type", "function"])?;
        if object.get("type").and_then(Value::as_str) != Some("function") {
            return Err("native_tool_type_not_supported".into());
        }
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .ok_or("native_tools_invalid")?;
        exact_keys(function, &["name", "description", "parameters"])?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|v| valid_name(v))
            .ok_or("native_tool_name_invalid")?;
        if !names.insert(name.to_owned()) {
            return Err("native_duplicate_tool_name".into());
        }
        if function.get("description").is_some_and(|v| !v.is_string()) {
            return Err("native_tool_description_invalid".into());
        }
        let parameters = function
            .get("parameters")
            .filter(|v| v.is_object())
            .ok_or("native_tool_schema_not_object")?;
        let serialized =
            serde_json::to_string(parameters).map_err(|_| "native_tool_schema_invalid")?;
        schema_bytes = schema_bytes
            .checked_add(serialized.len())
            .ok_or("native_tool_schema_limit")?;
        if schema_bytes > SCHEMA_LIMIT || !super::catalog::nesting_valid(serialized.as_bytes()) {
            return Err("native_tool_schema_limit".into());
        }
        let mut native = function.clone();
        native.insert("parameters".into(), Value::String(serialized));
        output.push(json!({"type":"function","function":native}));
    }
    Ok((Some(Value::Array(output)), names))
}

fn choice(
    value: Option<&Value>,
    names: &BTreeSet<String>,
) -> Result<(NativeToolChoice, Option<String>), String> {
    let Some(value) = value else {
        return Ok((NativeToolChoice::Auto, None));
    };
    let name = if let Some(value) = value.as_str() {
        match value {
            "auto" => return Ok((NativeToolChoice::Auto, Some(value.into()))),
            "none" => return Ok((NativeToolChoice::None, Some(value.into()))),
            "required" if !names.is_empty() => {
                return Ok((NativeToolChoice::Required, Some(value.into())))
            }
            _ => return Err("native_tool_choice_invalid".into()),
        }
    } else {
        let object = value.as_object().ok_or("native_tool_choice_invalid")?;
        exact_keys(object, &["type", "function"])?;
        if object.get("type").and_then(Value::as_str) != Some("function") {
            return Err("native_tool_choice_invalid".into());
        }
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .ok_or("native_tool_choice_invalid")?;
        exact_keys(function, &["name"])?;
        function
            .get("name")
            .and_then(Value::as_str)
            .ok_or("native_tool_choice_invalid")?
    };
    if !names.contains(name) {
        return Err("native_tool_choice_undeclared".into());
    }
    Ok((NativeToolChoice::Named(name.into()), Some(name.into())))
}

fn content(
    content: &Content,
    target: &TraeNativeModelView,
    options: NativeRequestOptions,
) -> Result<Value, String> {
    match content {
        Content::Text(text) => Ok(json!([{"type":"text","text":text}])),
        Content::Parts(parts) => {
            let mut output = Vec::with_capacity(parts.len());
            for part in parts {
                match part {
                    Part::Text { text } => output.push(json!({"type":"text","text":text})),
                    Part::ImageUrl { image_url } => {
                        match target.declared_vision {
                            Some(true) if target.protocol_carriers.images => (),
                            None if options.allow_undeclared_images
                                && target.protocol_carriers.images => {}
                            None => return Err("native_vision_capability_not_declared".into()),
                            _ => return Err("native_vision_not_supported".into()),
                        }
                        crate::protocol::strict_account::validate_native_image_url(image_url)?;
                        // Preserve validated URL/detail bytes; no fetch or normalization.
                        output.push(json!({"type":"image_url","image_url":image_url}));
                    }
                    Part::InputAudio { .. } => return Err("native_audio_not_supported".into()),
                    Part::VideoUrl { .. } => return Err("native_video_not_supported".into()),
                }
            }
            Ok(Value::Array(output))
        }
    }
}

/// Test-only default-options convenience; production always supplies validated options.
#[cfg(test)]
pub(crate) fn build_native_request(
    req: &ChatRequest,
    target: &TraeNativeModelView,
    function: SourceFunction,
) -> Result<PreparedNativeRequest, String> {
    build_native_request_with_options(req, target, function, NativeRequestOptions::default())
}

/// Candidate image opt-in must come from validated runtime configuration, never req.extra.
/// It neither changes the catalog DTO nor claims this account/model accepts images.
pub(crate) fn build_native_request_with_options(
    req: &ChatRequest,
    target: &TraeNativeModelView,
    function: SourceFunction,
    options: NativeRequestOptions,
) -> Result<PreparedNativeRequest, String> {
    // Validate again: internal callers may bypass the strict raw ingress converter.
    if req.temperature.is_some()
        || req.top_p.is_some()
        || req.stop.is_some()
        || req.thinking.is_some()
    {
        return Err("native_parameter_not_supported".into());
    }
    exact_keys(
        &req.extra,
        &[
            "reasoning_effort",
            "parallel_tool_calls",
            "user",
            "stream_options",
        ],
    )?;
    // user is a local attribution hint; stream_options controls the local response encoder only.
    // Neither is an upstream protocol field and neither influences model selection.
    if req.extra.get("user").is_some_and(|v| {
        v.as_str()
            .map_or(true, |s| s.len() > 1024 || s.chars().any(char::is_control))
    }) {
        return Err("native_user_invalid".into());
    }
    if let Some(options) = req.extra.get("stream_options") {
        let options = options.as_object().ok_or("native_stream_options_invalid")?;
        exact_keys(options, &["include_usage"])?;
        if !req.stream
            || options
                .get("include_usage")
                .is_some_and(|v| !v.is_boolean())
        {
            return Err("native_stream_options_invalid".into());
        }
    }
    if !valid_name(&target.config_name) || req.messages.is_empty() {
        return Err("native_request_invalid".into());
    }
    // Bound before cloning the request into a second representation.
    if serde_json::to_vec(req)
        .map_err(|_| "native_request_invalid")?
        .len()
        > REQUEST_LIMIT
    {
        return Err("native_request_limit".into());
    }
    if req
        .max_tokens
        .is_some_and(|v| v == 0 || target.max_tokens.is_some_and(|max| v > max))
    {
        return Err("native_max_tokens_out_of_range".into());
    }
    let (native_tools, names) = tools(req.tools.as_ref())?;
    if !names.is_empty()
        && (!target.protocol_carriers.tools || target.declared_tools == Some(false))
    {
        return Err("native_tools_not_supported".into());
    }
    let (tool_choice, native_choice) = choice(req.tool_choice.as_ref(), &names)?;
    let parallel = match req.extra.get("parallel_tool_calls") {
        Some(value) => {
            if names.is_empty() {
                return Err("native_parallel_requires_tools".into());
            }
            Some(value.as_bool().ok_or("native_parallel_invalid")?)
        }
        None => None,
    };
    let mut messages = Vec::with_capacity(req.messages.len());
    let mut history_calls = BTreeMap::<String, String>::new();
    let mut pending_calls = BTreeSet::new();
    let mut history_count = 0usize;
    let mut argument_bytes = 0usize;
    for message in &req.messages {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut native = json!({"role":role,"content":content(&message.content, target, options)?});
        if let Some(name) = &message.name {
            if !valid_name(name) {
                return Err("native_message_name_invalid".into());
            }
            native["name"] = json!(name);
        }
        if message.role != Role::Tool && !pending_calls.is_empty() {
            return Err("native_tool_results_missing".into());
        }
        if message.role == Role::Tool {
            let id = message
                .tool_call_id
                .as_ref()
                .filter(|v| valid_name(v))
                .ok_or("native_tool_result_id_missing")?;
            if !pending_calls.remove(id) {
                return Err("native_tool_result_orphan_or_duplicate".into());
            }
            if message
                .name
                .as_ref()
                .is_some_and(|name| history_calls.get(id) != Some(name))
            {
                return Err("native_tool_result_name_mismatch".into());
            }
            native["tool_call_id"] = json!(id);
        } else if message.tool_call_id.is_some() {
            return Err("native_tool_id_wrong_role".into());
        }
        if let Some(calls) = &message.tool_calls {
            if message.role != Role::Assistant || calls.is_empty() {
                return Err("native_tool_calls_wrong_role".into());
            }
            let mut native_calls = Vec::new();
            for call in calls {
                history_count += 1;
                argument_bytes = argument_bytes
                    .checked_add(call.function.arguments.len())
                    .ok_or("native_tool_arguments_limit")?;
                if history_count > CALL_LIMIT || argument_bytes > ARGUMENT_LIMIT {
                    return Err("native_tool_history_limit".into());
                }
                if call.kind != "function"
                    || !valid_name(&call.id)
                    || !valid_name(&call.function.name)
                {
                    return Err("native_tool_call_invalid".into());
                }
                json_object_arguments(&call.function.arguments)?;
                if history_calls
                    .insert(call.id.clone(), call.function.name.clone())
                    .is_some()
                {
                    return Err("native_tool_call_duplicate_id".into());
                }
                pending_calls.insert(call.id.clone());
                native_calls.push(json!({"id":call.id,"type":"function","function_call":{
                    "name":call.function.name,"arguments":call.function.arguments}}));
            }
            native["tool_calls"] = Value::Array(native_calls);
        }
        messages.push(native);
    }
    if !pending_calls.is_empty() {
        return Err("native_tool_results_missing".into());
    }
    let mut body = json!({"model":target.config_name,"config_name":target.config_name,
        "function":function.as_str(),"stream":true,"messages":messages});
    if function == SourceFunction::ChatV3 {
        let raw = target
            .raw_model
            .as_ref()
            .filter(|v| v.valid())
            .ok_or("native_raw_physical_model_missing")?;
        body.as_object_mut()
            .ok_or("native_request_invalid")?
            .remove("model");
        body.as_object_mut()
            .ok_or("native_request_invalid")?
            .remove("function");
        // The selected raw product chain is carried in the body as well as
        // the auth headers. It is never inferred from the public model alias.
        body["raw_chat_function"] = json!(function.as_str());
        body["model_name"] = json!(raw.model_name);
        body["config_source"] = json!(raw.config_source);
        body["session_id"] = json!(uuid::Uuid::new_v4().to_string());
        body["conversation_id"] = json!(uuid::Uuid::new_v4().to_string());
    } else if target.raw_model.is_some() {
        return Err("native_model_profile_mismatch".into());
    }
    if let Some(value) = req.max_tokens {
        body["max_tokens"] = json!(value);
    }
    if let Some(value) = native_tools {
        body["tools"] = value;
    }
    if let Some(value) = native_choice {
        body["tool_choice"] = json!(value);
    }
    if let Some(value) = parallel {
        body["parallel_tool_calls"] = json!(value);
    }
    if let Some(value) = req.extra.get("reasoning_effort") {
        let effort = value
            .as_str()
            .filter(|s| target.reasoning_effort_options.iter().any(|v| v == s))
            .ok_or("native_reasoning_effort_not_declared")?;
        body["reasoning_effort"] = json!(effort);
    }
    if serde_json::to_vec(&body)
        .map_err(|_| "native_request_invalid")?
        .len()
        > REQUEST_LIMIT
    {
        return Err("native_request_limit".into());
    }
    Ok(PreparedNativeRequest {
        body,
        constraints: NativeConstraints {
            config_name: target.config_name.clone(),
            declared_tools: names,
            tool_choice,
            parallel_tool_calls: parallel,
        },
    })
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
