use llm_gateway_lib::domain::{
    ChatRequest, ChatResponse, Content, FunctionCall, ImageUrl, Message, Part, Role, ToolCall,
    Usage,
};
use llm_gateway_lib::protocol::{anthropic, convert, gemini, ollama, openai};
use serde_json::json;

fn request(messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: "auto".into(),
        messages,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        thinking: None,
        extra: Default::default(),
    }
}

fn tool_call(id: &str, name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        kind: "function".into(),
        function: FunctionCall {
            name: name.into(),
            arguments: arguments.into(),
        },
    }
}

#[test]
fn openai_request_maps_roles_and_preserves_unknown_fields() {
    let input: openai::OaChatRequest = serde_json::from_value(json!({
        "model": "gpt-4o",
        "messages": [
            { "role": "system", "content": "sys" },
            { "role": "user", "content": "hello" },
            { "role": "assistant", "content": "hi" }
        ],
        "temperature": 0.5,
        "max_tokens": 100,
        "user": "tenant-alice",
        "future_option": { "enabled": true }
    }))
    .unwrap();

    let ir = input.to_internal().unwrap();
    assert_eq!(ir.model, "gpt-4o");
    assert_eq!(ir.messages[0].role, Role::System);
    assert_eq!(ir.messages[1].role, Role::User);
    assert_eq!(ir.messages[2].role, Role::Assistant);
    assert_eq!(ir.temperature, Some(0.5));
    assert_eq!(ir.max_tokens, Some(100));
    assert_eq!(ir.extra["user"], json!("tenant-alice"));
    assert_eq!(ir.extra["future_option"], json!({ "enabled": true }));
}

#[test]
fn openai_image_detail_normalizes_codex_original_and_drops_unknown_values() {
    let req = request(vec![Message {
        role: Role::User,
        content: Content::Parts(vec![
            Part::ImageUrl {
                image_url: ImageUrl {
                    url: "https://example.test/codex-original.png".into(),
                    detail: Some("original".into()),
                },
            },
            Part::ImageUrl {
                image_url: ImageUrl {
                    url: "https://example.test/unknown-detail.png".into(),
                    detail: Some("maximum".into()),
                },
            },
        ]),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }]);

    let upstream = openai::to_upstream_body(&req, "gpt-4o");
    let parts = upstream["messages"][0]["content"].as_array().unwrap();

    assert_eq!(parts[0]["image_url"]["detail"], json!("high"));
    assert!(parts[1]["image_url"].get("detail").is_none());
}

#[test]
fn openai_chat_video_parts_become_video_url_in_ir() {
    let input: openai::OaChatRequest = serde_json::from_value(json!({
        "model": "gpt-4o",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "描述视频" },
                { "type": "video_url", "video_url": { "url": "https://example.test/clip.mp4" } }
            ]
        }]
    }))
    .unwrap();
    let ir = input.to_internal().unwrap();
    let Content::Parts(parts) = &ir.messages[0].content else {
        panic!("expected multimodal content");
    };
    assert!(parts.iter().any(|part| matches!(
        part,
        Part::VideoUrl { video_url } if video_url.url == "https://example.test/clip.mp4"
    )));
}

#[test]
fn cache_usage_details_are_preserved_across_protocols() {
    let openai = convert::openai_response_to_internal(&json!({
        "choices": [{ "message": { "role": "assistant", "content": "ok" } }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 60, "cache_write_tokens": 20 }
        }
    }))
    .usage
    .unwrap();
    assert_eq!(openai.cache_read_tokens, 60);
    assert_eq!(openai.cache_creation_tokens, 20);
    assert_eq!(openai.normal_input_tokens(), 20);

    let anthropic = anthropic::anthropic_to_internal(&json!({
        "id": "msg_cache",
        "model": "claude",
        "content": [{ "type": "text", "text": "ok" }],
        "usage": {
            "input_tokens": 100,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 50,
            "output_tokens": 10
        }
    }))
    .usage
    .unwrap();
    assert_eq!(anthropic.prompt_tokens, 350);
    assert_eq!(anthropic.cache_read_tokens, 200);
    assert_eq!(anthropic.cache_creation_tokens, 50);
    assert_eq!(anthropic.normal_input_tokens(), 100);

    let gemini = gemini::from_gemini_response(&json!({
        "candidates": [{ "content": { "parts": [{ "text": "ok" }] } }],
        "usageMetadata": {
            "promptTokenCount": 100,
            "candidatesTokenCount": 10,
            "totalTokenCount": 110,
            "cachedContentTokenCount": 40
        }
    }))
    .usage
    .unwrap();
    assert_eq!(gemini.cache_read_tokens, 40);
    assert_eq!(gemini.normal_input_tokens(), 60);
}

#[test]
fn anthropic_system_is_plain_top_level_text_and_max_tokens_is_required() {
    let body = anthropic::internal_to_anthropic_body(
        &request(vec![Message::system("你是助手"), Message::user("你好")]),
        "claude-sonnet",
    );

    assert_eq!(body["system"], json!("你是助手"));
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], json!("user"));
    assert_eq!(body["max_tokens"], json!(4096));

    let mut explicit = request(vec![Message::user("hello")]);
    explicit.max_tokens = Some(8192);
    assert_eq!(
        anthropic::internal_to_anthropic_body(&explicit, "claude")["max_tokens"],
        json!(8192)
    );
}

#[test]
fn anthropic_tool_result_becomes_an_ir_tool_message() {
    let input: anthropic::AnthropicRequest = serde_json::from_value(json!({
        "model": "claude",
        "messages": [
            {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "ls",
                    "input": { "path": "/" }
                }]
            },
            {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "content": [{ "type": "text", "text": "a.txt" }]
                }]
            }
        ]
    }))
    .unwrap();

    let ir = input.to_internal().unwrap();
    assert_eq!(
        ir.messages[0].tool_calls.as_ref().unwrap()[0].function.name,
        "ls"
    );
    assert_eq!(ir.messages[1].role, Role::Tool);
    assert_eq!(ir.messages[1].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(ir.messages[1].content, Content::Text("a.txt".into()));
}

#[test]
fn tool_result_is_encoded_as_anthropic_user_block() {
    let message = Message {
        role: Role::Tool,
        content: Content::Text("执行结果".into()),
        tool_calls: None,
        tool_call_id: Some("call_1".into()),
        name: None,
    };

    let body = convert::message_to_anthropic(&message);
    assert_eq!(body["role"], json!("user"));
    assert_eq!(body["content"][0]["type"], json!("tool_result"));
    assert_eq!(body["content"][0]["tool_use_id"], json!("call_1"));
    assert_eq!(body["content"][0]["content"], json!("执行结果"));
}

#[test]
fn openai_tool_calls_round_trip_with_type_not_internal_kind() {
    let input: openai::OaChatRequest = serde_json::from_value(json!({
        "model": "gpt-4o",
        "messages": [{
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": { "name": "get_weather", "arguments": "{\"city\":\"青岛\"}" }
            }]
        }]
    }))
    .unwrap();
    let ir = input.to_internal().unwrap();
    let calls = ir.messages[0].tool_calls.as_ref().unwrap();
    assert_eq!(calls[0].kind, "function");
    assert_eq!(calls[0].function.name, "get_weather");

    let upstream = openai::to_upstream_body(&ir, "gpt-4o");
    assert_eq!(
        upstream["messages"][0]["tool_calls"][0]["type"],
        json!("function")
    );
    assert!(upstream["messages"][0]["tool_calls"][0]
        .get("kind")
        .is_none());

    let response = openai::chat_completion_response(
        "id_1",
        "gpt-4o",
        &ChatResponse {
            id: "id_1".into(),
            model: "gpt-4o".into(),
            content: String::new(),
            tool_calls: Some(vec![tool_call("call_1", "get_weather", "{}")]),
            finish_reason: Some("tool_calls".into()),
            usage: Some(Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
                ..Default::default()
            }),
        },
    );
    assert_eq!(
        response["choices"][0]["message"]["tool_calls"][0]["type"],
        json!("function")
    );
    assert!(response["choices"][0]["message"]["tool_calls"][0]
        .get("kind")
        .is_none());
}

#[test]
fn openai_o_series_and_stream_output_follow_openai_contract() {
    let mut req = request(vec![Message::user("hi")]);
    req.max_tokens = Some(500);
    req.stream = true;

    let o_series = openai::to_upstream_body(&req, "o1-preview");
    assert_eq!(o_series["max_completion_tokens"], json!(500));
    assert!(o_series.get("max_tokens").is_none());
    assert_eq!(o_series["stream_options"], json!({ "include_usage": true }));

    let payload = openai::sse_chunk("chatcmpl_1", "gpt-4o", &json!({ "content": "hi" }), None);
    assert!(!payload.starts_with("data:"));
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(value["object"], json!("chat.completion.chunk"));
    assert_eq!(value["choices"][0]["delta"]["content"], json!("hi"));
}

#[test]
fn gemini_converts_openai_tools_and_function_calls() {
    let mut req = request(vec![Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: Some(vec![tool_call(
            "call_1",
            "get_weather",
            "{\"city\":\"青岛\"}",
        )]),
        tool_call_id: None,
        name: None,
    }]);
    req.tools = Some(json!([{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "查询天气",
            "parameters": { "type": "object", "properties": { "city": { "type": "string" } } }
        }
    }]));
    req.tool_choice = Some(json!({
        "type": "function",
        "function": { "name": "get_weather" }
    }));

    let body = gemini::to_gemini_body(&req);
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"],
        json!("get_weather")
    );
    assert_eq!(
        body["toolConfig"]["functionCallingConfig"]["mode"],
        json!("ANY")
    );
    assert_eq!(
        body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
        json!(["get_weather"])
    );
    assert_eq!(
        body["contents"][0]["parts"][0]["functionCall"]["name"],
        json!("get_weather")
    );

    let response = gemini::from_gemini_response(&json!({
        "modelVersion": "gemini-2.5-flash",
        "candidates": [{
            "content": { "parts": [{
                "functionCall": { "name": "get_weather", "args": { "city": "青岛" } }
            }]},
            "finishReason": "STOP"
        }],
        "usageMetadata": { "promptTokenCount": 2, "candidatesTokenCount": 3, "totalTokenCount": 5 }
    }));
    assert_eq!(
        response.tool_calls.as_ref().unwrap()[0].function.name,
        "get_weather"
    );
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    assert_eq!(response.usage.unwrap().total_tokens, 5);
}

#[test]
fn ollama_uses_only_explicit_options_and_reports_total_usage() {
    let req = request(vec![Message::user("hi")]);
    let body = ollama::to_ollama_body(&req, "qwen");
    assert!(body.get("options").is_none());

    let response = ollama::from_ollama_response(&json!({
        "model": "qwen",
        "message": { "content": "hello" },
        "done": true,
        "prompt_eval_count": 7,
        "eval_count": 11
    }));
    assert_eq!(response.content, "hello");
    assert_eq!(response.usage.unwrap().total_tokens, 18);
}

#[test]
fn anthropic_response_preserves_tool_calls_and_stop_reason() {
    let response = anthropic::anthropic_to_internal(&json!({
        "id": "msg_1",
        "model": "claude",
        "content": [{ "type": "tool_use", "id": "c1", "name": "ls", "input": { "path": "/" } }],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 10, "output_tokens": 5 }
    }));
    assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(response.tool_calls.unwrap()[0].function.name, "ls");
    assert_eq!(response.usage.unwrap().total_tokens, 15);
}

#[test]
fn anthropic_user_message_keeps_all_tool_results_text_and_images() {
    let input: anthropic::AnthropicRequest = serde_json::from_value(json!({
        "model": "claude",
        "messages": [
            {
                "role": "assistant",
                "content": [
                    { "type": "tool_use", "id": "call_a", "name": "lookup", "input": { "q": "a" } },
                    { "type": "tool_use", "id": "call_b", "name": "lookup", "input": { "q": "b" } }
                ]
            },
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "工具已经执行：" },
                    { "type": "tool_result", "tool_use_id": "call_a", "content": [{ "type": "text", "text": "结果 A" }] },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "YWJj" } },
                    { "type": "tool_result", "tool_use_id": "call_b", "content": [{ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "ZGVm" } }] },
                    { "type": "text", "text": "请继续分析。" }
                ]
            }
        ]
    }))
    .unwrap();

    let ir = input.to_internal().unwrap();
    assert_eq!(ir.messages[0].role, Role::Assistant);
    assert_eq!(ir.messages[0].tool_calls.as_ref().unwrap().len(), 2);
    assert_eq!(ir.messages[1].role, Role::User);
    assert_eq!(ir.messages[1].content_text(), "工具已经执行：");
    assert_eq!(ir.messages[2].role, Role::Tool);
    assert_eq!(ir.messages[2].tool_call_id.as_deref(), Some("call_a"));
    assert_eq!(ir.messages[2].content_text(), "结果 A");
    assert_eq!(ir.messages[3].role, Role::User);
    assert!(matches!(&ir.messages[3].content, Content::Parts(_)));
    assert_eq!(ir.messages[4].role, Role::Tool);
    assert_eq!(ir.messages[4].tool_call_id.as_deref(), Some("call_b"));
    assert!(matches!(&ir.messages[4].content, Content::Parts(_)));
    assert_eq!(ir.messages[5].role, Role::User);
    assert_eq!(ir.messages[5].content_text(), "请继续分析。");

    let body = anthropic::internal_to_anthropic_body(&ir, "claude");
    let messages = body["messages"].as_array().unwrap();
    // 首条 assistant 会按既有兼容策略补一个 `(continue)` user 占位；重点是
    // 后续所有 user/tool 块必须只形成一个真正的 user message。
    assert_eq!(messages.len(), 3, "{body}");
    assert_eq!(messages[1]["role"], json!("assistant"));
    assert_eq!(messages[2]["role"], json!("user"));

    let blocks = messages[2]["content"].as_array().unwrap();
    let result_ids: Vec<&str> = blocks
        .iter()
        .filter(|block| block["type"] == json!("tool_result"))
        .filter_map(|block| block["tool_use_id"].as_str())
        .collect();
    assert_eq!(result_ids, vec!["call_a", "call_b"]);
    assert!(blocks.iter().any(|block| block["type"] == json!("image")));
    assert!(blocks
        .iter()
        .any(|block| block["text"] == json!("工具已经执行：")));
    assert!(blocks
        .iter()
        .any(|block| block["text"] == json!("请继续分析。")));
    let second_result = blocks
        .iter()
        .find(|block| block["tool_use_id"] == json!("call_b"))
        .unwrap();
    assert!(second_result["content"]
        .as_array()
        .unwrap()
        .iter()
        .any(|block| block["type"] == json!("image")));
}

#[test]
fn anthropic_outbound_merges_consecutive_tool_results_into_one_user_message() {
    let assistant = Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: Some(vec![
            tool_call("call_a", "lookup", "{\"q\":\"a\"}"),
            tool_call("call_b", "lookup", "{\"q\":\"b\"}"),
        ]),
        tool_call_id: None,
        name: None,
    };
    let first_result = Message {
        role: Role::Tool,
        content: Content::Text("结果 A".into()),
        tool_calls: None,
        tool_call_id: Some("call_a".into()),
        name: Some("lookup".into()),
    };
    let second_result = Message {
        role: Role::Tool,
        content: Content::Text("结果 B".into()),
        tool_calls: None,
        tool_call_id: Some("call_b".into()),
        name: Some("lookup".into()),
    };

    let body = anthropic::internal_to_anthropic_body(
        &request(vec![assistant, first_result, second_result]),
        "claude",
    );
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{body}");
    assert_eq!(messages[1]["role"], json!("assistant"));
    assert_eq!(messages[2]["role"], json!("user"));
    assert_eq!(messages[2]["content"].as_array().unwrap().len(), 2);
    assert_eq!(messages[2]["content"][0]["type"], json!("tool_result"));
    assert_eq!(messages[2]["content"][0]["tool_use_id"], json!("call_a"));
    assert_eq!(messages[2]["content"][1]["tool_use_id"], json!("call_b"));
}
