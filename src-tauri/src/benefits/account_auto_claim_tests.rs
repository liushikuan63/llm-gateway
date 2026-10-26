//! Slot permission consumers with artificial distinct native UIDs. No supplier calls.
use super::*;
use crate::{benefit_task, config::AppConfig, db::Db};
use serde_json::json;

#[derive(Default)]
struct Observed {
    calls: parking_lot::Mutex<Vec<(String, bool)>>,
    center: i64,
}
impl Observed {
    fn mutations(&self) -> Vec<String> {
        self.calls
            .lock()
            .iter()
            .filter(|(_, mutation)| *mutation)
            .map(|(token, _)| token.clone())
            .collect()
    }
}
#[async_trait]
impl BenefitTransport for Observed {
    async fn request(
        &self,
        platform: ClaimPlatform,
        token: &str,
        _proxy: Option<&str>,
        campaign: Option<&str>,
    ) -> Result<Value, String> {
        assert_eq!(platform, ClaimPlatform::TraeCn);
        let session = crate::client_session::SessionCredential::parse(token, platform).unwrap();
        let authorization = session.headers["authorization"].clone();
        assert!(authorization.starts_with("Cloud-IDE-JWT synthetic-"));
        self.calls.lock().push((authorization, campaign.is_some()));
        if campaign.is_some() {
            return Ok(
                json!({"campaignId":"owned-daily","campaignKey":"owned-window","status":"CLAIMED","replayed":false,"benefit":{"kind":"CREDITS","amount":8}}),
            );
        }
        Ok(
            json!({"claimable":true,"campaigns":[{"campaignId":"owned-daily","campaignKey":"owned-window","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT","startAt":self.center-3600,"endAt":self.center+3600,"benefit":{"kind":"CREDITS","amount":8}}]}),
        )
    }
}
fn config(mode: &str, flags: Value) -> BenefitsConfig {
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
    );
    serde_json::from_value(json!({"enabled":true,"auto_claim":true,"auto_claim_after_hour":0,"auto_claim_after_minute":0,"auto_claim_timezone":"local","scheduler_mode":mode,"accounts":[{"id":"a","platform":"trae_cn","label":"Owned A","enabled":true},{"id":"b","platform":"trae_cn","label":"Owned B","enabled":true}],"account_auto_claim":flags})).unwrap()
}
async fn center(db: &Db, now: DateTime<Local>) -> (BenefitsCenter, Arc<Observed>) {
    for (slot, uid, token) in [
        ("a", "owned-a", "synthetic-a"),
        ("b", "owned-b", "synthetic-b"),
    ] {
        source_tests::synthetic_native(
            db,
            slot,
            uid,
            token,
            Utc::now().timestamp_millis() + 60_000,
            true,
        )
        .await;
    }
    let observed = Arc::new(Observed {
        calls: Default::default(),
        center: now.timestamp(),
    });
    (
        BenefitsCenter::with_transport(db.pool().clone(), observed.clone()),
        observed,
    )
}

#[tokio::test]
async fn disabled_slot_auto_is_excluded_while_distinct_same_platform_uid_can_run() {
    let cfg = config("app", json!({"a":false,"b":true}));
    let now = Local::now();
    let db = Db::connect_in_memory().await.unwrap();
    let (center, observed) = center(&db, now).await;
    let results = center.auto_tick(&cfg, None, now).await.unwrap();
    assert_eq!(
        results
            .iter()
            .map(|result| result.run.account_id.as_str())
            .collect::<Vec<_>>(),
        vec!["b"]
    );
    assert_eq!(observed.mutations(), vec!["Cloud-IDE-JWT synthetic-b"]);
    assert!(
        observed
            .calls
            .lock()
            .iter()
            .all(|(token, _)| !token.ends_with("synthetic-a")),
        "disabled automatic slot must not even query status"
    );
}

#[tokio::test]
async fn direct_automatic_entry_cannot_bypass_disabled_slot_or_create_an_owner() {
    let cfg = config("app", json!({"a":false,"b":true}));
    let now = Local::now();
    let db = Db::connect_in_memory().await.unwrap();
    let (center, observed) = center(&db, now).await;
    let result = center.claim_at(&cfg, None, "a", false, now).await;
    assert!(
        result.is_err(),
        "a direct automatic call must honor the same slot gate"
    );
    assert!(observed.calls.lock().is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM benefit_principal_claims")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn headless_owner_skips_disabled_slot_before_any_daily_receipt_or_transport() {
    let cfg = config("windows_task", json!({"a":false,"b":true}));
    let root = std::env::temp_dir().join(format!("llmgw-slot-auto-owned-{}", uuid::Uuid::new_v4()));
    let db = Db::connect_path(&root.join("gateway.db")).await.unwrap();
    let now = Local::now();
    let (center, observed) = center(&db, now).await;
    let app = AppConfig {
        benefits: cfg,
        ..Default::default()
    };
    let result = benefit_task::execute_once(&app, &center, &root)
        .await
        .unwrap();
    let claims = result["claims"].as_array().unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0]["account_id"], "b");
    assert_eq!(observed.mutations(), vec!["Cloud-IDE-JWT synthetic-b"]);
    let receipts = std::fs::read_dir(root.join("benefits-receipts"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        receipts.iter().all(|name| !name.ends_with("-a.json")),
        "off slot must not acquire an unknown daily receipt"
    );
    assert!(receipts.iter().any(|name| name.ends_with("-b.json")));
    assert!(observed
        .calls
        .lock()
        .iter()
        .all(|(token, _)| !token.ends_with("synthetic-a")));
    db.pool().close().await;
    drop(center);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn manual_claim_still_works_with_both_global_and_slot_auto_off() {
    let mut cfg = config("app", json!({"a":false,"b":false}));
    cfg.auto_claim = false;
    let now = Local::now();
    let db = Db::connect_in_memory().await.unwrap();
    let (center, observed) = center(&db, now).await;
    let result = center.claim_at(&cfg, None, "a", true, now).await.unwrap();
    assert_eq!(result.run.verdict, BenefitVerdict::Granted);
    assert!(result.run.manual);
    assert_eq!(observed.mutations(), vec!["Cloud-IDE-JWT synthetic-a"]);
}

#[tokio::test]
async fn true_slot_flag_does_not_override_the_global_auto_switch() {
    let mut cfg = config("app", json!({"a":true,"b":true}));
    cfg.auto_claim = false;
    let now = Local::now();
    let db = Db::connect_in_memory().await.unwrap();
    let (center, observed) = center(&db, now).await;
    assert!(center.auto_tick(&cfg, None, now).await.unwrap().is_empty());
    assert!(observed.calls.lock().is_empty());
}

#[test]
fn slot_permission_rejects_non_boolean_and_unknown_account_keys() {
    let raw = serde_json::to_value(config("app", json!({}))).unwrap();
    let mut wrong_type = raw.clone();
    wrong_type["account_auto_claim"] = json!({"a":"false"});
    assert!(serde_json::from_value::<BenefitsConfig>(wrong_type).is_err());
    let mut unknown = raw;
    unknown["account_auto_claim"] = json!({"missing":true});
    assert!(serde_json::from_value::<BenefitsConfig>(unknown)
        .unwrap()
        .validate()
        .is_err());
}

#[test]
fn windows_trigger_plan_excludes_an_off_slot_but_preserves_primary_time() {
    let mut raw =
        serde_json::to_value(config("windows_task", json!({"a":false,"b":true}))).unwrap();
    raw["auto_claim_after_hour"] = json!(12);
    raw["auto_claim_after_minute"] = json!(5);
    raw["account_claim_times"] = json!({"a":"00:05","b":"12:05"});
    let cfg: BenefitsConfig = serde_json::from_value(raw).unwrap();
    assert_eq!(cfg.trigger_minutes(), vec![725]);
}

#[tokio::test]
async fn legacy_missing_slot_field_keeps_existing_automatic_accounts_and_schedule() {
    let mut raw = serde_json::to_value(config("app", json!({}))).unwrap();
    raw.as_object_mut().unwrap().remove("account_auto_claim");
    let cfg: BenefitsConfig = serde_json::from_value(raw.clone()).unwrap();
    let now = Local::now();
    let db = Db::connect_in_memory().await.unwrap();
    let (center, observed) = center(&db, now).await;
    assert_eq!(center.auto_tick(&cfg, None, now).await.unwrap().len(), 2);
    assert_eq!(observed.mutations().len(), 2);
    raw["auto_claim_after_hour"] = json!(12);
    raw["auto_claim_after_minute"] = json!(5);
    raw["account_claim_times"] = json!({"a":"00:05","b":"12:05"});
    let schedule: BenefitsConfig = serde_json::from_value(raw).unwrap();
    assert_eq!(schedule.trigger_minutes(), vec![5, 725]);
}
