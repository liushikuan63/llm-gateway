//! Synthetic encrypted session and loopback only. No real user database or key.
#[tokio::test]
async fn native_timeout_phase_distinguishes_owned_connect_and_request_without_replay() {
    use tracing::instrument::WithSubscriber;
    #[derive(Clone, Default)]
    struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    async {
        let t = setup().await;
        let http = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        *t.auth.core.http.write() = Ok(http.clone());
        let captured = t
            .auth
            .prepare_model_authorization(&binding("test"))
            .await
            .unwrap();
        let origin = &t.origin;
        let url = crate::proxy::trae::profile::operation_url(
            captured.binding.profile_id,
            NativeOperation::Model,
            Some(origin),
        )
        .unwrap();
        t.fixture.hold.store(true, Ordering::SeqCst);
        let started = AtomicBool::new(false);
        let request = http
            .post(url.clone())
            .json(&json!({"owned":"BODY_CANARY_NEVER_LOG"}))
            .timeout(std::time::Duration::from_millis(150))
            .build()
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            t.auth
                .execute_model_request(&captured, request, &started, || async { Ok(()) }),
        )
        .await
        .unwrap();
        assert_eq!(
            result.unwrap_err().as_str(),
            "native_model_deadline_exceeded"
        );
        assert!(started.load(Ordering::SeqCst));
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
        t.fixture.hold.store(false, Ordering::SeqCst);
        t.fixture.release.notify_one();
        // An owned proxy accepts TCP and stalls the proxy handshake; it never
        // parses/forwards CONNECT or connects to its target.
        // No target DNS, TLS, identity or provider HTTP request is performed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let seen = accepted.clone();
        let proxy = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            seen.fetch_add(1, Ordering::SeqCst);
            let _socket = socket;
            std::future::pending::<()>().await;
        });
        let http = reqwest::Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(format!("http://{address}")).unwrap())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_millis(150))
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        *t.auth.core.http.write() = Ok(http.clone());
        let url = crate::proxy::trae::profile::operation_url(
            captured.binding.profile_id,
            NativeOperation::Model,
            None,
        )
        .unwrap();
        let request = http
            .post(url)
            .json(&json!({"owned":"CONNECTION_CANARY_NEVER_LOG"}))
            .build()
            .unwrap();
        let started = AtomicBool::new(false);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            t.auth
                .execute_model_request(&captured, request, &started, || async { Ok(()) }),
        )
        .await
        .unwrap();
        proxy.abort();
        let _ = proxy.await;
        assert_eq!(
            result.unwrap_err().as_str(),
            "native_model_deadline_exceeded"
        );
        assert!(started.load(Ordering::SeqCst));
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
        // Ordinary owned send remains successful and creates no timeout record.
        let http = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        *t.auth.core.http.write() = Ok(http.clone());
        let request = http
            .post(
                crate::proxy::trae::profile::operation_url(
                    captured.binding.profile_id,
                    NativeOperation::Model,
                    Some(origin),
                )
                .unwrap(),
            )
            .json(&json!({}))
            .build()
            .unwrap();
        assert_eq!(
            t.auth
                .execute_model_request(&captured, request, &AtomicBool::new(false), || async {
                    Ok(())
                })
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 2);
    }
    .with_subscriber(subscriber)
    .await;
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert_eq!(
        logs.matches("Trae native transport timeout").count(),
        2,
        "each failed owned send needs one fixed phase diagnostic; success needs none"
    );
    assert!(logs.contains("phase=\"connect\""));
    assert!(logs.contains("phase=\"request\""));
    for forbidden in [
        "BODY_CANARY_NEVER_LOG",
        "CONNECTION_CANARY_NEVER_LOG",
        "synthetic-native-access",
        "127.0.0.1",
        "https://",
    ] {
        assert!(
            !logs.contains(forbidden),
            "diagnostic cannot contain values or URLs"
        );
    }
}

use super::*;
#[tokio::test]
async fn native_timeout_phase_body_uses_production_stream_without_false_terminal() {
    use tracing::instrument::WithSubscriber;
    #[derive(Clone, Default)]
    struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    async {
        let t=setup().await;
        t.fixture.model_sse.store(true,Ordering::SeqCst);t.fixture.stalled_body.store(true,Ordering::SeqCst);
        let http=reqwest::Client::builder().no_proxy().retry(reqwest::retry::never()).read_timeout(std::time::Duration::from_millis(150)).build().unwrap();
        *t.auth.core.http.write()=Ok(http);
        let selected:crate::agent_upstream::runtime::TraeModelSelection=serde_json::from_value(json!({"source_function":"solo_work_remote","selector_id":"owned-selector","config_name":"Exact-A","display_name":"Owned model","prompt_max_tokens":100000,"max_tokens":8192,"declared_vision":null,"declared_tools":null,"declared_thinking":null,"reasoning_effort_options":[],"protocol_carriers":{"text":true,"tools":true,"images":true,"audio":false,"video":false},"account_model_evidence":"unverified","catalog_revision":"12345678-1234-4234-9234-123456789abc","fetched_at_ms":1,"allow_undeclared_images":false})).unwrap();
        let request=crate::protocol::strict_account::parse_trae_openai(&json!({"model":"Exact-A","messages":[{"role":"user","content":"BODY_CANARY_NEVER_LOG"}]})).unwrap();
        for failing in [true,false] {
            t.fixture.stalled_body.store(failing,Ordering::SeqCst);
            *t.fixture.body.lock()="event: output\ndata: {\"response\":\"NORMAL_REPLY_CANARY_NEVER_LOG\"}\n\nevent: done\ndata: {\"finish_reason\":\"stop\"}\n\n".into();
            let started=Arc::new(AtomicBool::new(false));let usage=Arc::new(parking_lot::Mutex::new(crate::proxy::trae::sse::TraeUsageReport::default()));
            let stream=t.client.model_stream(&binding("test"),&selected,&request,std::time::Duration::from_secs(2),||async{Ok(())},crate::proxy::trae::transport::NativeCallEvidence{post_started:started.clone(),usage_report:usage.clone(),entitlement:None}).await.unwrap();
            let events=tokio::time::timeout(std::time::Duration::from_secs(3),stream.collect::<Vec<_>>()).await.unwrap();
            assert!(started.load(Ordering::SeqCst));assert_eq!(usage.lock().state,crate::proxy::trae::sse::TraeUsageState::Missing);
            if failing {
                assert!(events.iter().any(|e|e.as_ref().err().is_some_and(|e|e=="native_model_deadline_exceeded")));
                assert!(!events.iter().any(|e|matches!(e,Ok(crate::proxy::trae::sse::TraeEvent::Done{..}))));
            } else {
                assert!(events.iter().all(Result::is_ok));
                assert_eq!(events.iter().filter(|e|matches!(e,Ok(crate::proxy::trae::sse::TraeEvent::Done{..}))).count(),1);
            }
        }
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst),2,"one failed body and one explicit normal control only");
    }.with_subscriber(subscriber).await;
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert_eq!(
        logs.matches("Trae native model timeout").count(),
        1,
        "failed body must expose a fixed phase; normal completion must not"
    );
    assert!(logs.contains("phase=\"body\""));
    for forbidden in [
        "BODY_CANARY_NEVER_LOG",
        "NORMAL_REPLY_CANARY_NEVER_LOG",
        "OWNED_PREFIX_CANARY_NEVER_LOG",
        "synthetic-native-access",
        "127.0.0.1",
        "http://",
    ] {
        assert!(!logs.contains(forbidden));
    }
}

use crate::proxy::trae::{
    profile::{SourceFunction, TraeProfileId, CATALOG_PATH},
    TraeModelClient,
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Router,
};
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicUsize};
struct Fixture {
    body: parking_lot::Mutex<String>,
    status: parking_lot::Mutex<StatusCode>,
    calls: AtomicUsize,
    requests: parking_lot::Mutex<Vec<(String, String, HeaderMap, Value)>>,
    hold: AtomicBool,
    chunked: AtomicBool,
    stalled_body: AtomicBool,
    model_sse: AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
struct Setup {
    origin: Url,
    db: crate::db::Db,
    auth: TraeAuth,
    client: TraeModelClient,
    fixture: Arc<Fixture>,
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
    let body = axum::body::to_bytes(request.into_body(), 4096)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    f.calls.fetch_add(1, Ordering::SeqCst);
    f.requests.lock().push((method, uri, headers, body));
    f.entered.notify_one();
    if f.hold.load(Ordering::SeqCst) {
        f.release.notified().await;
    }
    let response = Response::builder()
        .status(*f.status.lock())
        .header(
            "content-type",
            if f.model_sse.load(Ordering::SeqCst) {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .header("location", "/must-not-follow");
    if f.stalled_body.load(Ordering::SeqCst) {
        let prefix = if f.model_sse.load(Ordering::SeqCst) {
            b"event: output\ndata: {\"response\":\"OWNED_PREFIX_CANARY_NEVER_LOG\"}\n\n".to_vec()
        } else {
            vec![b'{']
        };
        let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(prefix) })
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
fn credential() -> Credential {
    Credential {
        issuer: contract::ISSUER.into(),
        client_id: contract::CLIENT_ID.into(),
        uid: "native-owner-synthetic".into(),
        pair: Pair {
            access: "synthetic-native-access".into(),
            refresh: Some("synthetic-native-refresh".into()),
            access_expires_at_ms: Some(now() + 120_000),
            refresh_expires_at_ms: Some(now() + 240_000),
        },
        device: Device {
            device_id: "9876543210123456".into(),
            machine_id: Some("synthetic-native-machine".into()),
            device_name: "Fixture".into(),
            device_model: "中 文/+!~*'()".into(),
            device_brand: "UNUSED".into(),
            device_cpu: "x64".into(),
            os_info: "Windows".into(),
            os_version: "10.0".into(),
        },
        private_key: "synthetic-native-private".into(),
        public_key: "synthetic-native-public".into(),
        snapshot: AccountSnapshot {
            identity_hash: Some(format!(
                "{:x}",
                Sha256::digest(b"trae_cn:native-owner-synthetic")
            )),
            ..Default::default()
        },
    }
}
fn binding(account: &str) -> TraeModelBinding {
    TraeModelBinding {
        region: TraeRegion::Cn,
        account_id: account.into(),
        principal_hash: credential().snapshot.identity_hash.unwrap(),
        profile_id: TraeProfileId::CnOrdinary343Aicc20260212V1,
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
    use base64::Engine;
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        base64::engine::general_purpose::STANDARD.encode([73; 32]),
    );
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let fixture = Arc::new(Fixture {
        body: parking_lot::Mutex::new(
            json!({"config_info_list":[{"config_name":"Exact-A"}]}).to_string(),
        ),
        status: parking_lot::Mutex::new(StatusCode::OK),
        calls: AtomicUsize::new(0),
        requests: parking_lot::Mutex::new(Vec::new()),
        hold: AtomicBool::new(false),
        chunked: AtomicBool::new(false),
        stalled_body: AtomicBool::new(false),
        model_sse: AtomicBool::new(false),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let app = Router::new().fallback(handler).with_state(fixture.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let auth = TraeAuth::new(db.pool().clone(), None).unwrap();
    let client = TraeModelClient::for_test(auth.clone(), origin.clone());
    let t = Setup {
        origin,
        db,
        auth,
        client,
        fixture,
        task,
    };
    seed(&t, "test", &credential()).await;
    t
}
async fn catalog(t: &Setup) -> Result<crate::proxy::trae::catalog::TraeNativeCatalogView, String> {
    t.client
        .native_catalog(&binding("test"), SourceFunction::SoloWorkRemote)
        .await
}
#[tokio::test]
async fn native_official_lowercase_windows_catalog_preserves_wire_value() {
    let t = setup().await;
    let mut saved = credential();
    saved.device.os_info = "windows".into();
    seed(&t, "test", &saved).await;
    let result = catalog(&t).await;
    assert_eq!(
        t.fixture.calls.load(Ordering::SeqCst),
        1,
        "official windows must send one catalog POST; result={result:?}"
    );
    let view = result.expect("official lowercase windows must authorize");
    assert_eq!(view.entries[0].config_name, "Exact-A");
    let requests = t.fixture.requests.lock();
    let (method, path, headers, body) = &requests[0];
    assert_eq!(method, "POST");
    assert_eq!(path, CATALOG_PATH);
    assert_eq!(*body, SourceFunction::SoloWorkRemote.catalog_body());
    assert_eq!(headers["x-device-type"].as_bytes(), b"windows");
}
#[tokio::test]
async fn native_exact_device_os_variants_preserve_wire_and_session_ciphertext() {
    let t = setup().await;
    for os in ["windows", "mac", "linux", "Windows", "Darwin", "Linux"] {
        let mut saved = credential();
        saved.device.os_info = os.into();
        seed(&t, "test", &saved).await;
        let rows_before: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets ORDER BY name")
                .fetch_all(t.db.pool())
                .await
                .unwrap();
        let captured = t
            .auth
            .prepare_model_authorization(&binding("test"))
            .await
            .unwrap();
        // Both native consumers retain exactly the saved own-session bytes.
        for operation in [NativeOperation::Catalog, NativeOperation::Model] {
            let headers = captured.headers(operation).unwrap();
            assert_eq!(headers["x-device-type"].as_bytes(), os.as_bytes());
            assert!(headers["x-device-type"].is_sensitive());
        }
        let before = t.fixture.calls.load(Ordering::SeqCst);
        let view = catalog(&t).await.unwrap();
        assert_eq!(view.entries[0].config_name, "Exact-A");
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), before + 1);
        {
            let requests = t.fixture.requests.lock();
            assert_eq!(requests.len(), before + 1);
            let (method, path, headers, body) = &requests[before];
            assert_eq!(method, "POST");
            assert_eq!(path, CATALOG_PATH);
            assert_eq!(*body, SourceFunction::SoloWorkRemote.catalog_body());
            assert_eq!(headers["x-device-type"].as_bytes(), os.as_bytes());
        }
        let rows_after: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets ORDER BY name")
                .fetch_all(t.db.pool())
                .await
                .unwrap();
        assert_eq!(
            rows_before, rows_after,
            "{os}: no session rewrite or migration"
        );
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 6);
}
#[tokio::test]
async fn native_unknown_device_os_variants_reject_before_http() {
    let t = setup().await;
    for os in [
        "",
        " ",
        " windows",
        "windows ",
        "Windows ",
        "WINDOWS",
        "wIndows",
        "Windows 11",
        "win32",
        "darwin",
        "Mac",
        "MAC",
        "macOS",
        "MacOS",
        "LINUX",
        "lInux",
        "freebsd",
        "android",
        "ios",
        "Unverified description",
        "%77indows",
        "windows%20",
        "%6d%61%63",
        "%6c%69%6e%75%78",
        "windows/mac/linux",
        "windows\\linux",
        "windows\r",
        "windows\n",
        "windows\t",
        "windows\0",
        "windows%0D",
        "ｗindows",
    ] {
        let mut saved = credential();
        saved.device.os_info = os.into();
        seed(&t, "test", &saved).await;
        let before = t.fixture.calls.load(Ordering::SeqCst);
        let requests_before = t.fixture.requests.lock().len();
        let expected = if os.chars().any(char::is_control) {
            "native_model_authorization_invalid"
        } else {
            "native_device_profile_unverified"
        };
        let result = catalog(&t).await;
        assert_eq!(result.unwrap_err(), expected, "rejected OS {os:?}");
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), before, "OS {os:?}");
        assert_eq!(
            t.fixture.requests.lock().len(),
            requests_before,
            "OS {os:?}"
        );
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn native_accepted_device_os_does_not_enable_global_bootstrap() {
    let t = setup().await;
    for os in ["windows", "mac", "linux", "Windows", "Darwin", "Linux"] {
        let mut saved = credential();
        saved.device.os_info = os.into();
        seed(&t, "test", &saved).await;
        let mut global = binding("test");
        global.region = TraeRegion::Global;
        let before = t.fixture.calls.load(Ordering::SeqCst);
        let result = t
            .client
            .native_catalog(&global, SourceFunction::SoloWorkRemote)
            .await;
        assert_eq!(result.unwrap_err(), "native_device_bootstrap_unavailable");
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), before);
        assert!(t.fixture.requests.lock().is_empty());
    }
}
#[tokio::test]
async fn native_production_start_shape_omits_optional_machine_and_cpu() {
    let t = setup().await;
    let mut saved = credential();
    // Matches start_with_session_resolver's saved Device shape. All populated
    // values below are synthetic fixture values, never real client context.
    saved.device = Device {
        device_id: saved.device.device_id,
        machine_id: None,
        device_name: String::new(),
        device_model: saved.device.device_model,
        device_brand: String::new(),
        device_cpu: String::new(),
        os_info: saved.device.os_info,
        os_version: saved.device.os_version,
    };
    saved.validate().unwrap();
    seed(&t, "test", &saved).await;
    let before = t.auth.core.read(&key("session", "test")).await.unwrap();
    let captured = t
        .auth
        .prepare_model_authorization(&binding("test"))
        .await
        .expect("normal own-session Device shape must authorize locally");
    let headers = captured.headers(NativeOperation::Catalog).unwrap();
    assert!(!headers.contains_key("x-machine-id"));
    assert!(!headers.contains_key("x-device-cpu"));
    assert_eq!(headers["x-device-id"], saved.device.device_id);
    assert_eq!(
        headers["x-device-brand"],
        encode_component(&saved.device.device_model)
    );
    let view = catalog(&t).await.unwrap();
    assert_eq!(view.entries[0].config_name, "Exact-A");
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    {
        let requests = t.fixture.requests.lock();
        let (method, path, sent, body) = &requests[0];
        assert_eq!(method, "POST");
        assert_eq!(path, CATALOG_PATH);
        assert_eq!(*body, SourceFunction::SoloWorkRemote.catalog_body());
        assert!(!sent.contains_key("x-machine-id"));
        assert!(!sent.contains_key("x-device-cpu"));
        assert_eq!(sent["x-device-id"], saved.device.device_id);
    }
    assert_eq!(
        before,
        t.auth.core.read(&key("session", "test")).await.unwrap()
    );
}
#[tokio::test]
async fn native_present_machine_preserves_encoding_and_invalid_values_never_send() {
    let t = setup().await;
    let mut saved = credential();
    saved.device.machine_id = Some("机 器/+!~*'()".into());
    seed(&t, "test", &saved).await;
    let captured = t
        .auth
        .prepare_model_authorization(&binding("test"))
        .await
        .unwrap();
    let headers = captured.headers(NativeOperation::Catalog).unwrap();
    let encoded = "%E6%9C%BA%20%E5%99%A8%2F%2B!~*'()";
    assert_eq!(headers["x-machine-id"], encoded);
    assert!(headers["x-machine-id"].is_sensitive());
    catalog(&t).await.unwrap();
    assert_eq!(t.fixture.requests.lock()[0].2["x-machine-id"], encoded);
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    for invalid in [
        String::new(),
        "control\rvalue".into(),
        "x".repeat(129),
        "0".into(),
        "0000000000000000".into(),
    ] {
        saved.device.machine_id = Some(invalid);
        seed(&t, "test", &saved).await;
        assert!(t
            .auth
            .prepare_model_authorization(&binding("test"))
            .await
            .is_err());
        assert!(catalog(&t).await.is_err());
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn native_post_exact_profile_body_headers_once_and_safe_ticket() {
    let t = setup().await;
    let rows_before: Vec<(String, String)> =
        sqlx::query_as("SELECT name,value_enc FROM app_secrets ORDER BY name")
            .fetch_all(t.db.pool())
            .await
            .unwrap();
    let view = catalog(&t).await.unwrap();
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    {
        let requests = t.fixture.requests.lock();
        let (method, path, headers, body) = &requests[0];
        assert_eq!(method, "POST");
        assert_eq!(path, CATALOG_PATH);
        assert_eq!(*body, SourceFunction::SoloWorkRemote.catalog_body());
        assert_eq!(headers["x-ide-token"], "synthetic-native-access");
        assert_eq!(headers["x-app-id"], APP_ID);
        assert_eq!(headers["x-ide-version-code"], VERSION_CODE);
        assert_eq!(
            headers["x-device-brand"],
            "%E4%B8%AD%20%E6%96%87%2F%2B!~*'()"
        );
        assert_eq!(headers["x-device-type"], "Windows");
        for forbidden in [
            "authorization",
            "x-cloudide-token",
            "x-uid",
            "x-app-version-code",
        ] {
            assert!(!headers.contains_key(forbidden));
        }
    }
    let ticket = t
        .client
        .consume_cached_catalog(
            &binding("test"),
            SourceFunction::SoloWorkRemote,
            &view.catalog_revision,
            &view.entries[0].selector_id,
        )
        .await
        .unwrap();
    assert_eq!(ticket.entry().config_name, "Exact-A");
    assert_eq!(ticket.binding(), &binding("test"));
    assert_eq!(ticket.source_function(), SourceFunction::SoloWorkRemote);
    assert_eq!(ticket.catalog_revision(), view.catalog_revision);
    t.auth.set_proxy(None);
    t.auth.set_proxy(Some("  "));
    t.client.validate_ticket(&ticket).await.unwrap();
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    let captured = t
        .auth
        .prepare_model_authorization(&binding("test"))
        .await
        .unwrap();
    assert!(captured.headers(NativeOperation::Catalog).unwrap()["x-ide-token"].is_sensitive());
    let rows_after: Vec<(String, String)> =
        sqlx::query_as("SELECT name,value_enc FROM app_secrets ORDER BY name")
            .fetch_all(t.db.pool())
            .await
            .unwrap();
    assert_eq!(rows_before, rows_after);
}
#[tokio::test]
async fn native_no_retry_on_http_and_json_errors() {
    let t = setup().await;
    for (index, status) in [401, 429, 307, 500].into_iter().enumerate() {
        *t.fixture.status.lock() = StatusCode::from_u16(status).unwrap();
        assert!(catalog(&t).await.is_err());
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), index + 1);
    }
    *t.fixture.status.lock() = StatusCode::OK;
    for packet in [
        "not-json",
        r#"{"code":1,"config_info_list":[]}"#,
        r#"{"ResponseMetadata":{"Error":{}},"config_info_list":[]}"#,
    ] {
        let before = t.fixture.calls.load(Ordering::SeqCst);
        *t.fixture.body.lock() = packet.into();
        assert!(catalog(&t).await.is_err());
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), before + 1);
    }
}
#[tokio::test]
async fn native_invalid_binding_global_expiry_device_and_proxy_never_send() {
    let t = setup().await;
    let mut wrong = binding("test");
    wrong.principal_hash = "b".repeat(64);
    assert!(t
        .client
        .native_catalog(&wrong, SourceFunction::SoloWorkRemote)
        .await
        .is_err());
    wrong = binding("test");
    wrong.region = TraeRegion::Global;
    assert_eq!(
        t.client
            .native_catalog(&wrong, SourceFunction::SoloWorkRemote)
            .await
            .unwrap_err(),
        "native_device_bootstrap_unavailable"
    );
    for mutation in 0..7 {
        let mut c = credential();
        match mutation {
            0 => c.pair.access_expires_at_ms = None,
            1 => c.pair.access_expires_at_ms = Some(now() - 1),
            2 => c.uid = "0".into(),
            3 => c.device.device_id = "0000000000000000".into(),
            4 => c.client_id = "other".into(),
            5 => c.issuer = "https://invalid.test".into(),
            _ => c.device.os_info = "Unverified description".into(),
        }
        seed(&t, "test", &c).await;
        assert!(catalog(&t).await.is_err());
    }
    seed(&t, "test", &credential()).await;
    t.auth.set_proxy(Some("file:///invalid"));
    assert!(catalog(&t).await.is_err());
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
    t.auth.set_proxy(None);
    assert!(catalog(&t).await.is_ok());
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn native_auth_current_rejects_generation_instance_and_proxy_epoch() {
    let t = setup().await;
    let captured = t
        .auth
        .prepare_model_authorization(&binding("test"))
        .await
        .unwrap();
    let request = reqwest::Client::new()
        .post("https://invalid.test/credential-sink")
        .build()
        .unwrap();
    assert!(t
        .auth
        .execute_model_catalog(&captured, request)
        .await
        .is_err());
    t.auth.model_authorization_current(&captured).await.unwrap();
    let other = TraeAuth::new(t.db.pool().clone(), None).unwrap();
    assert!(other.model_authorization_current(&captured).await.is_err());
    seed(&t, "test", &credential()).await;
    assert!(t.auth.model_authorization_current(&captured).await.is_err());
    let current = t
        .auth
        .prepare_model_authorization(&binding("test"))
        .await
        .unwrap();
    t.auth.set_proxy(Some("  "));
    t.auth.model_authorization_current(&current).await.unwrap();
    t.auth.set_proxy(Some("http://127.0.0.1:17890"));
    assert!(t.auth.model_authorization_current(&current).await.is_err());
    let updated = t
        .auth
        .prepare_model_authorization(&binding("test"))
        .await
        .unwrap();
    t.auth.set_proxy(Some(" http://127.0.0.1:17890/ "));
    t.auth.model_authorization_current(&updated).await.unwrap();
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn native_late_response_discarded_after_refresh_logout_relogin_and_proxy_change() {
    for mutation in 0..4 {
        let t = setup().await;
        t.fixture.hold.store(true, Ordering::SeqCst);
        let client = t.client.clone();
        let handle = tokio::spawn(async move {
            client
                .native_catalog(&binding("test"), SourceFunction::SoloWorkRemote)
                .await
        });
        t.fixture.entered.notified().await;
        match mutation {
            0 => seed(&t, "test", &credential()).await,
            1 => {
                sqlx::query("DELETE FROM app_secrets WHERE name=?")
                    .bind(key("session", "test"))
                    .execute(t.db.pool())
                    .await
                    .unwrap();
            }
            2 => {
                let pending = Pending {
                    account: "test".into(),
                    id: "pending".into(),
                    deadline: now() + 60_000,
                    baseline: None,
                    expected_identity_hash: None,
                    state: "waiting".into(),
                    device: credential().device,
                    material: Material {
                        code_verifier: "x".into(),
                        code_challenge: "x".into(),
                        challenge_method: "S256".into(),
                        private_key: "x".into(),
                        public_key: "x".into(),
                    },
                };
                repo::set_secret(
                    t.db.pool(),
                    &key("pending", "pending"),
                    &encode(&pending).unwrap(),
                )
                .await
                .unwrap();
            }
            _ => t.auth.set_proxy(Some("http://127.0.0.1:17890")),
        }
        t.fixture.release.notify_one();
        assert!(handle.await.unwrap().is_err());
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn native_raw_escaped_value_and_key_reflections_are_rejected() {
    let t = setup().await;
    for secret in [
        credential().pair.access,
        credential().pair.refresh.unwrap(),
        credential().private_key,
        credential().public_key,
        credential().uid,
        credential().device.device_id,
        credential().device.machine_id.unwrap(),
    ] {
        for key in [false, true] {
            let escaped = secret
                .chars()
                .map(|c| format!("\\u{:04x}", u32::from(c)))
                .collect::<String>();
            *t.fixture.body.lock() = if key {
                format!("{{\"config_info_list\":[],\"{escaped}\":null}}")
            } else {
                format!("{{\"config_info_list\":[],\"unknown\":\"{escaped}\"}}")
            };
            assert_eq!(
                catalog(&t).await.unwrap_err(),
                "native_catalog_sensitive_reflection"
            );
        }
        *t.fixture.body.lock() = json!({"config_info_list":[],"unknown":secret}).to_string();
        assert_eq!(
            catalog(&t).await.unwrap_err(),
            "native_catalog_sensitive_reflection"
        );
    }
}
#[tokio::test]
async fn native_tickets_are_local_cross_account_generation_and_failure_bound() {
    let t = setup().await;
    let view = catalog(&t).await.unwrap();
    let selector = &view.entries[0].selector_id;
    seed(&t, "other", &credential()).await;
    assert!(t
        .client
        .consume_cached_catalog(
            &binding("other"),
            SourceFunction::SoloWorkRemote,
            &view.catalog_revision,
            selector
        )
        .await
        .is_err());
    assert!(t
        .client
        .consume_cached_catalog(
            &binding("test"),
            SourceFunction::SoloWorkRemote,
            &view.catalog_revision,
            "arbitrary"
        )
        .await
        .is_err());
    let ticket = t
        .client
        .consume_cached_catalog(
            &binding("test"),
            SourceFunction::SoloWorkRemote,
            &view.catalog_revision,
            selector,
        )
        .await
        .unwrap();
    seed(&t, "test", &credential()).await;
    assert!(t.client.validate_ticket(&ticket).await.is_err());
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    let view = catalog(&t).await.unwrap();
    *t.fixture.status.lock() = StatusCode::UNAUTHORIZED;
    assert!(catalog(&t).await.is_err());
    assert!(t
        .client
        .consume_cached_catalog(
            &binding("test"),
            SourceFunction::SoloWorkRemote,
            &view.catalog_revision,
            &view.entries[0].selector_id
        )
        .await
        .is_err());
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn native_cache_expiry_capacity_and_cancellation_never_fetch_implicitly() {
    let t = setup().await;
    let first = catalog(&t).await.unwrap();
    for i in 0..64 {
        let account = format!("account{i}");
        seed(&t, &account, &credential()).await;
        t.client
            .native_catalog(&binding(&account), SourceFunction::SoloWorkRemote)
            .await
            .unwrap();
    }
    assert!(t
        .client
        .consume_cached_catalog(
            &binding("test"),
            SourceFunction::SoloWorkRemote,
            &first.catalog_revision,
            &first.entries[0].selector_id
        )
        .await
        .is_err());
    let current = catalog(&t).await.unwrap();
    t.client.expire_cached_catalogs_for_test();
    assert!(t
        .client
        .consume_cached_catalog(
            &binding("test"),
            SourceFunction::SoloWorkRemote,
            &current.catalog_revision,
            &current.entries[0].selector_id
        )
        .await
        .is_err());
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 66);
    t.fixture.hold.store(true, Ordering::SeqCst);
    let client = t.client.clone();
    let handle = tokio::spawn(async move {
        client
            .native_catalog(&binding("test"), SourceFunction::SoloWorkRemote)
            .await
    });
    t.fixture.entered.notified().await; // Notify can still contain previous response; wait for actual new request below.
    while t.fixture.calls.load(Ordering::SeqCst) < 67 {
        tokio::task::yield_now().await;
    }
    handle.abort();
    let _ = handle.await;
    t.fixture.hold.store(false, Ordering::SeqCst);
    t.fixture.release.notify_one();
    assert!(catalog(&t).await.is_ok());
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 68);
}
#[tokio::test]
async fn native_byte_and_nesting_bounds_reject_with_one_post() {
    let t = setup().await;
    *t.fixture.body.lock() = " ".repeat(1024 * 1024 + 1);
    assert_eq!(catalog(&t).await.unwrap_err(), "native_catalog_body_limit");
    t.fixture.chunked.store(true, Ordering::SeqCst);
    assert_eq!(catalog(&t).await.unwrap_err(), "native_catalog_body_limit");
    t.fixture.chunked.store(false, Ordering::SeqCst);
    *t.fixture.body.lock() = format!("{}0{}", "[".repeat(33), "]".repeat(33));
    assert_eq!(
        catalog(&t).await.unwrap_err(),
        "native_catalog_nesting_invalid"
    );
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn native_total_deadline_includes_auth_lock_and_body() {
    let t = setup().await;
    let guard = t.auth.core.gate.lock().await;
    let start = std::time::Instant::now();
    assert_eq!(
        catalog(&t).await.unwrap_err(),
        "native_catalog_deadline_exceeded"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(22));
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
    drop(guard);
    t.fixture.stalled_body.store(true, Ordering::SeqCst);
    let start = std::time::Instant::now();
    assert!(catalog(&t).await.is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(22));
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
}
