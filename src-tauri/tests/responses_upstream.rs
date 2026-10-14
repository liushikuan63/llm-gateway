//! Native Responses upstream contracts. All HTTP traffic stays on a private loopback fixture.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::OriginalUri;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use futures_util::StreamExt;
use llm_gateway_lib::config::OllamaOptionsConfig;
use llm_gateway_lib::domain::{
    ChatRequest, Content, Dialect, FunctionCall, Message, ModelOverrides, ModelRef, ModelType,
    Provider, Role, ToolCall,
};
use llm_gateway_lib::protocol::responses;
use llm_gateway_lib::proxy::upstream::{parse_sse_block, UpstreamClient, UpstreamEvent};
use serde_json::{json, Value};

fn request() -> ChatRequest {
    ChatRequest {
        model: "fixture-model".into(),
        messages: vec![Message::user("hello")],
        temperature: Some(0.9),
        top_p: None,
        max_tokens: Some(4096),
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        thinking: None,
        extra: Default::default(),
    }
}

fn tool_request() -> ChatRequest {
    let mut req = request();
    req.tools = Some(json!([{
        "type": "function", "function": {
            "name": "lookup", "description": "fixture lookup", "strict": true,
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"], "additionalProperties": false}
        }
    }]));
    req.tool_choice = Some(json!({"type": "function", "function": {"name": "lookup"}}));
    req.messages.push(Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: Some(vec![ToolCall {
            id: "call_lookup".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "lookup".into(),
                arguments: "{\"city\":\"Paris\"}".into(),
            },
        }]),
        tool_call_id: None,
        name: None,
    });
    req.messages.push(Message {
        role: Role::Tool,
        content: Content::Text("{\"temperature\":20}".into()),
        tool_calls: None,
        tool_call_id: Some("call_lookup".into()),
        name: Some("lookup".into()),
    });
    req
}

fn provider(dialect: Dialect, base_url: String) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: "responses-fixture".into(),
        name: "responses fixture".into(),
        dialect,
        base_url,
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        models: Vec::new(),
        rpm_limit: 0,
        intelligence: 0,
        note: None,
        runtime_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn overridden_model() -> ModelRef {
    ModelRef {
        enabled: true,
        alias: "fixture-model".into(),
        upstream: "fixture-model".into(),
        context_window: 8192,
        supports_tools: true,
        supports_vision: false,
        supports_audio: false,
        supports_video: false,
        supports_thinking: false,
        supports_stream: true,
        model_type: ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: Some(ModelOverrides {
            max_tokens: Some(64),
            temperature: Some(0.25),
            extra_body: Some(json!({"fixture_extra": true})),
            extra_headers: None,
        }),
        local: None,
        capabilities: None,
    }
}

fn function_output() -> Value {
    json!({"id": "resp_fixture", "model": "fixture-model", "status": "completed", "output": [
        {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]},
        {"type": "function_call", "id": "fc_internal", "call_id": "call_lookup", "name": "lookup", "arguments": "{\"city\":\"Paris\"}"}
    ], "usage": {"input_tokens": 7, "output_tokens": 3, "total_tokens": 10, "input_tokens_details": {"cached_tokens": 2}}})
}

#[test]
fn responses_native_tools_and_choice_are_flattened() {
    let body = responses::to_responses_body(&tool_request(), "fixture-model");
    assert_eq!(body["tools"][0]["name"], "lookup");
    assert_eq!(body["tools"][0]["strict"], true);
    assert_eq!(body["tools"][0]["parameters"]["required"], json!(["city"]));
    assert!(body["tools"][0].get("function").is_none());
    assert_eq!(
        body["tool_choice"],
        json!({"type": "function", "name": "lookup"})
    );
}

#[test]
fn responses_tool_history_preserves_call_id_arguments_and_result() {
    let body = responses::to_responses_body(&tool_request(), "fixture-model");
    assert_eq!(body["input"].as_array().unwrap().len(), 3);
    assert_eq!(
        body["input"][1],
        json!({"type": "function_call", "call_id": "call_lookup", "name": "lookup", "arguments": "{\"city\":\"Paris\"}"})
    );
    assert_eq!(
        body["input"][2],
        json!({"type": "function_call_output", "call_id": "call_lookup", "output": "{\"temperature\":20}"})
    );
}

#[test]
fn responses_nonstream_function_output_keeps_public_call_id_and_usage() {
    let response = responses::from_responses_response(&function_output(), "fixture-model");
    assert_eq!(response.content, "hello");
    let calls = response
        .tool_calls
        .expect("Responses function_call output must be retained");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].id, "call_lookup",
        "item id fc_internal must not replace call_id"
    );
    assert_eq!(calls[0].function.name, "lookup");
    assert_eq!(calls[0].function.arguments, "{\"city\":\"Paris\"}");
    assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
    let usage = response.usage.unwrap();
    assert_eq!(
        (
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cache_read_tokens
        ),
        (7, 3, 2)
    );
}

#[test]
fn responses_incomplete_is_a_length_terminal_with_usage() {
    let events = parse_sse_block(Dialect::Responses, br#"event: response.incomplete
data: {"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[],"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10}}}"#);
    assert_eq!(events.len(), 1);
    match events.into_iter().next().unwrap().unwrap() {
        UpstreamEvent::Done {
            finish_reason,
            usage,
        } => {
            assert_eq!(finish_reason.as_deref(), Some("length"));
            assert_eq!(usage.unwrap().total_tokens, 10);
        }
        other => panic!("expected incomplete terminal, got {other:?}"),
    }
}

#[test]
fn responses_failed_and_error_events_are_not_successful_completions() {
    for payload in [
        json!({"type":"response.failed", "response":{"status":"failed", "error":{"code":"server_error", "message":"fixture failure"}}}),
        json!({"type":"error", "code":"server_error", "message":"fixture failure"}),
    ] {
        let events = parse_sse_block(Dialect::Responses, format!("data: {payload}").as_bytes());
        assert_eq!(events.len(), 1, "errors must not disappear from the stream");
        assert!(events.into_iter().next().unwrap().is_err());
    }
}

fn responses_sse() -> String {
    let mut frames = vec![
        json!({"type":"response.created", "response":{"id":"resp_fixture", "status":"in_progress"}}),
        json!({"type":"response.output_text.delta", "output_index":0, "delta":"hello"}),
        json!({"type":"response.output_item.added", "output_index":1, "item":{"type":"function_call", "id":"fc_internal", "call_id":"call_lookup", "name":"lookup", "arguments":""}}),
        json!({"type":"response.function_call_arguments.delta", "output_index":1, "item_id":"fc_internal", "delta":"{\"city\":"}),
        json!({"type":"response.function_call_arguments.delta", "output_index":1, "item_id":"fc_internal", "delta":"\"Paris\"}"}),
        json!({"type":"response.function_call_arguments.done", "output_index":1, "item_id":"fc_internal", "arguments":"{\"city\":\"Paris\"}"}),
        json!({"type":"response.output_item.done", "output_index":1, "item":function_output()["output"][1]}),
        json!({"type":"response.completed", "response":function_output()}),
    ].drain(..).map(|frame| format!("event: {}\r\ndata: {frame}\r\n\r\n", frame["type"].as_str().unwrap())).collect::<String>();
    frames.push_str("data: [DONE]\r\n\r\n");
    frames
}

#[tokio::test]
async fn responses_real_upstream_consumes_native_sse_once_without_duplicate_arguments() {
    let captured = Arc::new(Mutex::new(None));
    let sink = captured.clone();
    let app = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            *sink.lock().unwrap() = Some(body);
            async {
                // Split inside event data; the production byte stream must reassemble it.
                let sse = responses_sse();
                let bytes = sse.into_bytes();
                let middle = bytes.len() / 2;
                let chunks: Vec<Result<Vec<u8>, std::convert::Infallible>> =
                    vec![Ok(bytes[..middle].to_vec()), Ok(bytes[middle..].to_vec())];
                (
                    [(CONTENT_TYPE, "text/event-stream")],
                    Body::from_stream(futures_util::stream::iter(chunks)),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut stream = UpstreamClient::new()
        .call_stream(
            &provider(Dialect::Responses, base),
            &request(),
            "fixture-model",
            Duration::from_secs(3),
            &OllamaOptionsConfig::default(),
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    task.abort();
    assert_eq!(captured.lock().unwrap().as_ref().unwrap()["stream"], true);
    assert_eq!(
        events.len(),
        5,
        "text, tool start, two argument deltas, one terminal only"
    );
    assert!(matches!(&events[0], UpstreamEvent::Delta(text) if text == "hello"));
    let expected = [
        json!([{"index":1,"id":"call_lookup","type":"function","function":{"name":"lookup","arguments":""}}]),
        json!([{"index":1,"function":{"arguments":"{\"city\":"}}]),
        json!([{"index":1,"function":{"arguments":"\"Paris\"}"}}]),
    ];
    for (event, expected) in events[1..4].iter().zip(expected) {
        match event {
            UpstreamEvent::ToolCalls(value) => assert_eq!(*value, expected),
            other => panic!("expected tool delta, got {other:?}"),
        }
    }
    match &events[4] {
        UpstreamEvent::Done {
            finish_reason,
            usage,
        } => {
            assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
            let usage = usage.as_ref().unwrap();
            assert_eq!(
                (
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.total_tokens,
                    usage.cache_read_tokens
                ),
                (7, 3, 10, 2)
            );
        }
        other => panic!("expected terminal with usage, got {other:?}"),
    }
}

async fn assert_overrides_on_wire(dialect: Dialect) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let app = Router::new().fallback(post(move |OriginalUri(uri): OriginalUri, Json(body): Json<Value>| {
        sink.lock().unwrap().push(body.clone());
        async move {
            let stream = if dialect == Dialect::Gemini {
                uri.path().ends_with(":streamGenerateContent") && uri.query() == Some("alt=sse")
            } else {
                body["stream"] == true
            };
            if stream {
                let payload = match dialect {
                    Dialect::Responses => responses_sse(),
                    Dialect::Ollama => "{\"message\":{\"role\":\"assistant\",\"content\":\"ok\"},\"done\":true}\n".into(),
                    Dialect::Anthropic => "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".into(),
                    Dialect::Gemini => "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}\n\n".into(),
                    Dialect::OpenAI => "data: [DONE]\n\n".into(),
                };
                return ([(CONTENT_TYPE, if dialect == Dialect::Ollama { "application/x-ndjson" } else { "text/event-stream" })], payload).into_response();
            }
            let value = match dialect {
                Dialect::Responses => function_output(),
                Dialect::Gemini => json!({"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}),
                Dialect::Anthropic => json!({"id":"a", "type":"message", "content":[{"type":"text","text":"ok"}], "stop_reason":"end_turn", "usage":{"input_tokens":1,"output_tokens":1}}),
                Dialect::Ollama => json!({"model":"fixture-model", "message":{"role":"assistant","content":"ok"},"done":true}),
                Dialect::OpenAI => json!({"id":"a", "choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}),
            };
            Json(value).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut p = provider(
        dialect,
        format!("http://{}", listener.local_addr().unwrap()),
    );
    p.models.push(overridden_model());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = UpstreamClient::new();
    let req = request();
    client
        .call(
            &p,
            &req,
            "fixture-model",
            Duration::from_secs(3),
            &OllamaOptionsConfig::default(),
        )
        .await
        .unwrap();
    let mut stream = client
        .call_stream(
            &p,
            &req,
            "fixture-model",
            Duration::from_secs(3),
            &OllamaOptionsConfig::default(),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    task.abort();
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 2);
    for body in captured.iter() {
        if dialect == Dialect::Gemini {
            assert!(
                body.get("stream").is_none(),
                "Gemini selects streaming in the URL, not a JSON stream field"
            );
        }
        let (max, temperature) = match dialect {
            Dialect::Responses => (&body["max_output_tokens"], &body["temperature"]),
            Dialect::Gemini => (
                &body["generationConfig"]["maxOutputTokens"],
                &body["generationConfig"]["temperature"],
            ),
            Dialect::Ollama => (
                &body["options"]["num_predict"],
                &body["options"]["temperature"],
            ),
            _ => (&body["max_tokens"], &body["temperature"]),
        };
        assert_eq!(
            *max,
            json!(64),
            "{dialect:?} must apply the real output limit on the wire"
        );
        assert_eq!(*temperature, json!(0.25));
        assert_eq!(
            body["fixture_extra"], true,
            "extra_body remains supported after normalization"
        );
        if matches!(
            dialect,
            Dialect::Responses | Dialect::Gemini | Dialect::Ollama
        ) {
            assert!(body.get("max_tokens").is_none());
        }
    }
    assert_eq!(
        req.max_tokens,
        Some(4096),
        "per-candidate overrides never mutate the shared request"
    );
    assert_eq!(req.temperature, Some(0.9));
}

#[tokio::test]
async fn responses_model_overrides_apply_to_native_fields() {
    assert_overrides_on_wire(Dialect::Responses).await;
}
#[tokio::test]
async fn gemini_model_overrides_apply_to_native_fields() {
    assert_overrides_on_wire(Dialect::Gemini).await;
}
#[tokio::test]
async fn ollama_model_overrides_apply_to_native_fields() {
    assert_overrides_on_wire(Dialect::Ollama).await;
}
#[tokio::test]
async fn openai_model_overrides_remain_effective() {
    assert_overrides_on_wire(Dialect::OpenAI).await;
}
#[tokio::test]
async fn anthropic_model_overrides_remain_effective() {
    assert_overrides_on_wire(Dialect::Anthropic).await;
}
