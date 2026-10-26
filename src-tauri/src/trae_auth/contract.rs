use crate::client_session::AccountSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) const ISSUER: &str = "https://api.trae.cn";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CnOrigin {
    TraeCn,
    TraeComCn,
}
impl CnOrigin {
    pub(super) fn from_issuer(raw: &str) -> Result<Self, String> {
        match raw {
            ISSUER => Ok(Self::TraeCn),
            "https://api.trae.com.cn" => Ok(Self::TraeComCn),
            _ => Err("Trae认证来源无效".into()),
        }
    }
    pub(super) fn issuer(self) -> &'static str {
        match self {
            Self::TraeCn => ISSUER,
            Self::TraeComCn => "https://api.trae.com.cn",
        }
    }
}
pub(super) const CLIENT_ID: &str = "ono9krqynydwx5";
pub(super) const VERSION: &str = "3.4.2";
pub(super) const EXCHANGE: &str = "/trae/api/v3/oauth/ExchangeToken";
pub(super) const PRINCIPAL: &str = "/cloudide/api/v3/trae/GetUserInfo";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Device {
    pub device_id: String,
    pub machine_id: Option<String>,
    pub device_name: String,
    pub device_model: String,
    pub device_brand: String,
    pub device_cpu: String,
    pub os_info: String,
    pub os_version: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Material {
    pub code_verifier: String,
    pub code_challenge: String,
    pub challenge_method: String,
    pub private_key: String,
    pub public_key: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pair {
    pub access: String,
    pub refresh: Option<String>,
    pub access_expires_at_ms: Option<i64>,
    pub refresh_expires_at_ms: Option<i64>,
}
pub(super) struct Identity {
    pub uid: String,
    pub snapshot: AccountSnapshot,
}

pub(super) fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32768
        && !value
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        && reqwest::header::HeaderValue::from_str(value).is_ok()
}
impl Device {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.device_id.len() != 16
            || !self.device_id.bytes().all(|v| v.is_ascii_digit())
            || self.device_id.bytes().all(|v| v == b'0')
        {
            return Err("Trae需要本软件保存的真实设备上下文".into());
        }
        if self
            .machine_id
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 128 || v.chars().any(char::is_control))
        {
            return Err("Trae设备上下文无效".into());
        }
        for value in [
            &self.device_name,
            &self.device_model,
            &self.device_brand,
            &self.device_cpu,
            &self.os_info,
            &self.os_version,
        ] {
            if value.len() > 1024 || value.chars().any(char::is_control) {
                return Err("Trae设备上下文无效".into());
            }
        }
        Ok(())
    }
    pub(super) fn wire(&self, public_key: &str) -> Result<Value, String> {
        self.validate()?;
        if public_key.is_empty() || public_key.len() > 4096 {
            return Err("Trae设备公钥无效".into());
        }
        let mut value = json!({"DeviceID":self.device_id,"PlatformCode":"IDE_PC","DeviceType":"PC","DeviceName":self.device_name,"DeviceModel":self.device_model,"ClientVersion":VERSION,"DevicePublicKey":public_key,"DeviceBrand":self.device_brand,"DeviceCPU":self.device_cpu,"OSInfo":self.os_info,"OSVersion":self.os_version});
        if let Some(machine) = &self.machine_id {
            value["MachineID"] = json!(machine);
        }
        Ok(value)
    }
}
pub(super) fn auth_code_body(
    code: &str,
    material: &Material,
    device: &Device,
) -> Result<Value, String> {
    if !valid_token(code)
        || material.code_verifier.len() != 64
        || !material
            .code_verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err("Trae授权码或PKCE验证器无效".into());
    }
    Ok(
        json!({"ClientID":CLIENT_ID,"AuthCode":code,"CodeVerifier":material.code_verifier,"DeviceInfo":device.wire(&material.public_key)?,"IDEVersion":VERSION}),
    )
}
fn result(packet: &Value) -> Result<&Value, String> {
    if let Some(metadata) = packet.get("ResponseMetadata").filter(|v| !v.is_null()) {
        if !metadata.is_object() || metadata.get("Error").is_some_and(|v| !v.is_null()) {
            return Err("Trae认证服务未确认成功".into());
        }
    }
    packet
        .get("Result")
        .filter(|v| v.is_object())
        .ok_or_else(|| "Trae认证响应缺少有效结果".into())
}
fn date(value: Option<&Value>) -> Result<Option<i64>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let timestamp = if let Some(n) = value.as_i64() {
        if !(1_000_000_000_000..=8_640_000_000_000_000).contains(&n) {
            return Err("Trae令牌到期时间单位无效".into());
        }
        n
    } else if let Some(text) = value.as_str() {
        chrono::DateTime::parse_from_rfc3339(text)
            .map_err(|_| "Trae令牌到期时间无效")?
            .timestamp_millis()
    } else {
        return Err("Trae令牌到期时间无效".into());
    };
    if chrono::DateTime::from_timestamp_millis(timestamp).is_none() {
        return Err("Trae令牌到期时间超出范围".into());
    }
    Ok(Some(timestamp))
}
pub(super) fn parse_pair(
    packet: &Value,
    previous: Option<&Pair>,
    now: i64,
) -> Result<Pair, String> {
    let data = result(packet)?;
    let access = data
        .get("Token")
        .and_then(Value::as_str)
        .filter(|v| valid_token(v))
        .ok_or("Trae认证响应缺少有效令牌")?
        .to_owned();
    let refresh = match data.get("RefreshToken").filter(|v| !v.is_null()) {
        Some(value) => Some(
            value
                .as_str()
                .filter(|v| valid_token(v))
                .ok_or("Trae刷新令牌无效")?
                .to_owned(),
        ),
        None => previous.and_then(|p| p.refresh.clone()),
    };
    let mut access_expiry = date(data.get("TokenExpireAt"))?;
    let duration = data
        .get("TokenExpireDuration")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_i64()
                .filter(|v| *v > 0)
                .ok_or("Trae令牌相对有效期无效")
        })
        .transpose()?;
    // The current native xPe helper treats TokenExpireDuration as milliseconds.
    if access_expiry.is_none() || access_expiry.is_some_and(|v| v < now) {
        if let Some(duration) = duration {
            access_expiry = Some(
                now.checked_add(duration)
                    .filter(|v| chrono::DateTime::from_timestamp_millis(*v).is_some())
                    .ok_or("Trae令牌有效期超出范围")?,
            );
        }
    }
    if access_expiry.is_none() {
        access_expiry = previous
            .filter(|p| p.access == access)
            .and_then(|p| p.access_expires_at_ms);
    }
    let mut refresh_expiry = date(data.get("RefreshExpireAt"))?;
    if refresh_expiry.is_none() {
        refresh_expiry = previous
            .filter(|p| p.refresh == refresh)
            .and_then(|p| p.refresh_expires_at_ms);
    }
    Ok(Pair {
        access,
        refresh,
        access_expires_at_ms: access_expiry,
        refresh_expires_at_ms: refresh_expiry,
    })
}
pub(super) fn reflects(value: &Value, secrets: &[&str]) -> bool {
    match value {
        Value::String(s) => secrets.iter().any(|v| !v.is_empty() && s.contains(v)),
        Value::Array(values) => values.iter().any(|v| reflects(v, secrets)),
        Value::Object(values) => values.values().any(|v| reflects(v, secrets)),
        _ => false,
    }
}
pub(super) fn identity(packet: &Value, secrets: &[&str]) -> Result<Identity, String> {
    if reflects(packet, secrets) {
        return Err("Trae账号响应包含认证材料".into());
    }
    let data = result(packet)?;
    let uid = data
        .get("UserID")
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 256
                && *s != "0"
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        })
        .ok_or("Trae账号主体未核验")?
        .to_owned();
    let name = data
        .get("ScreenName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        .map(str::to_owned);
    let email = data
        .get("NonPlainTextEmail")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        .map(|s| {
            if s.contains('*') {
                s.to_owned()
            } else {
                crate::crypto::mask(s)
            }
        });
    let snapshot = AccountSnapshot {
        display_name: name,
        masked_email: email,
        identity_hash: Some(format!(
            "{:x}",
            Sha256::digest(format!("trae_cn:{uid}").as_bytes())
        )),
        captured_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    Ok(Identity { uid, snapshot })
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn device() -> Device {
        Device {
            device_id: "1234567890123456".into(),
            machine_id: Some("a".repeat(64)),
            device_name: "Synthetic test machine".into(),
            device_model: "Synthetic".into(),
            device_brand: "Synthetic".into(),
            device_cpu: "Synthetic CPU".into(),
            os_info: "Windows".into(),
            os_version: "Synthetic version".into(),
        }
    }
    fn material() -> Material {
        Material {
            code_verifier: "a".repeat(64),
            code_challenge: "b".repeat(43),
            challenge_method: "S256".into(),
            private_key: "synthetic-private-material".into(),
            public_key: "synthetic-public-material".into(),
        }
    }
    #[test]
    fn code_exchange_only_sends_public_device_key_and_fixed_official_identity() {
        let result = auth_code_body("synthetic-code", &material(), &device()).unwrap();
        assert_eq!(result["ClientID"], CLIENT_ID);
        assert_eq!(result["AuthCode"], "synthetic-code");
        assert_eq!(result["CodeVerifier"], "a".repeat(64));
        assert_eq!(result["DeviceInfo"]["PlatformCode"], "IDE_PC");
        assert_eq!(result["DeviceInfo"]["DeviceType"], "PC");
        assert_eq!(result["DeviceInfo"]["DeviceID"], "1234567890123456");
        assert_eq!(
            result["DeviceInfo"]["DevicePublicKey"],
            "synthetic-public-material"
        );
        assert!(!result.to_string().contains("synthetic-private-material"));
        assert!(result.get("host").is_none());
        assert!(result.get("DeviceProof").is_none());
    }
    #[test]
    fn token_dates_use_public_epoch_milliseconds_and_relative_milliseconds() {
        let now = 1_790_000_000_000;
        let first=parse_pair(&json!({"Result":{"Token":"synthetic-access","RefreshToken":"synthetic-refresh","TokenExpireAt":now+1000,"RefreshExpireAt":now+2000}}),None,now).unwrap();
        assert_eq!(
            (first.access_expires_at_ms, first.refresh_expires_at_ms),
            (Some(now + 1000), Some(now + 2000))
        );
        let fallback=parse_pair(&json!({"Result":{"Token":"synthetic-new-access","TokenExpireAt":now-1000,"TokenExpireDuration":5000}}),Some(&first),now).unwrap();
        assert_eq!(fallback.access_expires_at_ms, Some(now + 5000));
        assert_eq!(fallback.refresh.as_deref(), Some("synthetic-refresh"));
        assert_eq!(fallback.refresh_expires_at_ms, first.refresh_expires_at_ms);
        let new_refresh=parse_pair(&json!({"Result":{"Token":"synthetic-new-access","RefreshToken":"synthetic-rotated-refresh"}}),Some(&first),now).unwrap();
        assert_eq!(new_refresh.refresh_expires_at_ms, None);
        assert_eq!(new_refresh.access_expires_at_ms, None);
    }
    #[test]
    fn malformed_token_result_business_error_and_unknown_units_are_rejected() {
        for packet in [
            json!({"Token":"synthetic"}),
            json!({"Result":{}}),
            json!({"ResponseMetadata":{"Error":{"Code":"synthetic"}},"Result":{"Token":"synthetic"}}),
            json!({"Result":{"Token":"bad\nvalue"}}),
            json!({"Result":{"Token":"synthetic","TokenExpireAt":1790000000}}),
            json!({"Result":{"Token":"synthetic","TokenExpireAt":1790000000000i64,"TokenExpireDuration":-1}}),
        ] {
            assert!(parse_pair(&packet, None, 1_790_000_000_000).is_err());
        }
    }
    #[test]
    fn principal_is_the_verified_server_user_id_and_does_not_expose_raw_email() {
        let data=identity(&json!({"Result":{"UserID":"synthetic-owner","ScreenName":"Synthetic","NonPlainTextEmail":"s***@example.invalid"}}),&["synthetic-private-material"]).unwrap();
        assert_eq!(data.uid, "synthetic-owner");
        assert_eq!(
            data.snapshot.identity_hash,
            Some(format!("{:x}", Sha256::digest(b"trae_cn:synthetic-owner")))
        );
        assert_eq!(
            data.snapshot.masked_email.as_deref(),
            Some("s***@example.invalid")
        );
        assert!(identity(&json!({"Result":{"UserID":"synthetic-owner","ScreenName":"synthetic-private-material"}}),&["synthetic-private-material"]).is_err());
        assert!(identity(&json!({"UserID":"synthetic-owner"}), &[]).is_err());
    }
}
