//! Explicit official legacy web authorization, separate from native-device proof.
//! No guessed hardware, native credential migration, SDK, or automatic retry.
use super::*;
use axum::{http::HeaderMap, response::IntoResponse, routing::get, Router};

mod model;
mod refresh;
pub(super) use model::WebModelSource;
#[cfg(test)]
mod tests;

const PREFIX_WEB: &str = "account-login:trae-web:";
pub(super) const EXCHANGE_WEB: &str = "/cloudide/api/v3/trae/oauth/ExchangeToken";

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebRegion {
    Cn,
    Global,
}

impl WebRegion {
    fn name(self) -> &'static str {
        match self {
            Self::Cn => "cn",
            Self::Global => "global",
        }
    }
    fn authorization_url(self) -> &'static str {
        match self {
            Self::Cn => "https://www.trae.cn/authorization",
            Self::Global => "https://www.trae.ai/authorization",
        }
    }
    fn principal_hash(self, uid: &str) -> String {
        format!(
            "{:x}",
            Sha256::digest(format!("trae_{}:{uid}", self.name()).as_bytes())
        )
    }
}

// Finite ordinary-web auth origins, never inferred from native/model domains.
// Official web assets consume CheckLogin's dynamic Host; the exact SG-central
// origin additionally has observed official-web callback and documented public
// integration evidence. This is bounded compatibility, not a claim that its
// JWT-only principal API has passed live acceptance. Unknown Hosts get no token.
#[derive(Clone, Copy)]
enum WebOrigin {
    Cn(CnOrigin),
    Global,
    GlobalUs,
    GlobalSgCentral,
}
impl WebOrigin {
    fn parse(region: WebRegion, issuer: &str) -> Result<Self, String> {
        match (region, issuer) {
            (WebRegion::Cn, value) => CnOrigin::from_issuer(value).map(Self::Cn),
            (WebRegion::Global, "https://ug-normal.trae.ai") => Ok(Self::Global),
            (WebRegion::Global, "https://ug-normal.us.trae.ai") => Ok(Self::GlobalUs),
            (WebRegion::Global, "https://api-sg-central.trae.ai") => Ok(Self::GlobalSgCentral),
            _ => Err("Trae网页认证主机与所选地区不匹配，未发送凭证".into()),
        }
    }
    fn issuer(self) -> &'static str {
        match self {
            Self::Cn(origin) => origin.issuer(),
            Self::Global => "https://ug-normal.trae.ai",
            Self::GlobalUs => "https://ug-normal.us.trae.ai",
            Self::GlobalSgCentral => "https://api-sg-central.trae.ai",
        }
    }
}
fn web_key(region: WebRegion, kind: &str, id: &str) -> String {
    format!("{PREFIX_WEB}{}:{kind}:{id}", region.name())
}

// Match the existing decoder's plaintext ceiling before any new web record is
// persisted. Native credential encoding and its serialized format are unchanged.
fn encode<T: Serialize>(value: &T) -> Result<String, String> {
    let text = serde_json::to_string(value).map_err(|_| "Trae网页资料编码失败")?;
    if text.len() > 65536 {
        return Err("Trae网页资料超过本地保存上限，未写入".into());
    }
    crypto::encrypt(&text).map_err(|_| "Trae网页资料加密失败".into())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebPending {
    id: String,
    account: String,
    region: WebRegion,
    deadline: i64,
    baseline: Option<String>,
    #[serde(default)]
    expected_uid: Option<String>,
    state: String,
    #[serde(default)]
    request_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebSession {
    region: WebRegion,
    issuer: String,
    client_id: String,
    uid: String,
    pair: Pair,
    snapshot: AccountSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    store_country: Option<String>,
}
impl WebSession {
    fn validate(&self) -> Result<(), String> {
        WebOrigin::parse(self.region, &self.issuer)?;
        if self.client_id != contract::CLIENT_ID
            || self.uid.is_empty()
            || self.snapshot.identity_hash.as_deref()
                != Some(self.region.principal_hash(&self.uid).as_str())
            || self
                .store_country
                .as_ref()
                .is_some_and(|v| v.len() != 2 || !v.bytes().all(|b| b.is_ascii_uppercase()))
            || !contract::valid_token(&self.pair.access)
            || self
                .pair
                .refresh
                .as_ref()
                .is_some_and(|v| !contract::valid_token(v))
        {
            return Err("Trae网页会话无效，未作为原生设备资料使用".into());
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub struct WebLoginPending {
    pub login_id: String,
    pub deadline_ms: i64,
    pub state: String,
    pub request_id: Option<String>,
}

#[derive(Serialize)]
pub struct WebLoginStatus {
    pub region: WebRegion,
    pub authentication_mode: &'static str,
    pub account_id: String,
    pub status: String,
    pub identity: Option<AccountSnapshot>,
    pub access_expires_at_ms: Option<i64>,
    pub refresh_expires_at_ms: Option<i64>,
    // A capability hint only; the explicit command revalidates the whole source.
    pub can_refresh: bool,
    pub native_device_verified: bool,
    pub model_inference_verified: bool,
    pub pending_login: Option<WebLoginPending>,
    // Observation from the same verified UserID response, not TNC/storeRegion,
    // model routing authority, residency, availability or fee/entitlement proof.
    pub store_country: Option<String>,
}

fn principal_country(packet: &Value) -> Option<String> {
    packet
        .get("Result")?
        .get("StoreCountry")?
        .as_str()
        .filter(|v| v.len() == 2 && v.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(str::to_ascii_uppercase)
}

impl TraeAuth {
    pub async fn web_start(
        &self,
        region: WebRegion,
        account: &str,
        consent: bool,
    ) -> Result<LoginStart, String> {
        self.web_start_bound(region, account, consent, None).await
    }
    pub async fn web_start_bound(
        &self,
        region: WebRegion,
        account: &str,
        consent: bool,
        request_id: Option<&str>,
    ) -> Result<LoginStart, String> {
        if request_id.is_some_and(|id| {
            !uuid::Uuid::parse_str(id)
                .is_ok_and(|value| value.get_version_num() == 4 && value.to_string() == id)
        }) {
            return Err("Trae网页登录请求标识无效，未发起授权".into());
        }
        if !id_valid(account) || !consent {
            return Err("Trae网页登录需要有效本机账号名称及明确授权，未发起登录".into());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "Trae网页回调无法启动")?;
        let port = listener
            .local_addr()
            .map_err(|_| "Trae网页回调地址无效")?
            .port();
        let id = uuid::Uuid::new_v4().to_string();
        let _guard = self.core.gate.lock().await;
        let mut tx = self
            .core
            .pool
            .begin()
            .await
            .map_err(|_| "无法预约Trae网页登录")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(web_key(region, "session", account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae网页账号")?;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name LIKE ? LIMIT 65")
                .bind(web_key(region, "pending", "%"))
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| "无法读取Trae网页预约")?;
        let mut active = 0;
        for (name, raw) in rows {
            let old: WebPending = decode(&raw)?;
            if request_id.is_some() && old.request_id.as_deref() == request_id {
                return Err("Trae网页登录请求标识已使用，未重复发起授权".into());
            }
            if old.deadline <= now() {
                sqlx::query("DELETE FROM app_secrets WHERE name=? AND value_enc=?")
                    .bind(name)
                    .bind(raw)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| "无法清理过期Trae网页预约")?;
            } else {
                active += 1;
                if old.account == account && ["waiting", "exchanging"].contains(&old.state.as_str())
                {
                    return Err("该Trae网页账号已有进行中的授权，请先完成或取消".into());
                }
            }
        }
        if active >= 64 {
            return Err("Trae网页授权预约已达上限，未覆盖现有等待".into());
        }
        let baseline: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(web_key(region, "session", account))
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法核验Trae网页账号")?;
        let expected_uid = if let Some(raw) = &baseline {
            let previous: WebSession = decode(raw)?;
            previous.validate()?;
            Some(previous.uid)
        } else {
            None
        };
        // Hash the full encrypted record, not the access token: every captured
        // field/generation change remains visible without embedding old secrets.
        let baseline = baseline.as_deref().map(generation);
        let pending = WebPending {
            id: id.clone(),
            account: account.into(),
            region,
            deadline: now() + WAIT_MS,
            baseline,
            expected_uid,
            state: "waiting".into(),
            request_id: request_id.map(str::to_owned),
        };
        let at = chrono::Utc::now();
        sqlx::query(
            "INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?)",
        )
        .bind(web_key(region, "pending", &id))
        .bind(encode(&pending)?)
        .bind(at)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|_| "无法保存Trae网页预约")?;
        tx.commit().await.map_err(|_| "无法提交Trae网页预约")?;
        let (shutdown, mut stop) = watch::channel(false);
        let ctx = WebCallback {
            core: Arc::downgrade(&self.core),
            region,
            id: id.clone(),
            port,
            shutdown: shutdown.clone(),
        };
        let registry = Arc::downgrade(&self.listeners);
        let done_id = id.clone();
        let router = Router::new()
            .route("/authorize", get(web_callback))
            .with_state(ctx);
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
        let callback = format!("http://127.0.0.1:{port}/authorize");
        let mut url = Url::parse(region.authorization_url()).map_err(|_| "Trae网页地址无效")?;
        url.query_pairs_mut().extend_pairs([
            ("login_version", "1"),
            ("auth_from", "trae"),
            ("auth_type", "local"),
            ("client_id", contract::CLIENT_ID),
            ("redirect", "0"),
            ("plugin_version", "llm-gateway/0.2.1"),
            ("login_trace_id", &id),
            ("auth_callback_url", &callback),
        ]);
        Ok(LoginStart {
            login_id: id,
            authorization_url: url.into(),
            deadline_ms: pending.deadline,
        })
    }
    pub async fn web_status(
        &self,
        region: WebRegion,
        account: &str,
    ) -> Result<WebLoginStatus, String> {
        if !id_valid(account) {
            return Err("Trae网页账号名称无效".into());
        }
        let saved_raw = self.core.read(&web_key(region, "session", account)).await?;
        let saved = saved_raw.as_deref().map(decode::<WebSession>).transpose()?;
        if let Some(session) = &saved {
            session.validate()?;
            if session.region != region {
                return Err("Trae网页会话地区不一致".into());
            }
        }
        let pending_rows: Vec<String> = sqlx::query_scalar(
            "SELECT value_enc FROM app_secrets WHERE name LIKE ? ORDER BY updated_at DESC LIMIT 65",
        )
        .bind(web_key(region, "pending", "%"))
        .fetch_all(&self.core.pool)
        .await
        .map_err(|_| "无法读取Trae网页等待状态")?;
        let mut pending_login = None;
        for raw in pending_rows {
            let pending: WebPending = decode(&raw)?;
            if pending.region != region || pending.account != account {
                continue;
            }
            if ![
                "waiting",
                "exchanging",
                "authenticated",
                "cancelled",
                "failed_no_retry",
            ]
            .contains(&pending.state.as_str())
            {
                return Err("Trae网页等待状态无效".into());
            }
            let state = if ["waiting", "exchanging"].contains(&pending.state.as_str()) {
                if pending.deadline <= now() {
                    "expired"
                } else if !self.listeners.0.lock().contains_key(&pending.id) {
                    "authorization_interrupted"
                } else {
                    &pending.state
                }
            } else {
                &pending.state
            };
            pending_login = Some(WebLoginPending {
                login_id: pending.id,
                deadline_ms: pending.deadline,
                state: state.into(),
                request_id: pending.request_id,
            });
            break;
        }
        let attempt = self
            .core
            .read(&web_key(region, "refresh", account))
            .await?
            .map(|raw| decode::<RefreshAttempt>(&raw))
            .transpose()?;
        let can_refresh = saved.as_ref().is_some_and(refresh::eligible)
            && !attempt.is_some_and(|a| {
                saved_raw.as_deref().map(generation).as_deref() == Some(a.generation.as_str())
            })
            && !pending_login.as_ref().is_some_and(|p| {
                p.deadline_ms > now()
                    && ["waiting", "exchanging", "authorization_interrupted"]
                        .contains(&p.state.as_str())
            });
        Ok(WebLoginStatus {
            region,
            authentication_mode: "web_legacy",
            account_id: account.into(),
            status: match &saved {
                None => "not_logged_in",
                Some(v) if v.pair.access_expires_at_ms.is_some_and(|v| v <= now()) => {
                    "access_expired"
                }
                Some(_) => "authenticated",
            }
            .into(),
            identity: saved.as_ref().map(|v| v.snapshot.clone()),
            access_expires_at_ms: saved.as_ref().and_then(|v| v.pair.access_expires_at_ms),
            refresh_expires_at_ms: saved.as_ref().and_then(|v| v.pair.refresh_expires_at_ms),
            can_refresh,
            native_device_verified: false,
            model_inference_verified: false,
            pending_login,
            store_country: saved.as_ref().and_then(|v| v.store_country.clone()),
        })
    }
    pub async fn web_poll(
        &self,
        region: WebRegion,
        account: &str,
        id: &str,
    ) -> Result<WebLoginStatus, String> {
        if !id_valid(account) || !id_valid(id) {
            return Err("Trae网页等待标识无效".into());
        }
        let raw = self
            .core
            .read(&web_key(region, "pending", id))
            .await?
            .ok_or("Trae网页等待不存在")?;
        let pending: WebPending = decode(&raw)?;
        if pending.account != account || pending.id != id || pending.region != region {
            return Err("Trae网页等待归属不一致".into());
        }
        let mut status = self.web_status(region, account).await?;
        if pending.state != "authenticated" {
            status.status = if pending.deadline <= now() {
                "expired"
            } else if pending.state == "waiting" && !self.listeners.0.lock().contains_key(id) {
                "authorization_interrupted"
            } else {
                &pending.state
            }
            .into();
        }
        Ok(status)
    }
    pub async fn web_cancel(
        &self,
        region: WebRegion,
        account: &str,
        id: &str,
    ) -> Result<WebLoginStatus, String> {
        if !id_valid(account) || !id_valid(id) {
            return Err("Trae网页等待标识无效".into());
        }
        let _guard = self.core.gate.lock().await;
        let raw = self
            .core
            .read(&web_key(region, "pending", id))
            .await?
            .ok_or("Trae网页等待不存在")?;
        let mut pending: WebPending = decode(&raw)?;
        if pending.account != account || pending.id != id || pending.region != region {
            return Err("Trae网页等待归属不一致".into());
        }
        if ["waiting", "exchanging"].contains(&pending.state.as_str()) {
            pending.state = "cancelled".into();
            self.core
                .replace(&web_key(region, "pending", id), &raw, &encode(&pending)?)
                .await?;
            if let Some(listener) = self.listeners.0.lock().remove(id) {
                let _ = listener.shutdown.send(true);
                listener.abort.abort();
            }
        }
        self.web_poll(region, account, id).await
    }
}

struct WebPayload {
    refresh: String,
    origin: WebOrigin,
}
fn payload(region: WebRegion, id: &str, query: &str) -> Result<WebPayload, String> {
    if query.len() > 32768 {
        return Err("Trae网页回调超过上限".into());
    }
    let raw = query.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' {
            let a = raw.get(index + 1).and_then(|v| char::from(*v).to_digit(16));
            let b = raw.get(index + 2).and_then(|v| char::from(*v).to_digit(16));
            decoded.push(
                ((a.ok_or("Trae网页回调编码无效")? << 4) | b.ok_or("Trae网页回调编码无效")?) as u8,
            );
            index += 3;
        } else {
            decoded.push(if raw[index] == b'+' { b' ' } else { raw[index] });
            index += 1;
        }
    }
    std::str::from_utf8(&decoded).map_err(|_| "Trae网页回调UTF8无效")?;
    let url = Url::parse(&format!("http://127.0.0.1/?{query}")).map_err(|_| "Trae网页回调无效")?;
    let mut fields = HashMap::new();
    for (key, value) in url.query_pairs() {
        if fields
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err("Trae网页回调字段重复".into());
        }
    }
    if fields.get("loginTraceID").map(String::as_str) != Some(id)
        || fields.get("scope").map(String::as_str) != Some("trae")
        || fields.contains_key("authCodeInfo")
        || fields.contains_key("code")
    {
        return Err("Trae网页回调状态或认证模式不一致".into());
    }
    let origin = WebOrigin::parse(
        region,
        fields.get("host").ok_or("Trae网页回调缺少认证主机")?,
    )?;
    let refresh = fields
        .get("refreshToken")
        .or_else(|| fields.get("data"))
        .filter(|v| contract::valid_token(v))
        .ok_or("Trae网页回调缺少有效刷新凭证")?;
    if fields.get("data").is_some_and(|v| v != refresh)
        || fields.get("error_code").is_some_and(|v| v != "0")
    {
        return Err("Trae网页回调凭证冲突或授权失败".into());
    }
    Ok(WebPayload {
        refresh: refresh.clone(),
        origin,
    })
}

#[derive(Clone)]
struct WebCallback {
    core: Weak<Core>,
    region: WebRegion,
    id: String,
    port: u16,
    shutdown: watch::Sender<bool>,
}
/// Closed, value-free failure categories. Never expose the callback query,
/// arbitrary host, upstream body, token, UserID or error text to the page/log.
fn callback_failure_code(error: &str) -> &'static str {
    match error {
        "Trae网页认证主机与所选地区不匹配，未发送凭证" => {
            "web_callback_origin_unverified"
        }
        "Trae网页回调缺少认证主机" => "web_callback_origin_missing",
        "Trae网页回调状态或认证模式不一致" => "web_callback_state_or_mode_mismatch",
        "Trae网页回调字段重复" => "web_callback_duplicate_fields",
        "Trae网页回调编码无效" | "Trae网页回调UTF8无效" => {
            "web_callback_encoding_invalid"
        }
        "Trae网页回调超过上限" => "web_callback_limit_exceeded",
        "Trae网页回调缺少有效刷新凭证" => "web_callback_refresh_missing",
        "Trae网页回调凭证冲突或授权失败" => {
            "web_callback_conflict_or_provider_rejection"
        }
        "Trae网页回调主机无效" => "web_callback_listener_host_invalid",
        _ => "web_callback_rejected",
    }
}
fn callback_origin_class(query: &str) -> &'static str {
    if query.len() > 32768 {
        return "unrecognized";
    }
    let Ok(url) = Url::parse(&format!("http://127.0.0.1/?{query}")) else {
        return "unrecognized";
    };
    let origins: Vec<_> = url.query_pairs().filter(|(k, _)| k == "host").collect();
    if origins.len() != 1 {
        return "unrecognized";
    }
    let Ok(host) = Url::parse(&origins[0].1) else {
        return "unrecognized";
    };
    if host.scheme() != "https"
        || !host.username().is_empty()
        || host.password().is_some()
        || host.path() != "/"
        || host.query().is_some()
        || host.fragment().is_some()
    {
        return "unrecognized";
    }
    match host.as_str().trim_end_matches('/') {
        "https://api.trae.cn" => "cn_api",
        "https://api.trae.com.cn" => "cn_api_alternate",
        "https://ug-normal.trae.ai" => "global_web_default",
        "https://ug-normal.us.trae.ai" => "global_web_us",
        "https://api-sg-central.trae.ai" => "global_web_sg_central",
        "https://growsg-normal.trae.ai" => "global_native_sg",
        "https://grow-normal.trae.ai" => "global_native_default",
        "https://grow-normal.traeapi.us" => "global_native_usttp",
        _ => "unrecognized",
    }
}
async fn web_callback(
    axum::extract::State(ctx): axum::extract::State<WebCallback>,
    headers: HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> impl IntoResponse {
    let valid = headers.get("host").and_then(|v| v.to_str().ok())
        == Some(format!("127.0.0.1:{}", ctx.port).as_str());
    let result = if valid {
        match ctx.core.upgrade() {
            Some(core) => {
                core.web_complete(ctx.region, &ctx.id, query.as_deref().unwrap_or(""))
                    .await
            }
            None => Err("Trae网页等待已结束".into()),
        }
    } else {
        Err("Trae网页回调主机无效".into())
    };
    if valid {
        if let Some(core) = ctx.core.upgrade() {
            if core
                .read(&web_key(ctx.region, "pending", &ctx.id))
                .await
                .ok()
                .flatten()
                .and_then(|raw| decode::<WebPending>(&raw).ok())
                .is_some_and(|v| !["waiting", "exchanging"].contains(&v.state.as_str()))
            {
                let _ = ctx.shutdown.send(true);
            }
        }
    }
    // A bad callback still cannot consume a valid wait. Diagnostics classify
    // only fixed public origins; recognition is not authority or an allowlist.
    let (status, text) = match result {
        Ok(()) => (
            axum::http::StatusCode::OK,
            "网页授权完成，请返回网关；原生设备与模型尚未验证。".to_owned(),
        ),
        Err(error) => {
            let code = callback_failure_code(&error);
            let origin_class = callback_origin_class(query.as_deref().unwrap_or(""));
            tracing::warn!(
                region = ctx.region.name(),
                code,
                origin_class,
                "Trae web callback rejected"
            );
            (axum::http::StatusCode::BAD_REQUEST,format!("网页授权未完成，请返回网关查看状态。诊断：{code} / {origin_class}（服务类别仅供定位，不代表已受信任）。"))
        }
    };
    (
        status,
        [
            ("Cache-Control", "no-store"),
            ("Referrer-Policy", "no-referrer"),
            ("Content-Security-Policy", "default-src 'none'"),
        ],
        text,
    )
}

impl Core {
    fn web_transport_origin(&self, origin: WebOrigin) -> Result<Url, String> {
        #[cfg(test)]
        if self.transport_origins.is_some() {
            return self.transport_origin(match origin {
                WebOrigin::Cn(value) => value,
                WebOrigin::Global | WebOrigin::GlobalSgCentral => CnOrigin::TraeCn,
                WebOrigin::GlobalUs => CnOrigin::TraeComCn,
            });
        }
        Url::parse(origin.issuer()).map_err(|_| "Trae网页认证主机无效".into())
    }
    async fn web_current(&self, pending: &WebPending, expected: &str) -> Result<(), String> {
        let names = [
            web_key(pending.region, "pending", &pending.id),
            web_key(pending.region, "session", &pending.account),
        ];
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name IN (?,?)")
                .bind(&names[0])
                .bind(&names[1])
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "无法核验Trae网页授权")?;
        if pending.deadline <= now()
            || rows.iter().find(|v| v.0 == names[0]).map(|v| v.1.as_str()) != Some(expected)
            || rows
                .iter()
                .find(|v| v.0 == names[1])
                .map(|v| generation(&v.1))
                != pending.baseline
        {
            return Err("Trae网页授权已取消、过期或账号已改变，未重放".into());
        }
        Ok(())
    }
    async fn web_complete(&self, region: WebRegion, id: &str, query: &str) -> Result<(), String> {
        let callback = payload(region, id, query)?;
        let (pending, expected) = {
            let _guard = self.gate.lock().await;
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|_| "无法预约Trae网页交换")?;
            sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
                .bind(web_key(region, "pending", id))
                .execute(&mut *tx)
                .await
                .map_err(|_| "无法锁定Trae网页交换")?;
            let raw: String = sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(web_key(region, "pending", id))
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法读取Trae网页等待")?
                .ok_or("Trae网页等待不存在")?;
            let mut pending: WebPending = decode(&raw)?;
            let baseline: Option<String> =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                    .bind(web_key(region, "session", &pending.account))
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| "无法核验Trae网页账号")?;
            if pending.id != id
                || pending.region != region
                || pending.state != "waiting"
                || pending.deadline <= now()
                || baseline.as_deref().map(generation) != pending.baseline
                || pending.baseline.is_some() != pending.expected_uid.is_some()
            {
                return Err("Trae网页授权已使用、过期或账号已改变，未重放".into());
            }
            pending.state = "exchanging".into();
            let expected = encode(&pending)?;
            sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=?")
                .bind(&expected)
                .bind(chrono::Utc::now())
                .bind(web_key(region, "pending", id))
                .execute(&mut *tx)
                .await
                .map_err(|_| "无法保存Trae网页单次预约")?;
            tx.commit().await.map_err(|_| "无法提交Trae网页预约")?;
            (pending, expected)
        };
        let result = self.web_exchange(&pending, &expected, &callback).await;
        if result.is_err() {
            let mut failed = pending.clone();
            failed.state = "failed_no_retry".into();
            let _ = self
                .replace(
                    &web_key(region, "pending", id),
                    &expected,
                    &encode(&failed)?,
                )
                .await;
        }
        result
    }
    async fn web_exchange(
        &self,
        pending: &WebPending,
        expected: &str,
        callback: &WebPayload,
    ) -> Result<(), String> {
        self.web_current(pending, expected).await?;
        let origin = self.web_transport_origin(callback.origin)?;
        let http = self.http.read().clone()?;
        let packet=http::post(&http,&origin,EXCHANGE_WEB,"",&json!({"ClientID":contract::CLIENT_ID,"RefreshToken":callback.refresh,"ClientSecret":"-","UserID":""})).await?;
        self.web_current(pending, expected).await?;
        let pair = contract::parse_pair(&packet, None, now())?;
        if pair.access_expires_at_ms.is_some_and(|v| v <= now()) {
            return Err("Trae网页兑换返回已过期令牌，未保存".into());
        }
        let mut secrets = vec![callback.refresh.as_str(), pair.access.as_str()];
        secrets.extend(pair.refresh.as_deref());
        // Independent token-only verification, distinct from the official
        // website's Cookie-based IfWebPage call. Fail closed on any mismatch.
        // Global provider support must still be established by actual login.
        let principal = http::post(
            &http,
            &origin,
            contract::PRINCIPAL,
            &pair.access,
            &json!({"ReqSource":"IDE","IDEVersion":contract::VERSION}),
        )
        .await?;
        let mut identity = contract::identity(&principal, &secrets)?;
        identity.snapshot.identity_hash = Some(pending.region.principal_hash(&identity.uid));
        self.web_current(pending, expected).await?;
        if pending
            .expected_uid
            .as_deref()
            .is_some_and(|uid| uid != identity.uid)
        {
            return Err("Trae网页账号主体已改变，请使用不同的本机名称".into());
        }
        let session = WebSession {
            region: pending.region,
            issuer: callback.origin.issuer().into(),
            client_id: contract::CLIENT_ID.into(),
            uid: identity.uid,
            pair,
            snapshot: identity.snapshot,
            store_country: principal_country(&principal),
        };
        session.validate()?;
        let _guard = self.gate.lock().await;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "无法保存Trae网页会话")?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(web_key(pending.region, "pending", &pending.id))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法锁定Trae网页保存")?;
        for (name, wanted, is_generation) in [
            (
                web_key(pending.region, "pending", &pending.id),
                Some(expected),
                false,
            ),
            (
                web_key(pending.region, "session", &pending.account),
                pending.baseline.as_deref(),
                true,
            ),
        ] {
            let value: Option<String> =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| "无法核验Trae网页保存")?;
            let observed = if is_generation {
                value.as_deref().map(generation)
            } else {
                value
            };
            if observed.as_deref() != wanted {
                return Err("Trae网页登录已取消或改变，未保存迟到结果".into());
            }
        }
        if pending.deadline <= now()
            || session
                .pair
                .access_expires_at_ms
                .is_some_and(|v| v <= now())
        {
            return Err("Trae网页授权或令牌已过期，未保存".into());
        }
        let at = chrono::Utc::now();
        sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET value_enc=excluded.value_enc,updated_at=excluded.updated_at").bind(web_key(pending.region, "session",&pending.account)).bind(encode(&session)?).bind(at).bind(at).execute(&mut *tx).await.map_err(|_|"无法保存Trae网页会话")?;
        let mut used = pending.clone();
        used.state = "authenticated".into();
        sqlx::query("UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=?")
            .bind(encode(&used)?)
            .bind(at)
            .bind(web_key(pending.region, "pending", &pending.id))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法完成Trae网页等待")?;
        tx.commit().await.map_err(|_| "无法提交Trae网页会话")?;
        Ok(())
    }
}
