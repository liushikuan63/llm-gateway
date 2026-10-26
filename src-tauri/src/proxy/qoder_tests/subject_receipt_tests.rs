use super::*;

#[tokio::test]
async fn opaque_subject_receipt_actual_qoder_gateway_survives_credential_generation_change_not_region(
) {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut regions = Vec::new();
    for region in [Region::Global, Region::Cn] {
        let f = setup().await;
        let gateway = gateway(&f, region, false).await;
        let before = f
            .auth
            .prepare_model_authorization(region, "main")
            .await
            .unwrap();
        for stream in [false, true] {
            if stream {
                // Test-only owned credential generation changes, no real refresh endpoint.
                f.auth
                    .test_seed_model_account(
                        region,
                        "main",
                        chrono::Utc::now().timestamp_millis() + 3_600_000,
                    )
                    .await;
                let after = f
                    .auth
                    .prepare_model_authorization(region, "main")
                    .await
                    .unwrap();
                assert_ne!(before.fingerprint, after.fingerprint);
                assert_eq!(before.principal_fingerprint, after.principal_fingerprint);
            }
            let response=http.post(format!("{}/v1/chat/completions",gateway.url)).bearer_auth("synthetic-gateway-key")
    .header("x-session-id","owned-subject-conversation")
    .json(&json!({"model":"public","stream":stream,"max_tokens":32,"messages":[{"role":"user","content":"owned correlation only"}]})).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let _ = response.bytes().await.unwrap();
        }
        for (_, wire) in f.fixture.requests.lock().iter() {
            let wire = wire.to_string();
            for forbidden in [
                "subject_receipt",
                "scope_digest",
                "binding_digest",
                "conversation_feedback",
            ] {
                assert!(
                    !wire.contains(forbidden),
                    "local receipt metadata must not enter decoded signed body"
                );
            }
        }
        gateway
            .state
            .drain_audits(Duration::from_secs(3))
            .await
            .unwrap();
        let raw: Vec<String> = sqlx::query_scalar(
            "SELECT attempts_json FROM requests WHERE requested_model='public' ORDER BY id",
        )
        .fetch_all(f.db.pool())
        .await
        .unwrap();
        assert_eq!(raw.len(), 2);
        let receipts: Vec<Value> = raw
            .iter()
            .map(|r| serde_json::from_str::<Value>(r).unwrap()[0]["subject_receipt"].clone())
            .collect();
        assert!(receipts.iter().all(Value::is_object));
        assert_eq!(receipts[0]["scope_digest"], receipts[1]["scope_digest"]);
        assert_ne!(receipts[0]["binding_digest"], receipts[1]["binding_digest"]);
        let text = serde_json::to_string(&receipts).unwrap();
        for secret in [
            before.fingerprint.as_str(),
            before.principal_fingerprint.as_str(),
            "synthetic-owner-main",
            "synthetic-device",
            "synthetic-job",
            "organization_id",
            "uid",
            "authorization",
        ] {
            assert!(!text.contains(secret));
        }
        regions.push(receipts[0]["scope_digest"].clone());
        let summary = crate::db::repo::conversation_feedback::summary(
            f.db.pool(),
            &crate::audit::RequestFilter::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            (
                summary.total_tasks,
                summary.grouped_matching_requests,
                summary.ungrouped_matching_requests
            ),
            (1, 2, 0)
        );
        assert_eq!(f.fixture.posts.load(Ordering::SeqCst), 2);
    }
    assert_ne!(
        regions[0], regions[1],
        "same visible session/slot across regions must not have the same opaque scope"
    );
}
