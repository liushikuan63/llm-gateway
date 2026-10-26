//! Real loopback + production gateway audit expectations; no timing test IPC.
use super::*;

async fn wait_attempts(db: &db::Db, count: usize) -> serde_json::Value {
    for _ in 0..80 {
        let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
        if let Some(row) = rows
            .into_iter()
            .find(|v| v["attempts"].as_array().is_some_and(|a| a.len() == count))
        {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("production audit did not persist expected attempt count");
}
fn timing(attempt: &serde_json::Value, mode: &str, terminal: &str) -> serde_json::Value {
    let t = attempt
        .get("timing")
        .expect("actual HTTP attempt must carry measured timing");
    assert_eq!(t["schema_version"], 2);
    assert_eq!(t["mode"], mode);
    assert_eq!(t["terminal"], terminal);
    assert!(t["response_headers_ms"].as_u64().is_some());
    assert!(t["finished_ms"].as_u64().unwrap() >= t["response_headers_ms"].as_u64().unwrap());
    for name in ["queue_ms", "provider_compute_ms", "tool_wait_ms"] {
        assert!(t.get(name).is_none());
    }
    t.clone()
}
#[tokio::test]
async fn s2_buffered_failover_records_each_actual_http_attempt_without_inventing_ttft() {
    let (bad_url, bad_state, bad) =
        spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::TOO_MANY_REQUESTS)).await;
    let (good_url, good_state, good) =
        spawn_openai_upstream(OpenAiBehavior::Complete("buffered observation reply")).await;
    let (db, cfg, gateway, base) = spawn_gateway(vec![
        provider("timed-bad", bad_url, Dialect::OpenAI, 1),
        provider("timed-good", good_url, Dialect::OpenAI, 2),
    ])
    .await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&cfg.unified_key)
        .json(&openai_body("integration-model"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("buffered observation reply"));
    let row = wait_attempts(&db, 2).await;
    gateway.abort();
    bad.abort();
    good.abort();
    assert_eq!(bad_state.hits.load(Ordering::SeqCst), 1);
    assert_eq!(good_state.hits.load(Ordering::SeqCst), 1);
    let attempts = row["attempts"].as_array().unwrap();
    assert_eq!(attempts[0]["provider_id"], "timed-bad");
    assert_eq!(attempts[1]["provider_id"], "timed-good");
    for (index, terminal) in [(0, "failed"), (1, "completed")] {
        let t = timing(&attempts[index], "buffered", terminal);
        assert_eq!(t["first_text_ms"], serde_json::Value::Null);
        assert_eq!(t["first_reasoning_ms"], serde_json::Value::Null);
    }
}
#[tokio::test]
async fn s2_streaming_real_semantic_content_and_completion_reach_persisted_attempt() {
    let (url, state, upstream) =
        spawn_openai_upstream(OpenAiBehavior::Complete("streamed observation reply")).await;
    let (db, cfg, gateway, base) =
        spawn_gateway(vec![provider("timed-stream", url, Dialect::OpenAI, 1)]).await;
    let mut body = openai_body("integration-model");
    body["stream"] = serde_json::json!(true);
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&cfg.unified_key)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    assert!(text.contains("streamed observation reply"));
    assert!(text.contains("[DONE]"));
    let row = wait_attempts(&db, 1).await;
    gateway.abort();
    upstream.abort();
    assert_eq!(state.hits.load(Ordering::SeqCst), 1);
    let t = timing(&row["attempts"][0], "streaming", "completed");
    assert!(t["first_text_ms"].as_u64().unwrap() >= t["response_headers_ms"].as_u64().unwrap());
    assert!(t["finished_ms"].as_u64().unwrap() >= t["first_text_ms"].as_u64().unwrap());
    assert_eq!(t["first_reasoning_ms"], serde_json::Value::Null);
    assert_eq!(t["first_refusal_ms"], serde_json::Value::Null);
    assert_eq!(t["first_tool_ms"], serde_json::Value::Null);
    assert!(!t.to_string().contains("streamed observation reply"));
}

#[tokio::test]
async fn s2_raw_http_reasoning_and_refusal_keep_distinct_observations_without_text_ttft() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let mock=Router::new().route("/v1/chat/completions",post(move||{let hits=counter.clone();async move{
        hits.fetch_add(1,Ordering::SeqCst);
        let frames=[serde_json::json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":"synthetic observed reasoning"}}]}),serde_json::json!({"choices":[{"index":0,"delta":{"refusal":"synthetic observed refusal"}}]}),serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}})];
        let mut body=frames.iter().map(|v|format!("data: {v}\n\n")).collect::<String>();body.push_str("data: [DONE]\n\n");
        Response::builder().header(CONTENT_TYPE,"text/event-stream").body(axum::body::Body::from(body)).unwrap()
    }}));
    let source = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let (db, cfg, gateway, base) = spawn_gateway(vec![provider(
        "timed-semantic",
        format!("http://{address}/v1"),
        Dialect::OpenAI,
        1,
    )])
    .await;
    let mut body = openai_body("integration-model");
    body["stream"] = serde_json::json!(true);
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&cfg.unified_key)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    let row = wait_attempts(&db, 1).await;
    gateway.abort();
    source.abort();
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(
        text.contains("synthetic observed reasoning"),
        "raw reasoning event must survive the production decoder"
    );
    assert!(
        text.contains("synthetic observed refusal"),
        "raw refusal event must survive the production decoder"
    );
    let t = timing(&row["attempts"][0], "streaming", "completed");
    assert_eq!(t["first_text_ms"], serde_json::Value::Null);
    assert!(t["first_reasoning_ms"].as_u64().is_some());
    assert!(t["first_refusal_ms"].as_u64().is_some());
    assert!(!t.to_string().contains("synthetic observed"));
}

#[tokio::test]
async fn s2_client_disconnect_persists_one_cancelled_audit_without_retry_or_false_cost() {
    use futures_util::{FutureExt, StreamExt};
    use std::sync::atomic::AtomicBool;
    struct BodyDrop(Arc<AtomicBool>);
    impl Drop for BodyDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let source_dropped = Arc::new(AtomicBool::new(false));
    let counter = hits.clone();
    let dropped = source_dropped.clone();
    let mock=Router::new().route("/v1/chat/completions",post(move||{let hits=counter.clone();let dropped=dropped.clone();async move{
        hits.fetch_add(1,Ordering::SeqCst);
        let guard=BodyDrop(dropped);
        let body=axum::body::Body::from_stream(async_stream::stream! {
            let _guard=guard;
            yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from_static(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"owned partial\"}}]}\n\n"));
            std::future::pending::<()>().await;
        });
        Response::builder().header(CONTENT_TYPE,"text/event-stream").body(body).unwrap()
    }}));
    let source = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let (db, cfg, gateway, base) = spawn_gateway(vec![provider(
        "timed-cancel",
        format!("http://{address}/v1"),
        Dialect::OpenAI,
        1,
    )])
    .await;
    let result = std::panic::AssertUnwindSafe(async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .pool_max_idle_per_host(0)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let mut body = openai_body("integration-model");
        body["stream"] = serde_json::json!(true);
        let response = client
            .post(format!("{base}/v1/chat/completions"))
            .header("connection", "close")
            .bearer_auth(&cfg.unified_key)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let trace = response
            .headers()
            .get("x-trace-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let mut stream = response.bytes_stream();
        let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&first).contains("owned partial"));
        drop(stream);
        drop(client);
        let row = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let rows = repo::recent_requests(db.pool(), 10).await.unwrap();
                if let Some(row) = rows.into_iter().next() {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("disconnect must persist a production cancellation audit");
        assert_eq!(row["status"], 499);
        assert_eq!(row["error"], "client_cancelled");
        let stored_trace: String =
            sqlx::query_scalar("SELECT trace_id FROM requests ORDER BY id DESC LIMIT 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(stored_trace, trace);
        assert_eq!(row["routed_provider"], "timed-cancel");
        assert!(row["cost"].is_null());
        let attempts = row["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["wire_modes"]["schema_version"], 1);
        assert_eq!(attempts[0]["wire_modes"]["caller"]["dialect"], "openai");
        assert_eq!(attempts[0]["wire_modes"]["wire"]["reasoning"], "absent");
        let t = timing(&attempts[0], "streaming", "cancelled");
        assert!(t["first_text_ms"].as_u64().is_some());
        assert!(!t.to_string().contains("owned partial"));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !source_dropped.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("upstream HTTP body must be dropped promptly");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(repo::recent_requests(db.pool(), 10).await.unwrap().len(), 1);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    })
    .catch_unwind()
    .await;
    gateway.abort();
    source.abort();
    let _ = gateway.await;
    let _ = source.await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn cancel_during_selection(headers_sent: bool, prior: Option<u16>, streaming: bool) {
    use futures_util::FutureExt;
    use std::sync::atomic::AtomicBool;
    use tokio::io::AsyncWriteExt;
    struct SourceDrop(Arc<AtomicBool>);
    impl Drop for SourceDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let arrived = Arc::new(tokio::sync::Notify::new());
    let source_dropped = Arc::new(AtomicBool::new(false));
    let counter = hits.clone();
    let signal = arrived.clone();
    let dropped = source_dropped.clone();
    let auth = prior == Some(401);
    let mock=Router::new().route("/v1/chat/completions",post(move||{let hits=counter.clone();let signal=signal.clone();let dropped=dropped.clone();async move{
        let index=hits.fetch_add(1,Ordering::SeqCst);
        if auth && index==0 {return (StatusCode::UNAUTHORIZED,"owned authentication rejection").into_response();}
        let guard=SourceDrop(dropped);
        if !headers_sent { let _guard=guard;signal.notify_one();std::future::pending::<Response>().await }
        else {let body=axum::body::Body::from_stream(async_stream::stream!{
            let _guard=guard;yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from_static(if streaming { b": owned keepalive\n\n" } else { b"{\"id\":\"owned-pending\"" }));signal.notify_one();std::future::pending::<()>().await;
        });Response::builder().header(CONTENT_TYPE,if streaming {"text/event-stream"} else {"application/json"}).body(body).unwrap()}
    }}));
    let source = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let (backup_url, backup_state, backup) =
        spawn_openai_upstream(OpenAiBehavior::Complete("must never be replayed")).await;
    let mut providers = vec![
        provider(
            "timed-select",
            format!("http://{address}/v1"),
            Dialect::OpenAI,
            1,
        ),
        provider("timed-trap", backup_url, Dialect::OpenAI, 2),
    ];
    let (prior_state, prior_task) = if prior == Some(429) {
        let (url, state, task) =
            spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::TOO_MANY_REQUESTS)).await;
        providers.insert(0, provider("timed-prior", url, Dialect::OpenAI, 0));
        (Some(state), Some(task))
    } else {
        (None, None)
    };
    let (db, cfg, gateway, base) = spawn_gateway_with(providers, |cfg| {
        if auth {
            cfg.auth_failure.mode = AuthFailureMode::SkipAndDisable;
            cfg.auth_failure.confirm_retries = 1;
            cfg.max_fallback_attempts = 3;
        }
    })
    .await;
    let result=std::panic::AssertUnwindSafe(async{
        let authority=base.strip_prefix("http://").unwrap();let mut socket=tokio::net::TcpStream::connect(authority).await.unwrap();
        let mut body=openai_body("integration-model");body["stream"]=serde_json::json!(streaming);let body=body.to_string();let trace="0123456789abcdef0123456789abcdef";
        let request=format!("POST /v1/chat/completions HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Trace-Id: {trace}\r\n\r\n{body}",cfg.unified_key,body.len());
        socket.write_all(request.as_bytes()).await.unwrap();tokio::time::timeout(Duration::from_secs(2),arrived.notified()).await.unwrap();
        if headers_sent { tokio::time::sleep(Duration::from_millis(50)).await; }
        drop(socket);
        let row=tokio::time::timeout(Duration::from_secs(3),async{loop{
            let rows=repo::recent_requests(db.pool(),10).await.unwrap();if let Some(row)=rows.into_iter().next(){break row;}tokio::time::sleep(Duration::from_millis(20)).await;
        }}).await.expect("selection cancellation must persist the in-flight attempt");
        assert_eq!(row["status"],499);assert_eq!(row["error"],"client_cancelled");assert!(row["cost"].is_null());
        assert_eq!(row["routed_provider"],"timed-select");let stored_trace:String=sqlx::query_scalar("SELECT trace_id FROM requests LIMIT 1").fetch_one(db.pool()).await.unwrap();assert_eq!(stored_trace,trace);
        let attempts=row["attempts"].as_array().unwrap();let expected=if prior.is_some(){2}else{1};assert_eq!(attempts.len(),expected);
        if let Some(status)=prior {
            assert_eq!(attempts[0]["status"],status);assert_eq!(attempts[0]["ok"],false);assert_eq!(attempts[0]["timing"]["terminal"],"failed");
            assert!(attempts[0]["timing"]["response_headers_ms"].as_u64().is_some());assert!(attempts[0]["timing"]["finished_ms"].as_u64().is_some());
            assert_eq!(attempts[0]["provider_id"],if auth{"timed-select"}else{"timed-prior"});
        }
        let current=&attempts[expected-1];assert_eq!(current["provider_id"],"timed-select");assert_eq!(current["ok"],false);assert!(current["status"].is_null());
        let reason=current["reason"].as_str().unwrap();if auth{assert!(reason.starts_with("确认复测："));}assert!(reason.contains("client_cancelled"));
        assert_eq!(current["wire_modes"]["schema_version"],1);
        assert_eq!(current["wire_modes"]["caller"]["dialect"],"openai");
        let t=&current["timing"];assert_eq!(t["terminal"],"cancelled");assert_eq!(t["mode"],if streaming {"streaming"} else {"buffered"});assert_eq!(t["response_headers_ms"].as_u64().is_some(),headers_sent);
        assert!(t["finished_ms"].as_u64().is_some());for key in ["first_text_ms","first_reasoning_ms","first_refusal_ms","first_tool_ms"]{assert!(t[key].is_null());}
        // Read the actual shared statistics consumer from this persisted audit.
        let summary=repo::stats_overview(db.pool()).await.unwrap();let phased=&summary["request_timings"];
        assert_eq!(phased["total_requests"],1);assert_eq!(phased["observed_requests"],1);assert_eq!(phased["invalid_requests"],0);
        assert_eq!(phased["valid_attempts"],expected as u64);let groups=phased["groups"].as_array().unwrap();assert_eq!(groups.len(),expected);
        let cancelled=groups.iter().find(|g|g["outcome"]=="cancelled").unwrap();assert_eq!(cancelled["provider_id"],"timed-select");assert_eq!(cancelled["attempts"],1);
        assert_eq!(cancelled["metrics"]["first_text"]["samples"],0);assert!(cancelled["metrics"]["first_text"]["p50_ms"].is_null());
        assert_eq!(cancelled["metrics"]["response_headers"]["samples"],u64::from(headers_sent));assert_eq!(cancelled["metrics"]["finished"]["samples"],1);
        tokio::time::timeout(Duration::from_secs(2),async{while !source_dropped.load(Ordering::SeqCst){tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;assert_eq!(repo::recent_requests(db.pool(),10).await.unwrap().len(),1);assert_eq!(hits.load(Ordering::SeqCst),if auth{2}else{1});assert_eq!(backup_state.hits.load(Ordering::SeqCst),0);
        if let Some(state)=&prior_state{assert_eq!(state.hits.load(Ordering::SeqCst),1);}
        if auth{assert!(repo::list_providers(db.pool()).await.unwrap().iter().find(|p|p.id=="timed-select").unwrap().enabled,"cancelled confirmation must not disable a provider");}
    }).catch_unwind().await;
    gateway.abort();
    source.abort();
    backup.abort();
    if let Some(task) = &prior_task {
        task.abort();
    }
    let _ = gateway.await;
    let _ = source.await;
    let _ = backup.await;
    if let Some(task) = prior_task {
        let _ = task.await;
    }
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
#[tokio::test]
async fn s2_cancel_before_upstream_headers_keeps_unknown_offsets() {
    cancel_during_selection(false, None, true).await;
}
#[tokio::test]
async fn s2_cancel_after_headers_before_first_event_keeps_known_headers() {
    cancel_during_selection(true, None, true).await;
}
#[tokio::test]
async fn s2_cancel_after_prior_429_preserves_both_actual_hops() {
    cancel_during_selection(true, Some(429), true).await;
}
#[tokio::test]
async fn s2_cancel_during_auth_confirmation_keeps_original_failure_without_disabling() {
    cancel_during_selection(false, Some(401), true).await;
}

#[tokio::test]
async fn s2_buffered_cancel_before_upstream_headers_persists_once() {
    cancel_during_selection(false, None, false).await;
}
#[tokio::test]
async fn s2_buffered_cancel_during_body_preserves_known_headers() {
    cancel_during_selection(true, None, false).await;
}
#[tokio::test]
async fn s2_buffered_cancel_after_prior_429_preserves_both_hops() {
    cancel_during_selection(true, Some(429), false).await;
}
#[tokio::test]
async fn s2_buffered_cancel_during_auth_confirmation_does_not_disable() {
    cancel_during_selection(false, Some(401), false).await;
}

// A completed upstream must not disappear when context persistence is waiting.
// Both controls use the real gateway and one owned SQLite connection, no hooks.
async fn post_selection_persistence_control(disconnect: bool, fail_exchange: bool) {
    use futures_util::FutureExt;
    use sqlx::Row;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let arrived = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let hits = Arc::new(AtomicUsize::new(0));
    let mock = Router::new().route("/v1/chat/completions", post({
        let arrived = arrived.clone(); let release = release.clone(); let hits = hits.clone();
        move || { let arrived = arrived.clone(); let release = release.clone(); let hits = hits.clone(); async move {
            hits.fetch_add(1, Ordering::SeqCst); arrived.notify_one(); release.notified().await;
            Json(serde_json::json!({
                "id":"owned-post-selection", "object":"chat.completion", "model":"integration-model",
                "choices":[{"index":0,"message":{"role":"assistant","content":"completed before persistence"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}
            }))
        }}
    }));
    let upstream = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let (backup_url, backup_state, backup) =
        spawn_openai_upstream(OpenAiBehavior::Complete("must not replay completed reply")).await;
    let db = db::Db::connect_in_memory().await.unwrap();
    for p in [
        provider(
            "post-selected",
            format!("http://{address}/v1"),
            Dialect::OpenAI,
            1,
        ),
        provider("post-trap", backup_url, Dialect::OpenAI, 2),
    ] {
        repo::upsert_provider(db.pool(), &p).await.unwrap();
    }
    let cfg = AppConfig {
        port: unused_loopback_port().await,
        unified_key: "owned-post-selection-key".into(),
        ..Default::default()
    };
    let state = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
    state.reload_providers().await.unwrap();
    let base = format!("http://{}:{}", cfg.bind, cfg.port);
    let gateway = tokio::spawn({
        let state = state.clone();
        async move { serve(state).await.unwrap() }
    });
    wait_for_gateway(&base).await;
    let result = std::panic::AssertUnwindSafe(async {
        let authority=base.strip_prefix("http://").unwrap();
        let mut socket=Some(tokio::net::TcpStream::connect(authority).await.unwrap());
        let body=openai_body("integration-model").to_string();
        let trace="123456789abcdef0123456789abcdef0";
        let request=format!("POST /v1/chat/completions HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nX-Trace-Id: {trace}\r\n\r\n{body}",cfg.unified_key,body.len());
        socket.as_mut().unwrap().write_all(request.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2),arrived.notified()).await.unwrap();
        // Preflight is complete; upstream cannot finish until we own the only DB connection.
        let mut connection=db.pool().acquire().await.unwrap();
        if fail_exchange {
            // Only this newly owned in-memory fixture is damaged; audit table stays usable.
            sqlx::query("DROP TABLE session_messages").execute(&mut *connection).await.unwrap();
        }
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2),async {
            while state.health.get("post-selected","integration-model").last_checked_at == 0 {
                tokio::task::yield_now().await;
            }
        }).await.expect("actual upstream JSON/usage must finish before this cancellation");
        assert_eq!(hits.load(Ordering::SeqCst),1);
        let mut unexpected=[0u8;1];
        assert_eq!(socket.as_mut().unwrap().try_read(&mut unexpected).unwrap_err().kind(),std::io::ErrorKind::WouldBlock,"success cannot be sent before context commit");
        if disconnect {
            drop(socket.take());
            // Let the actual socket close reach Axum while DB persistence is still blocked.
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        drop(connection);
        if let Some(mut socket)=socket.take() {
            let mut response=Vec::new();
            tokio::time::timeout(Duration::from_secs(3),socket.read_to_end(&mut response)).await.unwrap().unwrap();
            let response=String::from_utf8(response).unwrap();
            assert!(response.starts_with(if fail_exchange {"HTTP/1.1 500"} else {"HTTP/1.1 200"}));
            if !fail_exchange { assert!(response.contains("completed before persistence")); }
        }
        let row=tokio::time::timeout(Duration::from_secs(3),async {loop {
            if let Some(row)=repo::recent_requests(db.pool(),10).await.unwrap().into_iter().next(){break row;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }}).await.expect("completed upstream audit must survive cancellation during persistence");
        assert_eq!(row["status"],if disconnect {499}else if fail_exchange {500}else{200});
        if disconnect {assert_eq!(row["error"],"client_cancelled");} else if fail_exchange {assert_eq!(row["error"],"unknown");} else {assert!(row["error"].is_null());}
        assert_eq!(row["prompt_tokens"],3,"known upstream consumption is retained even when the client leaves");
        assert_eq!(row["completion_tokens"],2);
        assert!(row["cost"].is_null(),"fixture has no known price; do not invent zero charge");
        let attempts=row["attempts"].as_array().unwrap(); assert_eq!(attempts.len(),1);
        assert_eq!(attempts[0]["ok"],true); assert!(attempts[0]["reason"].is_null());
        let t=timing(&attempts[0],"buffered","completed");assert!(t["first_byte_ms"].as_u64().is_some());assert!(t["first_text_ms"].is_null());
        let actual_trace:String=sqlx::query_scalar("SELECT trace_id FROM requests LIMIT 1").fetch_one(db.pool()).await.unwrap();assert_eq!(actual_trace,trace);
        if !fail_exchange {
            let assistants:i64=sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE role='assistant'").fetch_one(db.pool()).await.unwrap();
            assert_eq!(assistants,if disconnect {0}else{1},"blocked exchange is atomic and cancelled before commit");
        }
        let daily=sqlx::query("SELECT provider_id,model,requests,prompt_tokens,completion_tokens,errors FROM usage_daily").fetch_all(db.pool()).await.unwrap();
        assert_eq!(daily.len(),1);let daily=&daily[0];
        assert_eq!(daily.get::<String,_>("provider_id"),"post-selected");
        assert_eq!(daily.get::<String,_>("model"),"integration-model");
        assert_eq!(daily.get::<i64,_>("requests"),1);
        assert_eq!(daily.get::<i64,_>("prompt_tokens"),3);
        assert_eq!(daily.get::<i64,_>("completion_tokens"),2);
        assert_eq!(daily.get::<i64,_>("errors"),0,"completed HTTP call is not a provider failure when the client/DB fails locally");
        assert!(state.health.get("post-selected","integration-model").last_checked_at > 0);
        assert_eq!(state.health.total_failovers(),0);assert_eq!(backup_state.hits.load(Ordering::SeqCst),0);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(repo::recent_requests(db.pool(),10).await.unwrap().len(),1);
    }).catch_unwind().await;
    gateway.abort();
    upstream.abort();
    backup.abort();
    let _ = gateway.await;
    let _ = upstream.await;
    let _ = backup.await;
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn s2_post_selection_disconnect_preserves_completed_hop_and_known_usage() {
    post_selection_persistence_control(true, false).await;
}
#[tokio::test]
async fn s2_post_selection_connected_waits_for_atomic_exchange_and_one_success_audit() {
    post_selection_persistence_control(false, false).await;
}

#[tokio::test]
async fn s2_post_selection_database_failure_keeps_completed_upstream_audit() {
    post_selection_persistence_control(false, true).await;
}

async fn pre_dispatch_control(streaming: bool, disconnect: bool, unauthorized: bool) {
    use futures_util::FutureExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (url, upstream_state, upstream) =
        spawn_openai_upstream(OpenAiBehavior::Complete("owned preflight reply")).await;
    let db = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(
        db.pool(),
        &provider("preflight-main", url, Dialect::OpenAI, 1),
    )
    .await
    .unwrap();
    let mut cfg = AppConfig {
        port: unused_loopback_port().await,
        unified_key: "owned-preflight-key".into(),
        ..Default::default()
    };
    cfg.smart_routing.enabled = false;
    let state = Arc::new(GatewayState::new(db.clone(), cfg.clone()));
    state.reload_providers().await.unwrap();
    let base = format!("http://{}:{}", cfg.bind, cfg.port);
    let gateway = tokio::spawn({
        let state = state.clone();
        async move { serve(state).await.unwrap() }
    });
    wait_for_gateway(&base).await;
    let result=std::panic::AssertUnwindSafe(async {
        // auto skips the explicit-native lookup; the first DB wait after local
        // auth/allowlist is session touch, before any main upstream dispatch.
        let connection=db.pool().acquire().await.unwrap();
        let authority=base.strip_prefix("http://").unwrap();
        let mut socket=Some(tokio::net::TcpStream::connect(authority).await.unwrap());
        let mut body=openai_body("auto");body["stream"]=serde_json::json!(streaming);let body=body.to_string();
        let trace="23456789abcdef0123456789abcdef01";
        let key=if unauthorized {"owned-incorrect-key"} else {&cfg.unified_key};
        let request=format!("POST /v1/chat/completions HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nX-Session-Id: owned-preflight-session\r\nX-Trace-Id: {trace}\r\n\r\n{body}",body.len());
        socket.as_mut().unwrap().write_all(request.as_bytes()).await.unwrap();
        let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build().unwrap();
        assert_eq!(client.get(format!("{base}/healthz")).send().await.unwrap().status(),StatusCode::OK);
        let mut early_response=Vec::new();
        if unauthorized {
            tokio::time::timeout(Duration::from_secs(2),socket.as_mut().unwrap().read_to_end(&mut early_response)).await.unwrap().unwrap();
            assert!(String::from_utf8_lossy(&early_response).starts_with("HTTP/1.1 401"));
            drop(socket.take());
        } else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut unexpected=[0u8;1];
            assert_eq!(socket.as_mut().unwrap().try_read(&mut unexpected).unwrap_err().kind(),std::io::ErrorKind::WouldBlock);
            if disconnect {
                drop(socket.take());
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
        }
        assert_eq!(upstream_state.hits.load(Ordering::SeqCst),0,"owned DB gate prevents main HTTP dispatch");
        drop(connection);
        if let Some(mut socket)=socket.take() {
            let mut response=Vec::new();
            tokio::time::timeout(Duration::from_secs(3),socket.read_to_end(&mut response)).await.unwrap().unwrap();
            let response=String::from_utf8(response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200"));assert!(response.contains("owned preflight reply"));
        }
        if unauthorized {
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(repo::recent_requests(db.pool(),10).await.unwrap().is_empty(),"rejected requests retain the no-audit contract");
        } else {
            let row=tokio::time::timeout(Duration::from_secs(3),async {loop {
                if let Some(row)=repo::recent_requests(db.pool(),10).await.unwrap().into_iter().next(){break row;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }}).await.expect("permitted pre-dispatch cancellation must have one audit");
            assert_eq!(row["requested_model"],"auto");assert_eq!(row["status"],if disconnect {499}else{200});
            let actual_trace:String=sqlx::query_scalar("SELECT trace_id FROM requests LIMIT 1").fetch_one(db.pool()).await.unwrap();assert_eq!(actual_trace,trace);
            let raw_plan:String=sqlx::query_scalar("SELECT route_decision_json FROM requests LIMIT 1").fetch_one(db.pool()).await.unwrap();
            let plan:serde_json::Value=serde_json::from_str(&raw_plan).unwrap();
            assert_eq!(plan["kind"],if disconnect {"not_reached"}else{"dispatch_plan"});
            assert_eq!(plan["score_state"],"not_recorded");
            if disconnect {
                assert_eq!(row["error"],"client_cancelled");assert!(row["routed_provider"].is_null());assert!(row["routed_model"].is_null());
                assert_eq!(row["prompt_tokens"],0);assert_eq!(row["completion_tokens"],0);assert!(row["cost"].is_null());
                let raw:Option<String>=sqlx::query_scalar("SELECT attempts_json FROM requests LIMIT 1").fetch_one(db.pool()).await.unwrap();assert!(raw.is_none());
                let summary=repo::stats_overview(db.pool()).await.unwrap();assert!(summary.get("request_timings").is_none(),"zero main HTTP attempts cannot invent timing samples");
            } else {
                assert!(row["error"].is_null());assert_eq!(row["routed_provider"],"preflight-main");
                assert_eq!(row["attempts"].as_array().unwrap().len(),1);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert_eq!(repo::recent_requests(db.pool(),10).await.unwrap().len(),1,"preflight owner hands off exactly once");
        }
        assert_eq!(upstream_state.hits.load(Ordering::SeqCst),if unauthorized || disconnect {0}else{1});
        assert_eq!(state.health.total_failovers(),0);
        let assistants:i64=sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE role='assistant'").fetch_one(db.pool()).await.unwrap();
        assert_eq!(assistants,if unauthorized || disconnect {0}else{1});
    }).catch_unwind().await;
    gateway.abort();
    upstream.abort();
    let _ = gateway.await;
    let _ = upstream.await;
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn s2_pre_dispatch_buffered_disconnect_has_no_invented_http_sample() {
    pre_dispatch_control(false, true, false).await;
}
#[tokio::test]
async fn s2_pre_dispatch_streaming_disconnect_has_no_invented_http_sample() {
    pre_dispatch_control(true, true, false).await;
}
#[tokio::test]
async fn s2_pre_dispatch_connected_waits_then_hands_off_one_audit() {
    pre_dispatch_control(false, false, false).await;
}
#[tokio::test]
async fn s2_pre_dispatch_unauthorized_retains_no_audit_contract() {
    pre_dispatch_control(false, false, true).await;
}
