use llm_gateway_lib::domain::{
    ChatRequest, ChatResponse, Content, FunctionCall, Message, Role, ToolCall, Usage,
};
use llm_gateway_lib::protocol::ollama;
use serde_json::json;

fn request() -> ChatRequest {
    ChatRequest {
        model: "gateway-model".into(),
        messages: vec![Message::user("use a tool")],
        temperature: Some(0.2),
        top_p: Some(0.85),
        max_tokens: Some(128),
        stop: Some(vec!["END".into()]),
        stream: false,
        tools: Some(json!([{
            "type": "function",
            "function": {
                "name": "weather",
                "parameters": { "type": "object" }
            }
        }])),
        tool_choice: None,
        thinking: None,
        extra: Default::default(),
    }
}

#[test]
fn ollama_ingress_maps_tools_and_explicit_options_to_internal_request() {
    let request = ollama::ollama_request_to_internal(&json!({
        "model": "llama3.3",
        "stream": false,
        "messages": [{
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": { "name": "weather", "arguments": { "city": "Qingdao" } }
            }]
        }],
        "tools": [{
            "type": "function",
            "function": { "name": "weather", "parameters": { "type": "object" } }
        }],
        "options": {
            "temperature": 0.3,
            "top_p": 0.9,
            "num_predict": 256,
            "stop": ["DONE"]
        }
    }))
    .expect("Ollama request should parse");

    assert_eq!(request.model, "llama3.3");
    assert_eq!(request.temperature, Some(0.3));
    assert_eq!(request.top_p, Some(0.9));
    assert_eq!(request.max_tokens, Some(256));
    assert_eq!(request.stop, Some(vec!["DONE".into()]));
    assert_eq!(
        request.tools.as_ref().unwrap()[0]["function"]["name"],
        "weather"
    );
    let call = request.messages[0]
        .tool_calls
        .as_ref()
        .unwrap()
        .first()
        .unwrap();
    assert_eq!(call.id, "ollama-0-0");
    assert_eq!(call.function.name, "weather");
    assert_eq!(call.function.arguments, r#"{"city":"Qingdao"}"#);
}

#[test]
fn ollama_upstream_body_keeps_tools_options_and_tool_call_history() {
    let mut request = request();
    request.messages.push(Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: Some(vec![ToolCall {
            id: "call_weather".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "weather".into(),
                arguments: r#"{"city":"Qingdao"}"#.into(),
            },
        }]),
        tool_call_id: None,
        name: None,
    });

    // None = 不注入网关默认值，验证纯转换逻辑。
    let body = ollama::to_ollama_body(&request, "llama3.3", None);
    assert_eq!(body["tools"][0]["function"]["name"], "weather");
    assert!((body["options"]["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    assert!((body["options"]["top_p"].as_f64().unwrap() - 0.85).abs() < 1e-6);
    assert_eq!(body["options"]["num_predict"], 128);
    assert_eq!(body["options"]["stop"], json!(["END"]));
    assert_eq!(
        body["messages"][1]["tool_calls"][0]["function"]["arguments"],
        json!({ "city": "Qingdao" })
    );
}

#[test]
fn ollama_non_stream_response_preserves_tool_calls_and_usage() {
    let internal = ollama::from_ollama_response(&json!({
        "model": "llama3.3",
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "function": { "name": "weather", "arguments": { "city": "Qingdao" } }
            }]
        },
        "done": true,
        "prompt_eval_count": 11,
        "eval_count": 7
    }));
    assert_eq!(internal.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(
        internal.tool_calls.as_ref().unwrap()[0].function.arguments,
        r#"{"city":"Qingdao"}"#
    );

    let response = ollama::ollama_response(
        &ChatResponse {
            id: "response-1".into(),
            model: "llama3.3".into(),
            content: internal.content,
            // 普通 API 上游，没有「传输」概念。
            transport: None,
            tool_calls: internal.tool_calls,
            finish_reason: internal.finish_reason,
            usage: Some(Usage {
                prompt_tokens: 11,
                completion_tokens: 7,
                total_tokens: 18,
                ..Default::default()
            }),
        },
        "llama3.3",
    );
    assert_eq!(response["done"], true);
    assert_eq!(response["done_reason"], "tool_calls");
    assert_eq!(response["prompt_eval_count"], 11);
    assert_eq!(response["eval_count"], 7);
    assert_eq!(
        response["message"]["tool_calls"][0]["function"]["name"],
        "weather"
    );
    assert_eq!(
        response["message"]["tool_calls"][0]["function"]["arguments"],
        json!({ "city": "Qingdao" })
    );
}
