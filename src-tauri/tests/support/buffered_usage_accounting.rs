use super::*;
#[tokio::test]
async fn buffered_primary_report_unknown_is_not_zero_cost_and_legacy_wire_is_unchanged() {
    use serde_json::json;
    let reply = Arc::new(Mutex::new(json!({})));
    let hits = Arc::new(AtomicUsize::new(0));
    let response = reply.clone();
    let seen = hits.clone();
    let app=Router::new().route("/v1/chat/completions",post(move ||{let response=response.clone();let seen=seen.clone();async move {seen.fetch_add(1,Ordering::SeqCst);let usage=response.lock().unwrap().clone();Json(json!({"choices":[{"message":{"role":"assistant","content":"owned usage reply"},"finish_reason":"stop"}],"usage":usage}))}}));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut p = provider("owned-usage", base, Dialect::OpenAI, 1);
    p.models[0].price = Some(
        serde_json::from_value(
            json!({"prompt":1.0,"completion":2.0,"currency":"usd","source":"manual"}),
        )
        .unwrap(),
    );
    let (db, cfg, gateway, url) = spawn_gateway(vec![p]).await;
    let cases = [
        (json!({}), (0, 0, 0), None),
        (
            json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
            (0, 0, 0),
            Some(0.0),
        ),
        (json!({"prompt_tokens":3}), (3, 0, 3), None),
        (
            json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":4}),
            (3, 2, 5),
            None,
        ),
        (
            json!({"prompt_tokens":3,"completion_tokens":2}),
            (3, 2, 5),
            Some(0.000007),
        ),
        (
            json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}),
            (3, 2, 5),
            Some(0.000007),
        ),
    ];
    for (index, (usage, wire, expected)) in cases.into_iter().enumerate() {
        *reply.lock().unwrap() = usage;
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{url}/v1/chat/completions"))
            .bearer_auth(&cfg.unified_key)
            .json(&openai_body("owned-usage:integration-model"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let output: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            output["choices"][0]["message"]["content"],
            "owned usage reply"
        );
        assert_eq!(
            (
                output["usage"]["prompt_tokens"].as_u64().unwrap(),
                output["usage"]["completion_tokens"].as_u64().unwrap(),
                output["usage"]["total_tokens"].as_u64().unwrap()
            ),
            wire,
            "legacy wire values must remain exactly compatible"
        );
        let rows = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let rows = repo::query_requests(db.pool(), &Default::default())
                    .await
                    .unwrap()
                    .0;
                if rows.len() == index + 1 {
                    break rows;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        match expected {
            None => assert!(
                rows[0].cost.is_none(),
                "empty/partial/contradictory report must not become a known zero reference cost"
            ),
            Some(expected) => {
                let actual = rows[0]
                    .cost
                    .expect("complete report must retain normal positive/zero accounting");
                assert!((actual - expected).abs() < 1e-12);
            }
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            index + 1,
            "only explicit requests, no replay or extra model call"
        );
    }
    gateway.abort();
    upstream.abort();
}
