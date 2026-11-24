//! N4 owned Gateway/SQLite evidence, not vendor capture or installed acceptance.
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    response::Response,
    Router,
};
use futures_util::FutureExt;
use llm_gateway_lib::{
    config::AppConfig,
    db::{repo, Db},
    domain::Provider,
    proxy::server::GatewayState,
    proxy_control::gateway::GatewayService,
};
use serde_json::{json, Value};
use sqlx::Row;
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug)]
enum Ending {
    Complete,
    MissingTerminal,
    ErrorAfterDelta,
    ErrorThenTerminal,
    ErrorBeforeContent,
    MalformedBeforeContent,
    MalformedAfterContent,
}
struct Scenario {
    dialect: &'static str,
    ending: Ending,
    reason: &'static str,
    tool_frames: Option<(ToolFrames, watch::Receiver<u8>)>,
    calls: Mutex<Vec<(String, String, Value)>>,
}
fn event(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}
fn error_wire(dialect: &str) -> String {
    let message = "owned-private-upstream-message";
    match dialect {
        "anthropic" => event(
            "error",
            json!({"type":"error","error":{"type":"overloaded_error","message":message}}),
        ),
        "ollama" => format!("{}\n", json!({"error":message})),
        "gemini" => format!(
            "data: {}\n\n",
            json!({"error":{"code":503,"status":"UNAVAILABLE","message":message}})
        ),
        _ => format!(
            "data: {}\n\n",
            json!({"error":{"type":"server_error","code":"owned_error","message":message}})
        ),
    }
}
fn responses_error(reason: &str) -> String {
    if reason == "error" {
        event(
            "error",
            json!({"type":"error","code":"server_error","message":"owned-private-upstream-message"}),
        )
    } else {
        event(
            "response.failed",
            json!({"type":"response.failed","response":{"status":"failed","model":"owned-private-upstream-model","error":{"code":"server_error","message":"owned-private-upstream-message"}}}),
        )
    }
}
const BAD_TERMINALS: &[&str] = &[
    "missing_response",
    "null_response",
    "array_response",
    "missing_status",
    "unknown_status",
    "failed_status",
    "completed_incomplete",
    "incomplete_completed",
    "missing_output",
    "wrong_output",
    "invalid_item",
    "error_payload",
    "outer_error",
    "missing_reason",
    "completed_with_incomplete_details",
    "header_conflict",
];
fn malformed_terminal(reason: &str) -> String {
    let mut v = json!({"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}});
    match reason {
        "missing_response" => {
            v.as_object_mut().unwrap().remove("response");
        }
        "null_response" => v["response"] = Value::Null,
        "array_response" => v["response"] = json!([]),
        "missing_status" => {
            v["response"].as_object_mut().unwrap().remove("status");
        }
        "unknown_status" => v["response"]["status"] = json!("in_progress"),
        "failed_status" => v["response"]["status"] = json!("failed"),
        "completed_incomplete" => {
            v["response"]["status"] = json!("incomplete");
            v["response"]["incomplete_details"] = json!({"reason":"max_output_tokens"});
        }
        "incomplete_completed" => v["type"] = json!("response.incomplete"),
        "missing_output" => {
            v["response"].as_object_mut().unwrap().remove("output");
        }
        "wrong_output" => v["response"]["output"] = json!({}),
        "invalid_item" => v["response"]["output"] = json!([null]),
        "error_payload" => {
            v["response"]["error"] = json!({"message":"owned-private-upstream-message"})
        }
        "outer_error" => v["error"] = json!({"message":"owned-private-upstream-message"}),
        "missing_reason" => {
            v["type"] = json!("response.incomplete");
            v["response"]["status"] = json!("incomplete");
        }
        "completed_with_incomplete_details" => {
            v["response"]["incomplete_details"] = json!({"reason":"max_output_tokens"})
        }
        "header_conflict" => return event("response.failed", v),
        _ => unreachable!(),
    }
    event(v["type"].as_str().unwrap(), v.clone())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolFrames {
    SameFrame,
    Split,
    SplitWithoutIds,
    SplitWithUndelimitedTail,
    SplitWithTruncatedRecord,
    AnthropicEmpty(AnthropicEmptyFlow),
}

type ToolCall = (&'static str, &'static str, &'static str);

const TOOL_CALLS: [ToolCall; 2] = [
    ("owned-weather", "lookup_weather", r#"{"city":"Paris"}"#),
    ("owned-clock", "lookup_clock", r#"{"utc_offset":2}"#),
];

impl ToolFrames {
    fn expected_calls(self) -> Vec<ToolCall> {
        if self == Self::SplitWithoutIds {
            vec![
                ("call_0", TOOL_CALLS[0].1, TOOL_CALLS[0].2),
                ("call_1", TOOL_CALLS[1].1, TOOL_CALLS[1].2),
            ]
        } else if self == Self::SplitWithTruncatedRecord {
            TOOL_CALLS[..1].to_vec()
        } else {
            TOOL_CALLS.to_vec()
        }
    }
}

fn gemini_calls_wire(calls: &[ToolCall], include_ids: bool) -> String {
    let parts: Vec<_> = calls
        .iter()
        .map(|(id, name, args)| {
            let mut call =
                json!({"name": name, "args": serde_json::from_str::<Value>(args).unwrap()});
            if include_ids {
                call["id"] = json!(id);
            }
            json!({"functionCall": call})
        })
        .collect();
    format!(
        "data: {}",
        json!({"candidates": [{"content": {"role": "model", "parts": parts}}]})
    )
}

fn gemini_stop_wire() -> String {
    format!(
        "data: {}\n\n",
        json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}})
    )
}

fn ollama_calls_wire(calls: &[ToolCall], include_ids: bool) -> String {
    let calls: Vec<_> = calls
        .iter()
        .map(|(id, name, args)| {
            let mut call = json!({
                "function": {"name": name, "arguments": serde_json::from_str::<Value>(args).unwrap()}
            });
            if include_ids {
                call["id"] = json!(id);
            }
            call
        })
        .collect();
    json!({"model":"owned-physical","message":{"role":"assistant","content":"","tool_calls":calls},"done":false})
        .to_string()
}

fn ollama_done_wire() -> String {
    format!(
        "{}\n",
        json!({"model":"owned-physical","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":3,"eval_count":2})
    )
}

fn wire(s: &Scenario) -> String {
    if matches!(s.ending, Ending::MalformedBeforeContent) {
        return malformed_terminal(s.reason);
    }
    if matches!(s.ending, Ending::ErrorBeforeContent) {
        if s.dialect == "responses" {
            return responses_error(s.reason);
        }
        return error_wire(s.dialect);
    }
    match s.dialect {
        "anthropic" => {
            let mut value = event(
                "message_start",
                json!({"type":"message_start","message":{"id":"owned-n4","role":"assistant","content":[],"usage":{"input_tokens":3,"output_tokens":0}}}),
            );
            value += &event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            );
            value += &event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"owned partial"}}),
            );
            value += &event(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            );
            value += &event(
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":s.reason},"usage":{"output_tokens":2}}),
            );
            match s.ending {
                Ending::Complete => value += &event("message_stop", json!({"type":"message_stop"})),
                Ending::ErrorAfterDelta => {
                    value += &event(
                        "error",
                        json!({"type":"error","error":{"type":"overloaded_error","message":"owned stream error"}}),
                    )
                }
                Ending::MissingTerminal => {}
                Ending::ErrorThenTerminal => {
                    value += &error_wire(s.dialect);
                    value += &event("message_stop", json!({"type":"message_stop"}));
                }
                Ending::ErrorBeforeContent => unreachable!(),
                Ending::MalformedBeforeContent | Ending::MalformedAfterContent => unreachable!(),
            }
            value
        }
        "gemini" => {
            let mut value = format!(
                "data: {}\n\n",
                json!({"candidates":[{"content":{"role":"model","parts":[{"text":"owned partial"}]}}]})
            );
            if matches!(s.ending, Ending::ErrorThenTerminal) {
                value += &error_wire(s.dialect);
            }
            if matches!(s.ending, Ending::Complete | Ending::ErrorThenTerminal) {
                value += &gemini_stop_wire();
            }
            value
        }
        "ollama" => {
            let mut value = format!(
                "{}\n",
                json!({"model":"owned-physical","message":{"role":"assistant","content":"owned partial"},"done":false})
            );
            if matches!(s.ending, Ending::ErrorThenTerminal) {
                value += &error_wire(s.dialect);
            }
            if matches!(s.ending, Ending::Complete | Ending::ErrorThenTerminal) {
                value += &ollama_done_wire();
            }
            value
        }
        "openai" => {
            let mut value = format!(
                "data: {}\n\n",
                json!({"choices":[{"index":0,"delta":{"content":"owned partial"},"finish_reason":null}],"error":null,"metadata":{"error":"benign nested field"}})
            );
            if matches!(s.ending, Ending::ErrorThenTerminal) {
                value += &error_wire(s.dialect);
            }
            if matches!(s.ending, Ending::Complete | Ending::ErrorThenTerminal) {
                value += &format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                    json!({"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}})
                );
            }
            value
        }
        "responses" => {
            let mut value = event(
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"owned partial"}),
            );
            if matches!(
                s.ending,
                Ending::ErrorAfterDelta | Ending::ErrorThenTerminal
            ) {
                value += &responses_error(s.reason);
            }
            if matches!(s.ending, Ending::MalformedAfterContent) {
                value += &malformed_terminal(s.reason);
            }
            if matches!(
                s.ending,
                Ending::Complete | Ending::ErrorThenTerminal | Ending::MalformedAfterContent
            ) {
                value += &event(
                    "response.completed",
                    json!({"type":"response.completed","response":{"id":"owned-n4","object":"response","status":"completed","model":"owned-physical","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"owned partial"}]}],"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}),
                );
            }
            value
        }
        _ => unreachable!(),
    }
}
async fn respond(State(s): State<Arc<Scenario>>, request: Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().to_string();
    let data = to_bytes(request.into_body(), 262144).await.unwrap();
    s.calls.lock().unwrap().push((
        method,
        path,
        serde_json::from_slice(&data).unwrap_or(Value::Null),
    ));
    let body = if let Some((ToolFrames::AnthropicEmpty(flow), mut gate)) = s.tool_frames.clone() {
        Body::from_stream(async_stream::stream! {
            let (mut prefix, block_stop, message_delta, message_stop) = anthropic_empty_tool_wire();
            if flow == AnthropicEmptyFlow::AfterBlockStop {
                prefix.push_str(&block_stop);
            }
            yield Ok::<_, Infallible>(prefix);
            if gate.wait_for(|phase| *phase >= 1).await.is_err() {
                return;
            }
            match flow {
                AnthropicEmptyFlow::Complete => yield Ok(block_stop + &message_delta + &message_stop),
                AnthropicEmptyFlow::BeforeBlockStop => {
                    yield Ok(truncated_anthropic_frame(&block_stop).to_owned());
                }
                AnthropicEmptyFlow::AfterBlockStop => {
                    yield Ok(truncated_anthropic_frame(&message_delta).to_owned());
                }
            }
        })
    } else if let Some((frames, mut gate)) = s.tool_frames.clone() {
        let complete = matches!(s.ending, Ending::Complete);
        let ollama = s.dialect == "ollama";
        let (frame, delimiter, terminal) = if ollama {
            (
                ollama_calls_wire as fn(&[ToolCall], bool) -> String,
                "\n",
                ollama_done_wire as fn() -> String,
            )
        } else {
            (
                gemini_calls_wire as fn(&[ToolCall], bool) -> String,
                "\n\n",
                gemini_stop_wire as fn() -> String,
            )
        };
        Body::from_stream(async_stream::stream! {
            let include_ids = frames != ToolFrames::SplitWithoutIds;
            if frames == ToolFrames::SameFrame {
                yield Ok::<_, Infallible>(frame(&TOOL_CALLS, include_ids) + delimiter);
            } else {
                yield Ok(frame(&TOOL_CALLS[..1], include_ids) + delimiter);
                // Make the two tool messages reach the client in separate HTTP reads.
                if gate.wait_for(|phase| *phase >= 1).await.is_err() {
                    return;
                }
                let mut second = frame(&TOOL_CALLS[1..], include_ids);
                match frames {
                    ToolFrames::SplitWithUndelimitedTail => {
                        // HTTP EOF exposes the complete JSON without a protocol terminal.
                        yield Ok(second);
                        return;
                    }
                    ToolFrames::SplitWithTruncatedRecord => {
                        second.pop().unwrap();
                        yield Ok(second);
                        return;
                    }
                    _ => yield Ok(second + delimiter),
                }
            }
            // Release the terminal (or EOF) only after the client saw both calls.
            if gate.wait_for(|phase| *phase >= 2).await.is_ok() && complete {
                yield Ok(terminal());
            }
        })
    } else {
        Body::from(wire(&s))
    };
    Response::builder()
        .status(200)
        .header(
            "content-type",
            if s.dialect == "ollama" {
                "application/x-ndjson"
            } else {
                "text/event-stream"
            },
        )
        .body(body)
        .unwrap()
}
struct Owned {
    state: Arc<GatewayState>,
    service: GatewayService,
    scenario: Arc<Scenario>,
    base: String,
    upstream: tokio::task::JoinHandle<()>,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.upstream.abort();
    }
}
impl Owned {
    async fn new(dialect: &'static str, ending: Ending, reason: &'static str) -> Self {
        Self::start(dialect, ending, reason, None).await
    }

    async fn start(
        dialect: &'static str,
        ending: Ending,
        reason: &'static str,
        tool_frames: Option<(ToolFrames, watch::Receiver<u8>)>,
    ) -> Self {
        let supports_tools = tool_frames.is_some();
        let scenario = Arc::new(Scenario {
            dialect,
            ending,
            reason,
            tool_frames,
            calls: Mutex::new(Vec::new()),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(respond).with_state(scenario.clone());
        let upstream = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let db = Db::connect_in_memory().await.unwrap();
        for (id, priority) in [("primary", 0), ("backup", 100)] {
            let suffix = match dialect {
                "anthropic" => "/v1",
                "gemini" => "/v1beta",
                _ => "",
            };
            let physical = if id == "primary" {
                "owned-physical"
            } else {
                "backup-physical"
            };
            let p:Provider=serde_json::from_value(json!({"id":id,"name":id,"base_url":format!("{origin}{suffix}"),"api_key_enc":"","dialect":dialect,"enabled":true,"priority":priority,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"owned-model","upstream":physical,"enabled":true,"context_window":32768,"supports_tools":supports_tools,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","price":null}]})).unwrap();
            repo::upsert_provider(db.pool(), &p).await.unwrap();
        }
        let mut cfg = AppConfig {
            port: 0,
            unified_key: "owned-n4-key".into(),
            routing_strategy: llm_gateway_lib::config::RoutingStrategy::Priority,
            ..Default::default()
        };
        cfg.cache.enabled = false;
        cfg.search.enabled = false;
        cfg.smart_routing.enabled = false;
        cfg.dedup.mode = llm_gateway_lib::proxy::dedup::DedupMode::Off;
        let state = Arc::new(GatewayState::new(db, cfg));
        state.reload_providers().await.unwrap();
        let service = GatewayService::new(state.clone());
        let started = service.set_enabled(true).await.unwrap();
        let base = format!("http://{}", started.bound_addr.unwrap());
        Self {
            state,
            service,
            scenario,
            base,
            upstream,
        }
    }
    async fn verify(&self) {
        let response=reqwest::Client::builder().no_proxy().build().unwrap().post(format!("{}/v1/chat/completions",self.base)).bearer_auth("owned-n4-key").header("x-session-id","owned-n4-session").json(&json!({"model":"owned-model","messages":[{"role":"user","content":"owned request"}],"stream":true,"max_tokens":16})).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let trace = response
            .headers()
            .get("x-trace-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let body = response.text().await.unwrap();
        assert!(body.contains("owned partial"));
        assert!(!body.contains("owned-private-upstream-message"));
        let complete = matches!(self.scenario.ending, Ending::Complete);
        if complete {
            assert_eq!(body.matches("data: [DONE]").count(), 1);
            assert!(!body.contains("\"error\""));
            let reason = if self.scenario.reason == "max_tokens" {
                "length"
            } else {
                "stop"
            };
            assert!(body.contains(&format!("\"finish_reason\":\"{reason}\"")));
        } else {
            assert!(
                !body.contains("data: [DONE]"),
                "{:?} {} must not invent completion after message delta: {body}",
                self.scenario.ending,
                self.scenario.dialect
            );
            assert!(body.contains("\"error\""));
        }
        self.service.set_enabled(false).await.unwrap();
        {
            let calls = self.scenario.calls.lock().unwrap();
            assert_eq!(
                calls.len(),
                1,
                "no after-content retry, fallback or additional request"
            );
            assert_eq!(calls[0].0, "POST");
            assert!(calls[0].1.contains(match self.scenario.dialect {
                "anthropic" => "/messages",
                "gemini" => "owned-physical:streamGenerateContent",
                "openai" => "/chat/completions",
                "responses" => "/responses",
                _ => "/api/chat",
            }));
        }
        let rows=sqlx::query("SELECT status,prompt_tokens,completion_tokens,fallback_attempts,attempts_json,error FROM requests WHERE trace_id=?").bind(&trace).fetch_all(self.state.db.pool()).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            row.get::<i64, _>("status"),
            if complete { 200 } else { 502 }
        );
        assert_eq!(row.get::<i64, _>("fallback_attempts"), 0);
        let attempts: Value = serde_json::from_str(&row.get::<String, _>("attempts_json")).unwrap();
        assert_eq!(attempts.as_array().unwrap().len(), 1);
        if complete {
            assert_eq!(row.get::<i64, _>("prompt_tokens"), 3);
            assert_eq!(row.get::<i64, _>("completion_tokens"), 2);
            assert!(row.get::<Option<String>, _>("error").is_none());
        } else {
            let error = row.get::<Option<String>, _>("error").unwrap();
            assert!(!error.contains("owned-private-upstream-message"));
        }
        let sticky: Option<String> = sqlx::query_scalar(
            "SELECT sticky_provider_id FROM sessions WHERE id='owned-n4-session'",
        )
        .fetch_one(self.state.db.pool())
        .await
        .unwrap();
        assert_eq!(
            sticky.as_deref(),
            if complete { Some("primary") } else { None }
        );
        let stored = repo::recent_messages(self.state.db.pool(), "owned-n4-session", 10)
            .await
            .unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[1].content, "owned partial");
    }
    async fn finish(&self, result: std::thread::Result<()>) {
        self.service.set_enabled(false).await.unwrap();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
    async fn verify_error_before_content(&self) {
        let response=reqwest::Client::builder().no_proxy().build().unwrap().post(format!("{}/v1/chat/completions",self.base)).bearer_auth("owned-n4-key").header("x-session-id","owned-n4-inline").json(&json!({"model":"owned-model","messages":[{"role":"user","content":"owned request"}],"stream":true,"max_tokens":16})).send().await.unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(status, 502, "{}: {body}", self.scenario.dialect);
        assert_eq!(
            self.scenario.calls.lock().unwrap().len(),
            1,
            "HTTP request already dispatched; no trusted zero-cost proof for a retry/fallback"
        );
        assert!(!body.contains("owned-private-upstream-message"));
        assert!(!body.contains("owned-private-upstream-model"));
        self.service.set_enabled(false).await.unwrap();
        assert_eq!(
            self.scenario.calls.lock().unwrap().len(),
            1,
            "HTTP request already dispatched; no trusted zero-cost proof for a retry/fallback"
        );
        let rows=sqlx::query("SELECT status,fallback_attempts,attempts_json,error FROM requests WHERE session_id='owned-n4-inline'").fetch_all(self.state.db.pool()).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64, _>("status"), 502);
        assert_eq!(rows[0].get::<i64, _>("fallback_attempts"), 0);
        let attempts: Value =
            serde_json::from_str(&rows[0].get::<String, _>("attempts_json")).unwrap();
        assert_eq!(attempts.as_array().unwrap().len(), 1);
        let error = rows[0].get::<String, _>("error");
        assert!(!error.contains("owned-private-upstream-message"));
        let messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_messages")
            .fetch_one(self.state.db.pool())
            .await
            .unwrap();
        assert_eq!(messages, 0);
    }
}
#[tokio::test]
async fn anthropic_message_delta_is_not_a_terminal_gateway_event() {
    for (ending, reason) in [
        (Ending::Complete, "end_turn"),
        (Ending::Complete, "max_tokens"),
        (Ending::MissingTerminal, "end_turn"),
        (Ending::ErrorAfterDelta, "end_turn"),
    ] {
        let f = Owned::new("anthropic", ending, reason).await;
        let result = std::panic::AssertUnwindSafe(f.verify())
            .catch_unwind()
            .await;
        f.finish(result).await;
    }
}
#[tokio::test]
async fn gemini_and_ollama_require_their_own_terminal_and_do_not_replay_partial_content() {
    for dialect in ["gemini", "ollama"] {
        for ending in [Ending::Complete, Ending::MissingTerminal] {
            let f = Owned::new(dialect, ending, "stop").await;
            let result = std::panic::AssertUnwindSafe(f.verify())
                .catch_unwind()
                .await;
            f.finish(result).await;
        }
    }
}

#[tokio::test]
async fn native_inline_error_cannot_be_hidden_by_a_later_terminal() {
    for dialect in ["anthropic", "gemini", "ollama", "openai"] {
        let f = Owned::new(dialect, Ending::ErrorThenTerminal, "stop").await;
        let result = std::panic::AssertUnwindSafe(f.verify())
            .catch_unwind()
            .await;
        f.finish(result).await;
    }
}
#[tokio::test]
async fn native_inline_errors_before_content_never_replay_unknown_dispatch() {
    let normal = Owned::new("openai", Ending::Complete, "stop").await;
    let result = std::panic::AssertUnwindSafe(normal.verify())
        .catch_unwind()
        .await;
    normal.finish(result).await;
    for dialect in ["anthropic", "gemini", "ollama", "openai"] {
        let f = Owned::new(dialect, Ending::ErrorBeforeContent, "stop").await;
        let result = std::panic::AssertUnwindSafe(f.verify_error_before_content())
            .catch_unwind()
            .await;
        f.finish(result).await;
    }
}

#[tokio::test]
async fn responses_precontent_failed_and_error_never_replay_unknown_dispatch() {
    let normal = Owned::new("responses", Ending::Complete, "stop").await;
    let result = std::panic::AssertUnwindSafe(normal.verify())
        .catch_unwind()
        .await;
    normal.finish(result).await;
    for reason in ["failed", "error"] {
        let f = Owned::new("responses", Ending::ErrorBeforeContent, reason).await;
        let result = std::panic::AssertUnwindSafe(f.verify_error_before_content())
            .catch_unwind()
            .await;
        f.finish(result).await;
    }
}
#[tokio::test]
async fn responses_postcontent_error_remains_failed_and_private() {
    for reason in ["failed", "error"] {
        let f = Owned::new("responses", Ending::ErrorThenTerminal, reason).await;
        let result = std::panic::AssertUnwindSafe(f.verify())
            .catch_unwind()
            .await;
        f.finish(result).await;
    }
}

#[tokio::test]
async fn responses_bad_envelope_before_content_is_one_unknown_dispatch() {
    for reason in BAD_TERMINALS {
        let f = Owned::new("responses", Ending::MalformedBeforeContent, reason).await;
        let result = std::panic::AssertUnwindSafe(f.verify_error_before_content())
            .catch_unwind()
            .await;
        f.finish(result).await;
    }
}
#[tokio::test]
async fn responses_bad_envelope_after_content_never_turns_partial_output_into_success() {
    for reason in std::iter::once(&"null_response")
        .chain(BAD_TERMINALS.iter().filter(|&&r| r != "null_response"))
    {
        let f = Owned::new("responses", Ending::MalformedAfterContent, reason).await;
        let result = std::panic::AssertUnwindSafe(f.verify())
            .catch_unwind()
            .await;
        f.finish(result).await;
    }
}

async fn next_responses_event(
    response: &mut reqwest::Response,
    pending: &mut Vec<u8>,
) -> Option<Value> {
    loop {
        if let Some(end) = pending.windows(2).position(|pair| pair == b"\n\n") {
            let frame = std::str::from_utf8(&pending[..end]).unwrap();
            let kind = frame
                .lines()
                .find_map(|line| line.strip_prefix("event: "))
                .expect("Responses SSE event name");
            let data = frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .expect("Responses SSE event data");
            let payload: Value = serde_json::from_str(data).expect("Responses SSE JSON");
            assert_eq!(payload["type"], kind);
            pending.drain(..end + 2);
            return Some(payload);
        }
        let Some(chunk) = response.chunk().await.expect("read Responses SSE chunk") else {
            assert!(pending.is_empty(), "truncated downstream SSE frame");
            return None;
        };
        pending.extend_from_slice(&chunk);
    }
}

async fn verify_tool_responses(
    f: &Owned,
    release: watch::Sender<u8>,
    frames: ToolFrames,
    complete: bool,
) {
    const SESSION: &str = "owned-n4-upstream-tools";
    const INPUT: &str = "owned tool request";
    let calls = frames.expected_calls();
    let tools = TOOL_CALLS.map(
        |(_, name, _)| json!({"type": "function", "name": name, "parameters": {"type": "object"}}),
    );
    let mut response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .post(format!("{}/v1/responses", f.base))
        .bearer_auth("owned-n4-key")
        .header("x-session-id", SESSION)
        .json(&json!({"model": "owned-model", "input": INPUT, "stream": true, "tools": tools}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let trace = response.headers()["x-trace-id"]
        .to_str()
        .unwrap()
        .to_owned();

    let mut pending = Vec::new();
    let created = next_responses_event(&mut response, &mut pending)
        .await
        .unwrap();
    assert_eq!(created["type"], "response.created");
    assert_eq!(created["response"]["status"], "in_progress");
    assert_eq!(created["response"]["output"], json!([]));
    let response_id = created["response"]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .unwrap();

    let mut expected_output = Vec::new();
    for (index, (call_id, name, arguments)) in calls.iter().copied().enumerate() {
        let added = next_responses_event(&mut response, &mut pending)
            .await
            .unwrap();
        assert_eq!(
            added["type"], "response.output_item.added",
            "{} {frames:?} call {index} must open a separate Responses item",
            f.scenario.dialect
        );
        assert_eq!(added["output_index"], index);
        let item_id = added["item"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .unwrap();
        assert_eq!(
            added["item"],
            json!({
                "id": item_id, "type": "function_call", "status": "in_progress",
                "call_id": call_id, "name": name, "arguments": ""
            })
        );

        let delta = next_responses_event(&mut response, &mut pending)
            .await
            .unwrap();
        assert_eq!(delta["type"], "response.function_call_arguments.delta");
        assert_eq!(delta["output_index"], index);
        assert_eq!(delta["item_id"], item_id);
        assert_eq!(delta["delta"], arguments);
        expected_output.push(json!({
            "id": item_id, "type": "function_call", "status": "completed",
            "call_id": call_id, "name": name, "arguments": arguments
        }));
        if index == 0 && frames != ToolFrames::SameFrame {
            release.send(1).unwrap();
        }
    }
    if expected_output.len() == 2 {
        assert_ne!(expected_output[0]["id"], expected_output[1]["id"]);
        assert_ne!(expected_output[0]["call_id"], expected_output[1]["call_id"]);
    }
    if !matches!(
        frames,
        ToolFrames::SplitWithUndelimitedTail | ToolFrames::SplitWithTruncatedRecord
    ) {
        release.send(2).unwrap();
    }

    if complete {
        for (index, item) in expected_output.iter().enumerate() {
            let arguments_done = next_responses_event(&mut response, &mut pending)
                .await
                .unwrap();
            assert_eq!(
                arguments_done["type"],
                "response.function_call_arguments.done"
            );
            assert_eq!(arguments_done["output_index"], index);
            assert_eq!(arguments_done["item_id"], item["id"]);
            for key in ["call_id", "name", "arguments"] {
                assert_eq!(arguments_done[key], item[key]);
            }
            let item_done = next_responses_event(&mut response, &mut pending)
                .await
                .unwrap();
            assert_eq!(item_done["type"], "response.output_item.done");
            assert_eq!(item_done["output_index"], index);
            assert_eq!(&item_done["item"], item);
        }
        let completed = next_responses_event(&mut response, &mut pending)
            .await
            .unwrap();
        assert_eq!(completed["type"], "response.completed");
        assert_eq!(completed["response"]["id"], response_id);
        assert_eq!(completed["response"]["status"], "completed");
        assert_eq!(completed["response"]["output"], json!(expected_output));
        assert_eq!(
            completed["response"]["usage"],
            json!({
                "input_tokens": 3, "output_tokens": 2, "total_tokens": 5
            })
        );
    } else {
        let error = next_responses_event(&mut response, &mut pending)
            .await
            .unwrap();
        assert_eq!(error["type"], "error");
        assert!(error["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()));
    }
    assert!(next_responses_event(&mut response, &mut pending)
        .await
        .is_none());

    f.service.set_enabled(false).await.unwrap();
    {
        let upstream_requests = f.scenario.calls.lock().unwrap();
        assert_eq!(upstream_requests.len(), 1, "no replay or backup POST");
        let (method, path, request) = &upstream_requests[0];
        assert_eq!(method, "POST");
        if f.scenario.dialect == "ollama" {
            assert_eq!(path, "/api/chat");
            assert_eq!(request["model"], "owned-physical");
            assert_eq!(request["stream"], true);
            assert_eq!(request["messages"][0]["content"], INPUT);
            let declarations = request["tools"].as_array().unwrap();
            assert_eq!(declarations.len(), TOOL_CALLS.len());
            for (declaration, (_, name, _)) in declarations.iter().zip(TOOL_CALLS) {
                assert_eq!(declaration["function"]["name"], name);
            }
        } else {
            assert_eq!(
                path,
                "/v1beta/models/owned-physical:streamGenerateContent?alt=sse"
            );
            assert_eq!(request["contents"][0]["parts"][0]["text"], INPUT);
            let declarations = request["tools"][0]["functionDeclarations"]
                .as_array()
                .unwrap();
            assert_eq!(declarations.len(), TOOL_CALLS.len());
            for (declaration, (_, name, _)) in declarations.iter().zip(TOOL_CALLS) {
                assert_eq!(declaration["name"], name);
            }
        }
    }

    let rows = sqlx::query(
        "SELECT status, prompt_tokens, completion_tokens, fallback_attempts, attempts_json, error FROM requests WHERE trace_id = ?",
    )
    .bind(&trace)
    .fetch_all(f.state.db.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        row.get::<i64, _>("status"),
        if complete { 200 } else { 502 }
    );
    assert_eq!(row.get::<i64, _>("fallback_attempts"), 0);
    let attempts: Value = serde_json::from_str(&row.get::<String, _>("attempts_json")).unwrap();
    assert_eq!(attempts.as_array().unwrap().len(), 1);
    assert_eq!(row.get::<Option<String>, _>("error").is_none(), complete);
    if complete {
        assert_eq!(row.get::<i64, _>("prompt_tokens"), 3);
        assert_eq!(row.get::<i64, _>("completion_tokens"), 2);
    }
    let sticky: Option<String> =
        sqlx::query_scalar("SELECT sticky_provider_id FROM sessions WHERE id = ?")
            .bind(SESSION)
            .fetch_one(f.state.db.pool())
            .await
            .unwrap();
    assert_eq!(
        sticky.as_deref(),
        if complete { Some("primary") } else { None }
    );

    let stored = repo::recent_messages(f.state.db.pool(), SESSION, 10)
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].role, "user");
    assert_eq!(stored[0].content, INPUT);
    assert_eq!(stored[1].role, "assistant");
    assert!(stored[1].content.is_empty());
    let stored_calls: Value =
        serde_json::from_str(stored[1].tool_calls.as_deref().unwrap()).unwrap();
    let expected_calls: Vec<_> = calls
        .into_iter()
        .map(|(id, name, arguments)| {
            json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments}})
        })
        .collect();
    assert_eq!(stored_calls, json!(expected_calls));
}

async fn check_tool_frames(dialect: &'static str, frames: ToolFrames, ending: Ending) {
    let (release, gate) = watch::channel(0);
    let f = Owned::start(dialect, ending, "stop", Some((frames, gate))).await;
    let complete = matches!(ending, Ending::Complete);
    let result = std::panic::AssertUnwindSafe(verify_tool_responses(&f, release, frames, complete))
        .catch_unwind()
        .await;
    f.finish(result).await;
}

#[tokio::test]
async fn gemini_tool_calls_stream_to_responses_before_terminal_and_survive_eof() {
    for ending in [Ending::Complete, Ending::MissingTerminal] {
        check_tool_frames("gemini", ToolFrames::SameFrame, ending).await;
    }
}

#[tokio::test]
async fn gemini_tool_calls_keep_distinct_identity_across_sse_frames() {
    for (frames, ending) in [
        (ToolFrames::Split, Ending::Complete),
        (ToolFrames::SplitWithoutIds, Ending::MissingTerminal),
        (
            ToolFrames::SplitWithUndelimitedTail,
            Ending::MissingTerminal,
        ),
    ] {
        check_tool_frames("gemini", frames, ending).await;
    }
}

#[tokio::test]
async fn ollama_tool_calls_keep_distinct_identity_across_ndjson_records() {
    check_tool_frames("ollama", ToolFrames::SplitWithoutIds, Ending::Complete).await;
}

#[tokio::test]
async fn ollama_tool_calls_keep_identity_in_a_complete_undelimited_record_at_eof() {
    check_tool_frames(
        "ollama",
        ToolFrames::SplitWithUndelimitedTail,
        Ending::MissingTerminal,
    )
    .await;
}

#[tokio::test]
async fn ollama_truncated_tool_record_does_not_invent_a_second_call_or_completion() {
    check_tool_frames(
        "ollama",
        ToolFrames::SplitWithTruncatedRecord,
        Ending::MissingTerminal,
    )
    .await;
}

// R104's two client contracts on the clean Owned Gateway/SQLite fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnthropicEmptyFlow {
    Complete,
    BeforeBlockStop,
    AfterBlockStop,
}

const ANTHROPIC_CHECKPOINT: &str = "owned empty tool checkpoint";
const ANTHROPIC_LOOKUP_FIRST: &str = r#"{"city":""#;
const ANTHROPIC_LOOKUP_LAST: &str = r#"Paris"}"#;

fn anthropic_empty_tool_wire() -> (String, String, String, String) {
    let start = |index, block| {
        event(
            "content_block_start",
            json!({"type":"content_block_start","index":index,"content_block":block}),
        )
    };
    let delta = |index, delta| {
        event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":index,"delta":delta}),
        )
    };
    let stop = |index| {
        event(
            "content_block_stop",
            json!({"type":"content_block_stop","index":index}),
        )
    };
    let prefix = event(
        "message_start",
        json!({"type":"message_start","message":{"id":"owned-empty-tool","type":"message","role":"assistant","content":[],"model":"owned-physical","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"output_tokens":0}}}),
    ) + &start(0, json!({"type":"text","text":""}))
        + &delta(0, json!({"type":"text_delta","text":ANTHROPIC_CHECKPOINT}))
        + &stop(0)
        + &start(
            1,
            json!({"type":"tool_use","id":"owned-lookup","name":"lookup","input":{}}),
        )
        + &delta(
            1,
            json!({"type":"input_json_delta","partial_json":ANTHROPIC_LOOKUP_FIRST}),
        )
        + &delta(
            1,
            json!({"type":"input_json_delta","partial_json":ANTHROPIC_LOOKUP_LAST}),
        )
        + &stop(1)
        + &start(
            2,
            json!({"type":"tool_use","id":"owned-zero-args","name":"clock","input":{}}),
        );
    (
        prefix,
        stop(2),
        event(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":2}}),
        ),
        event("message_stop", json!({"type":"message_stop"})),
    )
}

fn truncated_anthropic_frame(frame: &str) -> &str {
    let start = frame.find("data: {").expect("fixture has a JSON payload");
    &frame[..start + "data: {".len()]
}

#[tokio::test]
async fn n4_anthropic_empty_tool_chat_complete() {
    check_anthropic_empty_tool(false, AnthropicEmptyFlow::Complete).await;
}

#[tokio::test]
async fn n4_anthropic_empty_tool_chat_before_stop() {
    check_anthropic_empty_tool(false, AnthropicEmptyFlow::BeforeBlockStop).await;
}

#[tokio::test]
async fn n4_anthropic_empty_tool_chat_after_stop() {
    check_anthropic_empty_tool(false, AnthropicEmptyFlow::AfterBlockStop).await;
}

#[tokio::test]
async fn n4_anthropic_empty_tool_gemini_complete() {
    check_anthropic_empty_tool(true, AnthropicEmptyFlow::Complete).await;
}

#[tokio::test]
async fn n4_anthropic_empty_tool_gemini_before_stop() {
    check_anthropic_empty_tool(true, AnthropicEmptyFlow::BeforeBlockStop).await;
}

#[tokio::test]
async fn n4_anthropic_empty_tool_gemini_after_stop() {
    check_anthropic_empty_tool(true, AnthropicEmptyFlow::AfterBlockStop).await;
}

async fn check_anthropic_empty_tool(gemini: bool, flow: AnthropicEmptyFlow) {
    let complete = flow == AnthropicEmptyFlow::Complete;
    let ending = if complete {
        Ending::Complete
    } else {
        Ending::MissingTerminal
    };
    let (release, gate) = watch::channel(0);
    let owned = Owned::start(
        "anthropic",
        ending,
        "tool_use",
        Some((ToolFrames::AnthropicEmpty(flow), gate)),
    )
    .await;
    let result =
        std::panic::AssertUnwindSafe(verify_anthropic_empty_tool(&owned, release, gemini, flow))
            .catch_unwind()
            .await;
    owned.finish(result).await;
}

async fn verify_anthropic_empty_tool(
    owned: &Owned,
    release: watch::Sender<u8>,
    gemini: bool,
    flow: AnthropicEmptyFlow,
) {
    const SESSION: &str = "owned-anthropic-empty-tool";
    const PROMPT: &str = "owned empty argument call";
    let complete = flow == AnthropicEmptyFlow::Complete;
    let empty = if flow == AnthropicEmptyFlow::BeforeBlockStop {
        ""
    } else {
        "{}"
    };
    let definitions = [
        json!({"name":"lookup","parameters":{"type":"object","properties":{"city":{"type":"string"}}}}),
        json!({"name":"clock","parameters":{"type":"object","properties":{}}}),
    ];
    let (path, request) = if gemini {
        (
            "/v1beta/models/owned-model:streamGenerateContent?alt=sse",
            json!({
                "contents":[{"role":"user","parts":[{"text":PROMPT}]}],
                "tools":[{"functionDeclarations":definitions}]
            }),
        )
    } else {
        (
            "/v1/chat/completions",
            json!({
                "model":"owned-model","messages":[{"role":"user","content":PROMPT}],
                "stream":true,"max_tokens":32,
                "tools":definitions.map(|function| json!({"type":"function","function":function}))
            }),
        )
    };
    let mut response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
        .post(format!("{}{path}", owned.base))
        .bearer_auth("owned-n4-key")
        .header("x-session-id", SESSION)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let trace = response.headers()["x-trace-id"]
        .to_str()
        .unwrap()
        .to_owned();

    // Observe the stream before releasing the final upstream fragment.
    let marker: &[u8] = if gemini {
        ANTHROPIC_CHECKPOINT.as_bytes()
    } else {
        b"owned-zero-args"
    };
    let mut wire = Vec::new();
    loop {
        wire.extend(
            response
                .chunk()
                .await
                .unwrap()
                .expect("stream before release"),
        );
        if wire.ends_with(b"\n\n") && wire.windows(marker.len()).any(|bytes| bytes == marker) {
            break;
        }
    }
    let observed = std::str::from_utf8(&wire).unwrap();
    if gemini {
        assert!(!observed.contains("functionCall"));
    } else if flow != AnthropicEmptyFlow::AfterBlockStop {
        assert!(!observed.contains(r#""arguments":"{}""#));
    }
    release.send(1).unwrap();
    while let Some(chunk) = response.chunk().await.unwrap() {
        wire.extend(chunk);
    }

    let data: Vec<_> = std::str::from_utf8(&wire)
        .unwrap()
        .strip_suffix("\n\n")
        .expect("complete downstream SSE frames")
        .split("\n\n")
        .filter(|frame| !frame.starts_with(':'))
        .map(|frame| {
            frame
                .strip_prefix("event: error\n")
                .unwrap_or(frame)
                .strip_prefix("data: ")
                .expect("downstream SSE data")
        })
        .collect();
    let events: Vec<Value> = data
        .iter()
        .filter(|&&data| data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect();
    let errors = events
        .iter()
        .filter(|frame| frame.get("error").is_some())
        .count();
    assert_eq!(errors, usize::from(!complete));
    let text = if gemini {
        "/candidates/0/content/parts/0/text"
    } else {
        "/choices/0/delta/content"
    };
    assert_eq!(
        events
            .iter()
            .filter_map(|event| event.pointer(text)?.as_str())
            .collect::<Vec<_>>(),
        [ANTHROPIC_CHECKPOINT]
    );
    verify_anthropic_empty_tool_output(gemini, complete, empty, &data, &events);
    if !complete {
        assert!(events.last().unwrap()["error"]["message"].is_string());
    }

    let stopped = owned.service.set_enabled(false).await.unwrap();
    assert!(!stopped.running && stopped.last_error.is_none());
    {
        let calls = owned.scenario.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "no duplicate POST or backup dispatch");
        let (method, path, body) = &calls[0];
        assert_eq!((method.as_str(), path.as_str()), ("POST", "/v1/messages"));
        assert_eq!(body["model"], "owned-physical");
        assert_eq!(body["stream"], true);
        let names: Vec<_> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["lookup", "clock"]);
    }
    let rows =
        sqlx::query("SELECT trace_id,status,error,fallback_attempts,attempts_json FROM requests")
            .fetch_all(owned.state.db.pool())
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let status = if complete { 200 } else { 502 };
    assert_eq!(row.get::<String, _>("trace_id"), trace);
    assert_eq!(row.get::<i64, _>("status"), status);
    assert_eq!(
        row.get::<Option<String>, _>("error").as_deref(),
        (!complete).then_some("upstream_5xx")
    );
    assert_eq!(row.get::<i64, _>("fallback_attempts"), 0);
    let attempts: Value = serde_json::from_str(&row.get::<String, _>("attempts_json")).unwrap();
    assert_eq!(attempts.as_array().unwrap().len(), 1);
    assert_eq!(attempts[0]["provider_id"], "primary");
    assert_eq!(attempts[0]["ok"], complete);

    let stored = repo::recent_messages(owned.state.db.pool(), SESSION, 10)
        .await
        .unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(
        (stored[0].role.as_str(), stored[0].content.as_str()),
        ("user", PROMPT)
    );
    assert_eq!(
        (stored[1].role.as_str(), stored[1].content.as_str()),
        ("assistant", ANTHROPIC_CHECKPOINT)
    );
    let saved: Value = serde_json::from_str(stored[1].tool_calls.as_deref().unwrap()).unwrap();
    assert_eq!(
        saved,
        json!([
            {"id":"owned-lookup","type":"function","function":{"name":"lookup","arguments":format!("{ANTHROPIC_LOOKUP_FIRST}{ANTHROPIC_LOOKUP_LAST}")}},
            {"id":"owned-zero-args","type":"function","function":{"name":"clock","arguments":empty}}
        ]),
    );
}

fn verify_anthropic_empty_tool_output(
    gemini: bool,
    complete: bool,
    empty: &str,
    data: &[&str],
    events: &[Value],
) {
    let last = events.last().unwrap();
    let done = data.iter().filter(|&&data| data == "[DONE]").count();
    if gemini {
        assert_eq!(done, 0);
        let finishes: Vec<_> = events
            .iter()
            .filter_map(|frame| frame["candidates"][0]["finishReason"].as_str())
            .collect();
        let calls: Vec<_> = events
            .iter()
            .filter_map(|frame| frame.pointer("/candidates/0/content/parts")?.as_array())
            .flatten()
            .filter_map(|part| part.get("functionCall"))
            .cloned()
            .collect();
        if complete {
            assert_eq!(finishes, ["STOP"]);
            assert_eq!(last["candidates"][0]["finishReason"], "STOP");
            assert_eq!(
                calls,
                [
                    json!({"id":"owned-lookup","name":"lookup","args":{"city":"Paris"}}),
                    json!({"id":"owned-zero-args","name":"clock","args":{}})
                ]
            );
        } else {
            assert!(finishes.is_empty());
            assert!(calls.is_empty(), "Gemini tools require message completion");
        }
    } else {
        assert_eq!(done, usize::from(complete));
        let calls: Vec<_> = events
            .iter()
            .filter_map(|frame| frame.pointer("/choices/0/delta/tool_calls")?.as_array())
            .flatten()
            .cloned()
            .collect();
        let mut expected = vec![
            json!({"index":1,"id":"owned-lookup","type":"function","function":{"name":"lookup","arguments":""}}),
            json!({"index":1,"function":{"arguments":ANTHROPIC_LOOKUP_FIRST}}),
            json!({"index":1,"function":{"arguments":ANTHROPIC_LOOKUP_LAST}}),
            json!({"index":2,"id":"owned-zero-args","type":"function","function":{"name":"clock","arguments":""}}),
        ];
        if !empty.is_empty() {
            expected.push(json!({"index":2,"function":{"arguments":"{}"}}));
        }
        assert_eq!(calls, expected);
        let finishes: Vec<_> = events
            .iter()
            .filter_map(|frame| frame["choices"][0]["finish_reason"].as_str())
            .collect();
        if complete {
            assert_eq!(finishes, ["tool_calls"]);
            assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
            assert_eq!(data.last(), Some(&"[DONE]"));
        } else {
            assert!(finishes.is_empty());
        }
    }
}
