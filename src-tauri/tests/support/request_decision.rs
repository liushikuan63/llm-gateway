//! Finite public-ingress controls for the immutable, per-row candidate plan.
use super::*;

async fn audits(db: &db::Db, count: usize) -> Vec<llm_gateway_lib::audit::AuditRow> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = repo::query_requests(db.pool(), &Default::default())
                .await
                .unwrap()
                .0;
            if rows.len() == count {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned durable audit deadline")
}
#[tokio::test]
async fn request_decision_normal_and_explicit_same_trace_do_not_exchange_plans() {
    let (first, first_seen, first_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("owned first")).await;
    let (second, second_seen, second_task) =
        spawn_openai_upstream(OpenAiBehavior::Complete("owned second")).await;
    let (db, cfg, gateway, url) = spawn_gateway_with(
        vec![
            provider("first", first, Dialect::OpenAI, 1),
            provider("second", second, Dialect::OpenAI, 2),
        ],
        |cfg| cfg.smart_routing.enabled = false,
    )
    .await;
    let trace = "0123456789abcdef0123456789abcdef";
    let client = reqwest::Client::new();
    for (index, model) in ["auto", "second:integration-model"].into_iter().enumerate() {
        let response = client
            .post(format!("{url}/v1/chat/completions"))
            .bearer_auth(&cfg.unified_key)
            .header("x-trace-id", trace)
            .json(&openai_body(model))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.text().await.unwrap();
        audits(&db, index + 1).await;
    }
    let rows = audits(&db, 2).await;
    assert_ne!(rows[0].id, rows[1].id);
    for row in &rows {
        assert_eq!(row.trace_id.as_deref(), Some(trace));
        assert_eq!(row.attempts.len(), 1);
        let json = row.to_json();
        let plan = &json["route_decision"]["snapshot"];
        if row.requested_model == "auto" {
            assert_eq!(plan["total_candidates"], 2);
            assert_eq!(plan["candidates"][0]["target"]["provider_id"], "first");
            assert_eq!(json["routed_provider"], "first");
        } else {
            assert_eq!(plan["total_candidates"], 1);
            assert_eq!(plan["candidates"][0]["target"]["provider_id"], "second");
            assert_eq!(plan["candidates"][0]["target"]["exact_match"], true);
            assert_eq!(json["routed_provider"], "second");
        }
    }
    assert_eq!(first_seen.hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_seen.hits.load(Ordering::SeqCst), 1);
    gateway.abort();
    first_task.abort();
    second_task.abort();
}
#[tokio::test]
async fn request_decision_fallback_buffered_and_stream_keep_plan_separate_from_result() {
    for streaming in [false, true] {
        let (bad, bad_seen, bad_task) =
            spawn_openai_upstream(OpenAiBehavior::Status(StatusCode::SERVICE_UNAVAILABLE)).await;
        let (good, good_seen, good_task) =
            spawn_openai_upstream(OpenAiBehavior::Complete("owned fallback")).await;
        let (db, cfg, gateway, url) = spawn_gateway_with(
            vec![
                provider("first", bad, Dialect::OpenAI, 1),
                provider("second", good.clone(), Dialect::OpenAI, 2),
            ],
            |cfg| {
                cfg.failover_enabled = true;
                cfg.max_fallback_attempts = 2;
                cfg.smart_routing.enabled = false;
            },
        )
        .await;
        let mut body = openai_body("auto");
        body["stream"] = serde_json::json!(streaming);
        // An ingress field with the same name is not an internal observation.
        body["route_decision_json"] = serde_json::json!({"provider_id":"forged"});
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/chat/completions"))
            .bearer_auth(&cfg.unified_key)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.text().await.unwrap().contains("owned fallback"));
        let rows = audits(&db, 1).await;
        let row = rows[0].to_json();
        let plan = &row["route_decision"]["snapshot"];
        assert_eq!(plan["total_candidates"], 2);
        assert_eq!(plan["candidates"][0]["target"]["provider_id"], "first");
        assert_eq!(plan["candidates"][1]["target"]["provider_id"], "second");
        assert_eq!(plan["schema_version"], 2);
        assert_eq!(plan["score_capture"], "first_sort_comparison");
        assert_eq!(plan["score_state"], "recorded");
        assert!(plan["candidates"][0]["score"]["total"].is_number());
        assert_eq!(row["routed_provider"], "second");
        assert_eq!(row["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(bad_seen.hits.load(Ordering::SeqCst), 1);
        assert_eq!(good_seen.hits.load(Ordering::SeqCst), 1);
        // Later configuration changes cannot rewrite a recorded plan.
        repo::upsert_provider(db.pool(), &provider("second", good, Dialect::OpenAI, 0))
            .await
            .unwrap();
        assert_eq!(
            audits(&db, 1).await[0].to_json()["route_decision"],
            row["route_decision"]
        );
        gateway.abort();
        bad_task.abort();
        good_task.abort();
    }
}
#[tokio::test]
async fn request_decision_cache_hit_has_current_capture_without_an_extra_post() {
    let (upstream, observed, worker) =
        spawn_openai_upstream(OpenAiBehavior::Complete("owned cache plan")).await;
    let (db, cfg, gateway, url) = spawn_gateway_with(
        vec![provider("cache-plan", upstream, Dialect::OpenAI, 1)],
        |cfg| {
            cfg.cache.enabled = true;
            cfg.smart_routing.enabled = false;
        },
    )
    .await;
    let client = reqwest::Client::new();
    let mut body = openai_body("integration-model");
    body["temperature"] = serde_json::json!(0.0);
    for n in 1..=2 {
        let response = client
            .post(format!("{url}/v1/chat/completions"))
            .bearer_auth(&cfg.unified_key)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["x-cache"],
            if n == 1 { "MISS" } else { "HIT" }
        );
        response.text().await.unwrap();
        let rows = audits(&db, n).await;
        assert_eq!(rows[0].to_json()["route_decision"]["state"], "recorded");
        if n == 2 {
            assert_ne!(rows[0].trace_id, rows[1].trace_id);
            assert_eq!(rows[0].attempts.len(), 0);
            assert_eq!(rows[1].attempts.len(), 1);
        }
    }
    assert_eq!(observed.hits.load(Ordering::SeqCst), 1);
    gateway.abort();
    worker.abort();
}

// Each counter belongs to a real owned POST handler. Endpoint failures must be
// generated by the embedding handler itself, not a chat-only mock behavior.
async fn typed_embedding_chain_control(first_fails: bool, all_fail: bool) {
    async fn owned_embedding(fails: bool) -> (String, Arc<AtomicUsize>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let observed = hits.clone();
        let app=Router::new().route("/v1/embeddings",post(move |Json(body):Json<serde_json::Value>| {
            let observed=observed.clone();async move {
                observed.fetch_add(1,Ordering::SeqCst);
                if fails {(StatusCode::SERVICE_UNAVAILABLE,Json(serde_json::json!({"error":{"message":"owned unavailable"}}))).into_response()}
                else {Json(serde_json::json!({"model":body["model"],"data":[{"embedding":[0.25],"index":0}],"usage":{"prompt_tokens":2,"total_tokens":2}})).into_response()}
            }
        }));
        (
            format!("http://{address}/v1"),
            hits,
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            }),
        )
    }
    let (first, first_hits, first_task) = owned_embedding(first_fails).await;
    let (second, second_hits, second_task) = owned_embedding(all_fail).await;
    let mut a = provider("typed-first", first.clone(), Dialect::OpenAI, 1);
    a.models[0].model_type = llm_gateway_lib::domain::ModelType::Embedding;
    a.models[0].upstream = "first-physical".into();
    let mut b = provider("typed-second", second, Dialect::OpenAI, 2);
    b.models[0].model_type = llm_gateway_lib::domain::ModelType::Embedding;
    b.models[0].upstream = "second-physical".into();
    let chat = provider("excluded-chat", first, Dialect::OpenAI, 0);
    let (db, cfg, gateway, url) = spawn_gateway_with(vec![chat, a, b], |cfg| {
        cfg.failover_enabled = true;
        cfg.max_fallback_attempts = 2;
    })
    .await;
    let response=reqwest::Client::new().post(format!("{url}/v1/embeddings")).bearer_auth(&cfg.unified_key).json(&serde_json::json!({"model":"auto","input":"DO_NOT_RECORD_INPUT","route_decision_json":{"phase":"forged-chat"}})).send().await.unwrap();
    assert_eq!(
        response.status(),
        if all_fail {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        }
    );
    response.text().await.unwrap();
    let rows = audits(&db, 1).await;
    let row = rows[0].to_json();
    let plan = &row["route_decision"]["snapshot"];
    assert_eq!(row["route_decision"]["state"], "recorded");
    assert_eq!(plan["phase"], "embeddings_plan");
    assert_eq!(plan["total_candidates"], 2);
    assert_eq!(
        plan["candidates"][0]["target"]["provider_id"],
        "typed-first"
    );
    assert_eq!(
        plan["candidates"][1]["target"]["provider_id"],
        "typed-second"
    );
    assert_eq!(
        plan["candidates"][0]["target"]["configured_upstream"],
        "first-physical"
    );
    let raw = plan.to_string();
    for omitted in [
        "DO_NOT_RECORD_INPUT",
        "excluded-chat",
        "127.0.0.1",
        "forged-chat",
    ] {
        assert!(!raw.contains(omitted));
    }
    assert_eq!(
        row["attempts"].as_array().unwrap().len(),
        if first_fails { 2 } else { 1 }
    );
    assert_eq!(row["fallback_attempts"], if first_fails { 1 } else { 0 });
    if all_fail {
        assert!(row["routed_provider"].is_null());
        assert!(row["cost"].is_null());
    } else {
        assert_eq!(
            row["routed_provider"],
            if first_fails {
                "typed-second"
            } else {
                "typed-first"
            }
        );
    }
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), usize::from(first_fails));
    gateway.abort();
    first_task.abort();
    second_task.abort();
}
#[tokio::test]
async fn typed_request_decision_normal_fallback_and_exhaustion_keep_actual_plan() {
    typed_embedding_chain_control(false, false).await;
    typed_embedding_chain_control(true, false).await;
    typed_embedding_chain_control(true, true).await;
}
