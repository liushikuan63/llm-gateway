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
    gemini_tools: Option<(GeminiToolFrames, watch::Receiver<bool>)>,
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
enum GeminiToolFrames {
    SameFrame,
    Split,
    SplitWithoutIds,
    SplitWithUndelimitedTail,
}

type GeminiCall = (&'static str, &'static str, &'static str);

const GEMINI_CALLS: [GeminiCall; 2] = [
    ("owned-weather", "lookup_weather", r#"{"city":"Paris"}"#),
    ("owned-clock", "lookup_clock", r#"{"utc_offset":2}"#),
];

impl GeminiToolFrames {
    fn expected_calls(self) -> [GeminiCall; 2] {
        if self == Self::SplitWithoutIds {
            [
                ("call_0", GEMINI_CALLS[0].1, GEMINI_CALLS[0].2),
                ("call_1", GEMINI_CALLS[1].1, GEMINI_CALLS[1].2),
            ]
        } else {
            GEMINI_CALLS
        }
    }
}

fn gemini_calls_wire(calls: &[GeminiCall], include_ids: bool) -> String {
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
                value += &format!(
                    "{}\n",
                    json!({"model":"owned-physical","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":3,"eval_count":2})
                );
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
    let body = if let Some((frames, mut gate)) = s.gemini_tools.clone() {
        let complete = matches!(s.ending, Ending::Complete);
        Body::from_stream(async_stream::stream! {
            let include_ids = frames != GeminiToolFrames::SplitWithoutIds;
            if frames == GeminiToolFrames::SameFrame {
                yield Ok::<_, Infallible>(gemini_calls_wire(&GEMINI_CALLS, include_ids) + "\n\n");
            } else {
                yield Ok(gemini_calls_wire(&GEMINI_CALLS[..1], include_ids) + "\n\n");
                let second = gemini_calls_wire(&GEMINI_CALLS[1..], include_ids);
                if frames == GeminiToolFrames::SplitWithUndelimitedTail {
                    // No delimiter or protocol terminal. Only HTTP EOF exposes this complete JSON.
                    if gate.wait_for(|released| *released).await.is_ok() {
                        yield Ok(second);
                    }
                    return;
                }
                yield Ok(second + "\n\n");
            }
            // Release the terminal (or EOF) only after the client saw both calls.
            if gate.wait_for(|released| *released).await.is_ok() && complete {
                yield Ok(gemini_stop_wire());
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
        gemini_tools: Option<(GeminiToolFrames, watch::Receiver<bool>)>,
    ) -> Self {
        let supports_tools = gemini_tools.is_some();
        let scenario = Arc::new(Scenario {
            dialect,
            ending,
            reason,
            gemini_tools,
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

async fn verify_gemini_tool_responses(
    f: &Owned,
    release: watch::Sender<bool>,
    frames: GeminiToolFrames,
    complete: bool,
) {
    const SESSION: &str = "owned-n4-gemini-tools";
    const INPUT: &str = "owned tool request";
    let calls = frames.expected_calls();
    let tools = calls.map(
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
    for (index, (call_id, name, arguments)) in calls.into_iter().enumerate() {
        let added = next_responses_event(&mut response, &mut pending)
            .await
            .unwrap();
        assert_eq!(
            added["type"], "response.output_item.added",
            "{frames:?} call {index} must open a separate Responses item"
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
        if index == 0 && frames == GeminiToolFrames::SplitWithUndelimitedTail {
            release.send(true).unwrap();
        }
    }
    assert_ne!(expected_output[0]["id"], expected_output[1]["id"]);
    assert_ne!(expected_output[0]["call_id"], expected_output[1]["call_id"]);
    if frames != GeminiToolFrames::SplitWithUndelimitedTail {
        release.send(true).unwrap();
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
        assert_eq!(
            path,
            "/v1beta/models/owned-physical:streamGenerateContent?alt=sse"
        );
        assert_eq!(request["contents"][0]["parts"][0]["text"], INPUT);
        let declarations = request["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap();
        assert_eq!(declarations.len(), calls.len());
        for (declaration, (_, name, _)) in declarations.iter().zip(calls) {
            assert_eq!(declaration["name"], name);
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
    let expected_calls = calls.map(|(id, name, arguments)| {
        json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments}})
    });
    assert_eq!(stored_calls, json!(expected_calls));
}

async fn check_gemini_tool_frames(frames: GeminiToolFrames, ending: Ending) {
    let (release, gate) = watch::channel(false);
    let f = Owned::start("gemini", ending, "stop", Some((frames, gate))).await;
    let complete = matches!(ending, Ending::Complete);
    let result =
        std::panic::AssertUnwindSafe(verify_gemini_tool_responses(&f, release, frames, complete))
            .catch_unwind()
            .await;
    f.finish(result).await;
}

#[tokio::test]
async fn gemini_tool_calls_stream_to_responses_before_terminal_and_survive_eof() {
    for ending in [Ending::Complete, Ending::MissingTerminal] {
        check_gemini_tool_frames(GeminiToolFrames::SameFrame, ending).await;
    }
}

#[tokio::test]
async fn gemini_tool_calls_keep_distinct_identity_across_sse_frames() {
    for (frames, ending) in [
        (GeminiToolFrames::Split, Ending::Complete),
        (GeminiToolFrames::SplitWithoutIds, Ending::MissingTerminal),
        (
            GeminiToolFrames::SplitWithUndelimitedTail,
            Ending::MissingTerminal,
        ),
    ] {
        check_gemini_tool_frames(frames, ending).await;
    }
}
