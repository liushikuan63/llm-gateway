//! Independent Trae CN authentication. No client processes or credential files.
//! Device-key registration needs explicit consent; imported legacy tokens are not
//! refreshable with a newly generated device key. Private types never enter IPC.
mod contract;
mod http;
#[cfg(test)]
mod tests;
use crate::{
    client_session::{AccountSnapshot, SessionCredential},
    crypto,
    db::repo,
};
use contract::{Device, Material, Pair};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};
use tokio::sync::{watch, Mutex};

const PREFIX: &str = "account-login:trae:cn:";
const WAIT_MS: i64 = 300_000;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    issuer: String,
    client_id: String,
    uid: String,
    pair: Pair,
    device: Device,
    private_key: String,
    public_key: String,
    snapshot: AccountSnapshot,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    account: String,
    id: String,
    deadline: i64,
    baseline: Option<String>,
    #[serde(default)]
    expected_identity_hash: Option<String>,
    state: String,
    device: Device,
    material: Material,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshAttempt {
    generation: String,
    id: String,
    state: String,
}
struct Core {
    pool: SqlitePool,
    http: parking_lot::RwLock<Result<reqwest::Client, String>>,
    gate: Mutex<()>,
    origin: Url,
    #[cfg(test)]
    preparation_pause:
        parking_lot::Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}
struct Listener {
    abort: tokio::task::AbortHandle,
    shutdown: watch::Sender<bool>,
}
#[derive(Default)]
struct Listeners(parking_lot::Mutex<HashMap<String, Listener>>);
impl Drop for Listeners {
    fn drop(&mut self) {
        for listener in self.0.get_mut().values() {
            let _ = listener.shutdown.send(true);
            listener.abort.abort();
        }
    }
}
#[derive(Clone)]
pub struct TraeAuth {
    core: Arc<Core>,
    listeners: Arc<Listeners>,
}
#[derive(Debug, Serialize)]
pub struct LoginStart {
    pub login_id: String,
    pub authorization_url: String,
    pub deadline_ms: i64,
}
#[derive(Debug, Serialize)]
pub struct LoginStatus {
    pub status: String,
    pub account_id: String,
    pub identity: Option<AccountSnapshot>,
    pub can_refresh: bool,
    pub access_expires_at_ms: Option<i64>,
    pub refresh_expires_at_ms: Option<i64>,
    pub refresh_attempted: bool,
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn id_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"-_".contains(&v))
}
fn key(kind: &str, id: &str) -> String {
    format!("{PREFIX}{kind}:{id}")
}
fn encode<T: Serialize>(value: &T) -> Result<String, String> {
    crypto::encrypt(&serde_json::to_string(value).map_err(|_| "Trae认证资料编码失败")?)
        .map_err(|_| "Trae认证资料加密失败".into())
}
fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, String> {
    let text = crypto::decrypt(raw).map_err(|_| "Trae认证资料解密失败")?;
    if text.len() > 65536 {
        return Err("Trae认证资料过大".into());
    }
    serde_json::from_str(&text).map_err(|_| "Trae认证资料无效".into())
}
fn generation(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}
async fn invalidate_refresh(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    account: &str,
) -> Result<(), String> {
    let time = chrono::Utc::now();
    let epoch = encode(&uuid::Uuid::new_v4().to_string())?;
    sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET value_enc=excluded.value_enc,updated_at=excluded.updated_at")
        .bind(key("refresh-cancel", account)).bind(epoch).bind(time).bind(time).execute(&mut **tx).await.map_err(|_| "无法保存Trae刷新取消标记")?;
    let raw: Option<String> = sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
        .bind(key("refresh", account))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| "无法读取Trae刷新预约")?;
    if let Some(raw) = raw {
        let mut reservation: RefreshAttempt = decode(&raw)?;
        reservation.state = "cancelled".into();
        sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=?")
            .bind(encode(&reservation)?)
            .bind(time)
            .bind(key("refresh", account))
            .execute(&mut **tx)
            .await
            .map_err(|_| "无法取消Trae刷新预约")?;
    }
    Ok(())
}
impl Credential {
    fn validate(&self) -> Result<(), String> {
        if self.issuer != contract::ISSUER
            || self.client_id != contract::CLIENT_ID
            || self.uid.is_empty()
            || !contract::valid_token(&self.pair.access)
            || self.private_key.is_empty()
            || self.public_key.is_empty()
        {
            return Err("Trae独立会话无效；旧导入令牌不能用新密钥刷新".into());
        }
        self.device.validate()
    }
}
impl TraeAuth {
    pub fn new(pool: SqlitePool, proxy: Option<&str>) -> Result<Self, String> {
        Ok(Self {
            core: Arc::new(Core {
                pool,
                http: parking_lot::RwLock::new(Ok(http::client(proxy)?)),
                gate: Mutex::new(()),
                origin: Url::parse(contract::ISSUER).map_err(|_| "Trae认证主机无效")?,
                #[cfg(test)]
                preparation_pause: parking_lot::Mutex::new(None),
            }),
            listeners: Arc::new(Listeners::default()),
        })
    }
    pub fn set_proxy(&self, proxy: Option<&str>) {
        *self.core.http.write() = http::client(proxy);
    }
    /// The caller obtains this context from this application's encrypted benefits
    /// store, never from an IPC-provided arbitrary device object or a client file.
    pub async fn start(
        &self,
        account: &str,
        session: &SessionCredential,
        consent_to_register_key: bool,
    ) -> Result<LoginStart, String> {
        if !id_valid(account) {
            return Err("Trae本机账号名称无效".into());
        }
        if !consent_to_register_key {
            return Err(
                "Trae首次独立登录需要明确同意设备公钥登记；可能影响原设备绑定，尚未发起登录".into(),
            );
        }
        session.validate(crate::benefits::ClaimPlatform::TraeCn)?;
        if !session.has_trae_device_context() || session.request_source != Some(1) {
            return Err("Trae独立登录需要已保存的真实普通IDE设备上下文；未启动客户端".into());
        }
        let device = Device {
            device_id: session.headers["x-device-id"].clone(),
            machine_id: None,
            device_name: String::new(),
            device_model: session
                .headers
                .get("x-device-brand")
                .cloned()
                .unwrap_or_default(),
            device_brand: String::new(),
            device_cpu: String::new(),
            os_info: session
                .headers
                .get("x-device-type")
                .cloned()
                .unwrap_or_default(),
            os_version: session
                .headers
                .get("x-os-version")
                .cloned()
                .unwrap_or_default(),
        };
        self.start_device(account, device, session.snapshot.identity_hash.clone())
            .await
    }
    async fn start_device(
        &self,
        account: &str,
        device: Device,
        expected_identity_hash: Option<String>,
    ) -> Result<LoginStart, String> {
        device.validate()?;
        if expected_identity_hash
            .as_ref()
            .is_some_and(|value| value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("所选Trae账号的身份指纹无效，请在权益中心重新核验".into());
        }
        let expected_identity_hash = expected_identity_hash.map(|value| value.to_ascii_lowercase());
        let material: Material = serde_json::from_value(
            crate::qoder_auth::invoke_trae(&json!({"operation":"login_material"})).await?,
        )
        .map_err(|_| "Trae私有签名组件格式无效")?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "Trae本机授权回调无法启动")?;
        let port = listener
            .local_addr()
            .map_err(|_| "Trae授权回调地址无效")?
            .port();
        let id = uuid::Uuid::new_v4().to_string();
        let deadline = now() + WAIT_MS;
        let _guard = self.core.gate.lock().await;
        let mut tx = self
            .core
            .pool
            .begin()
            .await
            .map_err(|_| "无法预约Trae登录")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(key("session", account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法核验Trae登录账号")?;
        let baseline: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(key("session", account))
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法读取Trae登录账号")?;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name LIKE ?")
                .bind(format!("{PREFIX}pending:%"))
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| "无法核验Trae登录等待")?;
        let mut count = 0;
        let mut superseded = Vec::new();
        for (name, raw) in rows {
            let previous: Pending = decode(&raw)?;
            if previous.account == account
                || previous.deadline <= now()
                || !["waiting", "exchanging"].contains(&previous.state.as_str())
            {
                sqlx::query("DELETE FROM app_secrets WHERE name=?")
                    .bind(name)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| "无法结束旧Trae登录等待")?;
                superseded.push(previous.id);
            } else {
                count += 1;
            }
        }
        if count >= 32 {
            return Err("Trae登录等待过多，请先取消已结束的等待".into());
        }
        if deadline <= now() {
            return Err("Trae登录等待已过期，未创建回调".into());
        }
        // A new login owns the account even while an older refresh is preparing
        // locally. Invalidate both pre-send work and any outstanding HTTP result.
        invalidate_refresh(&mut tx, account).await?;
        let pending = Pending {
            account: account.into(),
            id: id.clone(),
            deadline,
            baseline,
            expected_identity_hash,
            state: "waiting".into(),
            device: device.clone(),
            material: material.clone(),
        };
        sqlx::query(
            "INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?)",
        )
        .bind(key("pending", &id))
        .bind(encode(&pending)?)
        .bind(chrono::Utc::now())
        .bind(chrono::Utc::now())
        .execute(&mut *tx)
        .await
        .map_err(|_| "无法保存Trae登录等待")?;
        tx.commit().await.map_err(|_| "无法保存Trae登录等待")?;
        for old_id in superseded {
            if let Some(old) = self.listeners.0.lock().remove(&old_id) {
                let _ = old.shutdown.send(true);
                old.abort.abort();
            }
        }
        let (shutdown, mut stop) = watch::channel(false);
        let ctx = Callback {
            core: Arc::downgrade(&self.core),
            id: id.clone(),
            port,
            shutdown: shutdown.clone(),
        };
        let router = axum::Router::new()
            .route("/authorize/:nonce", axum::routing::get(callback))
            .with_state(ctx);
        let registry = Arc::downgrade(&self.listeners);
        let done_id = id.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let wait = async {
                        while !*stop.borrow() {
                            if stop.changed().await.is_err() {
                                break;
                            }
                        }
                    };
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(WAIT_MS as u64),
                        wait,
                    )
                    .await;
                })
                .await;
            if let Some(registry) = registry.upgrade() {
                registry.0.lock().remove(&done_id);
            }
        });
        self.listeners.0.lock().insert(
            id.clone(),
            Listener {
                abort: task.abort_handle(),
                shutdown,
            },
        );
        let mut url =
            Url::parse("https://www.trae.cn/authorization").map_err(|_| "Trae官方授权地址无效")?;
        url.query_pairs_mut().extend_pairs([
            ("login_version", "1"),
            ("auth_from", "trae"),
            ("login_channel", "native_ide"),
            ("auth_type", "local"),
            ("client_id", contract::CLIENT_ID),
            ("redirect", "0"),
            ("plugin_version", contract::VERSION),
            ("login_trace_id", &id),
            (
                "auth_callback_url",
                &format!("http://127.0.0.1:{port}/authorize/{id}"),
            ),
            ("device_id", &device.device_id),
            ("x_device_id", &device.device_id),
            ("x_app_version", contract::VERSION),
            ("x_app_type", "stable"),
            ("code_challenge", &material.code_challenge),
            ("code_challenge_method", "S256"),
        ]);
        Ok(LoginStart {
            login_id: id,
            authorization_url: url.to_string(),
            deadline_ms: deadline,
        })
    }
    pub async fn status(&self, account: &str) -> Result<LoginStatus, String> {
        self.core.status(account).await
    }
    pub async fn poll(&self, account: &str, login_id: &str) -> Result<LoginStatus, String> {
        let raw = self
            .core
            .read(&key("pending", login_id))
            .await?
            .ok_or("Trae登录等待不存在或已结束")?;
        let mut pending: Pending = decode(&raw)?;
        if pending.account != account || pending.id != login_id {
            return Err("Trae登录等待不属于当前账号".into());
        }
        if ["waiting", "exchanging"].contains(&pending.state.as_str()) && pending.deadline <= now()
        {
            pending.state = self.finish_wait(account, login_id, "expired").await?;
        }
        let mut status = self.status(account).await?;
        match pending.state.as_str() {
            "waiting" | "exchanging" => status.status = "awaiting_authorization".into(),
            "failed_no_retry" => status.status = "authorization_failed_no_retry".into(),
            "cancelled" => status.status = "cancelled".into(),
            "expired" => status.status = "expired".into(),
            _ => {}
        }
        Ok(status)
    }
    pub async fn cancel(&self, account: &str, login_id: &str) -> Result<(), String> {
        self.finish_wait(account, login_id, "cancelled").await?;
        Ok(())
    }
    async fn finish_wait(
        &self,
        account: &str,
        login_id: &str,
        reason: &str,
    ) -> Result<String, String> {
        let _guard = self.core.gate.lock().await;
        let name = key("pending", login_id);
        let mut tx = self
            .core
            .pool
            .begin()
            .await
            .map_err(|_| "无法结束Trae登录等待")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(&name)
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae登录等待")?;
        let raw: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(&name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法读取Trae登录等待")?;
        let Some(raw) = raw else {
            return Ok(reason.into());
        };
        let mut pending: Pending = decode(&raw)?;
        if pending.account != account || pending.id != login_id {
            return Err("Trae登录等待不属于当前账号".into());
        }
        if !["waiting", "exchanging"].contains(&pending.state.as_str()) {
            return Ok(pending.state);
        }
        // A callback may have committed while poll waited for the database writer.
        // Preserve that terminal result instead of replacing it with a timeout.
        pending.state = reason.into();
        sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=?")
            .bind(encode(&pending)?)
            .bind(chrono::Utc::now())
            .bind(&name)
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法结束Trae登录等待")?;
        tx.commit().await.map_err(|_| "无法保存Trae等待终态")?;
        if let Some(listener) = self.listeners.0.lock().remove(login_id) {
            let _ = listener.shutdown.send(true);
            listener.abort.abort();
        }
        Ok(pending.state)
    }
    pub async fn cancel_refresh(&self, account: &str) -> Result<(), String> {
        if !id_valid(account) {
            return Err("Trae本机账号名称无效".into());
        }
        let _guard = self.core.gate.lock().await;
        let mut tx = self
            .core
            .pool
            .begin()
            .await
            .map_err(|_| "无法取消Trae刷新")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(key("session", account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae刷新取消")?;
        invalidate_refresh(&mut tx, account).await?;
        tx.commit().await.map_err(|_| "无法保存Trae刷新取消".into())
    }
    pub async fn refresh(&self, account: &str) -> Result<LoginStatus, String> {
        self.core.refresh(account).await
    }
}
#[derive(Clone)]
struct Callback {
    core: Weak<Core>,
    id: String,
    port: u16,
    shutdown: watch::Sender<bool>,
}
async fn callback(
    axum::extract::State(ctx): axum::extract::State<Callback>,
    axum::extract::Path(nonce): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> impl axum::response::IntoResponse {
    let valid = nonce == ctx.id
        && headers.get("host").and_then(|v| v.to_str().ok())
            == Some(format!("127.0.0.1:{}", ctx.port).as_str());
    let result = if valid {
        match ctx.core.upgrade() {
            Some(core) => core.complete(&ctx.id, query.as_deref().unwrap_or("")).await,
            None => Err("Trae登录等待已结束".into()),
        }
    } else {
        Err("Trae登录回调无效".into())
    };
    // Invalid query fields do not consume the one-time exchange. Keep the actual
    // listener available for the subsequent correctly bound official callback.
    let consumed = if valid {
        match ctx.core.upgrade() {
            Some(core) => core
                .read(&key("pending", &ctx.id))
                .await
                .and_then(|raw| raw.map(|v| decode::<Pending>(&v)).transpose())
                .map(|p| p.map_or(true, |p| p.state != "waiting" || p.deadline <= now()))
                .unwrap_or(false),
            None => true,
        }
    } else {
        false
    };
    if consumed {
        let _ = ctx.shutdown.send(true);
    }
    (
        if result.is_ok() {
            axum::http::StatusCode::OK
        } else {
            axum::http::StatusCode::BAD_REQUEST
        },
        [
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
            ("content-type", "text/plain; charset=utf-8"),
        ],
        if result.is_ok() {
            "登录已核验，请返回软件查看。"
        } else {
            "本次登录未完成，请返回软件查看；不会自动重试。"
        },
    )
}

impl Core {
    async fn read(&self, name: &str) -> Result<Option<String>, String> {
        repo::get_secret(&self.pool, name)
            .await
            .map_err(|_| "无法读取Trae认证资料".into())
    }
    async fn replace(&self, name: &str, expected: &str, value: &str) -> Result<(), String> {
        let result = sqlx::query(
            "UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=? AND value_enc=?",
        )
        .bind(value)
        .bind(chrono::Utc::now())
        .bind(name)
        .bind(expected)
        .execute(&self.pool)
        .await
        .map_err(|_| "无法更新Trae认证资料")?;
        if result.rows_affected() != 1 {
            return Err("Trae认证已改变，未覆盖旧结果".into());
        }
        Ok(())
    }
    async fn status(&self, account: &str) -> Result<LoginStatus, String> {
        if !id_valid(account) {
            return Err("Trae本机账号名称无效".into());
        }
        let raw = self.read(&key("session", account)).await?;
        let Some(raw) = raw else {
            return Ok(LoginStatus {
                status: "not_logged_in".into(),
                account_id: account.into(),
                identity: None,
                can_refresh: false,
                access_expires_at_ms: None,
                refresh_expires_at_ms: None,
                refresh_attempted: false,
            });
        };
        let credential: Credential = decode(&raw)?;
        credential.validate()?;
        let attempted = self
            .read(&key("refresh", account))
            .await?
            .map(|v| decode::<RefreshAttempt>(&v))
            .transpose()?
            .is_some_and(|v| v.generation == generation(&raw));
        Ok(LoginStatus {
            status: if credential
                .pair
                .access_expires_at_ms
                .is_some_and(|v| v <= now())
            {
                "access_expired"
            } else {
                "authenticated"
            }
            .into(),
            account_id: account.into(),
            identity: Some(credential.snapshot),
            can_refresh: credential.pair.refresh.is_some()
                && !credential
                    .pair
                    .refresh_expires_at_ms
                    .is_some_and(|v| v <= now())
                && !attempted,
            access_expires_at_ms: credential.pair.access_expires_at_ms,
            refresh_expires_at_ms: credential.pair.refresh_expires_at_ms,
            refresh_attempted: attempted,
        })
    }
    async fn pending_current(&self, pending: &Pending, expected: &str) -> Result<(), String> {
        if pending.deadline <= now() {
            return Err("Trae登录等待已过期".into());
        }
        let names = [
            key("pending", &pending.id),
            key("session", &pending.account),
        ];
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name IN (?,?)")
                .bind(&names[0])
                .bind(&names[1])
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "无法核验Trae登录预约")?;
        let p = rows.iter().find(|v| v.0 == names[0]).map(|v| v.1.as_str());
        let session = rows.iter().find(|v| v.0 == names[1]).map(|v| v.1.as_str());
        if p != Some(expected)
            || session != pending.baseline.as_deref()
            || pending.deadline <= now()
        {
            return Err("Trae登录已取消、过期或账号已改变".into());
        }
        Ok(())
    }
    async fn reserve_pending(&self, id: &str) -> Result<(Pending, String), String> {
        let _guard = self.gate.lock().await;
        let name = key("pending", id);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "无法预约Trae授权交换")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(&name)
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae授权交换")?;
        let raw: String = sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
            .bind(&name)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| "无法读取Trae授权等待")?
            .ok_or("Trae授权等待不存在")?;
        let mut pending: Pending = decode(&raw)?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(key("session", &pending.account))
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法核验Trae账号")?;
        if pending.id != id
            || pending.state != "waiting"
            || pending.deadline <= now()
            || current != pending.baseline
        {
            return Err("Trae授权已使用、过期或账号已改变；未重放".into());
        }
        pending.state = "exchanging".into();
        let expected = encode(&pending)?;
        sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=?")
            .bind(&expected)
            .bind(chrono::Utc::now())
            .bind(&name)
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法保存Trae单次预约")?;
        tx.commit().await.map_err(|_| "无法提交Trae单次预约")?;
        Ok((pending, expected))
    }
    async fn complete(&self, id: &str, query: &str) -> Result<(), String> {
        let code = callback_code(id, query)?;
        let (pending, expected) = self.reserve_pending(id).await?;
        let result = self.exchange_pending(&pending, &expected, &code).await;
        if result.is_err() {
            let mut failed = pending.clone();
            failed.state = "failed_no_retry".into();
            let _ = self
                .replace(&key("pending", id), &expected, &encode(&failed)?)
                .await;
        }
        result
    }
    async fn exchange_pending(
        &self,
        pending: &Pending,
        expected: &str,
        code: &str,
    ) -> Result<(), String> {
        self.pending_current(pending, expected).await?;
        let http = self.http.read().clone()?;
        let packet = http::post(
            &http,
            &self.origin,
            contract::EXCHANGE,
            "",
            &contract::auth_code_body(code, &pending.material, &pending.device)?,
        )
        .await?;
        self.pending_current(pending, expected).await?;
        let pair = contract::parse_pair(&packet, None, now())?;
        if pair.access_expires_at_ms.is_some_and(|v| v <= now()) {
            return Err("Trae返回已过期令牌，未保存".into());
        }
        let mut secrets = vec![
            pair.access.as_str(),
            pending.material.private_key.as_str(),
            pending.material.code_verifier.as_str(),
            code,
        ];
        secrets.extend(pair.refresh.as_deref());
        let identity = http::principal(&http, &self.origin, &pair, &secrets).await?;
        self.pending_current(pending, expected).await?;
        if pending
            .expected_identity_hash
            .as_ref()
            .is_some_and(|expected| identity.snapshot.identity_hash.as_ref() != Some(expected))
        {
            return Err(
                "Trae登录主体与所选权益账号不一致，原资料保留；请在官方网页选择正确账号".into(),
            );
        }
        if let Some(old) = &pending.baseline {
            let old: Credential = decode(old)?;
            if old.uid != identity.uid {
                return Err("Trae登录主体已改变，请为另一账号使用不同本机名称".into());
            }
        }
        let credential = Credential {
            issuer: contract::ISSUER.into(),
            client_id: contract::CLIENT_ID.into(),
            uid: identity.uid,
            pair,
            device: pending.device.clone(),
            private_key: pending.material.private_key.clone(),
            public_key: pending.material.public_key.clone(),
            snapshot: identity.snapshot,
        };
        credential.validate()?;
        let _guard = self.gate.lock().await;
        let mut tx = self.pool.begin().await.map_err(|_| "无法保存Trae登录")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(key("pending", &pending.id))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae登录保存")?;
        for (name, wanted) in [
            (key("pending", &pending.id), Some(expected)),
            (
                key("session", &pending.account),
                pending.baseline.as_deref(),
            ),
        ] {
            let current: Option<String> =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| "无法核验Trae登录保存")?;
            if current.as_deref() != wanted {
                return Err("Trae登录已取消或改变，未保存迟到结果".into());
            }
        }
        if pending.deadline <= now()
            || credential
                .pair
                .access_expires_at_ms
                .is_some_and(|v| v <= now())
        {
            return Err("Trae登录或令牌已过期，未保存".into());
        }
        let now = chrono::Utc::now();
        sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET value_enc=excluded.value_enc,updated_at=excluded.updated_at").bind(key("session",&pending.account)).bind(encode(&credential)?).bind(now).bind(now).execute(&mut *tx).await.map_err(|_|"无法保存Trae独立会话")?;
        let mut used = pending.clone();
        used.state = "authenticated".into();
        sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=?")
            .bind(encode(&used)?)
            .bind(now)
            .bind(key("pending", &pending.id))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法完成Trae授权等待")?;
        tx.commit().await.map_err(|_| "无法完成Trae登录事务")?;
        Ok(())
    }
    async fn refresh_current(&self, account: &str, old: &str, attempt: &str) -> Result<(), String> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name IN (?,?)")
                .bind(key("session", account))
                .bind(key("refresh", account))
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "无法核验Trae刷新预约")?;
        if rows
            .iter()
            .find(|v| v.0 == key("session", account))
            .map(|v| v.1.as_str())
            != Some(old)
            || rows
                .iter()
                .find(|v| v.0 == key("refresh", account))
                .map(|v| v.1.as_str())
                != Some(attempt)
        {
            return Err("Trae刷新已取消或账号已改变，未覆盖旧结果".into());
        }
        let credential: Credential = decode(old)?;
        if credential
            .pair
            .refresh_expires_at_ms
            .is_some_and(|v| v <= now())
        {
            return Err("Trae刷新凭据已到期，未重放".into());
        }
        Ok(())
    }
    async fn refresh(&self, account: &str) -> Result<LoginStatus, String> {
        if !id_valid(account) {
            return Err("Trae本机账号名称无效".into());
        }
        // Prepare locally before the durable reservation. No HTTP is possible in
        // this phase, so a broken proxy or signer must not consume a refresh token.
        let cancel_epoch = self.read(&key("refresh-cancel", account)).await?;
        let old = self
            .read(&key("session", account))
            .await?
            .ok_or("请先完成Trae独立网页登录")?;
        let credential: Credential = decode(&old)?;
        credential.validate()?;
        if credential.pair.refresh.is_none()
            || credential
                .pair
                .refresh_expires_at_ms
                .is_some_and(|v| v <= now())
        {
            return Err("Trae刷新凭据缺失或过期，请重新网页登录".into());
        }
        let http = self.http.read().clone()?;
        let device = credential.device.wire(&credential.public_key)?;
        #[cfg(test)]
        {
            let pause = self.preparation_pause.lock().clone();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        let proof=crate::qoder_auth::invoke_trae(&json!({"operation":"refresh_proof","input":{"client_id":contract::CLIENT_ID,"refresh_token":credential.pair.refresh,"private_key":credential.private_key}})).await?;
        let body = json!({"ClientID":contract::CLIENT_ID,"ClientSecret":"","RefreshToken":credential.pair.refresh,"DeviceInfo":device,"DeviceProof":proof,"IDEVersion":contract::VERSION});
        let attempt = {
            let _guard = self.gate.lock().await;
            let mut tx = self.pool.begin().await.map_err(|_| "无法预约Trae刷新")?;
            sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
                .bind(key("session", account))
                .execute(&mut *tx)
                .await
                .map_err(|_| "无法锁定Trae刷新")?;
            let current: String =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                    .bind(key("session", account))
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| "无法读取Trae独立会话")?
                    .ok_or("请先完成Trae独立网页登录")?;
            if current != old {
                return Err("Trae账号在本地准备时已改变，未发起刷新".into());
            }
            let current_epoch: Option<String> =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                    .bind(key("refresh-cancel", account))
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| "无法核验Trae刷新取消标记")?;
            if current_epoch != cancel_epoch {
                return Err("Trae刷新在本地准备时已取消，未发起请求".into());
            }
            let waits: Vec<String> =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name LIKE ?")
                    .bind(format!("{PREFIX}pending:%"))
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(|_| "无法核验Trae登录等待")?;
            for raw in waits {
                let pending: Pending = decode(&raw)?;
                if pending.account == account
                    && ["waiting", "exchanging"].contains(&pending.state.as_str())
                    && pending.deadline > now()
                {
                    return Err("当前Trae账号正在重新登录，请先完成或取消登录".into());
                }
            }
            if credential.pair.refresh.is_none()
                || credential
                    .pair
                    .refresh_expires_at_ms
                    .is_some_and(|v| v <= now())
            {
                return Err("Trae刷新凭据缺失或过期，请重新网页登录".into());
            }
            let previous: Option<String> =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                    .bind(key("refresh", account))
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| "无法核验Trae刷新预约")?;
            let gen = generation(&old);
            if previous
                .map(|v| decode::<RefreshAttempt>(&v))
                .transpose()?
                .is_some_and(|v| v.generation == gen)
            {
                return Err("该代Trae刷新已执行或结果不明；不会重放，请重新网页登录".into());
            }
            let reservation = encode(&RefreshAttempt {
                generation: gen,
                id: uuid::Uuid::new_v4().to_string(),
                state: "reserved".into(),
            })?;
            let time = chrono::Utc::now();
            sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET value_enc=excluded.value_enc,updated_at=excluded.updated_at").bind(key("refresh",account)).bind(&reservation).bind(time).bind(time).execute(&mut *tx).await.map_err(|_|"无法保存Trae刷新预约")?;
            tx.commit().await.map_err(|_| "无法提交Trae刷新预约")?;
            reservation
        };
        self.refresh_current(account, &old, &attempt).await?;
        let packet = http::post(
            &http,
            &self.origin,
            contract::EXCHANGE,
            &credential.pair.access,
            &body,
        )
        .await?;
        self.refresh_current(account, &old, &attempt).await?;
        let pair = contract::parse_pair(&packet, Some(&credential.pair), now())?;
        if pair.access_expires_at_ms.is_some_and(|v| v <= now()) {
            return Err("Trae刷新返回已过期令牌，原资料保留".into());
        }
        let mut secrets = vec![
            pair.access.as_str(),
            credential.pair.access.as_str(),
            credential.private_key.as_str(),
        ];
        secrets.extend(pair.refresh.as_deref());
        secrets.extend(credential.pair.refresh.as_deref());
        let identity = http::principal(&http, &self.origin, &pair, &secrets).await?;
        if identity.uid != credential.uid {
            return Err("Trae刷新主体改变，原资料保留".into());
        }
        self.refresh_current(account, &old, &attempt).await?;
        let new = Credential {
            pair,
            snapshot: identity.snapshot,
            ..credential
        };
        let _guard = self.gate.lock().await;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "无法保存Trae刷新结果")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(key("session", account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae刷新结果")?;
        for (name, wanted) in [
            (key("session", account), old.as_str()),
            (key("refresh", account), attempt.as_str()),
        ] {
            let current: Option<String> =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| "无法核验Trae刷新结果")?;
            if current.as_deref() != Some(wanted) {
                return Err("Trae账号或刷新预约已改变，未保存迟到结果".into());
            }
        }
        let original: Credential = decode(&old)?;
        if original
            .pair
            .refresh_expires_at_ms
            .is_some_and(|v| v <= now())
            || new.pair.access_expires_at_ms.is_some_and(|v| v <= now())
        {
            return Err("Trae刷新令牌已过期，未保存".into());
        }
        sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=?")
            .bind(encode(&new)?)
            .bind(chrono::Utc::now())
            .bind(key("session", account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法保存Trae刷新结果")?;
        tx.commit().await.map_err(|_| "无法提交Trae刷新结果")?;
        self.status(account).await
    }
}

fn callback_code(id: &str, query: &str) -> Result<String, String> {
    if query.len() > 32768 {
        return Err("Trae回调超过上限".into());
    }
    // Validate percent-decoded UTF8 before Url::query_pairs can replace bad bytes.
    let raw = query.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' {
            let a = raw.get(index + 1).and_then(|v| char::from(*v).to_digit(16));
            let b = raw.get(index + 2).and_then(|v| char::from(*v).to_digit(16));
            decoded
                .push(((a.ok_or("Trae回调编码无效")? << 4) | b.ok_or("Trae回调编码无效")?) as u8);
            index += 3;
        } else {
            decoded.push(if raw[index] == b'+' { b' ' } else { raw[index] });
            index += 1;
        }
    }
    std::str::from_utf8(&decoded).map_err(|_| "Trae回调UTF8无效")?;
    let url = Url::parse(&format!("http://127.0.0.1/?{query}")).map_err(|_| "Trae回调无效")?;
    let mut params = HashMap::new();
    for (name, value) in url.query_pairs() {
        if params
            .insert(name.into_owned(), value.into_owned())
            .is_some()
        {
            return Err("Trae回调参数重复".into());
        }
    }
    if params.get("loginTraceID").map(String::as_str) != Some(id)
        || params.get("scope").is_some_and(|v| v != "marscode")
        || params.get("host").is_some_and(|v| v != contract::ISSUER)
    {
        return Err("Trae回调区域、来源或绑定不一致".into());
    }
    let code: Value =
        serde_json::from_str(params.get("authCodeInfo").ok_or("Trae回调缺少PKCE授权码")?)
            .map_err(|_| "Trae回调授权码格式无效")?;
    code.get("AuthCode")
        .and_then(Value::as_str)
        .filter(|v| contract::valid_token(v))
        .map(str::to_owned)
        .ok_or_else(|| "Trae回调授权码无效".into())
}
