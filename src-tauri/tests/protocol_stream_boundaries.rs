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
use std::sync::{Arc, Mutex};

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
                value += &format!(
                    "data: {}\n\n",
                    json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}})
                );
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
        .body(Body::from(wire(&s)))
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
        let scenario = Arc::new(Scenario {
            dialect,
            ending,
            reason,
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
            let p:Provider=serde_json::from_value(json!({"id":id,"name":id,"base_url":format!("{origin}{suffix}"),"api_key_enc":"","dialect":dialect,"enabled":true,"priority":priority,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"owned-model","upstream":physical,"enabled":true,"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","price":null}]})).unwrap();
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
