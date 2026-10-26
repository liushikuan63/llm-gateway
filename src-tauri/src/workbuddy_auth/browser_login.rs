//! Global native browser-code prerequisite. This is not a bill reader or a website session.
//! No command/UI calls this API yet. A live call creates a login capability and transmits
//! the selected account's access and optional refresh credentials; approve that separately.
use super::*;

const ROUTE: &str = "/v2/plugin/device/auth/code";
const INVALID: &str = "网页登录码响应无效，未重试或打开网页";
const CHANGED: &str = "账号来源、身份或有效期已变化，未发布网页登录码";

/// A locally consumed secret capability, deliberately without Debug/Serialize/Clone.
/// The returned String can be copied; this proves no server-side single-use property,
/// supplier TTL units, billing audience or entitlement.
pub struct BrowserLoginCode {
    code: String,
    supplier_expires_in: Option<u64>,
}
impl BrowserLoginCode {
    pub fn supplier_expiry_observation(&self) -> Option<u64> {
        self.supplier_expires_in
    }
    /// Consume only in a separately reviewed browser-session exchange. Never log or persist.
    pub fn into_sensitive_code(self) -> String {
        self.code
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    device_code: String,
    #[serde(alias = "expires_in")]
    expires_in: Option<u64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    code: Option<i64>,
    data: Option<Payload>,
    device_code: Option<String>,
    #[serde(alias = "expires_in")]
    expires_in: Option<u64>,
}

fn parse(raw: &[u8], credential: &Credential) -> Result<BrowserLoginCode, String> {
    let value: Envelope = serde_json::from_slice(raw).map_err(|_| INVALID)?;
    if value.code.is_some_and(|code| code != 0) {
        return Err(INVALID.into());
    }
    let payload = match (value.data, value.device_code, value.expires_in) {
        (Some(data), None, None) => data,
        (None, Some(device_code), expires_in) => Payload {
            device_code,
            expires_in,
        },
        _ => return Err(INVALID.into()),
    };
    if payload.device_code.is_empty()
        || payload.device_code.len() > 2048
        || !payload.device_code.bytes().all(|b| b.is_ascii_graphic())
        || payload.expires_in == Some(0)
        || [
            Some(credential.access_token.as_str()),
            credential.refresh_token.as_deref(),
            credential.uid.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|secret| !secret.is_empty() && payload.device_code.contains(secret))
    {
        return Err(INVALID.into());
    }
    Ok(BrowserLoginCode {
        code: payload.device_code,
        supplier_expires_in: payload.expires_in,
    })
}

fn current_identity(credential: &Credential, expected: &str, now: i64) -> Result<(), String> {
    validate_credential(credential, WorkBuddyRegion::Global).map_err(|_| CHANGED)?;
    let uid = credential
        .uid
        .as_deref()
        .filter(|uid| uid.len() <= 256 && valid_token(uid))
        .ok_or(CHANGED)?;
    let identity = WorkBuddyRegion::Global.identity_hash(uid);
    if expected != identity
        || credential.snapshot.identity_hash.as_deref() != Some(identity.as_str())
        || credential.enterprise_id.is_some()
        || !credential
            .access_expires_at_ms
            .is_some_and(|expiry| expiry > now)
        || credential
            .refresh_expires_at_ms
            .is_some_and(|expiry| expiry <= now)
    {
        return Err(CHANGED.into());
    }
    Ok(())
}

impl WorkBuddyAuth {
    /// Explicit opt-in for one native code POST, not a read-only bill request.
    /// No refresh, lookup, redirect, retry, fallback, cookie, navigation or DB write.
    /// Only the Global installed-client producer contract is currently established.
    /// A transport/cancellation failure may already have issued a code: never auto-replay.
    pub async fn create_browser_login_code(
        &self,
        account: &str,
        expected_identity: &str,
        explicitly_enabled: bool,
    ) -> Result<BrowserLoginCode, String> {
        if !explicitly_enabled {
            return Err("未明确启用网页登录码，本次零发送".into());
        }
        if self.region != WorkBuddyRegion::Global || !valid_id(account) {
            return Err("该区域或账号没有已核验的网页登录码合同，本次零发送".into());
        }
        let _guard = self.gate.lock().await;
        let key = self.session_key(account);
        let baseline = self.read(&key).await?.ok_or(CHANGED)?;
        let credential: Credential = decode(&baseline).map_err(|_| CHANGED)?;
        current_identity(&credential, expected_identity, now_ms())?;
        // This second pure read is the final asynchronous pre-send generation boundary.
        if self.read(&key).await?.as_deref() != Some(baseline.as_str()) {
            return Err(CHANGED.into());
        }
        current_identity(&credential, expected_identity, now_ms())?;
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in [
            (
                "authorization",
                Some(format!("Bearer {}", credential.access_token)),
            ),
            ("x-user-id", credential.uid.clone()),
            ("x-domain", Some(credential.domain.clone())),
            ("x-refresh-token", credential.refresh_token.clone()),
        ] {
            if let Some(value) = value {
                let mut header = HeaderValue::from_str(&value).map_err(|_| CHANGED)?;
                header.set_sensitive(true);
                headers.insert(name, header);
            }
        }
        // The preferred official producer sends no JSON body; the generic login
        // helper adds {} and drops one of the two credentials, so do not reuse it.
        let response = self
            .client
            .post(format!("{}{}", self.base, ROUTE))
            .headers(headers)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|_| "网页登录码发送或交付结果未知，未重试")?;
        if !response.status().is_success() {
            return Err("网页登录码服务拒绝请求，未重试或打开网页".into());
        }
        let mut stream = response.bytes_stream();
        let mut raw = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| INVALID)?;
            if raw.len() + chunk.len() > 16 * 1024 {
                return Err(INVALID.into());
            }
            raw.extend_from_slice(&chunk);
        }
        if self.read(&key).await?.as_deref() != Some(baseline.as_str()) {
            return Err(CHANGED.into());
        }
        current_identity(&credential, expected_identity, now_ms())?;
        parse(&raw, &credential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
        Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    type Seen = Arc<parking_lot::Mutex<Vec<(HeaderMap, Bytes)>>>;
    #[derive(Clone)]
    struct ServerState {
        seen: Seen,
        count: Arc<AtomicUsize>,
        unexpected: Arc<AtomicUsize>,
        reply: Arc<parking_lot::Mutex<(StatusCode, String)>>,
        blocked: Arc<std::sync::atomic::AtomicBool>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    struct Fixture {
        auth: WorkBuddyAuth,
        state: ServerState,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn fixture() -> Fixture {
        assert!(
            std::env::var_os("LLMGW_MASTER_KEY").is_some(),
            "gate must provide artificial process-only key"
        );
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let state = ServerState {
            seen: Arc::default(),
            count: Arc::new(AtomicUsize::new(0)),
            unexpected: Arc::new(AtomicUsize::new(0)),
            reply: Arc::new(parking_lot::Mutex::new((
                StatusCode::OK,
                r#"{"code":0,"data":{"deviceCode":"owned-fixture-browser-code","expiresIn":300}}"#
                    .into(),
            ))),
            blocked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            entered: Arc::default(),
            release: Arc::default(),
        };
        async fn handler(
            State(s): State<ServerState>,
            headers: HeaderMap,
            body: Bytes,
        ) -> axum::response::Response {
            s.count.fetch_add(1, Ordering::SeqCst);
            s.seen.lock().push((headers, body));
            if s.blocked.load(Ordering::SeqCst) {
                s.entered.notify_one();
                s.release.notified().await;
            }
            let (status, body) = s.reply.lock().clone();
            let mut response = (status, body).into_response();
            if status.is_redirection() {
                response.headers_mut().insert(
                    "location",
                    HeaderValue::from_static("/owned-unexpected-redirect"),
                );
            }
            response
        }
        async fn unexpected(State(s): State<ServerState>) -> StatusCode {
            s.unexpected.fetch_add(1, Ordering::SeqCst);
            StatusCode::INTERNAL_SERVER_ERROR
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route(ROUTE, post(handler))
            .fallback(unexpected)
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        // Normal production factory and normal method; only the owned issuer is substituted.
        let auth = WorkBuddyAuth::with_test_origin(db.pool().clone(), origin);
        auth.test_seed_account(
            "owned",
            "owned-browser-principal",
            "owned-browser-access",
            "owned-browser-refresh",
        )
        .await;
        change(&auth, |c| {
            c.access_expires_at_ms = Some(now_ms() + 60_000);
            c.refresh_expires_at_ms = Some(now_ms() + 120_000);
        })
        .await;
        Fixture { auth, state, task }
    }
    fn identity() -> String {
        WorkBuddyRegion::Global.identity_hash("owned-browser-principal")
    }
    async fn change(auth: &WorkBuddyAuth, f: impl FnOnce(&mut Credential)) {
        let key = auth.session_key("owned");
        let mut value: Credential = decode(&auth.read(&key).await.unwrap().unwrap()).unwrap();
        f(&mut value);
        repo::set_secret(&auth.pool, &key, &encode(&value).unwrap())
            .await
            .unwrap();
    }
    async fn issue(f: &Fixture) -> Result<BrowserLoginCode, String> {
        f.auth
            .create_browser_login_code("owned", &identity(), true)
            .await
    }
    fn counts(f: &Fixture, expected: usize) {
        assert_eq!(f.state.count.load(Ordering::SeqCst), expected);
        assert_eq!(f.state.unexpected.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn preferred_code_wire_uses_current_native_headers_and_no_body() {
        let f = fixture().await;
        let key = f.auth.session_key("owned");
        let before = f.auth.read(&key).await.unwrap();
        let value = issue(&f).await.unwrap();
        assert_eq!(value.supplier_expiry_observation(), Some(300));
        assert_eq!(value.into_sensitive_code(), "owned-fixture-browser-code");
        counts(&f, 1);
        assert_eq!(f.auth.read(&key).await.unwrap(), before);
        let seen = f.state.seen.lock();
        let (headers, body) = &seen[0];
        assert_eq!(headers["authorization"], "Bearer owned-browser-access");
        assert_eq!(headers["x-refresh-token"], "owned-browser-refresh");
        assert_eq!(headers["x-user-id"], "owned-browser-principal");
        assert_eq!(headers["x-domain"], "https://www.workbuddy.ai");
        assert_eq!(headers["content-type"], "application/json");
        for key in [
            "x-no-user-id",
            "x-no-enterprise-id",
            "x-no-department-info",
            "x-auth-refresh-source",
            "cookie",
        ] {
            assert!(!headers.contains_key(key));
        }
        assert!(body.is_empty());
    }
    #[tokio::test]
    async fn disabled_unproved_region_and_invalid_capture_are_zero_send() {
        let f = fixture().await;
        assert!(f
            .auth
            .create_browser_login_code("owned", &identity(), false)
            .await
            .is_err());
        let mut cn = f.auth.clone();
        cn.region = WorkBuddyRegion::Cn;
        assert!(cn
            .create_browser_login_code("owned", &identity(), true)
            .await
            .is_err());
        assert!(f
            .auth
            .create_browser_login_code("missing", &identity(), true)
            .await
            .is_err());
        assert!(f
            .auth
            .create_browser_login_code("owned", &"0".repeat(64), true)
            .await
            .is_err());
        change(&f.auth, |c| c.access_expires_at_ms = None).await;
        assert!(issue(&f).await.is_err());
        change(&f.auth, |c| c.access_expires_at_ms = Some(now_ms() - 1)).await;
        assert!(issue(&f).await.is_err());
        change(&f.auth, |c| {
            c.access_expires_at_ms = Some(now_ms() + 60_000);
            c.enterprise_id = Some("owned-enterprise".into());
        })
        .await;
        assert!(issue(&f).await.is_err());
        counts(&f, 0);
        for case in 0..3 {
            let f = fixture().await;
            change(&f.auth, |c| match case {
                0 => c.uid = None,
                1 => c.snapshot.identity_hash = Some("0".repeat(64)),
                2 => c.refresh_expires_at_ms = Some(now_ms() - 1),
                _ => unreachable!(),
            })
            .await;
            assert!(issue(&f).await.is_err());
            counts(&f, 0);
        }
    }
    #[tokio::test]
    async fn expiry_after_waiting_for_database_still_prevents_send() {
        let f = fixture().await;
        change(&f.auth, |c| c.access_expires_at_ms = Some(now_ms() + 200)).await;
        let connection = f.auth.pool.acquire().await.unwrap();
        let auth = f.auth.clone();
        let pending = tokio::spawn(async move {
            auth.create_browser_login_code("owned", &identity(), true)
                .await
                .map(|_| ())
        });
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!pending.is_finished());
        counts(&f, 0);
        drop(connection);
        assert!(pending.await.unwrap().is_err());
        counts(&f, 0);
    }
    #[tokio::test]
    async fn replacement_during_response_discards_code_without_replay() {
        let f = fixture().await;
        f.state.blocked.store(true, Ordering::SeqCst);
        let auth = f.auth.clone();
        let pending = tokio::spawn(async move {
            auth.create_browser_login_code("owned", &identity(), true)
                .await
                .map(|_| ())
        });
        tokio::time::timeout(Duration::from_secs(3), f.state.entered.notified())
            .await
            .unwrap();
        counts(&f, 1);
        // Same tokens and UID, fresh encrypted source generation must still invalidate it.
        change(&f.auth, |_| {}).await;
        f.state.release.notify_one();
        assert_eq!(pending.await.unwrap().err().unwrap(), CHANGED);
        counts(&f, 1);
    }
    #[tokio::test]
    async fn cancellation_drops_owned_wait_and_releases_gate_without_replay() {
        let f = fixture().await;
        f.state.blocked.store(true, Ordering::SeqCst);
        let auth = f.auth.clone();
        let pending = tokio::spawn(async move {
            auth.create_browser_login_code("owned", &identity(), true)
                .await
                .map(|_| ())
        });
        tokio::time::timeout(Duration::from_secs(3), f.state.entered.notified())
            .await
            .unwrap();
        counts(&f, 1);
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        let guard = tokio::time::timeout(Duration::from_secs(1), f.auth.gate.lock())
            .await
            .unwrap();
        drop(guard);
        f.state.release.notify_one();
        counts(&f, 1);
    }
    #[tokio::test]
    async fn response_failures_remain_bounded_and_never_retry_or_echo_secrets() {
        for (status, body) in [
            (302, "owned-browser-refresh".into()),
            (401, r#"{"error":"owned-browser-access"}"#.to_owned()),
            (503, "owned-browser-refresh".into()),
            (
                200,
                r#"{"code":9,"data":{"deviceCode":"owned-browser-access"}}"#.into(),
            ),
            (
                200,
                r#"{"code":0,"data":{"deviceCode":"owned-browser-refresh"}}"#.into(),
            ),
            (
                200,
                r#"{"data":{"deviceCode":"owned-browser-principal"}}"#.into(),
            ),
            (
                200,
                r#"{"data":{"deviceCode":"first","deviceCode":"second"}}"#.into(),
            ),
            (
                200,
                r#"{"data":{"deviceCode":"valid"},"deviceCode":"ambiguous"}"#.into(),
            ),
            (
                200,
                r#"{"data":{"deviceCode":"valid","expiresIn":1,"expires_in":2}}"#.into(),
            ),
            (200, r#"{"deviceCode":"bad\ncontrol"}"#.into()),
            (200, "x".repeat(16 * 1024 + 1)),
        ] {
            let f = fixture().await;
            *f.state.reply.lock() = (StatusCode::from_u16(status).unwrap(), body);
            let error = issue(&f)
                .await
                .err()
                .expect("must refuse malformed or reflected code");
            counts(&f, 1);
            assert!(!error.contains("owned-browser"));
        }
    }
    #[tokio::test]
    async fn bare_response_and_absent_optional_refresh_have_normal_positive_path() {
        let f = fixture().await;
        change(&f.auth, |c| {
            c.refresh_token = None;
            c.refresh_expires_at_ms = None;
        })
        .await;
        *f.state.reply.lock() = (
            StatusCode::OK,
            r#"{"deviceCode":"another-owned-code","expires_in":42}"#.into(),
        );
        assert_eq!(
            issue(&f).await.unwrap().into_sensitive_code(),
            "another-owned-code"
        );
        counts(&f, 1);
        assert!(!f.state.seen.lock()[0].0.contains_key("x-refresh-token"));
    }

    #[tokio::test]
    async fn lifecycle_response_expiry_without_replacement_discards_code() {
        let f = fixture().await;
        change(&f.auth, |c| c.access_expires_at_ms = Some(now_ms() + 200)).await;
        let key = f.auth.session_key("owned");
        let before = f.auth.read(&key).await.unwrap();
        f.state.blocked.store(true, Ordering::SeqCst);
        let auth = f.auth.clone();
        let pending = tokio::spawn(async move {
            auth.create_browser_login_code("owned", &identity(), true)
                .await
                .map(|_| ())
        });
        tokio::time::timeout(Duration::from_secs(3), f.state.entered.notified())
            .await
            .unwrap();
        counts(&f, 1);
        tokio::time::sleep(Duration::from_millis(250)).await;
        f.state.release.notify_one();
        assert_eq!(pending.await.unwrap().err().unwrap(), CHANGED);
        assert_eq!(f.auth.read(&key).await.unwrap(), before);
        counts(&f, 1);
    }

    #[tokio::test]
    async fn lifecycle_preparation_cancellation_is_zero_send_and_releases_gate() {
        let f = fixture().await;
        let connection = f.auth.pool.acquire().await.unwrap();
        let auth = f.auth.clone();
        let pending = tokio::spawn(async move {
            auth.create_browser_login_code("owned", &identity(), true)
                .await
                .map(|_| ())
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if f.auth.gate.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!pending.is_finished());
        counts(&f, 0);
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        let guard = tokio::time::timeout(Duration::from_secs(1), f.auth.gate.lock())
            .await
            .unwrap();
        drop(guard);
        drop(connection);
        counts(&f, 0);
    }
}
