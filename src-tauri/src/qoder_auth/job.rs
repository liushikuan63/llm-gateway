//! Model credentials have their own expiry and lifecycle, separate from device login.
use super::*;

#[path = "signing.rs"]
mod signing;
pub(crate) use signing::ModelAuthorization;
pub use signing::{CatalogModel, SigningStatus};

#[derive(Debug, Serialize)]
pub struct JobStatus {
    pub status: String,
    pub region: Region,
    pub account_id: String,
    pub access_expires_at_ms: Option<i64>,
    pub refresh_expires_at_ms: Option<i64>,
    pub can_refresh: bool,
    pub attempt_blocked: bool,
    pub inference_verified: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobCredential {
    region: Region,
    issuer: String,
    device_generation: String,
    access: String,
    refresh: Option<String>,
    access_expires_at_ms: Option<i64>,
    refresh_expires_at_ms: Option<i64>,
    #[serde(default)]
    principal_verified: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    generation: String,
    operation: String,
    id: String,
    deadline_ms: i64,
    cancelled: bool,
}

fn generation(device: &Credential) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(
        device.region,
        &device.issuer,
        &device.uid,
        &device.organization,
        &device.installation_id,
        &device.access,
    ))
    .map_err(|_| "Qoder认证代次编码失败")?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn job_expiry(value: &Value, refresh: bool) -> Result<Option<i64>, String> {
    let (absolute, alternate, relative) = if refresh {
        (
            "refresh_token_expires_at",
            "refresh_token_expire_time",
            "refresh_token_expires_in",
        )
    } else {
        ("expires_at", "expire_time", "expires_in")
    };
    let mut result = None;
    for field in [absolute, alternate] {
        if let Some(raw) = value.get(field).filter(|v| !v.is_null()) {
            let millis = if let Some(number) = raw.as_i64() {
                number
            } else {
                chrono::DateTime::parse_from_rfc3339(raw.as_str().ok_or("Qoder模型有效期无效")?)
                    .map_err(|_| "Qoder模型有效期无效")?
                    .timestamp_millis()
            };
            if millis <= 0
                || chrono::DateTime::from_timestamp_millis(millis).is_none()
                || result.is_some_and(|old| old != millis)
            {
                return Err("Qoder模型有效期冲突或超出范围".into());
            }
            result = Some(millis);
        }
    }
    if result.is_some() {
        return Ok(result);
    }
    if let Some(raw) = value.get(relative).filter(|v| !v.is_null()) {
        let millis = raw
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or("Qoder模型相对有效期无效")?;
        let time = now_ms()
            .checked_add(millis)
            .ok_or("Qoder模型有效期超出范围")?;
        if chrono::DateTime::from_timestamp_millis(time).is_none() {
            return Err("Qoder模型有效期超出范围".into());
        }
        return Ok(Some(time));
    }
    Ok(None)
}

fn validate_job(job: &JobCredential, device: &Credential) -> Result<(), String> {
    if job.region != device.region
        || job.issuer != device.issuer
        || job.device_generation != generation(device)?
        || !valid_token(&job.access)
        || job.refresh.as_deref().is_some_and(|r| !valid_token(r))
    {
        return Err("Qoder模型认证用途、区域或设备代次无效".into());
    }
    Ok(())
}

fn parse_job(
    device: &Credential,
    value: &Value,
    old: Option<&JobCredential>,
) -> Result<JobCredential, String> {
    let value = data(value)?;
    let access =
        text_alias(value, &["token", "access_token"], true)?.ok_or("Qoder模型认证缺少令牌")?;
    let fresh_refresh = text_alias(value, &["refresh_token"], false)?;
    if access == device.access
        || device.refresh.as_deref() == Some(&access)
        || fresh_refresh
            .as_ref()
            .is_some_and(|r| r == &device.access || Some(r) == device.refresh.as_ref())
        || value
            .get("user_id")
            .is_some_and(|v| !v.is_null() && profile_text(v) != Some(&device.uid))
    {
        return Err("Qoder模型认证与设备用途或账号不一致".into());
    }
    let retained = fresh_refresh.is_none();
    let job = JobCredential {
        region: device.region,
        issuer: device.issuer.clone(),
        device_generation: generation(device)?,
        access,
        refresh: fresh_refresh.or_else(|| old.and_then(|j| j.refresh.clone())),
        access_expires_at_ms: job_expiry(value, false)?,
        refresh_expires_at_ms: job_expiry(value, true)?.or_else(|| {
            retained
                .then(|| old.and_then(|j| j.refresh_expires_at_ms))
                .flatten()
        }),
        principal_verified: false,
    };
    validate_job(&job, device)?;
    if old.is_some_and(|old| old.access == job.access) {
        return Err("Qoder模型刷新未返回新认证".into());
    }
    if job.access_expires_at_ms.is_some_and(|t| t <= now_ms()) {
        return Err("Qoder模型返回已到期认证".into());
    }
    Ok(job)
}

fn operation(device_generation: &str, old: Option<&JobCredential>) -> String {
    let mut digest = Sha256::new();
    digest.update(device_generation.as_bytes());
    if let Some(old) = old {
        digest.update(b":refresh:");
        digest.update(old.access.as_bytes());
    } else {
        digest.update(b":issue");
    }
    format!("{:x}", digest.finalize())
}

impl QoderAuth {
    async fn job_device(
        &self,
        region: Region,
        account: &str,
    ) -> Result<(String, Credential), String> {
        if !valid_account(account) {
            return Err("本机账号名称无效".into());
        }
        let raw = self
            .read(&key(region, "session", account))
            .await?
            .ok_or("尚未完成Qoder独立登录")?;
        let device: Credential = decode(&raw)?;
        validate(&device, region)?;
        Ok((raw, device))
    }

    /// Local status only. Prepared credentials do not prove successful inference.
    pub async fn job_status(&self, region: Region, account: &str) -> Result<JobStatus, String> {
        if !valid_account(account) {
            return Err("本机账号名称无效".into());
        }
        let device = self.read(&key(region, "session", account)).await?;
        let device: Option<Credential> = device.as_deref().map(decode).transpose()?;
        if let Some(device) = &device {
            validate(device, region)?;
        }
        let raw = self.read(&key(region, "job", account)).await?;
        let job: Option<JobCredential> = raw.as_deref().map(decode).transpose()?;
        let matching = job.as_ref().filter(|job| {
            device
                .as_ref()
                .is_some_and(|d| validate_job(job, d).is_ok())
        });
        let attempt: Option<Attempt> = self
            .read(&key(region, "job-attempt", account))
            .await?
            .as_deref()
            .map(decode)
            .transpose()?;
        let blocked = if let (Some(device), Some(attempt)) = (&device, &attempt) {
            attempt.generation == generation(device)?
                && (attempt.cancelled
                    || attempt.operation == operation(&generation(device)?, matching))
        } else {
            false
        };
        Ok(JobStatus {
            status: if device.is_none() {
                "not_logged_in"
            } else if matching.is_none() {
                if job.is_some() {
                    "stale"
                } else {
                    "not_prepared"
                }
            } else if matching
                .is_some_and(|j| j.access_expires_at_ms.is_some_and(|t| t <= now_ms()))
            {
                "expired"
            } else if matching.is_some_and(|j| !j.principal_verified) {
                "verification_required"
            } else {
                "prepared"
            }
            .into(),
            region,
            account_id: account.into(),
            access_expires_at_ms: matching.and_then(|j| j.access_expires_at_ms),
            refresh_expires_at_ms: matching.and_then(|j| j.refresh_expires_at_ms),
            can_refresh: matching.is_some_and(|j| {
                j.refresh.is_some()
                    && !blocked
                    && !j.refresh_expires_at_ms.is_some_and(|t| t <= now_ms())
            }),
            attempt_blocked: blocked,
            inference_verified: false,
        })
    }

    pub async fn job_prepare(&self, region: Region, account: &str) -> Result<JobStatus, String> {
        self.job_update(region, account, false).await
    }
    pub async fn job_refresh(&self, region: Region, account: &str) -> Result<JobStatus, String> {
        self.job_update(region, account, true).await
    }

    async fn job_update(
        &self,
        region: Region,
        account: &str,
        force: bool,
    ) -> Result<JobStatus, String> {
        // Device refresh has its own gate. Never enter it while holding this gate.
        let (_, device) = self.job_device(region, account).await?;
        let initial_job = self.read(&key(region, "job", account)).await?;
        if !force
            && device
                .access_expires_at_ms
                .is_some_and(|t| t <= now_ms().saturating_add(60_000))
        {
            self.refresh(region, account).await?;
        }
        let _guard = self.gate.lock().await;
        let (baseline, device) = self.job_device(region, account).await?;
        if device.access_expires_at_ms.is_some_and(|t| t <= now_ms()) {
            return Err("Qoder设备认证已到期".into());
        }
        let job_name = key(region, "job", account);
        let old_raw = self.read(&job_name).await?;
        let stored: Option<JobCredential> = old_raw.as_deref().map(decode).transpose()?;
        let old = stored.as_ref().filter(|j| validate_job(j, &device).is_ok());
        if force
            && initial_job.is_some()
            && initial_job != old_raw
            && old.is_some_and(|j| {
                j.principal_verified
                    && !j
                        .access_expires_at_ms
                        .is_some_and(|t| t <= now_ms().saturating_add(60_000))
            })
        {
            return self.job_status(region, account).await;
        }
        if !force
            && old.is_some_and(|j| {
                j.principal_verified
                    && !j
                        .access_expires_at_ms
                        .is_some_and(|t| t <= now_ms().saturating_add(60_000))
            })
        {
            return self.job_status(region, account).await;
        }
        if force && old.is_none() {
            return Err("没有可刷新的本代Qoder模型认证".into());
        }
        let refresh = if let Some(old) = old {
            if old.refresh_expires_at_ms.is_some_and(|t| t <= now_ms()) {
                return Err("Qoder模型刷新凭据已到期，请重新登录".into());
            }
            Some(
                old.refresh
                    .as_deref()
                    .ok_or("Qoder模型没有刷新凭据，请重新登录")?,
            )
        } else {
            None
        };
        let gen = generation(&device)?;
        let op = operation(&gen, old);
        let attempt_name = key(region, "job-attempt", account);
        let previous = self.read(&attempt_name).await?;
        if let Some(raw) = &previous {
            let attempt: Attempt = decode(raw)?;
            if attempt.generation == gen && (attempt.operation == op || attempt.cancelled) {
                return Err("此代模型认证已尝试或取消，不自动重放，请重新登录".into());
            }
        }
        let attempt = Attempt {
            generation: gen,
            operation: op,
            id: uuid::Uuid::new_v4().to_string(),
            deadline_ms: now_ms().saturating_add(WAIT_MS),
            cancelled: false,
        };
        let reserved = encode(&attempt)?;
        let session_name = key(region, "session", account);
        // The first write obtains SQLite's reservation; CAS also serializes
        // independent services/processes that don't share the in-memory gate.
        #[cfg(test)]
        if let Some(pause) = &self.job_reserve_pause {
            pause.0.notify_one();
            pause.1.notified().await;
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "无法预约Qoder模型认证")?;
        let changed = if let Some(previous) = &previous {
            sqlx::query(
                "UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=? AND value_enc=?",
            )
            .bind(&reserved)
            .bind(chrono::Utc::now())
            .bind(&attempt_name)
            .bind(previous)
            .execute(&mut *tx)
            .await
        } else {
            sqlx::query(
                "INSERT OR IGNORE INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?)",
            )
            .bind(&attempt_name)
            .bind(&reserved)
            .bind(chrono::Utc::now())
            .bind(chrono::Utc::now())
            .execute(&mut *tx)
            .await
        }
        .map_err(|_| "无法预约Qoder模型认证")?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(&session_name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法核验设备认证代次")?;
        let current_job: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(&job_name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法核验模型认证代次")?;
        if changed.rows_affected() != 1
            || current.as_deref() != Some(&baseline)
            || current_job != old_raw
            || device.access_expires_at_ms.is_some_and(|t| t <= now_ms())
            || now_ms() >= attempt.deadline_ms
            || old.is_some_and(|j| j.refresh_expires_at_ms.is_some_and(|t| t <= now_ms()))
        {
            return Err("Qoder模型认证预约发生竞争或设备已到期，未发送请求".into());
        }
        tx.commit().await.map_err(|_| "无法提交Qoder模型认证预约")?;
        #[cfg(test)]
        if let Some(pause) = &self.job_commit_pause {
            pause.0.notify_one();
            pause.1.notified().await;
        }
        let active_attempt = self.read(&attempt_name).await?;
        if active_attempt.as_deref() != Some(&reserved)
            || now_ms() >= attempt.deadline_ms
            || device.access_expires_at_ms.is_some_and(|t| t <= now_ms())
            || old.is_some_and(|j| j.refresh_expires_at_ms.is_some_and(|t| t <= now_ms()))
        {
            return Err("Qoder模型认证预约已取消或到期，未发送请求".into());
        }
        let (path, body, bearer) = if let Some(refresh) = refresh {
            (
                "/api/v1/jobToken/refresh",
                json!({"refresh_token":refresh}),
                None,
            )
        } else {
            (
                "/api/v1/me/jobToken",
                json!({"clientId":CLIENT_ID}),
                Some(device.access.as_str()),
            )
        };
        let (http, value) = self
            .request(region, Method::POST, path, &[], bearer, Some(body))
            .await?;
        if http != 200 {
            return Err(format!(
                "Qoder模型认证被拒绝（HTTP {http}），不重放且保留原认证"
            ));
        }
        let mut job = parse_job(&device, &value, old)?;
        let mut checked = device.clone();
        let mut secrets = vec![device.access.as_str(), job.access.as_str()];
        secrets.extend(device.refresh.as_deref());
        secrets.extend(job.refresh.as_deref());
        if let Some(old) = old {
            secrets.push(&old.access);
            secrets.extend(old.refresh.as_deref());
        }
        // Verify the new model token itself. The device token would only prove
        // the existing device principal, even if the issued job token is foreign.
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
            return Err("Qoder模型认证账号核验失败，原认证保留".into());
        }
        apply_profile(&mut checked, &profile, &secrets)?;
        if checked.uid != device.uid || checked.organization != device.organization {
            return Err("Qoder模型认证账号或组织发生变化".into());
        }
        job.principal_verified = true;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "无法保存Qoder模型认证")?;
        // Reacquire writer and verify cancellation/generations after waiting.
        let changed =
            sqlx::query("UPDATE app_secrets SET updated_at=? WHERE name=? AND value_enc=?")
                .bind(chrono::Utc::now())
                .bind(&attempt_name)
                .bind(&reserved)
                .execute(&mut *tx)
                .await
                .map_err(|_| "无法核验模型认证预约")?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(&session_name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法核验设备认证代次")?;
        let current_job: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(&job_name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法核验模型认证代次")?;
        if changed.rows_affected() != 1
            || current.as_deref() != Some(&baseline)
            || current_job != old_raw
            || now_ms() >= attempt.deadline_ms
            || job.access_expires_at_ms.is_some_and(|t| t <= now_ms())
            || device.access_expires_at_ms.is_some_and(|t| t <= now_ms())
        {
            return Err("Qoder模型认证已取消、改变或到期，原资料保留".into());
        }
        sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET value_enc=excluded.value_enc,updated_at=excluded.updated_at")
            .bind(&job_name).bind(encode(&job)?).bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(&mut *tx).await
            .map_err(|_| "无法保存Qoder模型认证")?;
        tx.commit().await.map_err(|_| "无法提交Qoder模型认证")?;
        self.job_status(region, account).await
    }

    /// Cancellation leaves a tombstone; an unknown request cannot be replayed.
    pub async fn job_cancel(&self, region: Region, account: &str) -> Result<(), String> {
        let (baseline, device) = self.job_device(region, account).await?;
        let gen = generation(&device)?;
        let tombstone = Attempt {
            generation: gen.clone(),
            operation: operation(&gen, None),
            id: uuid::Uuid::new_v4().to_string(),
            deadline_ms: now_ms(),
            cancelled: true,
        };
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| "无法取消Qoder模型认证")?;
        sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET value_enc=excluded.value_enc,updated_at=excluded.updated_at")
            .bind(key(region, "job-attempt", account)).bind(encode(&tombstone)?).bind(chrono::Utc::now()).bind(chrono::Utc::now())
            .execute(&mut *tx).await.map_err(|_| "无法取消Qoder模型认证")?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(key(region, "session", account))
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| "无法核验取消认证的账号代次")?;
        if current.as_deref() != Some(&baseline) {
            return Err("Qoder账号已变化，未取消新代认证".into());
        }
        sqlx::query("DELETE FROM app_secrets WHERE name=?")
            .bind(key(region, "job", account))
            .execute(&mut *tx)
            .await
            .map_err(|_| "无法取消Qoder模型认证")?;
        tx.commit().await.map_err(|_| "无法提交Qoder模型认证取消")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use axum::{
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Json, Router,
    };
    use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU8, AtomicUsize, Ordering};

    struct Mock {
        region: Region,
        issues: AtomicUsize,
        refreshes: AtomicUsize,
        profiles: AtomicUsize,
        tag_requests: AtomicUsize,
        catalog_requests: AtomicUsize,
        catalog_http: AtomicU16,
        reflect_signing: AtomicU8,
        catalog: std::sync::Mutex<Value>,
        code: AtomicU16,
        issue: std::sync::Mutex<Value>,
        refresh: std::sync::Mutex<Value>,
        profile: std::sync::Mutex<Value>,
        wrong_job_owner: AtomicBool,
        hold: AtomicBool,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
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
    async fn issue(
        State(m): State<Arc<Mock>>,
        Path(region): Path<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        assert_eq!(region, m.region.key());
        assert_eq!(headers["authorization"], "Bearer device-access");
        assert_eq!(body, json!({"clientId":CLIENT_ID}));
        m.issues.fetch_add(1, Ordering::SeqCst);
        if m.hold.load(Ordering::SeqCst) {
            m.entered.notify_one();
            m.release.notified().await;
        }
        (
            StatusCode::from_u16(m.code.load(Ordering::SeqCst)).unwrap(),
            Json(m.issue.lock().unwrap().clone()),
        )
    }
    async fn refresh(
        State(m): State<Arc<Mock>>,
        Path(region): Path<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        assert_eq!(region, m.region.key());
        assert!(headers.get("authorization").is_none());
        assert_eq!(body, json!({"refresh_token":"job-refresh-old"}));
        m.refreshes.fetch_add(1, Ordering::SeqCst);
        if m.hold.load(Ordering::SeqCst) {
            m.entered.notify_one();
            m.release.notified().await;
        }
        (
            StatusCode::from_u16(m.code.load(Ordering::SeqCst)).unwrap(),
            Json(m.refresh.lock().unwrap().clone()),
        )
    }
    async fn profile(
        State(m): State<Arc<Mock>>,
        Path(region): Path<String>,
        headers: HeaderMap,
    ) -> Json<Value> {
        assert_eq!(region, m.region.key());
        let bearer = headers["authorization"].to_str().unwrap();
        assert!(matches!(
            bearer,
            "Bearer job-access-old" | "Bearer job-access-new"
        ));
        m.profiles.fetch_add(1, Ordering::SeqCst);
        if m.wrong_job_owner.load(Ordering::SeqCst) {
            return Json(json!({"uid":"different-account","organization_id":"org-1"}));
        }
        Json(m.profile.lock().unwrap().clone())
    }
    async fn organization_tags(
        State(m): State<Arc<Mock>>,
        Path((region, organization)): Path<(String, String)>,
        headers: HeaderMap,
    ) -> Json<Value> {
        assert_eq!(region, m.region.key());
        assert_eq!(organization, "org-1");
        assert_eq!(headers["authorization"], "Bearer job-access-old");
        m.tag_requests.fetch_add(1, Ordering::SeqCst);
        Json(json!({"tags":["synthetic-organization-tag"]}))
    }
    async fn catalog(
        State(m): State<Arc<Mock>>,
        Path(region): Path<String>,
        axum::extract::Query(query): axum::extract::Query<
            std::collections::HashMap<String, String>,
        >,
        headers: HeaderMap,
    ) -> (StatusCode, Json<Value>) {
        assert_eq!(region, m.region.key());
        assert_eq!(query.get("Encode").map(String::as_str), Some("1"));
        assert_eq!(query.len(), 1);
        assert!(headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer COSY."));
        assert_eq!(headers["cosy-user"], "account-1");
        assert_eq!(headers["cosy-organization-id"], "org-1");
        assert_eq!(
            headers["cosy-organization-tags"],
            "synthetic-organization-tag"
        );
        assert_eq!(headers["cosy-data-policy"], "disagree");
        m.catalog_requests.fetch_add(1, Ordering::SeqCst);
        let mut body = m.catalog.lock().unwrap().clone();
        match m.reflect_signing.load(Ordering::SeqCst) {
            1 => {
                body["assistant"][0]["display_name"] = json!(headers["cosy-key"].to_str().unwrap())
            }
            2 => body["unexpected"] = json!(headers["authorization"].to_str().unwrap()),
            3 => {
                let auth = headers["authorization"].to_str().unwrap();
                let payload = auth
                    .strip_prefix("Bearer COSY.")
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap();
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .unwrap();
                let info: Value = serde_json::from_slice(&decoded).unwrap();
                body["unexpected"] = info["info"].clone();
            }
            _ => {}
        }
        (
            StatusCode::from_u16(m.catalog_http.load(Ordering::SeqCst)).unwrap(),
            Json(body),
        )
    }
    async fn fixture(region: Region) -> Fixture {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            base64::engine::general_purpose::STANDARD.encode([23_u8; 32]),
        );
        let db = Db::connect_in_memory().await.unwrap();
        let auth = QoderAuth::new(db.pool().clone(), None).unwrap();
        let device = Credential {
            region,
            issuer: region.api().into(),
            access: "device-access".into(),
            refresh: Some("device-refresh".into()),
            uid: "account-1".into(),
            organization: Some("org-1".into()),
            installation_id: uuid::Uuid::new_v4().to_string(),
            access_expires_at_ms: Some(now_ms() + 3_600_000),
            refresh_expires_at_ms: None,
            snapshot: AccountSnapshot::default(),
            refresh_attempted: false,
        };
        repo::set_secret(
            &auth.pool,
            &key(region, "session", "main"),
            &encode(&device).unwrap(),
        )
        .await
        .unwrap();
        let m = Arc::new(Mock {
            region,
            issues: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
            profiles: AtomicUsize::new(0),
            tag_requests: AtomicUsize::new(0),
            catalog_requests: AtomicUsize::new(0),
            catalog_http: AtomicU16::new(200),
            reflect_signing: AtomicU8::new(0),
            catalog: std::sync::Mutex::new(
                json!({"assistant":[{"key":"synthetic-model","display_name":"Synthetic model","format":"openai","is_reasoning":true,"is_vl":false,"enable":true,"isFree":false,"priceFactor":1.5}]}),
            ),
            code: AtomicU16::new(200),
            issue: std::sync::Mutex::new(
                json!({"token":"job-access-old","refresh_token":"job-refresh-old","expires_in":3_600_000,"refresh_token_expires_in":7_200_000}),
            ),
            refresh: std::sync::Mutex::new(
                json!({"token":"job-access-new","refresh_token":"job-refresh-new","expires_in":3_600_000}),
            ),
            profile: std::sync::Mutex::new(json!({"uid":"account-1","organization_id":"org-1"})),
            wrong_job_owner: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let app = Router::new()
            .route("/:region/api/v1/me/jobToken", post(issue))
            .route("/:region/api/v1/jobToken/refresh", post(refresh))
            .route("/:region/api/v1/userinfo", get(profile))
            .route(
                "/:region/api/v1/organizations/:organization/tags",
                get(organization_tags),
            )
            .route("/:region/algo/api/v2/model/list", get(catalog))
            .with_state(m.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut auth = auth;
        auth.test_origin = Some(Url::parse(&format!("http://{address}")).unwrap());
        Fixture {
            auth,
            mock: m,
            server,
        }
    }

    #[tokio::test]
    async fn signing_context_is_private_cached_and_bound_to_both_regions() {
        for region in [Region::Global, Region::Cn] {
            let f = fixture(region).await;
            assert!(f.auth.model_signing_prepare(region, "main").await.is_err());
            assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 0);
            f.auth.job_prepare(region, "main").await.unwrap();
            let status = f.auth.model_signing_prepare(region, "main").await.unwrap();
            assert!(status.prepared);
            assert!(!status.inference_verified);
            let public = serde_json::to_string(&status).unwrap();
            assert!(!public.contains("job-access-old"));
            assert!(!public.contains("runtime_fields"));
            let encrypted = f
                .auth
                .read(&key(region, "job-signing", "main"))
                .await
                .unwrap()
                .unwrap();
            assert!(!encrypted.contains("job-access-old"));
            assert!(!encrypted.contains("synthetic-organization-tag"));
            f.auth.model_signing_prepare(region, "main").await.unwrap();
            assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
            assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 0);
            assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 2);
            assert_eq!(f.mock.tag_requests.load(Ordering::SeqCst), 1);
            f.auth.job_cancel(region, "main").await.unwrap();
            assert!(f.auth.model_signing_prepare(region, "main").await.is_err());
            assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 2);
            assert_eq!(f.mock.tag_requests.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn signing_context_rejects_unverified_expired_and_changed_principals() {
        let f = fixture(Region::Global).await;
        f.auth.job_prepare(Region::Global, "main").await.unwrap();
        let baseline = f
            .auth
            .read(&key(Region::Global, "job", "main"))
            .await
            .unwrap()
            .unwrap();
        let mut job: JobCredential = decode(&baseline).unwrap();
        job.principal_verified = false;
        repo::set_secret(
            &f.auth.pool,
            &key(Region::Global, "job", "main"),
            &encode(&job).unwrap(),
        )
        .await
        .unwrap();
        assert!(f
            .auth
            .model_signing_prepare(Region::Global, "main")
            .await
            .is_err());
        job.principal_verified = true;
        job.access_expires_at_ms = Some(now_ms() - 1);
        repo::set_secret(
            &f.auth.pool,
            &key(Region::Global, "job", "main"),
            &encode(&job).unwrap(),
        )
        .await
        .unwrap();
        assert!(f
            .auth
            .model_signing_prepare(Region::Global, "main")
            .await
            .is_err());
        assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 1);
        repo::set_secret(&f.auth.pool, &key(Region::Global, "job", "main"), &baseline)
            .await
            .unwrap();
        f.mock.wrong_job_owner.store(true, Ordering::SeqCst);
        assert!(f
            .auth
            .model_signing_prepare(Region::Global, "main")
            .await
            .is_err());
        assert!(f
            .auth
            .read(&key(Region::Global, "job-signing", "main"))
            .await
            .unwrap()
            .is_none());
        assert_eq!(f.mock.tag_requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancellation_prevents_late_signing_context_publication() {
        let mut f = fixture(Region::Cn).await;
        f.auth.job_prepare(Region::Cn, "main").await.unwrap();
        let pause = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        f.auth.signing_commit_pause = Some(pause.clone());
        let auth = f.auth.clone();
        let pending =
            tokio::spawn(async move { auth.model_signing_prepare(Region::Cn, "main").await });
        tokio::time::timeout(Duration::from_secs(10), pause.0.notified())
            .await
            .unwrap();
        f.auth.job_cancel(Region::Cn, "main").await.unwrap();
        pause.1.notify_one();
        assert!(pending.await.unwrap().is_err());
        assert!(f
            .auth
            .read(&key(Region::Cn, "job-signing", "main"))
            .await
            .unwrap()
            .is_none());
        assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
        assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 2);
        assert_eq!(f.mock.tag_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cached_signing_context_expiring_while_waiting_for_connection_is_rejected() {
        let mut f = fixture(Region::Global).await;
        f.auth.job_prepare(Region::Global, "main").await.unwrap();
        let mut job = saved(&f).await;
        job.access_expires_at_ms = Some(now_ms() + 2000);
        repo::set_secret(
            &f.auth.pool,
            &key(Region::Global, "job", "main"),
            &encode(&job).unwrap(),
        )
        .await
        .unwrap();
        f.auth
            .model_signing_prepare(Region::Global, "main")
            .await
            .unwrap();
        let pause = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        f.auth.signing_cache_pause = Some(pause.clone());
        let auth = f.auth.clone();
        let pending =
            tokio::spawn(async move { auth.model_signing_prepare(Region::Global, "main").await });
        tokio::time::timeout(Duration::from_secs(5), pause.0.notified())
            .await
            .unwrap();
        let connection = f.auth.pool.acquire().await.unwrap();
        pause.1.notify_one();
        while now_ms() <= job.access_expires_at_ms.unwrap() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!pending.is_finished(), "对照：签名缓存确实在等数据库连接");
        drop(connection);
        assert!(
            pending.await.unwrap().is_err(),
            "连接恢复后必须拒绝已到期缓存"
        );
        assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 2);
        assert_eq!(f.mock.tag_requests.load(Ordering::SeqCst), 1);
        assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn native_catalog_signs_both_regions_and_returns_only_model_metadata() {
        for region in [Region::Global, Region::Cn] {
            let f = fixture(region).await;
            assert!(f.auth.model_catalog(region, "main").await.is_err());
            assert_eq!(f.mock.catalog_requests.load(Ordering::SeqCst), 0);
            f.auth.job_prepare(region, "main").await.unwrap();
            let models = f.auth.model_catalog(region, "main").await.unwrap();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].id, "synthetic-model");
            assert_eq!(models[0].thinking, Some(true));
            assert_eq!(models[0].vision, Some(false));
            assert_eq!(models[0].free, Some(false));
            assert_eq!(models[0].credit_multiplier, Some(1.5));
            assert!(!serde_json::to_string(&models)
                .unwrap()
                .contains("job-access"));
            assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 2);
            assert_eq!(f.mock.tag_requests.load(Ordering::SeqCst), 1);
            assert_eq!(f.mock.catalog_requests.load(Ordering::SeqCst), 1);
            assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
            assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 0);
            f.mock.catalog_http.store(429, Ordering::SeqCst);
            assert!(f
                .auth
                .model_catalog(region, "main")
                .await
                .unwrap_err()
                .contains("HTTP 429"));
            assert_eq!(
                f.mock.catalog_requests.load(Ordering::SeqCst),
                2,
                "目录被拒绝只发送一次且不重试"
            );
            assert_eq!(
                f.mock.profiles.load(Ordering::SeqCst),
                2,
                "第二次目录复用签名上下文"
            );
        }
    }

    #[tokio::test]
    async fn catalog_rejects_reflected_rsa_key_ciphertext_and_authorization() {
        let f = fixture(Region::Global).await;
        f.auth.job_prepare(Region::Global, "main").await.unwrap();
        assert_eq!(
            f.auth
                .model_catalog(Region::Global, "main")
                .await
                .unwrap()
                .len(),
            1
        );
        for field in 1..=3 {
            f.mock.reflect_signing.store(field, Ordering::SeqCst);
            let error = f
                .auth
                .model_catalog(Region::Global, "main")
                .await
                .unwrap_err();
            assert_eq!(error, "Qoder模型目录包含认证材料");
        }
        assert_eq!(f.mock.catalog_requests.load(Ordering::SeqCst), 4);
        assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 2);
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 0);
    }
    async fn saved(f: &Fixture) -> JobCredential {
        decode(
            &f.auth
                .read(&key(f.mock.region, "job", "main"))
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn issued_and_refreshed_job_tokens_must_prove_the_device_principal() {
        for region in [Region::Global, Region::Cn] {
            let f = fixture(region).await;
            f.mock.wrong_job_owner.store(true, Ordering::SeqCst);
            assert!(f
                .auth
                .job_prepare(region, "main")
                .await
                .unwrap_err()
                .contains("账号或组织发生变化"));
            assert!(f
                .auth
                .read(&key(region, "job", "main"))
                .await
                .unwrap()
                .is_none());
            assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
            assert!(f.auth.job_prepare(region, "main").await.is_err());
            assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);

            let f = fixture(region).await;
            f.auth.job_prepare(region, "main").await.unwrap();
            let old = f.auth.read(&key(region, "job", "main")).await.unwrap();
            f.mock.wrong_job_owner.store(true, Ordering::SeqCst);
            assert!(f
                .auth
                .job_refresh(region, "main")
                .await
                .unwrap_err()
                .contains("账号或组织发生变化"));
            assert_eq!(f.auth.read(&key(region, "job", "main")).await.unwrap(), old);
            assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
            assert!(f.auth.job_refresh(region, "main").await.is_err());
            assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn legacy_job_records_require_principal_verification_before_cache_reuse() {
        for region in [Region::Global, Region::Cn] {
            for wrong_owner in [false, true] {
                let f = fixture(region).await;
                f.auth.job_prepare(region, "main").await.unwrap();
                let mut legacy = serde_json::to_value(saved(&f).await).unwrap();
                legacy.as_object_mut().unwrap().remove("principal_verified");
                let raw = encode(&legacy).unwrap();
                repo::set_secret(&f.auth.pool, &key(region, "job", "main"), &raw)
                    .await
                    .unwrap();
                assert_eq!(
                    f.auth.job_status(region, "main").await.unwrap().status,
                    "verification_required"
                );
                f.mock.wrong_job_owner.store(wrong_owner, Ordering::SeqCst);
                let result = f.auth.job_prepare(region, "main").await;
                assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
                assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
                assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 2);
                if wrong_owner {
                    assert!(result.unwrap_err().contains("账号或组织发生变化"));
                    assert_eq!(
                        f.auth
                            .read(&key(region, "job", "main"))
                            .await
                            .unwrap()
                            .as_deref(),
                        Some(raw.as_str())
                    );
                    assert!(f.auth.job_prepare(region, "main").await.is_err());
                    assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
                } else {
                    assert_eq!(result.unwrap().status, "prepared");
                    assert_eq!(
                        f.auth.job_prepare(region, "main").await.unwrap().status,
                        "prepared"
                    );
                    assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
                    assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
                    assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 2);
                }
            }
        }
    }

    #[tokio::test]
    async fn both_regions_issue_refresh_and_cache_without_exposing_credentials() {
        for region in [Region::Global, Region::Cn] {
            let f = fixture(region).await;
            assert_eq!(
                f.auth.job_status(region, "main").await.unwrap().status,
                "not_prepared"
            );
            assert_eq!(f.mock.issues.load(Ordering::SeqCst), 0);
            let status = f.auth.job_prepare(region, "main").await.unwrap();
            assert_eq!(status.status, "prepared");
            assert!(!status.inference_verified);
            assert!(status.can_refresh);
            let public = serde_json::to_string(&status).unwrap();
            assert!(!public.contains("job-access-old"));
            assert!(!public.contains("job-refresh-old"));
            assert!(!public.contains("device-access"));
            assert_eq!(
                f.auth.job_prepare(region, "main").await.unwrap().status,
                "prepared"
            );
            assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
            assert_eq!(f.mock.profiles.load(Ordering::SeqCst), 1);
            assert_eq!(
                f.auth.job_refresh(region, "main").await.unwrap().status,
                "prepared"
            );
            let job = saved(&f).await;
            assert_eq!(job.access, "job-access-new");
            assert_eq!(job.refresh.as_deref(), Some("job-refresh-new"));
            assert_eq!(job.refresh_expires_at_ms, None);
            assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
            let raw = f
                .auth
                .read(&key(region, "job", "main"))
                .await
                .unwrap()
                .unwrap();
            assert!(!raw.contains("job-access-new"));
        }
    }

    #[tokio::test]
    async fn failure_retains_pair_and_blocks_replay_across_independent_services() {
        for is_refresh in [false, true] {
            let f = fixture(Region::Global).await;
            let old = if is_refresh {
                f.auth.job_prepare(Region::Global, "main").await.unwrap();
                f.auth
                    .read(&key(Region::Global, "job", "main"))
                    .await
                    .unwrap()
            } else {
                None
            };
            f.mock.code.store(502, Ordering::SeqCst);
            let fail = if is_refresh {
                f.auth.job_refresh(Region::Global, "main").await
            } else {
                f.auth.job_prepare(Region::Global, "main").await
            };
            assert!(fail.unwrap_err().contains("HTTP 502"));
            let mut other = QoderAuth::new(f.auth.pool.clone(), None).unwrap();
            other.test_origin = f.auth.test_origin.clone();
            let retry = if is_refresh {
                other.job_refresh(Region::Global, "main").await
            } else {
                other.job_prepare(Region::Global, "main").await
            };
            assert!(retry.unwrap_err().contains("不自动重放"));
            assert_eq!(
                f.auth
                    .read(&key(Region::Global, "job", "main"))
                    .await
                    .unwrap(),
                old
            );
            assert!(
                f.auth
                    .job_status(Region::Global, "main")
                    .await
                    .unwrap()
                    .attempt_blocked
            );
            assert_eq!(
                f.mock.refreshes.load(Ordering::SeqCst),
                usize::from(is_refresh)
            );
            assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn cancellation_during_http_prevents_late_save_and_same_generation_replay() {
        let f = fixture(Region::Global).await;
        f.mock.hold.store(true, Ordering::SeqCst);
        let auth = f.auth.clone();
        let task = tokio::spawn(async move { auth.job_prepare(Region::Global, "main").await });
        tokio::time::timeout(Duration::from_secs(5), f.mock.entered.notified())
            .await
            .unwrap();
        f.auth.job_cancel(Region::Global, "main").await.unwrap();
        f.mock.release.notify_one();
        assert!(task.await.unwrap().is_err());
        assert!(f
            .auth
            .read(&key(Region::Global, "job", "main"))
            .await
            .unwrap()
            .is_none());
        assert!(f.auth.job_prepare(Region::Global, "main").await.is_err());
        assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn device_relogin_during_http_cannot_be_overwritten() {
        let f = fixture(Region::Global).await;
        f.mock.hold.store(true, Ordering::SeqCst);
        let auth = f.auth.clone();
        let task = tokio::spawn(async move { auth.job_prepare(Region::Global, "main").await });
        tokio::time::timeout(Duration::from_secs(5), f.mock.entered.notified())
            .await
            .unwrap();
        let (_, mut device) = f.auth.job_device(Region::Global, "main").await.unwrap();
        device.access = "device-access-new".into();
        let new = encode(&device).unwrap();
        repo::set_secret(&f.auth.pool, &key(Region::Global, "session", "main"), &new)
            .await
            .unwrap();
        f.mock.release.notify_one();
        assert!(task.await.unwrap().is_err());
        assert_eq!(
            f.auth
                .read(&key(Region::Global, "session", "main"))
                .await
                .unwrap()
                .unwrap(),
            new
        );
        assert!(f
            .auth
            .read(&key(Region::Global, "job", "main"))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn simultaneous_prepare_is_single_flight_and_refresh_is_explicit() {
        let f = fixture(Region::Global).await;
        let (a, b) = tokio::join!(
            f.auth.job_prepare(Region::Global, "main"),
            f.auth.job_prepare(Region::Global, "main")
        );
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 0);
        assert!(f.auth.job_refresh(Region::Global, "main").await.is_ok());
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn independent_service_cannot_issue_while_reservation_is_in_flight() {
        let f = fixture(Region::Global).await;
        f.mock.hold.store(true, Ordering::SeqCst);
        let auth = f.auth.clone();
        let a = tokio::spawn(async move { auth.job_prepare(Region::Global, "main").await });
        tokio::time::timeout(Duration::from_secs(5), f.mock.entered.notified())
            .await
            .unwrap();
        let mut other = QoderAuth::new(f.auth.pool.clone(), None).unwrap();
        other.test_origin = f.auth.test_origin.clone();
        assert!(other.job_prepare(Region::Global, "main").await.is_err());
        assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
        f.mock.release.notify_one();
        assert!(a.await.unwrap().is_ok());
        assert_eq!(
            other
                .job_prepare(Region::Global, "main")
                .await
                .unwrap()
                .status,
            "prepared"
        );
        assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn wrong_principal_reflected_secret_expired_or_wrong_purpose_never_save() {
        for variant in 0..5 {
            let f = fixture(Region::Global).await;
            match variant {
                0 => f.mock.profile.lock().unwrap()["uid"] = json!("other"),
                1 => f.mock.profile.lock().unwrap()["name"] = json!("job-access-old"),
                2 => f.mock.issue.lock().unwrap()["expires_at"] = json!(now_ms() - 1),
                3 => f.mock.issue.lock().unwrap()["token"] = json!("device-access"),
                _ => f.mock.issue.lock().unwrap()["user_id"] = json!("other"),
            }
            assert!(f.auth.job_prepare(Region::Global, "main").await.is_err());
            assert!(f
                .auth
                .read(&key(Region::Global, "job", "main"))
                .await
                .unwrap()
                .is_none());
            assert!(f.auth.job_prepare(Region::Global, "main").await.is_err());
            assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn region_missing_account_expired_refresh_and_invalid_account_fail_without_http() {
        let f = fixture(Region::Global).await;
        assert!(f.auth.job_prepare(Region::Cn, "main").await.is_err());
        assert!(f
            .auth
            .job_prepare(Region::Global, "bad/account")
            .await
            .is_err());
        assert_eq!(
            f.auth
                .job_status(Region::Global, "missing")
                .await
                .unwrap()
                .status,
            "not_logged_in"
        );
        assert!(f.auth.job_refresh(Region::Global, "main").await.is_err());
        assert_eq!(f.mock.issues.load(Ordering::SeqCst), 0);
        f.auth.job_prepare(Region::Global, "main").await.unwrap();
        let mut job = saved(&f).await;
        job.refresh_expires_at_ms = Some(now_ms() - 1);
        repo::set_secret(
            &f.auth.pool,
            &key(Region::Global, "job", "main"),
            &encode(&job).unwrap(),
        )
        .await
        .unwrap();
        assert!(f.auth.job_refresh(Region::Global, "main").await.is_err());
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn explicit_job_refresh_keeps_near_expiry_device_and_refreshes_model_only() {
        let f = fixture(Region::Global).await;
        f.auth.job_prepare(Region::Global, "main").await.unwrap();
        let (_, mut device) = f.auth.job_device(Region::Global, "main").await.unwrap();
        device.access_expires_at_ms = Some(now_ms() + 30_000);
        let raw = encode(&device).unwrap();
        repo::set_secret(&f.auth.pool, &key(Region::Global, "session", "main"), &raw)
            .await
            .unwrap();
        assert_eq!(
            f.auth
                .job_refresh(Region::Global, "main")
                .await
                .unwrap()
                .status,
            "prepared"
        );
        assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(f.mock.issues.load(Ordering::SeqCst), 1);
        assert_eq!(
            f.auth
                .read(&key(Region::Global, "session", "main"))
                .await
                .unwrap()
                .unwrap(),
            raw
        );
    }

    #[tokio::test]
    async fn refresh_expiring_while_waiting_for_database_connection_sends_no_request() {
        for after_commit in [false, true] {
            let mut f = fixture(Region::Global).await;
            f.auth.job_prepare(Region::Global, "main").await.unwrap();
            let mut job = saved(&f).await;
            let deadline = now_ms() + 300;
            job.refresh_expires_at_ms = Some(deadline);
            let raw = encode(&job).unwrap();
            repo::set_secret(&f.auth.pool, &key(Region::Global, "job", "main"), &raw)
                .await
                .unwrap();
            let pause = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
            if after_commit {
                f.auth.job_commit_pause = Some(pause.clone());
            } else {
                f.auth.job_reserve_pause = Some(pause.clone());
            }
            let auth = f.auth.clone();
            let task = tokio::spawn(async move { auth.job_refresh(Region::Global, "main").await });
            tokio::time::timeout(Duration::from_secs(5), pause.0.notified())
                .await
                .unwrap();
            // The single-connection test pool provides a deterministic reservation
            // wait: the paused updater cannot begin its write until this is released.
            let connection = f.auth.pool.acquire().await.unwrap();
            pause.1.notify_one();
            tokio::time::sleep(Duration::from_millis(
                (deadline - now_ms()).max(0) as u64 + 20,
            ))
            .await;
            drop(connection);
            assert!(tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .is_err());
            assert_eq!(f.mock.refreshes.load(Ordering::SeqCst), 0);
            assert_eq!(
                f.auth
                    .read(&key(Region::Global, "job", "main"))
                    .await
                    .unwrap()
                    .unwrap(),
                raw
            );
        }
    }

    #[tokio::test]
    async fn retained_refresh_preserves_expiry_and_new_device_generation_marks_job_stale() {
        let f = fixture(Region::Global).await;
        f.auth.job_prepare(Region::Global, "main").await.unwrap();
        let before = saved(&f).await;
        f.mock
            .refresh
            .lock()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("refresh_token");
        f.auth.job_refresh(Region::Global, "main").await.unwrap();
        let after = saved(&f).await;
        assert_eq!(before.refresh, after.refresh);
        assert_eq!(before.refresh_expires_at_ms, after.refresh_expires_at_ms);
        let (_, mut device) = f.auth.job_device(Region::Global, "main").await.unwrap();
        device.uid = "other".into();
        repo::set_secret(
            &f.auth.pool,
            &key(Region::Global, "session", "main"),
            &encode(&device).unwrap(),
        )
        .await
        .unwrap();
        let status = f.auth.job_status(Region::Global, "main").await.unwrap();
        assert_eq!(status.status, "stale");
        assert!(!status.can_refresh);
    }

    #[test]
    fn job_expiry_uses_its_public_millisecond_contract() {
        let before = now_ms();
        let parsed = job_expiry(&json!({"expires_in":60000}), false)
            .unwrap()
            .unwrap();
        assert!((before + 60000..=now_ms() + 60000).contains(&parsed));
        let absolute = 1_900_000_000_000_i64;
        assert_eq!(
            job_expiry(&json!({"expires_at":absolute}), false).unwrap(),
            Some(absolute)
        );
        assert_eq!(job_expiry(&json!({}), false).unwrap(), None);
        let device_expiry = expiry(&json!({"expires_in":60}), "expires_at", "expires_in")
            .unwrap()
            .unwrap();
        assert!((before + 60000..=now_ms() + 60000).contains(&device_expiry));
    }
}
