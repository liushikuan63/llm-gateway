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
    principal_hold: AtomicBool,
    principal_entered: tokio::sync::Notify,
    principal_release: tokio::sync::Notify,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    requests: parking_lot::Mutex<Vec<Value>>,
}
struct TestServer {
    service: TraeAuth,
    db: crate::db::Db,
    f: Arc<Fixture>,
    com_f: Arc<Fixture>,
    task: tokio::task::JoinHandle<()>,
    com_task: tokio::task::JoinHandle<()>,
}
impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
        self.com_task.abort();
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
    if f.principal_hold.load(Ordering::SeqCst) {
        f.principal_entered.notify_one();
        f.principal_release.notified().await;
    }
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
    let (origin, f, task) = fixture_server().await;
    let (com_origin, com_f, com_task) = fixture_server().await;
    let mut service = TraeAuth::new(db.pool().clone(), None).unwrap();
    Arc::get_mut(&mut service.core).unwrap().transport_origins = Some([origin, com_origin]);
    TestServer {
        service,
        db,
        f,
        com_f,
        task,
        com_task,
    }
}
async fn fixture_server() -> (Url, Arc<Fixture>, tokio::task::JoinHandle<()>) {
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
    (origin, f, task)
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
fn origin_query(id: &str, scope: Option<&str>, host: Option<&str>) -> String {
    let mut url = Url::parse("http://127.0.0.1/").unwrap();
    url.query_pairs_mut()
        .append_pair("loginTraceID", id)
        .append_pair("authCodeInfo", "{\"AuthCode\":\"synthetic-code\"}");
    if let Some(scope) = scope {
        url.query_pairs_mut().append_pair("scope", scope);
    }
    if let Some(host) = host {
        url.query_pairs_mut().append_pair("host", host);
    }
    url.query().unwrap().into()
}
#[test]
fn cn_callback_current_scope_accepts_explicit_legacy_cn_origin() {
    assert!(callback_code(
        "synthetic-trace",
        &origin_query("synthetic-trace", Some("trae"), Some("https://api.trae.cn"))
    )
    .is_ok());
}
#[test]
fn cn_callback_current_scope_accepts_explicit_com_cn_origin() {
    assert!(callback_code(
        "synthetic-trace",
        &origin_query(
            "synthetic-trace",
            Some("trae"),
            Some("https://api.trae.com.cn")
        )
    )
    .is_ok());
}
#[test]
fn cn_callback_scope_and_host_compatibility_matrix_is_closed() {
    for (scope, host, accepted) in [
        (Some("trae"), Some("https://api.trae.cn"), true),
        (Some("trae"), Some("https://api.trae.com.cn"), true),
        (Some("trae"), None, false),
        (Some("trae"), Some(""), false),
        (Some("marscode"), Some("https://api.trae.cn"), true),
        (Some("marscode"), None, true),
        (None, Some("https://api.trae.cn"), true),
        (None, None, true),
        (None, Some("https://api.trae.com.cn"), false),
        (Some("marscode"), Some("https://api.trae.com.cn"), false),
        (Some(""), Some("https://api.trae.cn"), false),
        (Some("global"), Some("https://api.trae.cn"), false),
        (None, Some(""), false),
    ] {
        assert_eq!(
            callback_code(
                "synthetic-trace",
                &origin_query("synthetic-trace", scope, host)
            )
            .is_ok(),
            accepted,
            "scope={scope:?}, host={host:?}"
        );
    }
}
fn untrusted_hosts() -> &'static [&'static str] {
    &[
        "http://api.trae.cn",
        "http://api.trae.com.cn",
        "https://api.trae.ai",
        "https://api.trae.com",
        "https://api.trae.cn.evil.invalid",
        "https://evil.api.trae.com.cn",
        "https://api.trae.com.cn.evil.invalid",
        "https://evilapi.trae.cn",
        "https://API.trae.cn",
        "https://api.trae.cn.",
        "https://api.trae.com.cn.",
        "https://api.trae.cn/",
        "https://api.trae.com.cn/",
        "https://api.trae.cn/path",
        "https://api.trae.cn?x=1",
        "https://api.trae.com.cn#fragment",
        "https://user@api.trae.cn",
        "https://api.trae.cn@evil.invalid",
        "https://user:pass@api.trae.com.cn",
        "https://api.trae.cn:443",
        "https://api.trae.com.cn:443",
        "https://api.trae.cn:80",
        "https://api.trae.com.cn:1234",
        "https://api.trae.cn\\path",
        " https://api.trae.cn",
        "https://api.trae.com.cn ",
        "https%3A%2F%2Fapi.trae.com.cn",
        "",
    ]
}
#[test]
fn cn_callback_rejects_non_exact_hosts_after_one_query_decode() {
    for host in untrusted_hosts() {
        assert!(
            callback_code(
                "synthetic-trace",
                &origin_query("synthetic-trace", Some("trae"), Some(host))
            )
            .is_err(),
            "{host}"
        );
    }
}
fn invalid_queries(id: &str) -> Vec<String> {
    let good = origin_query(id, Some("trae"), Some("https://api.trae.com.cn"));
    let mut invalid = vec![
        origin_query("wrong-trace", Some("trae"), Some("https://api.trae.com.cn")),
        origin_query("", Some("trae"), Some("https://api.trae.com.cn")),
        good.replace(&format!("loginTraceID={id}&"), ""),
        format!("{good}&x=%"),
        format!("{good}&x=%GG"),
        format!("{good}&x=%FF"),
        format!("{good}&x=%C0%AF"),
        format!("{good}&x={}", "a".repeat(32768)),
    ];
    for field in [
        "loginTraceID",
        "scope",
        "host",
        "authCodeInfo",
        "%68ost",
        "loginTrace%49D",
    ] {
        invalid.push(format!("{good}&{field}=synthetic"));
    }
    for auth in [
        "not-json",
        "{}",
        "{\"AuthCode\":4}",
        "{\"AuthCode\":\"\"}",
        "{\"AuthCode\":\"bad token\"}",
        "{\"AuthCode\":\"bad\\ncode\"}",
    ] {
        let mut url = Url::parse("http://127.0.0.1/").unwrap();
        url.query_pairs_mut()
            .append_pair("loginTraceID", id)
            .append_pair("scope", "trae")
            .append_pair("host", "https://api.trae.com.cn")
            .append_pair("authCodeInfo", auth);
        invalid.push(url.query().unwrap().into());
    }
    invalid.push(good.replace("authCodeInfo=", "unknown="));
    invalid
}
#[test]
fn cn_callback_parser_keeps_trace_duplicate_encoding_and_payload_guards() {
    for query in invalid_queries("synthetic-trace") {
        assert!(
            callback_code("synthetic-trace", &query).is_err(),
            "invalid synthetic query accepted"
        );
    }
}
fn counts(f: &Fixture) -> (usize, usize, usize) {
    (
        f.exchanges.load(Ordering::SeqCst),
        f.refreshes.load(Ordering::SeqCst),
        f.principals.load(Ordering::SeqCst),
    )
}
async fn stored_credential(t: &TestServer) -> Credential {
    decode(
        &t.service
            .core
            .read(&key("session", "test"))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}
#[tokio::test]
async fn cn_callback_routes_exchange_principal_and_restarted_refresh_to_persisted_origin() {
    for (scope, host, issuer, is_com) in [
        (
            Some("trae"),
            Some("https://api.trae.com.cn"),
            "https://api.trae.com.cn",
            true,
        ),
        (
            Some("trae"),
            Some("https://api.trae.cn"),
            "https://api.trae.cn",
            false,
        ),
        (
            Some("marscode"),
            Some("https://api.trae.cn"),
            "https://api.trae.cn",
            false,
        ),
        (Some("marscode"), None, "https://api.trae.cn", false),
        (
            None,
            Some("https://api.trae.cn"),
            "https://api.trae.cn",
            false,
        ),
        (None, None, "https://api.trae.cn", false),
    ] {
        let t = setup().await;
        let p = start(&t).await;
        t.service
            .core
            .complete(&p.login_id, &origin_query(&p.login_id, scope, host))
            .await
            .unwrap();
        let (selected, other) = if is_com {
            (&t.com_f, &t.f)
        } else {
            (&t.f, &t.com_f)
        };
        assert_eq!(counts(selected), (1, 0, 1));
        assert_eq!(counts(other), (0, 0, 0));
        let before = stored_credential(&t).await;
        assert_eq!(before.issuer, issuer);
        assert_eq!(before.uid, "synthetic-owner");
        assert_eq!(
            before.snapshot.identity_hash.as_deref(),
            Some(format!("{:x}", Sha256::digest(b"trae_cn:synthetic-owner")).as_str())
        );
        let restarted = another_service(&t);
        assert_eq!(
            restarted.refresh("test").await.unwrap().status,
            "authenticated"
        );
        let after = stored_credential(&t).await;
        assert_eq!(after.issuer, issuer);
        assert_eq!(after.uid, before.uid);
        assert_eq!(after.snapshot.identity_hash, before.snapshot.identity_hash);
        assert_eq!(after.private_key, before.private_key);
        assert_eq!(after.public_key, before.public_key);
        assert_eq!(
            serde_json::to_value(after.device).unwrap(),
            serde_json::to_value(before.device).unwrap()
        );
        assert_eq!(counts(selected), (1, 1, 2));
        assert_eq!(counts(other), (0, 0, 0));
        for request in selected.requests.lock().iter() {
            assert!(request.get("Scope").is_none());
        }
    }
}
#[tokio::test]
async fn cn_callback_invalid_input_never_reserves_or_contacts_either_origin() {
    let t = setup().await;
    let p = start(&t).await;
    let original = t
        .service
        .core
        .read(&key("pending", &p.login_id))
        .await
        .unwrap();
    let mut invalid = invalid_queries(&p.login_id);
    invalid.extend(
        untrusted_hosts()
            .iter()
            .map(|host| origin_query(&p.login_id, Some("trae"), Some(host))),
    );
    invalid.extend([
        origin_query(&p.login_id, Some("trae"), None),
        origin_query(&p.login_id, None, Some("https://api.trae.com.cn")),
        origin_query(
            &p.login_id,
            Some("marscode"),
            Some("https://api.trae.com.cn"),
        ),
    ]);
    for query in invalid {
        assert!(t.service.core.complete(&p.login_id, &query).await.is_err());
        assert_eq!(
            t.service
                .core
                .read(&key("pending", &p.login_id))
                .await
                .unwrap(),
            original
        );
        assert_eq!(counts(&t.f), (0, 0, 0));
        assert_eq!(counts(&t.com_f), (0, 0, 0));
    }
    let url = Url::parse(&p.authorization_url).unwrap();
    let callback = url
        .query_pairs()
        .find(|(key, _)| key == "auth_callback_url")
        .unwrap()
        .1
        .into_owned();
    let response = reqwest::get(format!(
        "{callback}?{}",
        origin_query(&p.login_id, Some("trae"), Some("https://api.trae.com.cn"))
    ))
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(counts(&t.f), (0, 0, 0));
    assert_eq!(counts(&t.com_f), (1, 0, 1));
}
#[tokio::test]
async fn cn_callback_failed_com_exchange_never_falls_back_or_replays_on_another_host() {
    let t = setup().await;
    login(&t).await;
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    let p = start(&t).await;
    t.com_f.fail.store(true, Ordering::SeqCst);
    for host in [
        "https://api.trae.com.cn",
        "https://api.trae.com.cn",
        "https://api.trae.cn",
    ] {
        assert!(t
            .service
            .core
            .complete(
                &p.login_id,
                &origin_query(&p.login_id, Some("trae"), Some(host))
            )
            .await
            .is_err());
    }
    assert_eq!(counts(&t.com_f), (1, 0, 0));
    assert_eq!(counts(&t.f), (1, 0, 1));
    assert_eq!(
        t.service.core.read(&key("session", "test")).await.unwrap(),
        original
    );
    assert_eq!(
        t.service.poll("test", &p.login_id).await.unwrap().status,
        "authorization_failed_no_retry"
    );
}
#[tokio::test]
async fn cn_callback_failed_com_refresh_keeps_issuer_and_blocks_generation_across_restart() {
    let t = setup().await;
    let p = start(&t).await;
    t.service
        .core
        .complete(
            &p.login_id,
            &origin_query(&p.login_id, Some("trae"), Some("https://api.trae.com.cn")),
        )
        .await
        .unwrap();
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    t.com_f.fail.store(true, Ordering::SeqCst);
    assert!(another_service(&t).refresh("test").await.is_err());
    assert!(another_service(&t).refresh("test").await.is_err());
    assert_eq!(counts(&t.com_f), (1, 1, 1));
    assert_eq!(counts(&t.f), (0, 0, 0));
    assert_eq!(
        t.service.core.read(&key("session", "test")).await.unwrap(),
        original
    );
    assert_eq!(
        stored_credential(&t).await.issuer,
        "https://api.trae.com.cn"
    );
}
#[tokio::test]
async fn cn_callback_invalid_saved_issuer_cannot_reserve_refresh_or_send_http() {
    let t = setup().await;
    login(&t).await;
    for issuer in [
        "https://api.trae.ai",
        "https://api.trae.cn/",
        "https://api.trae.com.cn:443",
        "https://user@api.trae.cn",
    ] {
        let mut credential = stored_credential(&t).await;
        credential.issuer = issuer.into();
        repo::set_secret(
            t.db.pool(),
            &key("session", "test"),
            &encode(&credential).unwrap(),
        )
        .await
        .unwrap();
        assert!(another_service(&t).refresh("test").await.is_err());
        assert!(t
            .service
            .core
            .read(&key("refresh", "test"))
            .await
            .unwrap()
            .is_none());
        assert_eq!(counts(&t.f), (1, 0, 1));
        assert_eq!(counts(&t.com_f), (0, 0, 0));
    }
}
#[tokio::test]
async fn cn_callback_cancelled_com_principal_cannot_replace_existing_cn_session() {
    let t = setup().await;
    login(&t).await;
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    let p = start(&t).await;
    t.com_f.principal_hold.store(true, Ordering::SeqCst);
    let core = t.service.core.clone();
    let id = p.login_id.clone();
    let task = tokio::spawn(async move {
        core.complete(
            &id,
            &origin_query(&id, Some("trae"), Some("https://api.trae.com.cn")),
        )
        .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        t.com_f.principal_entered.notified(),
    )
    .await
    .unwrap();
    assert_eq!(
        t.service.core.read(&key("session", "test")).await.unwrap(),
        original
    );
    t.service.cancel("test", &p.login_id).await.unwrap();
    t.com_f.principal_release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert_eq!(
        t.service.core.read(&key("session", "test")).await.unwrap(),
        original
    );
    assert_eq!(stored_credential(&t).await.issuer, "https://api.trae.cn");
    assert_eq!(counts(&t.f), (1, 0, 1));
    assert_eq!(counts(&t.com_f), (1, 0, 1));
}
#[tokio::test]
async fn cn_callback_com_principal_mismatch_keeps_old_session_and_never_tries_cn() {
    let t = setup().await;
    login(&t).await;
    let original = t.service.core.read(&key("session", "test")).await.unwrap();
    let p = start(&t).await;
    t.com_f.wrong.store(true, Ordering::SeqCst);
    assert!(t
        .service
        .core
        .complete(
            &p.login_id,
            &origin_query(&p.login_id, Some("trae"), Some("https://api.trae.com.cn"))
        )
        .await
        .is_err());
    assert_eq!(
        t.service.core.read(&key("session", "test")).await.unwrap(),
        original
    );
    assert_eq!(counts(&t.f), (1, 0, 1));
    assert_eq!(counts(&t.com_f), (1, 0, 1));
    assert_eq!(
        t.service.poll("test", &p.login_id).await.unwrap().status,
        "authorization_failed_no_retry"
    );
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
    Arc::get_mut(&mut service.core).unwrap().transport_origins =
        t.service.core.transport_origins.clone();
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

#[tokio::test]
async fn recovery_reload_discovers_original_login_without_renderer_login_id() {
    let t = setup().await;
    let lost_response = start(&t).await;
    let reloaded = t.service.clone();
    let view = reloaded.recovery("test").await.unwrap();
    let login = view.login.unwrap();
    assert_eq!(login.login_id, lost_response.login_id);
    assert_eq!(login.deadline_ms, lost_response.deadline_ms);
    assert_eq!(login.state, "waiting");
    assert_eq!(
        t.service.status("test").await.unwrap().status,
        "not_logged_in"
    );
    t.service
        .core
        .complete(&login.login_id, &query(&login.login_id))
        .await
        .unwrap();
    assert_eq!(
        reloaded
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "completed"
    );
}

#[tokio::test]
async fn recovery_cancel_preparing_login_fences_late_material() {
    let t = setup().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *t.service.core.preparation_pause.lock() = Some((entered.clone(), release.clone()));
    let service = t.service.clone();
    let task = tokio::spawn(async move { service.start("test", &session(), true).await });
    entered.notified().await;
    assert!(t.service.recovery("test").await.unwrap().login_preparing);
    t.service.cancel_recovery("test").await.unwrap();
    assert!(t.service.recovery("test").await.unwrap().login_preparing);
    assert!(t.service.start("test", &session(), true).await.is_err());
    assert!(t.service.refresh("test").await.is_err());
    release.notify_one();
    assert!(
        task.await.unwrap().is_err(),
        "cancelled preparation persisted a new wait"
    );
    let view = t.service.recovery("test").await.unwrap();
    assert!(!view.login_preparing);
    assert!(view.login.is_none());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
}

async fn recovery_storage(t: &TestServer) -> Vec<(String, String, String)> {
    sqlx::query_as("SELECT name,value_enc,CAST(updated_at AS TEXT) FROM app_secrets ORDER BY name")
        .fetch_all(t.db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn recovery_read_is_metadata_only_and_keeps_wait_session_and_database_unchanged() {
    let t = setup().await;
    login(&t).await;
    let p = start(&t).await;
    let before = recovery_storage(&t).await;
    for _ in 0..3 {
        let value = serde_json::to_value(t.service.recovery("test").await.unwrap()).unwrap();
        assert_eq!(
            value,
            json!({"account_id":"test","login_preparing":false,"refresh_in_flight":false,"refresh_interrupted":false,"login":{"login_id":p.login_id,"deadline_ms":p.deadline_ms,"state":"waiting"}})
        );
        let text = value.to_string();
        for secret in [
            "authorization_url",
            "private_key",
            "device",
            "code_verifier",
            "synthetic-access",
            "identity",
            "public_key",
        ] {
            assert!(!text.contains(secret));
        }
        assert_eq!(
            t.service.status("test").await.unwrap().status,
            "authenticated"
        );
    }
    assert_eq!(recovery_storage(&t).await, before);
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 0);
    assert_eq!(t.f.principals.load(Ordering::SeqCst), 1);
    let saved = t.service.core.read(&key("session", "test")).await.unwrap();
    t.service.cancel_recovery("test").await.unwrap();
    assert_eq!(
        t.service
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "cancelled"
    );
    assert_eq!(
        t.service.core.read(&key("session", "test")).await.unwrap(),
        saved
    );
}

#[tokio::test]
async fn recovery_expiry_is_a_read_only_projection_and_completed_terminal_survives_cleanup() {
    let t = setup().await;
    let p = login(&t).await;
    t.service.cancel_recovery("test").await.unwrap();
    assert_eq!(
        t.service
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "completed"
    );
    let name = key("pending", &p.login_id);
    let raw = t.service.core.read(&name).await.unwrap().unwrap();
    let mut pending: Pending = decode(&raw).unwrap();
    pending.state = "waiting".into();
    pending.deadline = now() - 1;
    t.service
        .core
        .replace(&name, &raw, &encode(&pending).unwrap())
        .await
        .unwrap();
    let before = recovery_storage(&t).await;
    for _ in 0..2 {
        assert_eq!(
            t.service
                .recovery("test")
                .await
                .unwrap()
                .login
                .unwrap()
                .state,
            "expired"
        );
    }
    assert_eq!(recovery_storage(&t).await, before);
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recovery_orphan_wait_does_not_rebind_or_replay_and_original_clone_still_owns_listener() {
    let t = setup().await;
    let p = start(&t).await;
    let restarted = another_service(&t);
    let before = recovery_storage(&t).await;
    assert_eq!(
        restarted
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "interrupted"
    );
    assert_eq!(
        restarted.poll("test", &p.login_id).await.unwrap().status,
        "authorization_interrupted"
    );
    assert_eq!(
        t.service
            .clone()
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "waiting"
    );
    assert!(restarted.listeners.0.lock().is_empty());
    assert_eq!(recovery_storage(&t).await, before);
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
    restarted.cancel_recovery("test").await.unwrap();
    assert!(t
        .service
        .core
        .complete(&p.login_id, &query(&p.login_id))
        .await
        .is_err());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn recovery_account_scope_and_pending_ownership_are_exact_and_fail_closed() {
    let t = setup().await;
    let p = start(&t).await;
    assert!(t.service.recovery("other").await.unwrap().login.is_none());
    t.service.cancel_recovery("other").await.unwrap();
    assert_eq!(
        t.service
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "waiting"
    );
    for bad in ["", "a:b", "账户", "test/%"] {
        assert!(t.service.recovery(bad).await.is_err());
        assert!(t.service.cancel_recovery(bad).await.is_err());
    }
    assert!(t.service.poll("other", &p.login_id).await.is_err());
    let name = key("pending", &p.login_id);
    let raw = t.service.core.read(&name).await.unwrap().unwrap();
    let mut pending: Pending = decode(&raw).unwrap();
    pending.id = "different-login".into();
    t.service
        .core
        .replace(&name, &raw, &encode(&pending).unwrap())
        .await
        .unwrap();
    let before = recovery_storage(&t).await;
    let error = t.service.recovery("test").await.unwrap_err();
    assert_eq!(error, "Trae原登录等待归属无效");
    assert!(t.service.cancel_recovery("test").await.is_err());
    assert_eq!(recovery_storage(&t).await, before);
}

#[tokio::test]
async fn recovery_refresh_preparation_stays_visible_after_cancel_until_worker_settles() {
    let t = setup().await;
    login(&t).await;
    let saved = t.service.core.read(&key("session", "test")).await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *t.service.core.preparation_pause.lock() = Some((entered.clone(), release.clone()));
    let service = t.service.clone();
    let task = tokio::spawn(async move { service.refresh("test").await });
    entered.notified().await;
    let view = t.service.recovery("test").await.unwrap();
    assert!(view.refresh_in_flight);
    assert!(!view.refresh_interrupted);
    t.service.cancel_recovery("test").await.unwrap();
    assert!(t.service.recovery("test").await.unwrap().refresh_in_flight);
    assert!(t.service.refresh("test").await.is_err());
    assert!(t.service.start("test", &session(), true).await.is_err());
    release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert!(!t.service.recovery("test").await.unwrap().refresh_in_flight);
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 0);
    assert_eq!(
        t.service.core.read(&key("session", "test")).await.unwrap(),
        saved
    );
    assert!(t.service.status("test").await.unwrap().can_refresh);
    *t.service.core.preparation_pause.lock() = None;
    assert!(t.service.refresh("test").await.is_ok());
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recovery_cross_service_cancel_fences_preparing_login_without_pending_row() {
    let t = setup().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *t.service.core.preparation_pause.lock() = Some((entered.clone(), release.clone()));
    let service = t.service.clone();
    let task = tokio::spawn(async move { service.start("test", &session(), true).await });
    entered.notified().await;
    let newer = another_service(&t);
    newer.cancel_recovery("test").await.unwrap();
    let replacement = newer.start("test", &session(), true).await.unwrap();
    release.notify_one();
    assert!(task.await.unwrap().is_err());
    let view = newer.recovery("test").await.unwrap();
    assert_eq!(view.login.unwrap().login_id, replacement.login_id);
    assert!(!view.login_preparing);
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn recovery_consumed_refresh_after_restart_is_interrupted_and_never_retried() {
    let t = setup().await;
    login(&t).await;
    let saved = t.service.core.read(&key("session", "test")).await.unwrap();
    t.f.hold.store(true, Ordering::SeqCst);
    let service = t.service.clone();
    let task = tokio::spawn(async move { service.refresh("test").await });
    t.f.entered.notified().await;
    let restarted = another_service(&t);
    let before = recovery_storage(&t).await;
    assert!(t.service.recovery("test").await.unwrap().refresh_in_flight);
    for _ in 0..2 {
        let view = restarted.recovery("test").await.unwrap();
        assert!(view.refresh_interrupted);
        assert!(!view.refresh_in_flight);
    }
    assert_eq!(recovery_storage(&t).await, before);
    assert!(restarted.refresh("test").await.is_err());
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
    restarted.cancel_recovery("test").await.unwrap();
    t.f.release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert_eq!(
        t.service.core.read(&key("session", "test")).await.unwrap(),
        saved
    );
    assert!(
        !restarted
            .recovery("test")
            .await
            .unwrap()
            .refresh_interrupted
    );
    assert!(restarted.status("test").await.unwrap().refresh_attempted);
    assert!(!restarted.status("test").await.unwrap().can_refresh);
    assert!(restarted.refresh("test").await.is_err());
    assert_eq!(t.f.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recovery_restart_exchange_is_interrupted_and_cleanup_fences_late_callback() {
    let t = setup().await;
    login(&t).await;
    let saved = t.service.core.read(&key("session", "test")).await.unwrap();
    let p = start(&t).await;
    t.f.hold.store(true, Ordering::SeqCst);
    let service = t.service.clone();
    let id = p.login_id.clone();
    let task = tokio::spawn(async move { service.core.complete(&id, &query(&id)).await });
    t.f.entered.notified().await;
    assert_eq!(
        t.service
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "exchanging"
    );
    let restarted = another_service(&t);
    assert_eq!(
        restarted
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "interrupted"
    );
    restarted.cancel_recovery("test").await.unwrap();
    t.f.release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert!(restarted
        .core
        .complete(&p.login_id, &query(&p.login_id))
        .await
        .is_err());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 2);
    assert_eq!(
        t.service.core.read(&key("session", "test")).await.unwrap(),
        saved
    );
    assert_eq!(
        restarted
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "cancelled"
    );
}

#[tokio::test]
async fn recovery_cancel_storage_failure_keeps_wait_listener_and_original_session() {
    let t = setup().await;
    login(&t).await;
    let p = start(&t).await;
    let before = recovery_storage(&t).await;
    sqlx::query("CREATE TRIGGER synthetic_cancel_failure BEFORE UPDATE OF value_enc ON app_secrets BEGIN SELECT RAISE(ABORT,'synthetic-private-error'); END")
        .execute(t.db.pool()).await.unwrap();
    let error = t.service.cancel_recovery("test").await.unwrap_err();
    assert!(!error.contains("synthetic-private-error"));
    assert_eq!(recovery_storage(&t).await, before);
    assert!(t.service.listener_live(&p.login_id));
    assert_eq!(
        t.service
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "waiting"
    );
    sqlx::query("DROP TRIGGER synthetic_cancel_failure")
        .execute(t.db.pool())
        .await
        .unwrap();
    t.service.cancel_recovery("test").await.unwrap();
    assert!(!t.service.listener_live(&p.login_id));
}

#[tokio::test]
async fn recovery_activities_are_account_scoped_and_drop_does_not_erase_new_work() {
    let t = setup().await;
    let old = t
        .service
        .core
        .begin_activity("test", recovery::ActivityKind::Login)
        .unwrap();
    assert!(t.service.recovery("test").await.unwrap().login_preparing);
    assert!(!t.service.recovery("other").await.unwrap().login_preparing);
    let other = t.service.start("other", &session(), true).await.unwrap();
    t.service.cancel_recovery("test").await.unwrap();
    assert!(old.check().is_err());
    assert_eq!(
        t.service
            .recovery("other")
            .await
            .unwrap()
            .login
            .unwrap()
            .login_id,
        other.login_id
    );
    drop(old);
    assert!(!t.service.recovery("test").await.unwrap().login_preparing);
    let newer = t
        .service
        .core
        .begin_activity("test", recovery::ActivityKind::Refresh)
        .unwrap();
    assert!(t.service.recovery("test").await.unwrap().refresh_in_flight);
    drop(newer);
    assert!(!t.service.recovery("test").await.unwrap().refresh_in_flight);
}

#[tokio::test]
async fn recovery_failed_callback_remains_terminal_and_corrupt_pending_fails_without_leaking() {
    let t = setup().await;
    let p = start(&t).await;
    t.f.fail.store(true, Ordering::SeqCst);
    assert!(t
        .service
        .core
        .complete(&p.login_id, &query(&p.login_id))
        .await
        .is_err());
    assert_eq!(
        t.service
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "failed_no_retry"
    );
    t.service.cancel_recovery("test").await.unwrap();
    assert_eq!(
        t.service
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "failed_no_retry"
    );
    assert!(t
        .service
        .core
        .complete(&p.login_id, &query(&p.login_id))
        .await
        .is_err());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
    let name = key("pending", &p.login_id);
    repo::set_secret(t.db.pool(), &name, "synthetic-private-corrupt-ciphertext")
        .await
        .unwrap();
    let before = recovery_storage(&t).await;
    let error = t.service.recovery("test").await.unwrap_err();
    assert_eq!(error, "Trae认证资料解密失败");
    assert!(!error.contains("synthetic-private"));
    assert!(t.service.cancel_recovery("test").await.is_err());
    assert_eq!(recovery_storage(&t).await, before);
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn official_cn_callback_contract_is_exact_authorize_and_trace_stays_bound() {
    let t = setup().await;
    let p = start(&t).await;
    let authorization = Url::parse(&p.authorization_url).unwrap();
    let callback = authorization
        .query_pairs()
        .find(|(name, _)| name == "auth_callback_url")
        .unwrap()
        .1
        .into_owned();
    let callback_url = Url::parse(&callback).unwrap();
    // Current CN official page.39801783d1.js accepts only this exact path.
    assert_eq!(callback_url.path(), "/authorize");
    assert_eq!(callback_url.host_str(), Some("127.0.0.1"));
    assert!(callback_url.port().is_some());
    assert!(callback_url.query().is_none());
    let http = reqwest::Client::new();
    let valid_query = query(&p.login_id);
    let missing_trace = valid_query
        .split('&')
        .filter(|field| !field.starts_with("loginTraceID="))
        .collect::<Vec<_>>()
        .join("&");
    for bad in [
        query("another-trace"),
        missing_trace,
        format!("{valid_query}&loginTraceID={}", p.login_id),
    ] {
        assert_eq!(
            http.get(format!("{callback}?{bad}"))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
        assert_eq!(
            t.service
                .recovery("test")
                .await
                .unwrap()
                .login
                .unwrap()
                .state,
            "waiting"
        );
    }
    assert_eq!(
        http.get(format!("{callback}/{}?{valid_query}", p.login_id))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        http.get(format!("{callback}?{valid_query}"))
            .header("host", "localhost:1")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
    assert_eq!(
        http.get(format!("{callback}?{valid_query}"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(t
        .service
        .core
        .complete(&p.login_id, &valid_query)
        .await
        .is_err());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(
        t.service
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .state,
        "completed"
    );
}

#[tokio::test]
async fn recovery_context_resolution_is_registered_and_cancelled_before_late_session() {
    let t = setup().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let waiter = release.clone();
    let signal = entered.clone();
    let service = t.service.clone();
    let task = tokio::spawn(async move {
        service
            .start_with_session_resolver("test", true, async move {
                signal.notify_one();
                waiter.notified().await;
                Ok(session())
            })
            .await
    });
    entered.notified().await;
    let discovered = t.service.recovery("test").await.unwrap().login_preparing;
    t.service.cancel_recovery("test").await.unwrap();
    release.notify_one();
    assert!(
        task.await.unwrap().is_err(),
        "late IPC context read created a callback after cleanup"
    );
    assert!(discovered, "in-flight IPC context read was invisible");
    assert!(t.service.recovery("test").await.unwrap().login.is_none());
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn recovery_context_resolver_failure_releases_guard_without_polling_unconsented_context() {
    let t = setup().await;
    let polled = AtomicBool::new(false);
    assert!(t
        .service
        .start_with_session_resolver("test", false, async {
            polled.store(true, Ordering::SeqCst);
            Ok(session())
        })
        .await
        .is_err());
    assert!(!polled.load(Ordering::SeqCst));
    assert!(t
        .service
        .start_with_session_resolver("test", true, async {
            Err("Synthetic context lookup failed".into())
        })
        .await
        .is_err());
    assert!(!t.service.recovery("test").await.unwrap().login_preparing);
    let next = start(&t).await;
    assert_eq!(
        t.service
            .recovery("test")
            .await
            .unwrap()
            .login
            .unwrap()
            .login_id,
        next.login_id
    );
    assert_eq!(t.f.exchanges.load(Ordering::SeqCst), 0);
}
