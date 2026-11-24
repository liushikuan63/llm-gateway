//! Full gateway dispatch through a local WorkBuddy protocol fixture; no vendor requests.
use crate::{
    config::AppConfig,
    db::{repo, Db},
    domain::{AgentRuntime, Dialect, Provider},
    proxy::{
        server::{serve, GatewayState},
        workbuddy::WorkBuddyModelClient,
    },
    workbuddy_auth::{WorkBuddyAuth, WorkBuddyRegion},
};
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Clone, Default)]
struct Fixture {
    requests: Arc<parking_lot::Mutex<Vec<Value>>>,
    posts: Arc<AtomicUsize>,
    catalog_reads: Arc<AtomicUsize>,
    hold_model: Arc<std::sync::atomic::AtomicBool>,
    fail_model: Arc<std::sync::atomic::AtomicBool>,
    model_entered: Arc<tokio::sync::Notify>,
    model_release: Arc<tokio::sync::Notify>,
    backup_posts: Arc<AtomicUsize>,
    retryable_primary_posts: Arc<AtomicUsize>,
    usage_frames: Arc<parking_lot::Mutex<Option<Vec<Value>>>>,
    unexpected: Arc<AtomicUsize>,
    region: WorkBuddyRegion,
}
struct Servers {
    gateway: Arc<GatewayState>,
    url: String,
    origin: String,
    fixture: Fixture,
    jobs: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Servers {
    fn drop(&mut self) {
        for job in &self.jobs {
            job.abort();
        }
    }
}
async fn catalog(State(f): State<Fixture>) -> Json<Value> {
    f.catalog_reads.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"code":0,"data":{"models":[{"id":"fixture-model","maxInputTokens":65536,"maxOutputTokens":256,"supportsToolCall":true,"supportsImages":true,"reasoning":{"supportedEfforts":["low","high"]}}]}}),
    )
}
async fn completion(
    State(f): State<Fixture>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    assert_eq!(headers["authorization"], "Bearer offline-gateway-access");
    assert_eq!(headers["x-user-id"], "777");
    assert!(!headers.contains_key("x-refresh-token"));
    if f.region == WorkBuddyRegion::Cn {
        assert_eq!(headers["origin"], "https://www.codebuddy.cn");
        assert_eq!(headers["x-domain"], "https://copilot.tencent.com");
        assert_eq!(headers["x-client-id"], "codebuddy-cli");
    } else {
        assert_eq!(headers["origin"], "https://www.workbuddy.ai");
    }
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["messages"][0]["role"], "system",
        "the real international API requires a system head"
    );
    let tools = body.get("tools").is_some();
    f.posts.fetch_add(1, Ordering::SeqCst);
    f.requests.lock().push(body);
    if f.hold_model.load(Ordering::SeqCst) {
        f.model_entered.notify_one();
        f.model_release.notified().await;
    }
    if f.fail_model.load(Ordering::SeqCst) {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"owned_unknown_delivery"}})),
        )
            .into_response();
    }
    let usage_frames = f.usage_frames.lock().clone();
    if let Some(usage_frames) = usage_frames {
        let frames =
            [json!({"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":"stop"}]})]
                .into_iter()
                .chain(
                    usage_frames
                        .into_iter()
                        .map(|usage| json!({"choices":[],"usage":usage})),
                );
        let mut text = frames
            .map(|value| format!("data: {value}\n\n"))
            .collect::<String>();
        text.push_str("data: [DONE]\n\n");
        return (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            text,
        )
            .into_response();
    }
    if tools {
        let frames = [
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_lookup","type":"function","function":{"name":"look","arguments":"{\"q\":\""}}]}}]}),
            json!({"choices":[{"delta":{"content":"between tools","reasoning_content":"vendor thought"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"up","arguments":"foo\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13}}),
        ];
        let mut text = frames
            .into_iter()
            .map(|value| format!("data: {value}\n\n"))
            .collect::<String>();
        text.push_str("data: [DONE]\n\n");
        return (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            text,
        )
            .into_response();
    }
    ([(axum::http::header::CONTENT_TYPE,"text/event-stream")],
     "data: {\"id\":\"fixture-answer\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"vendor reasoning\",\"content\":\"answer\"},\"finish_reason\":\"\"}]}\n\n\
      data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
      data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":3,\"total_tokens\":13,\"completion_tokens_details\":{\"reasoning_tokens\":1},\"prompt_tokens_details\":{\"cached_tokens\":4},\"credit\":0}}\n\n\
      data: [DONE]\n\n".to_owned()).into_response()
}
#[tokio::test]
async fn effort_extra_undeclared_gateway_has_zero_model_posts() {
    let s = setup().await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{}/v1/chat/completions", s.url))
        .bearer_auth("offline-gateway-key")
        .json(
            &json!({"model":"public","messages":[{"role":"user","content":"question"}],
            "reasoning_effort":"fixture-undeclared-secret"}),
        )
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(
        s.fixture.posts.load(Ordering::SeqCst),
        0,
        "undeclared extra effort reached a real loopback model POST; status={status}; body={body}"
    );
    assert!(!status.is_success());
}

async fn setup() -> Servers {
    setup_region(WorkBuddyRegion::Global).await
}

async fn ledger_fixture(
    region: WorkBuddyRegion,
    confirm: fn(&crate::domain::Usage) -> Option<i64>,
) -> (Servers, crate::entitlement::VerifiedEntitlement) {
    ledger_fixture_bound(region, confirm, "current").await
}

async fn ledger_fixture_bound(
    region: WorkBuddyRegion,
    confirm: fn(&crate::domain::Usage) -> Option<i64>,
    binding: &str,
) -> (Servers, crate::entitlement::VerifiedEntitlement) {
    let s = setup_region(region).await;
    let service = match region {
        WorkBuddyRegion::Global => s.gateway.workbuddy_auth.as_ref().unwrap(),
        WorkBuddyRegion::Cn => s.gateway.workbuddy_cn_auth.as_ref().unwrap(),
    };
    let captured = service
        .prepare_model_authorization("gateway-fixture")
        .await
        .unwrap();
    let proof = crate::entitlement::VerifiedEntitlement::artificial_dispatch(
        if region == WorkBuddyRegion::Global {
            "artificial_workbuddy_global"
        } else {
            "artificial_workbuddy_cn"
        },
        "gateway-fixture",
        (
            if binding == "wrong-principal" {
                let mut other = captured.identity_hash.clone();
                other.replace_range(
                    ..1,
                    if captured.identity_hash.starts_with('0') {
                        "1"
                    } else {
                        "0"
                    },
                );
                other
            } else {
                captured.identity_hash.clone()
            },
            captured.source_fingerprint().to_owned(),
        ),
        if binding == "wrong-model" {
            "owned-other-model"
        } else {
            "fixture-model"
        },
        "chat",
        2,
    );
    let provider = s.gateway.providers.read()[0].clone();
    s.gateway
        .cost_gate
        .accounts
        .install_artificial(&provider, "saved-id", proof.clone(), confirm)
        .await
        .unwrap();
    let mut cfg = s.gateway.cfg_snapshot();
    cfg.free_routing.enabled = true;
    cfg.failover_enabled = true;
    cfg.max_fallback_attempts = 3;
    cfg.auth_failure.mode = crate::config::AuthFailureMode::Skip;
    cfg.auth_failure.confirm_retries = 1;
    s.gateway.update_cfg(cfg);
    (s, proof)
}

async fn ledger_send(url: &str, stream: bool) -> reqwest::Response {
    reqwest::Client::builder().no_proxy().build().unwrap()
        .post(format!("{url}/v1/chat/completions")).bearer_auth("offline-gateway-key")
        .json(&json!({"model":"public","stream":stream,"messages":[{"role":"user","content":"owned entitlement control"}]}))
        .send().await.unwrap()
}

async fn ledger_success(stream: bool) {
    for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
        // Explicit private request-count contract; no token/credit conversion.
        let (s, proof) = ledger_fixture(region, |_| Some(1)).await;
        let response = ledger_send(&s.url, stream).await;
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(
            status, 200,
            "private verified grant must reach real WorkBuddy consumer: {body}"
        );
        assert!(body.contains("answer"));
        let balance = crate::entitlement::Ledger::new(s.gateway.db.pool().clone())
            .balance(&proof)
            .await
            .unwrap();
        assert_eq!(
            (
                balance.ceiling,
                balance.held,
                balance.spent,
                balance.blocked
            ),
            (2, 0, 1, false)
        );
        let rows: Vec<(String, i64, Option<i64>)> =
            sqlx::query_as("SELECT phase,bound,confirmed_units FROM free_entitlement_reservations")
                .fetch_all(s.gateway.db.pool())
                .await
                .unwrap();
        assert_eq!(rows, vec![("settled".into(), 1, Some(1))]);
        assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
        assert_eq!(s.fixture.catalog_reads.load(Ordering::SeqCst), 1);
        assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
        s.gateway
            .drain_audits(Duration::from_secs(3))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn workbuddy_ledger_success_buffered_single_settlement() {
    ledger_success(false).await;
}

#[tokio::test]
async fn workbuddy_ledger_success_stream_single_settlement() {
    ledger_success(true).await;
}

async fn ledger_assert_one(
    s: &Servers,
    proof: &crate::entitlement::VerifiedEntitlement,
    expected: (i64, i64, &str, Option<i64>),
) {
    let balance = crate::entitlement::Ledger::new(s.gateway.db.pool().clone())
        .balance(proof)
        .await
        .unwrap();
    assert_eq!((balance.held, balance.spent), (expected.0, expected.1));
    let rows: Vec<(String, i64, Option<i64>)> =
        sqlx::query_as("SELECT phase,bound,confirmed_units FROM free_entitlement_reservations")
            .fetch_all(s.gateway.db.pool())
            .await
            .unwrap();
    assert_eq!(rows, vec![(expected.2.into(), 1, expected.3)]);
    assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
}

async fn ledger_qualified_backup(s: &Servers, proof: &crate::entitlement::VerifiedEntitlement) {
    let mut backup = s.gateway.providers.read()[0].clone();
    backup.id = "owned-entitlement-fallback".into();
    backup.priority = 2;
    repo::upsert_provider(s.gateway.db.pool(), &backup)
        .await
        .unwrap();
    s.gateway
        .cost_gate
        .accounts
        .install_artificial(&backup, "saved-id", proof.clone(), |_| Some(1))
        .await
        .unwrap();
    assert!(s
        .gateway
        .cost_gate
        .accounts
        .eligible(&backup, "saved-id", "chat")
        .await
        .unwrap());
    s.gateway.reload_providers().await.unwrap();
}

#[tokio::test]
async fn workbuddy_ledger_source_principal_and_model_mismatch_refund_only_presend() {
    for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
        for binding in ["stale-source", "wrong-principal", "wrong-model"] {
            let (s, proof) = ledger_fixture_bound(region, |_| Some(1), binding).await;
            if binding == "stale-source" {
                let service = match region {
                    WorkBuddyRegion::Global => s.gateway.workbuddy_auth.as_ref().unwrap(),
                    WorkBuddyRegion::Cn => s.gateway.workbuddy_cn_auth.as_ref().unwrap(),
                };
                let before = service
                    .prepare_model_authorization("gateway-fixture")
                    .await
                    .unwrap();
                // Same access token and UID; a changed saved credential generation
                // still cannot borrow the old capability.
                service
                    .test_seed_account(
                        "gateway-fixture",
                        "777",
                        "offline-gateway-access",
                        "owned-other-refresh",
                    )
                    .await;
                let after = service
                    .prepare_model_authorization("gateway-fixture")
                    .await
                    .unwrap();
                assert_eq!(before.credential_fingerprint, after.credential_fingerprint);
                assert_eq!(before.identity_hash, after.identity_hash);
                assert_ne!(before.source_fingerprint(), after.source_fingerprint());
            }
            let response = ledger_send(&s.url, false).await;
            assert_eq!(
                response.status(),
                axum::http::StatusCode::CONFLICT,
                "{binding}"
            );
            response.bytes().await.unwrap();
            assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 0, "{binding}");
            assert_eq!(s.fixture.catalog_reads.load(Ordering::SeqCst), 1);
            ledger_assert_one(&s, &proof, (0, 0, "cancelled", None)).await;
        }
    }
}

#[tokio::test]
async fn workbuddy_ledger_unknown_dispatch_cannot_replay_to_qualified_account() {
    for stream in [false, true] {
        let (s, proof) = ledger_fixture(WorkBuddyRegion::Global, |_| Some(1)).await;
        ledger_qualified_backup(&s, &proof).await;
        s.fixture.fail_model.store(true, Ordering::SeqCst);
        let response = ledger_send(&s.url, stream).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        response.bytes().await.unwrap();
        assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
        assert_eq!(s.fixture.catalog_reads.load(Ordering::SeqCst), 1);
        ledger_assert_one(&s, &proof, (1, 0, "dispatched", None)).await;
        s.gateway
            .drain_audits(Duration::from_secs(3))
            .await
            .unwrap();
        let (attempts, fallbacks): (String, i64) =
            sqlx::query_as("SELECT attempts_json,fallback_attempts FROM requests")
                .fetch_one(s.gateway.db.pool())
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<Value>>(&attempts).unwrap().len(),
            1
        );
        assert_eq!(fallbacks, 0);
    }
}

#[tokio::test]
async fn workbuddy_ledger_untrusted_terminal_does_not_guess_zero_or_settlement() {
    for stream in [false, true] {
        let (s, proof) = ledger_fixture(WorkBuddyRegion::Global, |_| None).await;
        let response = ledger_send(&s.url, stream).await;
        assert_eq!(response.status(), 200);
        assert!(response.text().await.unwrap().contains("answer"));
        assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
        ledger_assert_one(&s, &proof, (1, 0, "dispatched", None)).await;
    }
}

#[tokio::test]
async fn workbuddy_ledger_storage_failures_reject_without_refund_or_replay() {
    for phase in ["reserve", "dispatch", "settle"] {
        let (s, proof) = ledger_fixture(WorkBuddyRegion::Global, |_| Some(1)).await;
        ledger_qualified_backup(&s, &proof).await;
        let trigger = match phase {
            "reserve" => "CREATE TRIGGER owned_reserve_failure BEFORE INSERT ON free_entitlement_reservations BEGIN SELECT RAISE(ABORT,'owned reserve failure'); END",
            "dispatch" => "CREATE TRIGGER owned_dispatch_failure BEFORE UPDATE ON free_entitlement_reservations WHEN NEW.phase='dispatched' BEGIN SELECT RAISE(ABORT,'owned dispatch failure'); END",
            _ => "CREATE TRIGGER owned_settle_failure BEFORE UPDATE ON free_entitlement_budgets WHEN NEW.spent>OLD.spent BEGIN SELECT RAISE(ABORT,'owned settle failure'); END",
        };
        sqlx::query(trigger)
            .execute(s.gateway.db.pool())
            .await
            .unwrap();
        let response = ledger_send(&s.url, false).await;
        assert!(!response.status().is_success(), "{phase}");
        response.bytes().await.unwrap();
        assert_eq!(
            s.fixture.posts.load(Ordering::SeqCst),
            usize::from(phase == "settle"),
            "{phase}"
        );
        if phase == "reserve" {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM free_entitlement_reservations")
                    .fetch_one(s.gateway.db.pool())
                    .await
                    .unwrap();
            assert_eq!(count, 0);
            let b = crate::entitlement::Ledger::new(s.gateway.db.pool().clone())
                .balance(&proof)
                .await
                .unwrap();
            assert_eq!((b.held, b.spent), (0, 0));
        } else {
            ledger_assert_one(
                &s,
                &proof,
                (
                    1,
                    0,
                    if phase == "dispatch" {
                        "reserved"
                    } else {
                        "dispatched"
                    },
                    None,
                ),
            )
            .await;
        }
        assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn workbuddy_ledger_post_dispatch_rotation_keeps_full_unknown_hold() {
    for stream in [false, true] {
        let (s, proof) = ledger_fixture(WorkBuddyRegion::Global, |_| Some(1)).await;
        ledger_qualified_backup(&s, &proof).await;
        s.fixture.hold_model.store(true, Ordering::SeqCst);
        let url = s.url.clone();
        let task = tokio::spawn(async move { ledger_send(&url, stream).await });
        tokio::time::timeout(Duration::from_secs(3), s.fixture.model_entered.notified())
            .await
            .unwrap();
        ledger_assert_one(&s, &proof, (1, 0, "dispatched", None)).await;
        s.gateway
            .workbuddy_auth
            .as_ref()
            .unwrap()
            .test_seed_account(
                "gateway-fixture",
                "777",
                "offline-gateway-access",
                "owned-after-post-refresh",
            )
            .await;
        s.fixture.model_release.notify_one();
        let response = task.await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        response.bytes().await.unwrap();
        assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
        ledger_assert_one(&s, &proof, (1, 0, "dispatched", None)).await;
    }
}

#[tokio::test]
async fn workbuddy_ledger_tcp_cancel_after_dispatch_keeps_full_unknown_hold() {
    use tokio::io::AsyncWriteExt;
    let (s, proof) = ledger_fixture(WorkBuddyRegion::Global, |_| Some(1)).await;
    s.fixture.hold_model.store(true, Ordering::SeqCst);
    let body = json!({"model":"public","stream":false,"messages":[{"role":"user","content":"owned cancel"}]}).to_string();
    let address = s.url.strip_prefix("http://").unwrap();
    let trace = uuid::Uuid::new_v4().simple().to_string();
    let wire = format!("POST /v1/chat/completions HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer offline-gateway-key\r\nX-Trace-Id: {trace}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    socket.write_all(wire.as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), s.fixture.model_entered.notified())
        .await
        .unwrap();
    ledger_assert_one(&s, &proof, (1, 0, "dispatched", None)).await;
    drop(socket);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE trace_id=? AND status=499")
                    .bind(&trace)
                    .fetch_one(s.gateway.db.pool())
                    .await
                    .unwrap();
            if count == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
    ledger_assert_one(&s, &proof, (1, 0, "dispatched", None)).await;
    s.fixture.model_release.notify_one();
}

#[tokio::test]
async fn workbuddy_snapshot_gateway_post_dispatch_rotation_cannot_confirm_or_fallback() {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
        for stream in [false, true] {
            let s = setup_region(region).await;
            let mut backup = s.gateway.providers.read()[0].clone();
            backup.id = "owned-qualified-backup".into();
            backup.name = "Owned qualified backup".into();
            backup.runtime_id = None;
            backup.base_url = format!("{}/v1", s.origin);
            backup.api_key_enc = crate::crypto::encrypt("owned-backup-key").unwrap();
            backup.priority = 2;
            backup.models[0].upstream = "owned-backup-model".into();
            let mut control_model = backup.models[0].clone();
            control_model.alias = "backup-only".into();
            backup.models.push(control_model);
            repo::upsert_provider(s.gateway.db.pool(), &backup)
                .await
                .unwrap();
            s.gateway.reload_providers().await.unwrap();
            let mut cfg = s.gateway.cfg_snapshot();
            cfg.auth_failure.mode = crate::config::AuthFailureMode::Skip;
            cfg.auth_failure.confirm_retries = 1;
            cfg.failover_enabled = true;
            cfg.max_fallback_attempts = 3;
            cfg.routing_strategy = crate::config::RoutingStrategy::Priority;
            s.gateway.update_cfg(cfg);
            // Prove that the keyed same-alias fallback has a working production
            // transport, rather than hiding a replay behind an unusable candidate.
            let control = http.post(format!("{}/v1/chat/completions", s.url))
                .bearer_auth("offline-gateway-key")
                .json(&json!({"model":"backup-only","messages":[{"role":"user","content":"owned healthy control"}]}))
                .send().await.unwrap();
            assert_eq!(control.status(), 200);
            assert!(control.text().await.unwrap().contains("backup answer"));
            assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 1);
            s.fixture.hold_model.store(true, Ordering::SeqCst);
            let url = s.url.clone();
            let sending = http.clone();
            let task = tokio::spawn(async move {
                sending.post(format!("{url}/v1/chat/completions"))
                    .bearer_auth("offline-gateway-key")
                    .json(&json!({"model":"public","stream":stream,"messages":[{"role":"user","content":"owned post-dispatch rotation"}]}))
                    .send().await.unwrap()
            });
            tokio::time::timeout(Duration::from_secs(3), s.fixture.model_entered.notified())
                .await
                .unwrap();
            assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
            let auth = match region {
                WorkBuddyRegion::Global => s.gateway.workbuddy_auth.as_ref().unwrap(),
                WorkBuddyRegion::Cn => s.gateway.workbuddy_cn_auth.as_ref().unwrap(),
            };
            auth.test_seed_account(
                "gateway-fixture",
                "777",
                "owned-rotated-access",
                "owned-rotated-refresh",
            )
            .await;
            s.fixture.model_release.notify_one();
            let response = tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
            response.bytes().await.unwrap();
            s.gateway
                .drain_audits(Duration::from_secs(3))
                .await
                .unwrap();
            let (attempts, fallbacks): (String, i64) = sqlx::query_as("SELECT attempts_json, fallback_attempts FROM requests WHERE requested_model='public'")
                .fetch_one(s.gateway.db.pool()).await.unwrap();
            let attempts: Vec<Value> = serde_json::from_str(&attempts).unwrap();
            assert_eq!(
                attempts.len(),
                1,
                "no hidden authentication confirmation or fallback: {attempts:?}"
            );
            assert_eq!(fallbacks, 0);
            assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
            assert_eq!(
                s.fixture.backup_posts.load(Ordering::SeqCst),
                1,
                "only the explicit healthy control may reach the backup"
            );
            assert_eq!(s.fixture.catalog_reads.load(Ordering::SeqCst), 1);
            assert_eq!(
                s.fixture.unexpected.load(Ordering::SeqCst),
                0,
                "no identity, refresh, directory or other HTTP"
            );
        }
    }
}

async fn backup_completion(State(f): State<Fixture>) -> Json<Value> {
    f.backup_posts.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"id":"owned-backup","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"backup answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
    )
}

async fn retryable_primary_completion(State(f): State<Fixture>) -> axum::http::StatusCode {
    f.retryable_primary_posts.fetch_add(1, Ordering::SeqCst);
    axum::http::StatusCode::BAD_GATEWAY
}

async fn unexpected_request(State(f): State<Fixture>) -> axum::http::StatusCode {
    f.unexpected.fetch_add(1, Ordering::SeqCst);
    axum::http::StatusCode::NOT_FOUND
}

#[tokio::test]
async fn responses_instructions_h10_workbuddy_both_regions_current_request_only() {
    for region in [WorkBuddyRegion::Cn, WorkBuddyRegion::Global] {
        let s = setup_region(region).await;
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        for stream in [false, true] {
            let sid = format!("instructions-{stream}");
            for instruction in [Some("WB-A α\r\n"), Some("WB-B\t"), None, Some(" \t\r\n ")] {
                let mut body = json!({"model":"public","input":"caller original","stream":stream});
                if let Some(instruction) = instruction {
                    body["instructions"] = json!(instruction);
                }
                let response = http
                    .post(format!("{}/v1/responses", s.url))
                    .bearer_auth("offline-gateway-key")
                    .header("x-session-id", &sid)
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), 200);
                let text = response.text().await.unwrap();
                if stream {
                    assert!(text.contains("response.completed"), "{text}");
                } else {
                    assert_eq!(
                        serde_json::from_str::<Value>(&text).unwrap()["status"],
                        "completed"
                    );
                }
                let wire = s.fixture.requests.lock().last().unwrap().clone();
                if let Some(instruction) = instruction {
                    assert_eq!(wire["messages"][0]["content"], instruction);
                } else {
                    assert!(!wire.to_string().contains("WB-A"));
                    assert!(!wire.to_string().contains("WB-B"));
                }
                let systems: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE role='system'")
                        .fetch_one(s.gateway.db.pool())
                        .await
                        .unwrap();
                assert_eq!(systems, 0);
            }
        }
        assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 8);
    }
}
async fn setup_region(region: WorkBuddyRegion) -> Servers {
    use base64::Engine;
    if std::env::var_os("LLMGW_MASTER_KEY").is_none() {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            base64::engine::general_purpose::STANDARD.encode([73; 32]),
        );
    }
    let db = Db::connect_in_memory().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let fixture = Fixture {
        region,
        ..Default::default()
    };
    let upstream = Router::new()
        .route("/v3/config", get(catalog))
        .route("/v2/chat/completions", post(completion))
        .route("/v1/chat/completions", post(backup_completion))
        .route(
            "/primary/v1/chat/completions",
            post(retryable_primary_completion),
        )
        .fallback(unexpected_request)
        .with_state(fixture.clone());
    let mock = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let auth = match region {
        WorkBuddyRegion::Global => {
            WorkBuddyAuth::with_test_origin(db.pool().clone(), origin.clone())
        }
        WorkBuddyRegion::Cn => {
            WorkBuddyAuth::with_test_origin_for_region(db.pool().clone(), origin.clone(), region)
        }
    };
    auth.test_seed_account(
        "gateway-fixture",
        "777",
        "offline-gateway-access",
        "offline-gateway-refresh",
    )
    .await;
    let mut runtime = AgentRuntime::new("wb-gateway", "workbuddy-model", "WB fixture");
    runtime.options = Some(
        json!({"region":region,"account_id":"gateway-fixture","model_aliases":{"saved-id":"fixture-model"}}),
    );
    repo::upsert_agent_runtime(db.pool(), &runtime)
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let provider=Provider {id:"wb-gateway-provider".into(),name:"WB gateway fixture".into(),dialect:Dialect::OpenAI,
        // Binding metadata is official; test_origin remains the sole network target.
        base_url:WorkBuddyAuth::issuer_for_region(region).into(),api_key_enc:String::new(),enabled:true,priority:1,rpm_limit:0,intelligence:50,
        models:vec![serde_json::from_value(json!({"alias":"public","upstream":"saved-id","context_window":65536,
           "supports_tools":true,"supports_vision":true,"supports_stream":true,"supports_thinking":true,
           "overrides":{"temperature":0.2,"max_tokens":32}})).unwrap()],runtime_id:Some(runtime.id),note:None,created_at:now,updated_at:now};
    repo::upsert_provider(db.pool(), &provider).await.unwrap();
    let cfg = AppConfig {
        port: 0,
        unified_key: "offline-gateway-key".into(),
        http_proxy: None,
        ..Default::default()
    };
    let mut gateway = GatewayState::new(db, cfg);
    let client = Ok(Arc::new(WorkBuddyModelClient::with_test_origin(
        auth.clone(),
        origin.clone(),
    )));
    match region {
        WorkBuddyRegion::Global => {
            gateway.workbuddy_auth = Ok(auth);
            gateway.workbuddy = client;
        }
        WorkBuddyRegion::Cn => {
            gateway.workbuddy_cn_auth = Ok(auth);
            gateway.workbuddy_cn = client;
        }
    }
    let gateway = Arc::new(gateway);
    gateway.reload_providers().await.unwrap();
    let for_task = gateway.clone();
    let server = tokio::spawn(async move {
        serve(for_task).await.unwrap();
    });
    let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(snapshot) = gateway.listener_snapshot() {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    Servers {
        gateway,
        url: format!("http://{}", snapshot.bound_addr),
        origin,
        fixture,
        jobs: vec![mock, server],
    }
}
#[tokio::test]
async fn gateway_workbuddy_rejects_lossy_openai_ingress_in_both_regions() {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut failures = Vec::new();
    for (region, label) in [
        (WorkBuddyRegion::Global, "global"),
        (WorkBuddyRegion::Cn, "cn"),
    ] {
        let s = setup_region(region).await;
        let invalid = json!([{"role":"user","content":[
            {"type":"text","text":"owned question"},
            {"type":"input_file","file_id":"owned-file"}
        ]}]);
        for stream in [false, true] {
            let before = (
                s.fixture.posts.load(Ordering::SeqCst),
                s.fixture.catalog_reads.load(Ordering::SeqCst),
            );
            let response = http
                .post(format!("{}/v1/chat/completions", s.url))
                .bearer_auth("offline-gateway-key")
                .json(&json!({"model":"public","stream":stream,"messages":invalid}))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let wire = response.text().await.unwrap();
            let error = serde_json::from_str::<Value>(&wire).unwrap_or(Value::Null);
            let after = (
                s.fixture.posts.load(Ordering::SeqCst),
                s.fixture.catalog_reads.load(Ordering::SeqCst),
            );
            if status != axum::http::StatusCode::BAD_REQUEST
                || !error["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("unsupported_account_content_part"))
                || after != before
            {
                failures.push(format!(
                    "{label} stream={stream}: status={status}, counts={before:?}->{after:?}"
                ));
            }
        }
        let control = http
            .post(format!("{}/v1/chat/completions", s.url))
            .bearer_auth("offline-gateway-key")
            .json(&json!({"model":"public","messages":[{"role":"user","content":"owned supported question"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(control.status(), axum::http::StatusCode::OK, "{label}");
        assert!(control.text().await.unwrap().contains("answer"));

        let mut ordinary = s.gateway.providers.read()[0].clone();
        ordinary.id = "owned-ordinary".into();
        ordinary.runtime_id = None;
        ordinary.base_url = format!("{}/v1", s.origin);
        ordinary.models[0].alias = "owned-ordinary-only".into();
        repo::upsert_provider(s.gateway.db.pool(), &ordinary)
            .await
            .unwrap();
        s.gateway.reload_providers().await.unwrap();
        let response = http
            .post(format!("{}/v1/chat/completions", s.url))
            .bearer_auth("offline-gateway-key")
            .json(&json!({"model":"owned-ordinary-only","messages":invalid}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK, "{label}");
        assert!(response.text().await.unwrap().contains("backup answer"));
        assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 1);
        assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
    }
    assert!(failures.is_empty(), "lossy WorkBuddy ingress: {failures:?}");
}

async fn mixed_openai_candidates() -> Servers {
    let s = setup_region(WorkBuddyRegion::Global).await;
    let mut ordinary = s.gateway.providers.read()[0].clone();
    ordinary.id = "owned-mixed-http".into();
    ordinary.runtime_id = None;
    ordinary.base_url = format!("{}/v1", s.origin);
    repo::upsert_provider(s.gateway.db.pool(), &ordinary)
        .await
        .unwrap();
    s.gateway.reload_providers().await.unwrap();
    s
}

#[tokio::test]
async fn mixed_gateway_rejects_lossy_openai_ingress_before_reusing_buffered_reply() {
    let s = mixed_openai_candidates().await;
    let mut cfg = s.gateway.cfg_snapshot();
    cfg.cache.enabled = true;
    cfg.failover_enabled = true;
    cfg.max_fallback_attempts = 2;
    s.gateway.update_cfg(cfg);

    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let send = |request: Value| {
        http.post(format!("{}/v1/chat/completions", s.url))
            .bearer_auth("offline-gateway-key")
            .json(&request)
            .send()
    };
    let clean = json!({"model":"public","messages":[{"role":"user","content":[
        {"type":"text","text":"owned cache and route control"}
    ]}]});
    let mut lossy = clean.clone();
    lossy["messages"][0]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"input_file","file_id":"owned-file"}));

    s.gateway.set_active("wb-gateway-provider").unwrap();
    for cache in ["MISS", "HIT"] {
        let response = send(clean.clone()).await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["x-cache"], cache);
        assert!(response.text().await.unwrap().contains("answer"));
    }
    for stream in [false, true] {
        let mut request = lossy.clone();
        request["stream"] = json!(stream);
        let response = send(request).await.unwrap();
        assert_eq!(response.status(), 400);
        assert!(!response.headers().contains_key("x-cache"));
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["type"], "invalid_request_error");
        assert!(error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unsupported_account_content_part"));
    }
    assert_eq!(s.gateway.cache.stats().hits, 1);
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
    assert_eq!(s.fixture.catalog_reads.load(Ordering::SeqCst), 1);
    assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 0);

    s.gateway.set_active("owned-mixed-http").unwrap();
    let response = send(lossy).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-cache"], "MISS");
    assert!(response.text().await.unwrap().contains("backup answer"));
    assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 1);
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
    assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mixed_gateway_rejects_lossy_openai_ingress_before_dedup_conflict() {
    let s = mixed_openai_candidates().await;
    let mut cfg = s.gateway.cfg_snapshot();
    cfg.cache.enabled = false;
    cfg.dedup.mode = crate::proxy::dedup::DedupMode::Enforce;
    cfg.dedup.window_secs = 60;
    s.gateway.update_cfg(cfg);
    s.gateway.set_active("wb-gateway-provider").unwrap();

    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let send = |request: &Value| {
        http.post(format!("{}/v1/chat/completions", s.url))
            .bearer_auth("offline-gateway-key")
            .json(request)
            .send()
    };
    let clean = json!({"model":"public","messages":[{"role":"user","content":[
        {"type":"text","text":"owned dedup collision"}
    ]}]});
    // Deliberately omit X-Session-Id so each admission sees the same normalized
    // request without the previous reply in its context.
    let first = send(&clean).await.unwrap();
    assert_eq!(first.status(), 200);
    assert!(first.text().await.unwrap().contains("answer"));
    let duplicate = send(&clean).await.unwrap();
    assert_eq!(duplicate.status(), 409);
    assert_eq!(duplicate.headers()["x-dedup"], "conflict");
    assert_eq!(s.gateway.dedup.stats().conflicts, 1);

    let mut lossy = clean;
    lossy["messages"][0]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"input_file","file_id":"owned-file"}));
    let response = send(&lossy).await.unwrap();
    let status = response.status();
    let was_deduplicated = response.headers().contains_key("x-dedup");
    let error: Value = response.json().await.unwrap();
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
    assert_eq!(s.fixture.catalog_reads.load(Ordering::SeqCst), 1);
    assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 0);
    assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
    assert_eq!(
        status, 400,
        "lossy account request must reach ingress rejection, got {error}"
    );
    assert!(!was_deduplicated);
    assert_eq!(s.gateway.dedup.stats().conflicts, 1);
    assert_eq!(error["error"]["type"], "invalid_request_error");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unsupported_account_content_part"));
}

#[tokio::test]
async fn account_ingress_fallback_separates_retryable_http_from_lossy_account_dispatch() {
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    for stream in [false, true] {
        for lossy in [false, true] {
            // Restart the fixture so the 502 cooldown and directory cache cannot
            // make subsequent requests appear to skip the account.
            let s = setup_region(WorkBuddyRegion::Global).await;
            let mut account = s.gateway.providers.read()[0].clone();
            let mut primary = account.clone();
            primary.id = "owned-retryable-primary".into();
            primary.name = "Owned retryable primary".into();
            primary.runtime_id = None;
            primary.base_url = format!("{}/primary/v1", s.origin);
            primary.priority = 1;
            primary.models[0].upstream = "owned-primary-model".into();
            account.priority = 2;
            let mut backup = primary.clone();
            backup.id = "owned-fallback-sentinel".into();
            backup.name = "Owned fallback sentinel".into();
            backup.base_url = format!("{}/v1", s.origin);
            backup.priority = 3;
            backup.models[0].upstream = "owned-backup-model".into();
            for provider in [&primary, &account, &backup] {
                repo::upsert_provider(s.gateway.db.pool(), provider)
                    .await
                    .unwrap();
            }
            s.gateway.reload_providers().await.unwrap();
            let mut cfg = s.gateway.cfg_snapshot();
            cfg.routing_strategy = crate::config::RoutingStrategy::Priority;
            cfg.failover_enabled = true;
            cfg.max_fallback_attempts = 3;
            cfg.smart_routing.enabled = false;
            cfg.search.enabled = false;
            cfg.cache.enabled = false;
            cfg.dedup.mode = crate::proxy::dedup::DedupMode::Off;
            s.gateway.update_cfg(cfg);
            s.gateway.set_active(&primary.id).unwrap();

            let mut parts = vec![json!({"type":"text","text":"owned fallback control"})];
            if lossy {
                parts.push(json!({"type":"input_file","file_id":"owned-file"}));
            }
            let response = http
                .post(format!("{}/v1/chat/completions", s.url))
                .bearer_auth("offline-gateway-key")
                .json(&json!({
                    "model":"public","stream":stream,
                    "messages":[{"role":"user","content":parts}]
                }))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.text().await.unwrap();
            let wire = (
                s.fixture.retryable_primary_posts.load(Ordering::SeqCst),
                s.fixture.catalog_reads.load(Ordering::SeqCst),
                s.fixture.posts.load(Ordering::SeqCst),
                s.fixture.backup_posts.load(Ordering::SeqCst),
                s.fixture.unexpected.load(Ordering::SeqCst),
            );
            if lossy {
                assert_eq!(wire, (1, 0, 0, 0, 0), "stream={stream}: {body}");
                assert_eq!(status, 400, "stream={stream}: {body}");
                assert!(!headers.contains_key("x-routed-via"));
                let error: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(error["error"]["type"], "invalid_request_error");
                assert!(error["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("unsupported_account_content_part"));
            } else {
                assert_eq!(wire, (1, 1, 1, 0, 0), "stream={stream}: {body}");
                assert_eq!(status, 200, "stream={stream}: {body}");
                assert_eq!(headers["x-routed-via"], "wb-gateway-provider/saved-id");
                assert_eq!(headers["x-fallback-attempts"], "2");
                assert!(body.contains("answer"), "{body}");
                if stream {
                    assert_eq!(
                        body.lines().filter(|line| *line == "data: [DONE]").count(),
                        1,
                        "{body}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn independent_account_gateway_preserves_protocols_usage_and_history() {
    let s = setup().await;
    preserves_protocols_usage_and_history(s).await;
}
#[tokio::test]
async fn cn_independent_account_gateway_preserves_protocols_usage_and_history() {
    let s = setup_region(WorkBuddyRegion::Cn).await;
    preserves_protocols_usage_and_history(s).await;
}
#[tokio::test]
async fn cn_gateway_rejects_global_provider_binding_before_model_post() {
    let s = setup_region(WorkBuddyRegion::Cn).await;
    sqlx::query("UPDATE providers SET base_url=? WHERE id=?")
        .bind(WorkBuddyAuth::issuer_for_region(WorkBuddyRegion::Global))
        .bind("wb-gateway-provider")
        .execute(s.gateway.db.pool())
        .await
        .unwrap();
    s.gateway.reload_providers().await.unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for stream in [false, true] {
        let response = http.post(format!("{}/v1/chat/completions", s.url))
            .bearer_auth("offline-gateway-key")
            .json(&json!({"model":"public","stream":stream,"messages":[{"role":"user","content":"question"}]}))
            .send().await.unwrap();
        assert!(
            !response.status().is_success(),
            "cross-region provider must fail"
        );
        let body = response.text().await.unwrap();
        assert!(
            body.contains("所有候选 Provider 均不可用")
                || body.contains("供应商区域或凭据类型不一致"),
            "{body}"
        );
    }
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 0);
    assert!(s.fixture.requests.lock().is_empty());
}
async fn preserves_protocols_usage_and_history(s: Servers) {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for stream in [false, true] {
        let response=http.post(format!("{}/v1/chat/completions",s.url)).bearer_auth("offline-gateway-key")
            .json(&json!({"model":"public","stream":stream,"messages":[{"role":"system","content":"system context"},{"role":"user","content":"question"}],"temperature":0.8,"max_tokens":64})).send().await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.text().await.unwrap();
        if stream {
            assert!(body.contains("reasoning_content"));
            assert!(body.contains("vendor reasoning"));
            assert!(body.contains("answer"));
            assert_eq!(body.matches("data: [DONE]").count(), 1);
            assert!(body.contains("reasoning_tokens"));
        } else {
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["choices"][0]["message"]["content"], "answer");
            assert_eq!(
                body["choices"][0]["message"]["reasoning_content"],
                "vendor reasoning"
            );
            assert_eq!(body["usage"]["prompt_tokens"], 10);
            assert_eq!(body["usage"]["credit"].as_f64(), Some(0.0));
        }
    }
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 2);
    for request in s.fixture.requests.lock().iter() {
        assert_eq!(request["model"], "fixture-model");
        assert_eq!(request["messages"][0]["content"], "system context");
        assert!((request["temperature"].as_f64().unwrap() - 0.2).abs() < 0.000001);
        assert_eq!(request["max_tokens"], 32);
    }
    tokio::time::timeout(Duration::from_secs(5),async {loop {
        let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE status=200 AND prompt_tokens=10 AND completion_tokens=3").fetch_one(s.gateway.db.pool()).await.unwrap();
        if count==2 {break;} tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.unwrap();
    let history: String =
        sqlx::query_scalar("SELECT group_concat(content,'|') FROM session_messages")
            .fetch_one(s.gateway.db.pool())
            .await
            .unwrap();
    assert!(history.contains("question") && history.contains("answer"));
    assert!(!history.contains("vendor reasoning"));
}

#[tokio::test]
async fn disabled_direct_account_does_not_call_http_or_cli() {
    let s = setup().await;
    let mut runtime = repo::get_agent_runtime(s.gateway.db.pool(), "wb-gateway")
        .await
        .unwrap()
        .unwrap();
    runtime.enabled = false;
    repo::upsert_agent_runtime(s.gateway.db.pool(), &runtime)
        .await
        .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for stream in [false, true] {
        let response=http.post(format!("{}/v1/chat/completions",s.url)).bearer_auth("offline-gateway-key")
            .json(&json!({"model":"public","stream":stream,"messages":[{"role":"user","content":"question"}]})).send().await.unwrap();
        assert!(!response.status().is_success());
    }
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fragmented_tool_names_and_arguments_survive_all_five_gateway_streams() {
    let s = setup().await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let function = json!({"name":"lookup","parameters":{"type":"object","properties":{"q":{"type":"string"}}}});
    let cases = [
        (
            "openai",
            "/v1/chat/completions",
            json!({"model":"public","stream":true,"messages":[{"role":"user","content":"call"}],"tools":[{"type":"function","function":function}]}),
        ),
        (
            "anthropic",
            "/v1/messages",
            json!({"model":"public","stream":true,"max_tokens":32,"messages":[{"role":"user","content":"call"}],"tools":[{"name":"lookup","input_schema":function["parameters"]}]}),
        ),
        (
            "responses",
            "/v1/responses",
            json!({"model":"public","stream":true,"input":"call","tools":[{"type":"function","name":"lookup","parameters":function["parameters"]}]}),
        ),
        (
            "ollama",
            "/api/chat",
            json!({"model":"public","stream":true,"messages":[{"role":"user","content":"call"}],"tools":[{"type":"function","function":function}]}),
        ),
        (
            "gemini",
            "/v1beta/models/public:streamGenerateContent?alt=sse",
            json!({"contents":[{"role":"user","parts":[{"text":"call"}]}],"tools":[{"functionDeclarations":[function]}]}),
        ),
    ];
    for (protocol, path, body) in cases {
        let response = http
            .post(format!("{}{path}", s.url))
            .bearer_auth("offline-gateway-key")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK, "{protocol}");
        let wire = response.text().await.unwrap();
        let frames: Vec<Value> = wire
            .lines()
            .filter_map(|line| {
                let line = if protocol == "ollama" {
                    line
                } else {
                    line.strip_prefix("data: ")?
                };
                serde_json::from_str(line).ok()
            })
            .collect();
        match protocol {
            "gemini" => {
                let tools: Vec<&Value> = frames
                    .iter()
                    .flat_map(|v| {
                        v["candidates"][0]["content"]["parts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                    })
                    .filter_map(|v| v.get("functionCall"))
                    .collect();
                assert_eq!(tools.len(), 1, "Gemini wire: {wire}");
                assert_eq!(tools[0]["name"], "lookup");
                assert_eq!(tools[0]["args"], json!({"q":"foo"}));
            }
            "ollama" => {
                let tools: Vec<&Value> = frames
                    .iter()
                    .flat_map(|v| v["message"]["tool_calls"].as_array().into_iter().flatten())
                    .collect();
                assert_eq!(tools.len(), 1);
                assert_eq!(tools[0]["function"]["name"], "lookup");
                assert_eq!(tools[0]["function"]["arguments"], json!({"q":"foo"}));
            }
            "anthropic" => {
                let arguments: String = frames
                    .iter()
                    .filter_map(|v| v["delta"]["partial_json"].as_str())
                    .collect();
                assert_eq!(
                    serde_json::from_str::<Value>(&arguments).unwrap(),
                    json!({"q":"foo"})
                );
                assert!(frames
                    .iter()
                    .any(|v| v["content_block"]["name"] == "lookup"));
            }
            "responses" => {
                let arguments: String = frames
                    .iter()
                    .filter(|v| v["type"] == "response.function_call_arguments.delta")
                    .filter_map(|v| v["delta"].as_str())
                    .collect();
                assert_eq!(
                    serde_json::from_str::<Value>(&arguments).unwrap(),
                    json!({"q":"foo"})
                );
            }
            _ => {
                let tools: Vec<&Value> = frames
                    .iter()
                    .flat_map(|v| {
                        v["choices"][0]["delta"]["tool_calls"]
                            .as_array()
                            .into_iter()
                            .flatten()
                    })
                    .collect();
                let arguments: String = tools
                    .iter()
                    .filter_map(|v| v["function"]["arguments"].as_str())
                    .collect();
                assert_eq!(
                    serde_json::from_str::<Value>(&arguments).unwrap(),
                    json!({"q":"foo"})
                );
                assert_eq!(
                    tools
                        .iter()
                        .filter_map(|v| v["function"]["name"].as_str())
                        .collect::<String>(),
                    "lookup"
                );
            }
        }
    }
    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 5);
}

// All fixtures use owned in-memory databases and loopback servers. Drop aborts both servers.
#[tokio::test]
async fn effort_gateway_both_regions_and_transports_reject_before_model_post() {
    let mut controls = Vec::new();
    for key in ["effort", "reasoning_effort"] {
        for value in [
            json!("fixture-private-effort"),
            json!(""),
            Value::Null,
            json!(false),
            json!(9),
            json!([]),
            json!({}),
            json!("HIGH"),
            json!(" high "),
        ] {
            let mut thinking = json!({});
            thinking[key] = value;
            controls.push(json!({"thinking":thinking}));
        }
    }
    for value in [
        json!("fixture-private-effort"),
        json!(""),
        Value::Null,
        json!(false),
        json!(9),
        json!([]),
        json!({}),
        json!("HIGH"),
        json!(" high "),
    ] {
        controls.push(json!({"reasoning_effort":value}));
    }
    controls.extend([
        json!({"thinking":{"effort":"low","reasoning_effort":"high"}}),
        json!({"thinking":{"reasoning_effort":"low","effort":"high"}}),
        json!({"thinking":{"effort":"high"},"reasoning_effort":"low"}),
        json!({"thinking":{"reasoning_effort":"high"},"reasoning_effort":"low"}),
        json!({"thinking":{"effort":"high","reasoning_effort":null}}),
        json!({"thinking":{"effort":null,"reasoning_effort":"high"}}),
    ]);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut cases = 0;
    for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
        for stream in [false, true] {
            for control in &controls {
                // A new gateway for each rejection prevents health/circuit state masking a POST.
                let s = setup_region(region).await;
                let mut request = json!({"model":"public","stream":stream,
                    "messages":[{"role":"user","content":"question"}]});
                request
                    .as_object_mut()
                    .unwrap()
                    .extend(control.as_object().unwrap().clone());
                let response = http
                    .post(format!("{}/v1/chat/completions", s.url))
                    .bearer_auth("offline-gateway-key")
                    .json(&request)
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                let body = response.text().await.unwrap();
                assert!(
                    !status.is_success(),
                    "{region:?} stream={stream} {control}: {status} {body}"
                );
                assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 0);
                assert!(s.fixture.requests.lock().is_empty());
                assert_eq!(
                    s.fixture.catalog_reads.load(Ordering::SeqCst),
                    1,
                    "rejection must reach the catalog-backed WorkBuddy path"
                );
                assert!(!body.contains("fixture-private-effort"));
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 132);
}

#[tokio::test]
async fn effort_gateway_valid_controls_preserve_exact_wire_finish_and_usage() {
    let controls = [
        json!({}),
        json!({"thinking":{"effort":"high"}}),
        json!({"thinking":{"reasoning_effort":"high"}}),
        json!({"reasoning_effort":"high"}),
        json!({"thinking":{"effort":"high","reasoning_effort":"high","budget_tokens":4321},"reasoning_effort":"high"}),
        json!({"thinking":{"type":"enabled","budget_tokens":4321}}),
        json!({"thinking":{"opaque":"fixture"},"reasoning_effort":"high"}),
    ];
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut cases = 0;
    for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
        for stream in [false, true] {
            for control in &controls {
                let s = setup_region(region).await;
                let mut request = json!({"model":"public","stream":stream,
                    "messages":[{"role":"user","content":"question"}]});
                request
                    .as_object_mut()
                    .unwrap()
                    .extend(control.as_object().unwrap().clone());
                let response = http
                    .post(format!("{}/v1/chat/completions", s.url))
                    .bearer_auth("offline-gateway-key")
                    .json(&request)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), 200);
                let text = response.text().await.unwrap();
                if stream {
                    assert!(text.contains("answer"));
                    assert!(text.contains("\"finish_reason\":\"stop\""));
                    assert!(text.contains("\"prompt_tokens\":10"));
                    assert!(text.contains("\"completion_tokens\":3"));
                    assert_eq!(text.matches("data: [DONE]").count(), 1);
                } else {
                    let body: Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(body["choices"][0]["message"]["content"], "answer");
                    assert_eq!(body["choices"][0]["finish_reason"], "stop");
                    assert_eq!(body["usage"]["prompt_tokens"], 10);
                    assert_eq!(body["usage"]["completion_tokens"], 3);
                }
                let mut expected = json!({"model":"fixture-model","stream":true,
                    "messages":[{"role":"system","content":"You are a helpful assistant."},
                        {"role":"user","content":"question"}],
                    "temperature":0.2f32,"max_tokens":32,"stream_options":{"include_usage":true}});
                let thinking = control.get("thinking");
                let typed = thinking.is_some_and(|t| {
                    t.get("effort").is_some() || t.get("reasoning_effort").is_some()
                });
                if typed || control.get("reasoning_effort").is_some() {
                    expected["reasoning_effort"] = json!("high");
                }
                if let Some(opaque) = thinking.filter(|_| !typed) {
                    expected["thinking"] = opaque.clone();
                }
                assert_eq!(s.fixture.posts.load(Ordering::SeqCst), 1);
                assert_eq!(s.fixture.catalog_reads.load(Ordering::SeqCst), 1);
                assert_eq!(s.fixture.requests.lock().as_slice(), [expected]);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 28);
}

mod subject_receipt_tests;

#[tokio::test]
async fn ordinary_responses_account_ingress_rejects_lossy_workbuddy() {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let file_input = json!([{"role":"user","content":[
        {"type":"input_text","text":"owned question"},
        {"type":"input_file","file_id":"owned-file"}
    ]}]);
    let mut failures = Vec::new();
    for (region, label) in [
        (WorkBuddyRegion::Global, "global"),
        (WorkBuddyRegion::Cn, "cn"),
    ] {
        let s = setup_region(region).await;
        let counts = || {
            (
                s.fixture.posts.load(Ordering::SeqCst),
                s.fixture.catalog_reads.load(Ordering::SeqCst),
            )
        };
        let send = |model: &str, input: Value, stream| {
            http.post(format!("{}/v1/responses", s.url))
                .bearer_auth("offline-gateway-key")
                .json(&json!({"model":model,"input":input,"stream":stream}))
                .send()
        };
        for stream in [false, true] {
            let before = counts();
            let response = send("public", file_input.clone(), stream).await.unwrap();
            let status = response.status();
            let body = response.text().await.unwrap();
            let error = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
            let expected_error = error["error"]["type"] == "invalid_request_error"
                && error["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.ends_with("unsupported_account_content_part"));
            let after = counts();
            if status != reqwest::StatusCode::BAD_REQUEST || !expected_error || before != after {
                failures.push(format!(
                    "{label} stream={stream} input_file: status={}, counts={before:?}->{after:?}, expected_error={expected_error}",
                    status.as_u16()
                ));
            }
        }

        for stream in [false, true] {
            let before = counts();
            let response = send("public", json!("owned supported question"), stream)
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                reqwest::StatusCode::OK,
                "{label} legal stream={stream}"
            );
            let body = response.text().await.unwrap();
            assert!(body.contains("answer"), "{label} legal stream={stream}");
            if stream {
                assert!(body.contains("response.completed"), "{label} legal stream");
            } else {
                assert_eq!(
                    serde_json::from_str::<Value>(&body).unwrap()["status"],
                    "completed"
                );
            }
            assert_eq!(counts(), (before.0 + 1, 1), "{label} legal stream={stream}");
        }
        assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 0);

        let mut ordinary = s.gateway.providers.read()[0].clone();
        ordinary.id = "owned-responses-ordinary".into();
        ordinary.runtime_id = None;
        ordinary.base_url = format!("{}/v1", s.origin);
        ordinary.models[0].alias = "owned-responses-ordinary".into();
        repo::upsert_provider(s.gateway.db.pool(), &ordinary)
            .await
            .unwrap();
        s.gateway.reload_providers().await.unwrap();
        let before = counts();
        let response = send("owned-responses-ordinary", file_input.clone(), false)
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "{label} ordinary HTTP"
        );
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["status"], "completed");
        assert!(body.to_string().contains("backup answer"));
        assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 1);
        assert_eq!(
            counts(),
            before,
            "{label} ordinary HTTP never reaches the account"
        );
        assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
    }
    assert!(
        failures.is_empty(),
        "ordinary Responses WorkBuddy ingress: {failures:?}"
    );
}

#[tokio::test]
async fn desktop_usage_retains_only_reported_workbuddy_fields_in_both_regions() {
    use crate::desktop_chat::{self, ChatInput, ChatMessage, ReplyFieldState, ReplyUsageEvidence};

    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut mismatches = Vec::new();
    for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
        let s = setup_region(region).await;
        let mut cfg = s.gateway.cfg_snapshot();
        let listener = s.gateway.listener_snapshot().unwrap().bound_addr;
        let url = format!("{}/v1/chat/completions", s.url);
        let mut posts = 0;
        for (case, frames, expected) in [
            (
                "partial",
                vec![
                    json!({"prompt_tokens":12,"prompt_tokens_details":{"cached_tokens":0,"unknown":"fixture-private-usage"}}),
                    json!(null),
                    json!({"credit":0,"completion_tokens":-1,"total_tokens":"fixture-private-usage"}),
                ],
                Some(
                    json!({"prompt_tokens":12,"prompt_tokens_details":{"cached_tokens":0},"credit":0}),
                ),
            ),
            (
                "zero",
                vec![json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0})],
                Some(json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0})),
            ),
            (
                "invalid",
                vec![
                    json!({"prompt_tokens":4294967296u64,"completion_tokens":-1,"credit":"fixture-private-usage"}),
                ],
                None,
            ),
            ("missing", vec![], None),
        ] {
            *s.fixture.usage_frames.lock() = Some(frames);
            for stream in [false, true] {
                let input = ChatInput {
                    personal_test: false,
                    model: "public".into(),
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: "owned desktop usage probe".into(),
                        reasoning_content: None,
                    }],
                    session_id: uuid::Uuid::new_v4().to_string(),
                    max_tokens: 32,
                };
                let result = if stream {
                    desktop_chat::stream::send(&cfg, listener, input, uuid::Uuid::new_v4(), |_| {
                        Ok(())
                    })
                    .await
                } else {
                    desktop_chat::send(&cfg, listener, input).await
                }
                .unwrap();
                posts += 1;
                assert_eq!(s.fixture.posts.load(Ordering::SeqCst), posts);
                assert_eq!(result.content, "answer");
                assert_eq!(result.finish_reason.as_deref(), Some("stop"));
                let meta = result.response_metadata.unwrap();
                let evidence_matches = matches!(
                    (&expected, &meta.usage, &meta.usage_evidence),
                    (
                        Some(_),
                        ReplyFieldState::Reported,
                        Some(ReplyUsageEvidence::Unknown)
                    ) | (
                        None,
                        ReplyFieldState::Unavailable,
                        Some(ReplyUsageEvidence::Unobserved)
                    )
                );
                if result.usage != expected || !evidence_matches {
                    mismatches.push(format!(
                        "{region:?}/{case}/stream={stream}: usage={:?}, state={:?}, evidence={:?}",
                        result.usage, meta.usage, meta.usage_evidence
                    ));
                }
            }
            if case == "partial" {
                for stream in [false, true] {
                    let response = http
                        .post(&url)
                        .bearer_auth(&cfg.unified_key)
                        .json(&json!({"model":"public","stream":stream,
                            "messages":[{"role":"user","content":"ordinary compatibility control"}]}))
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(response.status(), 200);
                    let body = response.text().await.unwrap();
                    posts += 1;
                    assert_eq!(s.fixture.posts.load(Ordering::SeqCst), posts);
                    assert!(!body.contains("llmgw_reply_observation"));
                    assert!(!body.contains("fixture-private-usage"));
                    let expected = json!({"prompt_tokens":12,"completion_tokens":0,"total_tokens":12,"credit":0.0});
                    if stream {
                        let frames: Vec<_> = body
                            .lines()
                            .filter_map(|line| line.strip_prefix("data: "))
                            .collect();
                        assert_eq!(frames.last().copied(), Some("[DONE]"));
                        assert_eq!(frames.iter().filter(|frame| **frame == "[DONE]").count(), 1);
                        let usage: Vec<_> = frames[..frames.len() - 1]
                            .iter()
                            .map(|frame| serde_json::from_str::<Value>(frame).unwrap())
                            .filter_map(|value| value.get("usage").cloned())
                            .collect();
                        assert_eq!(usage, vec![expected]);
                    } else {
                        let body: Value = serde_json::from_str(&body).unwrap();
                        assert_eq!(body["usage"], expected);
                    }
                }
            }
        }
        assert_eq!(posts, 10);

        cfg.cache.enabled = true;
        s.gateway.update_cfg(cfg.clone());
        *s.fixture.usage_frames.lock() = Some(vec![json!({"prompt_tokens":12})]);
        let request = json!({"model":"public","stream":false,
            "messages":[{"role":"user","content":"owned cached usage probe"}]});
        for (cache, new_posts, private_usage, evidence) in [
            ("MISS", 1, json!({"prompt_tokens":12}), "unknown"),
            ("HIT", 0, Value::Null, "unobserved"),
        ] {
            let response = http
                .post(&url)
                .bearer_auth(&cfg.unified_key)
                .header("x-llmgw-desktop-observation", "1")
                .json(&request)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["x-cache"], cache);
            let body: Value = response.json().await.unwrap();
            posts += new_posts;
            assert_eq!(s.fixture.posts.load(Ordering::SeqCst), posts);
            assert_eq!(
                body["usage"],
                json!({"prompt_tokens":12,"completion_tokens":0,"total_tokens":12})
            );
            let observed = &body["llmgw_reply_observation"];
            assert_eq!(observed["version"], 1);
            assert_eq!(observed["finish_reason"], "stop");
            if observed.get("usage") != Some(&private_usage)
                || observed.get("usage_present").and_then(Value::as_bool)
                    != Some(!private_usage.is_null())
                || observed["usage_evidence"] != evidence
            {
                mismatches.push(format!("{region:?}/cache={cache}: {observed}"));
            }
        }
        assert_eq!(posts, 11);
        assert_eq!(s.fixture.catalog_reads.load(Ordering::SeqCst), 1);
        assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 0);
        assert_eq!(s.fixture.retryable_primary_posts.load(Ordering::SeqCst), 0);
        assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
    }
    assert!(
        mismatches.is_empty(),
        "desktop WorkBuddy usage must preserve upstream field presence: {mismatches:?}"
    );
}

pub(super) fn owned_ollama_image_messages() -> Value {
    // Synthetic 1x1 PNG; a real nonempty Ollama image field, not a malformed payload.
    const PNG: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC";
    json!([{"role":"user","content":"owned image question","images":[PNG]}])
}

pub(super) fn owned_ollama_text_cases() -> [(&'static str, Value); 3] {
    [
        (
            "text",
            json!([{"role":"user","content":"owned text question"}]),
        ),
        (
            "null_images",
            json!([{"role":"user","content":"owned text question","images":null}]),
        ),
        (
            "empty_images",
            json!([{"role":"user","content":"owned text question","images":[]}]),
        ),
    ]
}

pub(super) async fn assert_owned_ollama_reply(
    response: reqwest::Response,
    stream: bool,
    answer: &str,
    case: &str,
) {
    assert_eq!(response.status(), reqwest::StatusCode::OK, "{case}");
    if stream {
        assert!(
            response.headers()[reqwest::header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("application/x-ndjson"),
            "{case}"
        );
        let text = response.text().await.unwrap();
        let chunks: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("owned Ollama NDJSON"))
            .collect();
        let (terminal, progress) = chunks.split_last().expect("native stream terminator");
        assert_eq!(terminal["done"], true, "{case}");
        assert!(
            chunks.iter().all(|chunk| chunk.get("error").is_none()),
            "{case}"
        );
        assert!(
            progress.iter().all(|chunk| chunk["done"] == false),
            "{case}"
        );
        let content = progress
            .iter()
            .map(|chunk| chunk["message"]["content"].as_str().unwrap_or_default())
            .collect::<String>();
        assert_eq!(content, answer, "{case}");
    } else {
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["done"], true, "{case}");
        assert_eq!(body["message"]["content"], answer, "{case}");
        assert!(body.get("error").is_none(), "{case}");
    }
}

#[tokio::test]
async fn ollama_account_images_ingress_rejects_workbuddy() {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let images = owned_ollama_image_messages();
    let mut failures = Vec::new();
    for (region, label) in [
        (WorkBuddyRegion::Global, "global"),
        (WorkBuddyRegion::Cn, "cn"),
    ] {
        let s = setup_region(region).await;
        let counts = || {
            (
                s.fixture.posts.load(Ordering::SeqCst),
                s.fixture.catalog_reads.load(Ordering::SeqCst),
            )
        };
        let send = |model: &str, messages: &Value, stream| {
            http.post(format!("{}/api/chat", s.url))
                .bearer_auth("offline-gateway-key")
                .json(&json!({"model":model,"messages":messages,"stream":stream}))
                .send()
        };
        for stream in [false, true] {
            let before = counts();
            let response = send("public", &images, stream).await.unwrap();
            let status = response.status();
            let wire = response.text().await.unwrap();
            let error = serde_json::from_str::<Value>(&wire).unwrap_or(Value::Null);
            let expected_error = error["error"]["type"] == "invalid_request_error"
                && error["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.ends_with("unsupported_account_content_part"));
            let after = counts();
            if status != reqwest::StatusCode::BAD_REQUEST || !expected_error || before != after {
                failures.push(format!(
                    "{label} stream={stream} nonempty_images: status={}, counts={before:?}->{after:?}, expected_error={expected_error}",
                    status.as_u16()
                ));
            }
        }

        for (shape, messages) in owned_ollama_text_cases() {
            for stream in [false, true] {
                let before = counts();
                let response = send("public", &messages, stream).await.unwrap();
                let case = format!("WorkBuddy {label} {shape} stream={stream}");
                assert_owned_ollama_reply(response, stream, "answer", &case).await;
                assert_eq!(counts(), (before.0 + 1, 1), "{case}");
            }
        }
        // Keep the same image-bearing body on an ordinary, explicitly selected HTTP route.
        assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 0);
        if region == WorkBuddyRegion::Global {
            let mut ordinary = s.gateway.providers.read()[0].clone();
            ordinary.id = "owned-ollama-ordinary".into();
            ordinary.runtime_id = None;
            ordinary.base_url = format!("{}/v1", s.origin);
            ordinary.models[0].alias = "owned-ollama-ordinary".into();
            repo::upsert_provider(s.gateway.db.pool(), &ordinary)
                .await
                .unwrap();
            s.gateway.reload_providers().await.unwrap();
            let before = counts();
            let response = send("owned-ollama-ordinary", &images, false).await.unwrap();
            assert_owned_ollama_reply(response, false, "backup answer", "ordinary HTTP").await;
            assert_eq!(counts(), before, "ordinary HTTP never reaches the account");
            assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 1);
        }
        assert_eq!(s.fixture.retryable_primary_posts.load(Ordering::SeqCst), 0);
        assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
    }
    assert!(
        failures.is_empty(),
        "Ollama WorkBuddy account images ingress: {failures:?}"
    );
}

fn owned_anthropic_document_block() -> Value {
    json!({"type":"document","source":{
        "type":"text","media_type":"text/plain","data":"owned document"
    }})
}

fn owned_anthropic_document_messages(tool_result: bool, with_document: bool) -> Value {
    let mut blocks = vec![json!({
        "type":"text",
        "text":if tool_result { "owned lookup result" } else { "owned direct question" }
    })];
    if with_document {
        blocks.push(owned_anthropic_document_block());
    }
    if tool_result {
        json!([
            {"role":"user","content":"owned lookup question"},
            {"role":"assistant","content":[{
                "type":"tool_use","id":"toolu_owned_doc","name":"lookup","input":{"q":"owned"}
            }]},
            {"role":"user","content":[{
                "type":"tool_result","tool_use_id":"toolu_owned_doc","content":blocks
            }]}
        ])
    } else {
        json!([{"role":"user","content":blocks}])
    }
}

async fn post_owned_anthropic_document_case(
    (http, gateway, key): (&reqwest::Client, &str, &str),
    model: &str,
    messages: &Value,
    stream: bool,
    shape: &str,
) -> reqwest::Response {
    http.post(format!("{gateway}/v1/messages"))
        .bearer_auth(key)
        .header(
            "x-session-id",
            format!("owned-r138-{model}-{shape}-{stream}"),
        )
        .json(&json!({"model":model,"stream":stream,"max_tokens":32,"messages":messages}))
        .send()
        .await
        .unwrap()
}

pub(super) fn assert_owned_anthropic_document_projection(
    request: &Value,
    tool_result: bool,
    case: &str,
) {
    let messages = request["messages"].as_array().unwrap();
    if tool_result {
        let assistant = messages
            .iter()
            .find(|row| row["role"] == "assistant")
            .unwrap();
        let calls = assistant["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1, "{case}");
        let call = &calls[0];
        assert_eq!(call["id"], "toolu_owned_doc", "{case}");
        assert_eq!(call["function"]["name"], "lookup", "{case}");
        let args: Value =
            serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args, json!({"q":"owned"}), "{case}");
        let results = messages
            .iter()
            .filter(|row| row["role"] == "tool")
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1, "{case}");
        assert_eq!(results[0]["tool_call_id"], "toolu_owned_doc", "{case}");
        assert_eq!(results[0]["content"], "owned lookup result", "{case}");
    } else {
        assert!(
            messages
                .iter()
                .any(|row| row["role"] == "user" && row["content"] == "owned direct question"),
            "{case}"
        );
    }
}

async fn assert_owned_anthropic_document_reply(
    response: reqwest::Response,
    stream: bool,
    answer: &str,
    case: &str,
) {
    let status = response.status();
    let mime = response.headers()[reqwest::header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .to_owned();
    let body = response.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK, "{case}: {body}");
    if stream {
        assert!(mime.starts_with("text/event-stream"), "{case}: {mime}");
        let mut events = Vec::new();
        for frame in body.split("\n\n").filter(|frame| !frame.trim().is_empty()) {
            let mut lines = frame.lines();
            let Some(event) = lines.next().and_then(|line| line.strip_prefix("event: ")) else {
                assert!(frame.starts_with(':'), "{case}: unexpected frame {frame:?}");
                continue;
            };
            let data = lines
                .next()
                .and_then(|line| line.strip_prefix("data: "))
                .unwrap();
            let data: Value = serde_json::from_str(data).unwrap();
            assert_eq!(data["type"], event, "{case}: {frame}");
            assert_ne!(event, "error", "{case}: {frame}");
            events.push((event, data));
        }
        assert_eq!(
            events.first().map(|event| event.0),
            Some("message_start"),
            "{case}"
        );
        assert_eq!(
            events.last().map(|event| event.0),
            Some("message_stop"),
            "{case}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.0 == "message_stop")
                .count(),
            1,
            "{case}"
        );
        let text = events
            .iter()
            .filter(|(event, data)| {
                *event == "content_block_delta" && data["delta"]["type"] == "text_delta"
            })
            .map(|(_, data)| data["delta"]["text"].as_str().unwrap())
            .collect::<String>();
        assert_eq!(text, answer, "{case}");
    } else {
        assert!(mime.starts_with("application/json"), "{case}: {mime}");
        let message: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(message["type"], "message", "{case}");
        assert_eq!(message["role"], "assistant", "{case}");
        assert!(message.get("error").is_none(), "{case}");
        let text = message["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| block["type"] == "text")
            .map(|block| block["text"].as_str().unwrap())
            .collect::<String>();
        assert_eq!(text, answer, "{case}");
    }
}

pub(super) async fn check_owned_anthropic_document_ingress(
    endpoint: (&reqwest::Client, &str, &str),
    adapter: &str,
    answer: &str,
    counts: impl Fn() -> (usize, usize, Option<usize>),
    assert_projection: impl Fn(bool, &str),
    failures: &mut Vec<String>,
) {
    // Negative cases precede every good dispatch in each fresh region. The
    // tuple records physical account POST / catalog GET / optional HTTP backup.
    assert_eq!(counts().0, 0, "{adapter}");
    assert_eq!(counts().1, 0, "{adapter}");
    let direct = owned_anthropic_document_messages(false, true);
    let tool_result = owned_anthropic_document_messages(true, true);
    let mut sibling = owned_anthropic_document_messages(true, false);
    sibling[2]["content"]
        .as_array_mut()
        .unwrap()
        .push(owned_anthropic_document_block());
    for stream in [false, true] {
        for (shape, messages, expected_status, error_type, reason) in [
            (
                "direct-document",
                &direct,
                400,
                "invalid_request_error",
                "unsupported_account_content_part",
            ),
            (
                "tool-result-document",
                &tool_result,
                400,
                "invalid_request_error",
                "unsupported_account_content_part",
            ),
            (
                "sibling-document",
                &sibling,
                500,
                "upstream_error",
                "unsupported user content block `document` alongside tool_result",
            ),
        ] {
            let before = counts();
            let response =
                post_owned_anthropic_document_case(endpoint, "public", messages, stream, shape)
                    .await;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body = response.text().await.unwrap();
            let json_error = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
            let json_header = headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("application/json"));
            let matches_error = json_header
                && json_error["error"]["type"] == error_type
                && json_error["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.ends_with(reason))
                && !headers.contains_key("x-routed-via")
                && !headers.contains_key("x-cache");
            let after = counts();
            let witness = format!(
                "R138 {adapter} {shape} stream={stream} status={status} physical(post,catalog,backup)={before:?}->{after:?} expected_json_error={matches_error}"
            );
            eprintln!("{witness}");
            if status != expected_status || !matches_error || before != after {
                failures.push(witness);
            }
        }
    }

    for stream in [false, true] {
        for (tool_result, control) in [(false, "typed-text"), (true, "legal-tool-history")] {
            let case = format!("{adapter}/{control} stream={stream}");
            let before = counts();
            let messages = owned_anthropic_document_messages(tool_result, false);
            let response =
                post_owned_anthropic_document_case(endpoint, "public", &messages, stream, control)
                    .await;
            assert_owned_anthropic_document_reply(response, stream, answer, &case).await;
            assert_eq!(counts(), (before.0 + 1, 1, before.2), "{case}");
            assert_projection(tool_result, &case);
            eprintln!(
                "R138 {case} passed; physical(post,catalog,backup)={before:?}->{:?}",
                counts()
            );
        }
    }
}

#[tokio::test]
async fn anthropic_account_document_ingress_rejects_workbuddy() {
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let mut failures = Vec::new();
    for (region, label) in [
        (WorkBuddyRegion::Global, "global"),
        (WorkBuddyRegion::Cn, "cn"),
    ] {
        let s = setup_region(region).await;
        let mut backup = s.gateway.providers.read()[0].clone();
        backup.id = "owned-anthropic-http".into();
        backup.runtime_id = None;
        backup.base_url = format!("{}/v1", s.origin);
        backup.priority = 2;
        let mut explicit_http_model = backup.models[0].clone();
        explicit_http_model.alias = "owned-anthropic-http".into();
        backup.models.push(explicit_http_model);
        repo::upsert_provider(s.gateway.db.pool(), &backup)
            .await
            .unwrap();
        s.gateway.reload_providers().await.unwrap();
        let mut cfg = s.gateway.cfg_snapshot();
        cfg.routing_strategy = crate::config::RoutingStrategy::Priority;
        cfg.failover_enabled = true;
        cfg.max_fallback_attempts = 2;
        cfg.smart_routing.enabled = false;
        cfg.search.enabled = false;
        cfg.cache.enabled = false;
        cfg.dedup.mode = crate::proxy::dedup::DedupMode::Off;
        s.gateway.update_cfg(cfg);
        s.gateway.set_active("wb-gateway-provider").unwrap();

        let counts = || {
            (
                s.fixture.posts.load(Ordering::SeqCst),
                s.fixture.catalog_reads.load(Ordering::SeqCst),
                Some(s.fixture.backup_posts.load(Ordering::SeqCst)),
            )
        };
        let endpoint = (&http, s.url.as_str(), "offline-gateway-key");
        check_owned_anthropic_document_ingress(
            endpoint,
            &format!("WorkBuddy/{label}"),
            "answer",
            counts,
            |tool_result, case| {
                let requests = s.fixture.requests.lock();
                assert_owned_anthropic_document_projection(
                    requests.last().unwrap(),
                    tool_result,
                    case,
                );
            },
            &mut failures,
        )
        .await;
        assert_eq!(counts().2, Some(0), "no fallback for the account cases");
        if region == WorkBuddyRegion::Global {
            let before = counts();
            let reply = post_owned_anthropic_document_case(
                endpoint,
                "owned-anthropic-http",
                &owned_anthropic_document_messages(false, true),
                false,
                "direct-document",
            )
            .await;
            assert_owned_anthropic_document_reply(
                reply,
                false,
                "backup answer",
                "ordinary HTTP exact body",
            )
            .await;
            assert_eq!(counts(), (before.0, before.1, Some(1)));
            eprintln!("R138 ordinary HTTP exact document passed; physical(post,catalog,backup)={before:?}->{:?}", counts());
        }
        assert_eq!(s.fixture.retryable_primary_posts.load(Ordering::SeqCst), 0);
        assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
    }
    assert!(
        failures.is_empty(),
        "Anthropic WorkBuddy account document ingress:\n{}",
        failures.join("\n")
    );
}

type OwnedGeminiRequests = Arc<parking_lot::Mutex<Vec<(bool, Value)>>>;

pub(super) struct OwnedGeminiHttp {
    requests: OwnedGeminiRequests,
    job: tokio::task::JoinHandle<()>,
}

impl OwnedGeminiHttp {
    pub(super) fn posts(&self) -> usize {
        self.requests.lock().len()
    }
}

impl Drop for OwnedGeminiHttp {
    fn drop(&mut self) {
        self.job.abort();
    }
}

async fn owned_gemini_http_reply(
    State(requests): State<OwnedGeminiRequests>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let stream = uri.path().ends_with(":streamGenerateContent");
    let wire = serde_json::from_slice(&body).unwrap_or(Value::Null);
    requests.lock().push((stream, wire));
    assert_eq!(method, axum::http::Method::POST);
    assert_eq!(
        uri.path(),
        if stream {
            "/v1beta/models/owned-gemini-model:streamGenerateContent"
        } else {
            "/v1beta/models/owned-gemini-model:generateContent"
        },
    );
    assert_eq!(uri.query(), if stream { Some("alt=sse") } else { None });

    let text =
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"ordinary answer"}]}}]});
    let usage = json!({"promptTokenCount":4,"candidatesTokenCount":2,"totalTokenCount":6});
    if stream {
        let end = json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":usage});
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {text}\n\ndata: {end}\n\n"),
        )
            .into_response()
    } else {
        let mut reply = text;
        reply["candidates"][0]["finishReason"] = json!("STOP");
        reply["usageMetadata"] = usage;
        Json(reply).into_response()
    }
}

pub(super) async fn start_owned_gemini_http(
    state: &GatewayState,
    account_provider: &str,
    same_alias_backup: bool,
) -> OwnedGeminiHttp {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = listener.local_addr().unwrap();
    let requests = OwnedGeminiRequests::default();
    let router = Router::new()
        .fallback(owned_gemini_http_reply)
        .with_state(requests.clone());
    let job = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let fixture = OwnedGeminiHttp { requests, job };

    let mut provider = state
        .providers
        .read()
        .iter()
        .find(|provider| provider.id == account_provider)
        .unwrap()
        .clone();
    provider.id = "owned-gemini-http".into();
    provider.name = "Owned Gemini HTTP".into();
    provider.runtime_id = None;
    provider.dialect = Dialect::Gemini;
    provider.base_url = format!("http://{origin}/v1beta");
    provider.api_key_enc.clear();
    provider.priority = 2;
    provider.models[0].upstream = "owned-gemini-model".into();
    let mut explicit = provider.models[0].clone();
    explicit.alias = "owned-gemini-http".into();
    if !same_alias_backup {
        provider.models.clear();
    }
    provider.models.push(explicit);
    repo::upsert_provider(state.db.pool(), &provider)
        .await
        .unwrap();
    state.reload_providers().await.unwrap();
    let mut cfg = state.cfg_snapshot();
    cfg.routing_strategy = crate::config::RoutingStrategy::Priority;
    cfg.failover_enabled = true;
    cfg.max_fallback_attempts = 2;
    cfg.smart_routing.enabled = false;
    cfg.search.enabled = false;
    cfg.cache.enabled = false;
    cfg.dedup.mode = crate::proxy::dedup::DedupMode::Off;
    state.update_cfg(cfg);
    state.set_active(account_provider).unwrap();
    fixture
}

fn owned_gemini_code_part_cases() -> [(&'static str, Value); 2] {
    [
        (
            "executableCode",
            json!({"contents":[{"role":"user","parts":[
                {"text":"owned code history"},
                {"executableCode":{"language":"PYTHON","code":"1 + 2"}}
            ]}]}),
        ),
        (
            "codeExecutionResult",
            json!({"contents":[{"role":"user","parts":[
                {"text":"owned code history"},
                {"codeExecutionResult":{"outcome":"OUTCOME_OK","output":"3"}}
            ]}]}),
        ),
    ]
}

type OwnedGeminiEndpoint<'a> = (&'a reqwest::Client, &'a str, &'a str);

async fn post_owned_gemini(
    (client, endpoint, auth): OwnedGeminiEndpoint<'_>,
    model: &str,
    input: &Value,
    stream: bool,
    label: &str,
) -> reqwest::Response {
    let action = if stream {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    };
    client
        .post(format!("{endpoint}/v1beta/models/{model}:{action}"))
        .bearer_auth(auth)
        .header(
            "x-session-id",
            format!("owned-r140-{model}-{label}-{stream}"),
        )
        .json(input)
        .send()
        .await
        .unwrap()
}

async fn assert_owned_gemini_reply(
    response: reqwest::Response,
    stream: bool,
    expected: &str,
    case: &str,
) {
    assert_eq!(response.status(), reqwest::StatusCode::OK, "{case}");
    let mime = response.headers()[reqwest::header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .to_owned();
    let wire = response.text().await.unwrap();
    let frames = if stream {
        assert!(mime.starts_with("text/event-stream"), "{case}: {mime}");
        assert!(
            !wire.contains("[DONE]"),
            "{case}: Gemini does not use the OpenAI sentinel"
        );
        wire.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|data| serde_json::from_str::<Value>(data.trim()).unwrap())
            .collect::<Vec<_>>()
    } else {
        assert!(mime.starts_with("application/json"), "{case}: {mime}");
        vec![serde_json::from_str::<Value>(&wire).unwrap()]
    };
    assert!(
        frames
            .iter()
            .all(|frame| frame.get("error").is_none() && frame.get("choices").is_none()),
        "{case}"
    );
    let candidates = frames
        .iter()
        .map(|frame| &frame["candidates"][0])
        .collect::<Vec<_>>();
    assert_eq!(candidates.last().unwrap()["finishReason"], "STOP", "{case}");
    assert_eq!(
        candidates
            .iter()
            .filter(|candidate| candidate.get("finishReason").is_some())
            .count(),
        1,
        "{case}"
    );
    let visible = candidates
        .iter()
        .flat_map(|candidate| {
            candidate["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .filter(|part| part["thought"] != true)
        .filter_map(|part| part["text"].as_str())
        .collect::<String>();
    assert_eq!(visible, expected, "{case}");
}

pub(super) fn assert_owned_gemini_account_projection(wire: &Value, history: bool, case: &str) {
    let messages = wire["messages"].as_array().unwrap();
    if history {
        let call = messages
            .iter()
            .find_map(|message| {
                message["tool_calls"]
                    .as_array()
                    .and_then(|calls| calls.first())
            })
            .unwrap();
        assert_eq!(call["id"], "owned-gemini-call", "{case}");
        assert_eq!(call["function"]["name"], "lookup", "{case}");
        assert_eq!(
            serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap()).unwrap(),
            json!({"query":"owned"}),
            "{case}"
        );
        let result = messages
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        assert_eq!(result["tool_call_id"], "owned-gemini-call", "{case}");
        assert_eq!(
            serde_json::from_str::<Value>(result["content"].as_str().unwrap()).unwrap(),
            json!({"kept":"tool output"}),
            "{case}"
        );
    } else {
        let user = messages
            .iter()
            .find(|message| message["role"] == "user")
            .unwrap();
        assert_eq!(user["content"], "owned null controls", "{case}");
    }
}

pub(super) async fn check_owned_gemini_code_ingress(
    endpoint: OwnedGeminiEndpoint<'_>,
    adapter: &str,
    account_answer: &str,
    counts: impl Fn() -> (usize, usize, usize),
    assert_projection: impl Fn(bool, &str),
    failures: &mut Vec<String>,
) {
    assert_eq!(counts(), (0, 0, 0), "{adapter}");
    for stream in [false, true] {
        for (shape, input) in owned_gemini_code_part_cases() {
            let before = counts();
            let reply = post_owned_gemini(endpoint, "public", &input, stream, shape).await;
            let status = reply.status().as_u16();
            let headers = reply.headers().clone();
            let body = reply.text().await.unwrap();
            let error = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
            let expected_error = headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("application/json"))
                && !headers.contains_key("x-routed-via")
                && !headers.contains_key("x-cache")
                && error["error"]["type"] == "invalid_request_error"
                && error["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.ends_with("unsupported_account_content_part"));
            let after = counts();
            let witness = format!("R140 {adapter} {shape} stream={stream} status={status} physical(post,catalog,backup)={before:?}->{after:?} expected_json_error={expected_error}");
            eprintln!("{witness}");
            if status != 400 || !expected_error || before != after {
                failures.push(witness);
            }
        }
    }

    let mut malformed = owned_gemini_code_part_cases()[0].1.clone();
    malformed["contents"][0]["parts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"functionCall":{"name":"lookup"}}));
    let with_null = json!({"contents":[{"role":"user","parts":[
        {"text":"owned null controls","thoughtSignature":"owned opaque signature","ownedUnknown":{"keep":"compatibility"}},
        {"executableCode":null},{"codeExecutionResult":null}
    ]}],"generationConfig":{"maxOutputTokens":32}});
    let history = json!({"contents":[
        {"role":"user","parts":[{"text":"look up owned data"}]},
        {"role":"model","parts":[{"functionCall":{"id":"owned-gemini-call","name":"lookup","args":{"query":"owned"}}}]},
        {"role":"user","parts":[{"functionResponse":{"id":"owned-gemini-call","name":"lookup","response":{"kept":"tool output"}}}]}
    ]});
    for stream in [false, true] {
        let before = counts();
        let reply = post_owned_gemini(
            endpoint,
            "public",
            &malformed,
            stream,
            "invalid-user-function-call",
        )
        .await;
        assert_eq!(reply.status(), 500, "{adapter} stream={stream}");
        let error: Value = serde_json::from_str(&reply.text().await.unwrap()).unwrap();
        assert_eq!(error["error"]["type"], "upstream_error");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("functionCall") && message.contains("model"),
            "{message}"
        );
        assert_eq!(counts(), before, "{adapter} stream={stream}");
        eprintln!("R140 {adapter} invalid-user-function-call stream={stream} status=500 physical(post,catalog,backup)={before:?}->{:?}", counts());

        for (is_history, label, input) in [
            (false, "null-and-metadata", &with_null),
            (true, "legal-tool-history", &history),
        ] {
            let before = counts();
            let case = format!("{adapter}/{label} stream={stream}");
            let reply = post_owned_gemini(endpoint, "public", input, stream, label).await;
            assert_owned_gemini_reply(reply, stream, account_answer, &case).await;
            assert_eq!(counts(), (before.0 + 1, 1, before.2), "{case}");
            assert_projection(is_history, &case);
            eprintln!(
                "R140 {case} passed; physical(post,catalog,backup)={before:?}->{:?}",
                counts()
            );
        }
    }
}

#[tokio::test]
async fn gemini_account_code_parts_ingress_rejects_workbuddy() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let mut failures = Vec::new();
    for (region, label) in [
        (WorkBuddyRegion::Global, "global"),
        (WorkBuddyRegion::Cn, "cn"),
    ] {
        let s = setup_region(region).await;
        let ordinary = start_owned_gemini_http(&s.gateway, "wb-gateway-provider", true).await;
        let counts = || {
            (
                s.fixture.posts.load(Ordering::SeqCst),
                s.fixture.catalog_reads.load(Ordering::SeqCst),
                ordinary.posts(),
            )
        };
        let endpoint = (&client, s.url.as_str(), "offline-gateway-key");
        check_owned_gemini_code_ingress(
            endpoint,
            &format!("WorkBuddy/{label}"),
            "answer",
            counts,
            |history, case| {
                let requests = s.fixture.requests.lock();
                assert_owned_gemini_account_projection(requests.last().unwrap(), history, case);
            },
            &mut failures,
        )
        .await;

        assert_eq!(counts().2, 0);
        if region == WorkBuddyRegion::Global {
            for stream in [false, true] {
                for (shape, input) in owned_gemini_code_part_cases() {
                    let before = counts();
                    let case = format!("ordinary HTTP {shape} stream={stream}");
                    let reply =
                        post_owned_gemini(endpoint, "owned-gemini-http", &input, stream, shape)
                            .await;
                    assert_owned_gemini_reply(reply, stream, "ordinary answer", &case).await;
                    assert_eq!(counts(), (before.0, before.1, before.2 + 1), "{case}");
                    let requests = ordinary.requests.lock();
                    let (stream_on_wire, wire) = requests.last().unwrap();
                    assert_eq!(*stream_on_wire, stream, "{case}");
                    assert_eq!(
                        wire["contents"][0]["parts"],
                        json!([{"text":"owned code history"}]),
                        "{case}"
                    );
                    drop(requests);
                    eprintln!(
                        "R140 {case} passed; physical(post,catalog,backup)={before:?}->{:?}",
                        counts()
                    );
                }
            }
        }
        assert_eq!(s.fixture.backup_posts.load(Ordering::SeqCst), 0);
        assert_eq!(s.fixture.retryable_primary_posts.load(Ordering::SeqCst), 0);
        assert_eq!(s.fixture.unexpected.load(Ordering::SeqCst), 0);
    }
    assert!(
        failures.is_empty(),
        "R140 WorkBuddy Gemini ingress:\n{}",
        failures.join("\n")
    );
}
