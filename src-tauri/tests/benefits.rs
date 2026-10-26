//! Offline campaign fixtures only. No client credentials or public endpoint is contacted.
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use async_trait::async_trait;
use chrono::{DateTime, Local, TimeZone};
use llm_gateway_lib::benefit_config::{BenefitAccount, BenefitsConfig};
use llm_gateway_lib::benefits::{
    auth_value, parse_claim, parse_status, platform_for_host, BenefitTransport, BenefitVerdict,
    BenefitsCenter, ClaimPlatform,
};
use llm_gateway_lib::{
    crypto,
    db::{repo, Db},
};
use serde_json::{json, Value};

const TOKEN: &str = "offline-benefit-token-649291";

fn init_crypto() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // Fixed synthetic master key avoids touching any real user's key file in this test binary.
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
        );
    });
}

fn cfg() -> BenefitsConfig {
    BenefitsConfig {
        enabled: true,
        auto_claim: false,
        auto_claim_after_hour: 10,
        auto_claim_after_minute: 0,
        auto_claim_timezone: "local".into(),
        scheduler_mode: "app".into(),
        account_claim_times: Default::default(),
        account_auto_claim: Default::default(),
        accounts: vec![BenefitAccount {
            id: "intl".into(),
            platform: "qoder".into(),
            label: "离线账号".into(),
            enabled: true,
        }],
    }
}

#[test]
fn individual_times_preserve_qoder_default_and_reject_invalid_overrides() {
    let mut settings = cfg();
    settings.auto_claim_after_hour = 12;
    settings.auto_claim_after_minute = 5;
    settings.auto_claim_timezone = "Asia/Shanghai".into();
    settings.accounts.push(BenefitAccount {
        id: "early".into(),
        platform: "qoder_cn".into(),
        label: "early".into(),
        enabled: true,
    });
    settings
        .account_claim_times
        .insert("early".into(), "00:05".into());
    let at = |time: &str| {
        DateTime::parse_from_rfc3339(time)
            .unwrap()
            .with_timezone(&Local)
    };
    assert!(!settings.account_is_due("early", at("2026-10-14T00:04:59+08:00")));
    assert!(settings.account_is_due("early", at("2026-10-14T00:05:00+08:00")));
    assert!(!settings.account_is_due("intl", at("2026-10-14T00:05:00+08:00")));
    assert!(!settings.account_is_due("intl", at("2026-10-14T12:04:59+08:00")));
    assert!(settings.account_is_due("intl", at("2026-10-14T12:05:00+08:00")));
    assert_eq!(settings.trigger_minutes(), vec![5, 725]);
    settings.validate().unwrap();
    for time in ["24:00", "00:60", "0:05", "00:05:00", "xx:yy"] {
        settings
            .account_claim_times
            .insert("early".into(), time.into());
        assert!(settings.validate().is_err());
    }
    settings.account_claim_times.clear();
    settings
        .account_claim_times
        .insert("missing".into(), "00:05".into());
    assert!(settings.validate().is_err());
    let legacy: BenefitsConfig =
        toml::from_str("auto_claim_after_hour = 12\nauto_claim_after_minute = 5").unwrap();
    assert!(legacy.account_claim_times.is_empty());
}

#[tokio::test]
async fn external_daily_owner_skips_app_tick_and_reserves_one_execution() {
    let now = Local::now();
    let (_db, center, mock) = setup(now).await;
    let mut config = cfg();
    config.auto_claim = true;
    config.scheduler_mode = "windows_task".into();
    config.auto_claim_after_hour = 0;
    assert!(center
        .auto_tick(&config, None, now)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(mock.gets.load(Ordering::SeqCst), 0);
    let root = std::env::temp_dir().join(format!("llmgw-benefit-test-{}", uuid::Uuid::new_v4()));
    let mut app = llm_gateway_lib::config::AppConfig {
        benefits: config,
        ..Default::default()
    };
    let result = llm_gateway_lib::benefit_task::execute_once(&app, &center, &root)
        .await
        .unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    let second = llm_gateway_lib::benefit_task::execute_once(&app, &center, &root)
        .await
        .unwrap();
    assert_eq!(second["claims"][0]["status"], "receipt_exists_no_retry");
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    app.benefits.enabled = false;
    let count = mock.gets.load(Ordering::SeqCst);
    assert_eq!(
        llm_gateway_lib::benefit_task::execute_once(&app, &center, &root)
            .await
            .unwrap()["status"],
        "disabled_or_not_due"
    );
    assert_eq!(mock.gets.load(Ordering::SeqCst), count);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn session_is_encrypted_and_platform_mismatch_preserves_previous_secret() {
    let now = Local::now();
    let (db, center, _) = setup(now).await;
    let raw=json!({"schema_version":1,"platform":"qoder","headers":{"authorization":"Bearer synthetic-session-secret"}}).to_string();
    let session =
        llm_gateway_lib::client_session::SessionCredential::parse(&raw, ClaimPlatform::Qoder)
            .unwrap();
    center.set_session(&cfg(), "intl", &session).await.unwrap();
    let encrypted = repo::get_secret(db.pool(), "benefit:qoder:intl:token")
        .await
        .unwrap()
        .unwrap();
    assert!(!encrypted.contains("synthetic-session-secret"));
    assert!(crypto::decrypt(&encrypted)
        .unwrap()
        .contains("synthetic-session-secret"));
    let mut bad = session;
    bad.platform = ClaimPlatform::QoderCn;
    assert!(center.set_session(&cfg(), "intl", &bad).await.is_err());
    assert_eq!(
        repo::get_secret(db.pool(), "benefit:qoder:intl:token")
            .await
            .unwrap()
            .unwrap(),
        encrypted
    );
}

#[tokio::test]
async fn a_failed_account_does_not_block_the_other_and_uncertain_receipts_are_not_retried() {
    struct FirstFails(Arc<Mock>);
    #[async_trait]
    impl BenefitTransport for FirstFails {
        async fn request(
            &self,
            platform: ClaimPlatform,
            token: &str,
            proxy: Option<&str>,
            campaign_id: Option<&str>,
        ) -> Result<Value, String> {
            if platform == ClaimPlatform::Qoder {
                return Err("offline auth failure".into());
            }
            self.0.request(platform, token, proxy, campaign_id).await
        }
    }
    init_crypto();
    let now = Local::now();
    let db = Db::connect_in_memory().await.unwrap();
    let mock = Mock::new(now);
    let center =
        BenefitsCenter::with_transport(db.pool().clone(), Arc::new(FirstFails(mock.clone())));
    let mut config = cfg();
    config.auto_claim = true;
    config.scheduler_mode = "windows_task".into();
    config.auto_claim_after_hour = 0;
    config.accounts.push(BenefitAccount {
        id: "cn".into(),
        platform: "qoder_cn".into(),
        label: "离线 CN".into(),
        enabled: true,
    });
    for a in &config.accounts {
        center.set_token(&config, &a.id, TOKEN).await.unwrap();
    }
    let app = llm_gateway_lib::config::AppConfig {
        benefits: config,
        ..Default::default()
    };
    let root = std::env::temp_dir().join(format!("llmgw-benefit-test-{}", uuid::Uuid::new_v4()));
    assert!(
        llm_gateway_lib::benefit_task::execute_once(&app, &center, &root)
            .await
            .is_err()
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    let summary: Value = serde_json::from_slice(
        &std::fs::read(root.join("benefits-receipts/last-run.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(summary["claims"][0]["result"]["run"]["verdict"], "error");
    assert_eq!(summary["claims"][1]["result"]["run"]["verdict"], "granted");
    assert!(
        llm_gateway_lib::benefit_task::execute_once(&app, &center, &root)
            .await
            .is_err()
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(root).unwrap();
}

fn beijing(raw: &str) -> DateTime<Local> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap()
        .with_timezone(&Local)
}

fn campaign(id: &str, key: &str, status: &str, start: i64, end: i64) -> Value {
    json!({"campaignId":id,"campaignKey":key,"claimStatus":status,"actionType":"CLAIM_BENEFIT",
        "startAt":start,"endAt":end,"benefit":{"kind":"CREDITS","amount":100,"validity":{"mode":"RELATIVE_DAYS","days":30}}})
}

fn status(id: &str, key: &str, state: &str, start: i64, end: i64) -> Value {
    json!({"uid":"not-exported","claimable":true,"campaignUrl":"https://fixture.invalid/private",
        "campaigns":[campaign("permanent","resident","CLAIMED",0,1),campaign(id,key,state,start,end)]})
}

fn outcome(id: &str, key: &str, replayed: bool) -> Value {
    json!({"campaignId":id,"campaignKey":key,"status":"CLAIMED","replayed":replayed,
        "benefit":{"kind":"CREDITS","amount":100,"validity":{"mode":"RELATIVE_DAYS","days":30}},
        "claimedAt":"2026-10-10T02:01:00Z","expiresAt":"2026-11-09T02:01:00Z"})
}

struct Mock {
    gets: AtomicUsize,
    posts: AtomicUsize,
    delay: AtomicU64,
    state: parking_lot::Mutex<Value>,
    reply: parking_lot::Mutex<Result<Value, String>>,
    platforms: parking_lot::Mutex<Vec<ClaimPlatform>>,
}

impl Mock {
    fn new(now: DateTime<Local>) -> Arc<Self> {
        Arc::new(Self {
            gets: AtomicUsize::new(0),
            posts: AtomicUsize::new(0),
            delay: AtomicU64::new(0),
            state: parking_lot::Mutex::new(status(
                "daily-1",
                "today",
                "CLAIMABLE",
                now.timestamp() - 3600,
                now.timestamp() + 3600,
            )),
            reply: parking_lot::Mutex::new(Ok(outcome("daily-1", "today", false))),
            platforms: parking_lot::Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl BenefitTransport for Mock {
    async fn request(
        &self,
        platform: ClaimPlatform,
        token: &str,
        _proxy: Option<&str>,
        campaign_id: Option<&str>,
    ) -> Result<Value, String> {
        assert_eq!(auth_value(token), format!("Bearer {TOKEN}"));
        self.platforms.lock().push(platform);
        if let Some(id) = campaign_id {
            assert_ne!(id, "permanent", "必须选择可领活动，不能重放常驻活动");
            self.posts.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(
                self.delay.load(Ordering::SeqCst),
            ))
            .await;
            self.reply.lock().clone()
        } else {
            self.gets.fetch_add(1, Ordering::SeqCst);
            Ok(self.state.lock().clone())
        }
    }
}

async fn setup(now: DateTime<Local>) -> (Db, BenefitsCenter, Arc<Mock>) {
    init_crypto();
    let db = Db::connect_in_memory().await.unwrap();
    let mock = Mock::new(now);
    let center = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    center.set_token(&cfg(), "intl", TOKEN).await.unwrap();
    (db, center, mock)
}

#[test]
fn configuration_defaults_and_invalid_accounts_are_explicit() {
    let defaults: BenefitsConfig = toml::from_str("").unwrap();
    assert!(!defaults.enabled && !defaults.auto_claim);
    assert_eq!(defaults.auto_claim_after_hour, 10);
    assert!(defaults.validate().is_ok());
    for id in ["", "../escape", "a b", "a:secret"] {
        let mut config = cfg();
        config.accounts[0].id = id.into();
        assert!(config.validate().is_err());
    }
    let mut config = cfg();
    config.accounts.push(config.accounts[0].clone());
    assert!(config.validate().is_err());
    let mut config = cfg();
    config.accounts[0].platform = "trae".into();
    assert!(config.validate().is_err());
    let mut config = cfg();
    config.auto_claim_after_hour = 24;
    assert!(config.validate().is_err());
}

#[test]
fn fixed_hosts_and_authorization_shapes_have_negative_controls() {
    assert_eq!(
        platform_for_host("OPENAPI.QODER.SH"),
        Some(ClaimPlatform::Qoder)
    );
    assert_eq!(
        platform_for_host("openapi.qoder.com.cn"),
        Some(ClaimPlatform::QoderCn)
    );
    for host in [
        "qoder.sh",
        "openapi.qoder.sh.evil.com",
        "openapi.qoder.com.cn.evil.com",
        "localhost",
    ] {
        assert_eq!(platform_for_host(host), None);
    }
    assert_eq!(auth_value(TOKEN), format!("Bearer {TOKEN}"));
    assert_eq!(
        auth_value(&format!("Bearer {TOKEN}")),
        format!("Bearer {TOKEN}")
    );
}

#[test]
fn campaigns_use_individual_status_and_claims_require_verified_credit_fields() {
    let mut raw = status("daily-1", "today", "CLAIMABLE", 1, 2);
    raw["claimable"] = json!(false);
    let parsed = parse_status(ClaimPlatform::Qoder, &raw).unwrap();
    assert_eq!(parsed.next_claimable().unwrap().campaign_id, "daily-1");
    assert!(!parsed.warnings.is_empty());
    let parsed = parse_status(
        ClaimPlatform::Qoder,
        &status("daily-1", "today", "CLAIMED", 1, 2),
    )
    .unwrap();
    assert!(parsed.next_claimable().is_none());
    assert!(!parsed.claimable);
    assert!(!parsed.warnings.is_empty());
    let replay = parse_claim(
        ClaimPlatform::Qoder,
        "daily-1",
        &outcome("daily-1", "today", true),
    )
    .unwrap();
    assert!(replay.replayed && !replay.granted);
    let granted = parse_claim(
        ClaimPlatform::Qoder,
        "daily-1",
        &outcome("daily-1", "today", false),
    )
    .unwrap();
    assert!(granted.granted && !granted.replayed);
    for field in [
        Value::Null,
        json!({"kind":"OTHER","amount":100}),
        json!({"kind":"CREDITS"}),
        json!({"kind":"CREDITS","amount":-1}),
    ] {
        let mut raw = outcome("daily-1", "today", false);
        raw["benefit"] = field;
        assert!(parse_claim(ClaimPlatform::Qoder, "daily-1", &raw).is_err());
    }
    assert!(parse_claim(
        ClaimPlatform::Qoder,
        "wrong-id",
        &outcome("daily-1", "today", false)
    )
    .is_err());
}

#[tokio::test]
async fn tokens_are_encrypted_masked_and_isolated_by_platform() {
    let now = Local::now();
    let (db, center, mock) = setup(now).await;
    let encrypted = repo::get_secret(db.pool(), "benefit:qoder:intl:token")
        .await
        .unwrap()
        .unwrap();
    assert!(!encrypted.contains(TOKEN));
    assert_eq!(crypto::decrypt(&encrypted).unwrap(), TOKEN);
    let overview = center.overview(&cfg(), None).await.unwrap();
    assert!(overview.accounts[0].has_token);
    assert_eq!(
        overview.accounts[0].token_masked.as_deref(),
        Some("********")
    );
    assert!(!serde_json::to_string(&overview).unwrap().contains(TOKEN));
    assert!(!toml::to_string(&cfg()).unwrap().contains(TOKEN));
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0, "overview只读GET");
    let mut config = cfg();
    config.accounts[0].platform = "qoder_cn".into();
    let overview = center.overview(&config, None).await.unwrap();
    assert!(!overview.accounts[0].has_token);
    assert!(overview.accounts[0].status.is_none());
    assert_eq!(
        mock.gets.load(Ordering::SeqCst),
        1,
        "不向国内host发送国际token"
    );
    center.clear_token(&cfg(), "intl").await.unwrap();
    assert!(repo::get_secret(db.pool(), "benefit:qoder:intl:token")
        .await
        .unwrap()
        .is_none());
    assert!(center.set_token(&cfg(), "intl", "").await.is_err());
    assert!(center
        .set_token(&cfg(), "intl", "Bearer x\r\nHost: evil")
        .await
        .is_err());
}

#[tokio::test]
async fn disabled_and_missing_credentials_never_contact_transport() {
    let now = Local::now();
    let (_db, center, mock) = setup(now).await;
    let mut config = cfg();
    config.enabled = false;
    config.auto_claim = true;
    center.overview(&config, None).await.unwrap();
    assert!(center
        .claim_at(&config, None, "intl", true, now)
        .await
        .is_err());
    assert!(center
        .auto_tick(&config, None, now)
        .await
        .unwrap()
        .is_empty());
    config.enabled = true;
    config.accounts[0].enabled = false;
    center.overview(&config, None).await.unwrap();
    assert!(center
        .claim_at(&config, None, "intl", true, now)
        .await
        .is_err());
    assert_eq!(mock.gets.load(Ordering::SeqCst), 0);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    config.accounts[0].enabled = true;
    center.clear_token(&config, "intl").await.unwrap();
    let result = center
        .claim_at(&config, None, "intl", true, now)
        .await
        .unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Error);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 0);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_requests_share_one_post_and_persistent_settlement_survives_new_center() {
    let now = Local::now();
    let (db, center, mock) = setup(now).await;
    mock.delay.store(100, Ordering::SeqCst);
    let config = cfg();
    let (one, two) = tokio::join!(
        center.claim_at(&config, None, "intl", true, now),
        center.claim_at(&config, None, "intl", true, now)
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!(one.run.id, two.run.id);
    assert_eq!(one.run.verdict, BenefitVerdict::Granted);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    let skipped = center
        .claim_at(&config, None, "intl", true, now)
        .await
        .unwrap();
    assert_eq!(skipped.run.verdict, BenefitVerdict::Skipped);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    let fresh = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    let result = fresh
        .claim_at(&config, None, "intl", true, now)
        .await
        .unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Skipped);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 2);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(
        repo::benefit_window_settled(db.pool(), "intl", &one.run.window_key)
            .await
            .unwrap()
    );
    llm_gateway_lib::db::migrations::run(db.pool())
        .await
        .unwrap();
    assert!(
        repo::benefit_window_settled(db.pool(), "intl", &one.run.window_key)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn concurrent_errors_are_shared_but_a_later_request_requeries_before_retry() {
    let now = Local::now();
    let (db, center, mock) = setup(now).await;
    mock.delay.store(100, Ordering::SeqCst);
    *mock.reply.lock() = Err(format!(
        "private failure {TOKEN} https://private.invalid/path"
    ));
    let config = cfg();
    let (one, two) = tokio::join!(
        center.claim_at(&config, None, "intl", true, now),
        center.claim_at(&config, None, "intl", true, now)
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!(one.run.id, two.run.id);
    assert_eq!(one.run.verdict, BenefitVerdict::Error);
    assert!(!serde_json::to_string(&one).unwrap().contains(TOKEN));
    assert!(!one.run.message.contains("private.invalid"));
    assert!(
        !repo::benefit_window_settled(db.pool(), "intl", &one.run.window_key)
            .await
            .unwrap()
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    *mock.reply.lock() = Ok(outcome("daily-1", "today", false));
    let result = center
        .claim_at(&config, None, "intl", true, now)
        .await
        .unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Granted);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 2);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn uncertain_post_success_is_reconciled_by_get_without_blind_repost() {
    let now = Local::now();
    let (_db, center, mock) = setup(now).await;
    *mock.reply.lock() = Err("权益请求连接失败或超时；未自动重发领取".into());
    assert_eq!(
        center
            .claim_at(&cfg(), None, "intl", true, now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Error
    );
    *mock.state.lock() = status(
        "daily-1",
        "today",
        "CLAIMED",
        now.timestamp() - 3600,
        now.timestamp() + 3600,
    );
    let result = center
        .claim_at(&cfg(), None, "intl", true, now)
        .await
        .unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Replayed);
    assert!(result.run.amount.is_none());
    assert_eq!(mock.gets.load(Ordering::SeqCst), 2);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn observed_resident_claim_does_not_hide_a_later_daily_campaign() {
    let before = beijing("2026-10-10T09:59:00+08:00");
    let due = beijing("2026-10-10T10:00:00+08:00");
    let (_db, center, mock) = setup(before).await;
    let resident = campaign(
        "permanent",
        "resident",
        "CLAIMED",
        beijing("2026-10-01T00:00:00+08:00").timestamp(),
        beijing("2026-11-01T00:00:00+08:00").timestamp(),
    );
    *mock.state.lock() = json!({"claimable":false,"campaigns":[resident.clone()]});
    let observed = center
        .claim_at(&cfg(), None, "intl", true, before)
        .await
        .unwrap();
    assert_eq!(observed.run.verdict, BenefitVerdict::Replayed);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);

    *mock.state.lock() = json!({"claimable":true,"campaigns":[
        resident,
        campaign("daily-2", "tomorrow", "CLAIMABLE", due.timestamp(), due.timestamp() + 86400)
    ]});
    *mock.reply.lock() = Ok(outcome("daily-2", "tomorrow", false));
    let next = center
        .claim_at(&cfg(), None, "intl", true, due)
        .await
        .unwrap();
    assert_eq!(next.run.verdict, BenefitVerdict::Granted);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 2);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);

    let repeated = center
        .claim_at(&cfg(), None, "intl", true, due)
        .await
        .unwrap();
    assert_eq!(repeated.run.verdict, BenefitVerdict::Skipped);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 2);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn actual_campaign_window_allows_0959_then_1000_and_survives_midnight() {
    let start = beijing("2026-10-09T10:00:00+08:00");
    let boundary = beijing("2026-10-10T10:00:00+08:00");
    let before = beijing("2026-10-10T09:59:00+08:00");
    let (_db, center, mock) = setup(before).await;
    *mock.state.lock() = status(
        "daily-1",
        "today",
        "CLAIMABLE",
        start.timestamp(),
        boundary.timestamp(),
    );
    assert_eq!(
        center
            .claim_at(&cfg(), None, "intl", true, before)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Granted
    );
    *mock.state.lock() = status(
        "daily-2",
        "tomorrow",
        "CLAIMABLE",
        boundary.timestamp(),
        boundary.timestamp() + 86400,
    );
    *mock.reply.lock() = Ok(outcome("daily-2", "tomorrow", false));
    assert_eq!(
        center
            .claim_at(&cfg(), None, "intl", true, boundary)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Granted
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 2);

    let late = beijing("2026-10-10T23:59:00+08:00");
    let next = beijing("2026-10-11T00:01:00+08:00");
    let (_db, center, mock) = setup(late).await;
    *mock.state.lock() = status(
        "daily-1",
        "today",
        "CLAIMABLE",
        boundary.timestamp(),
        boundary.timestamp() + 86400,
    );
    let first = center
        .claim_at(&cfg(), None, "intl", true, late)
        .await
        .unwrap();
    let repeated = center
        .claim_at(&cfg(), None, "intl", true, next)
        .await
        .unwrap();
    assert_eq!(first.run.window_key, repeated.run.window_key);
    assert_eq!(repeated.run.verdict, BenefitVerdict::Skipped);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn automatic_claim_respects_switch_local_hour_and_actual_window() {
    let early = Local
        .with_ymd_and_hms(2026, 10, 10, 9, 59, 0)
        .single()
        .unwrap();
    let due = Local
        .with_ymd_and_hms(2026, 10, 10, 10, 0, 0)
        .single()
        .unwrap();
    let (_db, center, mock) = setup(due).await;
    let mut config = cfg();
    assert!(center
        .auto_tick(&config, None, due)
        .await
        .unwrap()
        .is_empty());
    config.auto_claim = true;
    assert!(center
        .auto_tick(&config, None, early)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(mock.gets.load(Ordering::SeqCst), 0);
    let results = center.auto_tick(&config, None, due).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(!results[0].run.manual);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    let after = due + chrono::Duration::hours(2);
    let result = center
        .claim_at(&config, None, "intl", true, after)
        .await
        .unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::NoClaimable);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1, "窗口外不POST");
}

#[tokio::test]
async fn automatic_claim_honors_minute_and_explicit_beijing_clock() {
    let before = beijing("2026-10-10T04:04:59Z");
    let due = beijing("2026-10-10T04:05:00Z");
    let (_db, center, mock) = setup(due).await;
    let mut raw = serde_json::to_value(cfg()).unwrap();
    raw["auto_claim"] = json!(true);
    raw["auto_claim_after_hour"] = json!(12);
    raw["auto_claim_after_minute"] = json!(5);
    raw["auto_claim_timezone"] = json!("Asia/Shanghai");
    let config: BenefitsConfig = serde_json::from_value(raw).unwrap();
    assert!(center
        .auto_tick(&config, None, before)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        mock.gets.load(Ordering::SeqCst),
        0,
        "12:04:59 must not query or claim"
    );
    let result = center.auto_tick(&config, None, due).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        serde_json::to_value(&config).unwrap()["auto_claim_after_minute"],
        5
    );
    center.auto_tick(&config, None, due).await.unwrap();
    assert_eq!(
        mock.posts.load(Ordering::SeqCst),
        1,
        "same settled activity must not be claimed again"
    );
}

#[test]
fn benefit_schedule_defaults_remain_compatible_and_invalid_minutes_are_rejected() {
    let legacy: BenefitsConfig =
        serde_json::from_value(json!({"auto_claim_after_hour": 10})).unwrap();
    let encoded = serde_json::to_value(&legacy).unwrap();
    assert_eq!(encoded["auto_claim_after_minute"], 0);
    assert_eq!(encoded["auto_claim_timezone"], "local");
    for (field, value) in [
        ("auto_claim_after_minute", json!(60)),
        ("auto_claim_timezone", json!("arbitrary-zone")),
    ] {
        let mut bad = encoded.clone();
        bad[field] = value;
        assert!(serde_json::from_value::<BenefitsConfig>(bad)
            .unwrap()
            .validate()
            .is_err());
    }
}

#[tokio::test]
async fn replayed_and_mismatched_campaign_keys_do_not_report_new_credits() {
    let now = Local::now();
    let (db, center, mock) = setup(now).await;
    *mock.reply.lock() = Ok(outcome("daily-1", "today", true));
    let result = center
        .claim_at(&cfg(), None, "intl", true, now)
        .await
        .unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Replayed);
    assert!(result.run.amount.is_none());
    assert!(!result.outcome.unwrap().granted);
    assert!(
        repo::benefit_window_settled(db.pool(), "intl", &result.run.window_key)
            .await
            .unwrap()
    );
    let (_db, center, mock) = setup(now).await;
    *mock.reply.lock() = Ok(outcome("daily-1", "different-key", false));
    let result = center
        .claim_at(&cfg(), None, "intl", true, now)
        .await
        .unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Error);
    assert!(result.outcome.is_none());
}

#[tokio::test]
async fn settled_ledger_survives_closing_and_reopening_the_sqlite_file() {
    struct TempDirectory(std::path::PathBuf);
    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    init_crypto();
    let directory =
        std::env::temp_dir().join(format!("llmgw-benefit-ledger-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let directory = TempDirectory(directory);
    let path = directory.0.join("gateway.db");
    let now = Local::now();
    let mock = Mock::new(now);
    let db = Db::connect_path(&path).await.unwrap();
    let center = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    center.set_token(&cfg(), "intl", TOKEN).await.unwrap();
    let first = center
        .claim_at(&cfg(), None, "intl", true, now)
        .await
        .unwrap();
    assert_eq!(first.run.verdict, BenefitVerdict::Granted);
    drop(center);
    db.pool().close().await;

    let reopened = Db::connect_path(&path).await.unwrap();
    let center = BenefitsCenter::with_transport(reopened.pool().clone(), mock.clone());
    let repeated = center
        .claim_at(&cfg(), None, "intl", true, now)
        .await
        .unwrap();
    assert_eq!(repeated.run.verdict, BenefitVerdict::Skipped);
    assert_eq!(first.run.window_key, repeated.run.window_key);
    assert_eq!(mock.gets.load(Ordering::SeqCst), 2);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(center.runs(Some("intl"), 200).await.unwrap().len(), 2);
    drop(center);
    reopened.pool().close().await;
}

// Pause only the first POST. A second center can finish while that POST is
// outstanding, so the persisted success shortcut cannot conceal the race.
struct CrossInstanceTransport {
    gets: AtomicUsize,
    posts: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    now: DateTime<Local>,
}

#[async_trait]
impl BenefitTransport for CrossInstanceTransport {
    async fn request(
        &self,
        platform: ClaimPlatform,
        token: &str,
        proxy: Option<&str>,
        campaign_id: Option<&str>,
    ) -> Result<Value, String> {
        assert_eq!(platform, ClaimPlatform::Qoder);
        assert_eq!(auth_value(token), format!("Bearer {TOKEN}"));
        assert!(proxy.is_none());
        if let Some(id) = campaign_id {
            assert_eq!(id, "daily-1");
            if self.posts.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                tokio::time::timeout(std::time::Duration::from_secs(15), self.release.notified())
                    .await
                    .map_err(|_| "offline first POST was not released".to_string())?;
            }
            Ok(outcome("daily-1", "today", false))
        } else {
            self.gets.fetch_add(1, Ordering::SeqCst);
            Ok(status(
                "daily-1",
                "today",
                "CLAIMABLE",
                self.now.timestamp() - 3600,
                self.now.timestamp() + 3600,
            ))
        }
    }
}

async fn cross_instance_call(
    headless: bool,
    center: &BenefitsCenter,
    app: &llm_gateway_lib::config::AppConfig,
    root: &std::path::Path,
    now: DateTime<Local>,
) -> Result<Value, String> {
    if headless {
        llm_gateway_lib::benefit_task::execute_once(app, center, root).await
    } else {
        center
            .claim_at(&app.benefits, None, "intl", true, now)
            .await
            .map(|result| serde_json::to_value(result).unwrap())
    }
}

async fn assert_cross_instance_single_post(first_headless: bool, second_headless: bool) {
    init_crypto();
    let root = std::env::temp_dir().join(format!(
        "llmgw-benefit-cross-instance-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("gateway.db");
    // Separate pools/connections to the same owned SQLite file, not pool clones.
    let first_db = Db::connect_path(&path).await.unwrap();
    let second_db = Db::connect_path(&path).await.unwrap();
    let now = Local::now();
    let transport = Arc::new(CrossInstanceTransport {
        gets: AtomicUsize::new(0),
        posts: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        now,
    });
    let first = BenefitsCenter::with_transport(first_db.pool().clone(), transport.clone());
    let second = BenefitsCenter::with_transport(second_db.pool().clone(), transport.clone());
    let mut settings = cfg();
    settings.auto_claim = true;
    settings.auto_claim_after_hour = 0;
    settings.scheduler_mode = "windows_task".into();
    let app = llm_gateway_lib::config::AppConfig {
        benefits: settings,
        ..Default::default()
    };
    first.set_token(&app.benefits, "intl", TOKEN).await.unwrap();
    let first_call = cross_instance_call(first_headless, &first, &app, &root, now);
    let second_call = async {
        let entered = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            transport.entered.notified(),
        )
        .await;
        let result = if entered.is_ok() {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                cross_instance_call(second_headless, &second, &app, &root, now),
            )
            .await
            .map_err(|_| "second call did not finish while first POST was outstanding".to_string())
            .and_then(|result| result)
        } else {
            Err("first call did not reach POST".into())
        };
        // Release even on fixture timeout; no task is left waiting on this gate.
        transport.release.notify_one();
        result
    };
    let (one, two) = tokio::join!(first_call, second_call);
    let runs = first.runs(Some("intl"), 200).await.unwrap();
    drop(first);
    drop(second);
    close_workers(first_db.pool()).await;
    close_workers(second_db.pool()).await;
    std::fs::remove_dir_all(&root).unwrap();
    assert!(one.is_ok(), "first flow failed: {one:?}");
    if first_headless {
        assert!(
            two.is_err(),
            "pending daily receipt must deny the concurrent manual call"
        );
        assert_eq!(runs.len(), 1);
    } else {
        assert!(two.is_ok(), "second flow failed: {two:?}");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].window_key, runs[1].window_key);
        assert_eq!(runs[1].verdict, BenefitVerdict::Skipped);
    }
    assert_eq!(transport.gets.load(Ordering::SeqCst), 2);
    eprintln!(
        "cross-instance evidence: first_headless={first_headless} second_headless={second_headless} gets={} posts={} runs={} verdicts={:?}",
        transport.gets.load(Ordering::SeqCst),
        transport.posts.load(Ordering::SeqCst),
        runs.len(),
        runs.iter().map(|run| &run.verdict).collect::<Vec<_>>()
    );
    assert_eq!(
        transport.posts.load(Ordering::SeqCst),
        1,
        "same account/window must share a durable reservation across independent centers"
    );
}

#[tokio::test]
async fn cross_instance_two_centers_share_one_post() {
    assert_cross_instance_single_post(false, false).await;
}

#[tokio::test]
async fn cross_instance_gui_then_headless_share_one_post() {
    assert_cross_instance_single_post(false, true).await;
}

#[tokio::test]
async fn cross_instance_headless_then_gui_share_one_post() {
    assert_cross_instance_single_post(true, false).await;
}

#[tokio::test]
async fn cross_instance_uncertain_automatic_attempt_is_not_reposted_by_new_center() {
    let now = Local::now();
    let (db, center, mock) = setup(now).await;
    let mut config = cfg();
    config.auto_claim = true;
    config.auto_claim_after_hour = 0;
    *mock.reply.lock() = Err("offline response lost after POST".into());
    let first = center
        .claim_at(&config, None, "intl", false, now)
        .await
        .unwrap();
    assert_eq!(first.run.verdict, BenefitVerdict::Error);
    assert!(!first.run.manual);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(center.runs(Some("intl"), 200).await.unwrap().len(), 1);
    let fresh = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    let second = fresh
        .claim_at(&config, None, "intl", false, now)
        .await
        .unwrap();
    assert!(!second.run.manual);
    assert_eq!(first.run.window_key, second.run.window_key);
    eprintln!(
        "automatic unknown-result evidence: gets={} posts={} first={:?} second={:?}",
        mock.gets.load(Ordering::SeqCst),
        mock.posts.load(Ordering::SeqCst),
        first.run.verdict,
        second.run.verdict
    );
    assert_eq!(
        mock.posts.load(Ordering::SeqCst),
        1,
        "an automatic request must not repost a window with an uncertain prior attempt"
    );
}

// Explicit connection close awaits each SQLite worker's shutdown; pool close alone
// may return while its dropped workers still hold Windows file handles.
async fn close_workers(pool: &sqlx::SqlitePool) {
    use sqlx::Connection;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while pool.size() > 0 {
            pool.acquire()
                .await
                .unwrap()
                .detach()
                .close()
                .await
                .unwrap();
        }
        pool.close().await;
    })
    .await
    .expect("all owned SQLite workers shut down");
}

struct FileFixture {
    root: std::path::PathBuf,
    db: Db,
    center: BenefitsCenter,
    mock: Arc<Mock>,
    now: DateTime<Local>,
}

impl FileFixture {
    async fn new() -> Self {
        init_crypto();
        let root = std::env::temp_dir().join(format!(
            "llmgw-benefit-reservation-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        let db = Db::connect_path(root.join("gateway.db")).await.unwrap();
        let now = Local::now();
        let mock = Mock::new(now);
        let center = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
        center.set_token(&cfg(), "intl", TOKEN).await.unwrap();
        Self {
            root,
            db,
            center,
            mock,
            now,
        }
    }

    fn receipt(&self, bytes: &[u8]) -> std::path::PathBuf {
        let directory = self.root.join("benefits-receipts");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{}-intl.json", self.now.format("%Y-%m-%d")));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    async fn close(self) {
        drop(self.center);
        close_workers(self.db.pool()).await;
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

fn automatic() -> BenefitsConfig {
    let mut config = cfg();
    config.auto_claim = true;
    config.auto_claim_after_hour = 0;
    config
}

#[tokio::test]
async fn file_receipt_unknown_blocks_auto_and_manual_but_claimed_get_reconciles() {
    for bytes in [
        b"".as_slice(),
        b"broken",
        b"{\"status\":\"reserved_no_automatic_retry\"}",
        b"{\"result\":{\"run\":{\"verdict\":\"error\"}}}",
    ] {
        let f = FileFixture::new().await;
        let path = f.receipt(bytes);
        let auto = f
            .center
            .claim_at(&automatic(), None, "intl", false, f.now)
            .await;
        assert!(auto.is_err() || auto.unwrap().run.verdict == BenefitVerdict::Skipped);
        assert!(f
            .center
            .claim_at(&cfg(), None, "intl", true, f.now)
            .await
            .is_err());
        assert_eq!(f.mock.posts.load(Ordering::SeqCst), 0);
        *f.mock.state.lock() = status(
            "daily-1",
            "today",
            "CLAIMED",
            f.now.timestamp() - 3600,
            f.now.timestamp() + 3600,
        );
        let reconciled = f
            .center
            .claim_at(&cfg(), None, "intl", true, f.now)
            .await
            .unwrap();
        assert_eq!(reconciled.run.verdict, BenefitVerdict::Replayed);
        assert_eq!(f.mock.posts.load(Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read(path).unwrap(),
            bytes,
            "legacy receipt immutable"
        );
        f.close().await;
    }
}

#[tokio::test]
async fn file_completed_error_receipt_requires_exact_audit_and_allows_later_manual() {
    let f = FileFixture::new().await;
    *f.mock.reply.lock() = Err("offline lost response".into());
    let failed = f
        .center
        .claim_at(&automatic(), None, "intl", false, f.now)
        .await
        .unwrap();
    let bytes = serde_json::to_vec(&json!({"account_id":"intl","result":failed})).unwrap();
    let path = f.receipt(&bytes);
    let restarted = Db::connect_path(f.root.join("gateway.db")).await.unwrap();
    let center = BenefitsCenter::with_transport(restarted.pool().clone(), f.mock.clone());
    assert_eq!(
        center
            .claim_at(&automatic(), None, "intl", false, f.now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Skipped
    );
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 1);
    let mut forged: Value = serde_json::from_slice(&bytes).unwrap();
    forged["result"]["run"]["message"] = json!("changed");
    std::fs::write(&path, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(center
        .claim_at(&cfg(), None, "intl", true, f.now)
        .await
        .is_err());
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 1);
    std::fs::write(&path, &bytes).unwrap();
    *f.mock.reply.lock() = Ok(outcome("daily-1", "today", false));
    assert_eq!(
        center
            .claim_at(&cfg(), None, "intl", true, f.now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Granted
    );
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 2);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    drop(center);
    close_workers(restarted.pool()).await;
    f.close().await;
}

#[tokio::test]
async fn headless_origin_must_match_actual_file_database_parent() {
    let f = FileFixture::new().await;
    let wrong = f.root.join("unrelated-owned-directory");
    std::fs::create_dir(&wrong).unwrap();
    let mut settings = automatic();
    settings.scheduler_mode = "windows_task".into();
    let app = llm_gateway_lib::config::AppConfig {
        benefits: settings,
        ..Default::default()
    };
    assert!(
        llm_gateway_lib::benefit_task::execute_once(&app, &f.center, &wrong)
            .await
            .is_err()
    );
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 0);
    // The correctly attributed first owner remains usable; it still acquires the DB reservation.
    llm_gateway_lib::benefit_task::execute_once(&app, &f.center, &f.root)
        .await
        .unwrap();
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn legacy_error_audit_blocks_auto_but_unresolved_errors_do_not() {
    let now = Local::now();
    let (db, center, mock) = setup(now).await;
    let window = format!(
        "qoder:daily-1:{}:{}",
        now.timestamp() - 3600,
        now.timestamp() + 3600
    );
    let insert = |window: String| llm_gateway_lib::benefits::BenefitRunRecord {
        id: 0,
        account_id: "intl".into(),
        platform: "qoder".into(),
        window_key: window,
        campaign_key: None,
        verdict: BenefitVerdict::Error,
        amount: None,
        message: "legacy error".into(),
        manual: false,
        created_at: now.to_rfc3339(),
    };
    repo::insert_benefit_run(db.pool(), &insert(window))
        .await
        .unwrap();
    assert_eq!(
        center
            .claim_at(&automatic(), None, "intl", false, now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Skipped
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert_eq!(
        center
            .claim_at(&cfg(), None, "intl", true, now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Granted
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    let (db, center, mock) = setup(now).await;
    repo::insert_benefit_run(db.pool(), &insert("qoder:unresolved".into()))
        .await
        .unwrap();
    assert_eq!(
        center
            .claim_at(&automatic(), None, "intl", false, now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Granted
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

struct SlowGetTransport {
    mock: Arc<Mock>,
    first_post: tokio::sync::Notify,
    release_post: tokio::sync::Notify,
    second_get: tokio::sync::Notify,
    release_get: tokio::sync::Notify,
}

async fn notified(signal: &tokio::sync::Notify) {
    tokio::time::timeout(std::time::Duration::from_secs(5), signal.notified())
        .await
        .expect("fixture signal");
}

#[async_trait]
impl BenefitTransport for SlowGetTransport {
    async fn request(
        &self,
        _: ClaimPlatform,
        _: &str,
        _: Option<&str>,
        campaign: Option<&str>,
    ) -> Result<Value, String> {
        if campaign.is_some() {
            if self.mock.posts.fetch_add(1, Ordering::SeqCst) == 0 {
                self.first_post.notify_one();
                notified(&self.release_post).await;
                return Err("offline first attempt uncertain".into());
            }
            Ok(outcome("daily-1", "today", false))
        } else {
            if self.mock.gets.fetch_add(1, Ordering::SeqCst) == 1 {
                self.second_get.notify_one();
                notified(&self.release_get).await;
            }
            Ok(self.mock.state.lock().clone())
        }
    }
}

#[tokio::test]
async fn delayed_manual_get_cannot_turn_concurrent_intent_into_retry() {
    let f = FileFixture::new().await;
    let other = Db::connect_path(f.root.join("gateway.db")).await.unwrap();
    let transport = Arc::new(SlowGetTransport {
        mock: f.mock.clone(),
        first_post: Default::default(),
        release_post: Default::default(),
        second_get: Default::default(),
        release_get: Default::default(),
    });
    let first = Arc::new(BenefitsCenter::with_transport(
        f.db.pool().clone(),
        transport.clone(),
    ));
    let second = Arc::new(BenefitsCenter::with_transport(
        other.pool().clone(),
        transport.clone(),
    ));
    let now = f.now;
    let a = first.clone();
    let one = tokio::spawn(async move { a.claim_at(&cfg(), None, "intl", true, now).await });
    notified(&transport.first_post).await;
    let b = second.clone();
    let two = tokio::spawn(async move { b.claim_at(&cfg(), None, "intl", true, now).await });
    notified(&transport.second_get).await;
    transport.release_post.notify_one();
    assert_eq!(
        one.await.unwrap().unwrap().run.verdict,
        BenefitVerdict::Error
    );
    transport.release_get.notify_one();
    assert_eq!(
        two.await.unwrap().unwrap().run.verdict,
        BenefitVerdict::Skipped
    );
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        second
            .claim_at(&cfg(), None, "intl", true, now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Granted
    );
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 2);
    assert_eq!(f.mock.gets.load(Ordering::SeqCst), 3);
    drop(first);
    drop(second);
    close_workers(other.pool()).await;
    f.close().await;
}

#[tokio::test]
async fn actual_post_then_audit_failure_stays_unknown_and_claimed_get_reconciles() {
    let f = FileFixture::new().await;
    sqlx::query("CREATE TRIGGER offline_audit_failure BEFORE INSERT ON benefit_runs BEGIN SELECT RAISE(ABORT,'offline'); END")
        .execute(f.db.pool()).await.unwrap();
    assert!(f
        .center
        .claim_at(&cfg(), None, "intl", true, f.now)
        .await
        .is_err());
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 1);
    assert!(f.center.runs(None, 20).await.unwrap().is_empty());
    let raw: String = sqlx::query_scalar(
        "SELECT value_enc FROM app_secrets WHERE name LIKE 'benefit:reservation:%'",
    )
    .fetch_one(f.db.pool())
    .await
    .unwrap();
    assert!(crypto::decrypt(&raw)
        .unwrap()
        .contains("dispatch_started_outcome_unknown"));
    sqlx::query("DROP TRIGGER offline_audit_failure")
        .execute(f.db.pool())
        .await
        .unwrap();
    let reopened = Db::connect_path(f.root.join("gateway.db")).await.unwrap();
    let center = BenefitsCenter::with_transport(reopened.pool().clone(), f.mock.clone());
    for manual in [false, true] {
        assert_eq!(
            center
                .claim_at(&automatic(), None, "intl", manual, f.now)
                .await
                .unwrap()
                .run
                .verdict,
            BenefitVerdict::Skipped
        );
    }
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 1);
    *f.mock.state.lock() = status(
        "daily-1",
        "today",
        "CLAIMED",
        f.now.timestamp() - 3600,
        f.now.timestamp() + 3600,
    );
    assert_eq!(
        center
            .claim_at(&cfg(), None, "intl", true, f.now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Replayed
    );
    let after: String = sqlx::query_scalar(
        "SELECT value_enc FROM app_secrets WHERE name LIKE 'benefit:reservation:%'",
    )
    .fetch_one(f.db.pool())
    .await
    .unwrap();
    assert_eq!(
        after, raw,
        "GET reconciliation must not steal an unfinished owner's control row"
    );
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 1);
    drop(center);
    close_workers(reopened.pool()).await;
    f.close().await;
}

#[tokio::test]
async fn caller_disconnect_does_not_cancel_owner_or_release_unknown_reservation() {
    let f = FileFixture::new().await;
    let transport = Arc::new(CrossInstanceTransport {
        gets: AtomicUsize::new(0),
        posts: AtomicUsize::new(0),
        entered: Default::default(),
        release: Default::default(),
        now: f.now,
    });
    let owner = Arc::new(BenefitsCenter::with_transport(
        f.db.pool().clone(),
        transport.clone(),
    ));
    let other_db = Db::connect_path(f.root.join("gateway.db")).await.unwrap();
    let other = BenefitsCenter::with_transport(other_db.pool().clone(), transport.clone());
    let a = owner.clone();
    let now = f.now;
    let caller = tokio::spawn(async move { a.claim_at(&cfg(), None, "intl", true, now).await });
    notified(&transport.entered).await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert_eq!(
        other
            .claim_at(&cfg(), None, "intl", true, now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Skipped
    );
    transport.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let runs = other.runs(None, 20).await.unwrap();
            if runs.iter().any(|r| r.verdict == BenefitVerdict::Granted) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached owner commits");
    let runs = other.runs(None, 20).await.unwrap();
    assert_eq!(
        runs.iter()
            .filter(|r| r.verdict == BenefitVerdict::Granted)
            .count(),
        1
    );
    assert_eq!(transport.posts.load(Ordering::SeqCst), 1);
    drop(owner);
    drop(other);
    close_workers(other_db.pool()).await;
    f.close().await;
}

#[tokio::test]
async fn unverified_alias_cannot_bypass_unknown_but_another_platform_finishes_while_post_is_paused()
{
    let f = FileFixture::new().await;
    let slow = Arc::new(CrossInstanceTransport {
        gets: AtomicUsize::new(0),
        posts: AtomicUsize::new(0),
        entered: Default::default(),
        release: Default::default(),
        now: f.now,
    });
    let owner = Arc::new(BenefitsCenter::with_transport(
        f.db.pool().clone(),
        slow.clone(),
    ));
    let other_db = Db::connect_path(f.root.join("gateway.db")).await.unwrap();
    let other = BenefitsCenter::with_transport(other_db.pool().clone(), f.mock.clone());
    let a = owner.clone();
    let now = f.now;
    let pending = tokio::spawn(async move { a.claim_at(&cfg(), None, "intl", true, now).await });
    notified(&slow.entered).await;
    for (id, platform) in [("other", "qoder"), ("intl", "qoder_cn")] {
        let mut config = cfg();
        config.accounts[0].id = id.into();
        config.accounts[0].platform = platform.into();
        other.set_token(&config, id, TOKEN).await.unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            other.claim_at(&config, None, id, true, now),
        )
        .await
        .unwrap();
        if platform == "qoder" {
            assert_eq!(
                result.unwrap_err(),
                "旧领取预约没有可信主体归属，本窗口未发送领取请求",
                "different slot with the same raw token is not a verified independent UID"
            );
        } else {
            assert_eq!(result.unwrap().run.verdict, BenefitVerdict::Granted);
        }
    }
    assert_eq!(f.mock.posts.load(Ordering::SeqCst), 1);
    slow.release.notify_one();
    assert_eq!(
        pending.await.unwrap().unwrap().run.verdict,
        BenefitVerdict::Granted
    );
    assert_eq!(slow.posts.load(Ordering::SeqCst), 1);
    drop(owner);
    drop(other);
    close_workers(other_db.pool()).await;
    f.close().await;
}

#[tokio::test]
async fn completed_no_post_receipt_allows_a_new_window_but_yesterday_unknown_is_not_scanned() {
    for verdict in [BenefitVerdict::NoClaimable, BenefitVerdict::Skipped] {
        let f = FileFixture::new().await;
        let mut run = llm_gateway_lib::benefits::BenefitRunRecord {
            id: 0,
            account_id: "intl".into(),
            platform: "qoder".into(),
            window_key: "qoder:unresolved".into(),
            campaign_key: None,
            verdict,
            amount: None,
            message: "offline no POST".into(),
            manual: false,
            created_at: f.now.to_rfc3339(),
        };
        run.id = repo::insert_benefit_run(f.db.pool(), &run).await.unwrap();
        let bytes =
            serde_json::to_vec(&json!({"account_id":"intl","result":{"run":run,"outcome":null}}))
                .unwrap();
        let path = f.receipt(&bytes);
        assert_eq!(
            f.center
                .claim_at(&automatic(), None, "intl", false, f.now)
                .await
                .unwrap()
                .run
                .verdict,
            BenefitVerdict::Skipped
        );
        assert_eq!(f.mock.posts.load(Ordering::SeqCst), 0);
        assert_eq!(
            f.center
                .claim_at(&cfg(), None, "intl", true, f.now)
                .await
                .unwrap()
                .run
                .verdict,
            BenefitVerdict::Granted
        );
        assert_eq!(f.mock.posts.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        f.close().await;
    }
    let f = FileFixture::new().await;
    let tomorrow = f.now + chrono::Duration::days(1);
    let path = f.receipt(b"historical unknown");
    *f.mock.state.lock() = status(
        "daily-2",
        "tomorrow",
        "CLAIMABLE",
        tomorrow.timestamp() - 60,
        tomorrow.timestamp() + 3600,
    );
    *f.mock.reply.lock() = Ok(outcome("daily-2", "tomorrow", false));
    assert_eq!(
        f.center
            .claim_at(&automatic(), None, "intl", false, tomorrow)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Granted
    );
    assert_eq!(std::fs::read(path).unwrap(), b"historical unknown");
    f.close().await;
}

#[tokio::test]
async fn automatic_failure_remains_blocked_across_midnight_but_a_new_window_is_independent() {
    let now = Local::now();
    let (db, center, mock) = setup(now).await;
    *mock.state.lock() = status(
        "multi-day",
        "long",
        "CLAIMABLE",
        now.timestamp() - 3600,
        now.timestamp() + 3 * 86400,
    );
    *mock.reply.lock() = Err("offline lost response".into());
    assert_eq!(
        center
            .claim_at(&automatic(), None, "intl", false, now)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Error
    );
    let tomorrow = now + chrono::Duration::days(1);
    let fresh = BenefitsCenter::with_transport(db.pool().clone(), mock.clone());
    assert_eq!(
        fresh
            .claim_at(&automatic(), None, "intl", false, tomorrow)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Skipped
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    *mock.state.lock() = status(
        "new-window",
        "new",
        "CLAIMABLE",
        tomorrow.timestamp() - 60,
        tomorrow.timestamp() + 3600,
    );
    *mock.reply.lock() = Ok(outcome("new-window", "new", false));
    assert_eq!(
        fresh
            .claim_at(&automatic(), None, "intl", false, tomorrow)
            .await
            .unwrap()
            .run
            .verdict,
        BenefitVerdict::Granted
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reservation_corruption_and_insert_failure_deny_before_transport_post() {
    use sha2::{Digest, Sha256};
    for corrupt in [false, true] {
        let now = Local::now();
        let (db, center, mock) = setup(now).await;
        let window = format!(
            "qoder:daily-1:{}:{}",
            now.timestamp() - 3600,
            now.timestamp() + 3600
        );
        let key = format!(
            "benefit:reservation:v1:{:x}",
            Sha256::digest(serde_json::to_vec(&("qoder", "intl", &window)).unwrap())
        );
        if corrupt {
            repo::set_secret(db.pool(), &key, "broken-ciphertext")
                .await
                .unwrap();
        } else {
            sqlx::query("CREATE TRIGGER offline_insert_failure BEFORE INSERT ON app_secrets WHEN NEW.name LIKE 'benefit:reservation:%' BEGIN SELECT RAISE(ABORT,'offline'); END")
                .execute(db.pool()).await.unwrap();
        }
        assert!(center
            .claim_at(&cfg(), None, "intl", true, now)
            .await
            .is_err());
        assert_eq!(mock.gets.load(Ordering::SeqCst), 1);
        assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
        assert!(center.runs(None, 20).await.unwrap().is_empty());
        if corrupt {
            assert_eq!(
                repo::get_secret(db.pool(), &key).await.unwrap().as_deref(),
                Some("broken-ciphertext")
            );
        } else {
            sqlx::query("DROP TRIGGER offline_insert_failure")
                .execute(db.pool())
                .await
                .unwrap();
            assert_eq!(
                center
                    .claim_at(&cfg(), None, "intl", true, now)
                    .await
                    .unwrap()
                    .run
                    .verdict,
                BenefitVerdict::Granted
            );
            assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
        }
    }
}
