//! Native check-in delivery; persistent reservations survive cancellation and process restart.
use super::*;

const LEDGER_ERROR: &str = "无法确认当日领取收据；未再次发送领取请求";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkBuddyDailyClaim {
    pub account_id: String,
    pub region: WorkBuddyRegion,
    /// Local reservation day in the user's configured Beijing daily schedule, not a server date.
    pub reservation_day: String,
    pub verdict: WorkBuddyDailyVerdict,
    pub amount: Option<f64>,
    pub checked_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkBuddyDailyVerdict {
    Granted,
    AlreadyClaimed,
    OutcomeUnknown,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u8,
    region: WorkBuddyRegion,
    principal: String,
    day: String,
    account_id: String,
    attempt: uuid::Uuid,
    result: Option<WorkBuddyDailyClaim>,
}

fn day() -> String {
    day_at(chrono::Utc::now())
}

fn day_at(now: chrono::DateTime<chrono::Utc>) -> String {
    now.with_timezone(&chrono::FixedOffset::east_opt(28_800).expect("Beijing offset"))
        .format("%Y-%m-%d")
        .to_string()
}

pub(super) fn principal(
    credential: &Credential,
    region: WorkBuddyRegion,
) -> Result<String, String> {
    validate_credential(credential, region)?;
    let uid = credential
        .uid
        .as_deref()
        .ok_or("账号标识缺失，请重新登录")?;
    let hash = region.identity_hash(uid);
    if credential.snapshot.identity_hash.as_deref() != Some(hash.as_str()) {
        return Err("账号标识与登录身份不一致，请重新登录".into());
    }
    // Separate enterprise scopes without ever storing raw UID or enterprise ID in the ledger.
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(hash, &credential.enterprise_id)).map_err(|_| LEDGER_ERROR)?
        )
    ))
}

fn cached(
    raw: &str,
    region: WorkBuddyRegion,
    principal: &str,
    day: &str,
) -> Result<WorkBuddyDailyClaim, String> {
    if raw.len() > 16 * 1024 {
        return Err(LEDGER_ERROR.into());
    }
    let ledger: Ledger = decode(raw).map_err(|_| LEDGER_ERROR)?;
    if ledger.version != 1
        || ledger.region != region
        || ledger.principal != principal
        || ledger.day != day
        || !valid_id(&ledger.account_id)
        || ledger.attempt.get_version_num() != 4
    {
        return Err(LEDGER_ERROR.into());
    }
    let result = ledger.result.ok_or(LEDGER_ERROR)?;
    if result.account_id != ledger.account_id
        || result.region != region
        || result.reservation_day != day
        || chrono::DateTime::parse_from_rfc3339(&result.checked_at).is_err()
        || match result.verdict {
            WorkBuddyDailyVerdict::Granted => {
                !result.amount.is_some_and(|n| n.is_finite() && n > 0.0)
            }
            _ => result.amount.is_some(),
        }
    {
        return Err(LEDGER_ERROR.into());
    }
    Ok(result)
}

impl WorkBuddyAuth {
    /// One delivery per principal/region/Beijing day, including aliases and independent processes.
    /// A pending/crashed/invalid reservation is never automatically cleared or resent.
    pub async fn claim_daily(&self, account: &str) -> Result<WorkBuddyDailyClaim, String> {
        self.claim_daily_bound(account, None, None)
            .await
            .map(|(result, _)| result)
    }

    pub(crate) async fn claim_daily_bound(
        &self,
        account: &str,
        expected_principal: Option<&str>,
        expected_day: Option<&str>,
    ) -> Result<(WorkBuddyDailyClaim, bool), String> {
        if !valid_id(account) {
            return Err("账号 ID 无效".into());
        }
        let session_key = self.session_key(account);
        let initial = self.read(&session_key).await?.ok_or("请先登录 WorkBuddy")?;
        let credential: Credential = decode(&initial)?;
        let principal = principal(&credential, self.region)?;
        if expected_principal.is_some_and(|expected| expected != principal) {
            return Err("WorkBuddy 登录主体与权益关联不一致；未发送请求".into());
        }
        let today = day();
        if expected_day.is_some_and(|expected| expected != today) {
            return Err("WorkBuddy 领取日期已变化；未发送请求".into());
        }
        let name = format!(
            "workbuddy:daily-claim:v1:{}:{principal}:{today}",
            self.region.key()
        );
        // Read existing receipts before preparing auth: re-opening a receipt must not refresh auth.
        if let Some(raw) = self.read(&name).await? {
            return cached(&raw, self.region, &principal, &today).map(|result| (result, false));
        }
        self.prepare_model_authorization_bound(account, expected_principal)
            .await?;
        let cipher = self.read(&session_key).await?.ok_or("请先登录 WorkBuddy")?;
        let credential: Credential = decode(&cipher)?;
        if self::principal(&credential, self.region)? != principal {
            return Err("登录身份已变化，请重新检查".into());
        }
        let status = self.checkin_status(account).await?;
        if status.active != Some(true) {
            return Err("平台未确认签到活动已启用；未发送领取请求".into());
        }
        let checked_in = status
            .today_checked_in
            .ok_or("平台未明确今日签到状态；未发送领取请求")?;

        let mut ledger = Ledger {
            version: 1,
            region: self.region,
            principal: principal.clone(),
            day: today.clone(),
            account_id: account.into(),
            attempt: uuid::Uuid::new_v4(),
            result: None,
        };
        if checked_in {
            ledger.result = Some(WorkBuddyDailyClaim {
                account_id: account.into(),
                region: self.region,
                reservation_day: today.clone(),
                verdict: WorkBuddyDailyVerdict::AlreadyClaimed,
                amount: None,
                checked_at: chrono::Utc::now().to_rfc3339(),
            });
        }
        let reserved = encode(&ledger).map_err(|_| LEDGER_ERROR)?;
        let mut tx = self.pool.begin().await.map_err(|_| LEDGER_ERROR)?;
        // Acquire the SQLite writer reservation before observing credentials or claim ownership.
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name='workbuddy:daily-claim-writer-fence'")
            .execute(&mut *tx).await.map_err(|_| LEDGER_ERROR)?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(&session_key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| LEDGER_ERROR)?;
        if current.as_deref() != Some(cipher.as_str())
            || day() != today
            || credential
                .access_expires_at_ms
                .is_some_and(|at| at <= now_ms())
        {
            return Err("登录状态或日期已变化；未发送领取请求".into());
        }
        let changed = sqlx::query("INSERT INTO app_secrets(name,value_enc,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(name) DO NOTHING")
            .bind(&name).bind(&reserved).bind(chrono::Utc::now()).bind(chrono::Utc::now())
            .execute(&mut *tx).await.map_err(|_| LEDGER_ERROR)?.rows_affected();
        tx.commit().await.map_err(|_| LEDGER_ERROR)?;
        if changed != 1 {
            let raw = self.read(&name).await?.ok_or(LEDGER_ERROR)?;
            return cached(&raw, self.region, &principal, &today).map(|result| (result, false));
        }
        if let Some(result) = ledger.result {
            return Ok((result, false));
        }
        // Every exit after this commit leaves a durable block against replay, even cancellation.
        self.validate_claim_dispatch(&session_key, &cipher, &credential, &today, chrono::Utc::now)
            .await?;
        let amount = self.send_daily_claim(&credential).await.ok();
        if self.read(&session_key).await?.as_deref() != Some(cipher.as_str())
            || day() != today
            || credential
                .access_expires_at_ms
                .is_some_and(|at| at <= now_ms())
        {
            return Err("领取后登录状态或日期已变化；结果未确认，未自动重发".into());
        }
        let result = WorkBuddyDailyClaim {
            account_id: account.into(),
            region: self.region,
            reservation_day: today,
            verdict: if amount.is_some() {
                WorkBuddyDailyVerdict::Granted
            } else {
                WorkBuddyDailyVerdict::OutcomeUnknown
            },
            amount,
            checked_at: chrono::Utc::now().to_rfc3339(),
        };
        ledger.result = Some(result.clone());
        let terminal = encode(&ledger).map_err(|_| LEDGER_ERROR)?;
        let mut tx = self.pool.begin().await.map_err(|_| LEDGER_ERROR)?;
        sqlx::query("UPDATE app_secrets SET updated_at=updated_at WHERE name='workbuddy:daily-claim-writer-fence'")
            .execute(&mut *tx).await.map_err(|_| LEDGER_ERROR)?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(&session_key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| LEDGER_ERROR)?;
        if current.as_deref() != Some(cipher.as_str())
            || day() != ledger.day
            || credential
                .access_expires_at_ms
                .is_some_and(|at| at <= now_ms())
        {
            return Err("领取后登录状态或日期已变化；结果未确认，未自动重发".into());
        }
        let changed = sqlx::query(
            "UPDATE app_secrets SET value_enc=?,updated_at=? WHERE name=? AND value_enc=?",
        )
        .bind(terminal)
        .bind(chrono::Utc::now())
        .bind(&name)
        .bind(reserved)
        .execute(&mut *tx)
        .await
        .map_err(|_| LEDGER_ERROR)?
        .rows_affected();
        if changed != 1 {
            return Err(LEDGER_ERROR.into());
        }
        tx.commit().await.map_err(|_| LEDGER_ERROR)?;
        Ok((result, true))
    }

    async fn validate_claim_dispatch(
        &self,
        key: &str,
        cipher: &str,
        credential: &Credential,
        today: &str,
        clock: impl Fn() -> chrono::DateTime<chrono::Utc>,
    ) -> Result<(), String> {
        let current = self.read(key).await?;
        let now = clock();
        if current.as_deref() != Some(cipher)
            || day_at(now) != today
            || credential
                .access_expires_at_ms
                .is_some_and(|at| at <= now.timestamp_millis())
        {
            return Err("登录状态或日期已变化；领取预约已保留，未发送请求".into());
        }
        Ok(())
    }

    async fn send_daily_claim(&self, credential: &Credential) -> Result<f64, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut values = vec![
            (
                "authorization",
                format!("Bearer {}", credential.access_token),
            ),
            (
                "x-user-id",
                credential.uid.as_deref().ok_or("账号标识缺失")?.to_owned(),
            ),
            ("x-domain", credential.domain.clone()),
        ];
        if let Some(enterprise) = &credential.enterprise_id {
            values.push(("x-enterprise-id", enterprise.clone()));
            values.push(("x-tenant-id", enterprise.clone()));
        }
        for (key, value) in values {
            let mut value = HeaderValue::from_str(&value).map_err(|_| "账号认证头无效")?;
            value.set_sensitive(true);
            headers.insert(key, value);
        }
        let response = self
            .client
            .post(format!("{}/v2/billing/meter/daily-checkin", self.base))
            .headers(headers)
            .header("Accept", "application/json")
            .json(&json!({}))
            .send()
            .await
            .map_err(|_| LEDGER_ERROR)?;
        if response.status().as_u16() != 200 {
            return Err(LEDGER_ERROR.into());
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| LEDGER_ERROR)?;
            if bytes.len() + chunk.len() > BODY_LIMIT {
                return Err(LEDGER_ERROR.into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| LEDGER_ERROR)?;
        let mut secrets = vec![credential.access_token.as_str()];
        secrets.extend(credential.refresh_token.as_deref());
        if reflects(&value, &secrets) {
            return Err(LEDGER_ERROR.into());
        }
        // The first-party desktop unwraps code=0/data; its SDK also receives bare credit results.
        // Null, missing/zero credit and arbitrary business errors never imply success or replay.
        let amount = match value.get("code") {
            Some(code) if code.as_i64() == Some(0) => {
                value.get("data").and_then(|d| d.get("credit"))
            }
            None => value.get("credit"),
            _ => None,
        };
        amount
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or_else(|| LEDGER_ERROR.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::HeaderMap, routing::post, Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn dispatch_rechecks_time_after_a_blocked_pool_read() {
        use futures_util::FutureExt;
        for expire in [true, false] {
            let f = fixture(
                json!({"active":true,"today_checked_in":false}),
                axum::http::StatusCode::OK,
                json!({"credit":100}),
            )
            .await;
            let key = f.auth.session_key("fixture");
            let cipher = f.auth.read(&key).await.unwrap().unwrap();
            let mut credential: Credential = decode(&cipher).unwrap();
            let before = chrono::Utc::now();
            credential.access_expires_at_ms =
                Some(before.timestamp_millis() + if expire { 70_000 } else { 172_800_000 });
            let cipher = encode(&credential).unwrap();
            repo::set_secret(f.db.pool(), &key, &cipher).await.unwrap();
            let time = std::sync::atomic::AtomicI64::new(before.timestamp_millis());
            let today = day_at(before);
            assert!(f
                .auth
                .validate_claim_dispatch(&key, &cipher, &credential, &today, || before)
                .await
                .is_ok());
            let read_connection = f.db.pool().acquire().await.unwrap();
            let pending =
                f.auth
                    .validate_claim_dispatch(&key, &cipher, &credential, &today, || {
                        chrono::DateTime::from_timestamp_millis(time.load(Ordering::SeqCst))
                            .unwrap()
                    });
            tokio::pin!(pending);
            assert!(
                pending.as_mut().now_or_never().is_none(),
                "pool must be held before advancing the clock"
            );
            time.store(
                before.timestamp_millis() + if expire { 70_001 } else { 86_400_000 },
                Ordering::SeqCst,
            );
            drop(read_connection);
            assert!(
                pending.await.is_err(),
                "expiry/day must be evaluated after the database read"
            );
            assert_eq!(f.calls.load(Ordering::SeqCst), 0);
        }
    }

    struct Fixture {
        db: crate::db::Db,
        auth: WorkBuddyAuth,
        calls: Arc<AtomicUsize>,
        server: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    async fn fixture(status: Value, code: axum::http::StatusCode, response: Value) -> Fixture {
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = calls.clone();
        let app = Router::new()
            .route(
                "/v2/billing/meter/checkin-activity-status",
                post(move || {
                    let status = status.clone();
                    async move { Json(json!({"code":0,"data":status})) }
                }),
            )
            .route(
                "/v2/billing/meter/daily-checkin",
                post(move || {
                    let calls = captured.clone();
                    let response = response.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        (code, Json(response))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let auth = WorkBuddyAuth::with_test_origin_for_region(
            db.pool().clone(),
            base,
            WorkBuddyRegion::Cn,
        );
        auth.test_seed_account(
            "fixture",
            "fixture-claim-uid",
            "fixture-claim-access",
            "fixture-claim-refresh",
        )
        .await;
        Fixture {
            db,
            auth,
            calls,
            server,
        }
    }

    #[tokio::test]
    async fn inactive_unknown_and_already_claimed_status_never_dispatch() {
        for status in [
            json!({"active":false,"today_checked_in":false}),
            json!({"today_checked_in":false}),
            json!({"active":true}),
            json!({"active":true,"today_checked_in":true}),
        ] {
            let f = fixture(
                status.clone(),
                axum::http::StatusCode::OK,
                json!({"credit":100}),
            )
            .await;
            let result = f.auth.claim_daily("fixture").await;
            if status["active"] == true && status["today_checked_in"] == true {
                assert_eq!(
                    result.unwrap().verdict,
                    WorkBuddyDailyVerdict::AlreadyClaimed
                );
                assert_eq!(f.auth.claim_daily("fixture").await.unwrap().amount, None);
            } else {
                assert!(result.is_err());
            }
            assert_eq!(f.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn ambiguous_and_denied_results_are_durable_unknown_without_retry() {
        for (code, response) in [
            (200, json!(null)),
            (200, json!({})),
            (200, json!({"code":0,"data":{}})),
            (200, json!({"code":0,"data":{"credit":0}})),
            (200, json!({"credit":-1})),
            (200, json!({"credit":"100"})),
            (200, json!({"code":11128,"credit":100})),
            (400, json!({"code":10001,"msg":"今天已签到，请明天再来"})),
            (401, json!({"credit":100})),
            (429, json!({"credit":100})),
            (502, json!({"credit":100})),
            (
                200,
                json!({"credit":100,"msg":"fixture-claim-access fixture-claim-refresh"}),
            ),
        ] {
            let f = fixture(
                json!({"active":true,"today_checked_in":false}),
                axum::http::StatusCode::from_u16(code).unwrap(),
                response,
            )
            .await;
            let result = f.auth.claim_daily("fixture").await.unwrap();
            assert_eq!(result.verdict, WorkBuddyDailyVerdict::OutcomeUnknown);
            assert_eq!(result.amount, None);
            assert_eq!(f.auth.claim_daily("fixture").await.unwrap(), result);
            assert_eq!(f.calls.load(Ordering::SeqCst), 1);
            let rows: Vec<String> = sqlx::query_scalar(
                "SELECT value_enc FROM app_secrets WHERE name LIKE 'workbuddy:daily-claim:v1:%'",
            )
            .fetch_all(f.db.pool())
            .await
            .unwrap();
            assert_eq!(rows.len(), 1);
            let safe = crypto::decrypt(&rows[0]).unwrap();
            assert!(!safe.contains("fixture-claim-access"));
            assert!(!safe.contains("fixture-claim-refresh"));
        }
    }

    #[tokio::test]
    async fn duplicate_aliases_share_one_principal_day_receipt() {
        let f = fixture(
            json!({"active":true,"today_checked_in":false}),
            axum::http::StatusCode::OK,
            json!({"credit":12.5}),
        )
        .await;
        f.auth
            .test_seed_account(
                "alias",
                "fixture-claim-uid",
                "fixture-claim-access",
                "fixture-claim-refresh",
            )
            .await;
        let first = f.auth.claim_daily("fixture").await.unwrap();
        let second = f.auth.claim_daily("alias").await.unwrap();
        assert_eq!(first, second);
        assert_eq!(second.amount, Some(12.5));
        assert_eq!(second.account_id, "fixture"); // Preserve the original receipt attribution.
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn corrupt_pending_or_mismatched_receipt_never_unlocks_delivery() {
        for damaged in [
            "corrupt",
            "pending",
            "wrong-region",
            "wrong-day",
            "invalid-amount",
        ] {
            let f = fixture(
                json!({"active":true,"today_checked_in":false}),
                axum::http::StatusCode::OK,
                json!({"credit":100}),
            )
            .await;
            f.auth.claim_daily("fixture").await.unwrap();
            let row:(String,String) = sqlx::query_as("SELECT name,value_enc FROM app_secrets WHERE name LIKE 'workbuddy:daily-claim:v1:%'")
                .fetch_one(f.db.pool()).await.unwrap();
            let raw = if damaged == "corrupt" {
                "not-a-ciphertext".into()
            } else {
                let mut ledger: Ledger = decode(&row.1).unwrap();
                match damaged {
                    "pending" => ledger.result = None,
                    "wrong-region" => ledger.region = WorkBuddyRegion::Global,
                    "wrong-day" => ledger.day = "2000-01-01".into(),
                    "invalid-amount" => ledger.result.as_mut().unwrap().amount = None,
                    _ => unreachable!(),
                }
                encode(&ledger).unwrap()
            };
            repo::set_secret(f.db.pool(), &row.0, &raw).await.unwrap();
            assert!(f.auth.claim_daily("fixture").await.is_err());
            assert_eq!(f.calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                repo::get_secret(f.db.pool(), &row.0).await.unwrap(),
                Some(raw)
            );
        }
    }

    #[tokio::test]
    async fn independent_pools_share_delivery_and_file_receipt_survives_reopen() {
        let f = fixture(
            json!({"active":true,"today_checked_in":false}),
            axum::http::StatusCode::OK,
            json!({"credit":100}),
        )
        .await;
        let directory =
            std::env::temp_dir().join(format!("llmgw-native-claim-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("claim.sqlite");
        let left = crate::db::Db::connect_path(&path).await.unwrap();
        let right = crate::db::Db::connect_path(&path).await.unwrap();
        let a = WorkBuddyAuth::with_test_origin_for_region(
            left.pool().clone(),
            f.auth.base.clone(),
            WorkBuddyRegion::Cn,
        );
        let b = WorkBuddyAuth::with_test_origin_for_region(
            right.pool().clone(),
            f.auth.base.clone(),
            WorkBuddyRegion::Cn,
        );
        a.test_seed_account(
            "fixture",
            "fixture-claim-uid",
            "fixture-claim-access",
            "fixture-claim-refresh",
        )
        .await;
        let (first, second) = tokio::join!(a.claim_daily("fixture"), b.claim_daily("fixture"));
        assert!(first.is_ok() || second.is_ok(), "{first:?} {second:?}");
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
        drop(a);
        drop(b);
        left.pool().close().await;
        right.pool().close().await;
        drop(left);
        drop(right);
        let reopened = crate::db::Db::connect_path(&path).await.unwrap();
        let auth = WorkBuddyAuth::with_test_origin_for_region(
            reopened.pool().clone(),
            f.auth.base.clone(),
            WorkBuddyRegion::Cn,
        );
        assert_eq!(
            auth.claim_daily("fixture").await.unwrap().verdict,
            WorkBuddyDailyVerdict::Granted
        );
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
        drop(auth);
        reopened.pool().close().await;
        drop(reopened);
        // Only remove the explicitly owned UUID fixture directory after closing every pool.
        // SQLx's pool close can precede release of the SQLite worker's Windows file handle.
        // Only cleanup is boundedly retried here; platform delivery is never retried.
        for attempt in 0..10 {
            match std::fs::remove_file(&path) {
                Ok(()) => break,
                Err(error)
                    if cfg!(windows)
                        && matches!(error.raw_os_error(), Some(32 | 33))
                        && attempt < 9 =>
                {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => panic!("owned fixture cleanup failed: {error}"),
            }
        }
        std::fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn cancellation_and_late_identity_change_keep_pending_without_replay() {
        for cancel in [true, false] {
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let calls = Arc::new(AtomicUsize::new(0));
            let (entered_handler, release_handler, count) =
                (entered.clone(), release.clone(), calls.clone());
            let app = Router::new()
                .route(
                    "/v2/billing/meter/checkin-activity-status",
                    post(|| async {
                        Json(json!({"code":0,"data":{"active":true,"today_checked_in":false}}))
                    }),
                )
                .route(
                    "/v2/billing/meter/daily-checkin",
                    post(move || {
                        let (entered, release, count) = (
                            entered_handler.clone(),
                            release_handler.clone(),
                            count.clone(),
                        );
                        async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            entered.notify_one();
                            release.notified().await;
                            Json(json!({"credit":100}))
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let db = crate::db::Db::connect_in_memory().await.unwrap();
            let auth = Arc::new(WorkBuddyAuth::with_test_origin_for_region(
                db.pool().clone(),
                base.clone(),
                WorkBuddyRegion::Cn,
            ));
            auth.test_seed_account(
                "fixture",
                "fixture-claim-uid",
                "fixture-claim-access",
                "fixture-claim-refresh",
            )
            .await;
            let worker = auth.clone();
            let task = tokio::spawn(async move { worker.claim_daily("fixture").await });
            tokio::time::timeout(Duration::from_secs(5), entered.notified())
                .await
                .unwrap();
            if cancel {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                auth.test_seed_account(
                    "fixture",
                    "fixture-other-uid",
                    "fixture-other-access",
                    "fixture-other-refresh",
                )
                .await;
                release.notify_one();
                assert!(task.await.unwrap().is_err());
                auth.test_seed_account(
                    "fixture",
                    "fixture-claim-uid",
                    "fixture-claim-access",
                    "fixture-claim-refresh",
                )
                .await;
            }
            let fresh = WorkBuddyAuth::with_test_origin_for_region(
                db.pool().clone(),
                base,
                WorkBuddyRegion::Cn,
            );
            assert!(fresh.claim_daily("fixture").await.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let raw: String = sqlx::query_scalar(
                "SELECT value_enc FROM app_secrets WHERE name LIKE 'workbuddy:daily-claim:v1:%'",
            )
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert!(decode::<Ledger>(&raw).unwrap().result.is_none());
            release.notify_one();
            server.abort();
        }
    }

    #[tokio::test]
    async fn oversized_and_invalid_json_or_redirect_never_confirm_or_replay() {
        for (status, body) in [
            (200, "{invalid".to_string()),
            (
                200,
                format!(
                    "{{\"credit\":100,\"extra\":\"{}\"}}",
                    "x".repeat(BODY_LIMIT)
                ),
            ),
            (302, "{\"credit\":100}".to_string()),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let app = Router::new()
                .route(
                    "/v2/billing/meter/checkin-activity-status",
                    post(|| async {
                        Json(json!({"code":0,"data":{"active":true,"today_checked_in":false}}))
                    }),
                )
                .route(
                    "/v2/billing/meter/daily-checkin",
                    post(move || {
                        let (count, body) = (count.clone(), body.clone());
                        async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            (
                                axum::http::StatusCode::from_u16(status).unwrap(),
                                [("location", "/must-not-follow")],
                                body,
                            )
                        }
                    }),
                )
                .route(
                    "/must-not-follow",
                    axum::routing::get(|| async {
                        panic!("must not follow redirect");
                        #[allow(unreachable_code)]
                        ""
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let db = crate::db::Db::connect_in_memory().await.unwrap();
            let auth = WorkBuddyAuth::with_test_origin_for_region(
                db.pool().clone(),
                base,
                WorkBuddyRegion::Cn,
            );
            auth.test_seed_account(
                "fixture",
                "fixture-claim-uid",
                "fixture-claim-access",
                "fixture-claim-refresh",
            )
            .await;
            assert_eq!(
                auth.claim_daily("fixture").await.unwrap().verdict,
                WorkBuddyDailyVerdict::OutcomeUnknown
            );
            assert_eq!(
                auth.claim_daily("fixture").await.unwrap().verdict,
                WorkBuddyDailyVerdict::OutcomeUnknown
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            server.abort();
        }
    }

    #[tokio::test]
    async fn both_regions_claim_once_and_recreated_service_does_not_replay() {
        for region in [WorkBuddyRegion::Global, WorkBuddyRegion::Cn] {
            let calls = Arc::new(AtomicUsize::new(0));
            let captured = calls.clone();
            let app = Router::new()
                .route(
                    "/v2/billing/meter/checkin-activity-status",
                    post(|| async {
                        Json(json!({"code":0,"data":{"active":true,"today_checked_in":false}}))
                    }),
                )
                .route(
                    "/v2/billing/meter/daily-checkin",
                    post(move |headers: HeaderMap, Json(body): Json<Value>| {
                        let calls = captured.clone();
                        async move {
                            assert_eq!(headers["authorization"], "Bearer fixture-claim-access");
                            assert_eq!(headers["x-user-id"], "fixture-claim-uid");
                            assert!(!headers.contains_key("x-refresh-token"));
                            assert_eq!(body, json!({}));
                            calls.fetch_add(1, Ordering::SeqCst);
                            Json(json!({"code":0,"data":{"credit":100}}))
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let db = crate::db::Db::connect_in_memory().await.unwrap();
            let auth =
                WorkBuddyAuth::with_test_origin_for_region(db.pool().clone(), base.clone(), region);
            auth.test_seed_account(
                "fixture",
                "fixture-claim-uid",
                "fixture-claim-access",
                "fixture-claim-refresh",
            )
            .await;
            let first = serde_json::to_value(auth.claim_daily("fixture").await.unwrap()).unwrap();
            assert_eq!(first["verdict"], "granted");
            assert_eq!(first["amount"].as_f64(), Some(100.0));
            let restarted =
                WorkBuddyAuth::with_test_origin_for_region(db.pool().clone(), base, region);
            let second =
                serde_json::to_value(restarted.claim_daily("fixture").await.unwrap()).unwrap();
            assert_eq!(second, first);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            server.abort();
        }
    }
}
