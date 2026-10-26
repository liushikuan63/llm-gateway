use super::*;
async fn complete_flow(classifier_status: u16, streaming: bool) {
    let classified = Arc::new(AtomicUsize::new(0));
    let counter = classified.clone();
    let classifier=Router::new().route("/v1/systemone",post(move |Json(body):Json<serde_json::Value>| {let counter=counter.clone();async move {
        counter.fetch_add(1,Ordering::SeqCst);assert!(body.get("questions").is_some());
        (StatusCode::from_u16(classifier_status).unwrap(),Json(serde_json::json!({"answers":{"complexity":{"type":"choice","choice":"simple","probabilities":{"simple":0.98,"moderate":0.01,"complex":0.01},"confidence":0.99},"needs_web":{"type":"noul","noul":0.0},"clarity":{"type":"noul","noul":0.0}}}))).into_response()
    }}));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let jev_url = format!("http://{}", listener.local_addr().unwrap());
    let jev = tokio::spawn(async move {
        axum::serve(listener, classifier).await.unwrap();
    });
    let refined = Arc::new(AtomicUsize::new(0));
    let main = Arc::new(AtomicUsize::new(0));
    let r = refined.clone();
    let m = main.clone();
    let model=Router::new().route("/v1/chat/completions",post(move |Json(body):Json<serde_json::Value>| {let r=r.clone();let m=m.clone();async move {
        let text=if body["model"]=="refine-physical" {r.fetch_add(1,Ordering::SeqCst);"请把说明整理为清晰的请求，保留原意并明确必要的前提与预期输出"}else{assert_eq!(body["model"],"main-physical");m.fetch_add(1,Ordering::SeqCst);"owned main reply"};
        if body["stream"]==true {
            let delta=serde_json::json!({"choices":[{"index":0,"delta":{"role":"assistant","content":text}}]});
            let finish=serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
            let usage=serde_json::json!({"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":3,"total_tokens":7}});
            return Response::builder().header(CONTENT_TYPE,"text/event-stream").body(format!("data: {delta}\n\ndata: {finish}\n\ndata: {usage}\n\ndata: [DONE]\n\n").into()).unwrap();
        }
        Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":3,"total_tokens":7}})).into_response()
    }}));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let model_task = tokio::spawn(async move {
        axum::serve(listener, model).await.unwrap();
    });
    let mut p = provider("owned-aux", model_url, Dialect::OpenAI, 1);
    p.models[0].upstream = "main-physical".into();
    let (db, cfg, gateway, url) = spawn_gateway_with(vec![p], |cfg| {
        cfg.smart_routing.enabled = true;
        cfg.smart_routing.jev.base_url = jev_url;
        cfg.smart_routing.jev.model = "owned-decider".into();
        cfg.smart_routing.jev.auto_start.enabled = false;
        cfg.smart_routing.prompt_refine.enabled = true;
        cfg.smart_routing.prompt_refine.provider_id = Some("owned-aux".into());
        cfg.smart_routing.prompt_refine.model = Some("refine-physical".into());
        cfg.smart_routing.prompt_refine.min_chars = 1;
    })
    .await;
    let response=reqwest::Client::builder().no_proxy().build().unwrap().post(format!("{url}/v1/chat/completions")).bearer_auth(&cfg.unified_key).json(&serde_json::json!({"model":"smart","stream":streaming,"messages":[{"role":"user","content":"把这段说明写清楚一些，并保留原来的请求要点"}]})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    if streaming {
        assert!(
            response.headers()[CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream"),
            "the streaming control must actually enter the SSE gateway path"
        );
    }
    assert!(response.text().await.unwrap().contains("owned main reply"));
    assert_eq!(classified.load(Ordering::SeqCst), 1);
    assert_eq!(
        refined.load(Ordering::SeqCst),
        usize::from(classifier_status == 200)
    );
    assert_eq!(main.load(Ordering::SeqCst), 1);
    let rows = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = repo::query_requests(db.pool(), &Default::default())
                .await
                .unwrap()
                .0;
            if rows.len() == 1 {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        rows[0].route_classifier.as_deref(),
        Some(if classifier_status == 200 {
            "jev"
        } else {
            "heuristic"
        })
    );
    assert_eq!(
        rows[0].route_refined,
        if classifier_status == 200 {
            Some(true)
        } else {
            None
        }
    );
    let row = rows[0].to_json();
    let calls = &row["route_decision"]["snapshot"]["auxiliary_calls"];
    assert_eq!(
        calls.as_array().map(Vec::len),
        Some(if classifier_status == 200 { 2 } else { 1 }),
        "real internal invocations must remain associated with this one parent audit"
    );
    assert_eq!(calls[0]["purpose"], "classify");
    assert_eq!(calls[0]["model"], "owned-decider");
    assert_eq!(calls[0]["http_status"], classifier_status);
    assert_eq!(calls[0]["result_ok"], classifier_status == 200);
    assert!(calls[0]["primary_usage"].is_null());
    if classifier_status == 200 {
        assert_eq!(calls[1]["purpose"], "refine");
        assert_eq!(calls[1]["model"], "refine-physical");
        assert_eq!(calls[1]["primary_usage"]["total_tokens"], 7);
    }
    assert_eq!(
        row["attempts"].as_array().unwrap().len(),
        1,
        "child observations do not invent main attempts"
    );
    gateway.abort();
    jev.abort();
    model_task.abort();
}

#[tokio::test]
async fn auxiliary_observation_links_real_classifier_refine_and_parent_without_extra_posts() {
    complete_flow(200, false).await;
    complete_flow(503, false).await;
    complete_flow(200, true).await;
}

#[tokio::test]
async fn auxiliary_cancel_during_preflight_keeps_one_unknown_observation_and_isolates_other_request(
) {
    use tokio::io::AsyncWriteExt;
    for hold_refine in [false, true] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let classified = Arc::new(AtomicUsize::new(0));
        let refined = Arc::new(AtomicUsize::new(0));
        let main = Arc::new(AtomicUsize::new(0));
        let n = entered.clone();
        let c = classified.clone();
        let classifier=Router::new().route("/v1/systemone",post(move || {let n=n.clone();let c=c.clone();async move {
            c.fetch_add(1,Ordering::SeqCst);
            if !hold_refine {n.notify_one();return Response::builder().header(CONTENT_TYPE,"application/json").body(axum::body::Body::from_stream(async_stream::stream!{yield Ok::<_,std::io::Error>(axum::body::Bytes::from_static(b"{"));std::future::pending::<()>().await;})).unwrap();}
            Json(serde_json::json!({"answers":{"complexity":{"type":"choice","choice":"simple","probabilities":{"simple":0.99,"complex":0.01},"confidence":0.99},"clarity":{"type":"noul","noul":0.0},"needs_web":{"type":"noul","noul":0.0}}})).into_response()
        }}));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let jev_url = format!("http://{}", listener.local_addr().unwrap());
        let jev = tokio::spawn(async move {
            axum::serve(listener, classifier).await.unwrap();
        });
        let n = entered.clone();
        let r = refined.clone();
        let m = main.clone();
        let upstream=Router::new().route("/v1/chat/completions",post(move |Json(body):Json<serde_json::Value>| {let n=n.clone();let r=r.clone();let m=m.clone();async move {
            if body["model"]=="refine-physical" {r.fetch_add(1,Ordering::SeqCst);n.notify_one();return Response::builder().header(CONTENT_TYPE,"application/json").body(axum::body::Body::from_stream(async_stream::stream!{yield Ok::<_,std::io::Error>(axum::body::Bytes::from_static(b"{"));std::future::pending::<()>().await;})).unwrap();}
            m.fetch_add(1,Ordering::SeqCst);Json(serde_json::json!({"choices":[{"message":{"role":"assistant","content":"other owned reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})).into_response()
        }}));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let mut p = provider("owned-aux", upstream_url, Dialect::OpenAI, 1);
        p.models[0].upstream = "main-physical".into();
        let (db, cfg, gateway, url) = spawn_gateway_with(vec![p], |cfg| {
            cfg.smart_routing.enabled = true;
            cfg.smart_routing.timeout_ms = 2000;
            cfg.smart_routing.jev.timeout_ms = 2000;
            cfg.smart_routing.jev.base_url = jev_url;
            cfg.smart_routing.prompt_refine.enabled = true;
            cfg.smart_routing.prompt_refine.provider_id = Some("owned-aux".into());
            cfg.smart_routing.prompt_refine.model = Some("refine-physical".into());
            cfg.smart_routing.prompt_refine.min_chars = 1;
        })
        .await;
        let authority = url.strip_prefix("http://").unwrap();
        let trace = "1234567890abcdef1234567890abcdef";
        let mut socket = tokio::net::TcpStream::connect(authority).await.unwrap();
        let body=serde_json::json!({"model":"smart","messages":[{"role":"user","content":"把说明写清楚并保留请求要点"}]}).to_string();
        socket.write_all(format!("POST /v1/chat/completions HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\nX-Trace-Id: {trace}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",cfg.unified_key,body.len()).as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        let other = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{url}/v1/chat/completions"))
            .bearer_auth(&cfg.unified_key)
            .json(&openai_body("integration-model"))
            .send()
            .await
            .unwrap();
        assert_eq!(other.status(), StatusCode::OK);
        assert!(other.text().await.unwrap().contains("other owned reply"));
        drop(socket);
        let rows = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let rows = repo::query_requests(db.pool(), &Default::default())
                    .await
                    .unwrap()
                    .0;
                if rows.len() == 2 {
                    break rows;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let cancelled = rows
            .iter()
            .find(|r| r.status == Some(499))
            .unwrap()
            .to_json();
        let completed = rows
            .iter()
            .find(|r| r.status == Some(200))
            .unwrap()
            .to_json();
        assert_eq!(cancelled["trace_id"], trace);
        assert_eq!(
            cancelled["route_decision"]["snapshot"]["kind"],
            "not_reached"
        );
        assert!(cancelled["attempts"].as_array().unwrap().is_empty());
        assert!(cancelled["cost"].is_null());
        let calls = cancelled["route_decision"]["snapshot"]["auxiliary_calls"]
            .as_array()
            .unwrap();
        assert_eq!(calls.len(), if hold_refine { 2 } else { 1 });
        let last = calls.last().unwrap();
        assert_eq!(last["terminal"], "interrupted_unknown");
        assert!(last["primary_usage"].is_null());
        assert_eq!(last["wire_post_verified"], false);
        if hold_refine {
            assert_eq!(calls[0]["terminal"], "returned");
        }
        assert!(completed["route_decision"]["snapshot"]
            .get("auxiliary_calls")
            .is_none());
        assert_eq!(main.load(Ordering::SeqCst), 1);
        assert_eq!(classified.load(Ordering::SeqCst), 1);
        assert_eq!(refined.load(Ordering::SeqCst), usize::from(hold_refine));
        assert_eq!(
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(format!("{url}/healthz"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        gateway.abort();
        jev.abort();
        upstream_task.abort();
    }
}

#[tokio::test]
async fn auxiliary_preflight_ready_failure_keeps_the_completed_call_on_one_failed_parent() {
    let counted = Arc::new(AtomicUsize::new(0));
    let seen = counted.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let jev_url = format!("http://{}", listener.local_addr().unwrap());
    let app=Router::new().route("/v1/systemone",post(move || {let seen=seen.clone();async move {seen.fetch_add(1,Ordering::SeqCst);Json(serde_json::json!({"answers":{"complexity":{"type":"choice","choice":"simple","probabilities":{"simple":0.99,"complex":0.01},"confidence":0.99},"clarity":{"type":"noul","noul":1.0}}}))}}));
    let classifier = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (db, cfg, gateway, url) = spawn_gateway_with(vec![], |cfg| {
        cfg.smart_routing.enabled = true;
        cfg.smart_routing.jev.base_url = jev_url;
        cfg.smart_routing.jev.model = "owned-decider".into();
    })
    .await;
    let response=reqwest::Client::builder().no_proxy().build().unwrap().post(format!("{url}/v1/chat/completions")).bearer_auth(&cfg.unified_key).json(&serde_json::json!({"model":"smart","messages":[{"role":"user","content":"把说明写清楚一些"}]})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    response.text().await.unwrap();
    assert_eq!(
        counted.load(Ordering::SeqCst),
        1,
        "one actual completed internal call precedes the missing main-model error"
    );
    let rows = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = repo::query_requests(db.pool(), &Default::default())
                .await
                .unwrap()
                .0;
            if rows.len() == 1 {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(
        "accepted preflight failure after an actual child call must keep one failed parent audit",
    );
    assert_eq!(rows[0].status, Some(404));
    assert!(rows[0].attempts.is_empty());
    assert!(rows[0].cost.is_none());
    let row = rows[0].to_json();
    assert_eq!(row["route_decision"]["snapshot"]["kind"], "not_reached");
    let calls = &row["route_decision"]["snapshot"]["auxiliary_calls"];
    assert_eq!(calls.as_array().unwrap().len(), 1);
    assert_eq!(calls[0]["terminal"], "returned");
    assert_eq!(calls[0]["model"], "owned-decider");
    gateway.abort();
    classifier.abort();
}
