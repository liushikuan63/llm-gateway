//! Explicit, one-shot native check-in status; no refresh, claim, or credential export.
use super::*;

#[derive(Debug, Serialize, PartialEq)]
pub struct WorkBuddyCheckinStatus {
    pub account_id: String,
    pub region: WorkBuddyRegion,
    pub active: Option<bool>,
    pub today_checked_in: Option<bool>,
    pub streak_days: Option<u32>,
    pub daily_credit: Option<f64>,
    pub today_credit: Option<f64>,
    pub total_credits: Option<f64>,
    pub checked_at: String,
}

impl WorkBuddyAuth {
    /// Normal use prepares the saved authentication; the explicit CLI method below stays one-shot.
    pub async fn checkin_status_for_use(
        &self,
        account: &str,
    ) -> Result<WorkBuddyCheckinStatus, String> {
        self.prepare_model_authorization(account).await?;
        self.checkin_status(account).await
    }

    pub async fn checkin_status(&self, account: &str) -> Result<WorkBuddyCheckinStatus, String> {
        self.checkin_status_with_principal(account, None).await
    }

    pub(crate) async fn checkin_status_bound(
        &self,
        account: &str,
        expected_principal: &str,
    ) -> Result<WorkBuddyCheckinStatus, String> {
        self.prepare_model_authorization_bound(account, Some(expected_principal))
            .await?;
        self.checkin_status_with_principal(account, Some(expected_principal))
            .await
    }

    async fn checkin_status_with_principal(
        &self,
        account: &str,
        expected_principal: Option<&str>,
    ) -> Result<WorkBuddyCheckinStatus, String> {
        if !valid_id(account) {
            return Err("账号 ID 无效".into());
        }
        let key = self.session_key(account);
        let cipher = self.read(&key).await?.ok_or("请先登录 WorkBuddy")?;
        let credential: Credential = decode(&cipher)?;
        validate_credential(&credential, self.region)?;
        if let Some(expected) = expected_principal {
            if super::daily_claim::principal(&credential, self.region)? != expected {
                return Err("WorkBuddy 登录主体与权益关联不一致；未发送请求".into());
            }
        }
        if credential
            .access_expires_at_ms
            .is_some_and(|at| at <= now_ms())
        {
            return Err("访问认证已过期，请手动刷新或重新登录".into());
        }
        let uid = credential
            .uid
            .as_deref()
            .ok_or("账号标识缺失，请重新登录")?;
        if credential.snapshot.identity_hash.as_deref()
            != Some(self.region.identity_hash(uid).as_str())
        {
            return Err("账号标识与登录身份不一致，请重新登录".into());
        }
        let mut headers = reqwest::header::HeaderMap::new();
        let mut values = vec![
            (
                "authorization",
                format!("Bearer {}", credential.access_token),
            ),
            ("x-user-id", uid.to_owned()),
            ("x-domain", credential.domain.clone()),
        ];
        if let Some(enterprise) = &credential.enterprise_id {
            values.push(("x-enterprise-id", enterprise.clone()));
            values.push(("x-tenant-id", enterprise.clone()));
        }
        for (name, value) in values {
            let mut value = HeaderValue::from_str(&value).map_err(|_| "账号认证头无效")?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let response = self
            .client
            .post(format!(
                "{}/v2/billing/meter/checkin-activity-status",
                self.base
            ))
            .headers(headers)
            .header("Accept", "application/json")
            .json(&json!({}))
            .send()
            .await
            .map_err(|_| "签到状态请求失败或超时；未自动重试")?;
        let http_status = response.status().as_u16();
        if http_status != 200 {
            return Err(format!(
                "签到状态服务拒绝请求（HTTP {http_status}）；未自动刷新或重试"
            ));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "签到状态响应读取失败")?;
            if bytes.len() + chunk.len() > BODY_LIMIT {
                return Err("签到状态响应过大".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "签到状态响应格式无效")?;
        let mut secrets = vec![credential.access_token.as_str()];
        secrets.extend(credential.refresh_token.as_deref());
        if reflects(&value, &secrets) {
            return Err("签到状态响应包含认证资料，已拒绝展示".into());
        }
        if value["code"].as_i64() != Some(0) {
            return Err(match value["code"].as_i64() {
                Some(code) => format!("平台未确认签到状态（业务码 {code}）；未自动重试"),
                None => "平台未确认签到状态；未自动重试".into(),
            });
        }
        let data = value
            .get("data")
            .and_then(Value::as_object)
            .ok_or("签到状态响应缺少数据")?;
        let boolean = |key: &str| -> Result<Option<bool>, String> {
            match data.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v
                    .as_bool()
                    .map(Some)
                    .ok_or_else(|| "签到状态字段无效".into()),
            }
        };
        let credit = |key: &str| -> Result<Option<f64>, String> {
            match data.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .map(Some)
                    .ok_or_else(|| "签到积分字段无效".into()),
            }
        };
        let streak_days = match data.get("streak_days") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or("签到天数字段无效")?,
            ),
        };
        let result = WorkBuddyCheckinStatus {
            account_id: account.into(),
            region: self.region,
            active: boolean("active")?,
            today_checked_in: boolean("today_checked_in")?,
            streak_days,
            daily_credit: credit("daily_credit")?,
            today_credit: credit("today_credit")?,
            total_credits: credit("total_credits")?,
            checked_at: chrono::Utc::now().to_rfc3339(),
        };
        if self.read(&key).await?.as_deref() != Some(cipher.as_str())
            || credential
                .access_expires_at_ms
                .is_some_and(|at| at <= now_ms())
        {
            return Err("登录状态已变化或到期，请重新检查；未保存迟到结果".into());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn normal_use_prepares_expired_auth_once_before_reading_each_region() {
        for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
            let refreshes = Arc::new(AtomicUsize::new(0));
            let statuses = Arc::new(AtomicUsize::new(0));
            let refresh_count = refreshes.clone();
            let status_count = statuses.clone();
            let app = Router::new()
                .route("/v2/plugin/auth/token/refresh", post(move |headers: HeaderMap| {
                    let count = refresh_count.clone();
                    async move {
                        assert_eq!(headers["x-refresh-token"], "fixture-status-refresh");
                        assert!(!headers.contains_key("authorization"));
                        count.fetch_add(1,Ordering::SeqCst);
                        Json(json!({"code":0,"data":{"accessToken":"fixture-status-new-access","refreshToken":"fixture-status-new-refresh","expiresAt":now_ms()+3_600_000,"refreshExpiresAt":now_ms()+7_200_000}}))
                    }
                }))
                .route("/v2/plugin/accounts", axum::routing::get(|| async {
                    Json(json!({"code":0,"data":{"accounts":[{"uid":"fixture-status-uid"}]}}))
                }))
                .route("/v2/billing/meter/checkin-activity-status", post(move |headers: HeaderMap| {
                    let count = status_count.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer fixture-status-new-access");
                        assert!(!headers.contains_key("x-refresh-token"));
                        count.fetch_add(1,Ordering::SeqCst);
                        Json(json!({"code":0,"data":{"active":true,"today_checked_in":false}}))
                    }
                }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let db = crate::db::Db::connect_in_memory().await.unwrap();
            let auth =
                WorkBuddyAuth::with_test_origin_for_region(db.pool().clone(), origin, region);
            auth.test_seed_account(
                "fixture",
                "fixture-status-uid",
                "fixture-status-access",
                "fixture-status-refresh",
            )
            .await;
            let key = auth.session_key("fixture");
            let mut old: Credential = decode(&auth.read(&key).await.unwrap().unwrap()).unwrap();
            old.access_expires_at_ms = Some(now_ms() - 1);
            repo::set_secret(&auth.pool, &key, &encode(&old).unwrap())
                .await
                .unwrap();
            assert!(auth.checkin_status("fixture").await.is_err());
            assert_eq!(refreshes.load(Ordering::SeqCst), 0);
            assert_eq!(statuses.load(Ordering::SeqCst), 0);
            let status = auth.checkin_status_for_use("fixture").await.unwrap();
            assert_eq!(status.region, region);
            assert_eq!(status.today_checked_in, Some(false));
            assert_eq!(refreshes.load(Ordering::SeqCst), 1);
            assert_eq!(statuses.load(Ordering::SeqCst), 1);
            auth.checkin_status_for_use("fixture").await.unwrap();
            assert_eq!(refreshes.load(Ordering::SeqCst), 1);
            assert_eq!(statuses.load(Ordering::SeqCst), 2);
            server.abort();
        }
    }
    #[tokio::test]
    async fn both_regions_post_once_with_saved_uid_without_refresh_or_database_writes() {
        for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
            let calls = Arc::new(AtomicUsize::new(0));
            let app = Router::new().route("/v2/billing/meter/checkin-activity-status", post(|State(calls): State<Arc<AtomicUsize>>, headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers["authorization"], "Bearer fixture-status-access");
                assert_eq!(headers["x-user-id"], "fixture-status-uid");
                assert!(headers.get("x-refresh-token").is_none());
                assert!(headers.get("x-no-user-id").is_none());
                assert_eq!(body, json!({}));
                calls.fetch_add(1, Ordering::SeqCst);
                Json(json!({"code":0,"data":{"active":true,"today_checked_in":false,"streak_days":2,"daily_credit":100,"today_credit":0,"total_credits":200}}))
            })).with_state(calls.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
            let db = crate::db::Db::connect_in_memory().await.unwrap();
            let auth =
                WorkBuddyAuth::with_test_origin_for_region(db.pool().clone(), origin, region);
            auth.test_seed_account(
                "fixture",
                "fixture-status-uid",
                "fixture-status-access",
                "fixture-status-refresh",
            )
            .await;
            let before = auth.read(&auth.session_key("fixture")).await.unwrap();
            let status = auth.checkin_status("fixture").await.unwrap();
            assert_eq!(status.region, region);
            assert_eq!(status.active, Some(true));
            assert_eq!(status.today_checked_in, Some(false));
            assert_eq!(status.daily_credit, Some(100.0));
            assert_eq!(
                auth.read(&auth.session_key("fixture")).await.unwrap(),
                before
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            server.abort();
        }
    }
    #[tokio::test]
    async fn normal_use_failed_preparation_stays_locked_and_never_posts_status() {
        let (auth, calls, server) = fixture(json!({"code":0,"data":{}}), 200).await;
        let key = auth.session_key("fixture");
        let mut old: Credential = decode(&auth.read(&key).await.unwrap().unwrap()).unwrap();
        old.access_expires_at_ms = Some(now_ms() - 1);
        repo::set_secret(&auth.pool, &key, &encode(&old).unwrap())
            .await
            .unwrap();
        assert!(auth.checkin_status_for_use("fixture").await.is_err());
        let blocked = auth.read(&key).await.unwrap().unwrap();
        let saved: Credential = decode(&blocked).unwrap();
        assert!(saved.refresh_attempted);
        assert_eq!(saved.access_token, old.access_token);
        assert_eq!(saved.refresh_token, old.refresh_token);
        assert!(auth.checkin_status_for_use("fixture").await.is_err());
        assert_eq!(auth.read(&key).await.unwrap().unwrap(), blocked);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    async fn fixture(
        reply: Value,
        status: u16,
    ) -> (WorkBuddyAuth, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let app=Router::new().route("/v2/billing/meter/checkin-activity-status", post(|State((calls,reply,status)): State<(Arc<AtomicUsize>,Value,u16)>| async move {
            calls.fetch_add(1,Ordering::SeqCst);
            (axum::http::StatusCode::from_u16(status).unwrap(),Json(reply))
        })).with_state((calls.clone(),reply,status));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let auth = WorkBuddyAuth::with_test_origin(db.pool().clone(), origin);
        auth.test_seed_account(
            "fixture",
            "fixture-status-uid",
            "fixture-status-access",
            "fixture-status-refresh",
        )
        .await;
        (auth, calls, server)
    }

    #[tokio::test]
    async fn absent_fields_stay_unknown_and_bad_types_negative_values_or_reflections_fail() {
        let (auth, calls, server) = fixture(json!({"code":0,"data":{}}), 200).await;
        let result = auth.checkin_status("fixture").await.unwrap();
        assert_eq!(result.active, None);
        assert_eq!(result.today_checked_in, None);
        assert_eq!(result.daily_credit, None);
        assert_eq!(result.streak_days, None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
        for data in [
            json!({"active":"true"}),
            json!({"daily_credit":-1}),
            json!({"streak_days":-1}),
            json!({"streak_days":4294967296_u64}),
            json!({"unused":{"nested":"fixture-status-refresh"}}),
        ] {
            let (auth, calls, server) = fixture(json!({"code":0,"data":data}), 200).await;
            let err = auth.checkin_status("fixture").await.unwrap_err();
            assert!(!err.contains("fixture-status-refresh"));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            server.abort();
        }
    }

    #[tokio::test]
    async fn http_or_business_failure_does_not_retry_refresh_or_change_saved_credentials() {
        for (http, body) in [
            (202, json!({"code":0,"data":{"active":true}})),
            (401, json!({"code":401})),
            (302, json!({})),
            (500, json!({"code":500})),
            (200, json!({"code":41000,"msg":"fixture-status-access"})),
            (200, json!({"code":41000,"msg":"fixed-safe-rejection"})),
        ] {
            let safe_business = body["msg"].as_str() == Some("fixed-safe-rejection");
            let (auth, calls, server) = fixture(body, http).await;
            let before = auth.read(&auth.session_key("fixture")).await.unwrap();
            let error = auth.checkin_status("fixture").await.unwrap_err();
            assert!(!error.contains("fixture-status-access"));
            if http != 200 {
                assert!(error.contains(&format!("HTTP {http}")));
            }
            if safe_business {
                assert!(error.contains("41000"));
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                auth.read(&auth.session_key("fixture")).await.unwrap(),
                before
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn missing_expired_or_unbound_sessions_fail_before_http() {
        let (auth, calls, server) = fixture(json!({"code":0,"data":{}}), 200).await;
        assert!(auth.checkin_status("missing").await.is_err());
        assert!(auth.checkin_status("bad:id").await.is_err());
        let key = auth.session_key("fixture");
        let original = auth.read(&key).await.unwrap().unwrap();
        for mode in [0, 1, 2] {
            let mut credential: Credential = decode(&original).unwrap();
            match mode {
                0 => credential.access_expires_at_ms = Some(now_ms() - 1),
                1 => credential.uid = None,
                _ => credential.snapshot.identity_hash = Some("0".repeat(64)),
            }
            repo::set_secret(&auth.pool, &key, &encode(&credential).unwrap())
                .await
                .unwrap();
            assert!(auth.checkin_status("fixture").await.is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn oversized_body_is_rejected_without_returning_upstream_text() {
        let (auth, calls, server) = fixture(
            json!({"code":0,"data":{"unused":"x".repeat(BODY_LIMIT)}}),
            200,
        )
        .await;
        assert!(auth
            .checkin_status("fixture")
            .await
            .unwrap_err()
            .contains("过大"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn changed_account_generation_discards_late_reply_without_overwriting_new_session() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let app = Router::new()
            .route(
                "/v2/billing/meter/checkin-activity-status",
                post(
                    |State((entered, release)): State<(
                        Arc<tokio::sync::Notify>,
                        Arc<tokio::sync::Notify>,
                    )>| async move {
                        entered.notify_one();
                        release.notified().await;
                        Json(json!({"code":0,"data":{"active":true}}))
                    },
                ),
            )
            .with_state((entered.clone(), release.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let auth = WorkBuddyAuth::with_test_origin(db.pool().clone(), origin);
        auth.test_seed_account(
            "fixture",
            "old-uid",
            "fixture-old-access",
            "fixture-old-refresh",
        )
        .await;
        let task_auth = auth.clone();
        let task = tokio::spawn(async move { task_auth.checkin_status("fixture").await });
        entered.notified().await;
        auth.test_seed_account(
            "fixture",
            "new-uid",
            "fixture-new-access",
            "fixture-new-refresh",
        )
        .await;
        let current = auth.read(&auth.session_key("fixture")).await.unwrap();
        release.notify_one();
        assert!(task.await.unwrap().unwrap_err().contains("已变化"));
        assert_eq!(
            auth.read(&auth.session_key("fixture")).await.unwrap(),
            current
        );
        server.abort();
    }
}
