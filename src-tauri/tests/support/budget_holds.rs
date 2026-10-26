use super::*;
use axum::body::Body;
use axum::http::StatusCode;

fn priced(p: &mut Provider) {
    for m in &mut p.models {
        m.price = Some(ModelPrice {
            prompt: 0.0,
            completion: 1.0,
            cache_read: None,
            cache_creation: None,
            currency: Currency::Usd,
            tiers: vec![],
            rules: vec![],
            source: Default::default(),
        });
    }
}
async fn owned(limit: i64) -> Harness {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let app=Router::new().route("/chat/completions",post(move |Json(body):Json<serde_json::Value>| {
        let calls=handler_calls.clone();
        async move {
            calls.fetch_add(1,Ordering::SeqCst);
            let text=if body["model"]=="backup-physical" {"normal"} else {body["messages"][0]["content"].as_str().unwrap_or("")};
            if text=="failure" {return (StatusCode::BAD_GATEWAY,"owned uncertain upstream").into_response();}
            if text=="pending" {tokio::time::sleep(Duration::from_secs(10)).await;}
            let usage=if text=="unknown" {serde_json::json!({})} else if text=="overrun" {serde_json::json!({"prompt_tokens":0,"completion_tokens":7,"total_tokens":7})} else {serde_json::json!({"prompt_tokens":0,"completion_tokens":1,"total_tokens":1})};
            if body["stream"]==true {
                let data=format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",serde_json::json!({"choices":[{"index":0,"delta":{"content":"owned"},"finish_reason":"stop"}]}),serde_json::json!({"choices":[],"usage":usage}));
                return ([("content-type","text/event-stream")],Body::from(data)).into_response();
            }
            Json(serde_json::json!({"id":"owned","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"owned"},"finish_reason":"stop"}],"usage":usage})).into_response()
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let database = db::Db::connect_in_memory().await.unwrap();
    let mut p = provider(format!("http://{addr}"));
    priced(&mut p);
    repo::upsert_provider(database.pool(), &p).await.unwrap();
    repo::create_remote_access_key(
        database.pool(),
        &access_key("rk-main", true, (limit, "USD", vec![])),
    )
    .await
    .unwrap();
    let mut cfg = AppConfig {
        port: 0,
        unified_key: "lgw-local-unified".into(),
        ..Default::default()
    };
    cfg.remote_mode = llm_gateway_lib::config::RemoteModeConfig {
        enabled: true,
        public_url: Some("https://gateway.example.test".into()),
    };
    cfg.normalize_listener();
    let gateway = Arc::new(GatewayState::new(database.clone(), cfg));
    gateway.reload_providers().await.unwrap();
    let mut task = tokio::spawn({
        let gateway = gateway.clone();
        async move {
            serve(gateway).await.unwrap();
        }
    });
    let base_url = wait_for_gateway(&gateway, &mut task).await;
    Harness {
        db: database,
        gateway,
        base_url,
        mock_calls: calls,
        _task: task,
    }
}
async fn send(h: &Harness, text: &str, stream: bool, max: Option<u32>) -> reqwest::Response {
    client().post(format!("{}/v1/chat/completions",h.base_url))
        .header("x-forwarded-for","198.51.100.7").header("x-forwarded-proto","https")
        // Deliberately identical client traces must not merge ledger identities.
        .header("x-trace-id","aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").bearer_auth(KEY_SECRET)
        .json(&serde_json::json!({"model":"alpha","messages":[{"role":"user","content":text}],"stream":stream,"max_tokens":max})).send().await.unwrap()
}
async fn audits(h: &Harness, count: i64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
                .fetch_one(h.db.pool())
                .await
                .unwrap();
            if n >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn ledger(h: &Harness) -> Vec<(String, i64, Option<i64>, Option<i64>)> {
    sqlx::query_as("SELECT state,reserved_micros,reported_reference_micros,audit_request_id FROM budget_holds ORDER BY created_at,call_ordinal,hold_id").fetch_all(h.db.pool()).await.unwrap()
}

#[tokio::test]
async fn n3_buffered_stream_settle_once_and_atomic_audit_exclusion() {
    let h = owned(4).await;
    for (n, stream) in [(1, false), (2, true), (3, false)] {
        let response = send(&h, "normal", stream, Some(2)).await;
        assert_eq!(response.status(), StatusCode::OK);
        response.bytes().await.unwrap();
        audits(&h, n).await;
        let rows = ledger(&h).await;
        assert_eq!(rows.len(), n as usize);
        assert!(rows
            .iter()
            .all(|r| r.0 == "settled" && r.1 == 2 && r.2 == Some(1) && r.3.is_some()));
        assert_eq!(
            rows.iter()
                .map(|r| r.3.unwrap())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            n as usize
        );
    }
    let response = send(&h, "normal", false, Some(2)).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"]["code"],
        "budget_exceeded"
    );
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 3);
    // Removing audit rows cannot refund the independently retained ledger.
    sqlx::query("DELETE FROM requests")
        .execute(h.db.pool())
        .await
        .unwrap();
    assert_eq!(
        send(&h, "normal", false, Some(2)).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn n3_unknown_failure_and_reported_overrun_never_refund_or_replay() {
    for (text, stream, status, expected) in [
        ("unknown", false, 200, None),
        ("unknown", true, 200, None),
        ("failure", false, 502, None),
        ("failure", true, 502, None),
        ("overrun", false, 200, Some(7)),
    ] {
        let h = owned(2).await;
        let mut backup = h.gateway.providers.read()[0].clone();
        backup.id = "owned-backup".into();
        backup.name = "owned healthy backup".into();
        backup.priority = 100;
        backup.models.retain(|m| m.alias == "alpha");
        backup.models[0].upstream = "backup-physical".into();
        repo::upsert_provider(h.db.pool(), &backup).await.unwrap();
        h.gateway.reload_providers().await.unwrap();
        let mut cfg = h.gateway.cfg_snapshot();
        cfg.routing_strategy = serde_json::from_str("\"priority\"").unwrap();
        cfg.failover_enabled = true;
        cfg.max_fallback_attempts = 3;
        h.gateway.update_cfg(cfg);
        h.gateway.set_active("mock").unwrap();
        let response = send(&h, text, stream, Some(2)).await;
        assert_eq!(response.status().as_u16(), status, "{text}/{stream}");
        response.bytes().await.unwrap();
        audits(&h, 1).await;
        let rows = ledger(&h).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, 2);
        assert_eq!(rows[0].2, expected);
        assert_eq!(
            rows[0].0,
            if expected.is_some() {
                "settled"
            } else {
                "dispatched"
            }
        );
        assert_eq!(
            send(&h, "normal", false, Some(2)).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            h.mock_calls.load(Ordering::SeqCst),
            1,
            "no auth confirmation, retry or fallback after dispatch"
        );
        // Independent explicit control after the protected call: the configured
        // backup really works, rather than being absent/unusable. This is an
        // unlimited owned fixture request, not a refund/replay of the first one.
        let mut cfg = h.gateway.cfg_snapshot();
        cfg.budget.enabled = false;
        h.gateway.update_cfg(cfg);
        let control=client().post(format!("{}/v1/chat/completions",h.base_url))
            .header("x-forwarded-for","198.51.100.7").header("x-forwarded-proto","https").bearer_auth(KEY_SECRET)
            .json(&serde_json::json!({"model":"owned-backup:alpha","messages":[{"role":"user","content":"failure"}],"stream":stream,"max_tokens":2})).send().await.unwrap();
        assert_eq!(control.status(), StatusCode::OK);
        control.bytes().await.unwrap();
        assert_eq!(
            h.mock_calls.load(Ordering::SeqCst),
            2,
            "one primary dispatch plus one separate explicit healthy-backup control"
        );
    }
}

#[tokio::test]
async fn n3_missing_bounds_currency_and_storage_fail_before_post_unlimited_compatible() {
    let h = owned(10).await;
    let r = send(&h, "normal", false, None).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["error"]["reason"],
        "budget_output_bound_unknown"
    );
    audits(&h, 1).await;
    let rows = ledger(&h).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, Some(0));
    let mut p = h.gateway.providers.read()[0].clone();
    p.models[0].price.as_mut().unwrap().currency = Currency::Cny;
    repo::upsert_provider(h.db.pool(), &p).await.unwrap();
    h.gateway.reload_providers().await.unwrap();
    assert_eq!(
        send(&h, "normal", false, Some(2)).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 0);
    sqlx::query("DROP TABLE budget_holds")
        .execute(h.db.pool())
        .await
        .unwrap();
    assert_eq!(
        send(&h, "normal", false, Some(2)).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 0);
    let mut cfg = h.gateway.cfg_snapshot();
    cfg.budget.enabled = false;
    h.gateway.update_cfg(cfg);
    assert_eq!(
        send(&h, "normal", false, None).await.status(),
        StatusCode::OK
    );
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 1);
    let unlimited = owned(0).await;
    assert_eq!(
        send(&unlimited, "normal", false, None).await.status(),
        StatusCode::OK
    );
    assert!(ledger(&unlimited).await.is_empty());
}

#[tokio::test]
async fn n3_cancel_after_post_preserves_hold_and_other_request_completes() {
    let h = owned(4).await;
    let base = h.base_url.clone();
    let pending = tokio::spawn(async move {
        client().post(format!("{base}/v1/chat/completions")).header("x-forwarded-for","198.51.100.7").header("x-forwarded-proto","https").bearer_auth(KEY_SECRET)
            .json(&serde_json::json!({"model":"alpha","messages":[{"role":"user","content":"pending"}],"max_tokens":2})).send().await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while h.mock_calls.load(Ordering::SeqCst) != 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    pending.abort();
    let _ = pending.await;
    let r = send(&h, "normal", false, Some(2)).await;
    assert_eq!(r.status(), StatusCode::OK);
    r.bytes().await.unwrap();
    audits(&h, 2).await;
    let rows = ledger(&h).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .filter(|r| r.0 == "dispatched" && r.1 == 2 && r.2.is_none())
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.0 == "settled" && r.2 == Some(1))
            .count(),
        1
    );
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        send(&h, "normal", false, Some(2)).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        client()
            .get(format!("{}/healthz", h.base_url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn n3_settlement_storage_failure_is_not_replayed_and_keeps_full_hold() {
    let h = owned(2).await;
    sqlx::query("CREATE TRIGGER owned_settlement_failure BEFORE UPDATE OF state ON budget_holds WHEN NEW.state='settled' BEGIN SELECT RAISE(ABORT,'owned storage failure'); END").execute(h.db.pool()).await.unwrap();
    let r = send(&h, "normal", false, Some(2)).await;
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    r.bytes().await.unwrap();
    audits(&h, 1).await;
    let rows = ledger(&h).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "dispatched");
    assert_eq!(rows[0].2, None);
    assert_eq!(
        send(&h, "normal", false, Some(2)).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn n3_cache_hit_has_explicit_zero_dispatch_and_unknown_legacy_blocks() {
    let h = owned(10).await;
    let mut cfg = h.gateway.cfg_snapshot();
    cfg.cache.enabled = true;
    h.gateway.update_cfg(cfg);
    for n in 1..=2 {
        let r = send(&h, "normal", false, Some(2)).await;
        assert_eq!(r.status(), StatusCode::OK);
        r.bytes().await.unwrap();
        audits(&h, n).await;
    }
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 1);
    let rows = ledger(&h).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|r| r.2 == Some(0)).count(), 1);
    let old = owned(10).await;
    seed_spend(&old.db, "rk-main", 0.0, "USD").await;
    sqlx::query("UPDATE requests SET cost=NULL")
        .execute(old.db.pool())
        .await
        .unwrap();
    let r = send(&old, "normal", false, Some(2)).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(
        r.json::<serde_json::Value>().await.unwrap()["error"]["reason"],
        "legacy_cost_unknown"
    );
    assert_eq!(old.mock_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn n3_b_archive_is_counted_by_actual_concurrent_final_dispatch() {
    let h = owned(6).await;
    let month = llm_gateway_lib::budget::month_start_secs(chrono::Utc::now().timestamp());
    sqlx::query("INSERT INTO budget_legacy_archive(audit_request_id,key_id,admission_month,reference_cost,currency) VALUES(999,'rk-main',?,0.000002,'usd')").bind(month).execute(h.db.pool()).await.unwrap();
    let (a, b) = tokio::join!(
        send(&h, "normal", false, Some(4)),
        send(&h, "normal", false, Some(4))
    );
    assert_eq!(
        usize::from(a.status().is_success()) + usize::from(b.status().is_success()),
        1
    );
    let (bad, good) = if a.status().is_success() {
        (b, a)
    } else {
        (a, b)
    };
    assert!(bad
        .text()
        .await
        .unwrap()
        .contains("configured_reference_budget_exceeded"));
    assert!(good.text().await.unwrap().contains("owned"));
    assert_eq!(
        h.mock_calls.load(Ordering::SeqCst),
        1,
        "archive+reservation does not allow both maximum bounds"
    );
    audits(&h, 2).await;
    let snapshot =
        llm_gateway_lib::budget::observation::read(h.db.pool(), "rk-main", chrono::Utc::now())
            .await
            .unwrap();
    assert_eq!(snapshot.total_micros.as_deref(), Some("3"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM budget_legacy_archive")
            .fetch_one(h.db.pool())
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn n3_b_incomplete_history_closes_only_limited_enabled_calls_without_model_post() {
    for (limit, enabled, success) in [(6, true, false), (0, true, true), (6, false, true)] {
        let h = owned(limit).await;
        let month = llm_gateway_lib::budget::month_start_secs(chrono::Utc::now().timestamp());
        let next = chrono::DateTime::from_timestamp(month, 0)
            .unwrap()
            .checked_add_months(chrono::Months::new(1))
            .unwrap()
            .timestamp();
        sqlx::query("UPDATE budget_key_history SET complete_from_month=? WHERE key_id='rk-main'")
            .bind(next)
            .execute(h.db.pool())
            .await
            .unwrap();
        h.gateway.cfg.write().budget.enabled = enabled;
        let response = send(&h, "normal", false, Some(4)).await;
        assert_eq!(
            response.status().is_success(),
            success,
            "limit={limit} enabled={enabled}"
        );
        let body = response.text().await.unwrap();
        if success {
            assert!(body.contains("owned"));
            assert_eq!(h.mock_calls.load(Ordering::SeqCst), 1);
        } else {
            assert!(body.contains("budget_history_incomplete"));
            assert_eq!(h.mock_calls.load(Ordering::SeqCst), 0);
        }
    }
    let h = owned(6).await;
    let month = llm_gateway_lib::budget::month_start_secs(chrono::Utc::now().timestamp());
    sqlx::query("INSERT INTO budget_legacy_archive(audit_request_id,key_id,admission_month,reference_cost,currency) VALUES(999,'rk-main',?,NULL,'usd')").bind(month).execute(h.db.pool()).await.unwrap();
    let response = send(&h, "normal", false, Some(4)).await;
    assert!(!response.status().is_success());
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("legacy_cost_unknown"));
    assert_eq!(h.mock_calls.load(Ordering::SeqCst), 0);
}
