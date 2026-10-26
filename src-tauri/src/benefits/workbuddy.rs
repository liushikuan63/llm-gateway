//! Own-login references for WorkBuddy benefits; no imported credentials or client processes.
use super::*;
use crate::workbuddy_auth::{WorkBuddyAuth, WorkBuddyDailyVerdict, WorkBuddyRegion};
use sqlx::Row;

const ERROR: &str = "WorkBuddy 权益关联或收据无法确认；未再次发送领取请求";

pub(super) fn region(platform: &str) -> Option<WorkBuddyRegion> {
    match platform {
        "workbuddy" => Some(WorkBuddyRegion::Global),
        "workbuddy_cn" => Some(WorkBuddyRegion::Cn),
        _ => None,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    kind: String,
    version: u8,
    account_id: String,
    region: WorkBuddyRegion,
    login_account_id: String,
    principal_hash: String,
}

fn reference(raw: &str, account: &BenefitAccount) -> Result<Reference, String> {
    if raw.len() > 16 * 1024 {
        return Err(ERROR.into());
    }
    let r: Reference = serde_json::from_str(raw).map_err(|_| ERROR)?;
    if r.kind != "workbuddy_native_benefit_ref"
        || r.version != 1
        || r.account_id != account.id
        || region(&account.platform) != Some(r.region)
        || !crate::benefit_config::valid_account_id(&r.login_account_id)
        || r.principal_hash.len() != 64
        || !r
            .principal_hash
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(ERROR.into());
    }
    Ok(r)
}

pub(super) async fn read(
    connection: &mut sqlx::SqliteConnection,
    account: &BenefitAccount,
    raw: Option<String>,
) -> Result<source::Snapshot, String> {
    let mut view = BenefitAuthSourceView {
        account_id: account.id.clone(),
        platform: account.platform.clone(),
        source: if raw.is_some() {
            "invalid_binding"
        } else {
            "none"
        }
        .into(),
        source_revision: source::revision(raw.as_deref(), account),
        identity: None,
        access_expires_at_ms: None,
        expiry_unknown: false,
        candidate_identity: None,
        candidate_eligible: false,
        candidate_access_expires_at_ms: None,
        candidate_expiry_unknown: false,
        can_restore: false,
        previous_present: false,
        error_code: None,
        message: None,
    };
    let wire = match raw.as_deref() {
        None => Err("请关联本网关已保存的本区域 WorkBuddy 独立登录".to_string()),
        Some(cipher) => match crypto::decrypt(cipher).map_err(|_| ERROR.to_string()) {
            Err(error) => Err(error),
            Ok(plain) => match reference(&plain, account) {
                Err(error) => Err(error),
                Ok(r) => {
                    let key = WorkBuddyAuth::benefit_session_key(r.region, &r.login_account_id)?;
                    let login: Option<String> =
                        sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                            .bind(key)
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(|_| ERROR)?;
                    match login
                        .as_deref()
                        .map(|cipher| WorkBuddyAuth::benefit_identity(cipher, r.region))
                    {
                        Some(Ok((identity, hash, expiry))) if hash == r.principal_hash => {
                            view.source = "native_login".into();
                            view.identity = Some(identity);
                            view.access_expires_at_ms = expiry;
                            view.expiry_unknown = expiry.is_none();
                            Ok(plain)
                        }
                        _ => Err("WorkBuddy 独立登录缺失或主体已变化，请重新关联".into()),
                    }
                }
            },
        },
    };
    if let Err(message) = &wire {
        view.error_code = Some(
            if raw.is_some() {
                "binding_invalid"
            } else {
                "source_missing"
            }
            .into(),
        );
        view.message = Some(message.clone());
    }
    Ok(source::Snapshot {
        raw,
        wire,
        view,
        candidate: Err("native_missing".into()),
    })
}

impl BenefitsCenter {
    /// Local-only CAS binding: never copies access/refresh tokens into the benefits reference.
    pub async fn bind_workbuddy_benefit_auth(
        &self,
        cfg: &BenefitsConfig,
        account_id: &str,
        login_account_id: &str,
        expected_revision: &str,
    ) -> Result<BenefitAuthSourceView, String> {
        let account = account(cfg, account_id)?;
        let region = region(&account.platform).ok_or("请选择 WorkBuddy 权益账号")?;
        let key = WorkBuddyAuth::benefit_session_key(region, login_account_id)?;
        let gate = self.gate(account);
        let _guard = gate.lock().await;
        self.idle(account)?;
        let mut tx = source::begin(&self.pool).await?;
        let old = source::read(
            &mut tx,
            account,
            crate::trae_auth::NativeReadPurpose::Benefits,
        )
        .await?;
        if old.view.source_revision != expected_revision {
            return Err("权益关联已变化，请重新读取后再关联".into());
        }
        let cipher: Option<String> =
            sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
                .bind(key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| ERROR)?;
        let cipher = cipher.ok_or("请先在客户端接入中完成本区域 WorkBuddy 独立登录")?;
        let (_, hash, _) = WorkBuddyAuth::benefit_identity(&cipher, region).map_err(|_| ERROR)?;
        let r = Reference {
            kind: "workbuddy_native_benefit_ref".into(),
            version: 1,
            account_id: account.id.clone(),
            region,
            login_account_id: login_account_id.into(),
            principal_hash: hash,
        };
        let plain = serde_json::to_string(&r).map_err(|_| ERROR)?;
        let cipher = crypto::encrypt(&plain).map_err(|_| ERROR)?;
        source::write(&mut tx, account, Some(&cipher)).await?;
        let result = source::read(
            &mut tx,
            account,
            crate::trae_auth::NativeReadPurpose::Benefits,
        )
        .await?
        .view;
        tx.commit().await.map_err(|_| ERROR)?;
        Ok(result)
    }
}

fn auth(
    pool: &SqlitePool,
    region: WorkBuddyRegion,
    proxy: Option<&str>,
    endpoint: Option<&str>,
) -> Result<WorkBuddyAuth, String> {
    #[cfg(test)]
    if let Some(endpoint) = endpoint {
        return Ok(WorkBuddyAuth::with_test_origin_for_region(
            pool.clone(),
            endpoint.into(),
            region,
        ));
    }
    #[cfg(not(test))]
    let _ = endpoint;
    WorkBuddyAuth::new_for_region(pool.clone(), proxy, region)
}

pub(super) async fn overview(
    center: &BenefitsCenter,
    cfg: &BenefitsConfig,
    account: &BenefitAccount,
    proxy: Option<&str>,
) -> Result<BenefitAccountView, String> {
    let mut tx = source::begin(&center.pool).await?;
    let snapshot = source::read(
        &mut tx,
        account,
        crate::trae_auth::NativeReadPurpose::Benefits,
    )
    .await?;
    tx.commit().await.map_err(|_| ERROR)?;
    #[cfg(test)]
    let endpoint = center.workbuddy_endpoint.as_deref();
    #[cfg(not(test))]
    let endpoint = None;
    let (status, error) = if cfg.enabled && account.enabled {
        match &snapshot.wire {
            Err(error) => (None, Some(error.clone())),
            Ok(wire) => {
                let r = reference(wire, account)?;
                // Authentication refresh is subject-bound; status preparation must not follow a different login.
                let service = auth(&center.pool, r.region, proxy, endpoint)?;
                match service
                    .checkin_status_bound(&r.login_account_id, &r.principal_hash)
                    .await
                {
                    Err(error) => (None, Some(error)),
                    Ok(status) => {
                        let claimable =
                            status.active == Some(true) && status.today_checked_in == Some(false);
                        (Some(BenefitStatus {platform:ClaimPlatform::parse(&account.platform).ok_or(ERROR)?,claimable,
                            campaigns:vec![BenefitCampaign {campaign_id:"daily-checkin".into(),campaign_key:"daily-checkin".into(),
                                claim_status:if status.today_checked_in==Some(true){"CLAIMED"}else if claimable{"CLAIMABLE"}else{"UNKNOWN"}.into(),
                                action_type:"CLAIM_BENEFIT".into(),amount:status.daily_credit,kind:Some("CREDITS".into()),valid_days:None,start_at:None,end_at:None}],
                            source:"native_login".into(),warnings:vec!["活动积分不代表已到账或总余额；每日预约使用北京时间，结果不明不会自动重发。".into()],checked_at:status.checked_at}),None)
                    }
                }
            }
        }
    } else {
        (None, snapshot.view.message.clone())
    };
    let last_run = center.runs(Some(&account.id), 1).await?.into_iter().next();
    Ok(BenefitAccountView {
        account_id: account.id.clone(),
        platform: account.platform.clone(),
        label: account.label.clone(),
        enabled: account.enabled,
        has_token: snapshot.raw.is_some(),
        token_masked: snapshot.raw.is_some().then(|| TOKEN_MASK.into()),
        auth_source: snapshot.view,
        session_account: None,
        session_error: None,
        status,
        last_run,
        error,
        capabilities: BenefitCapabilities {
            query: true,
            manual_claim: true,
            auto_claim: true,
            scope: "本网关独立登录直连签到；无需运行客户端；到账以单次领取收据为准".into(),
        },
    })
}

async fn existing(
    conn: &mut sqlx::SqliteConnection,
    account: &BenefitAccount,
    window: &str,
) -> Result<Option<BenefitRunRecord>, String> {
    let row=sqlx::query("SELECT id,account_id,platform,window_key,campaign_key,verdict,amount,message,manual,created_at FROM benefit_runs WHERE account_id=? AND platform=? AND window_key=? ORDER BY id DESC LIMIT 1")
        .bind(&account.id).bind(&account.platform).bind(window).fetch_optional(conn).await.map_err(|_|ERROR)?;
    row.map(|row| {
        let verdict: String = row.try_get("verdict").map_err(|_| ERROR)?;
        Ok(BenefitRunRecord {
            id: row.try_get("id").map_err(|_| ERROR)?,
            account_id: row.try_get("account_id").map_err(|_| ERROR)?,
            platform: row.try_get("platform").map_err(|_| ERROR)?,
            window_key: row.try_get("window_key").map_err(|_| ERROR)?,
            campaign_key: row.try_get("campaign_key").map_err(|_| ERROR)?,
            verdict: BenefitVerdict::parse(&verdict).ok_or(ERROR)?,
            amount: row.try_get("amount").map_err(|_| ERROR)?,
            message: row.try_get("message").map_err(|_| ERROR)?,
            manual: row.try_get("manual").map_err(|_| ERROR)?,
            created_at: row.try_get("created_at").map_err(|_| ERROR)?,
        })
    })
    .transpose()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute(
    pool: &SqlitePool,
    account: &BenefitAccount,
    wire: Result<String, String>,
    proxy: Option<&str>,
    manual: bool,
    now: DateTime<Local>,
    admission: i64,
    receipt_day: &str,
    origin: Option<&ReceiptOrigin>,
    endpoint: Option<&str>,
) -> FlightResult {
    let wire = match wire {
        Ok(wire) => wire,
        Err(_) => {
            return record(
                pool,
                account,
                None,
                BenefitVerdict::Error,
                None,
                "WorkBuddy 独立登录尚未关联或关联已失效；未发送请求",
                manual,
                now,
            )
            .await
        }
    };
    let r = reference(&wire, account)?;
    let today = Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(28800).expect("Beijing"))
        .format("%Y-%m-%d")
        .to_string();
    let window = format!("{}:{}:{today}", account.platform, r.principal_hash);
    let mut tx = source::begin(pool).await?;
    let snapshot = source::read(
        &mut tx,
        account,
        crate::trae_auth::NativeReadPurpose::Benefits,
    )
    .await?;
    if snapshot.wire.as_deref() != Ok(wire.as_str()) {
        return Err(ERROR.into());
    }
    let permits = receipt::permits(
        &mut tx,
        pool,
        account,
        receipt_day,
        manual,
        admission,
        origin,
    )
    .await?;
    let previous = existing(&mut tx, account, &window).await?;
    let recheck_inactive = manual
        && previous
            .as_ref()
            .is_some_and(|run| run.verdict == BenefitVerdict::NoClaimable);
    if let Some(run) = previous.filter(|_| !recheck_inactive) {
        tx.commit().await.map_err(|_| ERROR)?;
        return cached_result(run);
    }
    if !permits {
        return Err("当日领取收据已存在；未再次发送领取请求".into());
    }
    let owner = reservation::acquire(&mut tx, account, &window, recheck_inactive, admission)
        .await?
        .ok_or("WorkBuddy 当日领取已预约或结果未确认；未再次发送请求")?;
    tx.commit().await.map_err(|_| ERROR)?;
    let owner = owner.dispatch(pool).await?;
    let service = auth(pool, r.region, proxy, endpoint)?;
    let result = service
        .claim_daily_bound(&r.login_account_id, Some(&r.principal_hash), Some(&today))
        .await;
    if result
        .as_ref()
        .is_ok_and(|(result, _)| result.reservation_day != today)
    {
        return Err(ERROR.into());
    }
    let created_at = result
        .as_ref()
        .map(|(result, _)| result.checked_at.clone())
        .unwrap_or_else(|_| Utc::now().to_rfc3339());
    let (verdict, amount, message) = match result {
        Ok((result, dispatched)) => match result.verdict {
            WorkBuddyDailyVerdict::Granted if dispatched => (
                BenefitVerdict::Granted,
                result.amount,
                "平台已确认本次发放积分",
            ),
            WorkBuddyDailyVerdict::Granted | WorkBuddyDailyVerdict::AlreadyClaimed => (
                BenefitVerdict::Replayed,
                None,
                "已有领取收据或平台已签到；未再次发放",
            ),
            WorkBuddyDailyVerdict::OutcomeUnknown => (
                BenefitVerdict::Error,
                None,
                "WorkBuddy 领取结果未确认；未自动重发，请读取签到状态",
            ),
        },
        Err(error) if error == "平台未确认签到活动已启用；未发送领取请求" => (
            BenefitVerdict::NoClaimable,
            None,
            "当前没有已确认启用的签到活动；未发送领取请求",
        ),
        Err(_) => (
            BenefitVerdict::Error,
            None,
            "WorkBuddy 领取结果未确认；未自动重发，请读取签到状态",
        ),
    };
    let mut tx = source::begin(pool).await?;
    let snapshot = source::read(
        &mut tx,
        account,
        crate::trae_auth::NativeReadPurpose::Benefits,
    )
    .await?;
    if snapshot.wire.as_deref() != Ok(wire.as_str()) {
        return Err(ERROR.into());
    }
    let binding_cipher = snapshot.raw.ok_or(ERROR)?;
    let login_key = WorkBuddyAuth::benefit_session_key(r.region, &r.login_account_id)?;
    let login_cipher: Option<String> =
        sqlx::query_scalar("SELECT value_enc FROM app_secrets WHERE name=?")
            .bind(&login_key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ERROR)?;
    let login_cipher = login_cipher.ok_or(ERROR)?;
    tx.commit().await.map_err(|_| ERROR)?;
    let run = BenefitRunRecord {
        id: 0,
        account_id: account.id.clone(),
        platform: account.platform.clone(),
        window_key: window,
        campaign_key: Some("daily-checkin".into()),
        verdict,
        amount,
        message: message.into(),
        manual,
        created_at,
    };
    owner
        .finish_guarded(
            pool,
            BenefitClaimResult { run, outcome: None },
            &[
                (&secret_name(account), &binding_cipher),
                (&login_key, &login_cipher),
            ],
        )
        .await
}

fn cached_result(run: BenefitRunRecord) -> FlightResult {
    let outcome = run.verdict.settles_window().then(|| ClaimOutcome {
        platform: ClaimPlatform::parse(&run.platform).expect("validated WorkBuddy platform"),
        campaign_id: "daily-checkin".into(),
        campaign_key: "daily-checkin".into(),
        status: "CLAIMED".into(),
        replayed: true,
        granted: false,
        amount: None,
        kind: Some("CREDITS".into()),
        claimed_at: Some(run.created_at.clone()),
        expires_at: None,
        message: "已有领取收据；未再次请求或发放".into(),
    });
    Ok(BenefitClaimResult { run, outcome })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::HeaderMap, routing::post, Json, Router};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Fixture {
        db: crate::db::Db,
        center: BenefitsCenter,
        statuses: Arc<AtomicUsize>,
        claims: Arc<AtomicUsize>,
        server: tokio::task::JoinHandle<()>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        active: Arc<AtomicBool>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }
    async fn fixture(response: Value) -> Fixture {
        let statuses = Arc::new(AtomicUsize::new(0));
        let status_count = statuses.clone();
        let claims = Arc::new(AtomicUsize::new(0));
        let claim_count = claims.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let on_enter = entered.clone();
        let release = Arc::new(tokio::sync::Notify::new());
        let on_release = release.clone();
        let active = Arc::new(AtomicBool::new(true));
        let status_active = active.clone();
        let app=Router::new()
            .route("/v2/billing/meter/checkin-activity-status",post(move||{let (count,active)=(status_count.clone(),status_active.clone());async move{
                count.fetch_add(1,Ordering::SeqCst);Json(json!({"code":0,"data":{"active":active.load(Ordering::SeqCst),"today_checked_in":false,"daily_credit":100}}))}}))
            .route("/v2/billing/meter/daily-checkin",post(move|headers:HeaderMap,Json(body):Json<Value>|{
                let (count,response,entered,release)=(claim_count.clone(),response.clone(),on_enter.clone(),on_release.clone());async move {
                    assert_eq!(body,json!({})); assert!(!headers.contains_key("x-refresh-token"));
                    assert!(headers["authorization"].to_str().unwrap().starts_with("Bearer fixture-"));
                    count.fetch_add(1,Ordering::SeqCst);
                    if response["hold"]==true {entered.notify_one();release.notified().await;}
                    Json(response)
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let mut center = BenefitsCenter::new(db.pool().clone());
        center.workbuddy_endpoint = Some(endpoint.clone());
        for (region, uid) in [
            (WorkBuddyRegion::Global, "fixture-global-uid"),
            (WorkBuddyRegion::Cn, "fixture-cn-uid"),
        ] {
            WorkBuddyAuth::with_test_origin_for_region(db.pool().clone(), endpoint.clone(), region)
                .test_seed_account("workbuddy", uid, "fixture-access", "fixture-refresh")
                .await;
        }
        Fixture {
            db,
            center,
            statuses,
            claims,
            server,
            entered,
            release,
            active,
        }
    }
    async fn bind_all(f: &Fixture, cfg: &BenefitsConfig) {
        for account in &cfg.accounts {
            let before = f
                .center
                .benefit_auth_source(cfg, &account.id)
                .await
                .unwrap();
            let after = f
                .center
                .bind_workbuddy_benefit_auth(cfg, &account.id, "workbuddy", &before.source_revision)
                .await
                .unwrap();
            assert_eq!(after.source, "native_login");
            assert!(after.identity.unwrap().identity_hash.is_some());
        }
    }

    fn config() -> BenefitsConfig {
        BenefitsConfig {
            enabled: true,
            auto_claim: true,
            auto_claim_timezone: "Asia/Shanghai".into(),
            accounts: vec![
                BenefitAccount {
                    id: "wb-global".into(),
                    platform: "workbuddy".into(),
                    label: "WorkBuddy 国际版".into(),
                    enabled: true,
                },
                BenefitAccount {
                    id: "wb-cn".into(),
                    platform: "workbuddy_cn".into(),
                    label: "WorkBuddy 中国版".into(),
                    enabled: true,
                },
            ],
            ..BenefitsConfig::default()
        }
    }

    #[test]
    fn generic_bearer_headers_are_not_a_workbuddy_login_source() {
        assert!(
            crate::client_session::request_headers("fixture-token", ClaimPlatform::Qoder).is_ok()
        );
        assert!(
            crate::client_session::request_headers("fixture-token", ClaimPlatform::Workbuddy)
                .is_err()
        );
        assert!(crate::client_session::request_headers(
            "fixture-token",
            ClaimPlatform::WorkbuddyCn
        )
        .is_err());
    }

    #[tokio::test]
    async fn both_native_platforms_accept_config_and_missing_binding_stays_local() {
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let center = BenefitsCenter::new(db.pool().clone());
        let cfg = config();
        cfg.validate().unwrap();
        let view = center.overview(&cfg, None).await.unwrap();
        assert_eq!(view.accounts.len(), 2);
        for account in view.accounts {
            assert!(!account.has_token);
            assert!(account.status.is_none());
            assert_eq!(account.auth_source.source, "none");
            assert!(account.error.is_some());
        }
    }

    #[tokio::test]
    async fn local_binding_and_all_switches_precede_http_then_auto_and_manual_share_receipts() {
        let f = fixture(json!({"code":0,"data":{"credit":100}})).await;
        let mut cfg = config();
        cfg.enabled = false;
        bind_all(&f, &cfg).await;
        assert_eq!(f.statuses.load(Ordering::SeqCst), 0);
        assert_eq!(f.claims.load(Ordering::SeqCst), 0);
        let before = DateTime::parse_from_rfc3339("2026-10-18T12:04:00+08:00")
            .unwrap()
            .with_timezone(&Local);
        let after = DateTime::parse_from_rfc3339("2026-10-18T12:05:00+08:00")
            .unwrap()
            .with_timezone(&Local);
        for a in &cfg.accounts {
            cfg.account_claim_times.insert(a.id.clone(), "12:05".into());
        }
        assert!(f
            .center
            .auto_tick(&cfg, None, after)
            .await
            .unwrap()
            .is_empty());
        cfg.enabled = true;
        cfg.auto_claim = false;
        assert!(f
            .center
            .auto_tick(&cfg, None, after)
            .await
            .unwrap()
            .is_empty());
        cfg.auto_claim = true;
        assert!(f
            .center
            .auto_tick(&cfg, None, before)
            .await
            .unwrap()
            .is_empty());
        cfg.scheduler_mode = "windows_task".into();
        assert!(f
            .center
            .auto_tick(&cfg, None, after)
            .await
            .unwrap()
            .is_empty());
        cfg.scheduler_mode = "app".into();
        assert_eq!(f.statuses.load(Ordering::SeqCst), 0);
        let view = f.center.overview(&cfg, None).await.unwrap();
        assert!(view
            .accounts
            .iter()
            .all(|a| a.status.as_ref().unwrap().claimable));
        let result = f.center.auto_tick(&cfg, None, after).await.unwrap();
        assert_eq!(result.len(), 2);
        assert!(result
            .iter()
            .all(|r| r.run.verdict == BenefitVerdict::Granted
                && r.run.amount == Some(100.0)
                && !r.run.manual));
        assert_eq!(f.claims.load(Ordering::SeqCst), 2);
        let repeat = f.center.auto_tick(&cfg, None, after).await.unwrap();
        assert!(repeat.iter().all(|r| r.outcome.as_ref().unwrap().replayed));
        let manual = f.center.claim_now(&cfg, None, "wb-cn").await.unwrap();
        assert!(manual.outcome.unwrap().replayed);
        assert_eq!(f.claims.load(Ordering::SeqCst), 2);
        assert_eq!(f.center.runs(None, 50).await.unwrap().len(), 2);
        assert_eq!(cfg.trigger_minutes(), vec![600, 725]);
    }

    #[tokio::test]
    async fn binding_revision_region_and_changed_principal_reject_without_http_or_client_import() {
        let f = fixture(json!({"credit":100})).await;
        let cfg = config();
        bind_all(&f, &cfg).await;
        assert!(f
            .center
            .bind_workbuddy_benefit_auth(&cfg, "wb-cn", "workbuddy", "stale")
            .await
            .is_err());
        assert!(f
            .center
            .set_token(&cfg, "wb-cn", "fixture-token")
            .await
            .is_err());
        let key = WorkBuddyAuth::benefit_session_key(WorkBuddyRegion::Cn, "workbuddy").unwrap();
        let before = repo::get_secret(f.db.pool(), &key).await.unwrap().unwrap();
        let session = WorkBuddyAuth::with_test_origin_for_region(
            f.db.pool().clone(),
            f.center.workbuddy_endpoint.clone().unwrap(),
            WorkBuddyRegion::Cn,
        );
        session
            .test_seed_account(
                "workbuddy",
                "fixture-other-user",
                "fixture-other-access",
                "fixture-other-refresh",
            )
            .await;
        let result = f.center.claim_now(&cfg, None, "wb-cn").await.unwrap();
        assert_eq!(result.run.verdict, BenefitVerdict::Error);
        assert_eq!(f.statuses.load(Ordering::SeqCst), 0);
        assert_eq!(f.claims.load(Ordering::SeqCst), 0);
        repo::set_secret(f.db.pool(), &key, &before).await.unwrap();
        f.center.clear_token(&cfg, "wb-cn").await.unwrap();
        assert_eq!(
            repo::get_secret(f.db.pool(), &key).await.unwrap(),
            Some(before)
        );
        assert_eq!(
            f.center
                .benefit_auth_source(&cfg, "wb-cn")
                .await
                .unwrap()
                .source,
            "none"
        );
    }

    #[tokio::test]
    async fn ambiguous_platform_result_records_error_and_blocks_all_later_delivery() {
        let f = fixture(json!(null)).await;
        let cfg = config();
        bind_all(&f, &cfg).await;
        let first = f.center.claim_now(&cfg, None, "wb-cn").await.unwrap();
        assert_eq!(first.run.verdict, BenefitVerdict::Error);
        assert_eq!(first.run.amount, None);
        let second = f.center.claim_now(&cfg, None, "wb-cn").await.unwrap();
        assert_eq!(second.run, first.run);
        assert_eq!(f.claims.load(Ordering::SeqCst), 1);
        assert_eq!(f.center.runs(Some("wb-cn"), 50).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn second_center_cannot_record_error_over_inflight_success() {
        let f = Arc::new(fixture(json!({"credit":100,"hold":true})).await);
        let cfg = config();
        bind_all(&f, &cfg).await;
        let mut second = BenefitsCenter::new(f.db.pool().clone());
        second.workbuddy_endpoint = f.center.workbuddy_endpoint.clone();
        let first_f = f.clone();
        let first_cfg = cfg.clone();
        let first =
            tokio::spawn(async move { first_f.center.claim_now(&first_cfg, None, "wb-cn").await });
        tokio::time::timeout(Duration::from_secs(5), f.entered.notified())
            .await
            .unwrap();
        let competing = second.claim_now(&cfg, None, "wb-cn").await;
        f.release.notify_one();
        let completed = first.await.unwrap().unwrap();
        assert_eq!(completed.run.verdict, BenefitVerdict::Granted);
        assert!(competing.is_err());
        assert_eq!(f.claims.load(Ordering::SeqCst), 1);
        assert_eq!(f.center.runs(Some("wb-cn"), 50).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn review_expected_old_day_cannot_dispatch_on_the_new_day() {
        let f = fixture(json!({"credit":100})).await;
        let region = WorkBuddyRegion::Cn;
        let key = WorkBuddyAuth::benefit_session_key(region, "workbuddy").unwrap();
        let cipher = repo::get_secret(f.db.pool(), &key).await.unwrap().unwrap();
        let (_, principal, _) = WorkBuddyAuth::benefit_identity(&cipher, region).unwrap();
        let service = auth(
            f.db.pool(),
            region,
            None,
            f.center.workbuddy_endpoint.as_deref(),
        )
        .unwrap();
        let result = service
            .claim_daily_bound("workbuddy", Some(&principal), Some("2000-01-01"))
            .await;
        assert!(result.is_err());
        assert_eq!(f.statuses.load(Ordering::SeqCst), 0);
        assert_eq!(f.claims.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn review_known_inactive_window_can_only_recheck_on_explicit_manual_action() {
        let f = fixture(json!({"credit":100})).await;
        let mut cfg = config();
        cfg.accounts.retain(|a| a.id == "wb-cn");
        cfg.auto_claim_after_hour = 0;
        bind_all(&f, &cfg).await;
        f.active.store(false, Ordering::SeqCst);
        let first = f.center.claim_now(&cfg, None, "wb-cn").await.unwrap();
        assert_eq!(first.run.verdict, BenefitVerdict::NoClaimable);
        assert_eq!(f.claims.load(Ordering::SeqCst), 0);
        f.active.store(true, Ordering::SeqCst);
        let calls = f.statuses.load(Ordering::SeqCst);
        let automatic = f.center.auto_tick(&cfg, None, Local::now()).await.unwrap();
        assert_eq!(automatic[0].run.verdict, BenefitVerdict::NoClaimable);
        assert_eq!(f.statuses.load(Ordering::SeqCst), calls);
        let manual = f.center.claim_now(&cfg, None, "wb-cn").await.unwrap();
        assert_eq!(manual.run.verdict, BenefitVerdict::Granted);
        assert_eq!(f.claims.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn file_receipt_and_owned_headless_origin_gate_the_same_native_claim_path() {
        let f = fixture(json!({"credit":100})).await;
        let root = std::env::temp_dir().canonicalize().unwrap();
        let directory = root.join(format!("llmgw-wb-receipt-{}", uuid::Uuid::new_v4()));
        assert_eq!(directory.parent(), Some(root.as_path()));
        std::fs::create_dir(&directory).unwrap();
        let db_path = directory.join("gateway.sqlite");
        let db = crate::db::Db::connect_path(&db_path).await.unwrap();
        let mut center = BenefitsCenter::new(db.pool().clone());
        center.workbuddy_endpoint = f.center.workbuddy_endpoint.clone();
        WorkBuddyAuth::with_test_origin_for_region(
            db.pool().clone(),
            center.workbuddy_endpoint.clone().unwrap(),
            WorkBuddyRegion::Cn,
        )
        .test_seed_account(
            "workbuddy",
            "fixture-cn-uid",
            "fixture-access",
            "fixture-refresh",
        )
        .await;
        let mut cfg = config();
        cfg.accounts.retain(|a| a.id == "wb-cn");
        cfg.auto_claim_after_hour = 0;
        cfg.scheduler_mode = "windows_task".into();
        let before = center.benefit_auth_source(&cfg, "wb-cn").await.unwrap();
        center
            .bind_workbuddy_benefit_auth(&cfg, "wb-cn", "workbuddy", &before.source_revision)
            .await
            .unwrap();
        let receipt_dir = directory.join("benefits-receipts");
        std::fs::create_dir(&receipt_dir).unwrap();
        let today = receipt::day(&cfg, Local::now());
        let receipt_path = receipt_dir.join(format!("{today}-wb-cn.json"));
        std::fs::write(&receipt_path, b"corrupt").unwrap();
        assert!(center.claim_now(&cfg, None, "wb-cn").await.is_err());
        assert_eq!(f.claims.load(Ordering::SeqCst), 0);
        assert_eq!(f.statuses.load(Ordering::SeqCst), 0);
        std::fs::remove_file(&receipt_path).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&receipt_path)
            .unwrap();
        let origin = ReceiptOrigin::persist(&mut file, &directory, &receipt_path).unwrap();
        let result = center
            .claim_with_origin(&cfg, None, "wb-cn", false, Local::now(), Some(origin))
            .await
            .unwrap();
        assert_eq!(result.run.verdict, BenefitVerdict::Granted);
        assert!(!result.run.manual);
        drop(file);
        std::fs::write(
            &receipt_path,
            serde_json::to_vec(&json!({"account_id":"wb-cn","result":result})).unwrap(),
        )
        .unwrap();
        let repeated = center.claim_now(&cfg, None, "wb-cn").await.unwrap();
        assert!(repeated.outcome.unwrap().replayed);
        assert_eq!(f.claims.load(Ordering::SeqCst), 1);
        std::fs::write(&receipt_path, b"corrupt").unwrap();
        assert!(center.claim_now(&cfg, None, "wb-cn").await.is_err());
        assert_eq!(f.claims.load(Ordering::SeqCst), 1);
        drop(center);
        db.pool().close().await;
        drop(db);
        std::fs::remove_file(&receipt_path).unwrap();
        std::fs::remove_dir(&receipt_dir).unwrap();
        for attempt in 0..10 {
            match std::fs::remove_file(&db_path) {
                Ok(()) => break,
                Err(e)
                    if cfg!(windows)
                        && matches!(e.raw_os_error(), Some(32 | 33))
                        && attempt < 9 =>
                {
                    tokio::time::sleep(Duration::from_millis(25)).await
                }
                Err(e) => panic!("owned fixture cleanup failed: {e}"),
            }
        }
        std::fs::remove_dir(&directory).unwrap();
    }
}
