use super::*;
use crate::{
    config::{SearchBackendKind, SearchConfig},
    domain::{Message, Role},
    search::{executor, resources::SearchResources, SearchQuery},
};
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{Request, Response, StatusCode},
    routing::post,
    Router,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct OwnedState {
    seen: Arc<Mutex<Vec<Value>>>,
    fault: &'static str,
    step: usize,
    release: Arc<tokio::sync::Notify>,
    seen_ready: Arc<tokio::sync::Notify>,
}
struct Owned {
    url: String,
    state: OwnedState,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.state.release.notify_waiters();
        self.task.abort();
    }
}
async fn serve(fault: &'static str, step: usize) -> Owned {
    let state = OwnedState {
        seen: Arc::new(Mutex::new(vec![])),
        fault,
        step,
        release: Arc::new(tokio::sync::Notify::new()),
        seen_ready: Arc::new(tokio::sync::Notify::new()),
    };
    let app = Router::new()
        .route("/", post(handle))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Owned { url, state, task }
}
async fn handle(State(state): State<OwnedState>, request: Request<Body>) -> Response<Body> {
    let headers = request.headers().clone();
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let bytes = to_bytes(request.into_body(), 8192).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let ordinal = {
        let mut seen = state.seen.lock().unwrap();
        let n = seen.len() + 1;
        seen.push(json!({"method":method,"uri":uri,"body":body,"content_type_count":headers.get_all("content-type").iter().count(),"authorization":headers.contains_key("authorization"),"cookie":headers.contains_key("cookie"),"referer":headers.contains_key("referer"),"opt_out":headers.get("x-moz-1st-party-data-opt-out").and_then(|v|v.to_str().ok()),"protocol":headers.get("mcp-protocol-version").and_then(|v|v.to_str().ok()),"accept":headers.get("accept").and_then(|v|v.to_str().ok())}));
        n
    };
    state.seen_ready.notify_one();
    if ordinal == state.step {
        match state.fault {
            "redirect" => {
                return Response::builder()
                    .status(302)
                    .header("location", "/redirected")
                    .body(Body::empty())
                    .unwrap()
            }
            "sse" => {
                return Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(Body::from("event: message\ndata: synthetic\n\n"))
                    .unwrap()
            }
            "session" => {
                return Response::builder()
                    .status(200)
                    .header("mcp-session-id", "synthetic-untrusted-session")
                    .body(Body::empty())
                    .unwrap()
            }
            "429" => {
                return Response::builder()
                    .status(429)
                    .header("retry-after", "120")
                    .body(Body::from("never-echo-this-private-error"))
                    .unwrap()
            }
            "500" => {
                return Response::builder()
                    .status(500)
                    .body(Body::from("never-echo-this-private-error"))
                    .unwrap()
            }
            "wrong_notify" => return Response::builder().status(200).body(Body::empty()).unwrap(),
            "oversize" => {
                return Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        " ".repeat(super::super::mdn_mcp_contract::MAX_REPLY_BYTES + 1),
                    ))
                    .unwrap()
            }
            "charset" => {
                return Response::builder()
                    .status(200)
                    .header("content-type", "application/json; charset=shift_jis")
                    .body(Body::from("{}"))
                    .unwrap()
            }
            "stall" => state.release.notified().await,
            _ => {}
        }
    }
    let payload = if body["method"] == "initialize" {
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":super::super::mdn_mcp_contract::PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"synthetic-mdn","version":"fixture"}}})
    } else if body["method"] == "notifications/initialized" {
        return Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .unwrap();
    } else {
        json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"# Synthetic MDN text\n`path`: `/not-verified`\n\nopaque untrusted reference"}]}})
    };
    Response::builder()
        .header("content-type", "application/json; charset=utf-8")
        .body(Body::from(payload.to_string()))
        .unwrap()
}
fn config() -> SearchConfig {
    SearchConfig {
        enabled: true,
        backend: SearchBackendKind::MdnMcp,
        timeout_ms: 3000,
        ..Default::default()
    }
}
async fn resources(cfg: &SearchConfig) -> SearchResources {
    SearchResources::with_database(
        cfg.clone(),
        crate::db::Db::connect_in_memory().await.unwrap(),
    )
}
fn query() -> SearchQuery {
    SearchQuery {
        text: "Array map public synthetic".into(),
        max_results: 3,
    }
}

#[tokio::test]
async fn fixed_three_post_handshake_preserves_opaque_result_without_forwarding_credentials() {
    let s = serve("none", 0).await;
    let cfg = config();
    let r = resources(&cfg).await;
    let out = OWNED_ENDPOINT
        .scope(s.url.clone(), execute(&cfg, &query(), &r))
        .await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(out.results.is_empty());
    assert_eq!(out.hits, 0);
    let dto = serde_json::to_value(&out).unwrap();
    assert!(dto["unstructured_reference"]["document_count"].is_null());
    assert!(dto["unstructured_reference"]["text"]
        .as_str()
        .unwrap()
        .contains("opaque"));
    let seen = s.state.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0]["body"]["method"], "initialize");
    assert_eq!(seen[1]["body"]["method"], "notifications/initialized");
    assert_eq!(
        seen[2]["body"]["params"],
        json!({"name":"search","arguments":{"query":query().text}})
    );
    for row in seen.iter() {
        assert_eq!(row["content_type_count"], 1);
        assert_eq!(row["method"], "POST");
        assert_eq!(row["uri"], "/");
        assert_eq!(row["authorization"], false);
        assert_eq!(row["cookie"], false);
        assert_eq!(row["referer"], false);
        assert_eq!(row["opt_out"], "1");
        assert_eq!(row["protocol"], "2025-03-26");
        assert_eq!(row["accept"], "application/json, text/event-stream");
    }
    assert!(!seen[0].to_string().contains("Array map public"));
    assert!(!seen[1].to_string().contains("Array map public"));
}

#[tokio::test]
async fn protocol_http_session_and_size_failures_stop_without_retry_or_raw_echo() {
    for (fault, step) in [
        ("redirect", 1),
        ("sse", 1),
        ("session", 1),
        ("charset", 1),
        ("wrong_notify", 2),
        ("500", 3),
        ("oversize", 3),
    ] {
        let s = serve(fault, step).await;
        let cfg = config();
        let r = resources(&cfg).await;
        let out = OWNED_ENDPOINT
            .scope(s.url.clone(), execute(&cfg, &query(), &r))
            .await;
        assert!(out.error.is_some(), "{fault}");
        assert!(!out.error.as_ref().unwrap().contains("never-echo"));
        assert!(out.unstructured_reference.is_none());
        assert!(out.results.is_empty());
        assert_eq!(
            s.state.seen.lock().unwrap().len(),
            step,
            "{fault} must stop at observed stage"
        );
    }
}

#[tokio::test]
async fn retry_after_is_committed_before_failure_and_reopen_blocks_new_network() {
    let s = serve("429", 3).await;
    let cfg = config();
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let r = SearchResources::with_database(cfg.clone(), db.clone());
    let out = OWNED_ENDPOINT
        .scope(s.url.clone(), execute(&cfg, &query(), &r))
        .await;
    assert!(out.error.as_deref().unwrap().contains("429"));
    assert_eq!(s.state.seen.lock().unwrap().len(), 3);
    r.drain_source_writes(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    let reopened = SearchResources::with_database(cfg.clone(), db);
    let next = OWNED_ENDPOINT
        .scope(s.url.clone(), execute(&cfg, &query(), &reopened))
        .await;
    assert!(next.error.as_deref().unwrap().contains("wait"));
    assert_eq!(s.state.seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn timeout_has_one_shared_deadline_no_second_search_and_no_implicit_retry() {
    let s = serve("stall", 3).await;
    let mut cfg = config();
    cfg.timeout_ms = 500;
    let r = resources(&cfg).await;
    let started = std::time::Instant::now();
    let out = OWNED_ENDPOINT
        .scope(s.url.clone(), execute(&cfg, &query(), &r))
        .await;
    assert!(out.error.as_deref().unwrap().contains("timeout"));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(s.state.seen.lock().unwrap().len(), 3);
    assert!(out.unstructured_reference.is_none());
}

#[tokio::test]
async fn storage_config_and_query_guards_reject_before_any_http() {
    let s = serve("none", 0).await;
    let cfg = config();
    let no_db = SearchResources::new(cfg.clone());
    let out = OWNED_ENDPOINT
        .scope(s.url.clone(), execute(&cfg, &query(), &no_db))
        .await;
    assert!(out.error.as_deref().unwrap().contains("storage"));
    let mut changed = cfg.clone();
    changed.timeout_ms = 1000;
    let stale = resources(&changed).await;
    let out = OWNED_ENDPOINT
        .scope(s.url.clone(), execute(&cfg, &query(), &stale))
        .await;
    assert!(out.error.as_deref().unwrap().contains("configuration"));
    let r = resources(&cfg).await;
    let invalid = SearchQuery {
        text: "x".repeat(257),
        max_results: 3,
    };
    let out = OWNED_ENDPOINT
        .scope(s.url.clone(), execute(&cfg, &invalid, &r))
        .await;
    assert!(out.error.is_some());
    assert!(s.state.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn selected_mdn_producer_reaches_existing_prefetch_and_ignores_caller_auth_client() {
    let s = serve("none", 0).await;
    let cfg = config();
    let r = resources(&cfg).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("authorization", "synthetic-no-forward".parse().unwrap());
    headers.insert("cookie", "synthetic-no-forward".parse().unwrap());
    let poison = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let mut history = vec![
        Message::system("trusted"),
        Message::user("Array map public synthetic"),
    ];
    let out = OWNED_ENDPOINT
        .scope(
            s.url.clone(),
            executor::prefetch_with_resources(
                &poison,
                &cfg,
                Some("synthetic-no-forward"),
                &mut history,
                "Array map public synthetic",
                &r,
            ),
        )
        .await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(history.len(), 3);
    assert_eq!(history[1].role, Role::User);
    assert!(history[1].tool_calls.is_none());
    assert!(history[1].content_text().contains("不可信引用"));
    assert_eq!(history[2].content_text(), "Array map public synthetic");
    assert_eq!(s.state.seen.lock().unwrap().len(), 3);
    assert!(s
        .state
        .seen
        .lock()
        .unwrap()
        .iter()
        .all(|v| v["authorization"] == false && v["cookie"] == false));
}

#[test]
fn experimental_backend_is_explicit_and_preserves_legacy_default_configuration() {
    let old = SearchConfig::default();
    assert_eq!(old.backend, SearchBackendKind::DuckDuckGo);
    assert!(!old.enabled);
    let cfg = config();
    assert_eq!(serde_json::to_value(cfg.backend).unwrap(), "mdn_mcp");
    assert!(crate::search::validate(&cfg).is_ok());
    assert_eq!(crate::search::backend_code(cfg.backend), "mdn_mcp");
}

#[tokio::test]
async fn review_mdn_new_service_wait_blocks_an_already_admitted_transaction_next_post() {
    let b = serve("stall", 1).await;
    let a = serve("429", 1).await;
    let cfg = config();
    let r = resources(&cfg).await;
    let b_query = query();
    let b_call = OWNED_ENDPOINT.scope(b.url.clone(), execute(&cfg, &b_query, &r));
    let intervene = async {
        loop {
            let observed = b.state.seen_ready.notified();
            if !b.state.seen.lock().unwrap().is_empty() {
                break;
            }
            observed.await;
        }
        let result = OWNED_ENDPOINT
            .scope(a.url.clone(), execute(&cfg, &query(), &r))
            .await;
        assert!(result.error.as_deref().unwrap().contains("429"));
        r.drain_source_writes(std::time::Duration::from_secs(2))
            .await
            .unwrap();
        b.state.release.notify_one();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(b_call, intervene)
    })
    .await
    .unwrap();
    assert!(
        result.error.as_deref().is_some_and(|v| v.contains("wait")),
        "already-admitted B must not send after A's acknowledged service wait: {:?}",
        result.error
    );
    assert_eq!(
        b.state.seen.lock().unwrap().len(),
        1,
        "B must not send initialized/search"
    );
    assert_eq!(a.state.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn review_mdn_own_long_local_reservation_does_not_block_its_protocol_stages() {
    let s = serve("none", 0).await;
    let mut cfg = config();
    cfg.source_min_interval_ms = 60_000;
    let r = resources(&cfg).await;
    let out = OWNED_ENDPOINT
        .scope(s.url.clone(), execute(&cfg, &query(), &r))
        .await;
    assert!(
        out.error.is_none(),
        "own spacing must not block own three stages: {:?}",
        out.error
    );
    assert_eq!(s.state.seen.lock().unwrap().len(), 3);
    let next = OWNED_ENDPOINT
        .scope(s.url.clone(), execute(&cfg, &query(), &r))
        .await;
    assert!(next.error.as_deref().unwrap().contains("wait"));
    assert_eq!(s.state.seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn review_mdn_owned_profile_rejects_nonloopback_or_decorated_test_targets_without_io() {
    for url in [
        "https://mcp.mdn.mozilla.net/",
        "http://example.invalid/",
        "http://user@127.0.0.1:1234/",
        "http://127.0.0.1:1234/path",
        "http://127.0.0.1:1234/?q=x",
        "http://127.0.0.1:1234/#fragment",
    ] {
        OWNED_ENDPOINT
            .scope(url.to_owned(), async {
                assert!(
                    endpoint().is_err(),
                    "only the strict owned loopback profile is permitted"
                );
            })
            .await;
    }
    OWNED_ENDPOINT
        .scope("http://127.0.0.1:1234/".to_owned(), async {
            assert!(endpoint().is_ok());
        })
        .await;
}
