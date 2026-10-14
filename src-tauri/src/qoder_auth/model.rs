//! Crate-private credential snapshots. Never serialized, logged or returned by IPC.
use super::*;

pub(crate) struct ModelAuthorization {
    pub(crate) region: Region,
    pub(crate) account: String,
    pub(crate) fingerprint: String,
    context: Value,
    records: Vec<(String, String)>,
    device_expiry: Option<i64>,
    job_expiry: Option<i64>,
    secrets: Vec<String>,
}
impl ModelAuthorization {
    pub(crate) fn secrets(&self) -> Vec<&str> {
        self.secrets.iter().map(String::as_str).collect()
    }
    fn expired(&self) -> bool {
        self.device_expiry.is_some_and(|v| v <= now_ms())
            || self.job_expiry.is_some_and(|v| v <= now_ms())
    }
}
impl QoderAuth {
    #[cfg(test)]
    pub(crate) fn with_model_test_origin(pool: SqlitePool, origin: Url) -> Self {
        let mut service = Self::new(pool, None).unwrap();
        service.test_origin = Some(origin);
        service
    }
    #[cfg(test)]
    pub(crate) async fn test_seed_model_account(
        &self,
        region: Region,
        account: &str,
        expires: i64,
    ) {
        let device = Credential {
            region,
            issuer: region.api().into(),
            access: format!("synthetic-device-{}-{account}", region.key()),
            refresh: None,
            uid: format!("synthetic-owner-{account}"),
            organization: None,
            installation_id: uuid::Uuid::new_v4().to_string(),
            access_expires_at_ms: Some(expires),
            refresh_expires_at_ms: None,
            snapshot: AccountSnapshot::default(),
            refresh_attempted: false,
        };
        let job = JobCredential {
            region,
            issuer: region.api().into(),
            device_generation: generation(&device).unwrap(),
            access: format!("synthetic-job-{}-{account}", region.key()),
            refresh: None,
            access_expires_at_ms: Some(expires),
            refresh_expires_at_ms: None,
            principal_verified: true,
        };
        let raw=serde_json::to_string(&json!({"uid":device.uid,"security_oauth_token":job.access,"organization_id":null,"organization_tags":[],"data_policy_agreed":false})).unwrap();
        let fields = protocol::invoke(&json!({"operation":"runtime_fields","raw":raw}))
            .await
            .unwrap();
        let context = SigningContext {
            region,
            issuer: device.issuer.clone(),
            device_generation: generation(&device).unwrap(),
            job_generation: job_generation(&job),
            uid: device.uid.clone(),
            organization_id: None,
            organization_tags: vec![],
            machine_id: device.installation_id.clone(),
            data_policy_agreed: false,
            access_expires_at_ms: job.access_expires_at_ms,
            runtime_fields: fields,
        };
        for (purpose, value) in [
            ("session", encode(&device).unwrap()),
            ("job", encode(&job).unwrap()),
            ("job-signing", encode(&context).unwrap()),
        ] {
            repo::set_secret(&self.pool, &key(region, purpose, account), &value)
                .await
                .unwrap();
        }
    }
    /// Uses only the application's verified encrypted records. No token issuance,
    /// refresh, login or model request is implied by authorization preparation.
    pub(crate) async fn prepare_model_authorization(
        &self,
        region: Region,
        account: &str,
    ) -> Result<ModelAuthorization, String> {
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
            .ok_or("Qoder模型签名未准备")?;
        let context: SigningContext = decode(&context_raw)?;
        if !job.principal_verified || !matches(&context, &device, &job)? {
            return Err("Qoder模型身份未核验或认证代次改变".into());
        }
        let records = vec![
            (key(region, "session", account), device_raw),
            (key(region, "job", account), job_raw),
            (key(region, "job-signing", account), context_raw),
        ];
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&records).map_err(|_| "Qoder模型认证代次编码失败")?)
        );
        let mut secrets = vec![device.access.clone(), job.access.clone()];
        secrets.extend(device.refresh.clone());
        secrets.extend(job.refresh.clone());
        for field in ["key", "encrypt_user_info"] {
            secrets.push(
                context
                    .runtime_fields
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or("Qoder签名认证材料无效")?
                    .to_owned(),
            );
        }
        let authorization = ModelAuthorization {
            region,
            account: account.into(),
            fingerprint,
            context: json!({"region":region,"machine_id":context.machine_id,"uid":context.uid,"version":"1.0.51","organization_id":context.organization_id,
                "organization_tags":context.organization_tags,"data_policy_agreed":context.data_policy_agreed,"runtime_fields":context.runtime_fields}),
            records,
            device_expiry: device.access_expires_at_ms,
            job_expiry: job.access_expires_at_ms,
            secrets,
        };
        self.model_authorization_current(&authorization).await?;
        Ok(authorization)
    }
    /// One SQL statement observes a coherent snapshot, including another service's
    /// relogin/refresh/cancel. Recheck expiry after waiting for the DB connection.
    pub(crate) async fn model_authorization_current(
        &self,
        auth: &ModelAuthorization,
    ) -> Result<(), String> {
        if auth.expired() {
            return Err("Qoder模型认证已到期，未重试或自动刷新".into());
        }
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name IN (?,?,?)")
                .bind(&auth.records[0].0)
                .bind(&auth.records[1].0)
                .bind(&auth.records[2].0)
                .fetch_all(&self.pool)
                .await
                .map_err(|_| "无法核验Qoder模型认证代次")?;
        if auth.expired()
            || rows.len() != 3
            || auth.records.iter().any(|record| !rows.contains(record))
        {
            return Err("Qoder模型认证已取消、改变或到期，未重试".into());
        }
        Ok(())
    }
    pub(crate) async fn prepare_model_wire_request(
        &self,
        auth: &mut ModelAuthorization,
        body: &Value,
        model: &str,
    ) -> Result<(reqwest::header::HeaderMap, String), String> {
        self.model_authorization_current(auth).await?;
        let raw = serde_json::to_string(body).map_err(|_| "Qoder模型请求编码失败")?;
        let request=protocol::invoke(&json!({"operation":"prepare_inference","context":auth.context,"raw":raw,"model":model})).await?;
        let expected=format!("{}/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1",auth.region.model_origin());
        if request["url"].as_str() != Some(&expected) {
            return Err("Qoder推理请求区域或目标无效".into());
        }
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in request["headers"]
            .as_object()
            .ok_or("Qoder签名请求头缺失")?
        {
            // Current normal-user SDK removes legacy WASM machine assertions.
            // This app has no verified MachineInfo, so never invent either value.
            if name.eq_ignore_ascii_case("cosy-machinetoken")
                || name.eq_ignore_ascii_case("cosy-machinetype")
            {
                continue;
            }
            let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "Qoder签名头无效")?;
            let text = value.as_str().ok_or("Qoder签名值无效")?;
            let mut value = HeaderValue::from_str(text).map_err(|_| "Qoder签名值无效")?;
            value.set_sensitive(true);
            headers.insert(header, value);
            if name.eq_ignore_ascii_case("authorization") {
                auth.secrets.push(text.into());
            }
        }
        self.model_authorization_current(auth).await?;
        let body = request["body"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or("Qoder编码请求体无效")?
            .to_owned();
        Ok((headers, body))
    }
}
