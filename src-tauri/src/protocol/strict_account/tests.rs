use super::*;
use serde_json::json;

fn request() -> Value {
    json!({"model":"exact-alias", "messages":[{"role":"user", "content":" hello\n世界 "}]})
}
fn definition() -> Value {
    json!({"type":"function", "function":{"name":"lookup", "description":" exactly\n", "parameters":{"type":"object", "properties":{"q":{"type":"string"}}, "required":["q"]}}})
}
fn call(id: &str) -> Value {
    json!({"id":id,"type":"function","function":{"name":"lookup","arguments":" {\n \"q\": \" a \", \"n\": 1.0 } "}})
}
fn with_calls() -> Value {
    json!({"model":"exact-alias", "messages":[{"role":"assistant", "content":null,"tool_calls":[call("call-a")]}]})
}
fn reject(value: &Value, code: &str, path: &str) {
    let error = parse_trae_openai(value)
        .expect_err("strict ingress must reject this value")
        .to_string();
    assert!(
        error.contains(&format!("native_request_{code} at {path}")),
        "unexpected safe error: {error}"
    );
}

// RED uses only this explicitly selected regression group and the unchanged old
// converter. The normal test suite always exercises the strict parser. Root
// serializes Cargo ownership before setting this test-only environment switch.
fn regression_parse(value: &Value) -> Result<ChatRequest> {
    if std::env::var_os("LLMGW_STRICT_ACCOUNT_RED").is_some() {
        let request: crate::protocol::openai::OaChatRequest =
            serde_json::from_value(value.clone()).unwrap();
        request.to_internal()
    } else {
        parse_trae_openai(value).map(NativeChatRequest::into_request)
    }
}

#[test]
fn strict_ingress_regression_unknown_part_must_not_disappear() {
    let value = json!({"model":"exact-alias","messages":[{"role":"user","content":[{"type":"text","text":"first"},{"type":"future-part","payload":"private"},{"type":"text","text":"last"}]}]});
    assert!(
        regression_parse(&value).is_err(),
        "unknown parts must fail before a converter drops them"
    );
}
#[test]
fn strict_ingress_regression_conflicting_max_must_not_choose_first() {
    let mut value = request();
    value["max_tokens"] = json!(17);
    value["max_completion_tokens"] = json!(18);
    assert!(
        regression_parse(&value).is_err(),
        "conflicting explicit output constraints must fail"
    );
}
#[test]
fn strict_ingress_regression_developer_must_not_remap_to_system() {
    let mut value = request();
    value["messages"][0]["role"] = json!("developer");
    assert!(
        regression_parse(&value).is_err(),
        "unsupported developer role must not become system"
    );
}
#[test]
fn strict_ingress_regression_tool_result_error_must_not_disappear() {
    let mut value = with_calls();
    value["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"tool","tool_call_id":"call-a","content":"result","is_error":true}));
    assert!(
        regression_parse(&value).is_err(),
        "unsupported tool error semantics must not disappear"
    );
}

#[test]
fn desktop_shape_preserves_bytes_and_has_no_invented_defaults() {
    let mut value = request();
    value["max_tokens"] = json!(127);
    let result = parse_trae_openai(&value).unwrap();
    assert_eq!(result.model, "exact-alias");
    assert_eq!(result.messages, vec![Message::user(" hello\n世界 ")]);
    assert_eq!(result.max_tokens, Some(127));
    assert!(!result.stream);
    assert!(result.extra.is_empty());
    assert!(result.tools.is_none());
    assert!(result.temperature.is_none() && result.top_p.is_none() && result.thinking.is_none());
    assert_eq!(parse_trae_openai(&request()).unwrap().max_tokens, None);
}

#[test]
fn complete_history_preserves_intermediate_system_parts_calls_and_results() {
    let image = "https://example.invalid/a%20b.png?sig=exact%2Bbytes";
    let value = json!({"model":"exact-alias","messages":[
        {"role":"system","content":" first system \n"},
        {"role":"user","name":"caller","content":[{"type":"text","text":"  before\n"},{"type":"image_url","image_url":{"url":image,"detail":"high"}},{"type":"text","text":"after  "}]},
        {"role":"assistant","content":null,"tool_calls":[call("call-a"),call("call-b")]},
        {"role":"system","content":" intermediate system "},
        {"role":"tool","tool_call_id":"call-b","name":"lookup","content":[{"type":"text","text":"result B\n"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AQID","detail":"low"}}]},
        {"role":"tool","tool_call_id":"call-a","content":" result A "},
        {"role":"assistant","content":"final"}
    ]});
    let result = parse_trae_openai(&value).unwrap();
    assert_eq!(result.messages.len(), 7);
    assert_eq!(result.messages[0], Message::system(" first system \n"));
    assert_eq!(result.messages[3], Message::system(" intermediate system "));
    assert_eq!(result.messages[1].name.as_deref(), Some("caller"));
    assert_eq!(
        result.messages[1].content,
        Content::Parts(vec![
            Part::Text {
                text: "  before\n".into()
            },
            Part::ImageUrl {
                image_url: ImageUrl {
                    url: image.into(),
                    detail: Some("high".into())
                }
            },
            Part::Text {
                text: "after  ".into()
            }
        ])
    );
    let calls = result.messages[2].tool_calls.as_ref().unwrap();
    assert_eq!(calls[0].id, "call-a");
    assert_eq!(calls[1].id, "call-b");
    assert_eq!(
        calls[0].function.arguments,
        " {\n \"q\": \" a \", \"n\": 1.0 } "
    );
    assert_eq!(calls[0].function.name, "lookup");
    assert_eq!(calls[0].kind, "function");
    assert_eq!(result.messages[4].role, Role::Tool);
    assert_eq!(result.messages[4].tool_call_id.as_deref(), Some("call-b"));
    assert_eq!(result.messages[4].name.as_deref(), Some("lookup"));
    assert_eq!(
        result.messages[4].content,
        Content::Parts(vec![
            Part::Text {
                text: "result B\n".into()
            },
            Part::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,AQID".into(),
                    detail: Some("low".into())
                }
            }
        ])
    );
    assert_eq!(
        result.messages[5].content,
        Content::Text(" result A ".into())
    );
}

#[test]
fn supported_options_retain_exact_structure() {
    let mut value = request();
    value["tools"] = json!([definition()]);
    value["tool_choice"] = json!({"type":"function","function":{"name":"lookup"}});
    value["parallel_tool_calls"] = json!(false);
    value["reasoning_effort"] = json!("medium");
    value["stream"] = json!(true);
    value["stream_options"] = json!({"include_usage":false});
    value["user"] = json!(" tenant ");
    let result = parse_trae_openai(&value).unwrap();
    assert_eq!(result.tools, Some(value["tools"].clone()));
    assert_eq!(result.tool_choice, Some(value["tool_choice"].clone()));
    assert!(result.stream);
    assert_eq!(result.extra.len(), 4);
    for key in [
        "parallel_tool_calls",
        "reasoning_effort",
        "stream_options",
        "user",
    ] {
        assert_eq!(result.extra[key], value[key]);
    }
}

#[test]
fn max_aliases_are_positive_u32_equal_or_absent() {
    for key in ["max_tokens", "max_completion_tokens"] {
        for bad in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!(4294967296_u64),
            json!(true),
            Value::Null,
            json!("12"),
        ] {
            let mut value = request();
            value[key] = bad;
            reject(&value, "invalid_value", &format!("$.{key}"));
        }
        let mut value = request();
        value[key] = json!(u32::MAX);
        assert_eq!(
            parse_trae_openai(&value).unwrap().max_tokens,
            Some(u32::MAX)
        );
    }
    let mut value = request();
    value["max_tokens"] = json!(18);
    value["max_completion_tokens"] = json!(18);
    assert_eq!(parse_trae_openai(&value).unwrap().max_tokens, Some(18));
}

#[test]
fn unsupported_constraints_including_null_fail_explicitly() {
    for key in [
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
    ] {
        for field in [json!({"sensitive":"do-not-echo"}), Value::Null] {
            let mut value = request();
            value[key] = field;
            reject(&value, "unsupported_constraint", &format!("$.{key}"));
        }
    }
}

#[test]
fn unknown_and_override_fields_fail_without_echoing_key_or_value() {
    for key in [
        "headers",
        "host",
        "function",
        "config_name",
        "metadata",
        "model_override",
        "SECRET-key-content",
    ] {
        let mut value = request();
        value[key] = json!("SECRET-value-content");
        let error = parse_trae_openai(&value).unwrap_err().to_string();
        assert_eq!(error, "协议转换失败: native_request_unsupported_field at $");
        assert!(!error.contains(key));
        assert!(!error.contains("SECRET-value-content"));
        value[key] = Value::Null;
        reject(&value, "unsupported_field", "$");
    }
}

#[test]
fn nested_unknown_fields_never_echo_untrusted_path_components() {
    let mut value = request();
    value["messages"][0]["SECRET.message"] = json!("secret");
    reject(&value, "unsupported_field", "$.messages[0]");
    let mut value = with_calls();
    value["messages"][0]["tool_calls"][0]["function"]["SECRET.arguments"] = json!(true);
    reject(
        &value,
        "unsupported_field",
        "$.messages[0].tool_calls[0].function",
    );
    let mut value = request();
    value["tools"] = json!([definition()]);
    value["tools"][0]["function"]["strict"] = json!(true);
    reject(&value, "unsupported_field", "$.tools[0].function");
}

#[test]
fn roles_and_content_shapes_are_strict() {
    for role in ["developer", "function", "unknown", "USER", ""] {
        let mut value = request();
        value["messages"][0]["role"] = json!(role);
        reject(&value, "unsupported_role", "$.messages[0].role");
    }
    for bad in [Value::Null, json!(2), json!({}), json!(true)] {
        let mut value = request();
        value["messages"][0]["content"] = bad;
        reject(&value, "invalid_type", "$.messages[0].content");
    }
    let mut value = request();
    value["messages"][0]
        .as_object_mut()
        .unwrap()
        .remove("content");
    reject(&value, "missing_field", "$.messages[0].content");
    let mut value = request();
    value["messages"][0]["content"] = json!([]);
    reject(&value, "limit_exceeded", "$.messages[0].content");
}

#[test]
fn assistant_null_or_absent_requires_nonempty_calls() {
    let mut value = with_calls();
    assert!(parse_trae_openai(&value).is_ok());
    value["messages"][0]
        .as_object_mut()
        .unwrap()
        .remove("content");
    assert!(parse_trae_openai(&value).is_ok());
    value["messages"][0]["tool_calls"] = json!([]);
    reject(&value, "limit_exceeded", "$.messages[0].tool_calls");
    value["messages"][0]
        .as_object_mut()
        .unwrap()
        .remove("tool_calls");
    reject(&value, "missing_field", "$.messages[0].content");
    value["messages"][0]["content"] = Value::Null;
    reject(&value, "invalid_type", "$.messages[0].content");
}

#[test]
fn absent_null_and_empty_optional_values_have_documented_behavior() {
    for key in [
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "reasoning_effort",
        "stream_options",
        "user",
        "stream",
    ] {
        let mut value = request();
        value[key] = Value::Null;
        assert!(parse_trae_openai(&value).is_err(), "null {key} must fail");
    }
    let mut value = request();
    value["tools"] = json!([]);
    value["stream_options"] = json!({});
    value["user"] = json!("");
    let result = parse_trae_openai(&value).unwrap();
    assert_eq!(result.tools, Some(json!([])));
    assert_eq!(result.extra["stream_options"], json!({}));
    for key in ["name", "tool_calls", "tool_call_id"] {
        let mut value = request();
        value["messages"][0][key] = Value::Null;
        assert!(parse_trae_openai(&value).is_err());
    }
}

#[test]
fn orphan_duplicate_and_reused_tool_id_are_rejected() {
    let mut value = with_calls();
    value["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"tool","tool_call_id":"orphan","content":"x"}));
    reject(&value, "orphan_tool_result", "$.messages[1].tool_call_id");
    value["messages"][1]["tool_call_id"] = json!("call-a");
    assert!(parse_trae_openai(&value).is_ok());
    let result = value["messages"][1].clone();
    value["messages"].as_array_mut().unwrap().push(result);
    reject(&value, "orphan_tool_result", "$.messages[2].tool_call_id");
    value["messages"][2] = json!({"role":"assistant","tool_calls":[call("call-a")]});
    reject(
        &value,
        "duplicate_tool_call",
        "$.messages[2].tool_calls[0].id",
    );
    let mut value = with_calls();
    value["messages"][0]["tool_calls"] = json!([call("call-a"), call("call-a")]);
    reject(
        &value,
        "duplicate_tool_call",
        "$.messages[0].tool_calls[1].id",
    );
}

#[test]
fn role_specific_tool_fields_and_unsupported_message_semantics_fail() {
    for role in ["user", "system", "tool"] {
        let mut value = with_calls();
        value["messages"][0]["role"] = json!(role);
        reject(&value, "invalid_role_field", "$.messages[0].tool_calls");
    }
    let mut value = request();
    value["messages"][0]["tool_call_id"] = json!("call-a");
    reject(&value, "invalid_role_field", "$.messages[0].tool_call_id");
    for key in ["is_error", "refusal", "audio", "function_call"] {
        let mut value = request();
        value["messages"][0][key] = json!(false);
        reject(&value, "unsupported_field", "$.messages[0]");
    }
}

#[test]
fn arguments_are_original_object_json_and_bounded() {
    for arguments in [
        json!("[]"),
        json!("null"),
        json!("1"),
        json!("{bad}"),
        json!({}),
        Value::Null,
    ] {
        let mut value = with_calls();
        value["messages"][0]["tool_calls"][0]["function"]["arguments"] = arguments;
        assert!(parse_trae_openai(&value).is_err());
    }
    let mut value = with_calls();
    value["messages"][0]["tool_calls"][0]["function"]["arguments"] =
        json!(format!("{{\"x\":\"{}\"}}", "a".repeat(ARGUMENT_BYTES)));
    reject(
        &value,
        "limit_exceeded",
        "$.messages[0].tool_calls[0].function.arguments",
    );
}

#[test]
fn tool_definitions_require_unique_function_names_and_object_schemas() {
    let mut value = request();
    value["tools"] = json!([definition(), definition()]);
    reject(&value, "duplicate_tool", "$.tools[1].function.name");
    for bad in [json!([]), Value::Null, json!("schema")] {
        let mut value = request();
        value["tools"] = json!([definition()]);
        value["tools"][0]["function"]["parameters"] = bad;
        reject(&value, "invalid_type", "$.tools[0].function.parameters");
    }
    let mut value = request();
    value["tools"] = json!([definition()]);
    value["tools"][0]["type"] = json!("custom");
    reject(&value, "unsupported_field", "$.tools[0].type");
    for name in ["", "bad name", "bad.name", "非函数"] {
        let mut value = request();
        value["tools"] = json!([definition()]);
        value["tools"][0]["function"]["name"] = json!(name);
        reject(&value, "invalid_value", "$.tools[0].function.name");
    }
}

#[test]
fn tool_choice_and_parallel_require_consistent_tools() {
    let mut value = request();
    value["tool_choice"] = json!("none");
    assert_eq!(
        parse_trae_openai(&value).unwrap().tool_choice,
        Some(json!("none"))
    );
    for choice in ["auto", "required"] {
        value["tool_choice"] = json!(choice);
        reject(&value, "missing_tools", "$.tool_choice");
        value["tools"] = json!([definition()]);
        assert!(parse_trae_openai(&value).is_ok());
        value.as_object_mut().unwrap().remove("tools");
    }
    value["tool_choice"] = json!({"type":"function","function":{"name":"missing"}});
    value["tools"] = json!([definition()]);
    reject(&value, "unknown_tool", "$.tool_choice.function.name");
    value["tool_choice"] = json!({"type":"function","function":{"name":"lookup"},"extra":false});
    reject(&value, "unsupported_field", "$.tool_choice");
    value.as_object_mut().unwrap().remove("tool_choice");
    value["parallel_tool_calls"] = json!(true);
    assert!(parse_trae_openai(&value).is_ok());
    value["parallel_tool_calls"] = json!(1);
    reject(&value, "invalid_type", "$.parallel_tool_calls");
    value["parallel_tool_calls"] = json!(false);
    value["tools"] = json!([]);
    reject(&value, "missing_tools", "$.parallel_tool_calls");
}

#[test]
fn image_urls_must_be_supported_secure_and_preserved() {
    for url in [
        "http://example.invalid/x",
        "https://user:pass@example.invalid/x",
        "https://@example.invalid/x",
        "https://example.invalid/x#secret",
        "file:///x",
        "data:image/svg+xml;base64,AQID",
        "data:image/png;base64,%%%",
        "data:image/png;base64,",
        "https://example.invalid/a b",
        "https://example.invalid\\@evil.invalid/x",
    ] {
        let mut value = request();
        value["messages"][0]["content"] = json!([{"type":"image_url","image_url":{"url":url}}]);
        assert!(
            parse_trae_openai(&value).is_err(),
            "invalid URL must fail without fetching: {url}"
        );
    }
    for mime in ["png", "jpeg", "gif", "webp"] {
        let mut value = request();
        let url = format!("data:image/{mime};base64,AQID");
        value["messages"][0]["content"] = json!([{"type":"image_url","image_url":{"url":url}}]);
        let parsed = parse_trae_openai(&value).unwrap();
        assert_eq!(
            parsed.messages[0].content,
            Content::Parts(vec![Part::ImageUrl {
                image_url: ImageUrl { url, detail: None }
            }])
        );
    }
}

#[test]
fn part_wrappers_details_and_types_are_never_coerced() {
    for bad in [
        json!({"text":"x"}),
        json!({"type":"text","text":3}),
        json!({"type":"text","text":"x","extra":null}),
        json!({"type":"image_url","image_url":"https://example.invalid/x"}),
        json!({"type":"image_url","image_url":{"url":"https://example.invalid/x","detail":"original"}}),
        json!({"type":"input_audio","input_audio":{}}),
        json!({"type":"video_url","video_url":{}}),
        json!({"type":"file","file":{}}),
    ] {
        let mut value = request();
        value["messages"][0]["content"] = json!([bad]);
        assert!(parse_trae_openai(&value).is_err());
    }
}

#[test]
fn identifiers_and_stream_options_are_validated_without_normalizing() {
    for alias in ["", " alias", "alias ", "bad\nmodel"] {
        let mut value = request();
        value["model"] = json!(alias);
        reject(&value, "invalid_value", "$.model");
    }
    for bad in [
        json!({"include_usage":null}),
        json!({"include_usage":1}),
        json!({"unknown":true}),
        json!([]),
    ] {
        let mut value = request();
        value["stream_options"] = bad;
        assert!(parse_trae_openai(&value).is_err());
    }
    for bad in [json!("false"), json!(0), Value::Null] {
        let mut value = request();
        value["stream"] = bad;
        reject(&value, "invalid_type", "$.stream");
    }
}

#[test]
fn request_schema_argument_depth_and_counts_are_bounded() {
    let mut value = request();
    value["messages"][0]["content"] = json!("x".repeat(REQUEST_BYTES));
    reject(&value, "limit_exceeded", "$");
    let mut value = request();
    value["tools"] = json!([definition()]);
    value["tools"][0]["function"]["parameters"] = json!({"description":"x".repeat(SCHEMA_BYTES)});
    reject(&value, "limit_exceeded", "$.tools[0].function.parameters");
    let mut value = with_calls();
    value["messages"][0]["tool_calls"] = Value::Array(
        (0..MAX_CALLS + 1)
            .map(|i| call(&format!("id-{i}")))
            .collect(),
    );
    reject(&value, "limit_exceeded", "$.messages[0].tool_calls");
    let mut nested = json!({});
    for _ in 0..MAX_DEPTH + 1 {
        nested = json!({"nested":nested});
    }
    let mut value = with_calls();
    value["messages"][0]["tool_calls"][0]["function"]["arguments"] = json!(nested.to_string());
    reject(
        &value,
        "limit_exceeded",
        "$.messages[0].tool_calls[0].function.arguments",
    );
    let mut value = request();
    value["tools"] = json!([definition()]);
    value["tools"][0]["function"]["parameters"] = nested;
    reject(&value, "limit_exceeded", "$");
    let mut value = request();
    value["messages"] = Value::Array(vec![json!({"role":"user","content":"x"}); MAX_MESSAGES + 1]);
    reject(&value, "limit_exceeded", "$.messages");
}

#[test]
fn exact_limit_and_outstanding_final_calls_are_accepted() {
    let mut value = with_calls();
    value["messages"][0]["tool_calls"] =
        Value::Array((0..MAX_CALLS).map(|i| call(&format!("id-{i}"))).collect());
    assert_eq!(
        parse_trae_openai(&value).unwrap().messages[0]
            .tool_calls
            .as_ref()
            .unwrap()
            .len(),
        MAX_CALLS
    );
    let mut value = with_calls();
    let arguments = format!("{{\"x\":\"{}\"}}", "a".repeat(ARGUMENT_BYTES - 8));
    assert_eq!(arguments.len(), ARGUMENT_BYTES);
    value["messages"][0]["tool_calls"][0]["function"]["arguments"] = json!(arguments);
    assert!(parse_trae_openai(&value).is_ok());
}

#[test]
fn schema_limit_applies_to_all_function_schemas_together() {
    let mut first = definition();
    first["function"]["parameters"] = json!({"description":"x".repeat(SCHEMA_BYTES / 2)});
    let mut second = first.clone();
    second["function"]["name"] = json!("second");
    let mut value = request();
    value["tools"] = json!([first, second]);
    reject(&value, "limit_exceeded", "$.tools");
}

#[test]
fn image_decoded_byte_limit_is_enforced_at_the_boundary() {
    let encoded = base64::engine::general_purpose::STANDARD.encode(vec![1_u8; IMAGE_BYTES]);
    let mut value = request();
    value["messages"][0]["content"] = json!([{"type":"image_url","image_url":{"url":format!("data:image/png;base64,{encoded}")}}]);
    assert!(parse_trae_openai(&value).is_ok());
    let encoded = base64::engine::general_purpose::STANDARD.encode(vec![1_u8; IMAGE_BYTES + 1]);
    value["messages"][0]["content"][0]["image_url"]["url"] =
        json!(format!("data:image/png;base64,{encoded}"));
    reject(
        &value,
        "limit_exceeded",
        "$.messages[0].content[0].image_url.url",
    );
}

#[test]
fn missing_nested_fields_and_null_definitions_are_rejected() {
    for key in ["id", "type", "function"] {
        let mut value = with_calls();
        value["messages"][0]["tool_calls"][0]
            .as_object_mut()
            .unwrap()
            .remove(key);
        reject(
            &value,
            "missing_field",
            &format!("$.messages[0].tool_calls[0].{key}"),
        );
    }
    for key in ["name", "arguments"] {
        let mut value = with_calls();
        value["messages"][0]["tool_calls"][0]["function"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        reject(
            &value,
            "missing_field",
            &format!("$.messages[0].tool_calls[0].function.{key}"),
        );
    }
    for key in ["name", "parameters"] {
        let mut value = request();
        value["tools"] = json!([definition()]);
        value["tools"][0]["function"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        reject(
            &value,
            "missing_field",
            &format!("$.tools[0].function.{key}"),
        );
    }
    let mut value = request();
    value["tools"] = json!([definition()]);
    value["tools"][0]["function"]["description"] = Value::Null;
    reject(&value, "invalid_type", "$.tools[0].function.description");
}

#[test]
fn top_level_messages_parts_and_tools_shapes_are_bounded() {
    for bad in [Value::Null, json!([]), json!(true), json!("request")] {
        reject(&bad, "invalid_type", "$");
    }
    let mut value = request();
    value["messages"] = json!([]);
    reject(&value, "limit_exceeded", "$.messages");
    value["messages"] = Value::Null;
    reject(&value, "invalid_type", "$.messages");
    let mut value = request();
    value["messages"][0]["content"] =
        Value::Array(vec![json!({"type":"text","text":"x"}); MAX_PARTS + 1]);
    reject(&value, "limit_exceeded", "$.messages[0].content");
    let mut value = request();
    value["tools"] = Value::Array(
        (0..MAX_CALLS + 1)
            .map(|index| {
                let mut value = definition();
                value["function"]["name"] = json!(format!("tool-{index}"));
                value
            })
            .collect(),
    );
    reject(&value, "limit_exceeded", "$.tools");
}

#[test]
fn call_count_limit_covers_the_whole_history_including_resolved_calls() {
    let mut value = with_calls();
    value["messages"][0]["tool_calls"] =
        Value::Array((0..MAX_CALLS).map(|i| call(&format!("id-{i}"))).collect());
    value["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"tool","tool_call_id":"id-0","content":"resolved"}));
    value["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"assistant","tool_calls":[call("id-extra")]}));
    reject(&value, "limit_exceeded", "$.messages[2].tool_calls");
}

#[test]
fn identifiers_are_bounded_and_unknown_tool_call_types_fail() {
    for id in [
        "".to_owned(),
        " id".to_owned(),
        "id\nsecret".to_owned(),
        "x".repeat(257),
    ] {
        let mut value = with_calls();
        value["messages"][0]["tool_calls"][0]["id"] = json!(id);
        reject(&value, "invalid_value", "$.messages[0].tool_calls[0].id");
    }
    let mut value = with_calls();
    value["messages"][0]["tool_calls"][0]["type"] = json!("custom");
    reject(
        &value,
        "unsupported_field",
        "$.messages[0].tool_calls[0].type",
    );
    let mut value = request();
    value["messages"][0]["name"] = json!("x".repeat(65));
    reject(&value, "invalid_value", "$.messages[0].name");
    value["messages"][0].as_object_mut().unwrap().remove("name");
    value["user"] = json!("x".repeat(257));
    reject(&value, "invalid_value", "$.user");
    value.as_object_mut().unwrap().remove("user");
    value["reasoning_effort"] = json!(true);
    reject(&value, "invalid_type", "$.reasoning_effort");
}

#[test]
fn finalfix_reasoning_sidecar_preserves_absent_null_empty_unicode_and_order() {
    let mut value = request();
    value["messages"] = json!([
        {"role":"user","content":"original"},
        {"role":"assistant","content":"","reasoning_content":null},
        {"role":"assistant","content":"","reasoning_content":""},
        {"role":"assistant","content":"visible","reasoning_content":"exact\n推理 🧭\t"},
        {"role":"user","content":"followup"}]);
    let parsed = parse_trae_openai(&value).unwrap();
    assert_eq!(
        parsed.reasoning(),
        &[
            None,
            Some(None),
            Some(Some(String::new())),
            Some(Some("exact\n推理 🧭\t".into())),
            None
        ]
    );
    assert_eq!(parsed.messages[3].content, Content::Text("visible".into()));
    assert!(!parsed.extra.contains_key("reasoning_content"));
}
#[test]
fn finalfix_reasoning_rejects_wrong_roles_types_limits_and_sidecar_injection() {
    for role in ["system", "user", "tool"] {
        let mut value = request();
        value["messages"][0]["role"] = json!(role);
        value["messages"][0]["reasoning_content"] = json!("private-sentinel");
        reject(
            &value,
            "invalid_role_field",
            "$.messages[0].reasoning_content",
        );
    }
    for bad in [
        json!(42),
        json!(true),
        json!([]),
        json!({"private":"private-sentinel"}),
    ] {
        let mut value = request();
        value["messages"][0]["role"] = json!("assistant");
        value["messages"][0]["reasoning_content"] = bad;
        let error = parse_trae_openai(&value).unwrap_err().to_string();
        assert!(!error.contains("private-sentinel"));
        assert!(error.contains("invalid_type"));
    }
    for key in [
        "reasoning",
        "reasoning_content",
        "history",
        "native_history",
        "request",
    ] {
        let mut value = request();
        value[key] = json!({"model":"override","messages":[],"host":"private-sentinel"});
        assert!(parse_trae_openai(&value).is_err());
    }
    let mut value = request();
    value["messages"][0]["role"] = json!("assistant");
    value["messages"][0]["reasoning_content"] = json!("x".repeat(REQUEST_BYTES));
    reject(&value, "limit_exceeded", "$");
    let mut value = request();
    value["messages"][0]["role"] = json!("assistant");
    value["messages"][0]["refusal"] = json!("unsupported");
    reject(&value, "unsupported_field", "$.messages[0]");
}
