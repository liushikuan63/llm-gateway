//! Production gateway with owned loopback transports. The SQLite catalog facts
//! below are explicit fixtures, not a verified supplier observation/entitlement.
use axum::{response::IntoResponse, Json, Router};
use llm_gateway_lib::{
    config::AppConfig,
    db::{
        repo::{self, catalog},
        Db,
    },
    domain::Provider,
    model_catalog::observation::decode_openrouter,
    proxy::server::GatewayState,
    proxy_control::gateway::GatewayService,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::{net::TcpListener, task::JoinHandle};

struct OwnedUpstream {
    base: String,
    hits: Arc<AtomicUsize>,
    task: JoinHandle<()>,
    status: Arc<AtomicU16>,
    delay_ms: Arc<std::sync::atomic::AtomicU64>,
    blocked: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Semaphore>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
}
impl Drop for OwnedUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn upstream() -> OwnedUpstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let status = Arc::new(AtomicU16::new(200));
    let blocked = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let delay_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let delay = delay_ms.clone();
    let (behavior, hold, signal, proceed, record) = (
        status.clone(),
        blocked.clone(),
        entered.clone(),
        release.clone(),
        requests.clone(),
    );
    let app=Router::new().fallback(move|request:axum::extract::Request|{let delay=delay.clone();let(count,status,hold,signal,proceed,record)=(count.clone(),behavior.clone(),hold.clone(),signal.clone(),proceed.clone(),record.clone());async move{
        count.fetch_add(1,Ordering::SeqCst);
        let path=request.uri().path().to_string();let bytes=axum::body::to_bytes(request.into_body(),8*1024*1024).await.unwrap();let body:Value=serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        record.lock().unwrap().push((path.clone(),body.clone()));signal.notify_one();
        if hold.load(Ordering::SeqCst){proceed.acquire().await.unwrap().forget();}
        let latency=delay.load(Ordering::SeqCst);if latency>0{tokio::time::sleep(std::time::Duration::from_millis(latency)).await;}
        let status=axum::http::StatusCode::from_u16(status.load(Ordering::SeqCst)).unwrap();
        if !status.is_success(){return(status,Json(json!({"error":{"message":"owned failure"}}))).into_response();}
        if path.ends_with("embeddings"){return Json(json!({"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.5]}],"model":body["model"],"usage":{"prompt_tokens":1,"total_tokens":1}})).into_response();}
        if path.ends_with("generations"){return Json(json!({"created":1,"data":[{"b64_json":"AAAA"}]})).into_response();}
        if body.get("stream").and_then(Value::as_bool)==Some(true){
            let data=format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"owned","model":"owned-model","choices":[{"index":0,"delta":{"role":"assistant","content":"owned answer"},"finish_reason":null}]}),json!({"id":"owned","model":"owned-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":2,"total_tokens":4}}));
            return ([(axum::http::header::CONTENT_TYPE,"text/event-stream")],data).into_response();
        }
        Json(json!({"id":"owned-free-result","object":"chat.completion","model":body["model"],"choices":[{"index":0,"message":{"role":"assistant","content":"owned answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":2,"total_tokens":4}})).into_response()
    }});
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    OwnedUpstream {
        base,
        hits,
        task,
        status,
        delay_ms,
        blocked,
        entered,
        release,
        requests,
    }
}
fn provider(id: &str, base: &str, priority: i32) -> Provider {
    serde_json::from_value(json!({"id":id,"name":id,"base_url":base,"api_key_enc":"","dialect":"openai","enabled":true,"priority":priority,"rpm_limit":0,"intelligence":50,"note":null,"runtime_id":null,"created_at":"2026-10-18T00:00:00Z","updated_at":"2026-10-18T00:00:00Z","models":[{"alias":"owned-model","upstream":"owned-model","enabled":true,"context_window":32768,"supports_tools":true,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","upstream_path":null,"price":null,"overrides":null,"local":null,"capabilities":null}]})).unwrap()
}
async fn seed_fee(db: &Db, p: &Provider, status: &str) {
    let output = match p.models[0].model_type {
        llm_gateway_lib::domain::ModelType::Embedding => "embeddings",
        llm_gateway_lib::domain::ModelType::Image => "image",
        _ => "text",
    };
    let observed=decode_openrouter(&json!({"data":[{"id":"owned-model","context_length":32768,"architecture":{"input_modalities":["text"],"output_modalities":[output]},"pricing":{"prompt":"0","completion":"0","request":"0","image":"0","web_search":"0","internal_reasoning":"0","input_cache_read":"0","input_cache_write":"0"}}]})).unwrap();
    let entry = &observed.models[0];
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query("INSERT INTO catalog_inventory(provider_id,upstream_id,target_fingerprint,observation_generation,observed_at,source_contract,fee_status,fee_evidence_json,fee_expires_at,model_json,lifecycle,last_seen_run_id,last_complete_run_id) VALUES(?,?,?,1,?,'openrouter_full_v1',?,?,?,?,'active','owned-run','owned-run')")
        .bind(&p.id).bind(&entry.model.id).bind(catalog::target_fingerprint(p)).bind(now).bind(status).bind(serde_json::to_string(&entry.fee_evidence).unwrap()).bind(now+catalog::DAY_MS).bind(serde_json::to_string(&entry.model).unwrap()).execute(db.pool()).await.unwrap();
}

async fn gateway(db: Db, mut cfg: AppConfig) -> (Arc<GatewayState>, GatewayService, String) {
    cfg.port = 0;
    cfg.unified_key = "owned-free-key".into();
    let gateway = Arc::new(GatewayState::new(db, cfg));
    gateway.reload_providers().await.unwrap();
    let service = GatewayService::new(gateway.clone());
    let started = service.set_enabled(true).await.unwrap();
    (
        gateway,
        service,
        format!("http://{}", started.bound_addr.unwrap()),
    )
}

async fn send(base: &str, path: &str, body: Value) -> (reqwest::StatusCode, String) {
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{base}{path}"))
        .bearer_auth("owned-free-key")
        .header("x-api-key", "owned-free-key")
        .json(&body)
        .send()
        .await
        .unwrap();
    (response.status(), response.text().await.unwrap())
}

#[tokio::test]
async fn reserved_free_model_uses_qualified_fact_and_never_the_higher_priority_paid_upstream() {
    let free = upstream().await;
    let paid = upstream().await;
    let db = Db::connect_in_memory().await.unwrap();
    let good = provider("free-owned", &free.base, 20);
    let bad = provider("paid-owned", &paid.base, 0);
    repo::upsert_provider(db.pool(), &good).await.unwrap();
    repo::upsert_provider(db.pool(), &bad).await.unwrap();
    seed_fee(&db, &good, "free").await;
    seed_fee(&db, &bad, "paid").await;
    let cfg = AppConfig {
        port: 0,
        unified_key: "owned-free-key".into(),
        ..Default::default()
    };
    let gateway = Arc::new(GatewayState::new(db, cfg));
    gateway.reload_providers().await.unwrap();
    let service = GatewayService::new(gateway);
    let state = service.set_enabled(true).await.unwrap();
    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/v1/chat/completions",
            state.bound_addr.unwrap()
        ))
        .bearer_auth("owned-free-key")
        .json(&json!({"model":"free","messages":[{"role":"user","content":"owned fixture"}]}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    service.set_enabled(false).await.unwrap();
    assert!(
        status.is_success(),
        "free virtual model should reach only the qualified candidate: {status} {body}"
    );
    assert_eq!(free.hits.load(Ordering::SeqCst), 1);
    assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn all_chat_ingress_and_streaming_keep_paid_and_unqualified_auxiliary_posts_at_zero() {
    let free = upstream().await;
    let paid = upstream().await;
    let db = Db::connect_in_memory().await.unwrap();
    let good = provider("free", &free.base, 20);
    let bad = provider("paid", &paid.base, 0);
    repo::upsert_provider(db.pool(), &good).await.unwrap();
    repo::upsert_provider(db.pool(), &bad).await.unwrap();
    seed_fee(&db, &good, "free").await;
    seed_fee(&db, &bad, "paid").await;
    let mut cfg = AppConfig::default();
    cfg.free_routing.enabled = true;
    cfg.smart_routing.enabled = true;
    cfg.routing_strategy = llm_gateway_lib::config::RoutingStrategy::Smart;
    cfg.smart_routing.jev.base_url = format!("{}/decision", paid.base);
    cfg.smart_routing.prompt_refine.enabled = true;
    cfg.smart_routing.prompt_refine.provider_id = Some("paid".into());
    cfg.smart_routing.prompt_refine.model = Some("owned-model".into());
    cfg.search.enabled = true;
    let (_, service, base) = gateway(db, cfg).await;
    let variants = [
        (
            "/v1/chat/completions",
            json!({"model":"free","messages":[{"role":"user","content":"owned fixture"}]}),
        ),
        (
            "/v1/messages",
            json!({"model":"free","max_tokens":32,"messages":[{"role":"user","content":"owned fixture"}]}),
        ),
        (
            "/v1beta/models/free:generateContent",
            json!({"contents":[{"role":"user","parts":[{"text":"owned fixture"}]}]}),
        ),
        (
            "/v1beta/models/free:streamGenerateContent",
            json!({"contents":[{"role":"user","parts":[{"text":"owned fixture"}]}]}),
        ),
        (
            "/api/chat",
            json!({"model":"free","stream":false,"messages":[{"role":"user","content":"owned fixture"}]}),
        ),
        (
            "/v1/responses",
            json!({"model":"free","input":"owned fixture","stream":false}),
        ),
        (
            "/v1/chat/completions",
            json!({"model":"free","stream":true,"messages":[{"role":"user","content":"owned fixture"}]}),
        ),
        (
            "/v1/messages",
            json!({"model":"free","stream":true,"max_tokens":32,"messages":[{"role":"user","content":"owned fixture"}]}),
        ),
        (
            "/api/chat",
            json!({"model":"free","stream":true,"messages":[{"role":"user","content":"owned fixture"}]}),
        ),
        (
            "/v1/responses",
            json!({"model":"free","input":"owned fixture","stream":true}),
        ),
    ];
    for (path, body) in variants {
        let (status, response) = send(&base, path, body).await;
        assert!(status.is_success(), "{path}: {status} {response}");
        assert!(response.contains("owned answer"), "{path}: {response}");
    }
    service.set_enabled(false).await.unwrap();
    assert_eq!(free.hits.load(Ordering::SeqCst), 10);
    {
        let records = free.requests.lock().unwrap();
        assert_eq!(records.len(), 10);
        assert!(records
            .iter()
            .all(|(_, body)| body["model"] == "owned-model"));
    }
    assert_eq!(
        paid.hits.load(Ordering::SeqCst),
        0,
        "all HTTP methods and paths on decision/refinement/paid fixture are counted"
    );
}

#[tokio::test]
async fn fallback_revalidates_revoked_fee_and_never_sends_to_that_upstream() {
    for change in [
        "paid",
        "unknown",
        "expired",
        "disabled",
        "credential",
        "target",
        "policy_off",
    ] {
        let first = upstream().await;
        first.status.store(503, Ordering::SeqCst);
        first.blocked.store(true, Ordering::SeqCst);
        let later = upstream().await;
        let db = Db::connect_in_memory().await.unwrap();
        let a = provider("a-failure", &first.base, 0);
        let b = provider("b-revoked", &later.base, 20);
        for p in [&a, &b] {
            repo::upsert_provider(db.pool(), p).await.unwrap();
            seed_fee(&db, p, "free").await;
        }
        let mut cfg = AppConfig::default();
        cfg.free_routing.enabled = change == "policy_off";
        let (state, service, base) = gateway(db.clone(), cfg).await;
        let requested = if change == "policy_off" {
            "auto"
        } else {
            "free"
        };
        let request = tokio::spawn(async move {
            send(
                &base,
                "/v1/chat/completions",
                json!({"model":requested,"messages":[{"role":"user","content":"owned fixture"}]}),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), first.entered.notified())
            .await
            .unwrap();
        let sql=match change {
        "unknown"=>"UPDATE catalog_inventory SET fee_status='unknown' WHERE provider_id='b-revoked'",
        "expired"=>"UPDATE catalog_inventory SET fee_expires_at=1 WHERE provider_id='b-revoked'",
        "disabled"=>"UPDATE models SET enabled=0 WHERE provider_id='b-revoked'",
        "credential"=>"UPDATE providers SET api_key_enc='owned-new-credential-generation' WHERE id='b-revoked'",
        "target"=>"UPDATE providers SET base_url='http://127.0.0.1:1/v1' WHERE id='b-revoked'",
        _=>"UPDATE catalog_inventory SET fee_status='paid' WHERE provider_id='b-revoked'",
    };
        sqlx::query(sql).execute(db.pool()).await.unwrap();
        if change == "policy_off" {
            let mut cfg = state.cfg_snapshot();
            cfg.free_routing.enabled = false;
            state.update_cfg(cfg);
        }
        first.release.add_permits(1);
        let (status, _) = request.await.unwrap();
        assert!(!status.is_success());
        service.set_enabled(false).await.unwrap();
        assert_eq!(first.hits.load(Ordering::SeqCst), 1);
        assert_eq!(later.hits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn physical_free_execution_permit_blocks_concurrent_send_and_releases_after_completion() {
    let free = upstream().await;
    let paid = upstream().await;
    free.blocked.store(true, Ordering::SeqCst);
    let db = Db::connect_in_memory().await.unwrap();
    let good = provider("free", &free.base, 20);
    let bad = provider("paid", &paid.base, 0);
    for (p, fee) in [(&good, "free"), (&bad, "paid")] {
        repo::upsert_provider(db.pool(), p).await.unwrap();
        seed_fee(&db, p, fee).await;
    }
    let (_, service, base) = gateway(db, AppConfig::default()).await;
    let first_base = base.clone();
    let first = tokio::spawn(async move {
        send(
            &first_base,
            "/v1/chat/completions",
            json!({"model":"free","messages":[{"role":"user","content":"first"}]}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), free.entered.notified())
        .await
        .unwrap();
    let (second, second_body) = send(
        &base,
        "/v1/chat/completions",
        json!({"model":"free","messages":[{"role":"user","content":"parallel"}]}),
    )
    .await;
    assert_eq!(second, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        serde_json::from_str::<Value>(&second_body).unwrap()["error"]["reason"],
        "free_qualified_models_busy"
    );
    assert_eq!(free.hits.load(Ordering::SeqCst), 1);
    assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
    free.blocked.store(false, Ordering::SeqCst);
    free.release.add_permits(1);
    assert!(first.await.unwrap().0.is_success());
    assert!(send(
        &base,
        "/v1/chat/completions",
        json!({"model":"free","messages":[{"role":"user","content":"after release"}]})
    )
    .await
    .0
    .is_success());
    service.set_enabled(false).await.unwrap();
    assert_eq!(free.hits.load(Ordering::SeqCst), 2);
    assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn free_to_free_fallback_after_503_succeeds_in_nonstream_and_stream_positive_controls() {
    for stream in [false, true] {
        let first = upstream().await;
        first.status.store(503, Ordering::SeqCst);
        let second = upstream().await;
        let paid = upstream().await;
        let db = Db::connect_in_memory().await.unwrap();
        let a = provider("a", &first.base, 0);
        let b = provider("b", &second.base, 20);
        let p = provider("paid", &paid.base, 0);
        for (provider, fee) in [(&a, "free"), (&b, "free"), (&p, "paid")] {
            repo::upsert_provider(db.pool(), provider).await.unwrap();
            seed_fee(&db, provider, fee).await;
        }
        let (_, service, base) = gateway(db, AppConfig::default()).await;
        let(status,body)=send(&base,"/v1/chat/completions",json!({"model":"free","stream":stream,"messages":[{"role":"user","content":"owned fallback"}]})).await;
        service.set_enabled(false).await.unwrap();
        assert!(status.is_success(), "stream={stream}: {status} {body}");
        assert!(body.contains("owned answer"));
        assert_eq!(first.hits.load(Ordering::SeqCst), 1);
        assert_eq!(second.hits.load(Ordering::SeqCst), 1);
        assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn embedding_image_passthrough_and_remote_compaction_keep_the_same_free_guard() {
    for (kind, path, body) in [
        (
            llm_gateway_lib::domain::ModelType::Embedding,
            "/v1/embeddings",
            json!({"model":"free","input":"owned"}),
        ),
        (
            llm_gateway_lib::domain::ModelType::Image,
            "/v1/images/generations",
            json!({"model":"free","prompt":"owned","n":1,"size":"256x256"}),
        ),
        (
            llm_gateway_lib::domain::ModelType::Chat,
            "/v1/responses",
            json!({"model":"free","input":[{"role":"user","content":[{"type":"input_text","text":"owned history"}]},{"type":"compaction_trigger"}]}),
        ),
    ] {
        let free = upstream().await;
        let paid = upstream().await;
        let db = Db::connect_in_memory().await.unwrap();
        let mut good = provider("free", &free.base, 20);
        let mut bad = provider("paid", &paid.base, 0);
        good.models[0].model_type = kind;
        bad.models[0].model_type = kind;
        for (p, fee) in [(&good, "free"), (&bad, "paid")] {
            repo::upsert_provider(db.pool(), p).await.unwrap();
            seed_fee(&db, p, fee).await;
        }
        let (_, service, base) = gateway(db, AppConfig::default()).await;
        let (status, response) = send(&base, path, body).await;
        service.set_enabled(false).await.unwrap();
        assert!(status.is_success(), "{path}: {status} {response}");
        if kind == llm_gateway_lib::domain::ModelType::Chat {
            assert!(response.contains("compaction"));
        }
        assert_eq!(free.hits.load(Ordering::SeqCst), 1);
        assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn inherited_virtual_free_policy_survives_before_and_spawned_after_request_summary() {
    let free = upstream().await;
    let paid = upstream().await;
    let db = Db::connect_in_memory().await.unwrap();
    let good = provider("free", &free.base, 20);
    let bad = provider("paid", &paid.base, 0);
    for (p, fee) in [(&good, "free"), (&bad, "paid")] {
        repo::upsert_provider(db.pool(), p).await.unwrap();
        seed_fee(&db, p, fee).await;
    }
    sqlx::query("INSERT INTO sessions(id,title,created_at,updated_at) VALUES('owned-summary','owned','2026-10-18T00:00:00Z','2026-10-18T00:00:00Z')").execute(db.pool()).await.unwrap();
    for (role, text) in [
        ("user", "owned old user history"),
        ("assistant", "owned old assistant history"),
    ] {
        sqlx::query("INSERT INTO session_messages(session_id,role,content,created_at) VALUES('owned-summary',?,?, '2026-10-18T00:00:00Z')").bind(role).bind(text).execute(db.pool()).await.unwrap();
    }
    let cfg = AppConfig {
        compact_threshold_tokens: 1,
        compact_keep_recent: 0,
        ..Default::default()
    };
    assert!(!cfg.free_routing.enabled);
    let (_, service, base) = gateway(db.clone(), cfg).await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth("owned-free-key")
        .header("x-session-id", "owned-summary")
        .json(&json!({"model":"free","messages":[{"role":"user","content":"owned new turn"}]}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "{status} {text}");
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let count: i64 =
                sqlx::query_scalar("SELECT compact_count FROM sessions WHERE id='owned-summary'")
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            if count >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    service.set_enabled(false).await.unwrap();
    assert_eq!(
        free.hits.load(Ordering::SeqCst),
        3,
        "before summary, answer and spawned after summary each use qualified free transport"
    );
    assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn free_preview_uses_current_facts_samples_and_headroom_without_consuming_quota_or_turns() {
    let free = upstream().await;
    let paid = upstream().await;
    let db = Db::connect_in_memory().await.unwrap();
    let mut good = provider("free", &free.base, 20);
    good.rpm_limit = 1;
    let bad = provider("paid", &paid.base, 0);
    for (p, fee) in [(&good, "free"), (&bad, "paid")] {
        repo::upsert_provider(db.pool(), p).await.unwrap();
        seed_fee(&db, p, fee).await;
    }
    let (state, service, base) = gateway(db.clone(), AppConfig::default()).await;
    for _ in 0..20 {
        let rows = state.explain_routing("free").await.unwrap();
        let good = rows.iter().find(|r| r.provider_id == "free").unwrap();
        assert!(good.eligible && good.free_eligibility.as_ref().unwrap().eligible);
        let weight = good.free_weight.as_ref().unwrap();
        assert_eq!(weight.usable_completion_samples, 0);
        assert_eq!(weight.minimum_samples, 5);
        assert!(!weight.metrics_used);
        assert_eq!(weight.local_quota_headroom, 1.0);
        assert_eq!(weight.value, 1.0);
        let bad = rows.iter().find(|r| r.provider_id == "paid").unwrap();
        assert!(!bad.eligible);
        assert!(!bad.free_eligibility.as_ref().unwrap().eligible);
    }
    assert_eq!(free.hits.load(Ordering::SeqCst), 0);
    assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
    assert!(send(
        &base,
        "/v1/chat/completions",
        json!({"model":"free","messages":[{"role":"user","content":"after preview"}]})
    )
    .await
    .0
    .is_success());
    let rows = state.explain_routing("free").await.unwrap();
    let good = rows.iter().find(|r| r.provider_id == "free").unwrap();
    assert!(!good.eligible);
    assert_eq!(good.free_weight.as_ref().unwrap().local_quota_headroom, 0.0);
    service.set_enabled(false).await.unwrap();
    assert_eq!(free.hits.load(Ordering::SeqCst), 1);
    assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn production_two_free_turns_are_neutral_then_measurement_weighted_and_one_preview_does_not_advance_them(
) {
    let mut baseline_first = None;
    for previews in [0, 1] {
        let a = upstream().await;
        let b = upstream().await;
        let paid = upstream().await;
        a.delay_ms.store(4, Ordering::SeqCst);
        b.delay_ms.store(400, Ordering::SeqCst);
        let db = Db::connect_in_memory().await.unwrap();
        let mut pa = provider("a", &a.base, 10);
        let mut pb = provider("b", &b.base, 20);
        let pp = provider("paid", &paid.base, 0);
        pa.rpm_limit = 100;
        pb.rpm_limit = 100;
        for (p, fee) in [(&pa, "free"), (&pb, "free"), (&pp, "paid")] {
            repo::upsert_provider(db.pool(), p).await.unwrap();
            seed_fee(&db, p, fee).await;
        }
        let cfg = AppConfig::default();
        assert!(!cfg.cache.enabled);
        assert!(!llm_gateway_lib::proxy::dedup::should_compute_fingerprint(
            &cfg.dedup
        ));
        let (state, service, base) = gateway(db, cfg).await;
        for _ in 0..previews {
            let rows = state.explain_routing("free").await.unwrap();
            assert_eq!(rows.iter().filter(|r| r.eligible).count(), 2);
        }
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let pick = |turn: usize| {
            let client = client.clone();
            let base = base.clone();
            async move {
                let response=client.post(format!("{base}/v1/chat/completions")).bearer_auth("owned-free-key").header("x-session-id",format!("owned-dist-{previews}-{turn}")).json(&json!({"model":"free","messages":[{"role":"user","content":format!("owned turn {turn}")}]})).send().await.unwrap();
                assert!(response.status().is_success());
                let via = response
                    .headers()
                    .get("x-routed-via")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                response.text().await.unwrap();
                via.split('/').next().unwrap().to_owned()
            }
        };
        let first = pick(0).await;
        if previews == 0 {
            baseline_first = Some(first);
            service.set_enabled(false).await.unwrap();
            assert_eq!(
                a.hits.load(Ordering::SeqCst) + b.hits.load(Ordering::SeqCst),
                1
            );
            assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
            continue;
        }
        assert_eq!(
            Some(&first),
            baseline_first.as_ref(),
            "one preview must not take the first production turn"
        );
        let mut cold = [usize::from(first == "a"), usize::from(first == "b")];
        for turn in 1..8 {
            let selected = pick(turn).await;
            cold[usize::from(selected == "b")] += 1;
        }
        assert_eq!(
            cold,
            [4, 4],
            "fresh independent sessions share neutral model metrics"
        );
        for pid in ["a", "b"] {
            assert!(state.health.get(pid, "owned-model").tps_samples < 5);
        }
        for turn in 8..16 {
            pick(turn).await;
        }
        let rows = state.explain_routing("free").await.unwrap();
        for pid in ["a", "b"] {
            let row = rows.iter().find(|r| r.provider_id == pid).unwrap();
            let weight = row.free_weight.as_ref().unwrap();
            assert!(weight.metrics_used && weight.usable_completion_samples >= 5);
            assert!(weight.local_quota_headroom > 0.0);
        }
        let mut measured = [0, 0];
        for turn in 16..76 {
            let selected = pick(turn).await;
            measured[usize::from(selected == "b")] += 1;
        }
        assert!(
            measured[0] > measured[1] && measured[1] > 0,
            "real measured fast/slow allocations: {measured:?}"
        );
        assert_eq!(paid.hits.load(Ordering::SeqCst), 0);
        for (pid, hits) in [
            ("a", a.hits.load(Ordering::SeqCst)),
            ("b", b.hits.load(Ordering::SeqCst)),
        ] {
            assert!(
                state.health.get(pid, "owned-model").tps_samples as usize <= hits,
                "samples cannot exceed real successful upstream requests"
            );
        }
        service.set_enabled(false).await.unwrap();
    }
}

#[tokio::test]
async fn empty_unknown_and_all_disabled_providers_never_become_free_defaults_and_legacy_is_preserved(
) {
    for disabled in [false, true] {
        let owned = upstream().await;
        let db = Db::connect_in_memory().await.unwrap();
        let mut p = provider("owned", &owned.base, 0);
        if disabled {
            p.models[0].enabled = false;
        } else {
            p.models.clear();
        }
        repo::upsert_provider(db.pool(), &p).await.unwrap();
        let (_, service, base) = gateway(db, AppConfig::default()).await;
        let (status, body) = send(
            &base,
            "/v1/chat/completions",
            json!({"model":"free","messages":[{"role":"user","content":"owned strict"}]}),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::CONFLICT);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
            "free_policy_rejected"
        );
        assert_eq!(owned.hits.load(Ordering::SeqCst), 0);
        let (status, _) = send(
            &base,
            "/v1/chat/completions",
            json!({"model":"auto","messages":[{"role":"user","content":"owned legacy"}]}),
        )
        .await;
        if disabled {
            assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
            assert_eq!(owned.hits.load(Ordering::SeqCst), 0);
        } else {
            assert!(status.is_success());
            assert_eq!(owned.hits.load(Ordering::SeqCst), 1);
        }
        service.set_enabled(false).await.unwrap();
    }
}

#[tokio::test]
async fn free_audit_projects_authoritative_zero_without_overwriting_user_price_and_price_conflicts_are_excluded(
) {
    let owned = upstream().await;
    let db = Db::connect_in_memory().await.unwrap();
    let p = provider("owned", &owned.base, 0);
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    seed_fee(&db, &p, "free").await;
    let (state, service, base) = gateway(db.clone(), AppConfig::default()).await;
    assert!(send(
        &base,
        "/v1/chat/completions",
        json!({"model":"free","messages":[{"role":"user","content":"owned free audit"}]})
    )
    .await
    .0
    .is_success());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
                .fetch_one(db.pool())
                .await
                .unwrap();
            if count > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let audit: (Option<f64>, Option<String>) =
        sqlx::query_as("SELECT cost,currency FROM requests ORDER BY id DESC LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audit, (Some(0.0), Some("usd".into())));
    let stored: Option<String> =
        sqlx::query_scalar("SELECT price_json FROM models WHERE provider_id='owned'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        stored.is_none(),
        "a runtime zero projection must not rewrite user configuration"
    );
    let price = r#"{"prompt":1,"completion":2,"currency":"usd","source":"manual"}"#;
    sqlx::query("UPDATE models SET price_json=? WHERE provider_id='owned'")
        .bind(price)
        .execute(db.pool())
        .await
        .unwrap();
    let (status, body) = send(
        &base,
        "/v1/chat/completions",
        json!({"model":"free","messages":[{"role":"user","content":"owned price conflict"}]}),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert!(body.contains("free_policy_rejected"));
    assert_eq!(owned.hits.load(Ordering::SeqCst), 1);
    let stored: String =
        sqlx::query_scalar("SELECT price_json FROM models WHERE provider_id='owned'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored, price);
    state.reload_providers().await.unwrap();
    assert!(send(
        &base,
        "/v1/chat/completions",
        json!({"model":"auto","messages":[{"role":"user","content":"owned legacy price"}]})
    )
    .await
    .0
    .is_success());
    service.set_enabled(false).await.unwrap();
    assert_eq!(owned.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn free_agent_entry_rejects_before_runtime_lookup_or_any_cli_execution_and_preserves_legacy_errors(
) {
    for global in [false, true] {
        let db = Db::connect_in_memory().await.unwrap();
        let mut cfg = AppConfig::default();
        cfg.free_routing.enabled = global;
        let (_, service, base) = gateway(db.clone(), cfg).await;
        let (status, body) = send(&base, "/gw/agent/run", json!({
            "runtime":"owned-missing-runtime", "model":if global {"owned-model"} else {"free"}, "prompt":"owned request"
        })).await;
        assert_eq!(status, reqwest::StatusCode::CONFLICT);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["error"]["reason"],
            "account_hard_stop_and_reservation_unverified"
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "rejection happens before agent execution/audit, with no registered runtime or credentials");
        if !global {
            let (status, body) = send(&base, "/gw/agent/run", json!({"runtime":"owned-missing-runtime", "model":"owned-model", "prompt":"owned legacy"})).await;
            assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
            assert!(!body.contains("free_policy_rejected"));
        }
        service.set_enabled(false).await.unwrap();
    }
}

#[tokio::test]
async fn model_overrides_hosted_tools_and_unknown_billing_extensions_never_bypass_the_final_wire_guard(
) {
    for (extra, reason) in [
        (
            json!({"model":"owned-paid-model"}),
            "wire_model_override_unverified",
        ),
        (
            json!({"service_tier":"priority"}),
            "billing_extension_unverified",
        ),
        (
            json!({"tools":[{"type":"web_search"}]}),
            "hosted_tool_billing_unverified",
        ),
    ] {
        let owned = upstream().await;
        let db = Db::connect_in_memory().await.unwrap();
        let mut p = provider("owned", &owned.base, 0);
        p.models[0].overrides = Some(serde_json::from_value(json!({"extra_body":extra})).unwrap());
        repo::upsert_provider(db.pool(), &p).await.unwrap();
        seed_fee(&db, &p, "free").await;
        let (state, service, base) = gateway(db, AppConfig::default()).await;
        let preview = state.explain_routing("free").await.unwrap();
        let candidate = preview
            .iter()
            .find(|row| row.provider_id == "owned")
            .unwrap();
        assert!(!candidate.eligible);
        assert_eq!(candidate.free_eligibility.as_ref().unwrap().reason, reason);
        let (status, body) = send(
            &base,
            "/v1/chat/completions",
            json!({"model":"free", "messages":[{"role":"user","content":"owned guard"}]}),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::CONFLICT, "{body}");
        assert_eq!(owned.hits.load(Ordering::SeqCst), 0);
        let (status, body) = send(
            &base,
            "/v1/chat/completions",
            json!({"model":"auto", "messages":[{"role":"user","content":"owned legacy"}]}),
        )
        .await;
        assert!(status.is_success(), "{status} {body}");
        service.set_enabled(false).await.unwrap();
        assert_eq!(
            owned.hits.load(Ordering::SeqCst),
            1,
            "legacy overrides remain supported"
        );
    }
}
