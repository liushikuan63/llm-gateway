//! Only this application's encrypted store holds delegated login context.
//! Never reads another client's credential files or returns headers to the UI.
use crate::benefits::ClaimPlatform;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountSnapshot {
    pub display_name: Option<String>,
    pub masked_email: Option<String>,
    pub identity_hash: Option<String>,
    pub captured_at: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCredential {
    pub schema_version: u8,
    pub platform: ClaimPlatform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_source: Option<u8>,
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub snapshot: AccountSnapshot,
}

impl SessionCredential {
    pub fn has_trae_device_context(&self) -> bool {
        self.platform == ClaimPlatform::TraeCn
            && self
                .headers
                .get("x-device-id")
                .is_some_and(|value| value.len() == 16 && value.bytes().all(|b| b.is_ascii_digit()))
    }
    pub fn parse(raw: &str, platform: ClaimPlatform) -> Result<Self, String> {
        if raw.len() > 32768 {
            return Err("登录上下文过大".into());
        }
        let value: Self = serde_json::from_str(raw).map_err(|_| "登录上下文格式无效".to_owned())?;
        value.validate(platform)?;
        Ok(value)
    }
    pub fn validate(&self, platform: ClaimPlatform) -> Result<(), String> {
        if self.schema_version != 1 || self.platform != platform {
            return Err("登录上下文与所选平台不一致".into());
        }
        let trae = platform == ClaimPlatform::TraeCn;
        if (trae && !matches!(self.request_source, Some(1 | 2)))
            || (!trae && self.request_source.is_some())
        {
            return Err("登录上下文的客户端来源无效".into());
        }
        let prefix = if trae { "Cloud-IDE-JWT " } else { "Bearer " };
        if self.headers.len() > 9
            || !self
                .headers
                .get("authorization")
                .is_some_and(|v| v.starts_with(prefix) && v.len() > prefix.len())
        {
            return Err("登录上下文缺少有效授权".into());
        }
        for (key, value) in &self.headers {
            let allowed = if trae {
                matches!(
                    key.as_str(),
                    "authorization"
                        | "x-device-id"
                        | "x-device-brand"
                        | "x-device-type"
                        | "x-os-version"
                        | "x-app-version"
                )
            } else {
                matches!(
                    key.as_str(),
                    "authorization"
                        | "cosy-clienttype"
                        | "cosy-machinecode"
                        | "cosy-machinehostname"
                        | "cosy-machineid"
                        | "cosy-machineos"
                        | "cosy-machinetoken"
                        | "cosy-machinetype"
                        | "cosy-version"
                )
            };
            if !allowed {
                return Err("登录上下文包含不允许的请求头".into());
            }
            if value.len() > 8192
                || value.contains(['\r', '\n'])
                || reqwest::header::HeaderValue::from_str(value).is_err()
            {
                return Err("登录上下文请求头无效".into());
            }
        }
        for value in [
            &self.snapshot.display_name,
            &self.snapshot.masked_email,
            &self.snapshot.identity_hash,
            &self.snapshot.captured_at,
        ]
        .into_iter()
        .flatten()
        {
            if value.len() > 256
                || value.contains(['\r', '\n'])
                || value.contains("Bearer ")
                || value.contains("Cloud-IDE-JWT ")
                || value.starts_with("eyJ")
            {
                return Err("登录资料格式无效".into());
            }
        }
        Ok(())
    }
}

pub fn request_headers(
    raw: &str,
    platform: ClaimPlatform,
) -> Result<reqwest::header::HeaderMap, String> {
    let values = if raw.trim_start().starts_with('{') {
        SessionCredential::parse(raw, platform)?.headers
    } else {
        let authorization = crate::benefits::auth_value(raw);
        BTreeMap::from([("authorization".to_owned(), authorization)])
    };
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in values {
        let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
            .map_err(|_| "登录上下文请求头无效".to_owned())?;
        let mut value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| "访问 token 不是合法的 Authorization 值".to_owned())?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(headers)
}

pub fn contains_credential(body: &[u8], raw: &str, platform: ClaimPlatform) -> bool {
    let text = String::from_utf8_lossy(body);
    if raw.trim_start().starts_with('{') {
        SessionCredential::parse(raw, platform).map_or(true, |c| {
            c.headers
                .iter()
                .filter(|(key, _)| {
                    matches!(
                        key.as_str(),
                        "authorization" | "cosy-machinetoken" | "cosy-machinecode" | "x-device-id"
                    )
                })
                .any(|(key, value)| {
                    let bare = if key == "authorization" {
                        value
                            .strip_prefix("Bearer ")
                            .or_else(|| value.strip_prefix("Cloud-IDE-JWT "))
                            .unwrap_or(value)
                    } else {
                        value
                    };
                    bare.len() >= 8 && text.contains(bare)
                })
        })
    } else {
        let bare = raw
            .trim()
            .split_once(' ')
            .map_or(raw.trim(), |(_, value)| value);
        !bare.is_empty() && text.contains(bare)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountQuota {
    pub total: Option<f64>,
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    pub unit: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionAccountView {
    pub identity: AccountSnapshot,
    pub plan_name: Option<String>,
    pub quota: Option<AccountQuota>,
    pub addon_quota: Option<AccountQuota>,
    pub checked_at: String,
}

fn quota(v: &serde_json::Value) -> Option<AccountQuota> {
    if !v.is_object() {
        return None;
    }
    let number = |k: &str| v[k].as_f64().filter(|n| n.is_finite() && *n >= 0.);
    Some(AccountQuota {
        total: number("total"),
        used: number("used"),
        remaining: number("remaining"),
        unit: v["unit"]
            .as_str()
            .filter(|s| s.len() <= 32)
            .map(str::to_owned),
    })
}

pub async fn account_view(
    platform: ClaimPlatform,
    raw: &str,
    proxy: Option<&str>,
) -> Result<Option<SessionAccountView>, String> {
    if !raw.trim_start().starts_with('{') {
        return Ok(None);
    }
    let session = SessionCredential::parse(raw, platform)?;
    if platform == ClaimPlatform::TraeCn {
        // Native identity is validated during import; no unsupported quota endpoint is invented.
        return Ok(Some(SessionAccountView {
            identity: session.snapshot,
            plan_name: None,
            quota: None,
            addon_quota: None,
            checked_at: chrono::Utc::now().to_rfc3339(),
        }));
    }
    let client = crate::outbound::apply_proxy(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15)),
        proxy,
    )
    .map_err(|_| "账号代理配置无效".to_owned())?
    .build()
    .map_err(|_| "无法初始化账号查询".to_owned())?;
    let client = &client;
    let get = |route: &'static str| async move {
        let response = client
            .get(format!("https://{}{}", platform.host(), route))
            .headers(request_headers(raw, platform)?)
            .send()
            .await
            .map_err(|_| "账号查询失败或超时".to_owned())?;
        if !response.status().is_success() {
            return Err("账号查询失败，登录可能已过期".to_owned());
        }
        use futures_util::StreamExt;
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "账号响应读取失败".to_owned())?;
            if body.len() + chunk.len() > 512 * 1024 {
                return Err("账号响应过大".to_owned());
            }
            body.extend_from_slice(&chunk);
        }
        if contains_credential(&body, raw, platform) {
            return Err("账号响应包含凭据，已拒绝展示".to_owned());
        }
        serde_json::from_slice::<serde_json::Value>(&body)
            .map_err(|_| "账号响应格式无效".to_owned())
    };
    let (usage, plan) = tokio::join!(get("/sash/api/v2/me/usage"), get("/api/v2/user/plan"));
    let usage = usage?;
    let plan = plan?;
    Ok(Some(SessionAccountView {
        identity: session.snapshot,
        plan_name: plan["plan_tier_name"]
            .as_str()
            .filter(|s| s.len() <= 100)
            .map(str::to_owned),
        quota: quota(&usage["qoderUsage"]["userQuota"]),
        addon_quota: quota(&usage["qoderUsage"]["addOnQuota"]),
        checked_at: chrono::Utc::now().to_rfc3339(),
    }))
}
