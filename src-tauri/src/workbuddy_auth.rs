//! Independent WorkBuddy authorization against the installed product's verified issuer.
//! Only this application's encrypted app_secrets are used. No client processes or credential files.
use crate::{client_session::AccountSnapshot, crypto, db::repo};
use futures_util::StreamExt;
use reqwest::{header::HeaderValue, Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

const ISSUER: &str = "https://www.workbuddy.ai";
const PREFIX: &str = "account-login:workbuddy:global:";
const LOGIN_LIMIT_MS: i64 = 300_000;
const BODY_LIMIT: usize = 512 * 1024;

// These types deliberately have no Debug and are never returned through IPC or stdout.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    issuer: String,
    access_token: String,
    refresh_token: Option<String>,
    domain: String,
    access_expires_at_ms: Option<i64>,
    refresh_expires_at_ms: Option<i64>,
    #[serde(default)]
    enterprise_id: Option<String>,
    #[serde(default)]
    uid: Option<String>,
    snapshot: AccountSnapshot,
    refresh_attempted: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingLogin {
    account_id: String,
    state: String,
    deadline_ms: i64,
    server_ttl_seconds: Option<i64>,
    baseline_cipher: Option<String>,
    reserved: bool,
    credential: Option<Credential>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LoginStart {
    pub login_id: String,
    pub authorization_url: String,
    pub deadline_ms: i64,
    /// None means the issuer did not supply a TTL. deadline_ms is our bounded wait limit.
    pub server_ttl_seconds: Option<i64>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LoginStatus {
    pub status: String,
    pub account_id: String,
    pub identity: Option<AccountSnapshot>,
    pub can_refresh: bool,
    pub access_expires_at_ms: Option<i64>,
    pub refresh_expires_at_ms: Option<i64>,
    pub refresh_attempted: bool,
}

pub fn valid_authorization_url(raw: &str) -> bool {
    raw.len() <= 8192
        && !raw.chars().any(char::is_control)
        && Url::parse(raw).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("www.workbuddy.ai")
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
        })
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 8192
        && !token
            .bytes()
            .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
        && HeaderValue::from_str(token).is_ok()
}

fn domain_ok(domain: &str) -> bool {
    matches!(
        domain,
        "www.workbuddy.ai" | "workbuddy.ai" | "https://www.workbuddy.ai" | "https://workbuddy.ai"
    )
}

fn session_key(account: &str) -> String {
    format!("{PREFIX}session:{account}")
}
fn pending_key(id: &str) -> String {
    format!("{PREFIX}pending:{id}")
}
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn encode<T: Serialize>(value: &T) -> Result<String, String> {
    let raw = serde_json::to_string(value).map_err(|_| "无法编码登录资料")?;
    crypto::encrypt(&raw).map_err(|_| "无法加密登录资料".into())
}
fn decode<T: serde::de::DeserializeOwned>(cipher: &str) -> Result<T, String> {
    let raw = crypto::decrypt(cipher).map_err(|_| "无法解密登录资料，请重新登录")?;
    if raw.len() > 65536 {
        return Err("登录资料过大".into());
    }
    serde_json::from_str(&raw).map_err(|_| "登录资料无效，请重新登录".into())
}

fn expiry(data: &Value, absolute: &str, relative: &str, at: i64) -> Result<Option<i64>, String> {
    if let Some(value) = data.get(absolute).filter(|v| !v.is_null()) {
        let value = value
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or("令牌到期时间格式无效")?;
        // Installed official calculateExpiresAt uses epoch milliseconds. Do not guess seconds.
        if chrono::DateTime::from_timestamp_millis(value).is_none() || value < 1_000_000_000_000 {
            return Err("令牌绝对到期时间单位无效".into());
        }
        return Ok(Some(value));
    }
    if let Some(value) = data.get(relative).filter(|v| !v.is_null()) {
        let seconds = value
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or("令牌有效期格式无效")?;
        let result = seconds
            .checked_mul(1000)
            .and_then(|v| at.checked_add(v))
            .filter(|v| chrono::DateTime::from_timestamp_millis(*v).is_some())
            .ok_or("令牌有效期超出范围")?;
        return Ok(Some(result));
    }
    Ok(None)
}

fn parse_token(
    value: &Value,
    previous: Option<&Credential>,
    at: i64,
) -> Result<Credential, String> {
    if value["code"].as_i64() != Some(0) {
        return Err("授权服务拒绝令牌请求".into());
    }
    let data = &value["data"];
    let access = data["accessToken"]
        .as_str()
        .filter(|v| valid_token(v))
        .ok_or("授权响应缺少有效访问令牌")?;
    let domain = data["domain"]
        .as_str()
        .or_else(|| previous.map(|c| c.domain.as_str()))
        .filter(|v| domain_ok(v))
        .ok_or("授权区域与 WorkBuddy 国际版不一致")?;
    let refresh = match data.get("refreshToken").filter(|v| !v.is_null()) {
        Some(v) => Some(
            v.as_str()
                .filter(|v| valid_token(v))
                .ok_or("授权响应刷新令牌无效")?
                .to_owned(),
        ),
        None => previous.and_then(|c| c.refresh_token.clone()),
    };
    let mut refresh_expiry = expiry(data, "refreshExpiresAt", "refreshExpiresIn", at)?;
    if refresh_expiry.is_none() && previous.is_some_and(|p| p.refresh_token == refresh) {
        refresh_expiry = previous.and_then(|p| p.refresh_expires_at_ms);
    }
    Ok(Credential {
        issuer: ISSUER.into(),
        access_token: access.into(),
        refresh_token: refresh,
        domain: domain.into(),
        access_expires_at_ms: expiry(data, "expiresAt", "expiresIn", at)?,
        refresh_expires_at_ms: refresh_expiry,
        enterprise_id: previous.and_then(|p| p.enterprise_id.clone()),
        uid: previous.and_then(|p| p.uid.clone()),
        snapshot: previous.map(|p| p.snapshot.clone()).unwrap_or_default(),
        refresh_attempted: false,
    })
}

fn validate_credential(value: &Credential) -> Result<(), String> {
    if value.issuer != ISSUER
        || !domain_ok(&value.domain)
        || !valid_token(&value.access_token)
        || value
            .refresh_token
            .as_deref()
            .is_some_and(|v| !valid_token(v))
    {
        return Err("登录凭据无效或区域不一致".into());
    }
    Ok(())
}

fn reflects(value: &Value, secrets: &[&str]) -> bool {
    match value {
        Value::String(text) => secrets
            .iter()
            .any(|secret| !secret.is_empty() && text.contains(secret)),
        Value::Array(values) => values.iter().any(|v| reflects(v, secrets)),
        Value::Object(values) => values.values().any(|v| reflects(v, secrets)),
        _ => false,
    }
}

fn enterprise_id(data: &Value) -> Result<Option<String>, String> {
    match data.get("enterpriseId").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(Value::String(value))
            if value.len() <= 256 && !value.chars().any(char::is_control) =>
        {
            Ok((!value.is_empty()).then(|| value.clone()))
        }
        Some(value) if value.as_u64().is_some() => {
            Ok(Some(value.as_u64().ok_or("企业账号标识无效")?.to_string()))
        }
        _ => Err("企业账号标识无效".into()),
    }
}

fn snapshot(
    value: &Value,
    credential: &Credential,
    state: Option<&str>,
) -> Result<AccountSnapshot, String> {
    let mut secrets = vec![credential.access_token.as_str()];
    secrets.extend(credential.refresh_token.as_deref());
    secrets.extend(state);
    if reflects(value, &secrets) {
        return Err("账号响应包含凭据，已拒绝展示".into());
    }
    if value["code"].as_i64() != Some(0) {
        return Err("账号查询未通过".into());
    }
    let data = &value["data"];
    let uid = account_uid(data)?;
    if data["domain"].as_str().is_some_and(|v| !domain_ok(v)) {
        return Err("账号区域与授权区域不一致".into());
    }
    let display_name = data["displayName"]
        .as_str()
        .or_else(|| data["nickname"].as_str())
        .filter(|v| v.len() <= 256 && !v.contains(['\r', '\n']))
        .map(str::to_owned);
    Ok(AccountSnapshot {
        display_name,
        masked_email: None,
        identity_hash: Some(format!(
            "{:x}",
            Sha256::digest(format!("workbuddy:{uid}").as_bytes())
        )),
        captured_at: Some(chrono::Utc::now().to_rfc3339()),
    })
}

#[cfg(test)]
fn refreshed_snapshot(
    value: &Value,
    next: &Credential,
    previous: &Credential,
) -> Result<AccountSnapshot, String> {
    Ok(bound_account(value, next, previous)?.1)
}

fn account_uid(data: &Value) -> Result<String, String> {
    data["uid"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| data["uid"].as_u64().map(|v| v.to_string()))
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 256
                && !v
                    .bytes()
                    .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
                && HeaderValue::from_str(v).is_ok()
        })
        .ok_or_else(|| "账号响应缺少可验证身份".into())
}

fn bound_account<'a>(
    value: &'a Value,
    next: &Credential,
    previous: &Credential,
) -> Result<(&'a Value, AccountSnapshot), String> {
    let mut secrets = vec![next.access_token.as_str(), previous.access_token.as_str()];
    secrets.extend(next.refresh_token.as_deref());
    secrets.extend(previous.refresh_token.as_deref());
    if reflects(value, &secrets) {
        return Err("账号响应包含凭据，已拒绝展示".into());
    }
    if value["code"].as_i64() != Some(0) {
        return Err("账号查询未通过".into());
    }
    let entries = value["data"]["accounts"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or("账号列表响应无效")?;
    let mut selected = None;
    for entry in entries {
        let candidate = snapshot(&json!({"code":0,"data":entry}), next, None)?;
        if candidate.identity_hash == previous.snapshot.identity_hash
            && enterprise_id(entry)? == previous.enterprise_id
        {
            if selected.is_some() {
                return Err("账号列表存在重复绑定，请重新登录".into());
            }
            selected = Some((entry, candidate));
        }
    }
    selected.ok_or_else(|| "刷新返回的账号身份不一致，请重新登录".into())
}

/// Memory-only authorization for the fixed model issuer. Never serialize or Debug this value.
pub(crate) struct ModelAuthorization {
    pub(crate) headers: reqwest::header::HeaderMap,
    pub(crate) identity_hash: String,
    pub(crate) credential_fingerprint: String,
}
impl ModelAuthorization {
    pub(crate) fn contains_credential(&self, body: &[u8]) -> bool {
        let Some(token) = self
            .headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
        else {
            return true;
        };
        String::from_utf8_lossy(body).contains(token)
            || serde_json::from_slice::<Value>(body).is_ok_and(|value| reflects(&value, &[token]))
    }
}

fn status(account: &str, credential: Option<&Credential>) -> LoginStatus {
    LoginStatus {
        status: match credential {
            Some(c) if c.access_expires_at_ms.is_some_and(|t| t <= now_ms()) => "access_expired",
            Some(_) => "authenticated",
            None => "not_logged_in",
        }
        .into(),
        account_id: account.into(),
        identity: credential.map(|c| c.snapshot.clone()),
        can_refresh: credential.is_some_and(|c| {
            c.refresh_token.is_some()
                && !c.refresh_attempted
                && c.refresh_expires_at_ms.map_or(true, |t| t > now_ms())
        }),
        access_expires_at_ms: credential.and_then(|c| c.access_expires_at_ms),
        refresh_expires_at_ms: credential.and_then(|c| c.refresh_expires_at_ms),
        refresh_attempted: credential.is_some_and(|c| c.refresh_attempted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Query, State},
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Json, Router,
    };
    use std::{
        collections::{HashMap, VecDeque},
        sync::atomic::{AtomicUsize, Ordering},
    };

    const ACCESS: &str = "fixture-workbuddy-access";
    const REFRESH: &str = "fixture-workbuddy-refresh";
    const STATE: &str = "fixture-workbuddy-state";

    fn token(access: &str, refresh: &str) -> Value {
        json!({"code":0,"data":{"accessToken":access,"refreshToken":refresh,"domain":"https://www.workbuddy.ai","expiresIn":3600,"refreshExpiresIn":86400}})
    }
    fn account(uid: u64) -> Value {
        json!({"code":0,"data":{"uid":uid,"displayName":"离线账号","domain":"https://www.workbuddy.ai"}})
    }
    fn credential(uid: u64) -> Credential {
        let mut credential = parse_token(&token(ACCESS, REFRESH), None, now_ms()).unwrap();
        credential.snapshot = snapshot(&account(uid), &credential, None).unwrap();
        credential
    }
    fn init_crypto() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            if std::env::var_os("LLMGW_MASTER_KEY").is_none() {
                use base64::Engine;
                std::env::set_var(
                    "LLMGW_MASTER_KEY",
                    base64::engine::general_purpose::STANDARD.encode([73_u8; 32]),
                );
            }
        });
    }

    #[derive(Clone)]
    struct Fixture {
        state_response: Arc<parking_lot::Mutex<Value>>,
        tokens: Arc<parking_lot::Mutex<VecDeque<Value>>>,
        accounts: Arc<parking_lot::Mutex<VecDeque<Value>>>,
        refreshes: Arc<parking_lot::Mutex<VecDeque<Value>>>,
        token_calls: Arc<AtomicUsize>,
        account_calls: Arc<AtomicUsize>,
        refresh_calls: Arc<AtomicUsize>,
        block_account: bool,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl Default for Fixture {
        fn default() -> Self {
            Self {
                state_response: Arc::new(parking_lot::Mutex::new(
                    json!({"code":0,"data":{"state":STATE,"authUrl":format!("{ISSUER}/plugin/login?state={STATE}")}}),
                )),
                tokens: Arc::new(parking_lot::Mutex::new(VecDeque::from([token(
                    ACCESS, REFRESH,
                )]))),
                accounts: Arc::new(parking_lot::Mutex::new(VecDeque::from([account(42)]))),
                refreshes: Arc::new(parking_lot::Mutex::new(VecDeque::from([token(
                    "fixture-workbuddy-new-access",
                    "fixture-workbuddy-new-refresh",
                )]))),
                token_calls: Arc::new(AtomicUsize::new(0)),
                account_calls: Arc::new(AtomicUsize::new(0)),
                refresh_calls: Arc::new(AtomicUsize::new(0)),
                block_account: false,
                entered: Arc::new(tokio::sync::Notify::new()),
                release: Arc::new(tokio::sync::Notify::new()),
            }
        }
    }
    async fn state_handler(
        State(f): State<Fixture>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        assert_eq!(
            query.get("platform").map(String::as_str),
            Some("workbuddy-ai")
        );
        assert_eq!(headers["x-domain"], "www.workbuddy.ai");
        assert_eq!(headers["x-no-authorization"], "true");
        assert!(!headers.contains_key("authorization"));
        assert!(!headers.contains_key("x-refresh-token"));
        assert_eq!(body, json!({}));
        Json(f.state_response.lock().clone())
    }
    async fn token_handler(
        State(f): State<Fixture>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Json<Value> {
        assert_eq!(query.get("state").map(String::as_str), Some(STATE));
        assert!(!headers.contains_key("authorization"));
        f.token_calls.fetch_add(1, Ordering::SeqCst);
        Json(
            f.tokens
                .lock()
                .pop_front()
                .expect("unexpected repeated token poll"),
        )
    }
    async fn account_handler(
        State(f): State<Fixture>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Json<Value> {
        assert!(headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer fixture-workbuddy-"));
        assert_eq!(headers["x-domain"], "https://www.workbuddy.ai");
        assert!(!headers.contains_key("x-refresh-token"));
        if query.get("state").map(String::as_str) != Some(STATE) {
            return Json(json!({"code":400,"msg":"state required for initial account"}));
        }
        f.account_calls.fetch_add(1, Ordering::SeqCst);
        if f.block_account {
            f.entered.notify_one();
            f.release.notified().await;
        }
        Json(
            f.accounts
                .lock()
                .pop_front()
                .expect("unexpected account request"),
        )
    }
    async fn accounts_handler(State(f): State<Fixture>, headers: HeaderMap) -> Json<Value> {
        assert!(headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer fixture-workbuddy-"));
        assert!(!headers.contains_key("x-refresh-token"));
        f.account_calls.fetch_add(1, Ordering::SeqCst);
        let value = f
            .accounts
            .lock()
            .pop_front()
            .expect("unexpected accounts request");
        Json(json!({"code":0,"data":{"accounts":[value["data"].clone()]}}))
    }
    async fn refresh_handler(
        State(f): State<Fixture>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        f.refresh_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(headers["x-refresh-token"], REFRESH);
        assert_eq!(headers["x-auth-refresh-source"], "plugin");
        assert_eq!(headers["x-domain"], "https://www.workbuddy.ai");
        assert!(!headers.contains_key("authorization"));
        assert_eq!(body, json!({}));
        // Let a simultaneous caller observe the same pre-refresh generation.
        tokio::time::sleep(Duration::from_millis(10)).await;
        Json(
            f.refreshes
                .lock()
                .pop_front()
                .expect("refresh mutation was replayed"),
        )
    }
    struct Server {
        auth: WorkBuddyAuth,
        fixture: Fixture,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn server(fixture: Fixture) -> Server {
        init_crypto();
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/v2/plugin/auth/state", post(state_handler))
            .route("/v2/plugin/auth/token", get(token_handler))
            .route("/v2/plugin/login/account", get(account_handler))
            .route("/v2/plugin/accounts", get(accounts_handler))
            .route("/v2/plugin/auth/token/refresh", post(refresh_handler))
            .with_state(fixture.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let auth = WorkBuddyAuth {
            pool: db.pool().clone(),
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            base,
            gate: Arc::new(Mutex::new(())),
            before_commit: None,
        };
        Server {
            auth,
            fixture,
            task,
        }
    }
    async fn save(auth: &WorkBuddyAuth, account: &str, value: &Credential) {
        repo::set_secret(&auth.pool, &session_key(account), &encode(value).unwrap())
            .await
            .unwrap();
    }
    async fn saved(auth: &WorkBuddyAuth, account: &str) -> Credential {
        decode(&auth.read(&session_key(account)).await.unwrap().unwrap()).unwrap()
    }

    #[test]
    fn expiry_keeps_unknown_and_respects_official_milliseconds_and_seconds() {
        let at = 1_791_920_000_000;
        let mut data = json!({});
        assert_eq!(expiry(&data, "expiresAt", "expiresIn", at).unwrap(), None);
        data["expiresIn"] = json!(60);
        assert_eq!(
            expiry(&data, "expiresAt", "expiresIn", at).unwrap(),
            Some(at + 60_000)
        );
        data["expiresAt"] = json!(at + 1000);
        assert_eq!(
            expiry(&data, "expiresAt", "expiresIn", at).unwrap(),
            Some(at + 1000)
        );
        for bad in [json!(at / 1000), json!(-1), json!("tomorrow"), json!(1.5)] {
            data["expiresAt"] = bad;
            assert!(expiry(&data, "expiresAt", "expiresIn", at).is_err());
        }
        assert!(expiry(&json!({"expiresIn":i64::MAX}), "expiresAt", "expiresIn", at).is_err());
        let no_expiry = json!({"code":0,"data":{"accessToken":ACCESS,"domain":"www.workbuddy.ai"}});
        let c = parse_token(&no_expiry, None, at).unwrap();
        assert_eq!(c.access_expires_at_ms, None);
        assert_eq!(c.refresh_token, None);
        assert!(!status("fixture", Some(&c)).can_refresh);
    }

    #[tokio::test]
    async fn independent_login_polls_known_pending_codes_and_saves_only_ciphertext() {
        let f = Fixture::default();
        f.tokens
            .lock()
            .push_front(json!({"code":11217,"msg":"pending"}));
        f.accounts
            .lock()
            .push_front(json!({"code":12151,"msg":"pending"}));
        let s = server(f).await;
        assert_eq!(
            s.auth.session_status("fixture").await.unwrap().status,
            "not_logged_in"
        );
        let begin = s.auth.start("fixture").await.unwrap();
        assert_eq!(begin.server_ttl_seconds, None);
        assert!(begin.deadline_ms > now_ms() && begin.deadline_ms <= now_ms() + LOGIN_LIMIT_MS);
        assert_eq!(
            s.auth.poll(&begin.login_id).await.unwrap().status,
            "awaiting_authorization"
        );
        assert_eq!(
            s.auth.poll(&begin.login_id).await.unwrap().status,
            "awaiting_authorization"
        );
        let complete = s.auth.poll(&begin.login_id).await.unwrap();
        assert_eq!(complete.status, "authenticated");
        assert!(complete.can_refresh);
        assert_eq!(
            complete.identity.unwrap().identity_hash,
            credential(42).snapshot.identity_hash
        );
        assert_eq!(s.fixture.token_calls.load(Ordering::SeqCst), 2);
        assert_eq!(s.fixture.account_calls.load(Ordering::SeqCst), 2);
        assert!(s.auth.poll(&begin.login_id).await.is_err());
        assert!(s
            .auth
            .read(&pending_key(&begin.login_id))
            .await
            .unwrap()
            .is_none());
        let cipher = s.auth.read(&session_key("fixture")).await.unwrap().unwrap();
        let safe = serde_json::to_string(&s.auth.session_status("fixture").await.unwrap()).unwrap();
        for secret in [ACCESS, REFRESH, STATE] {
            assert!(!cipher.contains(secret));
            assert!(!safe.contains(secret));
        }
        assert_eq!(
            saved(&s.auth, "fixture").await.refresh_token.as_deref(),
            Some(REFRESH)
        );
    }

    #[tokio::test]
    async fn invalid_start_or_other_pending_code_never_becomes_authenticated() {
        let s = server(Fixture::default()).await;
        for bad in [
            json!({"code":401,"data":{"state":STATE,"authUrl":format!("{ISSUER}/plugin/login")}}),
            json!({"code":0,"data":{"state":STATE,"authUrl":"https://www.codebuddy.ai/login"}}),
            json!({"code":0,"data":{"state":STATE,"authUrl":format!("{ISSUER}/login?state=different")}}),
            json!({"code":0,"data":{"state":STATE,"authUrl":format!("{ISSUER}/login"),"expiresIn":i64::MAX}}),
        ] {
            *s.fixture.state_response.lock() = bad;
            assert!(s.auth.start("fixture").await.is_err());
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM app_secrets")
                .fetch_one(&s.auth.pool)
                .await
                .unwrap(),
            0
        );
        assert!(s.auth.start("../../outside").await.is_err());
        *s.fixture.state_response.lock() =
            json!({"code":0,"data":{"state":STATE,"authUrl":format!("{ISSUER}/login")}});
        *s.fixture.tokens.lock() = VecDeque::from([json!({"code":11218,"msg":ACCESS})]);
        let begin = s.auth.start("fixture").await.unwrap();
        let error = s.auth.poll(&begin.login_id).await.unwrap_err();
        assert!(!error.contains(ACCESS));
        assert_eq!(s.fixture.account_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            s.auth.session_status("fixture").await.unwrap().status,
            "not_logged_in"
        );
    }

    #[tokio::test]
    async fn expiry_cancellation_and_reflected_credentials_block_completion() {
        let s = server(Fixture::default()).await;
        let begin = s.auth.start("fixture").await.unwrap();
        let key = pending_key(&begin.login_id);
        let cipher = s.auth.read(&key).await.unwrap().unwrap();
        let mut pending: PendingLogin = decode(&cipher).unwrap();
        pending.deadline_ms = now_ms() - 1;
        s.auth
            .replace(&key, &cipher, &encode(&pending).unwrap())
            .await
            .unwrap();
        assert!(s
            .auth
            .poll(&begin.login_id)
            .await
            .unwrap_err()
            .contains("过期"));
        assert_eq!(s.fixture.token_calls.load(Ordering::SeqCst), 0);
        let begin = s.auth.start("fixture").await.unwrap();
        s.auth.cancel(&begin.login_id).await.unwrap();
        assert!(s.auth.poll(&begin.login_id).await.is_err());
        assert_eq!(s.fixture.token_calls.load(Ordering::SeqCst), 0);
        *s.fixture.accounts.lock() =
            VecDeque::from([json!({"code":0,"data":{"uid":42,"displayName":REFRESH}})]);
        let begin = s.auth.start("fixture").await.unwrap();
        let error = s.auth.poll(&begin.login_id).await.unwrap_err();
        assert!(!error.contains(REFRESH));
        assert_eq!(
            s.auth.session_status("fixture").await.unwrap().status,
            "not_logged_in"
        );
    }

    #[tokio::test]
    async fn cancel_during_http_cannot_resurrect_or_save_a_login() {
        let f = Fixture {
            block_account: true,
            ..Fixture::default()
        };
        let s = server(f).await;
        let begin = s.auth.start("fixture").await.unwrap();
        let auth = s.auth.clone();
        let id = begin.login_id.clone();
        let poll = tokio::spawn(async move { auth.poll(&id).await });
        tokio::time::timeout(Duration::from_secs(3), s.fixture.entered.notified())
            .await
            .unwrap();
        s.auth.cancel(&begin.login_id).await.unwrap();
        s.fixture.release.notify_one();
        assert!(poll.await.unwrap().is_err());
        assert!(s
            .auth
            .read(&pending_key(&begin.login_id))
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            s.auth.session_status("fixture").await.unwrap().status,
            "not_logged_in"
        );
    }

    #[tokio::test]
    async fn principal_and_concurrent_account_changes_do_not_get_overwritten() {
        let s = server(Fixture::default()).await;
        save(&s.auth, "fixture", &credential(99)).await;
        let baseline = s.auth.read(&session_key("fixture")).await.unwrap();
        let begin = s.auth.start("fixture").await.unwrap();
        assert!(s
            .auth
            .poll(&begin.login_id)
            .await
            .unwrap_err()
            .contains("绑定账号"));
        assert_eq!(
            s.auth.read(&session_key("fixture")).await.unwrap(),
            baseline
        );

        *s.fixture.tokens.lock() = VecDeque::from([token(ACCESS, REFRESH)]);
        *s.fixture.accounts.lock() = VecDeque::from([account(99)]);
        let begin = s.auth.start("fixture").await.unwrap();
        let mut updated = credential(99);
        updated.access_token = "fixture-workbuddy-other-access".into();
        save(&s.auth, "fixture", &updated).await;
        let after = s.auth.read(&session_key("fixture")).await.unwrap();
        assert!(s
            .auth
            .poll(&begin.login_id)
            .await
            .unwrap_err()
            .contains("未覆盖"));
        assert_eq!(s.auth.read(&session_key("fixture")).await.unwrap(), after);
    }

    #[tokio::test]
    async fn relogin_cannot_expose_the_previously_saved_pair_in_account_metadata() {
        let f = Fixture::default();
        *f.tokens.lock() = VecDeque::from([token(
            "fixture-workbuddy-next-access",
            "fixture-workbuddy-next-refresh",
        )]);
        *f.accounts.lock() =
            VecDeque::from([json!({"code":0,"data":{"uid":42,"displayName":ACCESS}})]);
        let s = server(f).await;
        save(&s.auth, "fixture", &credential(42)).await;
        let baseline = s.auth.read(&session_key("fixture")).await.unwrap();
        let begin = s.auth.start("fixture").await.unwrap();
        let result = s.auth.poll(&begin.login_id).await;
        assert!(result.is_err());
        assert_eq!(
            s.auth.read(&session_key("fixture")).await.unwrap(),
            baseline
        );
    }

    #[tokio::test]
    async fn concurrent_refresh_rotates_pair_once_and_preserves_safe_status() {
        let s = server(Fixture::default()).await;
        save(&s.auth, "fixture", &credential(42)).await;
        let (a, b) = tokio::join!(s.auth.refresh("fixture"), s.auth.refresh("fixture"));
        assert_eq!(a.unwrap().status, "authenticated");
        assert_eq!(b.unwrap().status, "authenticated");
        assert_eq!(s.fixture.refresh_calls.load(Ordering::SeqCst), 1);
        let c = saved(&s.auth, "fixture").await;
        assert_eq!(c.access_token, "fixture-workbuddy-new-access");
        assert_eq!(
            c.refresh_token.as_deref(),
            Some("fixture-workbuddy-new-refresh")
        );
        assert!(!c.refresh_attempted);
        assert!(c.access_expires_at_ms.unwrap() > now_ms());
        assert!(c.refresh_expires_at_ms.unwrap() > c.access_expires_at_ms.unwrap());
        let status =
            serde_json::to_string(&s.auth.session_status("fixture").await.unwrap()).unwrap();
        assert!(!status.contains(&c.access_token));
        assert!(!status.contains(c.refresh_token.as_ref().unwrap()));
    }

    #[tokio::test]
    async fn failed_refresh_keeps_old_pair_and_reservation_prevents_replay() {
        let f = Fixture::default();
        *f.refreshes.lock() = VecDeque::from([json!({"code":403,"msg":REFRESH})]);
        let s = server(f).await;
        save(&s.auth, "fixture", &credential(42)).await;
        let error = s.auth.refresh("fixture").await.unwrap_err();
        assert!(!error.contains(REFRESH));
        let c = saved(&s.auth, "fixture").await;
        assert_eq!(c.access_token, ACCESS);
        assert_eq!(c.refresh_token.as_deref(), Some(REFRESH));
        assert!(c.refresh_attempted);
        assert!(s
            .auth
            .refresh("fixture")
            .await
            .unwrap_err()
            .contains("不会自动重试"));
        assert_eq!(s.fixture.refresh_calls.load(Ordering::SeqCst), 1);
        assert!(!s.auth.session_status("fixture").await.unwrap().can_refresh);
    }

    #[tokio::test]
    async fn success_envelope_without_a_new_access_generation_cannot_unlock_replay() {
        let f = Fixture::default();
        *f.refreshes.lock() = VecDeque::from([token(ACCESS, REFRESH)]);
        let s = server(f).await;
        save(&s.auth, "fixture", &credential(42)).await;
        assert!(s.auth.refresh("fixture").await.is_err());
        assert_eq!(saved(&s.auth, "fixture").await.access_token, ACCESS);
        assert!(saved(&s.auth, "fixture").await.refresh_attempted);
        assert!(s.auth.refresh("fixture").await.is_err());
        assert_eq!(s.fixture.refresh_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn decoded_escaped_and_short_state_reflections_are_not_exposed() {
        let mut c = credential(42);
        for secret in ["fixture\"refresh", "fixture\\refresh"] {
            c.refresh_token = Some(secret.into());
            let value = json!({"code":0,"data":{"uid":42,"displayName":secret}});
            assert!(snapshot(&value, &c, None).is_err());
        }
        let value = json!({"code":0,"data":{"uid":42,"displayName":"abc"}});
        assert!(snapshot(&value, &credential(42), Some("abc")).is_err());
    }

    #[test]
    fn refresh_account_list_rejects_old_secrets_and_other_enterprise_context() {
        let previous = credential(42);
        let next = parse_token(
            &token(
                "fixture-workbuddy-next-access",
                "fixture-workbuddy-next-refresh",
            ),
            Some(&previous),
            now_ms(),
        )
        .unwrap();
        for secret in [
            ACCESS,
            REFRESH,
            "fixture-workbuddy-next-access",
            "fixture-workbuddy-next-refresh",
        ] {
            let value = json!({"code":0,"data":{"accounts":[{"uid":42,"displayName":secret}]}});
            assert!(refreshed_snapshot(&value, &next, &previous).is_err());
        }
        let mut enterprise = previous.clone();
        enterprise.enterprise_id = Some("enterprise-fixture-73".into());
        let wrong = json!({"code":0,"data":{"accounts":[{"uid":42,"enterpriseId":"enterprise-fixture-74"}]}});
        assert!(refreshed_snapshot(&wrong, &next, &enterprise).is_err());
        let right = json!({"code":0,"data":{"accounts":[{"uid":42,"enterpriseId":"enterprise-fixture-73"}]}});
        assert_eq!(
            refreshed_snapshot(&right, &next, &enterprise)
                .unwrap()
                .identity_hash,
            previous.snapshot.identity_hash
        );
    }

    #[tokio::test]
    async fn new_access_generation_can_keep_a_nonrotating_refresh_token() {
        let f = Fixture::default();
        *f.refreshes.lock() = VecDeque::from([
            json!({"code":0,"data":{"accessToken":"fixture-workbuddy-next-access","domain":"https://www.workbuddy.ai","expiresIn":60}}),
        ]);
        let s = server(f).await;
        let previous = credential(42);
        save(&s.auth, "fixture", &previous).await;
        assert_eq!(
            s.auth.refresh("fixture").await.unwrap().status,
            "authenticated"
        );
        let next = saved(&s.auth, "fixture").await;
        assert_eq!(next.refresh_token, previous.refresh_token);
        assert_eq!(next.refresh_expires_at_ms, previous.refresh_expires_at_ms);
        assert_ne!(next.access_token, previous.access_token);
        assert!(!next.refresh_attempted);
        assert_eq!(s.fixture.refresh_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn expiry_while_waiting_for_sqlite_write_lock_cannot_save_a_login() {
        let f = Fixture {
            block_account: true,
            ..Fixture::default()
        };
        let mut s = server(f).await;
        let commit_reached = Arc::new(tokio::sync::Notify::new());
        s.auth.before_commit = Some(commit_reached.clone());
        let begin = s.auth.start("fixture").await.unwrap();
        let key = pending_key(&begin.login_id);
        let old = s.auth.read(&key).await.unwrap().unwrap();
        let mut pending: PendingLogin = decode(&old).unwrap();
        pending.deadline_ms = now_ms() + 500;
        s.auth
            .replace(&key, &old, &encode(&pending).unwrap())
            .await
            .unwrap();
        let auth = s.auth.clone();
        let id = begin.login_id.clone();
        let poll = tokio::spawn(async move { auth.poll(&id).await });
        tokio::time::timeout(Duration::from_secs(3), s.fixture.entered.notified())
            .await
            .unwrap();
        let blocker = s.auth.pool.begin().await.unwrap();
        s.fixture.release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), commit_reached.notified())
            .await
            .unwrap();
        assert!(
            now_ms() < pending.deadline_ms,
            "fixture failed to reach the lock before expiry"
        );
        tokio::time::sleep(Duration::from_millis(
            (pending.deadline_ms - now_ms() + 5) as u64,
        ))
        .await;
        blocker.rollback().await.unwrap();
        assert!(poll.await.unwrap().unwrap_err().contains("过期"));
        assert_eq!(
            s.auth.session_status("fixture").await.unwrap().status,
            "not_logged_in"
        );
    }

    #[tokio::test]
    async fn pending_limit_is_atomic_between_independent_service_instances() {
        let s = server(Fixture::default()).await;
        for _ in 0..31 {
            let pending = PendingLogin {
                account_id: "fixture".into(),
                state: STATE.into(),
                deadline_ms: now_ms() + LOGIN_LIMIT_MS,
                server_ttl_seconds: None,
                baseline_cipher: None,
                reserved: false,
                credential: None,
            };
            repo::set_secret(
                &s.auth.pool,
                &pending_key(&uuid::Uuid::new_v4().to_string()),
                &encode(&pending).unwrap(),
            )
            .await
            .unwrap();
        }
        let mut independent = s.auth.clone();
        independent.gate = Arc::new(Mutex::new(()));
        let (a, b) = tokio::join!(s.auth.start("fixture"), independent.start("fixture"));
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        let count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM app_secrets WHERE name LIKE ?")
                .bind(format!("{PREFIX}pending:%"))
                .fetch_one(&s.auth.pool)
                .await
                .unwrap();
        assert_eq!(count, 32);
    }

    #[tokio::test]
    async fn model_context_backfills_verified_uid_and_unknown_expiry_does_not_refresh() {
        let s = server(Fixture::default()).await;
        let mut value = credential(42);
        value.access_expires_at_ms = None;
        save(&s.auth, "fixture", &value).await;
        let headers = s.auth.prepare_model_authorization("fixture").await.unwrap();
        assert_eq!(headers.headers["x-user-id"], "42");
        assert!(!headers.headers.contains_key("x-refresh-token"));
        for value in headers.headers.values() {
            assert!(value.is_sensitive());
        }
        assert_eq!(s.fixture.refresh_calls.load(Ordering::SeqCst), 0);
        assert_eq!(s.fixture.account_calls.load(Ordering::SeqCst), 1);
        s.auth.prepare_model_authorization("fixture").await.unwrap();
        assert_eq!(s.fixture.refresh_calls.load(Ordering::SeqCst), 0);
        assert_eq!(s.fixture.account_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn simultaneous_near_expiry_model_contexts_share_one_refresh() {
        let s = server(Fixture::default()).await;
        let mut value = credential(42);
        value.access_expires_at_ms = Some(now_ms() + 30_000);
        save(&s.auth, "fixture", &value).await;
        let (a, b) = tokio::join!(
            s.auth.prepare_model_authorization("fixture"),
            s.auth.prepare_model_authorization("fixture")
        );
        let a = a.unwrap();
        let b = b.unwrap();
        assert_eq!(a.credential_fingerprint, b.credential_fingerprint);
        assert_eq!(a.headers["x-user-id"], "42");
        assert_eq!(s.fixture.refresh_calls.load(Ordering::SeqCst), 1);
        assert_eq!(s.fixture.account_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn expired_refresh_and_wrong_principal_fail_without_overwriting_pair() {
        let s = server(Fixture::default()).await;
        let mut c = credential(42);
        c.access_expires_at_ms = Some(now_ms() - 1);
        c.refresh_expires_at_ms = Some(now_ms() - 1);
        save(&s.auth, "fixture", &c).await;
        assert_eq!(
            s.auth.session_status("fixture").await.unwrap().status,
            "access_expired"
        );
        assert!(s.auth.refresh("fixture").await.is_err());
        assert_eq!(s.fixture.refresh_calls.load(Ordering::SeqCst), 0);
        save(&s.auth, "fixture", &credential(99)).await;
        assert!(s
            .auth
            .refresh("fixture")
            .await
            .unwrap_err()
            .contains("身份不一致"));
        assert_eq!(saved(&s.auth, "fixture").await.access_token, ACCESS);
        assert_eq!(
            saved(&s.auth, "fixture").await.snapshot.identity_hash,
            credential(99).snapshot.identity_hash
        );
    }

    #[tokio::test]
    async fn redirects_and_oversize_responses_do_not_bypass_fixed_issuer_contract() {
        let s = server(Fixture::default()).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route(
                "/redirect",
                get(|| async { (StatusCode::FOUND, [("Location", "https://evil.invalid")]) }),
            )
            .route("/large", get(|| async { "x".repeat(BODY_LIMIT + 1) }));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut auth = s.auth.clone();
        auth.base = base;
        assert!(auth
            .request(Method::GET, "/redirect", &[], None, false)
            .await
            .is_err());
        assert_eq!(
            auth.request(Method::GET, "/large", &[], None, false)
                .await
                .unwrap_err(),
            "授权响应过大"
        );
        task.abort();
    }
}

#[derive(Clone)]
pub struct WorkBuddyAuth {
    pool: SqlitePool,
    client: Client,
    base: String,
    gate: Arc<Mutex<()>>,
    #[cfg(test)]
    before_commit: Option<Arc<tokio::sync::Notify>>,
}

impl WorkBuddyAuth {
    #[cfg(test)]
    pub(crate) fn with_test_origin(pool: SqlitePool, origin: String) -> Self {
        let mut auth = Self::new(pool, None).expect("test HTTP client");
        auth.base = origin;
        auth
    }

    #[cfg(test)]
    pub(crate) async fn test_seed_account(
        &self,
        account_id: &str,
        uid: &str,
        access: &str,
        refresh: &str,
    ) {
        assert!(valid_id(account_id));
        let mut credential = Credential {
            issuer: ISSUER.into(),
            access_token: access.into(),
            refresh_token: Some(refresh.into()),
            domain: ISSUER.into(),
            access_expires_at_ms: None,
            refresh_expires_at_ms: None,
            enterprise_id: None,
            uid: Some(uid.into()),
            snapshot: AccountSnapshot::default(),
            refresh_attempted: false,
        };
        credential.snapshot = snapshot(
            &json!({"code":0,"data":{"uid":uid,"domain":ISSUER}}),
            &credential,
            None,
        )
        .unwrap();
        repo::set_secret(
            &self.pool,
            &session_key(account_id),
            &encode(&credential).unwrap(),
        )
        .await
        .unwrap();
    }

    pub fn issuer() -> &'static str {
        ISSUER
    }
    pub fn new(pool: SqlitePool, proxy: Option<&str>) -> Result<Self, String> {
        let client = crate::outbound::apply_proxy(
            Client::builder()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(6))
                .timeout(Duration::from_secs(20)),
            proxy,
        )
        .map_err(|_| "登录代理配置无效")?
        .build()
        .map_err(|_| "无法初始化登录服务")?;
        Ok(Self {
            pool,
            client,
            base: ISSUER.into(),
            gate: Arc::new(Mutex::new(())),
            #[cfg(test)]
            before_commit: None,
        })
    }

    pub(crate) fn with_proxy(&self, proxy: Option<&str>) -> Result<Self, String> {
        let mut next = Self::new(self.pool.clone(), proxy)?;
        next.gate = self.gate.clone();
        next.base = self.base.clone();
        Ok(next)
    }

    pub(crate) async fn prepare_model_authorization(
        &self,
        account: &str,
    ) -> Result<ModelAuthorization, String> {
        if !valid_id(account) {
            return Err("账号 ID 无效".into());
        }
        let _guard = self.gate.lock().await;
        let key = session_key(account);
        let mut cipher = self.read(&key).await?.ok_or("请先登录 WorkBuddy")?;
        let mut credential: Credential = decode(&cipher)?;
        validate_credential(&credential)?;
        if credential
            .access_expires_at_ms
            .is_some_and(|at| at <= now_ms() + 60_000)
        {
            self.refresh_locked(account, &cipher).await?;
            cipher = self.read(&key).await?.ok_or("账号登录已移除")?;
            credential = decode(&cipher)?;
            if credential
                .access_expires_at_ms
                .is_some_and(|at| at <= now_ms())
            {
                return Err("访问认证已过期，请重新登录".into());
            }
        }
        let identity = credential
            .snapshot
            .identity_hash
            .clone()
            .filter(|v| v.len() == 64)
            .ok_or("账号绑定缺失，请重新登录")?;
        if credential.uid.is_none() {
            let value = self
                .request(
                    Method::GET,
                    "/v2/plugin/accounts",
                    &[],
                    Some(&credential),
                    false,
                )
                .await?;
            let (entry, snapshot) = bound_account(&value, &credential, &credential)?;
            credential.uid = Some(account_uid(entry)?);
            credential.snapshot = snapshot;
            let next = encode(&credential)?;
            self.replace(&key, &cipher, &next).await?;
        }
        let uid = credential
            .uid
            .as_deref()
            .ok_or("账号标识缺失，请重新登录")?;
        if format!(
            "{:x}",
            Sha256::digest(format!("workbuddy:{uid}").as_bytes())
        ) != identity
        {
            return Err("账号标识与登录身份不一致，请重新登录".into());
        }
        let mut values = vec![
            (
                "authorization",
                format!("Bearer {}", credential.access_token),
            ),
            ("x-user-id", uid.into()),
            ("x-domain", credential.domain.clone()),
            ("x-product", "SaaS".into()),
        ];
        if let Some(enterprise) = credential.enterprise_id {
            values.push(("x-enterprise-id", enterprise.clone()));
            values.push(("x-tenant-id", enterprise));
        }
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in values {
            let mut value = HeaderValue::from_str(&value).map_err(|_| "账号模型认证头无效")?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        Ok(ModelAuthorization {
            headers,
            identity_hash: identity,
            credential_fingerprint: format!(
                "{:x}",
                Sha256::digest(credential.access_token.as_bytes())
            ),
        })
    }

    async fn request(
        &self,
        method: Method,
        route: &str,
        query: &[(&str, &str)],
        auth: Option<&Credential>,
        refresh: bool,
    ) -> Result<Value, String> {
        let mut request = self
            .client
            .request(method.clone(), format!("{}{}", self.base, route))
            .query(query)
            .header("Accept", "application/json")
            .header("X-Domain", auth.map_or("www.workbuddy.ai", |c| &c.domain));
        if method == Method::POST {
            request = request.json(&json!({}));
        }
        if let Some(auth) = auth {
            validate_credential(auth)?;
            if refresh {
                let mut value = HeaderValue::from_str(
                    auth.refresh_token
                        .as_deref()
                        .ok_or("没有刷新令牌，请重新登录")?,
                )
                .map_err(|_| "刷新令牌无效")?;
                value.set_sensitive(true);
                request = request
                    .header("X-Refresh-Token", value)
                    .header("X-Auth-Refresh-Source", "plugin");
            } else {
                request = request.bearer_auth(&auth.access_token);
            }
        } else {
            request = request.header("X-No-Authorization", "true");
        }
        request = request
            .header("X-No-User-Id", "true")
            .header("X-No-Enterprise-Id", "true")
            .header("X-No-Department-Info", "true");
        let response = request.send().await.map_err(|_| "授权网络请求失败或超时")?;
        if !response.status().is_success() {
            return Err("授权服务 HTTP 请求失败，请检查登录状态".into());
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "授权响应读取失败")?;
            if body.len() + chunk.len() > BODY_LIMIT {
                return Err("授权响应过大".into());
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| "授权响应格式无效".into())
    }

    async fn read(&self, key: &str) -> Result<Option<String>, String> {
        repo::get_secret(&self.pool, key)
            .await
            .map_err(|_| "无法读取加密登录资料".into())
    }
    async fn replace(&self, key: &str, old: &str, new: &str) -> Result<(), String> {
        let result = sqlx::query(
            "UPDATE app_secrets SET value_enc = ?, updated_at = ? WHERE name = ? AND value_enc = ?",
        )
        .bind(new)
        .bind(chrono::Utc::now())
        .bind(key)
        .bind(old)
        .execute(&self.pool)
        .await
        .map_err(|_| "无法保存加密登录资料")?;
        if result.rows_affected() != 1 {
            return Err("登录状态已变化，本次结果未覆盖现有资料".into());
        }
        Ok(())
    }

    pub async fn start(&self, account_id: &str) -> Result<LoginStart, String> {
        if !valid_id(account_id) {
            return Err("账号 ID 无效".into());
        }
        let _guard = self.gate.lock().await;
        let baseline = self.read(&session_key(account_id)).await?;
        if let Some(cipher) = &baseline {
            validate_credential(&decode::<Credential>(cipher)?)?;
        }
        // Only our encrypted pending namespace is inspected, bounded to 33 rows.
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name, value_enc FROM app_secrets WHERE name LIKE ? LIMIT 33")
                .bind(format!("{PREFIX}pending:%"))
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "无法读取授权会话")?;
        let mut active = 0;
        for (key, cipher) in rows {
            match decode::<PendingLogin>(&cipher) {
                Ok(pending) if pending.deadline_ms <= now_ms() => {
                    sqlx::query("DELETE FROM app_secrets WHERE name = ? AND value_enc = ?")
                        .bind(&key)
                        .bind(&cipher)
                        .execute(&self.pool)
                        .await
                        .map_err(|_| "无法清理过期授权")?;
                }
                _ => active += 1,
            }
        }
        if active >= 32 {
            return Err("未完成授权过多，请取消或等待过期".into());
        }
        let value = self
            .request(
                Method::POST,
                "/v2/plugin/auth/state",
                &[("platform", "workbuddy-ai")],
                None,
                false,
            )
            .await?;
        if value["code"].as_i64() != Some(0) {
            return Err("授权服务拒绝创建登录会话".into());
        }
        let data = &value["data"];
        let state = data["state"]
            .as_str()
            .filter(|v| valid_token(v) && v.len() <= 512)
            .ok_or("授权服务未返回有效 state")?;
        let url = data["authUrl"]
            .as_str()
            .filter(|v| valid_authorization_url(v))
            .ok_or("授权网址不是已验证的官方域名")?;
        if Url::parse(url)
            .map_err(|_| "授权网址无效")?
            .query_pairs()
            .any(|(k, v)| k == "state" && v != state)
        {
            return Err("授权网址与当前会话不一致".into());
        }
        let ttl = match data.get("expiresIn").filter(|v| !v.is_null()) {
            Some(v) => Some(v.as_i64().filter(|v| *v > 0).ok_or("授权会话有效期无效")?),
            None => None,
        };
        let wait_ms = ttl
            .map(|s| s.checked_mul(1000).ok_or("授权会话有效期超出范围"))
            .transpose()?
            .unwrap_or(LOGIN_LIMIT_MS)
            .min(LOGIN_LIMIT_MS);
        let deadline = now_ms().checked_add(wait_ms).ok_or("授权会话时间无效")?;
        let login_id = uuid::Uuid::new_v4().to_string();
        let pending = PendingLogin {
            account_id: account_id.into(),
            state: state.into(),
            deadline_ms: deadline,
            server_ttl_seconds: ttl,
            baseline_cipher: baseline,
            reserved: false,
            credential: None,
        };
        let inserted = sqlx::query("INSERT INTO app_secrets (name,value_enc,created_at,updated_at) SELECT ?,?,?,? WHERE (SELECT COUNT(*) FROM app_secrets WHERE name LIKE ?) < 32")
            .bind(pending_key(&login_id)).bind(encode(&pending)?).bind(chrono::Utc::now()).bind(chrono::Utc::now())
            .bind(format!("{PREFIX}pending:%")).execute(&self.pool).await.map_err(|_| "无法保存加密授权会话")?;
        if inserted.rows_affected() != 1 {
            return Err("未完成授权过多，请取消或等待过期".into());
        }
        Ok(LoginStart {
            login_id,
            authorization_url: url.into(),
            deadline_ms: deadline,
            server_ttl_seconds: ttl,
        })
    }

    pub async fn cancel(&self, login_id: &str) -> Result<(), String> {
        if uuid::Uuid::parse_str(login_id).is_err() {
            return Err("登录会话 ID 无效".into());
        }
        repo::delete_secret(&self.pool, &pending_key(login_id))
            .await
            .map_err(|_| "无法取消授权会话")?;
        Ok(())
    }

    pub async fn session_status(&self, account: &str) -> Result<LoginStatus, String> {
        if !valid_id(account) {
            return Err("账号 ID 无效".into());
        }
        let credential = self
            .read(&session_key(account))
            .await?
            .map(|c| decode::<Credential>(&c))
            .transpose()?;
        if let Some(c) = &credential {
            validate_credential(c)?;
        }
        Ok(status(account, credential.as_ref()))
    }

    pub async fn poll(&self, login_id: &str) -> Result<LoginStatus, String> {
        if uuid::Uuid::parse_str(login_id).is_err() {
            return Err("登录会话 ID 无效".into());
        }
        let _guard = self.gate.lock().await;
        let key = pending_key(login_id);
        let old = self.read(&key).await?.ok_or("授权会话不存在或已结束")?;
        let mut pending: PendingLogin = decode(&old)?;
        if pending.deadline_ms <= now_ms() {
            self.cancel(login_id).await?;
            return Err("授权会话已过期，请重新登录".into());
        }
        if pending.reserved {
            return Err("授权正在处理或被中断，请取消后重新登录".into());
        }
        if !valid_id(&pending.account_id) || !valid_token(&pending.state) {
            return Err("授权会话资料无效".into());
        }
        pending.reserved = true;
        let reserved = encode(&pending)?;
        self.replace(&key, &old, &reserved).await?;
        let result = self.poll_reserved(&key, &reserved, &mut pending).await;
        if result.is_err() {
            // GET failures may be checked again. CAS prevents resurrecting a cancelled session.
            pending.reserved = false;
            let _ = self.replace(&key, &reserved, &encode(&pending)?).await;
        }
        result
    }

    async fn poll_reserved(
        &self,
        key: &str,
        reserved: &str,
        pending: &mut PendingLogin,
    ) -> Result<LoginStatus, String> {
        if pending.credential.is_none() {
            let value = self
                .request(
                    Method::GET,
                    "/v2/plugin/auth/token",
                    &[("state", &pending.state)],
                    None,
                    false,
                )
                .await?;
            if value["code"].as_i64() == Some(11217) {
                return self.waiting(key, reserved, pending).await;
            }
            pending.credential = Some(parse_token(&value, None, now_ms())?);
        }
        let credential = pending.credential.as_mut().ok_or("授权未返回令牌")?;
        let value = self
            .request(
                Method::GET,
                "/v2/plugin/login/account",
                &[("state", &pending.state)],
                Some(credential),
                false,
            )
            .await?;
        if value["code"].as_i64() == Some(12151) {
            return self.waiting(key, reserved, pending).await;
        }
        let previous = pending
            .baseline_cipher
            .as_deref()
            .map(decode::<Credential>)
            .transpose()?;
        if let Some(previous) = &previous {
            let mut old_secrets = vec![previous.access_token.as_str()];
            old_secrets.extend(previous.refresh_token.as_deref());
            if reflects(&value, &old_secrets) {
                return Err("账号响应包含凭据，已拒绝展示".into());
            }
        }
        credential.snapshot = snapshot(&value, credential, Some(&pending.state))?;
        credential.enterprise_id = enterprise_id(&value["data"])?;
        credential.uid = Some(account_uid(&value["data"])?);
        if let Some(previous) = &previous {
            if previous.snapshot.identity_hash != credential.snapshot.identity_hash
                || previous.enterprise_id != credential.enterprise_id
            {
                return Err("登录账号与已绑定账号不一致，请使用新的账号 ID".into());
            }
        }
        if pending.deadline_ms <= now_ms() {
            return Err("授权会话已过期，未保存令牌".into());
        }
        let encrypted = encode(credential)?;
        #[cfg(test)]
        if let Some(notify) = &self.before_commit {
            notify.notify_one();
        }
        // Complete login atomically with pending consumption and baseline comparison.
        let mut tx = self.pool.begin().await.map_err(|_| "无法保存授权结果")?;
        let removed = sqlx::query("DELETE FROM app_secrets WHERE name = ? AND value_enc = ?")
            .bind(key)
            .bind(reserved)
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法结束授权会话")?;
        if removed.rows_affected() != 1 {
            return Err("授权已取消或状态已变化，未保存令牌".into());
        }
        // DELETE acquired the write lock; time waiting for SQLite must not extend the grant.
        if pending.deadline_ms <= now_ms() {
            return Err("授权会话已过期，未保存令牌".into());
        }
        let session = session_key(&pending.account_id);
        let saved = if let Some(baseline) = &pending.baseline_cipher {
            sqlx::query("UPDATE app_secrets SET value_enc = ?, updated_at = ? WHERE name = ? AND value_enc = ?")
                .bind(&encrypted).bind(chrono::Utc::now()).bind(&session).bind(baseline).execute(&mut *tx).await
        } else {
            sqlx::query("INSERT INTO app_secrets (name, value_enc, created_at, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(name) DO NOTHING")
                .bind(&session).bind(&encrypted).bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(&mut *tx).await
        }.map_err(|_| "无法保存加密认证")?;
        if saved.rows_affected() != 1 {
            return Err("账号认证已变化，本次登录未覆盖原资料".into());
        }
        if pending.deadline_ms <= now_ms() {
            return Err("授权会话已过期，未保存令牌".into());
        }
        tx.commit().await.map_err(|_| "无法提交加密认证")?;
        Ok(status(&pending.account_id, Some(credential)))
    }

    async fn waiting(
        &self,
        key: &str,
        reserved: &str,
        pending: &mut PendingLogin,
    ) -> Result<LoginStatus, String> {
        pending.reserved = false;
        self.replace(key, reserved, &encode(pending)?).await?;
        let mut value = status(&pending.account_id, None);
        value.status = "awaiting_authorization".into();
        Ok(value)
    }

    /// Explicit refresh is persisted as an attempted mutation before HTTP. No automatic replay.
    pub async fn refresh(&self, account: &str) -> Result<LoginStatus, String> {
        if !valid_id(account) {
            return Err("账号 ID 无效".into());
        }
        let key = session_key(account);
        let old = self.read(&key).await?.ok_or("尚未登录 WorkBuddy")?;
        let _guard = self.gate.lock().await;
        let current = self.read(&key).await?.ok_or("账号登录已移除")?;
        if current != old {
            let current: Credential = decode(&current)?;
            validate_credential(&current)?;
            if !current.refresh_attempted {
                return Ok(status(account, Some(&current)));
            }
            return Err("该认证的刷新已尝试或中断，请重新登录；不会自动重试".into());
        }
        self.refresh_locked(account, &old).await
    }

    async fn refresh_locked(&self, account: &str, old: &str) -> Result<LoginStatus, String> {
        let key = session_key(account);
        let mut credential: Credential = decode(old)?;
        validate_credential(&credential)?;
        if credential.refresh_attempted {
            return Err("该认证的刷新已尝试或中断，请重新登录；不会自动重试".into());
        }
        if credential.refresh_token.is_none() {
            return Err("没有刷新令牌，请重新登录".into());
        }
        if credential
            .refresh_expires_at_ms
            .is_some_and(|t| t <= now_ms())
        {
            return Err("刷新令牌已过期，请重新登录".into());
        }
        credential.refresh_attempted = true;
        let reserved = encode(&credential)?;
        self.replace(&key, old, &reserved).await?;
        let value = self
            .request(
                Method::POST,
                "/v2/plugin/auth/token/refresh",
                &[],
                Some(&credential),
                true,
            )
            .await?;
        let mut refreshed = parse_token(&value, Some(&credential), now_ms())?;
        if refreshed.access_token == credential.access_token {
            return Err("刷新未返回新的访问认证；不会重复本次刷新，请重新登录".into());
        }
        // Refresh must not replace a different principal, even on a nominally successful response.
        let account_value = self
            .request(
                Method::GET,
                "/v2/plugin/accounts",
                &[],
                Some(&refreshed),
                false,
            )
            .await?;
        let (entry, snapshot) = bound_account(&account_value, &refreshed, &credential)?;
        refreshed.snapshot = snapshot;
        refreshed.uid = Some(account_uid(entry)?);
        self.replace(&key, &reserved, &encode(&refreshed)?).await?;
        Ok(status(account, Some(&refreshed)))
    }
}
