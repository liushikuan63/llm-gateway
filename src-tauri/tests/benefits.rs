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
        accounts: vec![BenefitAccount {
            id: "intl".into(),
            platform: "qoder".into(),
            label: "离线账号".into(),
            enabled: true,
        }],
    }
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
