//! Qoder campaign benefits. Production endpoints are fixed; tokens are supplied by the user.
//! GET only observes campaigns. POST is explicit or enabled by the separate automatic switch.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Local, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::watch;

use crate::benefit_config::{BenefitAccount, BenefitsConfig};
use crate::{crypto, db::repo};

const BODY_LIMIT: usize = 512 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN_MASK: &str = "********";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimPlatform {
    Qoder,
    QoderCn,
    TraeCn,
}

impl ClaimPlatform {
    pub fn key(self) -> &'static str {
        match self {
            Self::Qoder => "qoder",
            Self::QoderCn => "qoder_cn",
            Self::TraeCn => "trae_cn",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Qoder => "Qoder 国际版",
            Self::QoderCn => "Qoder 国内版",
            Self::TraeCn => "Trae 中国版",
        }
    }

    pub fn host(self) -> &'static str {
        match self {
            Self::Qoder => "openapi.qoder.sh",
            Self::QoderCn => "openapi.qoder.com.cn",
            Self::TraeCn => "api.trae.cn",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        match key {
            "qoder" => Some(Self::Qoder),
            "qoder_cn" => Some(Self::QoderCn),
            "trae_cn" => Some(Self::TraeCn),
            _ => None,
        }
    }

    pub fn all() -> [Self; 3] {
        [Self::Qoder, Self::QoderCn, Self::TraeCn]
    }
}

pub fn platform_for_host(host: &str) -> Option<ClaimPlatform> {
    ClaimPlatform::all()
        .into_iter()
        .find(|platform| host.eq_ignore_ascii_case(platform.host()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenefitCampaign {
    pub campaign_id: String,
    pub campaign_key: String,
    pub claim_status: String,
    pub action_type: String,
    pub amount: Option<f64>,
    pub kind: Option<String>,
    pub valid_days: Option<u32>,
    pub start_at: Option<i64>,
    pub end_at: Option<i64>,
}

impl BenefitCampaign {
    pub fn is_claimable(&self) -> bool {
        self.claim_status == "CLAIMABLE" && self.action_type == "CLAIM_BENEFIT"
    }

    pub fn is_claimed(&self) -> bool {
        self.claim_status == "CLAIMED"
    }

    pub fn is_active(&self, now: i64) -> bool {
        matches!((self.start_at, self.end_at), (Some(start), Some(end)) if start <= now && now < end)
    }

    pub fn window_key(&self, platform: ClaimPlatform) -> Option<String> {
        let (start, end) = (self.start_at?, self.end_at?);
        (start < end).then(|| format!("{}:{}:{start}:{end}", platform.key(), self.campaign_id))
    }

    pub fn describe(&self) -> String {
        match (self.amount, self.kind.as_deref()) {
            (Some(amount), Some(kind)) => format!("{amount} {kind}"),
            _ => "活动未提供权益数量".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenefitStatus {
    pub platform: ClaimPlatform,
    pub claimable: bool,
    pub campaigns: Vec<BenefitCampaign>,
    pub source: String,
    pub warnings: Vec<String>,
    pub checked_at: String,
}

impl BenefitStatus {
    pub fn next_claimable(&self) -> Option<&BenefitCampaign> {
        self.campaigns
            .iter()
            .find(|campaign| campaign.is_claimable())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimOutcome {
    pub platform: ClaimPlatform,
    pub campaign_id: String,
    pub campaign_key: String,
    pub status: String,
    pub replayed: bool,
    pub granted: bool,
    pub amount: Option<f64>,
    pub kind: Option<String>,
    pub claimed_at: Option<String>,
    pub expires_at: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenefitVerdict {
    Granted,
    Replayed,
    NoClaimable,
    Skipped,
    Error,
}

impl BenefitVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Replayed => "replayed",
            Self::NoClaimable => "no_claimable",
            Self::Skipped => "skipped",
            Self::Error => "error",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "granted" => Some(Self::Granted),
            "replayed" => Some(Self::Replayed),
            "no_claimable" => Some(Self::NoClaimable),
            "skipped" => Some(Self::Skipped),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    pub fn settles_window(self) -> bool {
        matches!(self, Self::Granted | Self::Replayed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenefitRunRecord {
    pub id: i64,
    pub account_id: String,
    pub platform: String,
    pub window_key: String,
    pub campaign_key: Option<String>,
    pub verdict: BenefitVerdict,
    pub amount: Option<f64>,
    pub message: String,
    pub manual: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenefitClaimResult {
    pub run: BenefitRunRecord,
    pub outcome: Option<ClaimOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenefitCapabilities {
    pub query: bool,
    pub manual_claim: bool,
    pub auto_claim: bool,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenefitAccountView {
    pub account_id: String,
    pub platform: String,
    pub label: String,
    pub enabled: bool,
    pub has_token: bool,
    pub token_masked: Option<String>,
    pub status: Option<BenefitStatus>,
    pub last_run: Option<BenefitRunRecord>,
    pub error: Option<String>,
    pub capabilities: BenefitCapabilities,
    #[serde(default)]
    pub session_account: Option<crate::client_session::SessionAccountView>,
    #[serde(default)]
    pub session_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenefitOverview {
    pub enabled: bool,
    pub auto_claim: bool,
    pub auto_claim_after_hour: u32,
    pub auto_claim_after_minute: u32,
    pub auto_claim_timezone: String,
    pub checked_at: String,
    pub accounts: Vec<BenefitAccountView>,
}

pub fn auth_value(token: &str) -> String {
    let token = token.trim();
    if token.contains(' ') {
        token.to_string()
    } else {
        format!("Bearer {token}")
    }
}

fn ensure_token(token: &str) -> Result<(), String> {
    if token.trim().is_empty() {
        return Err("请先手工设置该账号的访问 token".into());
    }
    if token.len() > 16 * 1024
        || token.bytes().any(|byte| byte.is_ascii_control())
        || reqwest::header::HeaderValue::from_str(&auth_value(token)).is_err()
    {
        return Err("访问 token 不是合法的 Authorization 值".into());
    }
    Ok(())
}

fn identifier(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?;
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    .then(|| value.to_string())
}

fn amount(value: &Value) -> Option<f64> {
    value
        .get("amount")?
        .as_f64()
        .filter(|amount| amount.is_finite() && *amount >= 0.0)
}

fn kind(value: &Value) -> Option<String> {
    (value.get("kind")?.as_str()? == "CREDITS").then(|| "CREDITS".into())
}

fn timestamp(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|time| time.with_timezone(&Utc).to_rfc3339())
}

pub fn parse_status(platform: ClaimPlatform, value: &Value) -> Result<BenefitStatus, String> {
    let entries = value
        .get("campaigns")
        .and_then(Value::as_array)
        .ok_or_else(|| "平台活动响应缺少 campaigns 数组".to_string())?;
    if entries.len() > 256 {
        return Err("平台活动数量超过安全上限".into());
    }
    let mut campaigns = Vec::new();
    for entry in entries {
        let campaign_id = identifier(entry.get("campaignId"))
            .ok_or_else(|| "平台活动标识格式无效".to_string())?;
        let campaign_key = identifier(entry.get("campaignKey"))
            .ok_or_else(|| "平台活动 key 格式无效".to_string())?;
        let benefit = entry.get("benefit").unwrap_or(&Value::Null);
        let claim_status = match entry.get("claimStatus").and_then(Value::as_str) {
            Some("CLAIMABLE") => "CLAIMABLE",
            Some("CLAIMED") => "CLAIMED",
            _ => "UNKNOWN",
        };
        let action_type = match entry.get("actionType").and_then(Value::as_str) {
            Some("CLAIM_BENEFIT") => "CLAIM_BENEFIT",
            _ => "UNKNOWN",
        };
        campaigns.push(BenefitCampaign {
            campaign_id,
            campaign_key,
            claim_status: claim_status.into(),
            action_type: action_type.into(),
            amount: amount(benefit),
            kind: kind(benefit),
            valid_days: benefit
                .get("validity")
                .filter(|validity| {
                    validity.get("mode").and_then(Value::as_str) == Some("RELATIVE_DAYS")
                })
                .and_then(|validity| validity.get("days"))
                .and_then(Value::as_u64)
                .and_then(|days| u32::try_from(days).ok())
                .filter(|days| *days > 0),
            start_at: entry.get("startAt").and_then(Value::as_i64),
            end_at: entry.get("endAt").and_then(Value::as_i64),
        });
    }
    let claimable = campaigns.iter().any(BenefitCampaign::is_claimable);
    let mut warnings = Vec::new();
    if value.get("claimable").and_then(Value::as_bool) != Some(claimable) {
        warnings.push("平台总开关与逐条活动状态不一致，已以逐条活动为准".into());
    }
    if campaigns.iter().any(|campaign| {
        !matches!((campaign.start_at, campaign.end_at), (Some(start), Some(end)) if start < end)
    }) {
        warnings.push("部分活动没有有效的领取窗口，不会向这些活动发起领取".into());
    }
    Ok(BenefitStatus {
        platform,
        claimable,
        campaigns,
        source: if platform == ClaimPlatform::TraeCn {
            "https://api.trae.cn/trae/api/v2/ug/checkin_credits/status".into()
        } else {
            format!("https://{}/sash/api/v1/me/campaigns", platform.host())
        },
        warnings,
        checked_at: Utc::now().to_rfc3339(),
    })
}

pub fn parse_claim(
    platform: ClaimPlatform,
    campaign_id: &str,
    value: &Value,
) -> Result<ClaimOutcome, String> {
    let returned_id = identifier(value.get("campaignId"))
        .ok_or_else(|| "领取响应缺少合法活动标识".to_string())?;
    if returned_id != campaign_id {
        return Err("领取响应与请求活动不一致".into());
    }
    let campaign_key = identifier(value.get("campaignKey"))
        .ok_or_else(|| "领取响应缺少合法活动 key".to_string())?;
    if value.get("status").and_then(Value::as_str) != Some("CLAIMED") {
        return Err("平台未确认活动已领取".into());
    }
    let replayed = value
        .get("replayed")
        .and_then(Value::as_bool)
        .ok_or_else(|| "领取响应缺少 replayed 标志，无法确认是否发放".to_string())?;
    let benefit = value.get("benefit").unwrap_or(&Value::Null);
    if !replayed
        && (kind(benefit).is_none() || amount(benefit).map_or(true, |amount| amount <= 0.0))
    {
        return Err("平台未提供可确认的正数 CREDITS 发放结果".into());
    }
    Ok(ClaimOutcome {
        platform,
        campaign_id: returned_id,
        campaign_key,
        status: "CLAIMED".into(),
        replayed,
        granted: !replayed,
        amount: amount(benefit),
        kind: kind(benefit),
        claimed_at: timestamp(value.get("claimedAt")),
        expires_at: timestamp(value.get("expiresAt")),
        message: if replayed {
            "平台确认已领取，本次没有再次发放".into()
        } else {
            "平台确认本次发放权益".into()
        },
    })
}

/// Only stable, known platform diagnostics can cross the IPC boundary; arbitrary bodies cannot.
pub fn http_error(status: u16, body: &Value) -> String {
    let reason = match body.get("message").and_then(Value::as_str) {
        Some("missing authorization token") => "missing authorization token",
        Some("invalid authorization token") => "invalid authorization token",
        Some("token expired") => "token expired",
        _ => "平台未提供可安全显示的原因",
    };
    match status {
        401 | 403 => format!("凭据被平台拒绝（HTTP {status}）：{reason}"),
        404 => "平台领取接口不存在（HTTP 404），请核对适配器版本".into(),
        429 => "平台请求过于频繁（HTTP 429），请稍后重试".into(),
        _ => format!("平台权益请求失败（HTTP {status}）"),
    }
}

/// Typed transport injection keeps tests local without adding configurable production hosts.
#[async_trait]
pub trait BenefitTransport: Send + Sync {
    async fn request(
        &self,
        platform: ClaimPlatform,
        token: &str,
        proxy: Option<&str>,
        campaign_id: Option<&str>,
    ) -> Result<Value, String>;
    async fn session_account(
        &self,
        _platform: ClaimPlatform,
        _token: &str,
        _proxy: Option<&str>,
    ) -> Result<Option<crate::client_session::SessionAccountView>, String> {
        Ok(None)
    }
}

#[derive(Default)]
pub struct HttpBenefitTransport {
    #[cfg(test)]
    mock_endpoint: Option<String>,
}

#[async_trait]
impl BenefitTransport for HttpBenefitTransport {
    async fn session_account(
        &self,
        platform: ClaimPlatform,
        token: &str,
        proxy: Option<&str>,
    ) -> Result<Option<crate::client_session::SessionAccountView>, String> {
        #[cfg(test)]
        if self.mock_endpoint.is_some() {
            return Ok(None);
        }
        crate::client_session::account_view(platform, token, proxy).await
    }
    async fn request(
        &self,
        platform: ClaimPlatform,
        token: &str,
        proxy: Option<&str>,
        campaign_id: Option<&str>,
    ) -> Result<Value, String> {
        if platform == ClaimPlatform::TraeCn {
            return crate::trae_benefits::request(token, proxy, campaign_id).await;
        }
        if token.trim_start().starts_with('{') {
            crate::client_session::SessionCredential::parse(token, platform)?;
        } else {
            ensure_token(token)?;
        }
        if let Some(id) = campaign_id {
            identifier(Some(&Value::String(id.to_string())))
                .ok_or_else(|| "领取活动标识格式无效".to_string())?;
        }
        let builder = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(6))
            .timeout(REQUEST_TIMEOUT);
        #[cfg(test)]
        let builder = if self.mock_endpoint.is_some() {
            builder.no_proxy()
        } else {
            builder
        };
        let client = crate::outbound::apply_proxy(builder, proxy)
            .map_err(|_| "权益代理配置无效".to_string())?
            .build()
            .map_err(|_| "无法初始化权益请求".to_string())?;
        let base = format!("https://{}/sash/api/v1/me/campaigns", platform.host());
        #[cfg(test)]
        let base = self.mock_endpoint.as_ref().map_or(base, |endpoint| {
            format!("{endpoint}/sash/api/v1/me/campaigns")
        });
        let request = match campaign_id {
            Some(id) => client.post(format!("{base}/{id}/claim")),
            None => client.get(base),
        };
        let response = request
            .headers(crate::client_session::request_headers(token, platform)?)
            .header(reqwest::header::HOST, platform.host())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| "权益请求连接失败或超时；未自动重发领取".to_string())?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|len| len > BODY_LIMIT as u64)
        {
            return Err("平台权益响应超过 512 KiB 上限".into());
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "读取平台权益响应失败".to_string())?;
            if bytes.len().saturating_add(chunk.len()) > BODY_LIMIT {
                return Err("平台权益响应超过 512 KiB 上限".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(http_error(status.as_u16(), &value));
        }
        if value.is_null() {
            return Err("平台权益响应不是合法 JSON".into());
        }
        // A reflected Authorization value must never turn into an activity ID or other DTO field.
        if crate::client_session::contains_credential(&bytes, token, platform) {
            return Err("平台权益响应包含敏感凭据，已拒绝展示".into());
        }
        Ok(value)
    }
}

pub async fn fetch_status(
    platform: ClaimPlatform,
    token: &str,
    proxy: Option<&str>,
) -> Result<BenefitStatus, String> {
    let value = HttpBenefitTransport::default()
        .request(platform, token, proxy, None)
        .await?;
    parse_status(platform, &value)
}

pub async fn claim(
    platform: ClaimPlatform,
    token: &str,
    proxy: Option<&str>,
    campaign_id: &str,
) -> Result<ClaimOutcome, String> {
    let value = HttpBenefitTransport::default()
        .request(platform, token, proxy, Some(campaign_id))
        .await?;
    parse_claim(platform, campaign_id, &value)
}

type FlightResult = Result<BenefitClaimResult, String>;

struct Flight {
    result: watch::Receiver<Option<FlightResult>>,
}

type Flights = Arc<parking_lot::Mutex<HashMap<String, Arc<Flight>>>>;
type Windows = Arc<parking_lot::Mutex<HashMap<String, BenefitCampaign>>>;

struct FlightCleanup {
    flights: Flights,
    key: String,
    flight: Arc<Flight>,
}

impl Drop for FlightCleanup {
    fn drop(&mut self) {
        let mut flights = self.flights.lock();
        if flights
            .get(&self.key)
            .is_some_and(|entry| Arc::ptr_eq(entry, &self.flight))
        {
            flights.remove(&self.key);
        }
    }
}

pub struct BenefitsCenter {
    pool: SqlitePool,
    transport: Arc<dyn BenefitTransport>,
    flights: Flights,
    windows: Windows,
}

impl BenefitsCenter {
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_transport(pool, Arc::new(HttpBenefitTransport::default()))
    }

    pub fn with_transport(pool: SqlitePool, transport: Arc<dyn BenefitTransport>) -> Self {
        Self {
            pool,
            transport,
            flights: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            windows: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    pub async fn set_token(
        &self,
        cfg: &BenefitsConfig,
        account_id: &str,
        token: &str,
    ) -> Result<(), String> {
        let account = account(cfg, account_id)?;
        ensure_token(token)?;
        let encrypted =
            crypto::encrypt(token.trim()).map_err(|_| "无法加密权益 token".to_string())?;
        repo::set_secret(&self.pool, &secret_name(account), &encrypted)
            .await
            .map_err(|_| "无法保存加密权益 token".to_string())?;
        self.windows.lock().remove(&account_key(account));
        Ok(())
    }

    pub async fn set_session(
        &self,
        cfg: &BenefitsConfig,
        account_id: &str,
        session: &crate::client_session::SessionCredential,
    ) -> Result<(), String> {
        let account = account(cfg, account_id)?;
        let platform = ClaimPlatform::parse(&account.platform).ok_or("权益平台尚未适配")?;
        session.validate(platform)?;
        let raw = serde_json::to_string(session).map_err(|_| "登录上下文格式无效")?;
        crate::client_session::SessionCredential::parse(&raw, platform)?;
        let encrypted = crypto::encrypt(&raw).map_err(|_| "无法加密登录上下文")?;
        repo::set_secret(&self.pool, &secret_name(account), &encrypted)
            .await
            .map_err(|_| "无法保存加密登录上下文")?;
        self.windows.lock().remove(&account_key(account));
        Ok(())
    }

    pub async fn clear_token(&self, cfg: &BenefitsConfig, account_id: &str) -> Result<(), String> {
        let account = account(cfg, account_id)?;
        repo::delete_secret(&self.pool, &secret_name(account))
            .await
            .map_err(|_| "无法删除权益 token".to_string())?;
        self.windows.lock().remove(&account_key(account));
        Ok(())
    }

    pub(crate) async fn session_for_native(
        &self,
        cfg: &BenefitsConfig,
        account_id: &str,
    ) -> Result<crate::client_session::SessionCredential, String> {
        let account = account(cfg, account_id)?;
        if !cfg.enabled || !account.enabled || account.platform != "trae_cn" {
            return Err("Trae 权益账号未启用".into());
        }
        let encrypted = repo::get_secret(&self.pool, &secret_name(account))
            .await
            .map_err(|_| "无法读取加密登录")?
            .ok_or("请先复用 Trae 客户端登录")?;
        let raw = crypto::decrypt(&encrypted).map_err(|_| "无法解密登录，请重新导入")?;
        crate::client_session::SessionCredential::parse(&raw, ClaimPlatform::TraeCn)
    }

    pub async fn runs(
        &self,
        account_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<BenefitRunRecord>, String> {
        if !(1..=200).contains(&limit) {
            return Err("权益记录条数必须在 1..200 之间".into());
        }
        repo::list_benefit_runs(&self.pool, account_id, limit)
            .await
            .map_err(|_| "无法读取权益执行记录".to_string())
    }

    pub async fn overview(
        &self,
        cfg: &BenefitsConfig,
        proxy: Option<&str>,
    ) -> Result<BenefitOverview, String> {
        validate(cfg)?;
        let mut accounts = Vec::new();
        for account in &cfg.accounts {
            let encrypted = repo::get_secret(&self.pool, &secret_name(account))
                .await
                .map_err(|_| "无法读取权益 token 状态".to_string())?;
            let has_token = encrypted.is_some();
            let last_run = self.runs(Some(&account.id), 1).await?.into_iter().next();
            let (status, error) = if !cfg.enabled || !account.enabled {
                (None, None)
            } else if let Some(encrypted) = encrypted.as_ref() {
                match crypto::decrypt(encrypted) {
                    Ok(token) => {
                        match status_with(self.transport.as_ref(), account, &token, proxy).await {
                            Ok(status) => (Some(status), None),
                            Err(error) => (None, Some(safe_transport_error(&error))),
                        }
                    }
                    Err(_) => (None, Some("无法解密权益 token，请重新设置".into())),
                }
            } else {
                (None, Some("请先手工设置该账号的访问 token".into()))
            };
            let (session_account, session_error) = if status.is_some() {
                if let Some(raw) = encrypted
                    .as_ref()
                    .and_then(|secret| crypto::decrypt(secret).ok())
                {
                    match self
                        .transport
                        .session_account(
                            ClaimPlatform::parse(&account.platform).expect("validated platform"),
                            &raw,
                            proxy,
                        )
                        .await
                    {
                        Ok(view) => (view, None),
                        Err(_) => (
                            None,
                            Some("账号资料读取失败，请刷新或重新复用客户端登录".into()),
                        ),
                    }
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };
            let trae_direct = account.platform == "trae_cn"
                && encrypted
                    .as_ref()
                    .and_then(|secret| crypto::decrypt(secret).ok())
                    .and_then(|raw| {
                        crate::client_session::SessionCredential::parse(&raw, ClaimPlatform::TraeCn)
                            .ok()
                    })
                    .is_some_and(|session| session.has_trae_device_context());
            accounts.push(BenefitAccountView {
                account_id: account.id.clone(),
                platform: account.platform.clone(),
                label: account.label.clone(),
                enabled: account.enabled,
                has_token,
                session_account,
                session_error,
                token_masked: has_token.then(|| TOKEN_MASK.into()),
                status,
                last_run,
                error,
                capabilities: BenefitCapabilities {
                    query: true,
                    manual_claim: true,
                    auto_claim: true,
                    scope: if trae_direct {
                        "完整登录上下文每日直连签到；无需运行客户端；积分不代表总余额".into()
                    } else if account.platform == "trae_cn" {
                        "原生客户端每日签到；后台临时启动，不使用 HTTP 桥接；积分不代表总余额"
                            .into()
                    } else {
                        "已适配活动赠送权益；不代表账号总余额".into()
                    },
                },
            });
        }
        Ok(BenefitOverview {
            enabled: cfg.enabled,
            auto_claim: cfg.auto_claim,
            auto_claim_after_hour: cfg.auto_claim_after_hour,
            auto_claim_after_minute: cfg.auto_claim_after_minute,
            auto_claim_timezone: cfg.auto_claim_timezone.clone(),
            checked_at: Utc::now().to_rfc3339(),
            accounts,
        })
    }

    pub async fn claim_now(
        &self,
        cfg: &BenefitsConfig,
        proxy: Option<&str>,
        account_id: &str,
    ) -> FlightResult {
        self.claim(cfg, proxy, account_id, true).await
    }

    pub async fn claim(
        &self,
        cfg: &BenefitsConfig,
        proxy: Option<&str>,
        account_id: &str,
        manual: bool,
    ) -> FlightResult {
        self.claim_at(cfg, proxy, account_id, manual, Local::now())
            .await
    }

    pub async fn claim_at(
        &self,
        cfg: &BenefitsConfig,
        proxy: Option<&str>,
        account_id: &str,
        manual: bool,
        now: DateTime<Local>,
    ) -> FlightResult {
        let account = account(cfg, account_id)?.clone();
        if !cfg.enabled || !account.enabled {
            return Err("权益中心或该账号未启用；未发起网络请求".into());
        }
        if !manual && (!cfg.auto_claim || !cfg.account_is_due(account_id, now)) {
            return Err("自动领取未启用或尚未到开始时间；未发起网络请求".into());
        }
        let key = account_key(&account);
        let (flight, sender) = {
            let mut flights = self.flights.lock();
            if let Some(flight) = flights.get(&key) {
                (flight.clone(), None)
            } else {
                let (sender, result) = watch::channel(None);
                let flight = Arc::new(Flight { result });
                flights.insert(key.clone(), flight.clone());
                (flight, Some(sender))
            }
        };
        if let Some(sender) = sender {
            let cleanup = FlightCleanup {
                flights: self.flights.clone(),
                key,
                flight: flight.clone(),
            };
            let pool = self.pool.clone();
            let transport = self.transport.clone();
            let windows = self.windows.clone();
            let proxy = proxy.map(str::to_string);
            // Finish and persist even if the IPC/HTTP caller disconnects after remote POST.
            tokio::spawn(async move {
                let _cleanup = cleanup;
                let result = execute(Execution {
                    pool: &pool,
                    transport: transport.as_ref(),
                    windows: &windows,
                    account: &account,
                    proxy: proxy.as_deref(),
                    manual,
                    now,
                })
                .await;
                let _ = sender.send(Some(result));
            });
        }
        let mut result = flight.result.clone();
        loop {
            let current = result.borrow_and_update().clone();
            if let Some(current) = current {
                return current;
            }
            result
                .changed()
                .await
                .map_err(|_| "权益执行已中止，请刷新本地记录".to_string())?;
        }
    }

    pub async fn auto_tick(
        &self,
        cfg: &BenefitsConfig,
        proxy: Option<&str>,
        now: DateTime<Local>,
    ) -> Result<Vec<BenefitClaimResult>, String> {
        validate(cfg)?;
        if !cfg.enabled || !cfg.auto_claim || cfg.scheduler_mode == "windows_task" {
            return Ok(Vec::new());
        }
        let mut results = Vec::new();
        for account in cfg
            .accounts
            .iter()
            .filter(|account| account.enabled && cfg.account_is_due(&account.id, now))
        {
            results.push(self.claim_at(cfg, proxy, &account.id, false, now).await?);
        }
        Ok(results)
    }
}

fn validate(cfg: &BenefitsConfig) -> Result<(), String> {
    cfg.validate().map_err(|error| error.to_string())
}

fn account<'a>(cfg: &'a BenefitsConfig, id: &str) -> Result<&'a BenefitAccount, String> {
    validate(cfg)?;
    cfg.accounts
        .iter()
        .find(|account| account.id == id)
        .ok_or_else(|| "权益账号不存在".into())
}

fn account_key(account: &BenefitAccount) -> String {
    format!("{}:{}", account.platform, account.id)
}

fn secret_name(account: &BenefitAccount) -> String {
    format!("benefit:{}:{}:token", account.platform, account.id)
}

fn safe_transport_error(error: &str) -> String {
    // Transport implementors may return raw errors. Only this fixed allowlist reaches storage/UI.
    const MESSAGES: &[&str] = &[
        "请先手工设置该账号的访问 token",
        "访问 token 不是合法的 Authorization 值",
        "权益代理配置无效",
        "无法初始化权益请求",
        "权益请求连接失败或超时；未自动重发领取",
        "读取平台权益响应失败",
        "平台权益响应超过 512 KiB 上限",
        "平台权益响应不是合法 JSON",
        "平台权益响应包含敏感凭据，已拒绝展示",
        "平台活动响应缺少 campaigns 数组",
        "平台活动数量超过安全上限",
        "平台活动标识格式无效",
        "平台活动 key 格式无效",
        "领取响应缺少合法活动标识",
        "领取响应与请求活动不一致",
        "领取响应缺少合法活动 key",
        "平台未提供可确认的正数 CREDITS 发放结果",
        "领取响应与当前活动 key 不一致",
        "平台未确认活动已领取",
        "领取响应缺少 replayed 标志，无法确认是否发放",
        "平台领取接口不存在（HTTP 404），请核对适配器版本",
        "Trae 原生账号绑定缺失，请重新复用客户端登录",
        "Trae 直连领取被平台拒绝；未自动重发",
        "Trae 直连领取未获同日状态确认；未自动重发",
        "Trae 直连领取数量未获确认；未自动重发",
        "Trae 原生签到接口不可用；请检查客户端登录和后台连接，未自动重发",
        "Trae 原生响应超过安全上限",
        "Trae 原生签到未返回可验证结果",
        "Trae 原生签到返回重复结果",
        "Trae 原生签到响应格式无效",
        "Trae 原生签到结果缺少重放标志",
        "Trae 原生签到确认条件未满足",
        "平台请求过于频繁（HTTP 429），请稍后重试",
    ];
    if MESSAGES.contains(&error) {
        return error.into();
    }
    for status in [401u16, 403, 400, 405, 408, 500, 502, 503, 504] {
        for reason in [
            "missing authorization token",
            "invalid authorization token",
            "token expired",
            "平台未提供可安全显示的原因",
        ] {
            let expected = http_error(status, &serde_json::json!({"message":reason}));
            if error == expected {
                return expected;
            }
        }
    }
    "权益请求失败；原始响应及凭据信息已隐藏".into()
}

async fn status_with(
    transport: &dyn BenefitTransport,
    account: &BenefitAccount,
    token: &str,
    proxy: Option<&str>,
) -> Result<BenefitStatus, String> {
    ensure_token(token)?;
    let platform =
        ClaimPlatform::parse(&account.platform).ok_or_else(|| "权益平台尚未适配".to_string())?;
    let value = transport.request(platform, token, proxy, None).await?;
    reject_reflected_token(&value, token)?;
    parse_status(platform, &value)
}

fn reject_reflected_token(value: &Value, token: &str) -> Result<(), String> {
    let bare = token
        .trim()
        .split_once(' ')
        .map_or(token.trim(), |(_, value)| value);
    let raw = serde_json::to_string(value).map_err(|_| "平台权益响应不是合法 JSON".to_string())?;
    if !bare.is_empty() && raw.contains(bare) {
        return Err("平台权益响应包含敏感凭据，已拒绝展示".into());
    }
    Ok(())
}

struct Execution<'a> {
    pool: &'a SqlitePool,
    transport: &'a dyn BenefitTransport,
    windows: &'a Windows,
    account: &'a BenefitAccount,
    proxy: Option<&'a str>,
    manual: bool,
    now: DateTime<Local>,
}

async fn execute(execution: Execution<'_>) -> FlightResult {
    let Execution {
        pool,
        transport,
        windows,
        account,
        proxy,
        manual,
        now,
    } = execution;
    let platform =
        ClaimPlatform::parse(&account.platform).ok_or_else(|| "权益平台尚未适配".to_string())?;
    let cached = windows.lock().get(&account_key(account)).cloned();
    if let Some(cached) = cached.filter(|campaign| campaign.is_active(now.timestamp())) {
        if let Some(window_key) = cached.window_key(platform) {
            if repo::benefit_window_settled(pool, &account.id, &window_key)
                .await
                .map_err(|_| "无法读取权益幂等记录".to_string())?
            {
                return record(
                    pool,
                    account,
                    Some(&cached),
                    BenefitVerdict::Skipped,
                    None,
                    "该活动窗口已领取，本次未发起网络请求",
                    manual,
                    now,
                )
                .await;
            }
        }
    }
    let token = match repo::get_secret(pool, &secret_name(account)).await {
        Ok(Some(encrypted)) => match crypto::decrypt(&encrypted) {
            Ok(token) => token,
            Err(_) => {
                return record(
                    pool,
                    account,
                    None,
                    BenefitVerdict::Error,
                    None,
                    "无法解密权益 token，请重新设置",
                    manual,
                    now,
                )
                .await
            }
        },
        Ok(None) => {
            return record(
                pool,
                account,
                None,
                BenefitVerdict::Error,
                None,
                "请先手工设置该账号的访问 token",
                manual,
                now,
            )
            .await
        }
        Err(_) => return Err("无法读取加密权益 token".into()),
    };
    let status = match status_with(transport, account, &token, proxy).await {
        Ok(status) => status,
        Err(error) => {
            return record(
                pool,
                account,
                None,
                BenefitVerdict::Error,
                None,
                &safe_transport_error(&error),
                manual,
                now,
            )
            .await
        }
    };
    let campaign = status
        .campaigns
        .iter()
        .find(|campaign| campaign.is_claimable() && campaign.is_active(now.timestamp()));
    let Some(campaign) = campaign else {
        if let Some(claimed) = status
            .campaigns
            .iter()
            .find(|campaign| campaign.is_claimed() && campaign.is_active(now.timestamp()))
        {
            return record(
                pool,
                account,
                Some(claimed),
                BenefitVerdict::Replayed,
                None,
                "平台活动显示已领取，本次没有再次发放，也未调用领取接口",
                manual,
                now,
            )
            .await;
        }
        return record(
            pool,
            account,
            None,
            BenefitVerdict::NoClaimable,
            None,
            "当前没有处于有效窗口的可领活动；未调用领取接口",
            manual,
            now,
        )
        .await;
    };
    let window_key = campaign
        .window_key(platform)
        .ok_or_else(|| "活动没有有效的领取窗口".to_string())?;
    if repo::benefit_window_settled(pool, &account.id, &window_key)
        .await
        .map_err(|_| "无法读取权益幂等记录".to_string())?
    {
        windows
            .lock()
            .insert(account_key(account), campaign.clone());
        return record(
            pool,
            account,
            Some(campaign),
            BenefitVerdict::Skipped,
            None,
            "该活动窗口已有领取记录，本次未调用领取接口",
            manual,
            now,
        )
        .await;
    }
    // One POST per execution. On uncertain failures, a later call first performs GET again.
    let outcome = match transport
        .request(platform, &token, proxy, Some(&campaign.campaign_id))
        .await
    {
        Ok(value) => match parse_claim(platform, &campaign.campaign_id, &value) {
            Ok(outcome) => {
                if reject_reflected_token(&value, &token).is_err() {
                    return record(
                        pool,
                        account,
                        Some(campaign),
                        BenefitVerdict::Error,
                        None,
                        "平台权益响应包含敏感凭据，已拒绝展示",
                        manual,
                        now,
                    )
                    .await;
                }
                if outcome.campaign_key != campaign.campaign_key {
                    return record(
                        pool,
                        account,
                        Some(campaign),
                        BenefitVerdict::Error,
                        None,
                        "领取响应与当前活动 key 不一致",
                        manual,
                        now,
                    )
                    .await;
                }
                outcome
            }
            Err(error) => {
                return record(
                    pool,
                    account,
                    Some(campaign),
                    BenefitVerdict::Error,
                    None,
                    &safe_transport_error(&error),
                    manual,
                    now,
                )
                .await
            }
        },
        Err(error) => {
            return record(
                pool,
                account,
                Some(campaign),
                BenefitVerdict::Error,
                None,
                &safe_transport_error(&error),
                manual,
                now,
            )
            .await
        }
    };
    let verdict = if outcome.granted {
        BenefitVerdict::Granted
    } else {
        BenefitVerdict::Replayed
    };
    let message = outcome.message.clone();
    let result = record(
        pool,
        account,
        Some(campaign),
        verdict,
        Some(outcome),
        &message,
        manual,
        now,
    )
    .await?;
    windows
        .lock()
        .insert(account_key(account), campaign.clone());
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn record(
    pool: &SqlitePool,
    account: &BenefitAccount,
    campaign: Option<&BenefitCampaign>,
    verdict: BenefitVerdict,
    outcome: Option<ClaimOutcome>,
    message: &str,
    manual: bool,
    now: DateTime<Local>,
) -> FlightResult {
    let platform =
        ClaimPlatform::parse(&account.platform).ok_or_else(|| "权益平台尚未适配".to_string())?;
    let mut run = BenefitRunRecord {
        id: 0,
        account_id: account.id.clone(),
        platform: account.platform.clone(),
        window_key: campaign
            .and_then(|campaign| campaign.window_key(platform))
            .unwrap_or_else(|| format!("{}:unresolved", platform.key())),
        campaign_key: campaign.map(|campaign| campaign.campaign_key.clone()),
        verdict,
        amount: outcome
            .as_ref()
            .filter(|outcome| outcome.granted)
            .and_then(|outcome| outcome.amount),
        message: message.into(),
        manual,
        created_at: now.with_timezone(&Utc).to_rfc3339(),
    };
    run.id = repo::insert_benefit_run(pool, &run)
        .await
        .map_err(|_| "无法保存权益执行记录，请刷新平台状态确认领取结果".to_string())?;
    Ok(BenefitClaimResult { run, outcome })
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use axum::{
        body::Body,
        extract::Path,
        http::HeaderMap,
        response::Response,
        routing::{get, post},
        Json, Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TOKEN: &str = "offline-http-benefit-token-8217";

    struct MockServer {
        transport: HttpBenefitTransport,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn server(app: Router) -> MockServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        MockServer {
            transport: HttpBenefitTransport {
                mock_endpoint: Some(endpoint),
            },
            task,
        }
    }

    #[tokio::test]
    async fn real_http_mock_matches_hosts_paths_methods_and_headers() {
        let gets = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(AtomicUsize::new(0));
        let get_count = gets.clone();
        let post_count = posts.clone();
        let app=Router::new()
            .route("/sash/api/v1/me/campaigns",get(move |headers:HeaderMap| {
                let count=get_count.clone();async move {
                    assert_eq!(headers.get("host").unwrap(),"openapi.qoder.sh");
                    assert_eq!(headers.get("authorization").unwrap(),format!("Bearer {TOKEN}").as_str());
                    assert_eq!(headers.get("accept").unwrap(),"application/json");
                    assert!(!headers.contains_key("cosy-machinetoken"));
                    count.fetch_add(1,Ordering::SeqCst);
                    Json(serde_json::json!({"claimable":false,"campaigns":[
                        {"campaignId":"resident","campaignKey":"old","claimStatus":"CLAIMED","actionType":"CLAIM_BENEFIT"},
                        {"campaignId":"daily","campaignKey":"today","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT","startAt":1,"endAt":2,"benefit":{"kind":"CREDITS","amount":100}}
                    ]}))
                }
            }))
            .route("/sash/api/v1/me/campaigns/:id/claim",post(move |Path(id):Path<String>,headers:HeaderMap| {
                let count=post_count.clone();async move {
                    assert_eq!(id,"daily");assert_eq!(headers.get("host").unwrap(),"openapi.qoder.com.cn");
                    assert_eq!(headers.get("authorization").unwrap(),format!("Bearer {TOKEN}").as_str());
                    count.fetch_add(1,Ordering::SeqCst);
                    Json(serde_json::json!({"campaignId":"daily","campaignKey":"today","status":"CLAIMED","replayed":false,"benefit":{"kind":"CREDITS","amount":100}}))
                }
            }));
        let mock = server(app).await;
        let value = mock
            .transport
            .request(ClaimPlatform::Qoder, TOKEN, None, None)
            .await
            .unwrap();
        let status = parse_status(ClaimPlatform::Qoder, &value).unwrap();
        assert_eq!(status.next_claimable().unwrap().campaign_id, "daily");
        assert!(!status.warnings.is_empty());
        assert_eq!(posts.load(Ordering::SeqCst), 0);
        let value = mock
            .transport
            .request(
                ClaimPlatform::QoderCn,
                &format!("Bearer {TOKEN}"),
                None,
                Some("daily"),
            )
            .await
            .unwrap();
        assert!(
            parse_claim(ClaimPlatform::QoderCn, "daily", &value)
                .unwrap()
                .granted
        );
        assert_eq!(gets.load(Ordering::SeqCst), 1);
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        assert!(mock
            .transport
            .request(ClaimPlatform::Qoder, "", None, None)
            .await
            .is_err());
        assert!(mock
            .transport
            .request(ClaimPlatform::Qoder, "Bearer x\r\nHost: evil", None, None)
            .await
            .is_err());
        assert!(mock
            .transport
            .request(ClaimPlatform::Qoder, TOKEN, None, Some("../escape"))
            .await
            .is_err());
        assert_eq!(gets.load(Ordering::SeqCst), 1);
        assert_eq!(posts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn auth_errors_are_distinct_from_missing_route_and_hide_reflected_credentials() {
        let app=Router::new().route("/sash/api/v1/me/campaigns",get(|| async {
            (axum::http::StatusCode::UNAUTHORIZED,Json(serde_json::json!({"code":"TOKEN_INVALID","message":"missing authorization token"})))
        })).route("/sash/api/v1/me/campaigns/:id/claim",post(|Path(id):Path<String>| async move {
            if id=="missing" {
                (axum::http::StatusCode::NOT_FOUND,Json(serde_json::json!({"message":TOKEN})))
            } else {
                (axum::http::StatusCode::UNAUTHORIZED,Json(serde_json::json!({"message":format!("echo Bearer {TOKEN} https://private.invalid/path")})))
            }
        }));
        let mock = server(app).await;
        let denied = mock
            .transport
            .request(ClaimPlatform::Qoder, TOKEN, None, None)
            .await
            .unwrap_err();
        assert!(denied.contains("凭据") && denied.contains("missing authorization token"));
        let missing = mock
            .transport
            .request(ClaimPlatform::Qoder, TOKEN, None, Some("missing"))
            .await
            .unwrap_err();
        assert!(missing.contains("接口不存在"));
        assert!(!missing.contains("凭据被平台拒绝"));
        let reflected = mock
            .transport
            .request(ClaimPlatform::Qoder, TOKEN, None, Some("denied"))
            .await
            .unwrap_err();
        assert!(!reflected.contains(TOKEN));
        assert!(!reflected.contains("private.invalid"));
    }

    #[tokio::test]
    async fn redirects_are_not_followed_and_streaming_bodies_have_a_hard_limit() {
        let followed = Arc::new(AtomicUsize::new(0));
        let count = followed.clone();
        let app = Router::new()
            .route(
                "/sash/api/v1/me/campaigns",
                get(|| async {
                    Response::builder()
                        .status(302)
                        .header("location", "/redirected")
                        .body(Body::empty())
                        .unwrap()
                }),
            )
            .route(
                "/redirected",
                get(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({"campaigns":[]}))
                    }
                }),
            )
            .route(
                "/sash/api/v1/me/campaigns/:id/claim",
                post(|| async {
                    let chunks =
                        (0..20).map(|_| Ok::<Vec<u8>, std::io::Error>(vec![b'x'; 32 * 1024]));
                    Response::new(Body::from_stream(futures_util::stream::iter(chunks)))
                }),
            );
        let mock = server(app).await;
        let redirect = mock
            .transport
            .request(ClaimPlatform::Qoder, TOKEN, None, None)
            .await
            .unwrap_err();
        assert!(redirect.contains("HTTP 302"));
        assert_eq!(followed.load(Ordering::SeqCst), 0);
        let large = mock
            .transport
            .request(ClaimPlatform::Qoder, TOKEN, None, Some("daily"))
            .await
            .unwrap_err();
        assert!(large.contains("512 KiB"));
    }

    #[tokio::test]
    async fn successful_json_that_reflects_a_token_is_not_exposed() {
        let mock = server(Router::new().route(
            "/sash/api/v1/me/campaigns",
            get(|| async { Json(serde_json::json!({"campaigns":[],"debug":TOKEN})) }),
        ))
        .await;
        let error = mock
            .transport
            .request(ClaimPlatform::Qoder, TOKEN, None, None)
            .await
            .unwrap_err();
        assert!(error.contains("敏感凭据"));
        assert!(!error.contains(TOKEN));
    }
}
