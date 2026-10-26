//! This application's Qoder device login. Cloud device credentials are distinct
//! from PATs, model job tokens, and native COSY machine context.
use crate::{client_session::AccountSnapshot, crypto, db::repo};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::StreamExt;
use rand::RngCore;
use reqwest::{header::HeaderValue, Client, Method, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

const PREFIX: &str = "account-login:qoder:";
const WAIT_MS: i64 = 300_000;
const BODY_LIMIT: usize = 512 * 1024;
// Public client metadata from both locally installed product bundles.
const CLIENT_ID: &str = "732aef47-9cf2-46a2-95fe-4cebb5d0d1fa";

mod diagnostics;
mod job;
mod protocol;
pub(crate) use protocol::invoke_trae;
pub mod request;
pub mod stream;
pub(crate) use job::ModelAuthorization;
#[cfg(test)]
pub(crate) async fn test_decode_wire_body(encoded: &str) -> Value {
    protocol::invoke(&json!({"operation":"decode_catalog","body":encoded}))
        .await
        .unwrap()
}
pub use job::{CatalogModel, JobStatus, SigningStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    Global,
    Cn,
}

impl Region {
    pub(crate) fn model_origin(self) -> &'static str {
        match self {
            Self::Global => "https://api2.qoder.sh",
            Self::Cn => "https://gateway.qoder.com.cn",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Cn => "cn",
        }
    }
    fn web(self) -> &'static str {
        match self {
            Self::Global => "https://qoder.com",
            Self::Cn => "https://qoder.cn",
        }
    }
    fn api(self) -> &'static str {
        match self {
            Self::Global => "https://openapi.qoder.sh",
            Self::Cn => "https://openapi.qoder.com.cn",
        }
    }
}

// None of the encrypted record types implements Debug or enters a public DTO.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    region: Region,
    issuer: String,
    access: String,
    refresh: Option<String>,
    uid: String,
    organization: Option<String>,
    installation_id: String,
    access_expires_at_ms: Option<i64>,
    refresh_expires_at_ms: Option<i64>,
    snapshot: AccountSnapshot,
    refresh_attempted: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    region: Region,
    account_id: String,
    nonce: String,
    verifier: String,
    installation_id: String,
    deadline_ms: i64,
    baseline: Option<String>,
    reserved: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LoginStart {
    pub login_id: String,
    pub region: Region,
    pub authorization_url: String,
    pub deadline_ms: i64,
    pub server_ttl_seconds: Option<i64>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LoginStatus {
    pub status: String,
    pub region: Region,
    pub account_id: String,
    pub identity: Option<AccountSnapshot>,
    pub access_expires_at_ms: Option<i64>,
    pub refresh_expires_at_ms: Option<i64>,
    pub can_refresh: bool,
    pub refresh_attempted: bool,
}

#[derive(Clone)]
pub struct QoderAuth {
    pool: SqlitePool,
    client: Client,
    gate: Arc<Mutex<()>>,
    #[cfg(test)]
    test_origin: Option<Url>,
    #[cfg(test)]
    job_reserve_pause: Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
    #[cfg(test)]
    job_commit_pause: Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
    #[cfg(test)]
    signing_commit_pause: Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
    #[cfg(test)]
    signing_cache_pause: Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn valid_account(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8192
        && value.is_ascii()
        && !value
            .bytes()
            .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
        && HeaderValue::from_str(value).is_ok()
}
fn key(region: Region, category: &str, id: &str) -> String {
    format!("{PREFIX}{}:{category}:{id}", region.key())
}
fn encode<T: Serialize>(value: &T) -> Result<String, String> {
    crypto::encrypt(&serde_json::to_string(value).map_err(|_| "认证编码失败")?)
        .map_err(|_| "认证加密失败".into())
}
fn decode<T: DeserializeOwned>(value: &str) -> Result<T, String> {
    serde_json::from_str(&crypto::decrypt(value).map_err(|_| "认证解密失败")?)
        .map_err(|_| "认证记录无效".into())
}
fn status(region: Region, account: &str, credential: Option<&Credential>) -> LoginStatus {
    let expired = credential
        .and_then(|c| c.access_expires_at_ms)
        .is_some_and(|time| time <= now_ms());
    LoginStatus {
        status: if credential.is_none() {
            "not_logged_in"
        } else if expired {
            "expired"
        } else {
            "logged_in"
        }
        .into(),
        region,
        account_id: account.into(),
        identity: credential.map(|c| c.snapshot.clone()),
        access_expires_at_ms: credential.and_then(|c| c.access_expires_at_ms),
        refresh_expires_at_ms: credential.and_then(|c| c.refresh_expires_at_ms),
        can_refresh: credential.is_some_and(|c| {
            c.refresh.is_some()
                && !c.refresh_attempted
                && !c.refresh_expires_at_ms.is_some_and(|time| time <= now_ms())
        }),
        refresh_attempted: credential.is_some_and(|c| c.refresh_attempted),
    }
}
fn valid_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value)
}
fn validate(credential: &Credential, region: Region) -> Result<(), String> {
    if credential.region != region
        || credential.issuer != region.api()
        || !valid_token(&credential.access)
        || credential.refresh.as_ref().is_some_and(|v| !valid_token(v))
        || !valid_uuid(&credential.installation_id)
        || credential.uid.is_empty()
        || credential.uid.len() > 128
        || credential.uid.chars().any(char::is_control)
    {
        return Err("Qoder认证或区域绑定无效".into());
    }
    Ok(())
}

fn reflects(value: &Value, secrets: &[&str]) -> bool {
    match value {
        Value::String(text) => secrets
            .iter()
            .any(|secret| !secret.is_empty() && text.contains(secret)),
        Value::Array(values) => values.iter().any(|value| reflects(value, secrets)),
        Value::Object(values) => values.values().any(|value| reflects(value, secrets)),
        _ => false,
    }
}

fn data(value: &Value) -> Result<&Value, String> {
    if value.get("code").is_some_and(|code| {
        !code.is_null() && code.as_i64() != Some(0) && code.as_str() != Some("0")
    }) {
        return Err("Qoder认证服务返回业务错误".into());
    }
    match value.get("data") {
        Some(value) if value.is_object() => Ok(value),
        None if value.is_object() => Ok(value),
        _ => Err("Qoder认证响应结构无效".into()),
    }
}

fn text_alias(value: &Value, names: &[&str], required: bool) -> Result<Option<String>, String> {
    let mut result: Option<String> = None;
    for name in names {
        if let Some(raw) = value.get(*name).filter(|value| !value.is_null()) {
            let text = raw
                .as_str()
                .filter(|text| valid_token(text))
                .ok_or("Qoder令牌字段无效")?;
            if result.as_deref().is_some_and(|old| old != text) {
                return Err("Qoder令牌字段冲突".into());
            }
            result = Some(text.into());
        }
    }
    if required && result.is_none() {
        return Err("Qoder认证响应缺少令牌".into());
    }
    Ok(result)
}

fn expiry(value: &Value, absolute: &str, relative: &str) -> Result<Option<i64>, String> {
    // Current public desktop device-login code feeds these fields directly to
    // new Date(value). They have an established millisecond contract, unlike
    // numeric expires_at, whose unit must not be inferred from its magnitude.
    let millis_field = match absolute {
        "expires_at" => "expire_time",
        "refresh_token_expires_at" => "refresh_token_expire_time",
        _ => return Err("Qoder有效期字段合同无效".into()),
    };
    if let Some(raw) = value.get(millis_field).filter(|raw| !raw.is_null()) {
        let millis = raw
            .as_i64()
            .filter(|time| *time > 0)
            .ok_or("Qoder毫秒有效期字段无效")?;
        if chrono::DateTime::from_timestamp_millis(millis).is_none() {
            return Err("Qoder毫秒有效期超出范围".into());
        }
        return Ok(Some(millis));
    }
    if let Some(raw) = value.get(absolute).filter(|value| !value.is_null()) {
        // Only the installed product's ISO timestamp contract is interpreted.
        // A numeric absolute timestamp has an unverified unit and stays unknown.
        if raw.is_number() {
            return Ok(None);
        }
        let date = raw.as_str().ok_or("Qoder有效期字段无效")?;
        return chrono::DateTime::parse_from_rfc3339(date)
            .map(|date| Some(date.timestamp_millis()))
            .map_err(|_| "Qoder有效期字段无效".into());
    }
    if let Some(raw) = value.get(relative).filter(|value| !value.is_null()) {
        let seconds = raw
            .as_i64()
            .filter(|seconds| *seconds > 0)
            .ok_or("Qoder有效期字段无效")?;
        return now_ms()
            .checked_add(seconds.checked_mul(1000).ok_or("Qoder有效期超出范围")?)
            .map(Some)
            .ok_or("Qoder有效期超出范围".into());
    }
    Ok(None)
}

fn parse_credential(
    region: Region,
    installation_id: &str,
    value: &Value,
    previous: Option<&Credential>,
) -> Result<Credential, String> {
    let value = data(value)?;
    if value
        .get("user_id")
        .is_some_and(|raw| !raw.is_null() && profile_text(raw).is_none())
    {
        return Err("Qoder令牌身份字段无效".into());
    }
    let access = text_alias(value, &["device_token", "token", "access_token"], true)?
        .ok_or("Qoder认证响应缺少令牌")?;
    let refresh = text_alias(value, &["refresh_token", "refreshToken"], false)?;
    let retained = refresh.is_none();
    let refresh = refresh.or_else(|| previous.and_then(|old| old.refresh.clone()));
    let refresh_expires_at_ms = expiry(
        value,
        "refresh_token_expires_at",
        "refresh_token_expires_in",
    )?
    .or_else(|| {
        retained
            .then(|| previous.and_then(|old| old.refresh_expires_at_ms))
            .flatten()
    });
    Ok(Credential {
        region,
        issuer: region.api().into(),
        access,
        refresh,
        uid: String::new(),
        organization: None,
        installation_id: installation_id.into(),
        access_expires_at_ms: expiry(value, "expires_at", "expires_in")?,
        refresh_expires_at_ms,
        snapshot: AccountSnapshot::default(),
        refresh_attempted: false,
    })
}

fn profile_text(raw: &Value) -> Option<&str> {
    raw.as_str()
        .filter(|text| !text.is_empty() && text.len() <= 128 && !text.chars().any(char::is_control))
}

fn profile_alias<'a>(
    value: &'a Value,
    names: &[&str],
    allow_empty: bool,
) -> Result<Option<&'a str>, String> {
    let mut found = None;
    for name in names {
        if let Some(raw) = value.get(*name).filter(|v| !v.is_null()) {
            if allow_empty && raw.as_str() == Some("") {
                continue;
            }
            let text = profile_text(raw).ok_or("Qoder账号身份或组织字段无效")?;
            if found.is_some_and(|old| old != text) {
                return Err("Qoder账号身份或组织字段冲突".into());
            }
            found = Some(text);
        }
    }
    Ok(found)
}

fn apply_profile(
    credential: &mut Credential,
    value: &Value,
    secrets: &[&str],
) -> Result<(), String> {
    if reflects(value, secrets) {
        return Err("Qoder账号响应包含凭据，已拒绝保存".into());
    }
    let value = data(value)?;
    let uid = profile_alias(value, &["id", "user_id", "uid"], false)?
        .ok_or("Qoder账号响应缺少有效身份")?;
    let flat = profile_alias(value, &["orgId", "organization_id", "organizationId"], true)?;
    let nested = match value.get("organization").filter(|v| !v.is_null()) {
        Some(raw) if raw.is_object() => profile_alias(raw, &["org_id", "orgId", "id"], true)?,
        Some(_) => return Err("Qoder组织结构无效".into()),
        None => None,
    };
    if flat.is_some() && nested.is_some() && flat != nested {
        return Err("Qoder账号组织字段冲突".into());
    }
    let organization = flat.or(nested).map(str::to_owned);
    let display_name = ["name", "username", "user_name"]
        .iter()
        .find_map(|name| profile_text(&value[*name]))
        .map(str::to_owned);
    let masked_email = profile_text(&value["email"]).and_then(|email| {
        let (name, host) = email.split_once('@')?;
        if name.is_empty() || host.is_empty() {
            return None;
        }
        Some(format!("{}***@{}", name.chars().next()?, host))
    });
    let identity = serde_json::to_string(&(credential.region.key(), uid, organization.as_deref()))
        .map_err(|_| "Qoder身份编码失败")?;
    credential.uid = uid.into();
    credential.organization = organization;
    credential.snapshot = AccountSnapshot {
        display_name,
        masked_email,
        identity_hash: Some(format!("{:x}", Sha256::digest(identity.as_bytes()))),
        captured_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    validate(credential, credential.region)
}

pub fn valid_authorization_url(value: &str, region: Region) -> bool {
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return false;
    }
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let Ok(base) = Url::parse(region.web()) else {
        return false;
    };
    if url.origin() != base.origin()
        || url.path() != "/device/selectAccounts"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let fields: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
    fields.len() == 5
        && url.query_pairs().count() == 5
        && fields.get("client_id").is_some_and(|v| v == CLIENT_ID)
        && fields.get("challenge_method").is_some_and(|v| v == "S256")
        && fields.get("challenge").is_some_and(|v| {
            v.len() == 43
                && URL_SAFE_NO_PAD
                    .decode(v.as_bytes())
                    .is_ok_and(|b| b.len() == 32)
        })
        && fields.get("nonce").is_some_and(|v| valid_uuid(v))
        && fields.get("machine_id").is_some_and(|v| valid_uuid(v))
}

impl QoderAuth {
    pub fn new(pool: SqlitePool, proxy: Option<&str>) -> Result<Self, String> {
        let client = crate::outbound::apply_proxy(
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(20)),
            proxy,
        )
        .map_err(|_| "Qoder代理配置无效")?
        .build()
        .map_err(|_| "Qoder认证传输初始化失败")?;
        Ok(Self {
            pool,
            client,
            gate: Arc::new(Mutex::new(())),
            #[cfg(test)]
            test_origin: None,
            #[cfg(test)]
            job_reserve_pause: None,
            #[cfg(test)]
            job_commit_pause: None,
            #[cfg(test)]
            signing_commit_pause: None,
            #[cfg(test)]
            signing_cache_pause: None,
        })
    }
    pub fn with_proxy(&self, proxy: Option<&str>) -> Result<Self, String> {
        let mut new = Self::new(self.pool.clone(), proxy)?;
        new.gate = self.gate.clone();
        #[cfg(test)]
        {
            new.test_origin = self.test_origin.clone();
            new.job_reserve_pause = self.job_reserve_pause.clone();
            new.job_commit_pause = self.job_commit_pause.clone();
            new.signing_commit_pause = self.signing_commit_pause.clone();
            new.signing_cache_pause = self.signing_cache_pause.clone();
        }
        Ok(new)
    }
    async fn read(&self, name: &str) -> Result<Option<String>, String> {
        repo::get_secret(&self.pool, name)
            .await
            .map_err(|_| "无法读取Qoder认证".into())
    }
    async fn replace(&self, name: &str, old: &str, new: &str) -> Result<(), String> {
        let changed = sqlx::query(
            "UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=? AND value_enc=?",
        )
        .bind(new)
        .bind(chrono::Utc::now())
        .bind(name)
        .bind(old)
        .execute(&self.pool)
        .await
        .map_err(|_| "无法更新Qoder认证")?;
        if changed.rows_affected() != 1 {
            return Err("Qoder认证已取消或发生变化".into());
        }
        Ok(())
    }
    async fn request(
        &self,
        region: Region,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        access: Option<&str>,
        body: Option<Value>,
    ) -> Result<(u16, Value), String> {
        let started = std::time::Instant::now();
        let operation = diagnostics::Operation::from_path(path);
        let mut url = Url::parse(region.api()).map_err(|_| "Qoder认证域名无效")?;
        url.set_path(path);
        #[cfg(test)]
        if let Some(origin) = &self.test_origin {
            url = origin.clone();
            url.set_path(&format!("/{}{path}", region.key()));
        }
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let mut request = self
            .client
            .request(method, url)
            .header("Accept", "application/json");
        if let Some(access) = access {
            let mut header = HeaderValue::from_str(&format!("Bearer {access}"))
                .map_err(|_| "Qoder授权值无效")?;
            header.set_sensitive(true);
            request = request.header("Authorization", header);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| {
            diagnostics::record(
                operation,
                diagnostics::Stage::Send,
                diagnostics::Kind::from_reqwest(&error),
                None,
                started.elapsed(),
            );
            "Qoder认证请求失败或超时"
        })?;
        let status = response.status().as_u16();
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                diagnostics::record(
                    operation,
                    diagnostics::Stage::ResponseRead,
                    diagnostics::Kind::from_reqwest(&error),
                    Some(status),
                    started.elapsed(),
                );
                "Qoder认证响应读取失败"
            })?;
            if bytes.len().saturating_add(chunk.len()) > BODY_LIMIT {
                diagnostics::record(
                    operation,
                    diagnostics::Stage::ResponseRead,
                    diagnostics::Kind::BodyLimit,
                    Some(status),
                    started.elapsed(),
                );
                return Err("Qoder认证响应过大".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        // Error text is never returned to IPC, logs or persisted diagnostics.
        if status != 200 {
            return Ok((status, Value::Null));
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| {
            diagnostics::record(
                operation,
                diagnostics::Stage::ResponseParse,
                diagnostics::Kind::InvalidJson,
                Some(status),
                started.elapsed(),
            );
            "Qoder认证响应不是有效JSON"
        })?;
        Ok((status, value))
    }
    pub async fn start(&self, region: Region, account: &str) -> Result<LoginStart, String> {
        if !valid_account(account) {
            return Err("本机账号名称无效".into());
        }
        let _guard = self.gate.lock().await;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name LIKE ? LIMIT 33")
                .bind(format!("{PREFIX}%:pending:%"))
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "无法读取Qoder授权会话")?;
        for (name, raw) in rows {
            if decode::<Pending>(&raw).is_ok_and(|p| p.deadline_ms <= now_ms()) {
                sqlx::query("DELETE FROM app_secrets WHERE name=? AND value_enc=?")
                    .bind(name)
                    .bind(raw)
                    .execute(&self.pool)
                    .await
                    .map_err(|_| "无法清理过期Qoder授权")?;
            }
        }
        // This is an application installation identifier for device login;
        // it does not replace, derive or claim to be a native COSY fingerprint.
        let device_key = key(region, "installation", "device");
        let installation_id = if let Some(raw) = self.read(&device_key).await? {
            decode::<String>(&raw)?
        } else {
            let proposed = uuid::Uuid::new_v4().to_string();
            let encoded = encode(&proposed)?;
            sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO NOTHING")
                .bind(&device_key).bind(encoded).bind(chrono::Utc::now()).bind(chrono::Utc::now())
                .execute(&self.pool).await.map_err(|_|"无法保存本软件设备登录标识")?;
            decode::<String>(
                &self
                    .read(&device_key)
                    .await?
                    .ok_or("本软件设备登录标识缺失")?,
            )?
        };
        if !valid_uuid(&installation_id) {
            return Err("本软件设备登录标识损坏".into());
        }
        let mut random = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let verifier = URL_SAFE_NO_PAD.encode(random);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let nonce = uuid::Uuid::new_v4().to_string();
        let login_id = uuid::Uuid::new_v4().to_string();
        let deadline_ms = now_ms().checked_add(WAIT_MS).ok_or("授权等待期限无效")?;
        let pending = Pending {
            region,
            account_id: account.into(),
            nonce: nonce.clone(),
            verifier,
            installation_id: installation_id.clone(),
            deadline_ms,
            baseline: self.read(&key(region, "session", account)).await?,
            reserved: false,
        };
        let encrypted = encode(&pending)?;
        let added=sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) SELECT ?,?,?,? WHERE (SELECT COUNT(*) FROM app_secrets WHERE name LIKE ?) < 32")
            .bind(key(region,"pending",&login_id)).bind(encrypted).bind(chrono::Utc::now()).bind(chrono::Utc::now())
            .bind(format!("{PREFIX}%:pending:%")).execute(&self.pool).await.map_err(|_|"无法创建Qoder授权会话")?;
        if added.rows_affected() != 1 {
            return Err("未完成Qoder授权过多，请取消或等待过期".into());
        }
        let mut url = Url::parse(region.web()).map_err(|_| "Qoder登录域名无效")?;
        url.set_path("/device/selectAccounts");
        url.query_pairs_mut()
            .append_pair("challenge", &challenge)
            .append_pair("challenge_method", "S256")
            .append_pair("nonce", &nonce)
            .append_pair("machine_id", &installation_id)
            .append_pair("client_id", CLIENT_ID);
        let authorization_url = url.to_string();
        if !valid_authorization_url(&authorization_url, region) {
            return Err("Qoder授权网址绑定无效".into());
        }
        Ok(LoginStart {
            login_id,
            region,
            authorization_url,
            deadline_ms,
            server_ttl_seconds: None,
        })
    }
    pub async fn session_status(
        &self,
        region: Region,
        account: &str,
    ) -> Result<LoginStatus, String> {
        if !valid_account(account) {
            return Err("本机账号名称无效".into());
        }
        let credential = self
            .read(&key(region, "session", account))
            .await?
            .map(|raw| decode::<Credential>(&raw))
            .transpose()?;
        if let Some(c) = &credential {
            validate(c, region)?;
        }
        Ok(status(region, account, credential.as_ref()))
    }
    pub async fn poll(&self, region: Region, login_id: &str) -> Result<LoginStatus, String> {
        if !valid_uuid(login_id) {
            return Err("授权会话ID无效".into());
        }
        let _guard = self.gate.lock().await;
        let name = key(region, "pending", login_id);
        let raw = self
            .read(&name)
            .await?
            .ok_or("Qoder授权已取消、过期或不存在")?;
        let mut pending: Pending = decode(&raw)?;
        if pending.region != region
            || !valid_account(&pending.account_id)
            || !valid_uuid(&pending.nonce)
            || !valid_uuid(&pending.installation_id)
            || pending.verifier.len() != 43
            || !valid_token(&pending.verifier)
        {
            return Err("Qoder授权会话绑定无效".into());
        }
        if pending.deadline_ms <= now_ms() {
            self.cancel(region, login_id).await?;
            return Err("Qoder授权等待已过期".into());
        }
        if pending.reserved {
            return Err("该授权已发起轮询且结果不明，请取消后重新登录".into());
        }
        pending.reserved = true;
        let reserved = encode(&pending)?;
        self.replace(&name, &raw, &reserved).await?;
        if pending.deadline_ms <= now_ms() {
            return Err("Qoder授权等待已过期".into());
        }
        let (http, value) = self
            .request(
                region,
                Method::GET,
                "/api/v1/deviceToken/poll",
                &[
                    ("nonce", &pending.nonce),
                    ("verifier", &pending.verifier),
                    ("challenge_method", "S256"),
                ],
                None,
                None,
            )
            .await?;
        if http == 404 || http == 202 {
            if pending.deadline_ms <= now_ms() {
                return Err("Qoder授权等待已过期".into());
            }
            pending.reserved = false;
            self.replace(&name, &reserved, &encode(&pending)?).await?;
            let mut waiting = status(region, &pending.account_id, None);
            waiting.status = "awaiting_authorization".into();
            return Ok(waiting);
        }
        if http != 200 {
            return Err(format!("Qoder授权轮询被拒绝（HTTP {http}）"));
        }
        let mut credential = parse_credential(region, &pending.installation_id, &value, None)?;
        if credential
            .access_expires_at_ms
            .is_some_and(|time| time <= now_ms())
        {
            return Err("Qoder授权返回已过期认证".into());
        }
        let mut secrets = vec![
            credential.access.as_str(),
            pending.verifier.as_str(),
            pending.nonce.as_str(),
        ];
        secrets.extend(credential.refresh.as_deref());
        // Profile validation is mandatory; a token response alone cannot claim
        // that the device grant belongs to the selected account and region.
        let (http, profile) = self
            .request(
                region,
                Method::GET,
                "/api/v1/userinfo",
                &[],
                Some(&credential.access),
                None,
            )
            .await?;
        if http != 200 {
            return Err("Qoder账号核验失败，未保存认证".into());
        }
        let mut checked = credential.clone();
        apply_profile(&mut checked, &profile, &secrets)?;
        let token_data = data(&value)?;
        if token_data["user_id"]
            .as_str()
            .is_some_and(|uid| uid != checked.uid)
        {
            return Err("Qoder令牌与账号身份不一致".into());
        }
        credential = checked;
        if let Some(baseline) = &pending.baseline {
            let previous: Credential = decode(baseline)?;
            validate(&previous, region)?;
            if credential.uid != previous.uid || credential.organization != previous.organization {
                return Err("Qoder登录与原账号或组织不一致".into());
            }
        }
        let cipher = encode(&credential)?;
        let mut transaction = self.pool.begin().await.map_err(|_| "无法保存Qoder登录")?;
        let removed = sqlx::query("DELETE FROM app_secrets WHERE name=? AND value_enc=?")
            .bind(&name)
            .bind(&reserved)
            .execute(&mut *transaction)
            .await
            .map_err(|_| "无法结束Qoder授权")?;
        if removed.rows_affected() != 1 || pending.deadline_ms <= now_ms() {
            return Err("Qoder授权已取消或过期，未保存认证".into());
        }
        let session = key(region, "session", &pending.account_id);
        let changed=match &pending.baseline {
            Some(baseline)=>sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=? AND value_enc=?")
                .bind(&cipher).bind(chrono::Utc::now()).bind(&session).bind(baseline).execute(&mut *transaction).await,
            None=>sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO NOTHING")
                .bind(&session).bind(&cipher).bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(&mut *transaction).await,
        }.map_err(|_|"无法保存Qoder认证")?;
        if changed.rows_affected() != 1
            || pending.deadline_ms <= now_ms()
            || credential
                .access_expires_at_ms
                .is_some_and(|time| time <= now_ms())
        {
            return Err("Qoder认证已变化或过期，原资料未被覆盖".into());
        }
        transaction
            .commit()
            .await
            .map_err(|_| "无法提交Qoder认证")?;
        Ok(status(region, &pending.account_id, Some(&credential)))
    }

    pub async fn refresh(&self, region: Region, account: &str) -> Result<LoginStatus, String> {
        if !valid_account(account) {
            return Err("本机账号名称无效".into());
        }
        let name = key(region, "session", account);
        let baseline = self.read(&name).await?.ok_or("尚未完成Qoder独立登录")?;
        let _guard = self.gate.lock().await;
        let current = self.read(&name).await?.ok_or("Qoder登录已移除")?;
        let mut previous: Credential = decode(&current)?;
        validate(&previous, region)?;
        if baseline != current && !previous.refresh_attempted {
            return Ok(status(region, account, Some(&previous)));
        }
        if previous.refresh_attempted {
            return Err("此认证已尝试刷新，失败或不明结果不会重放，请重新登录".into());
        }
        let refresh = previous
            .refresh
            .clone()
            .ok_or("Qoder未返回设备刷新凭据，请重新登录")?;
        if previous
            .refresh_expires_at_ms
            .is_some_and(|time| time <= now_ms())
        {
            return Err("Qoder刷新凭据已到期，请重新登录".into());
        }
        previous.refresh_attempted = true;
        let reserved = encode(&previous)?;
        self.replace(&name, &current, &reserved).await?;
        if previous
            .refresh_expires_at_ms
            .is_some_and(|time| time <= now_ms())
        {
            return Err("Qoder刷新凭据已到期，未发送刷新请求".into());
        }
        let (http, value) = self
            .request(
                region,
                Method::POST,
                "/api/v1/deviceToken/refresh",
                &[],
                None,
                Some(json!({"refresh_token":refresh})),
            )
            .await?;
        if http != 200 {
            return Err(format!(
                "Qoder刷新被拒绝（HTTP {http}），原认证保留且不重放"
            ));
        }
        let mut credential =
            parse_credential(region, &previous.installation_id, &value, Some(&previous))?;
        if credential.access == previous.access {
            return Err("Qoder刷新未返回新认证，原资料保留".into());
        }
        let mut secrets = vec![previous.access.as_str(), credential.access.as_str()];
        secrets.extend(previous.refresh.as_deref());
        secrets.extend(credential.refresh.as_deref());
        let (http, profile) = self
            .request(
                region,
                Method::GET,
                "/api/v1/userinfo",
                &[],
                Some(&credential.access),
                None,
            )
            .await?;
        if http != 200 {
            return Err("Qoder刷新后的账号核验失败，原认证保留".into());
        }
        let mut checked = credential.clone();
        apply_profile(&mut checked, &profile, &secrets)?;
        credential = checked;
        if data(&value)?["user_id"]
            .as_str()
            .is_some_and(|uid| uid != credential.uid)
        {
            return Err("Qoder刷新令牌与账号身份不一致".into());
        }
        if credential.uid != previous.uid || credential.organization != previous.organization {
            return Err("Qoder刷新账号或组织与原认证不一致".into());
        }
        if credential
            .access_expires_at_ms
            .is_some_and(|time| time <= now_ms())
        {
            return Err("Qoder刷新返回已过期认证".into());
        }
        let mut transaction = self.pool.begin().await.map_err(|_| "无法保存Qoder刷新")?;
        let changed = sqlx::query(
            "UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=? AND value_enc=?",
        )
        .bind(encode(&credential)?)
        .bind(chrono::Utc::now())
        .bind(&name)
        .bind(&reserved)
        .execute(&mut *transaction)
        .await
        .map_err(|_| "无法保存Qoder刷新")?;
        // Acquiring SQLite's write reservation may wait. Recheck after it has
        // been acquired so a token cannot expire while waiting and still commit.
        if changed.rows_affected() != 1
            || credential
                .access_expires_at_ms
                .is_some_and(|time| time <= now_ms())
        {
            return Err("Qoder刷新认证已变化或过期，原资料保留".into());
        }
        transaction
            .commit()
            .await
            .map_err(|_| "无法提交Qoder刷新")?;
        Ok(status(region, account, Some(&credential)))
    }
    pub async fn cancel(&self, region: Region, login_id: &str) -> Result<(), String> {
        if !valid_uuid(login_id) {
            return Err("授权会话ID无效".into());
        }
        sqlx::query("DELETE FROM app_secrets WHERE name=?")
            .bind(key(region, "pending", login_id))
            .execute(&self.pool)
            .await
            .map_err(|_| "无法取消Qoder授权")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use axum::{
        extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Mock {
        poll_code: std::sync::atomic::AtomicU16,
        poll: std::sync::Mutex<Value>,
        profile: std::sync::Mutex<Value>,
        refresh: std::sync::Mutex<Value>,
        refresh_code: std::sync::atomic::AtomicU16,
        polls: AtomicUsize,
        profiles: AtomicUsize,
        refreshes: AtomicUsize,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        hold: std::sync::atomic::AtomicBool,
    }
    struct Fixture {
        auth: QoderAuth,
        mock: Arc<Mock>,
        server: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn poll_handler(
        State(state): State<Arc<Mock>>,
        axum::extract::Query(query): axum::extract::Query<
            std::collections::HashMap<String, String>,
        >,
        headers: axum::http::HeaderMap,
    ) -> impl IntoResponse {
        assert_eq!(
            query.get("challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(query.get("nonce").is_some_and(|value| valid_uuid(value)));
        assert_eq!(query.get("verifier").map(String::len), Some(43));
        assert!(headers.get("authorization").is_none());
        state.polls.fetch_add(1, Ordering::SeqCst);
        if state.hold.load(Ordering::SeqCst) {
            state.entered.notify_one();
            state.release.notified().await;
        }
        (
            StatusCode::from_u16(state.poll_code.load(Ordering::SeqCst)).unwrap(),
            Json(state.poll.lock().unwrap().clone()),
        )
    }
    async fn profile_handler(
        State(state): State<Arc<Mock>>,
        headers: axum::http::HeaderMap,
    ) -> impl IntoResponse {
        let body = if state.refreshes.load(Ordering::SeqCst) > 0 {
            state.refresh.lock().unwrap().clone()
        } else {
            state.poll.lock().unwrap().clone()
        };
        let token = body["device_token"]
            .as_str()
            .or_else(|| body["token"].as_str())
            .unwrap();
        assert_eq!(
            headers["authorization"].to_str().unwrap(),
            format!("Bearer {token}")
        );
        state.profiles.fetch_add(1, Ordering::SeqCst);
        Json(state.profile.lock().unwrap().clone())
    }
    async fn refresh_handler(
        State(state): State<Arc<Mock>>,
        headers: axum::http::HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        assert!(headers.get("authorization").is_none());
        assert_eq!(body["refresh_token"], "fixture-refresh-old");
        state.refreshes.fetch_add(1, Ordering::SeqCst);
        (
            StatusCode::from_u16(state.refresh_code.load(Ordering::SeqCst)).unwrap(),
            Json(state.refresh.lock().unwrap().clone()),
        )
    }
    async fn fixture() -> Fixture {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            base64::engine::general_purpose::STANDARD.encode([23_u8; 32]),
        );
        let db = Db::connect_in_memory().await.unwrap();
        let mock = Arc::new(Mock {
            poll_code: std::sync::atomic::AtomicU16::new(200),
            poll: std::sync::Mutex::new(
                json!({"token":"fixture-access-old","refresh_token":"fixture-refresh-old","user_id":"account-1"}),
            ),
            profile: std::sync::Mutex::new(
                json!({"uid":"account-1","name":"Fixture User","email":"fixture@example.invalid","organization_id":"org-1"}),
            ),
            refresh: std::sync::Mutex::new(
                json!({"device_token":"fixture-access-new","refresh_token":"fixture-refresh-new","expires_in":3600}),
            ),
            refresh_code: std::sync::atomic::AtomicU16::new(200),
            polls: AtomicUsize::new(0),
            profiles: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            hold: std::sync::atomic::AtomicBool::new(false),
        });
        let app = Router::new()
            .route("/:region/api/v1/deviceToken/poll", get(poll_handler))
            .route("/:region/api/v1/userinfo", get(profile_handler))
            .route(
                "/:region/api/v1/deviceToken/refresh",
                axum::routing::post(refresh_handler),
            )
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut auth = QoderAuth::new(db.pool().clone(), None).unwrap();
        auth.test_origin = Some(Url::parse(&format!("http://{address}")).unwrap());
        Fixture { auth, mock, server }
    }
    async fn login(f: &Fixture) -> LoginStart {
        let start = f.auth.start(Region::Global, "main").await.unwrap();
        assert_eq!(
            f.auth
                .poll(Region::Global, &start.login_id)
                .await
                .unwrap()
                .status,
            "logged_in"
        );
        start
    }

    #[test]
    fn official_userinfo_id_username_and_nested_organization_are_verified() {
        for region in [Region::Global, Region::Cn] {
            let mut credential = parse_credential(
                region,
                &uuid::Uuid::new_v4().to_string(),
                &json!({"token":"fixture-access"}),
                None,
            )
            .unwrap();
            apply_profile(
                &mut credential,
                &json!({"id":"account-1","username":"Fixture User",
                "email":"fixture@example.invalid","organization":{"id":"org-1"}}),
                &["fixture-access"],
            )
            .unwrap();
            assert_eq!(credential.uid, "account-1");
            assert_eq!(credential.organization.as_deref(), Some("org-1"));
            assert_eq!(
                credential.snapshot.display_name.as_deref(),
                Some("Fixture User")
            );
            assert!(apply_profile(
                &mut credential,
                &json!({"id":"account-1","uid":"other"}),
                &[]
            )
            .is_err());
            for malformed in [
                json!({"id":7,"uid":"account-1"}),
                json!({"id":"account-1","orgId":"org-1","organization":{"id":"org-2"}}),
                json!({"id":"account-1","organization":[]}),
                json!({"id":"account-1","organizationId":7}),
            ] {
                assert!(apply_profile(&mut credential, &malformed, &[]).is_err());
            }
        }
    }

    #[tokio::test]
    async fn start_uses_region_bound_pkce_and_only_encrypted_own_pending() {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            base64::engine::general_purpose::STANDARD.encode([23_u8; 32]),
        );
        let db = Db::connect_in_memory().await.unwrap();
        let auth = QoderAuth::new(db.pool().clone(), None).unwrap();
        let global = auth.start(Region::Global, "main").await.unwrap();
        let cn = auth.start(Region::Cn, "main").await.unwrap();
        assert!(valid_authorization_url(
            &global.authorization_url,
            Region::Global
        ));
        assert!(valid_authorization_url(&cn.authorization_url, Region::Cn));
        assert!(!valid_authorization_url(
            &global.authorization_url,
            Region::Cn
        ));
        let pending_raw = auth
            .read(&key(Region::Global, "pending", &global.login_id))
            .await
            .unwrap()
            .unwrap();
        let pending: Pending = decode(&pending_raw).unwrap();
        assert!(!pending_raw.contains(&pending.verifier) && !pending_raw.contains(&pending.nonce));
        assert!(!global.authorization_url.contains(&pending.verifier));
        let url = Url::parse(&global.authorization_url).unwrap();
        let challenge = url
            .query_pairs()
            .find(|(key, _)| key == "challenge")
            .unwrap()
            .1
            .into_owned();
        assert_eq!(
            challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(pending.verifier.as_bytes()))
        );
        assert!(pending.deadline_ms > now_ms() && pending.deadline_ms <= now_ms() + WAIT_MS);
        assert_eq!(global.server_ttl_seconds, None);
        auth.cancel(Region::Global, &global.login_id).await.unwrap();
        assert!(auth
            .read(&key(Region::Global, "pending", &global.login_id))
            .await
            .unwrap()
            .is_none());
        assert!(auth
            .read(&key(Region::Cn, "pending", &cn.login_id))
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn pending_then_verified_login_is_consumed_and_tokens_never_enter_dto() {
        let f = fixture().await;
        f.mock.poll_code.store(404, Ordering::SeqCst);
        let start = f.auth.start(Region::Global, "main").await.unwrap();
        assert_eq!(
            f.auth
                .poll(Region::Global, &start.login_id)
                .await
                .unwrap()
                .status,
            "awaiting_authorization"
        );
        assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 0);
        f.mock.poll_code.store(200, Ordering::SeqCst);
        let result = f.auth.poll(Region::Global, &start.login_id).await.unwrap();
        assert_eq!(result.status, "logged_in");
        assert_eq!(result.access_expires_at_ms, None);
        assert!(result.can_refresh);
        assert_eq!(
            result.identity.as_ref().unwrap().masked_email.as_deref(),
            Some("f***@example.invalid")
        );
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("fixture-access"));
        assert!(f.auth.poll(Region::Global, &start.login_id).await.is_err());
        assert_eq!(f.mock.polls.load(Ordering::SeqCst), 2);
        assert_eq!(
            f.auth
                .session_status(Region::Cn, "main")
                .await
                .unwrap()
                .status,
            "not_logged_in"
        );
    }
    #[tokio::test]
    async fn login_cannot_commit_an_already_expired_token() {
        let f = fixture().await;
        f.mock.poll.lock().unwrap()["expires_at"] = json!("2020-01-01T00:00:00Z");
        let start = f.auth.start(Region::Global, "main").await.unwrap();
        assert!(f.auth.poll(Region::Global, &start.login_id).await.is_err());
        assert_eq!(
            f.auth
                .session_status(Region::Global, "main")
                .await
                .unwrap()
                .status,
            "not_logged_in"
        );
    }
    #[tokio::test]
    async fn late_poll_cannot_restore_a_cancelled_login() {
        let f = fixture().await;
        f.mock.hold.store(true, Ordering::SeqCst);
        let start = f.auth.start(Region::Global, "main").await.unwrap();
        let auth = f.auth.clone();
        let id = start.login_id.clone();
        let poll = tokio::spawn(async move { auth.poll(Region::Global, &id).await });
        f.mock.entered.notified().await;
        f.auth
            .cancel(Region::Global, &start.login_id)
            .await
            .unwrap();
        f.mock.release.notify_one();
        assert!(poll.await.unwrap().is_err());
        assert_eq!(
            f.auth
                .session_status(Region::Global, "main")
                .await
                .unwrap()
                .status,
            "not_logged_in"
        );
    }
    #[tokio::test]
    async fn token_profile_mismatch_and_reflected_secrets_are_not_saved() {
        for profile in [
            json!({"uid":"different-owner"}),
            json!({"uid":"account-1","name":"fixture-access-old"}),
        ] {
            let f = fixture().await;
            *f.mock.profile.lock().unwrap() = profile;
            let start = f.auth.start(Region::Global, "main").await.unwrap();
            assert!(f.auth.poll(Region::Global, &start.login_id).await.is_err());
            assert_eq!(
                f.auth
                    .session_status(Region::Global, "main")
                    .await
                    .unwrap()
                    .status,
                "not_logged_in"
            );
        }
    }
    #[tokio::test]
    async fn failed_refresh_is_not_replayed_and_old_pair_is_retained() {
        let f = fixture().await;
        login(&f).await;
        f.mock.refresh_code.store(502, Ordering::SeqCst);
        assert!(f.auth.refresh(Region::Global, "main").await.is_err());
        assert!(f.auth.refresh(Region::Global, "main").await.is_err());
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
        let raw = f
            .auth
            .read(&key(Region::Global, "session", "main"))
            .await
            .unwrap()
            .unwrap();
        let old: Credential = decode(&raw).unwrap();
        assert_eq!(old.access, "fixture-access-old");
        assert_eq!(old.refresh.as_deref(), Some("fixture-refresh-old"));
        assert!(old.refresh_attempted);
    }
    #[tokio::test]
    async fn refresh_rotates_atomically_and_rechecks_owner() {
        let f = fixture().await;
        login(&f).await;
        let result = f.auth.refresh(Region::Global, "main").await.unwrap();
        assert!(result.access_expires_at_ms.unwrap() > now_ms());
        assert!(!result.refresh_attempted);
        let raw = f
            .auth
            .read(&key(Region::Global, "session", "main"))
            .await
            .unwrap()
            .unwrap();
        let new: Credential = decode(&raw).unwrap();
        assert_eq!(new.access, "fixture-access-new");
        assert_eq!(new.refresh.as_deref(), Some("fixture-refresh-new"));
        assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn wrong_region_session_or_poll_is_rejected_without_a_vendor_call() {
        let f = fixture().await;
        let start = f.auth.start(Region::Global, "main").await.unwrap();
        assert!(f.auth.poll(Region::Cn, &start.login_id).await.is_err());
        assert_eq!(f.mock.polls.load(Ordering::SeqCst), 0);
        assert!(f.auth.start(Region::Global, "bad/../name").await.is_err());
    }
    #[test]
    fn unknown_numeric_absolute_expiry_is_not_guessed_as_seconds_or_milliseconds() {
        assert_eq!(
            expiry(
                &json!({"expires_at":1790000000}),
                "expires_at",
                "expires_in"
            )
            .unwrap(),
            None
        );
        assert_eq!(
            expiry(&json!({}), "expires_at", "expires_in").unwrap(),
            None
        );
        assert!(expiry(&json!({"expires_in":-1}), "expires_at", "expires_in").is_err());
    }

    #[test]
    fn known_device_millisecond_expiries_are_preserved_without_unit_guessing() {
        let deadline = now_ms() + 60_000;
        assert_eq!(
            expiry(&json!({"expire_time":deadline}), "expires_at", "expires_in").unwrap(),
            Some(deadline)
        );
        assert_eq!(
            expiry(
                &json!({"refresh_token_expire_time":deadline}),
                "refresh_token_expires_at",
                "refresh_token_expires_in"
            )
            .unwrap(),
            Some(deadline)
        );
        assert!(expiry(&json!({"expire_time":"bad"}), "expires_at", "expires_in").is_err());
        assert!(expiry(&json!({"expire_time":-1}), "expires_at", "expires_in").is_err());
    }

    #[tokio::test]
    async fn expired_device_millisecond_login_is_rejected_before_profile_request() {
        let f = fixture().await;
        f.mock.poll.lock().unwrap()["expire_time"] = json!(now_ms() - 1);
        let start = f.auth.start(Region::Global, "main").await.unwrap();
        assert!(f.auth.poll(Region::Global, &start.login_id).await.is_err());
        assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 0);
        assert_eq!(
            f.auth
                .session_status(Region::Global, "main")
                .await
                .unwrap()
                .status,
            "not_logged_in"
        );
    }

    #[tokio::test]
    async fn pending_limit_is_shared_across_regions_and_expired_records_are_reclaimed() {
        let f = fixture().await;
        let mut starts = Vec::new();
        for index in 0..32 {
            starts.push(
                f.auth
                    .start(
                        if index % 2 == 0 {
                            Region::Global
                        } else {
                            Region::Cn
                        },
                        "main",
                    )
                    .await
                    .unwrap(),
            );
        }
        assert!(f.auth.start(Region::Cn, "overflow").await.is_err());
        let name = key(starts[0].region, "pending", &starts[0].login_id);
        let raw = f.auth.read(&name).await.unwrap().unwrap();
        let mut expired: Pending = decode(&raw).unwrap();
        expired.deadline_ms = now_ms() - 1;
        f.auth
            .replace(&name, &raw, &encode(&expired).unwrap())
            .await
            .unwrap();
        f.auth.start(Region::Cn, "reclaimed").await.unwrap();
        assert!(f.auth.read(&name).await.unwrap().is_none());
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM app_secrets WHERE name LIKE 'account-login:qoder:%:pending:%'",
        )
        .fetch_one(&f.auth.pool)
        .await
        .unwrap();
        assert_eq!(count, 32);
        assert_eq!(f.mock.polls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn late_login_cannot_overwrite_an_account_created_after_start() {
        let f = fixture().await;
        let stale = f.auth.start(Region::Global, "main").await.unwrap();
        login(&f).await;
        let name = key(Region::Global, "session", "main");
        let before = f.auth.read(&name).await.unwrap().unwrap();
        assert!(f.auth.poll(Region::Global, &stale.login_id).await.is_err());
        assert_eq!(f.auth.read(&name).await.unwrap().unwrap(), before);
    }

    #[tokio::test]
    async fn conflicting_tokens_and_invalid_identity_are_rejected_before_profile_request() {
        let f = fixture().await;
        for body in [
            json!({"token":"fixture-access-old","device_token":"conflicting-access"}),
            json!({"token":"fixture-access-old","user_id":123}),
        ] {
            *f.mock.poll.lock().unwrap() = body;
            let start = f.auth.start(Region::Cn, "main").await.unwrap();
            assert!(f.auth.poll(Region::Cn, &start.login_id).await.is_err());
            assert!(f.auth.poll(Region::Cn, &start.login_id).await.is_err());
            f.auth.cancel(Region::Cn, &start.login_id).await.unwrap();
        }
        assert_eq!(f.mock.polls.load(Ordering::SeqCst), 2);
        assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 0);
        assert_eq!(
            f.auth
                .session_status(Region::Cn, "main")
                .await
                .unwrap()
                .status,
            "not_logged_in"
        );
    }

    #[tokio::test]
    async fn refresh_rejects_changed_principal_organization_and_token_identity() {
        for field in ["uid", "organization_id", "user_id"] {
            let f = fixture().await;
            login(&f).await;
            if field == "user_id" {
                f.mock.refresh.lock().unwrap()[field] = json!("different-account");
            } else {
                f.mock.profile.lock().unwrap()[field] = json!("different-account");
            }
            assert!(f.auth.refresh(Region::Global, "main").await.is_err());
            let old: Credential = decode(
                &f.auth
                    .read(&key(Region::Global, "session", "main"))
                    .await
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(old.access, "fixture-access-old");
            assert_eq!(old.refresh.as_deref(), Some("fixture-refresh-old"));
            assert!(old.refresh_attempted);
            assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn concurrent_services_do_not_replay_a_failed_refresh() {
        let f = fixture().await;
        login(&f).await;
        f.mock.refresh_code.store(502, Ordering::SeqCst);
        let mut independent = QoderAuth::new(f.auth.pool.clone(), None).unwrap();
        independent.test_origin = f.auth.test_origin.clone();
        let (first, second) = tokio::join!(
            f.auth.refresh(Region::Global, "main"),
            independent.refresh(Region::Global, "main")
        );
        assert!(first.is_err() && second.is_err());
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn expired_refresh_credential_is_rejected_without_an_http_request() {
        let f = fixture().await;
        login(&f).await;
        let name = key(Region::Global, "session", "main");
        let raw = f.auth.read(&name).await.unwrap().unwrap();
        let mut old: Credential = decode(&raw).unwrap();
        old.refresh_expires_at_ms = Some(now_ms() - 1);
        f.auth
            .replace(&name, &raw, &encode(&old).unwrap())
            .await
            .unwrap();
        assert!(
            !f.auth
                .session_status(Region::Global, "main")
                .await
                .unwrap()
                .can_refresh
        );
        assert!(f.auth.refresh(Region::Global, "main").await.is_err());
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn retained_refresh_preserves_expiry_but_new_refresh_does_not_inherit_it() {
        let f = fixture().await;
        login(&f).await;
        let mut old: Credential = decode(
            &f.auth
                .read(&key(Region::Global, "session", "main"))
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        old.refresh_expires_at_ms = Some(now_ms() + 60_000);
        let retained = parse_credential(
            Region::Global,
            &old.installation_id,
            &json!({"token":"new-access"}),
            Some(&old),
        )
        .unwrap();
        assert_eq!(retained.refresh, old.refresh);
        assert_eq!(retained.refresh_expires_at_ms, old.refresh_expires_at_ms);
        let rotated = parse_credential(
            Region::Global,
            &old.installation_id,
            &json!({"token":"new-access", "refresh_token":"new-refresh"}),
            Some(&old),
        )
        .unwrap();
        assert_eq!(rotated.refresh_expires_at_ms, None);
    }

    #[tokio::test]
    async fn authorization_url_rejects_wrong_region_extra_fields_and_unsafe_origins() {
        let f = fixture().await;
        let start = f.auth.start(Region::Cn, "main").await.unwrap();
        assert!(valid_authorization_url(
            &start.authorization_url,
            Region::Cn
        ));
        assert!(!valid_authorization_url(
            &start.authorization_url,
            Region::Global
        ));
        for replacement in [
            "https://qoder.cn.evil.invalid",
            "http://qoder.cn",
            "https://user@qoder.cn",
            "https://qoder.cn:8443",
        ] {
            assert!(!valid_authorization_url(
                &start
                    .authorization_url
                    .replace("https://qoder.cn", replacement),
                Region::Cn
            ));
        }
        assert!(!valid_authorization_url(
            &format!(
                "{}&redirect_uri=https://evil.invalid",
                start.authorization_url
            ),
            Region::Cn
        ));
        assert!(!valid_authorization_url(
            &format!("{}#fragment", start.authorization_url),
            Region::Cn
        ));
    }

    #[derive(Clone)]
    struct SafeDiagnosticWriter(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for SafeDiagnosticWriter {
        fn write(&mut self, value: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(value);
            Ok(value.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SafeDiagnosticWriter {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
    fn diagnostic_capture() -> (tracing::Dispatch, Arc<std::sync::Mutex<Vec<u8>>>) {
        let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .without_time()
            .with_writer(SafeDiagnosticWriter(bytes.clone()))
            .finish();
        (tracing::Dispatch::new(subscriber), bytes)
    }
    fn captured_diagnostic(bytes: &Arc<std::sync::Mutex<Vec<u8>>>) -> String {
        String::from_utf8(bytes.lock().unwrap().clone()).unwrap()
    }
    fn assert_safe_diagnostic(text: &str, operation: &str, stage: &str, kind: &str) {
        assert_eq!(
            text.matches("Qoder authentication request failed").count(),
            1,
            "{text}"
        );
        for field in [
            format!("operation=\"{operation}\""),
            format!("stage=\"{stage}\""),
            format!("classification=\"{kind}\""),
            "remote_auth_outcome=\"unknown\"".into(),
            "request_elapsed_ms=".into(),
        ] {
            assert!(text.contains(&field), "missing fixed field {field}: {text}");
        }
        for private in [
            "DO_NOT_LOG",
            "nonce=",
            "verifier=",
            "Bearer",
            "http://",
            "https://",
        ] {
            assert!(
                !text.contains(private),
                "private input leaked into diagnostic"
            );
        }
    }
    async fn synthetic_response(f: &mut Fixture, response: Vec<u8>) -> tokio::task::JoinHandle<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        f.auth.test_origin =
            Some(Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = [0; 4096];
            let _ = stream.read(&mut headers).await.unwrap();
            // A body-limit test can legitimately close before this completes.
            let _ = stream.write_all(&response).await;
        })
    }
    #[tokio::test]
    async fn transport_diagnostic_send_connect_keeps_pending_unknown_and_does_not_retry() {
        use tracing::instrument::WithSubscriber;
        let mut f = fixture().await;
        let start = f.auth.start(Region::Global, "main").await.unwrap();
        f.server.abort();
        let _ = (&mut f.server).await;
        let (dispatch, bytes) = diagnostic_capture();
        let error = f
            .auth
            .poll(Region::Global, &start.login_id)
            .with_subscriber(dispatch.clone())
            .await
            .unwrap_err();
        assert_eq!(error, "Qoder认证请求失败或超时");
        let again = f
            .auth
            .poll(Region::Global, &start.login_id)
            .with_subscriber(dispatch)
            .await
            .unwrap_err();
        assert!(again.contains("已发起轮询且结果不明"));
        let text = captured_diagnostic(&bytes);
        assert_safe_diagnostic(&text, "device_poll", "send", "connect_or_tls");
        assert!(text.contains("http_status=None"));
    }
    #[tokio::test]
    async fn transport_diagnostic_userinfo_does_not_log_bearer_or_query() {
        use tracing::instrument::WithSubscriber;
        let mut f = fixture().await;
        f.server.abort();
        let _ = (&mut f.server).await;
        let (dispatch, bytes) = diagnostic_capture();
        let error = f
            .auth
            .request(
                Region::Global,
                Method::GET,
                "/api/v1/userinfo",
                &[("nonce", "DO_NOT_LOG_NONCE")],
                Some("DO_NOT_LOG_ACCESS"),
                None,
            )
            .with_subscriber(dispatch)
            .await
            .unwrap_err();
        assert_eq!(error, "Qoder认证请求失败或超时");
        assert_safe_diagnostic(
            &captured_diagnostic(&bytes),
            "userinfo",
            "send",
            "connect_or_tls",
        );
    }
    #[tokio::test]
    async fn transport_diagnostic_timeout_is_not_guessed_as_connect_failure() {
        use tracing::instrument::WithSubscriber;
        let mut f = fixture().await;
        f.auth.client = Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .timeout(Duration::from_millis(40))
            .build()
            .unwrap();
        f.mock.hold.store(true, Ordering::SeqCst);
        let start = f.auth.start(Region::Global, "main").await.unwrap();
        let (dispatch, bytes) = diagnostic_capture();
        let error = f
            .auth
            .poll(Region::Global, &start.login_id)
            .with_subscriber(dispatch)
            .await
            .unwrap_err();
        f.mock.release.notify_one();
        assert_eq!(error, "Qoder认证请求失败或超时");
        assert_safe_diagnostic(
            &captured_diagnostic(&bytes),
            "device_poll",
            "send",
            "timeout",
        );
        assert_eq!(f.mock.polls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn transport_diagnostic_response_read_retains_known_http_without_body() {
        use tracing::instrument::WithSubscriber;
        let mut f = fixture().await;
        let server = synthetic_response(&mut f, b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nDO_NOT_LOG_TRUNCATED_BODY".to_vec()).await;
        let (dispatch, bytes) = diagnostic_capture();
        let error = f
            .auth
            .request(
                Region::Global,
                Method::GET,
                "/api/v1/deviceToken/poll",
                &[],
                None,
                None,
            )
            .with_subscriber(dispatch)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(error, "Qoder认证响应读取失败");
        let text = captured_diagnostic(&bytes);
        assert_safe_diagnostic(&text, "device_poll", "response_read", "other");
        assert!(text.contains("http_status=Some(200)"));
    }
    #[tokio::test]
    async fn transport_diagnostic_json_failure_drops_raw_body_and_parser_error() {
        use tracing::instrument::WithSubscriber;
        let mut f = fixture().await;
        let body = b"DO_NOT_LOG_INVALID_JSON";
        let response = [
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes(),
            body.to_vec(),
        ]
        .concat();
        let server = synthetic_response(&mut f, response).await;
        let (dispatch, bytes) = diagnostic_capture();
        let error = f
            .auth
            .request(Region::Cn, Method::GET, "/api/v1/userinfo", &[], None, None)
            .with_subscriber(dispatch)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(error, "Qoder认证响应不是有效JSON");
        assert_safe_diagnostic(
            &captured_diagnostic(&bytes),
            "userinfo",
            "response_parse",
            "invalid_json",
        );
    }
    #[tokio::test]
    async fn transport_diagnostic_body_limit_remains_a_failure_with_safe_status() {
        use tracing::instrument::WithSubscriber;
        let mut f = fixture().await;
        let body = vec![b'x'; BODY_LIMIT + 1];
        let response = [
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes(),
            body,
        ]
        .concat();
        let server = synthetic_response(&mut f, response).await;
        let (dispatch, bytes) = diagnostic_capture();
        let error = f
            .auth
            .request(Region::Cn, Method::GET, "/api/v1/userinfo", &[], None, None)
            .with_subscriber(dispatch)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(error, "Qoder认证响应过大");
        assert_safe_diagnostic(
            &captured_diagnostic(&bytes),
            "userinfo",
            "response_read",
            "body_limit",
        );
    }
    #[tokio::test]
    async fn transport_diagnostic_normal_pending_emits_no_failure() {
        use tracing::instrument::WithSubscriber;
        let f = fixture().await;
        f.mock.poll_code.store(202, Ordering::SeqCst);
        let start = f.auth.start(Region::Cn, "main").await.unwrap();
        let (dispatch, bytes) = diagnostic_capture();
        let status = f
            .auth
            .poll(Region::Cn, &start.login_id)
            .with_subscriber(dispatch)
            .await
            .unwrap();
        assert_eq!(status.status, "awaiting_authorization");
        assert_eq!(f.mock.polls.load(Ordering::SeqCst), 1);
        assert!(captured_diagnostic(&bytes).is_empty());
    }
}
