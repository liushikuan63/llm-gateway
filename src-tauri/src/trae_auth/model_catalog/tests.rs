//! Synthetic in-memory/loopback acceptance. No real account or provider access.
use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Router,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Fixture {
    body: parking_lot::Mutex<String>,
    status: parking_lot::Mutex<StatusCode>,
    length: AtomicBool,
    chunked: AtomicBool,
    delayed_body: AtomicBool,
    hold: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: AtomicUsize,
    unexpected: AtomicUsize,
    requests: parking_lot::Mutex<Vec<(String, String, HeaderMap, usize)>>,
}
struct Setup {
    auth: TraeAuth,
    db: crate::db::Db,
    f: Arc<Fixture>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Setup {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn handler(State(f): State<Arc<Fixture>>, request: Request) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), 1024)
        .await
        .unwrap();
    f.calls.fetch_add(1, Ordering::SeqCst);
    if method != "GET" || !uri.starts_with(PATH) {
        f.unexpected.fetch_add(1, Ordering::SeqCst);
    }
    f.requests.lock().push((method, uri, headers, body.len()));
    f.entered.notify_one();
    if f.hold.load(Ordering::SeqCst) {
        f.release.notified().await;
    }
    let mut response = Response::builder()
        .status(*f.status.lock())
        .header("Content-Type", "application/json");
    if *f.status.lock() == StatusCode::FOUND {
        response = response.header("Location", "/must-never-follow");
    }
    if f.length.load(Ordering::SeqCst) {
        response = response.header("Content-Length", (BODY_LIMIT + 1).to_string());
    }
    if f.delayed_body.load(Ordering::SeqCst) {
        let stream = futures_util::stream::once(async { Ok::<_, std::io::Error>(vec![b'{']) })
            .chain(futures_util::stream::pending());
        return response.body(Body::from_stream(stream)).unwrap();
    }
    if f.chunked.load(Ordering::SeqCst) {
        let stream =
            futures_util::stream::iter((0..17).map(|_| Ok::<_, std::io::Error>(vec![b' '; 65536])));
        return response.body(Body::from_stream(stream)).unwrap();
    }
    response.body(Body::from(f.body.lock().clone())).unwrap()
}
fn packet(models: Value) -> Value {
    json!({"code":0,"data":{"list":[{"function":FUNCTIONS[0],"models":models}]}})
}
fn credential() -> Credential {
    Credential {
        issuer: contract::ISSUER.into(),
        client_id: contract::CLIENT_ID.into(),
        uid: "synthetic-catalog-owner".into(),
        pair: Pair {
            access: "synthetic-catalog-access".into(),
            refresh: Some("synthetic-catalog-refresh".into()),
            access_expires_at_ms: Some(now() + 120_000),
            refresh_expires_at_ms: Some(now() + 240_000),
        },
        device: Device {
            device_id: "9876543210123456".into(),
            machine_id: Some("synthetic-machine-context".into()),
            device_name: "Synthetic".into(),
            device_model: "Synthetic".into(),
            device_brand: "Synthetic".into(),
            device_cpu: "Synthetic".into(),
            os_info: "Synthetic".into(),
            os_version: "Synthetic".into(),
        },
        private_key: "synthetic-private-key-material".into(),
        public_key: "synthetic-public-key-material".into(),
        snapshot: AccountSnapshot {
            identity_hash: Some(format!(
                "{:x}",
                Sha256::digest(b"trae_cn:synthetic-catalog-owner")
            )),
            ..Default::default()
        },
    }
}
async fn seed(t: &Setup, account: &str, value: &Credential) {
    repo::set_secret(
        t.db.pool(),
        &key("session", account),
        &encode(value).unwrap(),
    )
    .await
    .unwrap();
}
async fn setup() -> Setup {
    // Explicit fixture key prevents the production master-key path from being read.
    use base64::Engine;
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        base64::engine::general_purpose::STANDARD.encode([73; 32]),
    );
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let f = Arc::new(Fixture {
        body: parking_lot::Mutex::new(packet(json!([{"name":"Model-A"}])).to_string()),
        status: parking_lot::Mutex::new(StatusCode::OK),
        length: AtomicBool::new(false),
        chunked: AtomicBool::new(false),
        delayed_body: AtomicBool::new(false),
        hold: AtomicBool::new(false),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: AtomicUsize::new(0),
        unexpected: AtomicUsize::new(0),
        requests: parking_lot::Mutex::new(Vec::new()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(listener.local_addr().unwrap().ip().is_loopback());
    let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let app = Router::new().fallback(handler).with_state(f.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut auth = TraeAuth::new(db.pool().clone(), None).unwrap();
    Arc::get_mut(&mut auth.core).unwrap().catalog_test_origin = Some(origin);
    let t = Setup { auth, db, f, task };
    seed(&t, "test", &credential()).await;
    // An unrelated benefit credential must also remain byte-for-byte unchanged.
    repo::set_secret(
        t.db.pool(),
        "benefit:trae_cn:test:token",
        &crypto::encrypt("synthetic-legacy-benefit-token").unwrap(),
    )
    .await
    .unwrap();
    t
}
async fn rows(t: &Setup) -> Vec<(String, String, String, String)> {
    sqlx::query_as("SELECT name,value_enc,CAST(created_at AS TEXT),CAST(updated_at AS TEXT) FROM app_secrets ORDER BY name").fetch_all(t.db.pool()).await.unwrap()
}
#[test]
fn official_empty_envelope_projects_without_inventing_models() {
    let view = project("test", json!({"code":0,"data":{"list":[]}})).unwrap();
    assert!(view.groups.is_empty());
    assert!(view.warnings.is_empty());
    assert_eq!(view.origin, ORIGIN);
}
#[test]
fn primary_channels_keep_exact_ids_order_and_explicit_boolean_capabilities() {
    let view=project("test",json!({"code":0,"data":{"list":[
        {"function":FUNCTIONS[2],"models":[{"name":"Shared","multimodal":"true","reasoning_effort_config":{"support_thinking":1}}]},
        {"function":FUNCTIONS[0],"models":[{"name":"Shared","multimodal":true,"reasoning_effort_config":{"support_thinking":false}},{"name":"No","multimodal":false,"display_name":"Unproven Label"}]},
        {"function":FUNCTIONS[1],"models":[{"name":"Shared","reasoning_effort_config":{"support_thinking":true}}]}
    ]}})).unwrap();
    assert_eq!(
        view.groups.iter().map(|g| g.r#function).collect::<Vec<_>>(),
        FUNCTIONS
    );
    assert_eq!(view.groups[0].models[0].id, "Shared");
    assert_eq!(view.groups[1].models[0].id, "Shared");
    assert_eq!(view.groups[0].models[0].supports_vision, Some(true));
    assert_eq!(view.groups[0].models[0].supports_thinking, Some(false));
    assert_eq!(view.groups[0].models[1].supports_vision, Some(false));
    assert_eq!(view.groups[0].models[1].name, "No");
    assert_eq!(view.groups[1].models[0].supports_vision, None);
    assert_eq!(view.groups[1].models[0].supports_thinking, Some(true));
    assert_eq!(view.groups[2].models[0].supports_vision, None);
    assert_eq!(view.groups[2].models[0].supports_thinking, None);
    let dto = serde_json::to_value(view).unwrap();
    assert_eq!(dto["region"], "cn");
    assert_eq!(
        dto["groups"][2]["models"][0]["supports_vision"],
        Value::Null
    );
}
#[test]
fn custom_credentials_client_connections_and_unknown_sources_are_excluded() {
    let mut models = vec![
        json!({"name":"Official","config_source":1,"ak":"","sk":"","base_url":"","client_connect":false}),
    ];
    for (k, v) in [
        ("ak", json!("private")),
        ("sk", json!("private")),
        ("base_url", json!("https://invalid.local")),
        ("client_connect", json!(true)),
        ("client_connect", json!(1)),
        ("client_connect", json!("false")),
        ("config_source", json!(2)),
        ("config_source", json!(null)),
        ("config_source", json!("1")),
        ("is_custom", json!(true)),
        ("custom_model", json!({})),
    ] {
        let mut m = json!({"name":"Hidden-Custom"});
        m[k] = v;
        models.push(m);
    }
    let view = project("test", packet(json!(models))).unwrap();
    assert_eq!(view.groups[0].models.len(), 1);
    assert_eq!(view.warnings.len(), 1);
    assert!(!view.warnings[0].contains("Hidden-Custom"));
}
#[test]
fn identical_duplicates_dedupe_and_conflicting_declarations_fail() {
    assert_eq!(
        project("test", packet(json!([{"name":"A"},{"name":"A"}])))
            .unwrap()
            .groups[0]
            .models
            .len(),
        1
    );
    assert!(project(
        "test",
        packet(json!([{"name":"A"},{"name":"A","multimodal":false}]))
    )
    .is_err());
}
#[test]
fn malformed_envelopes_and_entries_fail_instead_of_partial_success() {
    for value in [
        json!({}),
        json!({"code":"0","data":{"list":[]}}),
        json!({"code":0.0,"data":{"list":[]}}),
        json!({"code":1,"message":"secret"}),
        json!({"Result":{"code":0,"data":{"list":[]}}}),
        json!({"code":0,"data":{}}),
        packet(json!([{}])),
        packet(json!([null])),
        packet(json!([{"name":5}])),
        packet(json!([{"name":"bad\nname"}])),
        json!({"code":0,"data":{"list":[{"function":1,"models":[]}]}}),
        json!({"code":0,"data":{"list":[{"function":FUNCTIONS[0],"models":{}}]}}),
    ] {
        assert!(project("test", value).is_err());
    }
}
#[test]
fn bounded_groups_models_ids_and_unknown_channels() {
    assert!(project("test", packet(json!([{"name":"a".repeat(257)}]))).is_err());
    assert!(project(
        "test",
        packet(json!((0..513)
            .map(|i| json!({"name":format!("m{i}")}))
            .collect::<Vec<_>>()))
    )
    .is_err());
    assert!(project(
        "test",
        json!({"code":0,"data":{"list":vec![json!({"function":FUNCTIONS[0],"models":[]});17]}})
    )
    .is_err());
    let view=project("test",json!({"code":0,"data":{"list":[{"function":"new-channel","models":[{"name":"Do-not-copy"}]}]}})).unwrap();
    assert!(view.groups.is_empty());
    assert_eq!(view.warnings.len(), 1);
    assert!(!view.warnings[0].contains("new-channel"));
}
#[tokio::test]
async fn actual_get_has_exact_query_headers_no_body_and_zero_database_writes() {
    let t = setup().await;
    let before = rows(&t).await;
    *t.f.body.lock()=json!({"code":0,"data":{"list":FUNCTIONS.iter().map(|function|json!({"function":function,"models":[{"name":"Model-A"}]})).collect::<Vec<_>>()}}).to_string();
    let view = t.auth.model_catalog("test").await.unwrap();
    assert_eq!(view.groups.len(), 3);
    assert_eq!(view.groups[0].models[0].id, "Model-A");
    assert_eq!(before, rows(&t).await);
    assert_eq!(t.f.calls.load(Ordering::SeqCst), 1);
    assert_eq!(t.f.unexpected.load(Ordering::SeqCst), 0);
    let requests = t.f.requests.lock();
    let (method, uri, headers, bytes) = &requests[0];
    assert_eq!(method, "GET");
    assert_eq!(*bytes, 0);
    let url = Url::parse(&format!("http://127.0.0.1{uri}")).unwrap();
    assert_eq!(url.path(), PATH);
    let query = url
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(query.len(), 2);
    assert_eq!(query["functions"], FUNCTIONS.join(","));
    assert_eq!(query["show_custom_model"], "false");
    assert_eq!(
        headers["authorization"],
        "Cloud-IDE-JWT synthetic-catalog-access"
    );
    assert_eq!(headers["accept"], "application/json");
    assert_eq!(headers["x-trae-client-type"], "web");
    assert_eq!(headers["x-preferenced-language"], "zh-CN");
    for absent in [
        "x-ide-token",
        "x-ide-version-code",
        "x-device-id",
        "content-type",
    ] {
        assert!(!headers.contains_key(absent));
    }
}
#[tokio::test]
async fn rejected_credentials_never_make_network_requests_or_mutate_rows() {
    let t = setup().await;
    for case in 0..10 {
        let mut c = credential();
        match case {
            0 => c.issuer = "https://api.trae.ai".into(),
            1 => c.uid = "0".into(),
            2 => c.uid = "bad uid".into(),
            3 => c.snapshot.identity_hash = Some("mismatch".into()),
            4 => c.pair.access_expires_at_ms = None,
            5 => c.pair.access_expires_at_ms = Some(now() - 1),
            6 => c.private_key.clear(),
            7 => c.public_key.clear(),
            8 => c.device.device_id = "0000000000000000".into(),
            _ => c.client_id = "wrong".into(),
        }
        seed(&t, "test", &c).await;
        let before = rows(&t).await;
        assert!(t.auth.model_catalog("test").await.is_err());
        assert_eq!(before, rows(&t).await);
    }
    sqlx::query("DELETE FROM app_secrets WHERE name=?")
        .bind(key("session", "test"))
        .execute(t.db.pool())
        .await
        .unwrap();
    assert!(t.auth.model_catalog("test").await.is_err());
    assert!(t.auth.model_catalog("../invalid").await.is_err());
    assert_eq!(t.f.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn raw_and_escaped_secret_reflections_are_rejected_and_redacted() {
    let t = setup().await;
    let c = credential();
    for secret in [
        &c.pair.access,
        c.pair.refresh.as_ref().unwrap(),
        &c.private_key,
        &c.public_key,
        &c.uid,
        &c.device.device_id,
    ] {
        *t.f.body.lock() = packet(json!([{"name":secret}])).to_string();
        let error = t.auth.model_catalog("test").await.unwrap_err();
        assert!(!error.contains(secret));
        assert!(error.contains("敏感"));
    }
    *t.f.body.lock() = packet(json!([{"name":c.pair.access}]))
        .to_string()
        .replace("synthetic-catalog-access", "\\u0073ynthetic-catalog-access");
    assert!(t
        .auth
        .model_catalog("test")
        .await
        .unwrap_err()
        .contains("敏感"));
}
#[tokio::test]
async fn http_errors_never_retry_redirect_or_echo_server_messages() {
    let t = setup().await;
    let before = rows(&t).await;
    for status in [
        StatusCode::UNAUTHORIZED,
        StatusCode::FOUND,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        *t.f.status.lock() = status;
        *t.f.body.lock() = "untrusted-server-secret".into();
        let error = t.auth.model_catalog("test").await.unwrap_err();
        assert!(error.contains(&status.as_u16().to_string()));
        assert!(!error.contains("untrusted"));
    }
    assert_eq!(t.f.calls.load(Ordering::SeqCst), 3);
    assert_eq!(t.f.unexpected.load(Ordering::SeqCst), 0);
    assert_eq!(before, rows(&t).await);
}
#[tokio::test]
async fn oversized_declared_length_and_streamed_body_fail_closed() {
    let t = setup().await;
    // Match declared bytes so the server's HTTP encoder accepts the fixture.
    *t.f.body.lock() = " ".repeat(BODY_LIMIT + 1);
    t.f.length.store(true, Ordering::SeqCst);
    assert!(t
        .auth
        .model_catalog("test")
        .await
        .unwrap_err()
        .contains("超过上限"));
    t.f.length.store(false, Ordering::SeqCst);
    t.f.chunked.store(true, Ordering::SeqCst);
    assert!(t
        .auth
        .model_catalog("test")
        .await
        .unwrap_err()
        .contains("超过上限"));
    assert_eq!(t.f.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn invalid_proxy_fails_closed_without_network_or_database_changes() {
    let t = setup().await;
    let before = rows(&t).await;
    t.auth.set_proxy(Some("not a proxy URL"));
    assert!(t
        .auth
        .model_catalog("test")
        .await
        .unwrap_err()
        .contains("代理"));
    assert_eq!(t.f.calls.load(Ordering::SeqCst), 0);
    assert_eq!(before, rows(&t).await);
}
#[tokio::test]
async fn in_flight_rotations_deletion_expiry_and_identity_changes_discard_results() {
    for case in 0..5 {
        let t = setup().await;
        t.f.hold.store(true, Ordering::SeqCst);
        let auth = t.auth.clone();
        let task = tokio::spawn(async move { auth.model_catalog("test").await });
        tokio::time::timeout(Duration::from_secs(2), t.f.entered.notified())
            .await
            .unwrap();
        // This fails if the native gate is held across network await.
        let guard = tokio::time::timeout(Duration::from_secs(2), t.auth.core.gate.lock())
            .await
            .unwrap();
        let mut c = credential();
        match case {
            0 => c.pair.access = "synthetic-new-access".into(),
            1 => c.pair.access_expires_at_ms = Some(now() - 1),
            2 => {
                c.uid = "another-owner".into();
                c.snapshot.identity_hash =
                    Some(format!("{:x}", Sha256::digest(b"trae_cn:another-owner")));
            }
            3 => {}
            _ => {}
        }
        if case == 3 {
            sqlx::query("DELETE FROM app_secrets WHERE name=?")
                .bind(key("session", "test"))
                .execute(t.db.pool())
                .await
                .unwrap();
        } else {
            seed(&t, "test", &c).await;
        }
        // case 4 re-encrypts the very same principal/token; ciphertext generation still changes.
        drop(guard);
        let after_mutation = rows(&t).await;
        t.f.release.notify_one();
        assert!(task.await.unwrap().is_err());
        assert_eq!(after_mutation, rows(&t).await);
        assert_eq!(t.f.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn disjoint_account_updates_do_not_invalidate_current_account() {
    let t = setup().await;
    t.f.hold.store(true, Ordering::SeqCst);
    let auth = t.auth.clone();
    let task = tokio::spawn(async move { auth.model_catalog("test").await });
    tokio::time::timeout(Duration::from_secs(2), t.f.entered.notified())
        .await
        .unwrap();
    seed(&t, "other", &credential()).await;
    let before = rows(&t).await;
    t.f.release.notify_one();
    assert!(task.await.unwrap().is_ok());
    assert_eq!(before, rows(&t).await);
}
#[tokio::test]
async fn total_deadline_includes_streamed_body() {
    let t = setup().await;
    t.f.delayed_body.store(true, Ordering::SeqCst);
    let before = rows(&t).await;
    let start = std::time::Instant::now();
    let error = t.auth.model_catalog("test").await.unwrap_err();
    assert!(error.contains("超时"));
    assert!(start.elapsed() >= Duration::from_secs(19));
    assert!(start.elapsed() < Duration::from_secs(24));
    assert_eq!(t.f.calls.load(Ordering::SeqCst), 1);
    assert_eq!(before, rows(&t).await);
}
#[tokio::test]
async fn malformed_json_and_business_failure_are_static_network_errors() {
    let t = setup().await;
    let before = rows(&t).await;
    for body in [
        "not-json-server-message",
        r#"{"code":17,"message":"untrusted-secret"}"#,
        r#"{"Result":{"code":0,"data":{"list":[]}}}"#,
        r#"{"code":0,"data":{"list":[{"function":"solo_agent_remote","models":[{}]}]}}"#,
    ] {
        *t.f.body.lock() = body.into();
        let error = t.auth.model_catalog("test").await.unwrap_err();
        assert!(!error.contains("untrusted"));
        assert!(!error.contains("not-json"));
    }
    assert_eq!(t.f.calls.load(Ordering::SeqCst), 4);
    assert_eq!(before, rows(&t).await);
}
#[tokio::test]
async fn naturally_expired_inflight_access_is_discarded_without_rewriting_credentials() {
    let t = setup().await;
    let mut c = credential();
    c.pair.access_expires_at_ms = Some(now() + 500);
    seed(&t, "test", &c).await;
    t.f.hold.store(true, Ordering::SeqCst);
    let auth = t.auth.clone();
    let task = tokio::spawn(async move { auth.model_catalog("test").await });
    tokio::time::timeout(Duration::from_secs(2), t.f.entered.notified())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(550)).await;
    let before = rows(&t).await;
    t.f.release.notify_one();
    assert!(task.await.unwrap().is_err());
    assert_eq!(before, rows(&t).await);
}
#[tokio::test]
async fn known_alternate_cn_issuer_still_uses_fixed_remote_catalog_origin() {
    let t = setup().await;
    let mut c = credential();
    c.issuer = "https://api.trae.com.cn".into();
    seed(&t, "test", &c).await;
    assert_eq!(t.auth.model_catalog("test").await.unwrap().origin, ORIGIN);
    assert_eq!(t.f.calls.load(Ordering::SeqCst), 1);
}
