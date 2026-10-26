use super::super::catalog::ProtocolCarriers;
use super::*;
use crate::domain::{FunctionCall, ImageUrl, Message, ToolCall};

fn target() -> TraeNativeModelView {
    TraeNativeModelView {
        raw_model: None,
        selector_id: "ticket-selector".into(),
        config_name: "wire-exact-模型".into(),
        display_name: "display-is-not-wire".into(),
        prompt_max_tokens: Some(32000),
        max_tokens: Some(4096),
        declared_vision: Some(true),
        declared_tools: None,
        declared_thinking: None,
        reasoning_effort_options: vec!["high".into()],
        protocol_carriers: ProtocolCarriers {
            text: true,
            tools: true,
            images: true,
            audio: false,
            video: false,
        },
        account_model_evidence: "unverified",
    }
}
fn req() -> ChatRequest {
    ChatRequest {
        model: "public-alias".into(),
        messages: vec![Message::user("hello")],
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        thinking: None,
        extra: Map::new(),
    }
}
fn tool() -> Value {
    json!([{"type":"function","function":{"name":"weather","description":"保留 description\n",
    "parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}])
}
fn build(req: &ChatRequest) -> Result<PreparedNativeRequest, String> {
    build_native_request(req, &target(), SourceFunction::SoloWorkRemote)
}
fn call(id: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        kind: "function".into(),
        function: FunctionCall {
            name: "weather".into(),
            arguments: arguments.into(),
        },
    }
}

#[test]
fn exact_sealed_body_preserves_history_tool_results_images_and_schema_once() {
    let mut request = req();
    request.tools = Some(tool());
    request.max_tokens = Some(123);
    request.tool_choice = Some(json!({"type":"function","function":{"name":"weather"}}));
    request
        .extra
        .insert("parallel_tool_calls".into(), json!(false));
    request
        .extra
        .insert("reasoning_effort".into(), json!("high"));
    let mut assistant = Message::assistant("");
    assistant.tool_calls = Some(vec![call("c1", " { \"city\" : \"上海\" } ")]);
    let result = Message {
        role: Role::Tool,
        content: Content::Text("result\nraw".into()),
        tool_calls: None,
        tool_call_id: Some("c1".into()),
        name: Some("weather".into()),
    };
    let mut user = Message::user("");
    user.content = Content::Parts(vec![
        Part::Text {
            text: "before".into(),
        },
        Part::ImageUrl {
            image_url: ImageUrl {
                url: "https://example.invalid/a%20b?x=1&y=2".into(),
                detail: None,
            },
        },
        Part::Text {
            text: "after".into(),
        },
    ]);
    request.messages = vec![
        Message::system("  exact\r\n系统  "),
        Message::user("question"),
        assistant,
        result,
        user,
    ];
    let prepared = build(&request).unwrap();
    let parameters = serde_json::to_string(&tool()[0]["function"]["parameters"]).unwrap();
    assert_eq!(
        prepared.body,
        json!({"model":"wire-exact-模型","config_name":"wire-exact-模型","function":"solo_work_remote",
        "stream":true,"max_tokens":123,"tool_choice":"weather","parallel_tool_calls":false,"reasoning_effort":"high",
        "tools":[{"type":"function","function":{"name":"weather","description":"保留 description\n","parameters":parameters}}],
        "messages":[{"role":"system","content":[{"type":"text","text":"  exact\r\n系统  "}]},
            {"role":"user","content":[{"type":"text","text":"question"}]},
            {"role":"assistant","content":[{"type":"text","text":""}],"tool_calls":[{"id":"c1","type":"function",
                "function_call":{"name":"weather","arguments":" { \"city\" : \"上海\" } "}}]},
            {"role":"tool","name":"weather","tool_call_id":"c1","content":[{"type":"text","text":"result\nraw"}]},
            {"role":"user","content":[{"type":"text","text":"before"},{"type":"image_url","image_url":{"url":"https://example.invalid/a%20b?x=1&y=2"}},
                {"type":"text","text":"after"}]}]})
    );
    assert_eq!(
        prepared.constraints.tool_choice,
        NativeToolChoice::Named("weather".into())
    );
    assert_eq!(prepared.constraints.parallel_tool_calls, Some(false));
}
#[test]
fn none_keeps_tools_and_constraint_instead_of_reference_field_drop() {
    let mut r = req();
    r.tools = Some(tool());
    r.tool_choice = Some(json!("none"));
    let body = build(&r).unwrap();
    assert!(body.body["tools"].is_array());
    assert_eq!(body.body["tool_choice"], "none");
    assert_eq!(body.constraints.tool_choice, NativeToolChoice::None);
}
#[test]
fn explicit_zero_and_over_target_limits_fail_instead_of_clamping_or_omitting() {
    let mut r = req();
    for limit in [0, 4097, u32::MAX] {
        r.max_tokens = Some(limit);
        assert_eq!(build(&r).unwrap_err(), "native_max_tokens_out_of_range");
    }
    r.max_tokens = Some(4096);
    assert_eq!(build(&r).unwrap().body["max_tokens"], 4096);
    r.max_tokens = None;
    assert!(build(&r).unwrap().body.get("max_tokens").is_none());
}
#[test]
fn internal_extra_overrides_and_unknown_options_are_rejected() {
    for key in [
        "function",
        "model",
        "config_name",
        "messages",
        "stream",
        "max_tokens",
        "headers",
        "response_format",
        "stop",
        "temperature",
        "seed",
    ] {
        let mut r = req();
        r.extra.insert(key.into(), json!("override"));
        assert_eq!(
            build(&r).unwrap_err(),
            "native_request_unknown_field",
            "{key}"
        );
    }
}
#[test]
fn unsupported_typed_parameters_are_not_silently_dropped() {
    let mut variants = vec![req(); 5];
    variants[0].temperature = Some(0.0);
    variants[1].top_p = Some(1.0);
    variants[2].stop = Some(vec![]);
    variants[3].thinking = Some(json!({}));
    variants[4].thinking = Some(Value::Null);
    for r in variants {
        assert_eq!(build(&r).unwrap_err(), "native_parameter_not_supported");
    }
}
#[test]
fn native_choice_validates_exact_named_tool_and_parallel_boolean() {
    let mut r = req();
    r.tools = Some(tool());
    for choice in [
        json!("missing"),
        json!("AUTO"),
        json!({"type":"function","function":{"name":"missing"}}),
        json!({"type":"function","function":{"name":"weather","extra":1}}),
    ] {
        r.tool_choice = Some(choice);
        assert!(build(&r).is_err());
    }
    for choice in ["auto", "none", "required"] {
        r.tool_choice = Some(json!(choice));
        assert_eq!(build(&r).unwrap().body["tool_choice"], choice);
    }
    r.extra.insert("parallel_tool_calls".into(), json!(1));
    assert_eq!(build(&r).unwrap_err(), "native_parallel_invalid");
    r.tools = None;
    r.tool_choice = None;
    r.extra.insert("parallel_tool_calls".into(), json!(false));
    assert_eq!(build(&r).unwrap_err(), "native_parallel_requires_tools");
}
#[test]
fn optional_model_tool_evidence_does_not_disable_protocol_carrier() {
    let mut r = req();
    r.tools = Some(tool());
    assert!(build(&r).is_ok());
    let mut t = target();
    t.declared_tools = Some(false);
    assert_eq!(
        build_native_request(&r, &t, SourceFunction::SoloWorkRemote).unwrap_err(),
        "native_tools_not_supported"
    );
}
#[test]
fn vision_unknown_is_distinct_from_declared_false_and_valid_detail_is_preserved() {
    let mut r = req();
    r.messages[0].content = Content::Parts(vec![Part::ImageUrl {
        image_url: ImageUrl {
            url: "data:image/png;base64,AA==".into(),
            detail: None,
        },
    }]);
    let mut t = target();
    t.declared_vision = None;
    assert_eq!(
        build_native_request(&r, &t, SourceFunction::SoloWorkRemote).unwrap_err(),
        "native_vision_capability_not_declared"
    );
    t.declared_vision = Some(false);
    assert_eq!(
        build_native_request(&r, &t, SourceFunction::SoloWorkRemote).unwrap_err(),
        "native_vision_not_supported"
    );
    t.declared_vision = Some(true);
    assert!(build_native_request(&r, &t, SourceFunction::SoloWorkRemote).is_ok());
    if let Content::Parts(parts) = &mut r.messages[0].content {
        if let Part::ImageUrl { image_url } = &mut parts[0] {
            image_url.detail = Some("auto".into());
        }
    }
    assert_eq!(
        build(&r).unwrap().body["messages"][0]["content"][0]["image_url"]["detail"],
        "auto"
    );
}
#[test]
fn typed_invalid_images_are_revalidated_with_declared_and_candidate_vision() {
    use base64::Engine;
    let oversized =
        base64::engine::general_purpose::STANDARD.encode(vec![0u8; 4 * 1024 * 1024 + 1]);
    let invalid = [
        ("data:image/png;base64,%%%".to_owned(), None),
        ("data:image/svg+xml;base64,AA==".to_owned(), None),
        (
            "https://synthetic-user:synthetic-secret@example.invalid/image".to_owned(),
            None,
        ),
        ("https://example.invalid/image#fragment".to_owned(), None),
        ("https://@example.invalid/image".to_owned(), None),
        ("https://example.invalid/white space".to_owned(), None),
        ("http://example.invalid/image".to_owned(), None),
        ("".to_owned(), None),
        ("data:image/png;base64,".to_owned(), None),
        (format!("data:image/png;base64,{oversized}"), None),
        (
            format!("https://example.invalid/{}", "x".repeat(8192)),
            None,
        ),
        (
            "https://example.invalid/image".to_owned(),
            Some("ultra".to_owned()),
        ),
    ];
    for declared in [Some(true), None] {
        let mut t = target();
        t.declared_vision = declared;
        for (case, (url, detail)) in invalid.iter().enumerate() {
            let mut r = req();
            r.messages[0].content = Content::Parts(vec![Part::ImageUrl {
                image_url: ImageUrl {
                    url: url.clone(),
                    detail: detail.clone(),
                },
            }]);
            let result = build_native_request_with_options(
                &r,
                &t,
                SourceFunction::SoloWorkRemote,
                NativeRequestOptions {
                    allow_undeclared_images: true,
                },
            );
            assert!(
                matches!(result,Err(ref error) if error=="native_image_invalid"),
                "invalid typed image accepted or wrong error: case {case}, declared={declared:?}"
            );
        }
    }
}
#[test]
fn valid_image_detail_modes_preserve_exact_wrapper_and_order() {
    for detail in [None, Some("auto"), Some("low"), Some("high")] {
        for url in [
            "https://example.invalid/exact%20image?x=1&y=2",
            "data:image/png;base64,AA==",
            "data:image/jpeg;base64,AA==",
            "data:image/gif;base64,AA==",
            "data:image/webp;base64,AA==",
        ] {
            for declared in [Some(true), None] {
                let mut t = target();
                t.declared_vision = declared;
                let image = ImageUrl {
                    url: url.into(),
                    detail: detail.map(str::to_owned),
                };
                let mut r = req();
                r.messages[0].content = Content::Parts(vec![
                    Part::Text {
                        text: "before".into(),
                    },
                    Part::ImageUrl {
                        image_url: image.clone(),
                    },
                    Part::Text {
                        text: "after".into(),
                    },
                ]);
                let prepared = build_native_request_with_options(
                    &r,
                    &t,
                    SourceFunction::SoloWorkRemote,
                    NativeRequestOptions {
                        allow_undeclared_images: true,
                    },
                )
                .unwrap();
                assert_eq!(
                    prepared.body["messages"][0]["content"],
                    json!([
                    {"type":"text","text":"before"},{"type":"image_url","image_url":image},{"type":"text","text":"after"}])
                );
                assert_eq!(
                    t.declared_vision, declared,
                    "validation must not create model capability evidence"
                );
            }
        }
    }
}
#[test]
fn undeclared_image_candidate_requires_explicit_opt_in_and_available_carrier() {
    let mut r = req();
    r.messages[0].content = Content::Parts(vec![
        Part::Text {
            text: "before".into(),
        },
        Part::ImageUrl {
            image_url: ImageUrl {
                url: "https://example.invalid/exact%20image".into(),
                detail: None,
            },
        },
        Part::Text {
            text: "after".into(),
        },
    ]);
    let mut t = target();
    t.declared_vision = None;
    let off = NativeRequestOptions {
        allow_undeclared_images: false,
    };
    let on = NativeRequestOptions {
        allow_undeclared_images: true,
    };
    assert_eq!(NativeRequestOptions::default(), off);
    assert_eq!(
        build_native_request(&r, &t, SourceFunction::SoloWorkRemote).unwrap_err(),
        "native_vision_capability_not_declared"
    );
    assert_eq!(
        build_native_request_with_options(&r, &t, SourceFunction::SoloWorkRemote, off).unwrap_err(),
        "native_vision_capability_not_declared"
    );
    let prepared =
        build_native_request_with_options(&r, &t, SourceFunction::SoloWorkRemote, on).unwrap();
    assert_eq!(
        prepared.body["messages"][0]["content"],
        json!([
        {"type":"text","text":"before"},
        {"type":"image_url","image_url":{"url":"https://example.invalid/exact%20image"}},
        {"type":"text","text":"after"}])
    );
    assert_eq!(
        t.declared_vision, None,
        "candidate opt-in must not create capability evidence"
    );
    assert!(prepared.body.get("allow_undeclared_images").is_none());
    t.declared_vision = Some(false);
    for options in [off, on] {
        assert_eq!(
            build_native_request_with_options(&r, &t, SourceFunction::SoloWorkRemote, options)
                .unwrap_err(),
            "native_vision_not_supported"
        );
    }
    t.declared_vision = None;
    t.protocol_carriers.images = false;
    assert_eq!(
        build_native_request_with_options(&r, &t, SourceFunction::SoloWorkRemote, on).unwrap_err(),
        "native_vision_capability_not_declared"
    );
    t.declared_vision = Some(true);
    assert_eq!(
        build_native_request_with_options(&r, &t, SourceFunction::SoloWorkRemote, on).unwrap_err(),
        "native_vision_not_supported"
    );
}
#[test]
fn audio_video_and_undeclared_reasoning_fail() {
    let mut r = req();
    r.messages[0].content = Content::Parts(vec![Part::InputAudio {
        input_audio: json!({}),
    }]);
    assert_eq!(build(&r).unwrap_err(), "native_audio_not_supported");
    r.messages[0].content = Content::Parts(vec![Part::VideoUrl {
        video_url: ImageUrl {
            url: "x".into(),
            detail: None,
        },
    }]);
    assert_eq!(build(&r).unwrap_err(), "native_video_not_supported");
    r = req();
    r.extra.insert("reasoning_effort".into(), json!("HIGH"));
    assert_eq!(
        build(&r).unwrap_err(),
        "native_reasoning_effort_not_declared"
    );
}
#[test]
fn local_consumers_are_validated_and_not_forwarded() {
    let mut r = req();
    r.stream = true;
    r.extra.insert("user".into(), json!("local-attribution"));
    r.extra
        .insert("stream_options".into(), json!({"include_usage":true}));
    let body = build(&r).unwrap().body;
    assert!(body.get("user").is_none());
    assert!(body.get("stream_options").is_none());
    r.stream = false;
    assert_eq!(build(&r).unwrap_err(), "native_stream_options_invalid");
    r.stream = true;
    r.extra
        .insert("stream_options".into(), json!({"unknown":true}));
    assert!(build(&r).is_err());
    r.extra.remove("stream_options");
    r.extra.insert("user".into(), json!(false));
    assert_eq!(build(&r).unwrap_err(), "native_user_invalid");
}
#[test]
fn invalid_or_duplicate_history_tools_fail_before_wire() {
    let mut r = req();
    let mut a = Message::assistant("");
    a.tool_calls = Some(vec![call("id", "{}")]);
    r.messages = vec![a.clone()];
    assert_eq!(build(&r).unwrap_err(), "native_tool_results_missing");
    let result = Message {
        role: Role::Tool,
        content: Content::Text("x".into()),
        tool_calls: None,
        tool_call_id: Some("id".into()),
        name: Some("weather".into()),
    };
    r.messages = vec![a.clone(), result.clone()];
    assert!(build(&r).is_ok());
    r.messages.push(result.clone());
    assert_eq!(
        build(&r).unwrap_err(),
        "native_tool_result_orphan_or_duplicate"
    );
    r.messages = vec![a.clone(), Message::user("hidden missing result")];
    assert_eq!(build(&r).unwrap_err(), "native_tool_results_missing");
    a.tool_calls = Some(vec![call("id", "[]")]);
    r.messages = vec![a, result];
    assert_eq!(build(&r).unwrap_err(), "native_tool_arguments_not_object");
}
#[test]
fn schemas_are_object_only_exact_once_and_bounded() {
    let mut r = req();
    r.tools = Some(tool());
    r.tools.as_mut().unwrap()[0]["function"]["parameters"] = json!("{}");
    assert_eq!(build(&r).unwrap_err(), "native_tool_schema_not_object");
    r.tools = Some(tool());
    r.tools.as_mut().unwrap()[0]["function"]["strict"] = json!(true);
    assert_eq!(build(&r).unwrap_err(), "native_request_unknown_field");
    r.tools = Some(json!([tool()[0].clone(), tool()[0].clone()]));
    assert_eq!(build(&r).unwrap_err(), "native_duplicate_tool_name");
    r.tools = Some(tool());
    r.tools.as_mut().unwrap()[0]["function"]["parameters"] =
        json!({"description":"x".repeat(SCHEMA_LIMIT)});
    assert_eq!(build(&r).unwrap_err(), "native_tool_schema_limit");
}
#[test]
fn request_and_history_call_bounds_are_enforced() {
    let mut r = req();
    r.messages[0] = Message::user("x".repeat(REQUEST_LIMIT));
    assert_eq!(build(&r).unwrap_err(), "native_request_limit");
    let mut a = Message::assistant("");
    a.tool_calls = Some((0..65).map(|i| call(&format!("id{i}"), "{}")).collect());
    r.messages = vec![a];
    assert_eq!(build(&r).unwrap_err(), "native_tool_history_limit");
}
