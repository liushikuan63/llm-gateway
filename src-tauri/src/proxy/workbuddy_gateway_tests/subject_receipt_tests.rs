use super::*;

// A missing real transport capture/audit projection must fail, not a fake JSON helper.
#[tokio::test]
async fn opaque_subject_receipt_actual_workbuddy_gateway_groups_retained_turns_without_identity_export(
) {
    for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
        let s = setup_region(region).await;
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        for stream in [false, true] {
            let response=http.post(format!("{}/v1/chat/completions",s.url))
                .bearer_auth("offline-gateway-key").header("x-session-id","owned-subject-conversation")
                .json(&json!({"model":"public","max_tokens":32,"stream":stream,"messages":[{"role":"user","content":"owned synthetic question"}]}))
                .send().await.unwrap();
            assert_eq!(response.status(), 200);
            let _body = response.bytes().await.unwrap();
        }
        for wire in s.fixture.requests.lock().iter() {
            let wire = wire.to_string();
            for forbidden in [
                "subject_receipt",
                "scope_digest",
                "binding_digest",
                "conversation_feedback",
            ] {
                assert!(
                    !wire.contains(forbidden),
                    "local receipt metadata must not enter model wire body"
                );
            }
        }
        s.gateway
            .drain_audits(Duration::from_secs(3))
            .await
            .unwrap();
        let raw: Vec<String> = sqlx::query_scalar(
            "SELECT attempts_json FROM requests WHERE requested_model='public' ORDER BY id",
        )
        .fetch_all(s.gateway.db.pool())
        .await
        .unwrap();
        assert_eq!(raw.len(), 2);
        let mut scopes = Vec::new();
        let mut bindings = Vec::new();
        for row in raw {
            let attempts: Value = serde_json::from_str(&row).unwrap();
            let receipt = &attempts[0]["subject_receipt"];
            assert!(
                receipt.is_object(),
                "actual sender identity was not preserved in the original audit"
            );
            assert_eq!(
                receipt.as_object().unwrap().len(),
                5,
                "minimal allowlisted receipt only"
            );
            assert_eq!(receipt["version"], 1);
            assert_eq!(receipt["purpose"], "conversation_feedback");
            let text = receipt.to_string();
            for forbidden in [
                "gateway-fixture",
                "offline-gateway-access",
                "authorization",
                "identity_hash",
                "principal",
                "credential",
                "email",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "private identity/material leaked into public audit metadata"
                );
            }
            scopes.push(receipt["scope_digest"].as_str().unwrap().to_owned());
            bindings.push(receipt["binding_digest"].as_str().unwrap().to_owned());
        }
        assert_eq!(
            scopes[0], scopes[1],
            "two turns from captured same subject must remain one scope"
        );
        assert_ne!(
            bindings[0], bindings[1],
            "each real request has its own trace/attempt binding"
        );
        let summary = repo::conversation_feedback::summary(
            s.gateway.db.pool(),
            &crate::audit::RequestFilter::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(summary.total_tasks, 1);
        assert_eq!(summary.grouped_matching_requests, 2);
        assert_eq!(summary.ungrouped_matching_requests, 0);
        assert_eq!(summary.tasks[0].counts.requests, 2);
        assert_eq!(
            s.fixture.posts.load(Ordering::SeqCst),
            2,
            "no retry or diagnostic request"
        );
        assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
    }
}
