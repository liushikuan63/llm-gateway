//! RemoteChatAsk construction from the existing protocol-independent request.
//! Protocol field names were checked against the current public client bundle.
//! No vendor implementation or credential-file reader is used here.
use super::CatalogModel;
use crate::domain::{ChatRequest, Content, Part, Role};
use crate::error::{GatewayError, Result};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use uuid::Uuid;

const BODY_LIMIT: usize = 4 * 1024 * 1024;
fn invalid(message: &str) -> GatewayError {
    GatewayError::Protocol(format!("Qoder {message}"))
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}
fn effort(value: &str) -> Result<&str> {
    match value {
        "none" | "low" | "medium" | "high" | "xhigh" | "max" => Ok(value),
        "minimal" => Ok("low"),
        _ => Err(invalid("reasoning_effort无效")),
    }
}
fn reasoning(req: &ChatRequest) -> Result<Map<String, Value>> {
    let explicit = req
        .extra
        .get("reasoning_effort")
        .map(|v| {
            effort(
                v.as_str()
                    .ok_or_else(|| invalid("reasoning_effort必须为字符串"))?,
            )
            .map(str::to_owned)
        })
        .transpose()?;
    let mut candidates = Vec::new();
    let mut enabled = None;
    let mut budget = None;
    if let Some(thinking) = &req.thinking {
        let object = thinking
            .as_object()
            .ok_or_else(|| invalid("thinking必须为对象"))?;
        if object
            .keys()
            .any(|k| !["type", "budget_tokens", "effort", "reasoning_effort"].contains(&k.as_str()))
        {
            return Err(invalid("thinking包含未验证的参数"));
        }
        for key in ["effort", "reasoning_effort"] {
            if let Some(raw) = thinking.get(key) {
                candidates.push(
                    effort(
                        raw.as_str()
                            .ok_or_else(|| invalid("思考强度必须为字符串"))?,
                    )?
                    .to_owned(),
                );
            }
        }
        if let Some(kind) = thinking.get("type") {
            match kind.as_str() {
                Some("disabled") => {
                    if thinking.get("budget_tokens").is_some() {
                        return Err(invalid("关闭思考不能同时提供预算"));
                    }
                    enabled = Some(false);
                }
                Some("enabled") => {
                    budget = Some(
                        thinking
                            .get("budget_tokens")
                            .and_then(Value::as_u64)
                            .filter(|v| *v > 0 && *v <= u64::from(u32::MAX))
                            .ok_or_else(|| invalid("启用思考需要有效的budget_tokens"))?,
                    );
                    enabled = Some(true);
                }
                _ => return Err(invalid("该thinking类型尚无已验证的协议映射")),
            }
        } else if thinking.get("budget_tokens").is_some() {
            return Err(invalid("budget_tokens缺少明确的thinking类型"));
        } else if candidates.is_empty() {
            return Err(invalid("thinking缺少明确的强度或类型"));
        }
    }
    candidates.extend(explicit);
    if candidates.iter().any(|v| v != &candidates[0]) {
        return Err(invalid("思考参数冲突"));
    }
    let selected = candidates.first().map(String::as_str);
    if enabled == Some(false) && selected.is_some_and(|v| v != "none")
        || enabled == Some(true) && selected == Some("none")
    {
        return Err(invalid("思考开关和强度参数冲突"));
    }
    let mut result = Map::new();
    if selected == Some("none") {
        enabled = Some(false);
    }
    if let Some(value) = enabled {
        result.insert("enable_thinking".into(), json!(value));
    }
    if let Some(value) = budget {
        result.insert("reasoning_budget_tokens".into(), json!(value));
    }
    if let Some(value) = selected.filter(|v| *v != "none") {
        result.insert("reasoning_effort".into(), json!(value));
    }
    Ok(result)
}

fn definitions(req: &ChatRequest) -> Result<(Value, HashSet<String>)> {
    let tools = req
        .tools
        .as_ref()
        .map(|v| {
            v.as_array()
                .filter(|v| v.len() <= 64)
                .ok_or_else(|| invalid("tools必须为有限数组"))
        })
        .transpose()?;
    let mut names = HashSet::new();
    let mut normalized = Vec::new();
    if let Some(tools) = tools {
        for tool in tools {
            let tool = if tool.get("type").and_then(Value::as_str) == Some("function")
                && tool.get("function").is_some()
            {
                tool.clone()
            } else {
                let object = tool.as_object().ok_or_else(|| invalid("工具必须为对象"))?;
                if object.keys().any(|k| {
                    !["type", "name", "description", "input_schema", "parameters"]
                        .contains(&k.as_str())
                }) || tool
                    .get("type")
                    .is_some_and(|v| v.as_str() != Some("function"))
                {
                    return Err(invalid("工具定义包含尚未验证的类型或扩展字段"));
                }
                let parameters = tool
                    .get("input_schema")
                    .or_else(|| tool.get("parameters"))
                    .filter(|v| v.is_object())
                    .ok_or_else(|| invalid("工具缺少完整参数schema"))?;
                if let (Some(a), Some(b)) = (tool.get("input_schema"), tool.get("parameters")) {
                    if a != b {
                        return Err(invalid("工具schema字段冲突"));
                    }
                }
                let mut function = json!({"name":tool.get("name").ok_or_else(||invalid("工具缺少函数名"))?,"parameters":parameters});
                if let Some(description) = tool.get("description") {
                    function["description"] = description.clone();
                }
                json!({"type":"function","function":function})
            };
            let function = tool
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| invalid("工具缺少function对象"))?;
            let tool_name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|v| name(v))
                .ok_or_else(|| invalid("工具函数名无效"))?;
            if !names.insert(tool_name.into()) {
                return Err(invalid("工具函数名重复"));
            }
            if !function.get("parameters").is_some_and(Value::is_object) {
                return Err(invalid("工具参数schema必须为对象"));
            }
            if function.get("description").is_some_and(|v| !v.is_string()) {
                return Err(invalid("工具描述必须为字符串"));
            }
            normalized.push(tool);
        }
    }
    Ok((Value::Array(normalized), names))
}
fn tool_choice(choice: &Value, names: &HashSet<String>) -> Result<Value> {
    let result = match choice {
        Value::String(v) if ["auto", "none", "required"].contains(&v.as_str()) => choice.clone(),
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("auto") => json!("auto"),
            Some("none") => json!("none"),
            Some("any") => json!("required"),
            Some("function") | Some("tool") => {
                let selected = choice
                    .pointer("/function/name")
                    .or_else(|| choice.get("name"))
                    .and_then(Value::as_str)
                    .filter(|v| names.contains(*v))
                    .ok_or_else(|| invalid("tool_choice未引用已声明的函数"))?;
                json!({"type":"function","function":{"name":selected}})
            }
            _ => return Err(invalid("tool_choice类型尚未验证")),
        },
        _ => return Err(invalid("tool_choice无效")),
    };
    if names.is_empty() && result != "none" {
        return Err(invalid("tool_choice需要已声明的工具"));
    }
    Ok(result)
}

/// Pure request construction, not proof of an accepted real inference request.
/// Model metadata must come from the selected account/region's signed directory.
pub fn build(
    req: &ChatRequest,
    model: &CatalogModel,
    session: Uuid,
    request: Uuid,
    started_ms: i64,
) -> Result<Value> {
    if model.enabled != Some(true) || model.format != "openai" {
        return Err(invalid("模型未启用或目录格式尚未验证"));
    }
    if req.messages.is_empty() || req.messages.len() > 1024 || started_ms < 0 {
        return Err(invalid("消息数量或请求时间无效"));
    }
    // user identifies the gateway session, not the authenticated vendor principal.
    // stream_options controls the inbound client's usage preference; the adapter
    // must collect upstream usage regardless. Neither field enters vendor auth.
    if req
        .extra
        .keys()
        .any(|k| !["user", "stream_options", "reasoning_effort"].contains(&k.as_str()))
    {
        return Err(invalid("请求包含尚未验证的扩展字段，请先补齐协议映射"));
    }
    if let Some(options) = req.extra.get("stream_options") {
        let options = options
            .as_object()
            .ok_or_else(|| invalid("stream_options必须为对象"))?;
        if options
            .iter()
            .any(|(k, v)| k != "include_usage" || !v.is_boolean())
        {
            return Err(invalid("stream_options包含未验证的参数"));
        }
    }
    let (tools, names) = definitions(req)?;
    let mut messages = Vec::with_capacity(req.messages.len());
    let mut pending = HashSet::new();
    let mut seen = HashSet::new();
    for message in &req.messages {
        if message.role != Role::Tool && !pending.is_empty() {
            return Err(invalid("工具结果必须完整且紧接其assistant调用"));
        }
        if message.role == Role::Tool {
            let id = message
                .tool_call_id
                .as_ref()
                .filter(|v| pending.remove(*v))
                .ok_or_else(|| invalid("工具结果缺少唯一匹配的调用ID"))?;
            if id.len() > 512 {
                return Err(invalid("工具调用ID超过本地限制"));
            }
        } else if message.tool_call_id.is_some() {
            return Err(invalid("非tool消息不能携带tool_call_id"));
        }
        let parts = match &message.content {
            Content::Text(text) => vec![json!({"type":"text","text":text})],
            Content::Parts(parts) => {
                let mut values = Vec::new();
                for part in parts {
                    match part {
                        Part::Text { text } => values.push(json!({"type":"text","text":text})),
                        Part::ImageUrl { image_url } => {
                            if model.vision != Some(true) || message.role != Role::User {
                                return Err(invalid("所选模型或消息角色未声明支持图片"));
                            }
                            if image_url.url.is_empty() || image_url.url.len() > BODY_LIMIT {
                                return Err(invalid("图片地址无效"));
                            }
                            if !(image_url.url.starts_with("https://")
                                || image_url.url.starts_with("data:image/"))
                            {
                                return Err(invalid("图片需要HTTPS或内联image data URL"));
                            }
                            if image_url.url.starts_with("https://") {
                                let url = reqwest::Url::parse(&image_url.url)
                                    .map_err(|_| invalid("图片HTTPS地址无效"))?;
                                if url.host_str().is_none()
                                    || !url.username().is_empty()
                                    || url.password().is_some()
                                {
                                    return Err(invalid("图片HTTPS地址无效或含内嵌认证"));
                                }
                            } else {
                                use base64::Engine;
                                let (header, data) = image_url
                                    .url
                                    .split_once(',')
                                    .ok_or_else(|| invalid("图片data URL无效"))?;
                                if ![
                                    "data:image/png;base64",
                                    "data:image/jpeg;base64",
                                    "data:image/webp;base64",
                                    "data:image/gif;base64",
                                ]
                                .contains(&header)
                                    || base64::engine::general_purpose::STANDARD
                                        .decode(data)
                                        .ok()
                                        .map_or(true, |v| v.is_empty())
                                {
                                    return Err(invalid("图片data URL格式或编码无效"));
                                }
                            }
                            values.push(json!({"type":"image_url","image_url":image_url}));
                        }
                        Part::InputAudio { .. } | Part::VideoUrl { .. } => {
                            return Err(invalid("音视频协议尚未验证，未丢弃内容或发送请求"))
                        }
                    }
                }
                values
            }
        };
        let mut value = serde_json::to_value(message).map_err(|_| invalid("消息编码失败"))?;
        if message.role != Role::Tool {
            value["contents"] = Value::Array(parts);
        }
        if let Some(calls) = &message.tool_calls {
            if message.role != Role::Assistant || calls.is_empty() || calls.len() > 64 {
                return Err(invalid("工具调用必须属于assistant且数量有效"));
            }
            for call in calls {
                if call.kind != "function"
                    || !name(&call.function.name)
                    || call.id.trim().is_empty()
                    || call.id.len() > 512
                    || !seen.insert(call.id.clone())
                {
                    return Err(invalid("历史工具调用类型、名称或ID无效"));
                }
                let arguments: Value = serde_json::from_str(&call.function.arguments)
                    .map_err(|_| invalid("历史工具参数不是完整JSON"))?;
                if !arguments.is_object() {
                    return Err(invalid("历史工具参数必须为对象"));
                }
                pending.insert(call.id.clone());
            }
        }
        messages.push(value);
    }
    if !pending.is_empty() {
        return Err(invalid("请求末尾仍有未返回结果的工具调用"));
    }
    let system = req
        .messages
        .iter()
        .take_while(|m| m.role == Role::System)
        .map(|m| m.content_text())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut parameters = Map::new();
    if let Some(tokens) = req.max_tokens {
        if tokens == 0 {
            return Err(invalid("max_tokens必须大于0"));
        }
        if model
            .max_output_tokens
            .is_some_and(|maximum| tokens > maximum)
        {
            return Err(invalid("max_tokens超过目录声明的输出上限"));
        }
        parameters.insert("max_tokens".into(), json!(tokens));
    }
    for (field, value, upper) in [
        ("temperature", req.temperature, 2.0),
        ("top_p", req.top_p, 1.0),
    ] {
        if let Some(value) = value {
            if !value.is_finite() || !(0.0..=upper).contains(&value) {
                return Err(invalid("采样参数超出范围"));
            }
            parameters.insert(field.into(), json!(value));
        }
    }
    if let Some(stop) = &req.stop {
        if stop.len() > 64 || stop.iter().any(|v| v.is_empty() || v.len() > 4096) {
            return Err(invalid("stop无效"));
        }
        parameters.insert("stop".into(), json!(stop));
    }
    if let Some(choice) = &req.tool_choice {
        parameters.insert("tool_choice".into(), tool_choice(choice, &names)?);
    }
    let reasoning = reasoning(req)?;
    if (reasoning.get("enable_thinking") == Some(&json!(true))
        || reasoning.contains_key("reasoning_effort"))
        && model.thinking != Some(true)
    {
        return Err(invalid("所选模型尚未声明支持思考"));
    }
    parameters.extend(reasoning);
    let business_name = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content_text().chars().take(10).collect::<String>())
        .unwrap_or_default();
    let mut body = json!({
        "request_id":request,"request_set_id":request,"chat_record_id":request,"session_id":session,
        "business":{"product":"cli","type":"agent","version":"1.0.51","id":Uuid::new_v4(),"name":business_name,"begin_at":started_ms,"stage":"start"},
        "stream":true,"chat_task":"FREE_INPUT","chat_context":{},"is_reply":true,"is_retry":false,
        "source":1,"version":"3","agent_id":"agent_common","task_id":"common","session_type":"qodercli",
        "aliyun_user_type":"","custom_model":null,"system":system,"messages":messages,"tools":tools,
        "model_config":{"key":model.id,"format":model.format,"source":"system","enable":true,"display_name":model.name,"is_vl":model.vision,"is_reasoning":model.thinking},
        "parameters":parameters
    });
    for (field, known) in [("is_vl", model.vision), ("is_reasoning", model.thinking)] {
        if known.is_none() {
            body["model_config"]
                .as_object_mut()
                .ok_or_else(|| invalid("模型配置无效"))?
                .remove(field);
        }
    }
    if serde_json::to_vec(&body)
        .map_err(|_| invalid("请求编码失败"))?
        .len()
        > BODY_LIMIT
    {
        return Err(invalid("请求超过本地4MiB限制"));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> CatalogModel {
        CatalogModel {
            id: "synthetic-model".into(),
            name: "Synthetic".into(),
            format: "openai".into(),
            enabled: Some(true),
            vision: Some(true),
            thinking: Some(true),
            free: None,
            credit_multiplier: Some(1.0),
            context_window: None,
            max_output_tokens: None,
        }
    }
    #[test]
    fn declared_output_limit_rejects_overflow_and_unknown_limit_does_not_invent_a_bound() {
        let mut req = request(json!([{"role":"user","content":"synthetic"}]));
        let mut metadata = model();
        metadata.max_output_tokens = Some(32);
        assert_eq!(
            body(&req, &metadata).unwrap()["parameters"]["max_tokens"],
            32
        );
        req.max_tokens = Some(33);
        assert!(body(&req, &metadata).is_err());
        metadata.max_output_tokens = None;
        assert_eq!(
            body(&req, &metadata).unwrap()["parameters"]["max_tokens"],
            33
        );
        req.max_tokens = Some(0);
        assert!(body(&req, &metadata).is_err());
    }
    fn request(messages: Value) -> ChatRequest {
        serde_json::from_value(
            json!({"model":"gateway-alias","messages":messages,"stream":false,"max_tokens":32}),
        )
        .unwrap()
    }
    fn body(req: &ChatRequest, metadata: &CatalogModel) -> Result<Value> {
        build(
            req,
            metadata,
            Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
            1700000000000,
        )
    }
    #[test]
    fn full_history_tools_and_image_details_survive_without_flattening_or_reordering() {
        let messages = json!([
            {"role":"system","content":"第一条约束"},
            {"role":"system","content":[{"type":"text","text":"第二条约束"}]},
            {"role":"user","content":"旧问题"},
            {"role":"assistant","content":"旧回答"},
            {"role":"assistant","content":"","tool_calls":[{"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{\"text\":\"原参数\"}"}}]},
            {"role":"tool","tool_call_id":"call-1","content":"原工具结果"},
            {"role":"system","content":"后续约束"},
            {"role":"user","content":[{"type":"text","text":"带图片的问题"},{"type":"image_url","image_url":{"url":"https://example.invalid/synthetic.png","detail":"high"}}]}
        ]);
        let mut req = request(messages.clone());
        req.tools = Some(
            json!([{"type":"function","function":{"name":"lookup","description":"原描述","parameters":{"type":"object","properties":{"text":{"type":"string"}}}}}]),
        );
        req.tool_choice = Some(json!({"type":"tool","name":"lookup"}));
        req.thinking = Some(json!({"type":"enabled","budget_tokens":8192}));
        let result = body(&req, &model()).unwrap();
        assert_eq!(
            result["messages"].as_array().unwrap().len(),
            messages.as_array().unwrap().len()
        );
        for (index, original) in messages.as_array().unwrap().iter().enumerate() {
            for (key, value) in original.as_object().unwrap() {
                assert_eq!(&result["messages"][index][key], value);
            }
        }
        assert_eq!(result["system"], "第一条约束\n\n第二条约束");
        assert_eq!(result["tools"], req.tools.unwrap());
        assert_eq!(
            result["parameters"]["tool_choice"],
            json!({"type":"function","function":{"name":"lookup"}})
        );
        assert_eq!(result["parameters"]["enable_thinking"], true);
        assert_eq!(result["parameters"]["reasoning_budget_tokens"], 8192);
        assert!(result["parameters"].get("reasoning_effort").is_none());
        assert_eq!(result["parameters"]["max_tokens"], 32);
        assert_eq!(result["model_config"]["key"], "synthetic-model");
        assert_eq!(result["request_id"], result["chat_record_id"]);
        assert_eq!(result["request_id"], result["request_set_id"]);
        assert_eq!(result["business"]["name"], "带图片的问题");
        assert_eq!(result["stream"], true);
        assert_eq!(result["is_retry"], false);
        assert_eq!(result["business"]["begin_at"], 1700000000000_i64);
    }
    #[test]
    fn invalid_tool_history_is_rejected_instead_of_repaired_or_dropped() {
        for messages in [
            json!([{"role":"tool","tool_call_id":"unknown","content":"orphan"}]),
            json!([{"role":"assistant","content":"","tool_calls":[{"id":"a","type":"function","function":{"name":"f","arguments":"{}"}}]},{"role":"user","content":"missing tool result"}]),
            json!([{"role":"assistant","content":"","tool_calls":[{"id":"a","type":"function","function":{"name":"f","arguments":"{"}}]},{"role":"tool","tool_call_id":"a","content":"result"}]),
            json!([{"role":"user","content":"x","tool_call_id":"invented"}]),
        ] {
            assert!(body(&request(messages), &model()).is_err());
        }
    }
    #[test]
    fn unsupported_media_unknown_capabilities_and_disabled_models_fail_before_sending() {
        let text = request(json!([{"role":"user","content":"hello"}]));
        for enabled in [Some(false), None] {
            let mut metadata = model();
            metadata.enabled = enabled;
            assert!(body(&text, &metadata).is_err());
        }
        for part in [
            json!({"type":"input_audio","input_audio":{"data":"synthetic","format":"wav"}}),
            json!({"type":"video_url","video_url":{"url":"https://example.invalid/video"}}),
        ] {
            assert!(body(
                &request(json!([{"role":"user","content":[part]}])),
                &model()
            )
            .is_err());
        }
        let image = request(
            json!([{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.invalid/image"}}]}]),
        );
        for vision in [Some(false), None] {
            let mut metadata = model();
            metadata.vision = vision;
            assert!(body(&image, &metadata).is_err());
        }
        let mut unknown = model();
        unknown.vision = None;
        unknown.thinking = None;
        let config = body(&text, &unknown).unwrap()["model_config"].clone();
        assert!(config.get("is_vl").is_none() && config.get("is_reasoning").is_none());
    }
    #[test]
    fn image_input_validation_accepts_inline_data_and_rejects_bad_urls_or_encoding() {
        for (url, valid) in [
            ("data:image/png;base64,AQID", true),
            ("https://example.invalid/image", true),
            ("https://", false),
            ("https://user:password@example.invalid/image", false),
            ("data:image/png;base64,!", false),
            ("data:image/png;base64,", false),
            ("file:///private.png", false),
        ] {
            let req = request(
                json!([{"role":"user","content":[{"type":"image_url","image_url":{"url":url}}]}]),
            );
            assert_eq!(body(&req, &model()).is_ok(), valid, "{url}");
        }
    }
    #[test]
    fn reasoning_mapping_preserves_disable_and_rejects_conflicts_and_unknown_intent() {
        let mut req = request(json!([{"role":"user","content":"hello"}]));
        req.thinking = Some(json!({"type":"disabled"}));
        assert_eq!(
            body(&req, &model()).unwrap()["parameters"]["enable_thinking"],
            false
        );
        req.extra.insert("reasoning_effort".into(), json!("high"));
        assert!(body(&req, &model()).is_err());
        req.thinking = None;
        assert_eq!(
            body(&req, &model()).unwrap()["parameters"]["reasoning_effort"],
            "high"
        );
        let mut metadata = model();
        metadata.thinking = None;
        assert!(body(&req, &metadata).is_err());
        req.extra.clear();
        for value in [
            json!({"type":"enabled"}),
            json!({"type":"adaptive"}),
            json!({"effort":"invented"}),
            json!({"budget_tokens":1024}),
            json!({"effort":"high","unexpected":true}),
        ] {
            req.thinking = Some(value);
            assert!(body(&req, &model()).is_err());
        }
    }
    #[test]
    fn explicit_thinking_disable_uses_the_sdk_boolean_without_a_synthetic_effort() {
        let mut req = request(json!([{"role":"user","content":"synthetic"}]));
        req.thinking = Some(json!({"type":"disabled"}));
        let parameters = body(&req, &model()).unwrap()["parameters"].clone();
        assert_eq!(parameters["enable_thinking"], false);
        assert!(parameters.get("reasoning_effort").is_none());
        let mut metadata = model();
        metadata.thinking = None;
        assert_eq!(
            body(&req, &metadata).unwrap()["parameters"]["enable_thinking"],
            false
        );
    }
    #[test]
    fn enabled_thinking_keeps_the_exact_budget_and_an_independent_explicit_effort() {
        let mut req = request(json!([{"role":"user","content":"synthetic"}]));
        req.thinking = Some(json!({"type":"enabled","budget_tokens":4321}));
        let parameters = body(&req, &model()).unwrap()["parameters"].clone();
        assert_eq!(parameters["enable_thinking"], true);
        assert_eq!(parameters["reasoning_budget_tokens"], 4321);
        assert!(parameters.get("reasoning_effort").is_none());
        req.extra.insert("reasoning_effort".into(), json!("high"));
        let parameters = body(&req, &model()).unwrap()["parameters"].clone();
        assert_eq!(parameters["reasoning_budget_tokens"], 4321);
        assert_eq!(parameters["reasoning_effort"], "high");
        let mut metadata = model();
        metadata.thinking = None;
        assert!(body(&req, &metadata).is_err());
    }
    #[test]
    fn extension_or_auth_target_override_and_invalid_sampling_are_rejected() {
        let mut req = request(json!([{"role":"user","content":"hello"}]));
        for field in [
            "base_url",
            "Authorization",
            "headers",
            "api_key",
            "response_format",
            "n",
        ] {
            req.extra.insert(field.into(), json!("synthetic-private"));
            let error = body(&req, &model()).unwrap_err();
            assert!(!error.to_string().contains("synthetic-private"));
            req.extra.clear();
        }
        req.extra
            .insert("stream_options".into(), json!({"include_usage":true}));
        req.extra.insert("user".into(), json!("local-session"));
        let result = body(&req, &model()).unwrap();
        assert!(result.get("user").is_none());
        assert!(result.get("stream_options").is_none());
        req.extra.clear();
        for value in [f32::NAN, f32::INFINITY, -0.1, 2.1] {
            req.temperature = Some(value);
            assert!(body(&req, &model()).is_err());
        }
        req.temperature = None;
        req.top_p = Some(1.1);
        assert!(body(&req, &model()).is_err());
        req.top_p = None;
        req.max_tokens = Some(0);
        assert!(body(&req, &model()).is_err());
    }
    #[test]
    fn duplicate_tool_definitions_invalid_selection_and_oversized_context_are_rejected() {
        let mut req = request(json!([{"role":"user","content":"hello"}]));
        let tool =
            json!({"type":"function","function":{"name":"f","parameters":{"type":"object"}}});
        req.tools = Some(json!([tool.clone(), tool.clone()]));
        assert!(body(&req, &model()).is_err());
        req.tools = Some(json!([tool]));
        req.tool_choice = Some(json!({"type":"tool","name":"unknown"}));
        assert!(body(&req, &model()).is_err());
        req.tools = None;
        req.tool_choice = None;
        req.messages[0].content = Content::Text("x".repeat(BODY_LIMIT));
        assert!(body(&req, &model()).is_err());
    }
}
