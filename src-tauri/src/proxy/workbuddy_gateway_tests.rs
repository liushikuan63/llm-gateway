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
