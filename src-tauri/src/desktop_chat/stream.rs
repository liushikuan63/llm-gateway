//! Incremental desktop delivery. No credentials or raw transport errors in events.
use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct StreamEvent {
    pub request_id: String,
    pub session_id: String,
    pub requested_model: String,
    pub routed_via: Option<String>,
    pub sequence: u64,
    pub kind: String,
    pub text: String,
}

const LIMIT: usize = 8 * 1024 * 1024;

/// The callback is synchronous and request-owned: there is no detached delivery
/// worker, retry, or replay queue surviving cancellation of this future.
pub async fn send(
    cfg: &AppConfig,
    listener: std::net::SocketAddr,
    input: ChatInput,
    request: uuid::Uuid,
    mut emit: impl FnMut(StreamEvent) -> Result<(), String> + Send,
) -> Result<ChatResult, String> {
    validate(&input)?;
    if input.personal_test {
        return Err("desktop_stream_personal_test_unsupported".into());
    }
    let host = if listener.is_ipv6() {
        "[::1]"
    } else {
        "127.0.0.1"
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(request_timeout(cfg, &input.model)?)
        .build()
        .map_err(|_| "desktop_stream_client_failed")?;
    let started = Instant::now();
    let mut response = client.post(format!("http://{host}:{}/v1/chat/completions", listener.port()))
        .bearer_auth(&cfg.unified_key)
        .header("x-session-id", &input.session_id)
        .header("x-trace-id", request.simple().to_string())
        .header("x-client-name", "desktop-chat")
        .header("x-llmgw-desktop-observation", "1")
        .json(&serde_json::json!({"model":input.model,"messages":input.messages,"max_tokens":input.max_tokens,"stream":true}))
        .send().await.map_err(|_| "desktop_stream_connect_failed_check_audit")?;
    if !response.status().is_success() {
        // Do not echo arbitrary upstream response/error bodies into IPC or logs.
        return Err(format!(
            "desktop_stream_http_{}_check_audit",
            response.status().as_u16()
        ));
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    if !header("content-type").is_some_and(|s| {
        s.split(';')
            .next()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"))
    }) {
        return Err("desktop_stream_protocol_unsupported".into());
    }
    if header("x-session-id").is_some_and(|s| s != input.session_id) {
        return Err("desktop_stream_session_mismatch".into());
    }
    let routed_via = header("x-routed-via");
    let trace_id = header("x-trace-id");
    let fallback_attempts = header("x-fallback-attempts")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
        .saturating_sub(1);
    let mut decoder = Decoder::default();
    let mut sequence = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "desktop_stream_interrupted_check_audit")?
    {
        for (kind, text) in decoder.feed(&chunk)? {
            sequence += 1;
            emit(StreamEvent {
                request_id: request.to_string(),
                session_id: input.session_id.clone(),
                requested_model: input.model.clone(),
                routed_via: routed_via.clone(),
                sequence,
                kind,
                text,
            })
            .map_err(|_| "desktop_stream_delivery_failed_check_audit")?;
        }
    }
    let body = decoder.finish()?;
    let (finish_reason, usage, response_metadata) = reply_observations(&body);
    Ok(ChatResult {
        personal_test_outcome: None,
        content: decoder.content,
        reasoning: (!decoder.reasoning.is_empty()).then_some(decoder.reasoning),
        refusal: (!decoder.refusal.is_empty()).then_some(decoder.refusal),
        model: decoder.model.unwrap_or_else(|| "未知".into()),
        routed_via,
        session_id: input.session_id,
        trace_id,
        fallback_attempts,
        latency_ms: started.elapsed().as_millis() as u64,
        usage,
        finish_reason,
        response_metadata: Some(response_metadata),
        cached: false,
    })
}

#[derive(Default)]
struct Decoder {
    line: Vec<u8>,
    data: String,
    event: String,
    bytes: usize,
    done: bool,
    finish_seen: bool,
    observation: Option<Value>,
    id: Option<String>,
    model: Option<String>,
    content: String,
    reasoning: String,
    refusal: String,
    finish_reason: Option<Value>,
    usage: Option<Value>,
}
impl Decoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
        self.bytes = self.bytes.saturating_add(bytes.len());
        if self.bytes > LIMIT {
            return Err("desktop_stream_size_limit".into());
        }
        let mut out = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            let line = std::mem::take(&mut self.line);
            let line = std::str::from_utf8(&line)
                .map_err(|_| "desktop_stream_invalid_utf8")?
                .trim_end_matches('\r');
            if line.is_empty() {
                self.dispatch(&mut out)?;
            } else if !line.starts_with(':') {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "data" => {
                        if !self.data.is_empty() {
                            self.data.push('\n');
                        }
                        self.data.push_str(value);
                    }
                    "event" => self.event = value.into(),
                    "id" | "retry" => (), // SSE transport metadata is never a routing identity.
                    _ => (),
                }
            }
        }
        Ok(out)
    }
    fn dispatch(&mut self, out: &mut Vec<(String, String)>) -> Result<(), String> {
        let data = std::mem::take(&mut self.data);
        let event = std::mem::take(&mut self.event);
        if data.is_empty() {
            return Ok(());
        }
        if self.done {
            return Err("desktop_stream_after_done".into());
        }
        if data == "[DONE]" {
            if !event.is_empty() {
                return Err("desktop_stream_invalid_terminal".into());
            }
            self.done = true;
            return Ok(());
        }
        let value: Value =
            serde_json::from_str(&data).map_err(|_| "desktop_stream_invalid_json")?;
        match event.as_str() {
            "ping" => return Ok(()),
            "llmgw_reply_observation" => {
                if self.observation.is_some() {
                    return Err("desktop_stream_duplicate_observation".into());
                }
                let parsed: GatewayReplyObservation = serde_json::from_value(value.clone())
                    .map_err(|_| "desktop_stream_invalid_observation")?;
                if parsed.version != 1 {
                    return Err("desktop_stream_invalid_observation".into());
                }
                self.observation = Some(value);
                return Ok(());
            }
            "" | "message" => (),
            _ => return Err("desktop_stream_event_unsupported".into()),
        }
        if value.get("error").is_some() {
            return Err("desktop_stream_upstream_error_check_audit".into());
        }
        for (key, current) in [("id", &mut self.id), ("model", &mut self.model)] {
            if let Some(v) = value.get(key) {
                let v = v
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("desktop_stream_identity_invalid")?;
                if current.as_deref().is_some_and(|s| s != v) {
                    return Err("desktop_stream_identity_changed".into());
                }
                *current = Some(v.into());
            }
        }
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .ok_or("desktop_stream_choices_invalid")?;
        if choices.len() > 1 {
            return Err("desktop_stream_multiple_choices_unsupported".into());
        }
        if let Some(choice) = choices.first() {
            if choice.get("index").and_then(Value::as_u64) != Some(0) {
                return Err("desktop_stream_choice_invalid".into());
            }
            let delta = choice
                .get("delta")
                .and_then(Value::as_object)
                .ok_or("desktop_stream_delta_invalid")?;
            if delta.get("tool_calls").is_some_and(|v| !v.is_null()) {
                return Err("desktop_stream_tools_unsupported".into());
            }
            for (key, target, kind) in [
                ("content", &mut self.content, "content"),
                ("reasoning_content", &mut self.reasoning, "reasoning"),
                ("refusal", &mut self.refusal, "refusal"),
            ] {
                if let Some(v) = delta.get(key).filter(|v| !v.is_null()) {
                    let text = v.as_str().ok_or("desktop_stream_text_invalid")?;
                    if !text.is_empty() {
                        if self.finish_seen || self.observation.is_some() {
                            return Err("desktop_stream_delta_after_finish".into());
                        }
                        target.push_str(text);
                        out.push((kind.into(), text.into()));
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                if self.finish_seen {
                    return Err("desktop_stream_duplicate_finish".into());
                }
                self.finish_seen = true;
                self.finish_reason = Some(reason.clone());
            }
        }
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            if self.usage.is_some() {
                return Err("desktop_stream_duplicate_usage".into());
            }
            self.usage = Some(usage.clone());
        }
        Ok(())
    }
    fn finish(&self) -> Result<Value, String> {
        if !self.line.is_empty() || !self.data.is_empty() || !self.done {
            return Err("desktop_stream_missing_terminal".into());
        }
        let observation = self
            .observation
            .as_ref()
            .ok_or("desktop_stream_observation_unavailable")?;
        if self.content.is_empty() && self.reasoning.is_empty() && self.refusal.is_empty() {
            return Err("desktop_stream_empty_reply".into());
        }
        // Private pre-normalization evidence wins over compatibility counters.
        // Partial/null source usage may deliberately differ from public defaults.
        let usage = match observation.get("usage") {
            Some(value) => (!value.is_null()).then_some(value),
            None => self.usage.as_ref(),
        };
        Ok(
            serde_json::json!({"choices":[{"finish_reason":self.finish_reason}],"usage":usage,"llmgw_reply_observation":observation}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::post, Router};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn input() -> ChatInput {
        ChatInput {
            personal_test: false,
            model: "fixture-account:chat".into(),
            session_id: uuid::Uuid::new_v4().to_string(),
            max_tokens: 16,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "synthetic".into(),
                reasoning_content: None,
            }],
        }
    }

    fn frame(text: &str) -> String {
        format!(
            "data: {}\n\n",
            serde_json::json!({"id":"reply-1","model":"fixture-model","choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]})
        )
    }
    fn terminal() -> String {
        format!(
            "event: llmgw_reply_observation\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({"version":1,"finish_reason":null,"usage_present":false,"usage_evidence":"unobserved"}),
            serde_json::json!({"id":"reply-1","model":"fixture-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}})
        )
    }

    #[tokio::test]
    async fn delta_reaches_consumer_before_server_releases_terminal() {
        let seen = Arc::new(AtomicBool::new(false));
        let release = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let (s, r, c) = (seen.clone(), release.clone(), calls.clone());
        let app=Router::new().route("/v1/chat/completions",post(move |axum::Json(body):axum::Json<Value>| {
            let (s,r,c)=(s.clone(),r.clone(),c.clone());
            async move {
                c.fetch_add(1,Ordering::SeqCst); assert_eq!(body["stream"],true);
                let stream=async_stream::stream! {
                    // Split inside UTF-8 and SSE framing; each callback is real network progress.
                    let bytes=frame("早").into_bytes();
                    for b in bytes { yield Ok::<_,std::convert::Infallible>(vec![b]); }
                    tokio::time::timeout(Duration::from_secs(3),r.notified()).await.expect("consumer must emit before terminal");
                    assert!(s.load(Ordering::SeqCst));
                    yield Ok(terminal().into_bytes());
                };
                ([ ("content-type","text/event-stream"), ("x-routed-via","fixture-account/model") ], Body::from_stream(stream))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let data = input();
        let session = data.session_id.clone();
        let request = uuid::Uuid::new_v4();
        let mut events = Vec::new();
        let result = send(&AppConfig::default(), addr, data, request, |event| {
            assert_eq!(event.request_id, request.to_string());
            assert_eq!(event.session_id, session);
            assert_eq!(event.requested_model, "fixture-account:chat");
            assert_eq!(event.routed_via.as_deref(), Some("fixture-account/model"));
            assert_eq!(event.text, "早");
            seen.store(true, Ordering::SeqCst);
            release.notify_one();
            events.push(event);
            Ok(())
        })
        .await;
        server.abort();
        let result = result.expect("stream must deliver before aggregate completion");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(events.len(), 1);
        assert_eq!(result.content, "早");
        assert!(result.finish_reason.is_none());
        assert!(result.usage.is_none());
    }
    #[test]
    fn framing_and_terminal_contracts_are_strict_and_metadata_is_not_invented() {
        let complete = format!("{}{}", frame("answer"), terminal());
        for split in 0..=complete.len() {
            let mut d = Decoder::default();
            let mut events = d.feed(&complete.as_bytes()[..split]).unwrap();
            events.extend(d.feed(&complete.as_bytes()[split..]).unwrap());
            assert_eq!(events.len(), 1);
            assert_eq!(d.content, "answer");
            let (finish, usage, meta) = reply_observations(&d.finish().unwrap());
            assert!(finish.is_none());
            assert!(usage.is_none());
            assert!(matches!(meta.usage, ReplyFieldState::Unavailable));
        }
        let valid = complete.replace('\n', "\r\n");
        let mut d = Decoder::default();
        d.feed(valid.as_bytes()).unwrap();
        d.finish().unwrap();
        let mut d = Decoder::default();
        d.feed(frame("partial").as_bytes()).unwrap();
        assert!(d.finish().unwrap_err().contains("missing_terminal"));
        for (wire, expected) in [
            (format!("{complete}data: [DONE]\n\n"), "after_done"),
            (format!("{complete}{}", frame("late")), "after_done"),
            (
                format!(
                    "{}data: {{\"error\":{{\"message\":\"secret-marker\"}}}}\n\n",
                    frame("partial")
                ),
                "upstream_error",
            ),
            (
                format!("{}event: unknown\ndata: {{}}\n\n", frame("partial")),
                "event_unsupported",
            ),
            (
                format!(
                    "{}{}",
                    frame("first"),
                    frame("second").replace("reply-1", "reply-2")
                ),
                "identity_changed",
            ),
        ] {
            let mut d = Decoder::default();
            let error = d.feed(wire.as_bytes()).unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("secret-marker"));
        }
        let mut d = Decoder::default();
        d.feed(format!("{}data: [DONE]\n\n", frame("answer")).as_bytes())
            .unwrap();
        assert!(d.finish().unwrap_err().contains("observation_unavailable"));
    }

    #[test]
    fn late_usage_zero_and_partial_usage_are_preserved_without_settlement_claims() {
        for usage in [
            serde_json::json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
            serde_json::json!({"completion_tokens":2,"credit":1.5}),
        ] {
            let mut d = Decoder::default();
            let finish = serde_json::json!({"id":"reply-1","model":"fixture-model","choices":[{"index":0,"delta":{},"finish_reason":"length"}]});
            let observed = serde_json::json!({"version":1,"finish_reason":"length","usage_present":true,"usage_evidence":"unknown"});
            d.feed(format!("{}data: {finish}\n\ndata: {}\n\nevent: llmgw_reply_observation\ndata: {observed}\n\ndata: [DONE]\n\n",frame("a"),serde_json::json!({"choices":[],"usage":usage})).as_bytes()).unwrap();
            let (reason, actual, meta) = reply_observations(&d.finish().unwrap());
            assert_eq!(reason.as_deref(), Some("length"));
            assert_eq!(actual.as_ref(), Some(&usage));
            assert!(matches!(
                meta.usage_evidence,
                Some(ReplyUsageEvidence::Unknown)
            ));
            let mut duplicate = Decoder::default();
            assert!(duplicate
                .feed(format!("data: {finish}\n\ndata: {finish}\n\n").as_bytes())
                .unwrap_err()
                .contains("duplicate_finish"));
        }
        let mut d = Decoder::default();
        assert!(d
            .feed(&vec![b'a'; LIMIT + 1])
            .unwrap_err()
            .contains("size_limit"));
        let mut d = Decoder::default();
        assert!(d
            .feed(b"data: \xff\n\n")
            .unwrap_err()
            .contains("invalid_utf8"));
    }

    #[tokio::test]
    async fn controlled_cancel_drops_pending_transport_without_retry_or_late_delivery() {
        let controls = Arc::new(RequestControls::default());
        let request_id = uuid::Uuid::new_v4();
        let request = controls
            .register("window", &request_id.to_string())
            .unwrap();
        let (sent, received) = oneshot::channel();
        let delivered = Arc::new(AtomicUsize::new(0));
        let seen = delivered.clone();
        let task = tokio::spawn(request.run(async move {
            seen.fetch_add(1, Ordering::SeqCst);
            sent.send(()).unwrap();
            std::future::pending::<Result<(), String>>().await
        }));
        received.await.unwrap();
        assert!(controls.cancel("window", &request_id.to_string()).unwrap());
        assert_eq!(task.await.unwrap().unwrap_err(), CANCELLED);
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
        assert!(!controls.cancel("window", &request_id.to_string()).unwrap());
    }

    #[tokio::test]
    async fn cancel_actual_http_stream_ends_delivery_and_never_posts_twice() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let app=Router::new().route("/v1/chat/completions",post(move || {let count=count.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            let wire=async_stream::stream! {yield Ok::<_,std::convert::Infallible>(frame("partial"));std::future::pending::<()>().await;};
            ([("content-type","text/event-stream")],Body::from_stream(wire))
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let controls = Arc::new(RequestControls::default());
        let id = uuid::Uuid::new_v4();
        let request = controls.register("owned-window", &id.to_string()).unwrap();
        let progress = Arc::new(tokio::sync::Notify::new());
        let callback = progress.clone();
        let chunks = Arc::new(AtomicUsize::new(0));
        let delivered = chunks.clone();
        let task = tokio::spawn(async move {
            request
                .run(send(&AppConfig::default(), addr, input(), id, move |_| {
                    delivered.fetch_add(1, Ordering::SeqCst);
                    callback.notify_one();
                    Ok(())
                }))
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), progress.notified())
            .await
            .unwrap();
        assert!(!controls.cancel("other-window", &id.to_string()).unwrap());
        assert!(controls.cancel("owned-window", &id.to_string()).unwrap());
        assert_eq!(task.await.unwrap().unwrap_err(), CANCELLED);
        assert_eq!(chunks.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn unsupported_http_and_personal_paths_fail_closed_without_response_body_leaks() {
        for (status, content_type, expected) in [
            (200, "application/json", "protocol_unsupported"),
            (401, "text/event-stream", "http_401"),
            (302, "text/event-stream", "http_302"),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let app = Router::new().route(
                "/v1/chat/completions",
                post(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        axum::http::Response::builder()
                            .status(status)
                            .header("content-type", content_type)
                            .header("location", "http://127.0.0.1:1/never-follow")
                            .body(Body::from("private-response-marker"))
                            .unwrap()
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let error = send(
                &AppConfig::default(),
                addr,
                input(),
                uuid::Uuid::new_v4(),
                |_| panic!("no delta expected"),
            )
            .await
            .unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("private-response-marker"));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let mut personal = input();
            personal.personal_test = true;
            assert!(send(
                &AppConfig::default(),
                addr,
                personal,
                uuid::Uuid::new_v4(),
                |_| Ok(())
            )
            .await
            .unwrap_err()
            .contains("personal_test_unsupported"));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            server.abort();
        }
    }
}
