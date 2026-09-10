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
use axum::Router;
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
