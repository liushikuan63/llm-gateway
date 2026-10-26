//! Owned gateway service and SQLite only; no installed application/user data.
use super::*;
use futures_util::FutureExt;
use llm_gateway_lib::proxy_control::gateway::GatewayService;
use tokio::io::AsyncWriteExt;

async fn owned_service(
    provider: Provider,
) -> (
    db::Db,
    Arc<GatewayState>,
    Arc<GatewayService>,
    String,
    String,
) {
    let db = db::Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(db.pool(), &provider).await.unwrap();
    let cfg = AppConfig {
        bind: "127.0.0.1".into(),
        port: 0,
        unified_key: "owned-audit-durability-key".into(),
        ..Default::default()
    };
    let key = cfg.unified_key.clone();
    let state = Arc::new(GatewayState::new(db.clone(), cfg));
    state.reload_providers().await.unwrap();
    let service = Arc::new(GatewayService::new(state.clone()));
    let started = service.set_enabled(true).await.unwrap();
    let base = format!("http://{}", started.bound_addr.unwrap());
    (db, state, service, base, key)
}

async fn audit_database_failure(stopped: bool) {
    let (url, hits, upstream) =
        spawn_openai_upstream(OpenAiBehavior::Complete("owned audited reply")).await;
    let (db, state, service, base, key) =
        owned_service(provider("audit-main", url, Dialect::OpenAI, 1)).await;
    let result = std::panic::AssertUnwindSafe(async {
        // The owned audit table itself fails. Session writes still work.
        sqlx::query("CREATE TRIGGER owned_audit_failure BEFORE INSERT ON requests BEGIN SELECT RAISE(ABORT,'owned audit failure'); END")
            .execute(db.pool()).await.unwrap();
        let response = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap()
            .post(format!("{base}/v1/chat/completions")).bearer_auth(&key)
            .json(&openai_body("integration-model")).send().await.unwrap();
        // Preserve the existing asynchronous audit HTTP contract. Durability
        // must instead be reported truthfully by the existing service status.
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.text().await.unwrap().contains("owned audited reply"));
        let status = if stopped {
            service.set_enabled(false).await.unwrap()
        } else {
            tokio::time::timeout(Duration::from_secs(2), async {loop {
                let status=service.status().await;
                if status.last_error.is_some() {break status;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }}).await.expect("audit database failure must be visible in live service status")
        };
        assert_eq!(status.running, !stopped);
        let error=status.last_error.expect("normal stop cannot report clean completion after audit failure");
        assert!(error.contains("审计") && error.contains("失败"), "{error}");
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM requests").fetch_one(db.pool()).await.unwrap(),0);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM usage_daily").fetch_one(db.pool()).await.unwrap(),0,"failed transaction cannot leave a partial ledger");
        assert_eq!(hits.hits.load(Ordering::SeqCst),1);
        assert_eq!(state.health.total_failovers(),0,"local audit failure is not an upstream failure");
    }).catch_unwind().await;
    let _ = service.set_enabled(false).await;
    upstream.abort();
    let _ = upstream.await;
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn s2_audit_durability_database_failure_is_visible_while_running() {
    audit_database_failure(false).await;
}

#[tokio::test]
async fn s2_audit_durability_database_failure_is_not_clean_normal_stop() {
    audit_database_failure(true).await;
}

#[tokio::test]
async fn s2_audit_durability_normal_stop_waits_for_cancelled_request_commit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let arrived = Arc::new(tokio::sync::Notify::new());
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post({
            let arrived = arrived.clone();
            let hits = hits.clone();
            move || {
                let arrived = arrived.clone();
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    arrived.notify_one();
                    std::future::pending::<Response>().await
                }
            }
        }),
    );
    let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (db, state, service, base, key) = owned_service(provider(
        "audit-main",
        format!("http://{address}/v1"),
        Dialect::OpenAI,
        1,
    ))
    .await;
    let result=std::panic::AssertUnwindSafe(async {
        let authority=base.strip_prefix("http://").unwrap();let mut socket=tokio::net::TcpStream::connect(authority).await.unwrap();
        let body=openai_body("integration-model").to_string();let trace="3456789abcdef0123456789abcdef012";
        socket.write_all(format!("POST /v1/chat/completions HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Trace-Id: {trace}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2),arrived.notified()).await.unwrap();
        let connection=db.pool().acquire().await.unwrap();
        drop(socket);tokio::time::sleep(Duration::from_millis(60)).await;
        let mut closing=tokio::spawn({let service=service.clone();async move {service.set_enabled(false).await}});
        if tokio::time::timeout(Duration::from_millis(120),&mut closing).await.is_ok() {
            panic!("normal stop returned before an owned cancelled-request audit could acquire SQLite");
        }
        drop(connection);
        let stopped=tokio::time::timeout(Duration::from_secs(3),closing).await.unwrap().unwrap().unwrap();
        assert!(!stopped.running);assert!(stopped.last_error.is_none(),"{:?}",stopped.last_error);
        // No row polling: clean close must mean that the audit transaction is committed now.
        let rows=repo::recent_requests(db.pool(),10).await.unwrap();assert_eq!(rows.len(),1);
        assert_eq!(rows[0]["status"],499);assert_eq!(rows[0]["error"],"client_cancelled");
        let actual_trace:String=sqlx::query_scalar("SELECT trace_id FROM requests LIMIT 1").fetch_one(db.pool()).await.unwrap();assert_eq!(actual_trace,trace);
        let attempts=rows[0]["attempts"].as_array().unwrap();assert_eq!(attempts.len(),1);assert_eq!(attempts[0]["timing"]["terminal"],"cancelled");
        assert!(attempts[0]["timing"]["response_headers_ms"].is_null());assert!(rows[0]["cost"].is_null());
        assert_eq!(hits.load(Ordering::SeqCst),1);assert_eq!(state.health.total_failovers(),0);
        let rebound=TcpListener::bind(authority).await.expect("only the owned listener must be released");drop(rebound);
    }).catch_unwind().await;
    let _ = service.set_enabled(false).await;
    upstream.abort();
    let _ = upstream.await;
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
