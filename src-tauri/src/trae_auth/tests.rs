//! Actual loopback and HTTP fixtures, not a real provider login or key registration.
use super::*;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct Fixture {
    exchanges: AtomicUsize,
    refreshes: AtomicUsize,
    principals: AtomicUsize,
    fail: AtomicBool,
    wrong: AtomicBool,
    reflect: AtomicBool,
    expired: AtomicBool,
    hold: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    requests: parking_lot::Mutex<Vec<Value>>,
}
struct TestServer {
    service: TraeAuth,
    db: crate::db::Db,
    f: Arc<Fixture>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn exchange(
    State(f): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(body["ClientID"], contract::CLIENT_ID);
    assert_eq!(body["IDEVersion"], contract::VERSION);
    assert_eq!(body["DeviceInfo"]["PlatformCode"], "IDE_PC");
    assert_eq!(body["DeviceInfo"]["DeviceType"], "PC");
    let public = body["DeviceInfo"]["DevicePublicKey"].as_str().unwrap();
    assert!(public.starts_with("-----BEGIN PUBLIC KEY-----"));
    assert!(!body.to_string().contains("PRIVATE KEY"));
    let index = if body.get("AuthCode").is_some() {
        assert_eq!(headers["x-cloudide-token"], "");
        assert_eq!(body["AuthCode"], "synthetic-code");
        assert_eq!(body["CodeVerifier"].as_str().unwrap().len(), 64);
        f.exchanges.fetch_add(1, Ordering::SeqCst) + 1
    } else {
        assert!(headers["x-cloudide-token"]
            .to_str()
            .unwrap()
            .starts_with("synthetic-access-"));
        assert_eq!(body["ClientSecret"], "");
        assert!(body["DeviceProof"]["Signature"].as_str().unwrap().len() > 64);
        assert_eq!(body["DeviceProof"]["Nonce"].as_str().unwrap().len(), 32);
        assert!(body["DeviceProof"]["Timestamp"].as_i64().unwrap() > 0);
        f.refreshes.fetch_add(1, Ordering::SeqCst) + 2
    };
    f.requests.lock().push(body);
    if f.hold.load(Ordering::SeqCst) {
        f.entered.notify_one();
        f.release.notified().await;
    }
    if f.fail.load(Ordering::SeqCst) {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"internal":"synthetic-secret-should-never-echo"})),
        );
    }
    let expires = if f.expired.load(Ordering::SeqCst) {
        now() - 1000
    } else {
        now() + 60000
    };
    (
        StatusCode::OK,
        Json(
            json!({"ResponseMetadata":{"RequestID":"synthetic"},"Result":{"Token":format!("synthetic-access-{index}"),"RefreshToken":format!("synthetic-refresh-{index}"),"TokenExpireAt":expires,"RefreshExpireAt":now()+120000}}),
        ),
    )
}
async fn user(
    State(f): State<Arc<Fixture>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let token = headers["x-cloudide-token"].to_str().unwrap();
    assert!(token.starts_with("synthetic-access-"));
    assert_eq!(
        body,
        json!({"ReqSource":"IDE","IDEVersion":contract::VERSION})
    );
    f.principals.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"Result":{"UserID":if f.wrong.load(Ordering::SeqCst){"other-owner"}else{"synthetic-owner"},"ScreenName":if f.reflect.load(Ordering::SeqCst){token}else{"Synthetic account"},"NonPlainTextEmail":"s***@example.invalid"}}),
    )
}
async fn setup() -> TestServer {
    use base64::Engine;
    if std::env::var_os("LLMGW_MASTER_KEY").is_none() {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            base64::engine::general_purpose::STANDARD.encode([73; 32]),
        );
    }
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let f = Arc::new(Fixture::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let router = Router::new()
        .route(contract::EXCHANGE, post(exchange))
        .route(contract::PRINCIPAL, post(user))
        .with_state(f.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut service = TraeAuth::new(db.pool().clone(), None).unwrap();
    Arc::get_mut(&mut service.core).unwrap().origin = origin;
    TestServer {
        service,
        db,
        f,
        task,
    }
}
fn session() -> SessionCredential {
    serde_json::from_value(json!({"schema_version":1,"platform":"trae_cn","request_source":1,"headers":{"authorization":"Cloud-IDE-JWT synthetic-imported-access","x-device-id":"1234567890123456","x-device-brand":"Synthetic hardware","x-device-type":"Windows","x-os-version":"Synthetic Windows version","x-app-version":"3.4.2"},"snapshot":{}})).unwrap()
}
fn query(id: &str) -> String {
    let mut url = Url::parse("http://127.0.0.1/").unwrap();
    url.query_pairs_mut()
        .append_pair("loginTraceID", id)
        .append_pair("scope", "marscode")
        .append_pair("host", contract::ISSUER)
        .append_pair("authCodeInfo", "{\"AuthCode\":\"synthetic-code\"}");
    url.query().unwrap().into()
}
async fn start(t: &TestServer) -> LoginStart {
    t.service.start("test", &session(), true).await.unwrap()
}
async fn login(t: &TestServer) -> LoginStart {
    let pending = start(t).await;
    t.service
        .core
        .complete(&pending.login_id, &query(&pending.login_id))
        .await
        .unwrap();
    pending
}
#[tokio::test]
async fn login_callback_uses_pkce_and_verifies_principal_before_encrypting_new_keys() {
    let t = setup().await;
    let started = start(&t).await;
    let url = Url::parse(&started.authorization_url).unwrap();
    assert_eq!(url.host_str(), Some("www.trae.cn"));
    let params: HashMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(params["auth_from"], "trae");
    assert_eq!(params["code_challenge_method"], "S256");
    assert!(!params.contains_key("code_verifier"));
    assert!(!params.contains_key("private_key"));
    let callback = format!(
        "{}?{}",
        params["auth_callback_url"],
        query(&started.login_id)
    );
    let response = reqwest::get(callback).await.unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    assert!(!text.contains("synthetic-code"));
    assert_eq!(
        t.service
            .poll("test", &started.login_id)
            .await
            .unwrap()
            .status,
        "authenticated"
    );
    let cipher = t
        .service
        .core
        .read(&key("session", "test"))
        .await
        .unwrap()
        .unwrap();
    assert!(!cipher.contains("synthetic-access"));
    assert!(!cipher.contains("PRIVATE KEY"));
    let credential: Credential = decode(&cipher).unwrap();
    assert_eq!(credential.uid, "synthetic-owner");
    assert!(credential
        .private_key
        .starts_with("-----BEGIN PRIVATE KEY-----"));
    assert_eq!(
        (
            t.f.exchanges.load(Ordering::SeqCst),
            t.f.principals.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
    assert!(t
        .service
        .core
        .complete(&started.login_id, &query(&started.login_id))
        .await
        .is_err());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn explicit_consent_and_saved_real_device_are_required_before_creating_any_pending_state() {
    let t = setup().await;
    assert!(t.service.start("test", &session(), false).await.is_err());
    for name in ["", "../unsafe", "非法名称"] {
        assert!(t.service.start(name, &session(), true).await.is_err());
    }
    let mut wrong = session();
    wrong.headers.insert("x-device-id".into(), "0".into());
    assert!(t.service.start("test", &wrong, true).await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM app_secrets")
        .fetch_one(t.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn first_login_rejects_a_different_subject_from_the_selected_saved_benefits_account() {
    let t = setup().await;
    let mut source = session();
    source.snapshot.identity_hash =
        Some(format!("{:x}", Sha256::digest(b"trae_cn:synthetic-owner")));
    let p = t.service.start("test", &source, true).await.unwrap();
    t.f.wrong.store(true, Ordering::SeqCst);
    assert!(t
        .service
        .core
        .complete(&p.login_id, &query(&p.login_id))
        .await
        .is_err());
    assert!(t
        .service
        .core
        .read(&key("session", "test"))
        .await
        .unwrap()
        .is_none());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(
        t.service.poll("test", &p.login_id).await.unwrap().status,
        "authorization_failed_no_retry"
    );
}

#[tokio::test]
async fn first_login_accepts_the_same_verified_saved_subject() {
    let t = setup().await;
    let mut source = session();
    source.snapshot.identity_hash =
        Some(format!("{:x}", Sha256::digest(b"trae_cn:synthetic-owner")));
    let p = t.service.start("test", &source, true).await.unwrap();
    t.service
        .core
        .complete(&p.login_id, &query(&p.login_id))
        .await
        .unwrap();
    assert_eq!(
        t.service.poll("test", &p.login_id).await.unwrap().status,
        "authenticated"
    );
}
#[tokio::test]
async fn duplicate_wrong_region_or_invalid_utf8_callbacks_do_not_start_an_exchange() {
    let t = setup().await;
    let p = start(&t).await;
    for value in [
        query("wrong-trace"),
        format!("{}&loginTraceID={}", query(&p.login_id), p.login_id),
        query(&p.login_id).replace("api.trae.cn", "api.example.invalid"),
        format!("{}&x=%FF", query(&p.login_id)),
    ] {
        let url = Url::parse(&p.authorization_url).unwrap();
        let callback = url
            .query_pairs()
            .find(|(k, _)| k == "auth_callback_url")
            .unwrap()
            .1
            .into_owned();
        let response = reqwest::get(format!("{callback}?{value}")).await.unwrap();
        assert_eq!(response.status(), 400);
    }
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
    assert_eq!(
        t.service.poll("test", &p.login_id).await.unwrap().status,
        "awaiting_authorization"
    );
    let url = Url::parse(&p.authorization_url).unwrap();
    let callback = url
        .query_pairs()
        .find(|(k, _)| k == "auth_callback_url")
        .unwrap()
        .1
        .into_owned();
    assert_eq!(
        reqwest::get(format!("{callback}?{}", query(&p.login_id)))
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_new_login_keeps_old_session_without_reporting_new_authentication() {
    let t = setup().await;
    login(&t).await;
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    let p = start(&t).await;
    let name = key("pending", &p.login_id);
    let raw = t.service.core.read(&name).await.unwrap().unwrap();
    let mut pending: Pending = decode(&raw).unwrap();
    pending.deadline = now() - 1;
    t.service
        .core
        .replace(&name, &raw, &encode(&pending).unwrap())
        .await
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            t.service.poll("test", &p.login_id).await.unwrap().status,
            "expired"
        );
    }
    assert!(t.service.core.read(&key("session", "test")).await.unwrap() == original);
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(
        t.service.status("test").await.unwrap().status,
        "authenticated"
    );
}

fn another_service(t: &TestServer) -> TraeAuth {
    let mut service = TraeAuth::new(t.db.pool().clone(), None).unwrap();
    Arc::get_mut(&mut service.core).unwrap().origin = t.service.core.origin.clone();
    service
}

#[tokio::test]
async fn local_refresh_preparation_failure_can_recover_across_services_without_http_replay() {
    let t = setup().await;
    login(&t).await;
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    *t.service.core.http.write() = Err("Synthetic local configuration failure".into());
    assert!(t.service.refresh("test").await.is_err());
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 0);
    assert!(t.service.core.read(&key("session", "test")).await.unwrap() == original);
    let restored = another_service(&t);
    assert!(restored.status("test").await.unwrap().can_refresh);
    assert_eq!(
        restored.refresh("test").await.unwrap().status,
        "authenticated"
    );
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
    t.f.fail.store(true, Ordering::SeqCst);
    assert!(restored.refresh("test").await.is_err());
    let after_restart = another_service(&t);
    assert!(after_restart.refresh("test").await.is_err());
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn callback_failure_never_replays_and_does_not_report_an_old_session_as_new_success() {
    let t = setup().await;
    login(&t).await;
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    let p = start(&t).await;
    t.f.fail.store(true, Ordering::SeqCst);
    let error = t
        .service
        .core
        .complete(&p.login_id, &query(&p.login_id))
        .await
        .unwrap_err();
    assert!(!error.contains("synthetic-secret"));
    assert!(t
        .service
        .core
        .complete(&p.login_id, &query(&p.login_id))
        .await
        .is_err());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 2);
    assert_eq!(
        t.service.poll("test", &p.login_id).await.unwrap().status,
        "authorization_failed_no_retry"
    );
    assert!(t.service.core.read(&key("session", "test")).await.unwrap() == original);
}
#[tokio::test]
async fn cancelled_inflight_callback_cannot_save_late_tokens() {
    let t = setup().await;
    let p = start(&t).await;
    t.f.hold.store(true, Ordering::SeqCst);
    let core = t.service.core.clone();
    let id = p.login_id.clone();
    let task = tokio::spawn(async move { core.complete(&id, &query(&id)).await });
    t.f.entered.notified().await;
    t.service.cancel("test", &p.login_id).await.unwrap();
    t.f.release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert!(t
        .service
        .core
        .read(&key("session", "test"))
        .await
        .unwrap()
        .is_none());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(t.f.principals.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn new_login_supersedes_old_wait_and_blocks_a_late_owner_across_services() {
    let t = setup().await;
    let first = start(&t).await;
    t.f.hold.store(true, Ordering::SeqCst);
    let core = t.service.core.clone();
    let id = first.login_id.clone();
    let old = tokio::spawn(async move { core.complete(&id, &query(&id)).await });
    t.f.entered.notified().await;
    let other = another_service(&t);
    let second = other.start("test", &session(), true).await.unwrap();
    t.f.release.notify_one();
    assert!(old.await.unwrap().is_err());
    assert!(t
        .service
        .core
        .read(&key("session", "test"))
        .await
        .unwrap()
        .is_none());
    assert!(t
        .service
        .core
        .read(&key("pending", &first.login_id))
        .await
        .unwrap()
        .is_none());
    t.f.hold.store(false, Ordering::SeqCst);
    other
        .core
        .complete(&second.login_id, &query(&second.login_id))
        .await
        .unwrap();
}
#[tokio::test]
async fn refresh_uses_same_key_and_rotated_tokens_then_rechecks_the_server_principal() {
    let t = setup().await;
    login(&t).await;
    let old: Credential = decode(
        &t.service
            .core
            .read(&key("session", "test"))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let status = t.service.refresh("test").await.unwrap();
    assert_eq!(status.status, "authenticated");
    assert!(status.can_refresh);
    let new: Credential = decode(
        &t.service
            .core
            .read(&key("session", "test"))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(old.private_key == new.private_key && old.public_key == new.public_key);
    assert_eq!(new.pair.access, "synthetic-access-2");
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(t.f.principals.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn failed_or_wrong_principal_refresh_preserves_original_and_blocks_same_generation_replay() {
    for wrong in [false, true] {
        let t = setup().await;
        login(&t).await;
        let original = t.service.core.read(&key("session", "test")).await.unwrap();
        if wrong {
            t.f.wrong.store(true, Ordering::SeqCst);
        } else {
            t.f.fail.store(true, Ordering::SeqCst);
        }
        assert!(t.service.refresh("test").await.is_err());
        assert!(t.service.refresh("test").await.is_err());
        assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
        assert!(t.service.core.read(&key("session", "test")).await.unwrap() == original);
        assert!(!t.service.status("test").await.unwrap().can_refresh);
    }
}
#[tokio::test]
async fn cancelled_refresh_cannot_overwrite_the_account_from_a_late_reply() {
    let t = setup().await;
    login(&t).await;
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    t.f.hold.store(true, Ordering::SeqCst);
    let service = t.service.clone();
    let task = tokio::spawn(async move { service.refresh("test").await });
    t.f.entered.notified().await;
    t.service.cancel_refresh("test").await.unwrap();
    t.f.release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert!(t.service.core.read(&key("session", "test")).await.unwrap() == original);
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_new_login_takes_ownership_from_an_inflight_refresh_across_services() {
    let t = setup().await;
    login(&t).await;
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    t.f.hold.store(true, Ordering::SeqCst);
    let service = t.service.clone();
    let task = tokio::spawn(async move { service.refresh("test").await });
    t.f.entered.notified().await;
    let other = another_service(&t);
    let p = other.start("test", &session(), true).await.unwrap();
    t.f.release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert!(t.service.core.read(&key("session", "test")).await.unwrap() == original);
    assert_eq!(
        other.poll("test", &p.login_id).await.unwrap().status,
        "awaiting_authorization"
    );
    t.f.hold.store(false, Ordering::SeqCst);
    other
        .core
        .complete(&p.login_id, &query(&p.login_id))
        .await
        .unwrap();
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_active_login_prevents_refresh_from_invalidating_its_baseline() {
    let t = setup().await;
    login(&t).await;
    let p = start(&t).await;
    let other = another_service(&t);
    assert!(other.refresh("test").await.is_err());
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 0);
    t.service.cancel("test", &p.login_id).await.unwrap();
    assert_eq!(other.refresh("test").await.unwrap().status, "authenticated");
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_during_local_refresh_preparation_prevents_any_http_across_services() {
    let t = setup().await;
    login(&t).await;
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *t.service.core.preparation_pause.lock() = Some((entered.clone(), release.clone()));
    let service = t.service.clone();
    let task = tokio::spawn(async move { service.refresh("test").await });
    entered.notified().await;
    let other = another_service(&t);
    other.cancel_refresh("test").await.unwrap();
    release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 0);
    assert!(t.service.core.read(&key("session", "test")).await.unwrap() == original);
    assert!(other.status("test").await.unwrap().can_refresh);
    // A separate explicit operation after cancellation remains possible because
    // the cancelled preparation never transmitted a refresh token.
    assert_eq!(other.refresh("test").await.unwrap().status, "authenticated");
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn secret_reflection_expired_token_and_imported_legacy_data_never_become_an_independent_session(
) {
    for expired in [false, true] {
        let t = setup().await;
        let p = start(&t).await;
        if expired {
            t.f.expired.store(true, Ordering::SeqCst);
        } else {
            t.f.reflect.store(true, Ordering::SeqCst);
        }
        assert!(t
            .service
            .core
            .complete(&p.login_id, &query(&p.login_id))
            .await
            .is_err());
        assert!(t
            .service
            .core
            .read(&key("session", "test"))
            .await
            .unwrap()
            .is_none());
    }
    let t = setup().await;
    repo::set_secret(
        t.db.pool(),
        &key("session", "test"),
        &encode(&session()).unwrap(),
    )
    .await
    .unwrap();
    assert!(t.service.refresh("test").await.is_err());
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 0);
}
