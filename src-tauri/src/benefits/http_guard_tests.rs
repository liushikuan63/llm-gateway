//! Actual center -> HTTP adapter -> owned loopback. No client or supplier calls.
use super::*;
use crate::{benefit_config::BenefitAccount, client_session::SessionCredential, db::Db};
use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Default)]
struct Upstream {
    statuses: Arc<AtomicUsize>,
    claims: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    pause_second: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
struct OwnedServer {
    endpoint: String,
    upstream: Upstream,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for OwnedServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server() -> OwnedServer {
    let upstream = Upstream::default();
    let routes = Router::new()
        .route("/trae/api/v2/ug/checkin_credits/status", post(|State(s): State<Upstream>, headers: HeaderMap| async move {
            assert!(headers["authorization"].to_str().unwrap().starts_with("Cloud-IDE-JWT synthetic-"));
            let n=s.statuses.fetch_add(1,Ordering::SeqCst);
            if n==1 && s.pause_second.load(Ordering::SeqCst) {
                s.entered.notify_one();
                s.release.notified().await;
            }
            Json(json!({"code":0,"enable":true,"checked_in":s.claims.load(Ordering::SeqCst)>0 && !s.fail.load(Ordering::SeqCst),"credits":8}))
        }))
        .route("/trae/api/v2/ug/checkin_credits/claim", post(|State(s): State<Upstream>, headers: HeaderMap| async move {
            assert_eq!(headers["x-device-id"],"1234567890123456");
            s.claims.fetch_add(1,Ordering::SeqCst);
            let status=if s.fail.load(Ordering::SeqCst){axum::http::StatusCode::SERVICE_UNAVAILABLE}else{axum::http::StatusCode::OK};
            (status,Json(json!({"code":0})))
        }))
        .with_state(upstream.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    OwnedServer {
        endpoint,
        upstream,
        task,
    }
}
fn config() -> BenefitsConfig {
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
    );
    BenefitsConfig {
        enabled: true,
        accounts: vec![BenefitAccount {
            id: "owned".into(),
            platform: "trae_cn".into(),
            label: "Owned".into(),
            enabled: true,
        }],
        ..Default::default()
    }
}
fn center(db: &Db, server: &OwnedServer) -> BenefitsCenter {
    BenefitsCenter::with_transport(
        db.pool().clone(),
        Arc::new(HttpBenefitTransport {
            mock_endpoint: Some(server.endpoint.clone()),
        }),
    )
}
fn session(device: bool, access: &str) -> SessionCredential {
    let mut value = json!({"schema_version":1,"platform":"trae_cn","request_source":1,"headers":{"authorization":format!("Cloud-IDE-JWT {access}")},"snapshot":{"identity_hash":"f".repeat(64)}});
    if device {
        value["headers"]["x-device-id"] = json!("1234567890123456");
    }
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn legacy_without_device_is_observable_but_requires_explicit_claim_migration() {
    for restored_old_record in [true, false] {
        let cfg = config();
        let db = Db::connect_in_memory().await.unwrap();
        let server = server().await;
        let center = center(&db, &server);
        let saved = session(false, "synthetic-legacy");
        if restored_old_record {
            repo::set_secret(
                db.pool(),
                "benefit:trae_cn:owned:token",
                &crypto::encrypt(&serde_json::to_string(&saved).unwrap()).unwrap(),
            )
            .await
            .unwrap();
        } else {
            center.set_session(&cfg, "owned", &saved).await.unwrap();
        }
        let original = repo::get_secret(db.pool(), "benefit:trae_cn:owned:token")
            .await
            .unwrap();
        let overview = center.overview(&cfg, None).await.unwrap();
        assert!(
            overview.accounts[0].status.is_some(),
            "readonly status remains supported"
        );
        let result = center.claim_now(&cfg, None, "owned").await.unwrap();
        assert_eq!(result.run.verdict, BenefitVerdict::Error);
        assert_eq!(
            server.upstream.claims.load(Ordering::SeqCst),
            0,
            "no HTTP mutation or client fallback"
        );
        assert_eq!(server.upstream.statuses.load(Ordering::SeqCst), 2);
        assert_eq!(
            repo::get_secret(db.pool(), "benefit:trae_cn:owned:token")
                .await
                .unwrap(),
            original
        );
        assert!(
            result.run.message.contains("独立登录"),
            "rejection must explain migration: {}",
            result.run.message
        );
        assert!(!overview.accounts[0].capabilities.manual_claim);
        assert!(!overview.accounts[0].capabilities.auto_claim);
        assert!(overview.accounts[0].capabilities.scope.contains("缺少"));
    }
}

#[tokio::test]
async fn native_complete_device_succeeds_and_http_failure_never_reposts_or_falls_back() {
    for failure in [false, true] {
        let cfg = config();
        let db = Db::connect_in_memory().await.unwrap();
        let server = server().await;
        server.upstream.fail.store(failure, Ordering::SeqCst);
        source_tests::synthetic_native(
            &db,
            "owned",
            "owned-native",
            "synthetic-native",
            Utc::now().timestamp_millis() + 60_000,
            true,
        )
        .await;
        let center = center(&db, &server);
        let result = center.claim_now(&cfg, None, "owned").await.unwrap();
        assert_eq!(
            result.run.verdict,
            if failure {
                BenefitVerdict::Error
            } else {
                BenefitVerdict::Granted
            }
        );
        assert_eq!(server.upstream.claims.load(Ordering::SeqCst), 1);
        assert_eq!(
            server.upstream.statuses.load(Ordering::SeqCst),
            if failure { 2 } else { 3 }
        );
        assert_eq!(
            center
                .claim_now(&cfg, None, "owned")
                .await
                .unwrap()
                .run
                .verdict,
            BenefitVerdict::Skipped
        );
        assert_eq!(
            server.upstream.claims.load(Ordering::SeqCst),
            1,
            "failure and repeated UI intent cannot make a fallback mutation"
        );
    }
}

#[tokio::test]
async fn saved_source_change_inside_actual_http_adapter_sends_zero_old_mutations() {
    let cfg = config();
    let db = Db::connect_in_memory().await.unwrap();
    let server = server().await;
    server.upstream.pause_second.store(true, Ordering::SeqCst);
    let center = Arc::new(center(&db, &server));
    center
        .set_session(&cfg, "owned", &session(true, "synthetic-old"))
        .await
        .unwrap();
    let pending = tokio::spawn({
        let center = center.clone();
        let cfg = cfg.clone();
        async move { center.claim_now(&cfg, None, "owned").await }
    });
    tokio::time::timeout(Duration::from_secs(3), server.upstream.entered.notified())
        .await
        .unwrap();
    let other = BenefitsCenter::new(db.pool().clone());
    other
        .set_session(&cfg, "owned", &session(true, "synthetic-new"))
        .await
        .unwrap();
    server.upstream.release.notify_one();
    let result = pending.await.unwrap().unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Error);
    assert_eq!(result.run.message, source::error("source_changed"));
    assert_eq!(server.upstream.claims.load(Ordering::SeqCst), 0);
    assert_eq!(
        center
            .claim_now(&cfg, None, "owned")
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Granted,
        "new saved source positive control still works"
    );
    assert_eq!(server.upstream.claims.load(Ordering::SeqCst), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_run_principals")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0,
        "a saved source epoch never manufactures UID authority"
    );
}

#[tokio::test]
async fn legacy_refusal_then_native_binding_does_not_unlock_the_unattributed_same_window() {
    let cfg = config();
    let db = Db::connect_in_memory().await.unwrap();
    let server = server().await;
    let center = center(&db, &server);
    let uid = source_tests::synthetic_native(
        &db,
        "owned",
        "owned-native",
        "synthetic-native",
        Utc::now().timestamp_millis() + 60_000,
        false,
    )
    .await;
    let mut legacy = session(false, "synthetic-legacy");
    center.set_session(&cfg, "owned", &legacy).await.unwrap();
    let mismatched_ciphertext = repo::get_secret(db.pool(), "benefit:trae_cn:owned:token")
        .await
        .unwrap();
    let mismatched = center.benefit_auth_source(&cfg, "owned").await.unwrap();
    assert_eq!(
        center
            .bind_trae_benefit_auth(&cfg, None, "owned", &mismatched.source_revision, &uid)
            .await
            .unwrap_err(),
        source::error("principal_mismatch"),
        "a known conflicting hash must still reject before the unknown-subject scenario"
    );
    assert_eq!(server.upstream.statuses.load(Ordering::SeqCst), 0);
    assert_eq!(server.upstream.claims.load(Ordering::SeqCst), 0);
    assert_eq!(
        repo::get_secret(db.pool(), "benefit:trae_cn:owned:token")
            .await
            .unwrap(),
        mismatched_ciphertext
    );
    // Establish the intended unknown-subject input BEFORE any old run/owner.
    // This is fixture preparation, never retroactive UID attribution.
    legacy.snapshot.identity_hash = None;
    center.set_session(&cfg, "owned", &legacy).await.unwrap();
    assert!(center
        .benefit_auth_source(&cfg, "owned")
        .await
        .unwrap()
        .identity
        .and_then(|identity| identity.identity_hash)
        .is_none());
    let original_source = repo::get_secret(db.pool(), "benefit:trae_cn:owned:token")
        .await
        .unwrap();
    let refused = center.claim_now(&cfg, None, "owned").await.unwrap();
    assert_eq!(refused.run.verdict, BenefitVerdict::Error);
    assert_eq!(server.upstream.claims.load(Ordering::SeqCst), 0);
    let old_owner: (String, String) = sqlx::query_as(
        "SELECT name,value_enc FROM app_secrets WHERE name GLOB 'benefit:reservation:v1:*'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let before = center.benefit_auth_source(&cfg, "owned").await.unwrap();
    let bound = center
        .bind_trae_benefit_auth(&cfg, None, "owned", &before.source_revision, &uid)
        .await
        .unwrap();
    assert_eq!(bound.source, "native_login");
    assert!(bound.can_restore && bound.previous_present);
    assert_eq!(
        center.claim_now(&cfg, None, "owned").await.unwrap_err(),
        "旧领取预约没有可信主体归属，本窗口未发送领取请求"
    );
    assert_eq!(
        server.upstream.claims.load(Ordering::SeqCst),
        0,
        "binding is not same-window permission to replay an unowned intent"
    );
    assert_eq!(
        repo::get_secret(db.pool(), &old_owner.0)
            .await
            .unwrap()
            .as_deref(),
        Some(old_owner.1.as_str())
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_run_principals WHERE run_id=?")
            .bind(refused.run.id)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0,
        "do not retrofit today's UID onto the old error receipt"
    );
    center
        .restore_benefit_auth(&cfg, "owned", &bound.source_revision)
        .await
        .unwrap();
    assert_eq!(
        repo::get_secret(db.pool(), "benefit:trae_cn:owned:token")
            .await
            .unwrap(),
        original_source
    );
    assert_eq!(
        repo::get_secret(db.pool(), &old_owner.0)
            .await
            .unwrap()
            .as_deref(),
        Some(old_owner.1.as_str())
    );
    assert!(
        refused.run.message.contains("新活动窗口"),
        "migration must disclose the window restriction: {}",
        refused.run.message
    );
}

#[tokio::test]
async fn native_binding_can_use_a_new_window_while_a_prior_unknown_owner_is_preserved() {
    let cfg = config();
    let db = Db::connect_in_memory().await.unwrap();
    let server = server().await;
    let center = center(&db, &server);
    let mut legacy = session(false, "synthetic-legacy");
    legacy.snapshot.identity_hash = None;
    center.set_session(&cfg, "owned", &legacy).await.unwrap();
    // Artificial prior-window v1 shape, not a clock change or a real receipt.
    let old_window = "trae_cn:owned-previous-window:100:200";
    let name = format!(
        "benefit:reservation:v1:{:x}",
        Sha256::digest(serde_json::to_vec(&("trae_cn", "owned", old_window)).unwrap())
    );
    let old = json!({"kind":"benefit_claim_reservation","version":1,"platform":"trae_cn","account_id":"owned","window_key":old_window,"attempt_id":uuid::Uuid::new_v4().to_string(),"state":{"phase":"dispatch_started_outcome_unknown"}});
    let original = crypto::encrypt(&old.to_string()).unwrap();
    repo::set_secret(db.pool(), &name, &original).await.unwrap();
    let uid = source_tests::synthetic_native(
        &db,
        "owned",
        "owned-native",
        "synthetic-native",
        Utc::now().timestamp_millis() + 60_000,
        false,
    )
    .await;
    let before = center.benefit_auth_source(&cfg, "owned").await.unwrap();
    center
        .bind_trae_benefit_auth(&cfg, None, "owned", &before.source_revision, &uid)
        .await
        .unwrap();
    let result = center.claim_now(&cfg, None, "owned").await.unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Granted);
    assert_eq!(server.upstream.claims.load(Ordering::SeqCst), 1);
    assert_ne!(result.run.window_key.as_str(), old_window);
    assert_eq!(
        repo::get_secret(db.pool(), &name).await.unwrap().as_deref(),
        Some(original.as_str())
    );
    let attributed: String =
        sqlx::query_scalar("SELECT principal_hash FROM benefit_run_principals WHERE run_id=?")
            .bind(result.run.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        attributed, uid,
        "only the new-window native attempt receives verified attribution"
    );
}
