use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures_util::StreamExt;
use llm_gateway_lib::domain::{ChatRequest, Dialect, Message, Provider};
use llm_gateway_lib::error::GatewayError;
use llm_gateway_lib::proxy::upstream::{parse_sse_block, UpstreamClient, UpstreamEvent};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

fn local_provider(dialect: Dialect, base_url: String) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: "local-mock".into(),
        name: "local mock".into(),
        dialect,
        base_url,
        api_key_enc: String::new(),
        enabled: true,
        priority: 0,
        models: Vec::new(),
        rpm_limit: 0,
        intelligence: 0,
        note: None,
        created_at: now,
        updated_at: now,
    }
}

fn chat_request() -> ChatRequest {
    ChatRequest {
        model: "local-model".into(),
        messages: vec![Message::user("hello")],
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

async fn spawn_axum(app: Router) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), server)
}

async fn read_request_headers(socket: &mut tokio::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = socket.read(&mut chunk).await.unwrap();
        assert!(
            read > 0,
            "mock upstream received an incomplete HTTP request"
        );
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8_lossy(&request).into_owned();
        }
    }
}

fn chunked_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = format!("{:X}\r\n", payload.len()).into_bytes();
    frame.extend_from_slice(payload);
    frame.extend_from_slice(b"\r\n");
    frame
}

async fn spawn_split_openai_sse() -> (String, oneshot::Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.set_nodelay(true).unwrap();
        let request_headers = read_request_headers(&mut socket).await;
        let _ = request_tx.send(request_headers);

        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();

        // The JSON payload ends halfway through a field in the first TCP write.
        let first_write = chunked_frame(
            b"data: {\"id\":\"chatcmpl-local\",\"choices\":[{\"delta\":{\"content\":\"hel",
        );
        socket.write_all(&first_write).await.unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;

        let second_write = chunked_frame(
            b"lo\"},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"id\":\"chatcmpl-local\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n\
data: [DONE]\n\n",
        );
        socket.write_all(&second_write).await.unwrap();
        socket.write_all(b"0\r\n\r\n").await.unwrap();
        socket.flush().await.unwrap();
    });

    (format!("http://{addr}"), request_rx, server)
}

#[tokio::test]
async fn openai_sse_reassembles_an_event_split_across_tcp_writes() {
    let (base_url, request_rx, server) = spawn_split_openai_sse().await;
    let provider = local_provider(Dialect::OpenAI, base_url);
    let client = UpstreamClient::new();

    let mut stream = client
        .call_stream(
            &provider,
            &chat_request(),
            "local-model",
            Duration::from_secs(2),
            &llm_gateway_lib::config::OllamaOptionsConfig::default(),
        )
        .await
        .expect("empty api_key local OpenAI provider should be callable");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("mock SSE event should parse"));
    }

    let request_headers = request_rx.await.unwrap().to_ascii_lowercase();
    assert!(
        !request_headers.contains("\r\nauthorization:"),
        "an empty api_key must not add an Authorization header"
    );
    server.await.unwrap();

    assert_eq!(events.len(), 4);
    match &events[0] {
        UpstreamEvent::Delta(content) => assert_eq!(content, "hello"),
        event => panic!("expected a reassembled text delta, got {event:?}"),
    }
    match &events[1] {
        UpstreamEvent::Finish { finish_reason } => {
            assert_eq!(finish_reason.as_deref(), Some("stop"));
        }
        event => panic!("expected a non-terminal finish marker, got {event:?}"),
    }
    match &events[2] {
        UpstreamEvent::Usage(usage) => {
            assert_eq!(usage.prompt_tokens, 3);
            assert_eq!(usage.completion_tokens, 2);
            assert_eq!(usage.total_tokens, 5);
        }
        event => panic!("expected an OpenAI usage-only event, got {event:?}"),
    }
    match &events[3] {
        UpstreamEvent::Done {
            finish_reason,
            usage,
        } => {
            assert_eq!(finish_reason.as_deref(), Some("stop"));
            assert!(usage.is_none());
        }
        event => panic!("expected OpenAI [DONE] completion, got {event:?}"),
    }
}

async fn ollama_ndjson(headers: HeaderMap) -> Response {
    if headers.contains_key(AUTHORIZATION) {
        return (StatusCode::BAD_REQUEST, "unexpected Authorization header").into_response();
    }

    let body = Body::from_stream(async_stream::stream! {
        yield Ok::<Bytes, Infallible>(Bytes::from_static(
            b"{\"model\":\"local-model\",\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"done\":false}\n",
        ));
        tokio::task::yield_now().await;
        yield Ok::<Bytes, Infallible>(Bytes::from_static(
            b"{\"model\":\"local-model\",\"message\":{\"role\":\"assistant\",\"content\":\"lo\"},\"done\":false}\n",
        ));
        yield Ok::<Bytes, Infallible>(Bytes::from_static(
            b"{\"model\":\"local-model\",\"message\":{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[{\"id\":\"ollama-call-1\",\"function\":{\"name\":\"get_weather\",\"arguments\":{\"city\":\"Qingdao\"}}}]},\"done\":true}\n",
        ));
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/x-ndjson")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn ollama_ndjson_emits_incremental_tool_calls_and_completion_events() {
    let app = Router::new().route("/api/chat", post(ollama_ndjson));
    let (base_url, server) = spawn_axum(app).await;
    let provider = local_provider(Dialect::Ollama, base_url);
    let client = UpstreamClient::new();

    let mut stream = client
        .call_stream(
            &provider,
            &chat_request(),
            "local-model",
            Duration::from_secs(2),
            &llm_gateway_lib::config::OllamaOptionsConfig::default(),
        )
        .await
        .expect("empty api_key local Ollama provider should be callable");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("mock NDJSON event should parse"));
    }
    server.abort();

    assert_eq!(events.len(), 4);
    match &events[0] {
        UpstreamEvent::Delta(content) => assert_eq!(content, "Hel"),
        event => panic!("expected first Ollama increment, got {event:?}"),
    }
    match &events[1] {
        UpstreamEvent::Delta(content) => assert_eq!(content, "lo"),
        event => panic!("expected second Ollama increment, got {event:?}"),
    }
    match &events[2] {
        UpstreamEvent::ToolCalls(calls) => assert_eq!(
            calls,
            &json!([{
                "index": 0,
                "id": "ollama-call-1",
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Qingdao\"}",
                },
            }]),
        ),
        event => panic!("expected Ollama tool-call event, got {event:?}"),
    }
    match &events[3] {
        UpstreamEvent::Done {
            finish_reason,
            usage,
        } => {
            assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
            assert!(usage.is_none());
        }
        event => panic!("expected Ollama completion, got {event:?}"),
    }
}

#[test]
fn anthropic_tool_use_start_and_json_deltas_are_openai_appendable() {
    let mut start = parse_sse_block(
        Dialect::Anthropic,
        br#"event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_123","name":"get_weather","input":{}}}"#,
    );
    assert_eq!(start.len(), 1);
    let start_call = match start.remove(0).unwrap() {
        UpstreamEvent::ToolCalls(calls) => calls,
        event => panic!("expected Anthropic tool-call start, got {event:?}"),
    };
    assert_eq!(
        start_call,
        json!([{
            "index": 1,
            "id": "toolu_123",
            "type": "function",
            "function": { "name": "get_weather", "arguments": "" },
        }]),
    );

    let first_delta = parse_sse_block(
        Dialect::Anthropic,
        br#"event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Qing"}}"#,
    );
    let second_delta = parse_sse_block(
        Dialect::Anthropic,
        br#"event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"dao\"}"}}"#,
    );

    let first_call = match first_delta.into_iter().next().unwrap().unwrap() {
        UpstreamEvent::ToolCalls(calls) => calls,
        event => panic!("expected first Anthropic argument delta, got {event:?}"),
    };
    let second_call = match second_delta.into_iter().next().unwrap().unwrap() {
        UpstreamEvent::ToolCalls(calls) => calls,
        event => panic!("expected second Anthropic argument delta, got {event:?}"),
    };
    assert_eq!(first_call[0]["index"], json!(1));
    assert_eq!(second_call[0]["index"], json!(1));
    assert!(first_call[0].get("id").is_none());
    assert!(second_call[0]
        .get("function")
        .and_then(|function| function.get("name"))
        .is_none());

    let mut arguments = start_call[0]["function"]["arguments"]
        .as_str()
        .unwrap()
        .to_string();
    arguments.push_str(first_call[0]["function"]["arguments"].as_str().unwrap());
    arguments.push_str(second_call[0]["function"]["arguments"].as_str().unwrap());
    assert_eq!(arguments, r#"{"city":"Qingdao"}"#);

    let terminal = parse_sse_block(
        Dialect::Anthropic,
        br#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}"#,
    );
    match terminal.into_iter().next().unwrap().unwrap() {
        UpstreamEvent::Done { finish_reason, .. } => {
            assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
        }
        event => panic!("expected Anthropic tool-call completion, got {event:?}"),
    }
}

#[test]
fn gemini_function_call_becomes_openai_tool_call_delta() {
    let mut events = parse_sse_block(
        Dialect::Gemini,
        br#"data: {"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_weather","args":{"city":"Qingdao"}}}]},"finishReason":"STOP"}]}"#,
    );
    assert_eq!(events.len(), 2);
    match events.remove(0).unwrap() {
        UpstreamEvent::ToolCalls(calls) => assert_eq!(
            calls,
            json!([{
                "index": 0,
                "id": "call_0",
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Qingdao\"}",
                },
            }]),
        ),
        event => panic!("expected Gemini function-call event, got {event:?}"),
    }
    match events.remove(0).unwrap() {
        UpstreamEvent::Done { finish_reason, .. } => {
            assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
        }
        event => panic!("expected Gemini tool-call completion, got {event:?}"),
    }
}

async fn html_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from("<html><body>proxy error</body></html>"))
        .unwrap()
}

async fn truncated_json_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from("{\"id\": \"truncated\""))
        .unwrap()
}

async fn wrapped_application_error_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            "{\"code\":500,\"msg\":\"temporary upstream failure\",\"success\":false}",
        ))
        .unwrap()
}

async fn temporary_redirect_response() -> Response {
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header("location", "/redirect-target")
        .body(Body::empty())
        .unwrap()
}

async fn redirect_target(State(hits): State<Arc<AtomicUsize>>) -> Response {
    hits.fetch_add(1, Ordering::SeqCst);
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            "{\"id\":\"redirected\",\"model\":\"local-model\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"unexpected\"},\"finish_reason\":\"stop\"}]}",
        ))
        .unwrap()
}

#[tokio::test]
async fn successful_http_with_invalid_or_error_payload_is_retryable_bad_gateway() {
    let app = Router::new()
        .route("/html/chat/completions", post(html_response))
        .route("/truncated/chat/completions", post(truncated_json_response))
        .route(
            "/wrapped-error/chat/completions",
            post(wrapped_application_error_response),
        );
    let (base_url, server) = spawn_axum(app).await;
    let client = UpstreamClient::new();

    for (path, expected_body) in [
        ("html", "<html><body>proxy error</body></html>"),
        ("truncated", "{\"id\": \"truncated\""),
        (
            "wrapped-error",
            "{\"code\":500,\"msg\":\"temporary upstream failure\",\"success\":false}",
        ),
    ] {
        let provider = local_provider(Dialect::OpenAI, format!("{base_url}/{path}"));
        let error = client
            .call(
                &provider,
                &chat_request(),
                "local-model",
                Duration::from_secs(2),
                &llm_gateway_lib::config::OllamaOptionsConfig::default(),
            )
            .await
            .expect_err("HTTP 200 with malformed JSON must not be accepted");

        assert!(error.retryable());
        match error {
            GatewayError::Upstream { status, body, .. } => {
                assert_eq!(status, 502);
                assert_eq!(body, expected_body);
            }
            other => panic!("expected a retryable 502 upstream error, got {other:?}"),
        }
    }

    server.abort();
}

#[tokio::test]
async fn upstream_redirects_are_not_followed() {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/redirect/chat/completions",
            post(temporary_redirect_response),
        )
        .route("/redirect-target", post(redirect_target))
        .with_state(hits.clone());
    let (base_url, server) = spawn_axum(app).await;
    let provider = local_provider(Dialect::OpenAI, format!("{base_url}/redirect"));

    let error = UpstreamClient::new()
        .call(
            &provider,
            &chat_request(),
            "local-model",
            Duration::from_secs(2),
            &llm_gateway_lib::config::OllamaOptionsConfig::default(),
        )
        .await
        .expect_err("307 must be surfaced instead of being followed");

    match error {
        GatewayError::Upstream { status, .. } => assert_eq!(status, 307),
        other => panic!("expected a 307 upstream error, got {other:?}"),
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "redirect target was contacted"
    );
    server.abort();
}

#[tokio::test]
async fn model_overrides_apply_after_gateway_normalization_and_skip_protected_fields() {
    use axum::http::HeaderMap as AxumHeaderMap;
    use llm_gateway_lib::domain::{HeaderPair, ModelOverrides};

    async fn echo_request(
        headers: AxumHeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> impl IntoResponse {
        let header = headers
            .get("x-tenant")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        Json(json!({
            "id": "chatcmpl-override",
            "object": "chat.completion",
            "model": "local-model",
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": header }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 },
            "echo_body": body,
        }))
    }

    let app = Router::new()
        .route("/v1/chat/completions", post(echo_request))
        .with_state(());
    let (base_url, server) = spawn_axum(app).await;

    let mut provider = local_provider(Dialect::OpenAI, format!("{base_url}/v1"));
    provider.models = vec![llm_gateway_lib::domain::ModelRef {
        alias: "local-model".into(),
        upstream: "local-model".into(),
        context_window: 8192,
        supports_tools: true,
        supports_vision: false,
        supports_audio: false,
        supports_video: false,
        supports_thinking: false,
        supports_stream: true,
        model_type: llm_gateway_lib::domain::ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: Some(ModelOverrides {
            temperature: Some(0.25),
            max_tokens: Some(77),
            extra_body: Some(json!({ "top_k": 12, "seed": 7 })),
            extra_headers: Some(vec![HeaderPair {
                name: "X-Tenant".into(),
                value: "tenant-42".into(),
            }]),
        }),
        local: None,
    }];

    let mut req = chat_request();
    req.temperature = Some(0.9);

    let client = UpstreamClient::new();
    let response = client
        .call(&provider, &req, "local-model", Duration::from_secs(5), &llm_gateway_lib::config::OllamaOptionsConfig::default())
        .await
        .expect("带覆盖配置的请求应成功");

    // extra_headers 生效（大小写不敏感），应答内容确认服务端确实收到了该头。
    assert_eq!(response.content, "tenant-42");

    // 再捕获一次完整请求体，确认参数覆盖发生在网关整流之后。
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = captured.clone();
    let app2 = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |headers: AxumHeaderMap, Json(body): Json<serde_json::Value>| {
                let sink = sink.clone();
                async move {
                    *sink.lock().unwrap() = Some((headers, body));
                    Json(json!({
                        "id": "chatcmpl-override-2",
                        "object": "chat.completion",
                        "model": "local-model",
                        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
                        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 },
                    }))
                }
            }),
        )
        .with_state(());
    let (base_url2, server2) = spawn_axum(app2).await;
    let mut provider2 = local_provider(Dialect::OpenAI, format!("{base_url2}/v1"));
    provider2.models = provider.models.clone();

    let mut req2 = chat_request();
    req2.temperature = Some(0.9);
    req2.max_tokens = Some(1000);
    let _ = client
        .call(&provider2, &req2, "local-model", Duration::from_secs(5), &llm_gateway_lib::config::OllamaOptionsConfig::default())
        .await
        .expect("第二次覆盖请求应成功");

    let (headers, body) = captured.lock().unwrap().clone().expect("应捕获上游请求");
    assert_eq!(body["temperature"], 0.25, "模型级温度覆盖必须覆盖客户端值");
    assert_eq!(body["max_tokens"], 77, "模型级 max_tokens 覆盖必须生效");
    assert_eq!(body["top_k"], 12, "额外请求体字段必须合并进上游请求");
    assert_eq!(body["seed"], 7);
    assert_eq!(
        headers
            .get("x-tenant")
            .and_then(|value| value.to_str().ok()),
        Some("tenant-42")
    );

    server.abort();
    server2.abort();
}

#[test]
fn model_override_validation_rejects_protected_and_malformed_values() {
    use llm_gateway_lib::domain::{HeaderPair, ModelOverrides};

    let ok = ModelOverrides {
        temperature: Some(1.0),
        max_tokens: Some(64),
        extra_body: Some(json!({ "top_k": 5 })),
        extra_headers: Some(vec![HeaderPair {
            name: "X-Ok".into(),
            value: "v".into(),
        }]),
    };
    assert!(ok.validate().is_ok());

    let cases: Vec<(ModelOverrides, &str)> = vec![
        (
            ModelOverrides {
                temperature: Some(2.5),
                ..Default::default()
            },
            "温度",
        ),
        (
            ModelOverrides {
                max_tokens: Some(0),
                ..Default::default()
            },
            "max_tokens",
        ),
        (
            ModelOverrides {
                extra_body: Some(json!(["not-an-object"])),
                ..Default::default()
            },
            "JSON 对象",
        ),
        (
            ModelOverrides {
                extra_body: Some(json!({ "messages": [] })),
                ..Default::default()
            },
            "messages",
        ),
        (
            ModelOverrides {
                extra_body: Some(json!({ "model": "hijack" })),
                ..Default::default()
            },
            "model",
        ),
        (
            ModelOverrides {
                extra_headers: Some(vec![HeaderPair {
                    name: "Authorization".into(),
                    value: "Bearer attacker".into(),
                }]),
                ..Default::default()
            },
            "Authorization",
        ),
        (
            ModelOverrides {
                extra_headers: Some(vec![HeaderPair {
                    name: "X-Bad Name".into(),
                    value: "v".into(),
                }]),
                ..Default::default()
            },
            "合法的 HTTP 头名称",
        ),
    ];

    for (overrides, expected) in cases {
        let error = overrides
            .validate()
            .expect_err(&format!("必须拒绝：{expected}"));
        assert!(
            error.contains(expected),
            "拒绝原因应包含 {expected}，实际为 {error}"
        );
    }
}
