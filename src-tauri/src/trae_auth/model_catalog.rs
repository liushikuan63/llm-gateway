//! Official CN Web Remote catalog declarations; never gateway model registration.
use super::*;
use futures_util::StreamExt;
use std::time::Duration;

const ORIGIN: &str = "https://trae-api-cn.mchost.guru";
const PATH: &str = "/api/remote/v1/models";
const FUNCTIONS: [&str; 3] = [
    "solo_agent_remote",
    "solo_work_remote",
    "solo_design_remote",
];
const BODY_LIMIT: usize = 1024 * 1024;
const INVALID: &str = "Trae模型目录响应格式无效";
const SESSION: &str = "Trae模型目录需要有效且未过期的本软件独立会话";

#[derive(Debug, Serialize)]
pub struct TraeModelCatalogView {
    pub account_id: String,
    pub region: &'static str,
    pub origin: &'static str,
    pub fetched_at_ms: i64,
    pub groups: Vec<TraeCatalogGroup>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct TraeCatalogGroup {
    pub r#function: &'static str,
    pub models: Vec<TraeCatalogModelView>,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct TraeCatalogModelView {
    pub id: String,
    pub name: String,
    pub supports_vision: Option<bool>,
    pub supports_thinking: Option<bool>,
}

// No Debug/Serialize: all fields remain within native memory.
struct CatalogSession {
    credential: Credential,
    generation: String,
    principal: String,
}
impl Core {
    async fn catalog_session(&self, account: &str) -> Result<CatalogSession, String> {
        if !id_valid(account) {
            return Err(SESSION.into());
        }
        let raw = self
            .read(&key("session", account))
            .await
            .map_err(|_| SESSION)?
            .ok_or(SESSION)?;
        let credential: Credential = decode(&raw).map_err(|_| SESSION)?;
        credential.validate().map_err(|_| SESSION)?;
        let uid = &credential.uid;
        if uid.is_empty()
            || uid.len() > 256
            || uid.bytes().all(|b| b == b'0')
            || !uid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || !credential
                .pair
                .access_expires_at_ms
                .is_some_and(|v| v > now())
        {
            return Err(SESSION.into());
        }
        let principal = format!("{:x}", Sha256::digest(format!("trae_cn:{uid}").as_bytes()));
        if credential.snapshot.identity_hash.as_deref() != Some(principal.as_str()) {
            return Err(SESSION.into());
        }
        Ok(CatalogSession {
            credential,
            generation: generation(&raw),
            principal,
        })
    }
    fn catalog_url(&self) -> Result<Url, String> {
        #[cfg(test)]
        if let Some(origin) = &self.catalog_test_origin {
            if origin.scheme() != "http" || origin.host_str() != Some("127.0.0.1") {
                return Err("Trae模型目录测试传输无效".into());
            }
            return origin.join(PATH).map_err(|_| INVALID.into());
        }
        Url::parse(ORIGIN)
            .and_then(|url| url.join(PATH))
            .map_err(|_| INVALID.into())
    }
}
impl TraeAuth {
    /// Manual single GET. No cache, refresh, login, provider registration or DB writes.
    pub async fn model_catalog(&self, account_id: &str) -> Result<TraeModelCatalogView, String> {
        let captured = {
            let _guard = self.core.gate.lock().await;
            self.core.catalog_session(account_id).await?
        };
        let client = self
            .core
            .http
            .read()
            .clone()
            .map_err(|_| "Trae模型目录代理配置无效")?;
        let url = self.core.catalog_url()?;
        let mut authorization = reqwest::header::HeaderValue::from_str(&format!(
            "Cloud-IDE-JWT {}",
            captured.credential.pair.access
        ))
        .map_err(|_| SESSION)?;
        authorization.set_sensitive(true);
        // Includes send and every body chunk; no per-chunk deadline reset.
        let bytes = tokio::time::timeout(Duration::from_secs(20), async {
            let response = client
                .get(url)
                .query(&[
                    ("functions", FUNCTIONS.join(",")),
                    ("show_custom_model", "false".into()),
                ])
                .header(reqwest::header::AUTHORIZATION, authorization)
                .header(reqwest::header::ACCEPT, "application/json")
                .header("X-Trae-Client-Type", "web")
                .header("X-Preferenced-Language", "zh-CN")
                .send()
                .await
                .map_err(|_| "Trae模型目录连接失败或超时")?;
            if response.status().as_u16() != 200 {
                return Err(format!(
                    "Trae模型目录返回HTTP{}",
                    response.status().as_u16()
                ));
            }
            if response
                .content_length()
                .is_some_and(|v| v > BODY_LIMIT as u64)
            {
                return Err("Trae模型目录响应超过上限".into());
            }
            let mut bytes = Vec::new();
            let mut chunks = response.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|error| {
                    if error.is_timeout() { "Trae\u{6a21}\u{578b}\u{76ee}\u{5f55}\u{8bfb}\u{53d6}\u{8d85}\u{65f6}" }
                    else { "Trae\u{6a21}\u{578b}\u{76ee}\u{5f55}\u{54cd}\u{5e94}\u{8bfb}\u{53d6}\u{5931}\u{8d25}" }
                })?;
                if bytes.len().saturating_add(chunk.len()) > BODY_LIMIT {
                    return Err("Trae模型目录响应超过上限".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, String>(bytes)
        })
        .await
        .map_err(|_| "Trae模型目录读取超时")??;
        let secrets = [
            captured.credential.pair.access.as_str(),
            captured.credential.pair.refresh.as_deref().unwrap_or(""),
            captured.credential.private_key.as_str(),
            captured.credential.public_key.as_str(),
            captured.credential.uid.as_str(),
            captured.credential.device.device_id.as_str(),
            captured
                .credential
                .device
                .machine_id
                .as_deref()
                .unwrap_or(""),
        ];
        if secrets
            .iter()
            .filter(|s| !s.is_empty())
            .any(|s| contains_bytes(&bytes, s.as_bytes()))
        {
            return Err("Trae模型目录响应包含敏感资料，已丢弃".into());
        }
        let packet: Value = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
        // Also catches escaped JSON strings and object keys.
        if reflects(&packet, &secrets) {
            return Err("Trae模型目录响应包含敏感资料，已丢弃".into());
        }
        let mut view = project(account_id, packet)?;
        {
            let _guard = self.core.gate.lock().await;
            let current = self.core.catalog_session(account_id).await?;
            if current.generation != captured.generation || current.principal != captured.principal
            {
                return Err("Trae会话已改变，模型目录结果已丢弃".into());
            }
        }
        view.fetched_at_ms = now();
        Ok(view)
    }
}
fn contains_bytes(bytes: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && bytes.windows(needle.len()).any(|v| v == needle)
}
fn reflects(value: &Value, secrets: &[&str]) -> bool {
    let matches = |v: &str| secrets.iter().any(|s| !s.is_empty() && v.contains(s));
    match value {
        Value::String(v) => matches(v),
        Value::Array(v) => v.iter().any(|v| reflects(v, secrets)),
        Value::Object(v) => v.iter().any(|(k, v)| matches(k) || reflects(v, secrets)),
        _ => false,
    }
}
fn safe_name(raw: &Value) -> Result<&str, String> {
    raw.as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= 256 && !s.chars().any(char::is_control))
        .ok_or_else(|| INVALID.into())
}
fn custom(model: &serde_json::Map<String, Value>) -> bool {
    ["ak", "sk", "base_url"].iter().any(|k| {
        model
            .get(*k)
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
    }) || model
        .get("client_connect")
        .is_some_and(|v| v.as_bool() != Some(false))
        || model
            .get("config_source")
            .is_some_and(|v| v.as_i64() != Some(1))
        || ["is_custom", "custom", "custom_model", "is_custom_model"]
            .iter()
            .any(|k| model.get(*k).is_some_and(|v| v.as_bool() != Some(false)))
}
fn project(account_id: &str, packet: Value) -> Result<TraeModelCatalogView, String> {
    if packet.get("code").and_then(Value::as_i64) != Some(0) {
        return Err("Trae模型目录服务未确认成功".into());
    }
    let list = packet
        .get("data")
        .and_then(|v| v.get("list"))
        .and_then(Value::as_array)
        .ok_or(INVALID)?;
    if list.len() > 16 {
        return Err("Trae模型目录分组超过上限".into());
    }
    let mut groups: [Vec<TraeCatalogModelView>; 3] = std::array::from_fn(|_| Vec::new());
    let mut present = [false; 3];
    let mut seen: [HashMap<&str, &serde_json::Map<String, Value>>; 3] =
        std::array::from_fn(|_| HashMap::new());
    let (mut total, mut unknown, mut excluded) = (0usize, 0usize, 0usize);
    for group in list {
        let group = group.as_object().ok_or(INVALID)?;
        let function = group
            .get("function")
            .and_then(Value::as_str)
            .ok_or(INVALID)?;
        safe_name(&Value::String(function.into()))?;
        let models = group
            .get("models")
            .and_then(Value::as_array)
            .ok_or(INVALID)?;
        total = total.saturating_add(models.len());
        if total > 512 {
            return Err("Trae模型目录条目超过上限".into());
        }
        let index = FUNCTIONS.iter().position(|v| *v == function);
        if let Some(index) = index {
            present[index] = true;
        } else {
            unknown += 1;
        }
        for raw in models {
            let raw = raw.as_object().ok_or(INVALID)?;
            let id = safe_name(raw.get("name").ok_or(INVALID)?)?;
            // Display-name semantics are not established by the captured consumer.
            // Its exact config_name (raw.name) is both ID and display label.
            let model = TraeCatalogModelView {
                id: id.into(),
                name: id.into(),
                supports_vision: raw.get("multimodal").and_then(Value::as_bool),
                supports_thinking: raw
                    .get("reasoning_effort_config")
                    .and_then(|v| v.get("support_thinking"))
                    .and_then(Value::as_bool),
            };
            if custom(raw) {
                excluded += 1;
                continue;
            }
            if let Some(index) = index {
                if let Some(prior) = seen[index].get(id) {
                    if *prior != raw {
                        return Err("Trae模型目录包含冲突条目".into());
                    }
                } else {
                    seen[index].insert(id, raw);
                    groups[index].push(model);
                }
            }
        }
    }
    let mut warnings = Vec::new();
    if unknown > 0 {
        warnings.push(format!("已排除{unknown}个未支持的目录分组"));
    }
    if excluded > 0 {
        warnings.push(format!("已排除{excluded}个自定义或来源不明条目"));
    }
    Ok(TraeModelCatalogView {
        account_id: account_id.into(),
        region: "cn",
        origin: ORIGIN,
        fetched_at_ms: now(),
        groups: groups
            .into_iter()
            .enumerate()
            .filter(|(i, _)| present[*i])
            .map(|(i, models)| TraeCatalogGroup {
                r#function: FUNCTIONS[i],
                models,
            })
            .collect(),
        warnings,
    })
}
#[cfg(test)]
mod tests;
