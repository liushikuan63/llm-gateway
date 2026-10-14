//! Purpose-separated encrypted COSY context tied to verified model credentials.
use super::*;
#[path = "model.rs"]
mod model;
pub(crate) use model::ModelAuthorization;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SigningContext {
    region: Region,
    issuer: String,
    device_generation: String,
    job_generation: String,
    uid: String,
    organization_id: Option<String>,
    organization_tags: Vec<String>,
    machine_id: String,
    data_policy_agreed: bool,
    access_expires_at_ms: Option<i64>,
    runtime_fields: Value,
}

#[derive(Debug, Serialize)]
pub struct SigningStatus {
    pub region: Region,
    pub account_id: String,
    pub prepared: bool,
    pub access_expires_at_ms: Option<i64>,
    pub inference_verified: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub format: String,
    pub enabled: Option<bool>,
    pub vision: Option<bool>,
    pub thinking: Option<bool>,
    pub free: Option<bool>,
    /// Vendor credit multiplier, never converted to currency without a price.
    pub credit_multiplier: Option<f64>,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

fn token_limit(value: &Value, field: &str) -> Result<Option<u32>, String> {
    let Some(raw) = value.get(field).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let value = raw
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or("Qoder目录Token上限无效")?;
    Ok((value > 0).then_some(value))
}

fn bool_alias(value: &Value, aliases: &[&str]) -> Result<Option<bool>, String> {
    let mut found = None;
    for alias in aliases {
        if let Some(raw) = value.get(*alias).filter(|v| !v.is_null()) {
            let current = raw.as_bool().ok_or("Qoder目录能力字段无效")?;
            if found.is_some_and(|old| old != current) {
                return Err("Qoder目录能力字段冲突".into());
            }
            found = Some(current);
        }
    }
    Ok(found)
}

fn parse_catalog(value: &Value, secrets: &[&str]) -> Result<Vec<CatalogModel>, String> {
    if reflects(value, secrets) {
        return Err("Qoder模型目录包含认证材料".into());
    }
    let list = value
        .get("assistant")
        .and_then(Value::as_array)
        .filter(|v| v.len() <= 512)
        .ok_or("Qoder模型目录缺少assistant场景列表")?;
    let mut ids = std::collections::HashSet::new();
    let mut result = Vec::new();
    for item in list {
        let text = |field: &str, limit: usize| -> Result<String, String> {
            item.get(field)
                .and_then(Value::as_str)
                .filter(|s| {
                    !s.trim().is_empty() && s.len() <= limit && !s.chars().any(char::is_control)
                })
                .map(str::to_owned)
                .ok_or_else(|| "Qoder目录模型字段无效".into())
        };
        let id = text("key", 256)?;
        if !ids.insert(id.clone()) {
            return Err("Qoder模型目录包含重复标识".into());
        }
        let mut credit_multiplier = None;
        for name in ["priceFactor", "price_factor"] {
            if let Some(raw) = item.get(name).filter(|v| !v.is_null()) {
                let multiplier = raw
                    .as_f64()
                    .or_else(|| raw.as_str().and_then(|v| v.parse().ok()))
                    .filter(|v: &f64| v.is_finite() && *v >= 0.0)
                    .ok_or("Qoder额度倍率无效")?;
                if credit_multiplier.is_some_and(|old| old != multiplier) {
                    return Err("Qoder额度倍率冲突".into());
                }
                credit_multiplier = Some(multiplier);
            }
        }
        result.push(CatalogModel {
            id,
            name: text("display_name", 256)?,
            format: text("format", 128)?,
            enabled: bool_alias(item, &["enable"])?,
            vision: bool_alias(item, &["is_vl", "isVl"])?,
            thinking: bool_alias(item, &["is_reasoning", "isReasoning"])?,
            free: bool_alias(item, &["isFree", "is_free"])?,
            credit_multiplier,
            context_window: token_limit(item, "max_input_tokens")?,
            max_output_tokens: token_limit(item, "max_output_tokens")?,
        });
    }
    Ok(result)
}

fn job_generation(job: &JobCredential) -> String {
    format!("{:x}", Sha256::digest(job.access.as_bytes()))
}

fn matches(
    context: &SigningContext,
    device: &Credential,
    job: &JobCredential,
) -> Result<bool, String> {
    Ok(context.region == device.region
        && context.issuer == device.issuer
        && context.device_generation == generation(device)?
        && context.job_generation == job_generation(job)
        && context.uid == device.uid
        && context.organization_id == device.organization
        && context.machine_id == device.installation_id
        && context.access_expires_at_ms == job.access_expires_at_ms)
}

fn tags(value: &Value, secrets: &[&str]) -> Result<Vec<String>, String> {
    if reflects(value, secrets) {
        return Err("Qoder组织响应包含凭据".into());
    }
    let value = data(value)?;
    let Some(raw) = value.get("tags").filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let array = raw
        .as_array()
        .filter(|a| a.len() <= 64)
        .ok_or("Qoder组织标签无效")?;
    array
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|v| {
                    !v.is_empty()
                        && v.len() <= 128
                        && v.bytes().all(|b| (0x20..=0x7e).contains(&b) && b != b',')
                })
                .map(str::to_owned)
                .ok_or_else(|| "Qoder组织标签无效".into())
        })
        .collect()
}

impl QoderAuth {
    /// One signed GET to the fixed regional catalog. Never sends model inference.
    pub async fn model_catalog(
        &self,
        region: Region,
        account: &str,
    ) -> Result<Vec<CatalogModel>, String> {
        self.model_signing_prepare(region, account).await?;
        let _guard = self.gate.lock().await;
        let (device_raw, device) = self.job_device(region, account).await?;
        let job_raw = self
            .read(&key(region, "job", account))
            .await?
            .ok_or("Qoder模型认证已取消")?;
        let job: JobCredential = decode(&job_raw)?;
        validate_job(&job, &device)?;
        let context_raw = self
            .read(&key(region, "job-signing", account))
            .await?
            .ok_or("Qoder签名上下文缺失")?;
        let context: SigningContext = decode(&context_raw)?;
        if !job.principal_verified
            || !matches(&context, &device, &job)?
            || device.access_expires_at_ms.is_some_and(|t| t <= now_ms())
            || job.access_expires_at_ms.is_some_and(|t| t <= now_ms())
        {
            return Err("Qoder目录认证未核验、改变或已到期".into());
        }
        let request = protocol::invoke(&json!({"operation":"prepare_catalog","context":{
            "region":region,"machine_id":context.machine_id,"uid":context.uid,"version":"1.0.51",
            "organization_id":context.organization_id,"organization_tags":context.organization_tags,
            "data_policy_agreed":context.data_policy_agreed,"runtime_fields":context.runtime_fields}})).await?;
        let expected = match region {
            Region::Global => "https://api2.qoder.sh/algo/api/v2/model/list?Encode=1",
            Region::Cn => "https://gateway.qoder.com.cn/algo/api/v2/model/list?Encode=1",
        };
        if request["url"].as_str() != Some(expected) {
            return Err("Qoder目录区域或地址无效".into());
        }
        let url = Url::parse(expected).map_err(|_| "Qoder目录地址无效")?;
        #[cfg(test)]
        let url = if let Some(origin) = &self.test_origin {
            let mut url = origin.clone();
            url.set_path(&format!("/{}/algo/api/v2/model/list", region.key()));
            url.set_query(Some("Encode=1"));
            url
        } else {
            url
        };
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, raw) in request["headers"]
            .as_object()
            .ok_or("Qoder目录签名头缺失")?
        {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "Qoder目录签名头无效")?;
            let mut value = HeaderValue::from_str(raw.as_str().ok_or("Qoder目录签名值无效")?)
                .map_err(|_| "Qoder目录签名值无效")?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        // The pipe may take time; do not dispatch a stale/cancelled credential.
        if self.read(&key(region, "session", account)).await?.as_ref() != Some(&device_raw)
            || self.read(&key(region, "job", account)).await?.as_ref() != Some(&job_raw)
            || device.access_expires_at_ms.is_some_and(|t| t <= now_ms())
            || job.access_expires_at_ms.is_some_and(|t| t <= now_ms())
        {
            return Err("Qoder目录认证已取消、改变或到期，未发送请求".into());
        }
        let response = self
            .client
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(|_| "Qoder目录请求失败或超时")?;
        if response.status().as_u16() != 200 {
            return Err(format!(
                "Qoder模型目录被拒绝（HTTP {}），未重试",
                response.status().as_u16()
            ));
        }
        let mut chunks = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| "Qoder目录响应读取失败")?;
            if bytes.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
                return Err("Qoder目录响应过大".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = std::str::from_utf8(&bytes).map_err(|_| "Qoder目录响应编码无效")?;
        let value = protocol::invoke(&json!({"operation":"decode_catalog","body":body})).await?;
        let mut secrets = vec![device.access.as_str(), job.access.as_str()];
        secrets.extend(device.refresh.as_deref());
        secrets.extend(job.refresh.as_deref());
        secrets.extend(context.runtime_fields.get("key").and_then(Value::as_str));
        secrets.extend(
            context
                .runtime_fields
                .get("encrypt_user_info")
                .and_then(Value::as_str),
        );
        for name in ["Authorization", "Cosy-Key"] {
            secrets.extend(request["headers"].get(name).and_then(Value::as_str));
        }
        if self.read(&key(region, "session", account)).await?.as_ref() != Some(&device_raw)
            || self.read(&key(region, "job", account)).await?.as_ref() != Some(&job_raw)
            || device.access_expires_at_ms.is_some_and(|t| t <= now_ms())
            || job.access_expires_at_ms.is_some_and(|t| t <= now_ms())
        {
            return Err("Qoder目录响应对应认证已改变或到期".into());
        }
        parse_catalog(&value, &secrets)
    }
    /// No login/refresh, inference, benefits or policy consent is implicit here.
    /// Only a safe status is public; signing materials stay in encrypted storage.
    pub async fn model_signing_prepare(
        &self,
        region: Region,
        account: &str,
    ) -> Result<SigningStatus, String> {
        let _guard = self.gate.lock().await;
        let (device_raw, device) = self.job_device(region, account).await?;
        let job_raw = self
            .read(&key(region, "job", account))
            .await?
            .ok_or("尚未准备Qoder模型认证")?;
        let job: JobCredential = decode(&job_raw)?;
        validate_job(&job, &device)?;
        if !job.principal_verified
            || device.access_expires_at_ms.is_some_and(|t| t <= now_ms())
            || job.access_expires_at_ms.is_some_and(|t| t <= now_ms())
        {
            return Err("Qoder认证未核验或已到期，请显式准备或刷新认证".into());
        }
        let attempt_raw = self.read(&key(region, "job-attempt", account)).await?;
        if let Some(raw) = &attempt_raw {
            let attempt: Attempt = decode(raw)?;
            if attempt.cancelled && attempt.generation == generation(&device)? {
                return Err("Qoder模型认证已取消".into());
            }
        }
        let context_name = key(region, "job-signing", account);
        #[cfg(test)]
        if let Some(pause) = &self.signing_cache_pause {
            pause.0.notify_one();
            pause.1.notified().await;
        }
        if let Some(raw) = self.read(&context_name).await? {
            let context: SigningContext = decode(&raw)?;
            if matches(&context, &device, &job)? {
                if device.access_expires_at_ms.is_some_and(|t| t <= now_ms())
                    || job.access_expires_at_ms.is_some_and(|t| t <= now_ms())
                {
                    return Err("Qoder缓存签名认证已到期".into());
                }
                return Ok(SigningStatus {
                    region,
                    account_id: account.into(),
                    prepared: true,
                    access_expires_at_ms: job.access_expires_at_ms,
                    inference_verified: false,
                });
            }
        }
        let mut secrets = vec![device.access.as_str(), job.access.as_str()];
        secrets.extend(device.refresh.as_deref());
        secrets.extend(job.refresh.as_deref());
        let (http, profile) = self
            .request(
                region,
                Method::GET,
                "/api/v1/userinfo",
                &[],
                Some(&job.access),
                None,
            )
            .await?;
        if http != 200 {
            return Err(format!("Qoder模型账号核验被拒绝（HTTP {http}）"));
        }
        let mut checked = device.clone();
        apply_profile(&mut checked, &profile, &secrets)?;
        if checked.uid != device.uid || checked.organization != device.organization {
            return Err("Qoder签名账号或组织发生变化".into());
        }
        let organization_tags = if let Some(organization) = &device.organization {
            let mut url = Url::parse(region.api()).map_err(|_| "Qoder组织域名无效")?;
            url.path_segments_mut()
                .map_err(|_| "Qoder组织路径无效")?
                .extend(["api", "v1", "organizations", organization, "tags"]);
            let (http, response) = self
                .request(
                    region,
                    Method::GET,
                    url.path(),
                    &[],
                    Some(&job.access),
                    None,
                )
                .await?;
            if http != 200 {
                return Err(format!("Qoder组织标签被拒绝（HTTP {http}）"));
            }
            tags(&response, &secrets)?
        } else {
            Vec::new()
        };
        // Privacy consent is never inferred from account membership or a missing field.
        let raw = serde_json::to_string(&json!({"uid":device.uid,"security_oauth_token":job.access,
            "organization_id":device.organization,"organization_tags":organization_tags,"data_policy_agreed":false}))
            .map_err(|_| "Qoder签名上下文编码失败")?;
        let fields = protocol::invoke(&json!({"operation":"runtime_fields","raw":raw})).await?;
        let context = SigningContext {
            region,
            issuer: device.issuer.clone(),
            device_generation: generation(&device)?,
            job_generation: job_generation(&job),
            uid: device.uid.clone(),
            organization_id: device.organization.clone(),
            organization_tags,
            machine_id: device.installation_id.clone(),
            data_policy_agreed: false,
            access_expires_at_ms: job.access_expires_at_ms,
            runtime_fields: fields,
        };
        #[cfg(test)]
        if let Some(pause) = &self.signing_commit_pause {
            pause.0.notify_one();
            pause.1.notified().await;
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "无法保存Qoder签名上下文")?;
        // Acquire the SQLite writer before checking snapshots; cancellation and
        // refresh in another service/process cannot race a late publication.
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name=?")
            .bind(key(region, "session", account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法核验签名认证代次")?;
        for (name, expected) in [
            (key(region, "session", account), Some(&device_raw)),
            (key(region, "job", account), Some(&job_raw)),
            (key(region, "job-attempt", account), attempt_raw.as_ref()),
        ] {
            let current: Option<String> =
                sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|_| "无法核验签名认证代次")?;
            if current.as_ref() != expected {
                return Err("Qoder签名认证已取消或改变，未保存旧上下文".into());
            }
        }
        if device.access_expires_at_ms.is_some_and(|t| t <= now_ms())
            || job.access_expires_at_ms.is_some_and(|t| t <= now_ms())
        {
            return Err("Qoder签名认证已到期，未保存上下文".into());
        }
        sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET value_enc=excluded.value_enc,updated_at=excluded.updated_at")
            .bind(context_name).bind(encode(&context)?).bind(chrono::Utc::now()).bind(chrono::Utc::now())
            .execute(&mut *tx).await.map_err(|_| "无法保存Qoder签名上下文")?;
        tx.commit().await.map_err(|_| "无法提交Qoder签名上下文")?;
        Ok(SigningStatus {
            region,
            account_id: account.into(),
            prepared: true,
            access_expires_at_ms: job.access_expires_at_ms,
            inference_verified: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn organization_tags_reject_invalid_values_and_reflected_credentials() {
        assert_eq!(
            tags(
                &json!({"tags":["tag-one","tag-two"]}),
                &["synthetic-secret"]
            )
            .unwrap(),
            vec!["tag-one", "tag-two"]
        );
        assert!(tags(&json!({"tags":["one,two"]}), &[]).is_err());
        assert!(tags(&json!({"tags":["synthetic-secret"]}), &["synthetic-secret"]).is_err());
        assert!(tags(&json!({"tags":[42]}), &[]).is_err());
        assert!(tags(&json!({"code":1,"tags":[]}), &[]).is_err());
        assert!(tags(&json!({}), &[]).unwrap().is_empty());
    }
    #[test]
    fn catalog_metadata_preserves_unknowns_and_rejects_malformed_or_sensitive_rows() {
        let row = json!({"key":"model","display_name":"Model","format":"openai"});
        let result = parse_catalog(&json!({"assistant":[row.clone()]}), &[]).unwrap();
        assert_eq!(result[0].thinking, None);
        assert_eq!(result[0].vision, None);
        assert_eq!(result[0].credit_multiplier, None);
        assert!(parse_catalog(&json!({"assistant":[row.clone(),row.clone()]}), &[]).is_err());
        assert!(parse_catalog(&json!({"models":[row.clone()]}), &[]).is_err());
        let mut bad = row.clone();
        bad["display_name"] = json!("private-synthetic-token");
        assert!(parse_catalog(&json!({"assistant":[bad]}), &["private-synthetic-token"]).is_err());
        let mut bad = row.clone();
        bad["is_reasoning"] = json!("true");
        assert!(parse_catalog(&json!({"assistant":[bad]}), &[]).is_err());
        let mut bad = row.clone();
        bad["priceFactor"] = json!(1);
        bad["price_factor"] = json!(2);
        assert!(parse_catalog(&json!({"assistant":[bad]}), &[]).is_err());
        let mut bad = row;
        bad["priceFactor"] = json!(-1);
        assert!(parse_catalog(&json!({"assistant":[bad]}), &[]).is_err());
        assert!(parse_catalog(&json!({"assistant":[]}), &[])
            .unwrap()
            .is_empty());
    }
    #[test]
    fn catalog_token_limits_preserve_declared_bounds_without_guessing_or_overflow() {
        let row = json!({"key":"synthetic-model","display_name":"Synthetic","format":"openai","max_input_tokens":128000,"max_output_tokens":8192});
        let result = parse_catalog(&json!({"assistant":[row.clone()]}), &[]).unwrap();
        assert_eq!(
            (result[0].context_window, result[0].max_output_tokens),
            (Some(128000), Some(8192))
        );
        for value in [json!(null), json!(0)] {
            let mut unknown = row.clone();
            unknown["max_input_tokens"] = value.clone();
            unknown["max_output_tokens"] = value;
            let result = parse_catalog(&json!({"assistant":[unknown]}), &[]).unwrap();
            assert_eq!(
                (result[0].context_window, result[0].max_output_tokens),
                (None, None)
            );
        }
        for field in ["max_input_tokens", "max_output_tokens"] {
            for invalid in [json!(-1), json!(4294967296u64), json!(1.5), json!("8192")] {
                let mut bad = row.clone();
                bad[field] = invalid;
                assert!(parse_catalog(&json!({"assistant":[bad]}), &[]).is_err());
            }
        }
    }
}
