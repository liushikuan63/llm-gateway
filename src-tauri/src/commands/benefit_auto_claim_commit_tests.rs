use super::{commit_config_with, ConfigCommit};
use crate::{
    benefit_config::{AccountAutoClaimChange, BenefitAccount},
    config::AppConfig,
};
use parking_lot::RwLock;

fn account(id: &str, platform: &str) -> BenefitAccount {
    BenefitAccount {
        id: id.into(),
        platform: platform.into(),
        label: format!("Owned {id}"),
        enabled: true,
    }
}
fn legacy() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.benefits.enabled = true;
    cfg.benefits.auto_claim = true;
    cfg.benefits.auto_claim_after_hour = 12;
    cfg.benefits.auto_claim_after_minute = 5;
    cfg.benefits.auto_claim_timezone = "Asia/Shanghai".into();
    cfg.benefits.accounts = vec![account("a", "trae_cn")];
    cfg.benefits
        .account_claim_times
        .insert("a".into(), "00:05".into());
    cfg
}

#[test]
fn full_config_new_slot_is_off_even_when_payload_copies_an_allowed_flag() {
    let initial = legacy();
    let current = RwLock::new(initial.clone());
    let mut payload = initial;
    payload.benefits.accounts.push(account("b", "trae_cn"));
    payload.benefits.account_auto_claim.insert("b".into(), true);
    let saved =
        commit_config_with(&current, ConfigCommit::Save(&payload), |_| Ok(()), |_| {}).unwrap();
    assert_eq!(saved.benefits.account_auto_claim.get("b"), Some(&false));
    assert!(saved.benefits.account_auto_claim_allowed("a"));
    assert_eq!(saved.benefits.trigger_minutes(), vec![5, 725]);
}

#[test]
fn full_config_stale_payload_cannot_reenable_the_latest_disabled_slot() {
    let mut initial = legacy();
    initial
        .benefits
        .account_auto_claim
        .insert("a".into(), false);
    let current = RwLock::new(initial.clone());
    let mut stale = initial;
    stale.benefits.account_auto_claim.insert("a".into(), true);
    let saved =
        commit_config_with(&current, ConfigCommit::Save(&stale), |_| Ok(()), |_| {}).unwrap();
    assert!(!saved.benefits.account_auto_claim_allowed("a"));
}

#[test]
fn old_unchanged_slot_without_permission_field_keeps_primary_schedule() {
    let initial = legacy();
    let current = RwLock::new(initial.clone());
    let saved =
        commit_config_with(&current, ConfigCommit::Save(&initial), |_| Ok(()), |_| {}).unwrap();
    assert!(saved.benefits.account_auto_claim.is_empty());
    assert!(saved.benefits.auto_claim);
    assert_eq!(saved.benefits.trigger_minutes(), vec![5, 725]);
}

fn change(cfg: &AppConfig, allowed: bool) -> AccountAutoClaimChange {
    AccountAutoClaimChange {
        account_id: "a".into(),
        platform: "trae_cn".into(),
        expected_revision: cfg.benefits.settings_revision(),
        allowed,
    }
}

#[test]
fn platform_replacement_requires_fresh_auto_opt_in() {
    let mut initial = legacy();
    initial.benefits.account_auto_claim.insert("a".into(), true);
    let current = RwLock::new(initial.clone());
    let mut next = initial;
    next.benefits.accounts[0].platform = "qoder_cn".into();
    let saved =
        commit_config_with(&current, ConfigCommit::Save(&next), |_| Ok(()), |_| {}).unwrap();
    assert!(!saved.benefits.account_auto_claim_allowed("a"));
}

#[test]
fn missing_field_from_old_whole_config_does_not_reset_latest_off() {
    let mut initial = legacy();
    initial
        .benefits
        .account_auto_claim
        .insert("a".into(), false);
    let mut raw = serde_json::to_value(&initial).unwrap();
    raw["benefits"]
        .as_object_mut()
        .unwrap()
        .remove("account_auto_claim");
    let old: AppConfig = serde_json::from_value(raw).unwrap();
    let current = RwLock::new(initial);
    let saved = commit_config_with(&current, ConfigCommit::Save(&old), |_| Ok(()), |_| {}).unwrap();
    assert!(!saved.benefits.account_auto_claim_allowed("a"));
}

#[test]
fn narrow_save_preserves_other_fields_and_reopens_the_owned_policy_file() {
    let mut initial = legacy();
    initial
        .benefits
        .account_auto_claim
        .insert("a".into(), false);
    let input = change(&initial, true);
    let current = RwLock::new(initial);
    current.write().search.max_results = 3;
    let root =
        std::env::temp_dir().join(format!("llmgw-slot-policy-owned-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("config.toml");
    let saved = commit_config_with(
        &current,
        ConfigCommit::AccountAutoClaim(&input),
        |cfg| {
            std::fs::write(&file, toml::to_string_pretty(cfg).unwrap())
                .map_err(|error| error.to_string())
        },
        |_| {},
    )
    .unwrap();
    assert!(saved.benefits.account_auto_claim_allowed("a"));
    assert_eq!(saved.search.max_results, 3);
    assert_eq!(saved.benefits.trigger_minutes(), vec![5, 725]);
    let (reopened, _) = AppConfig::load_or_init_at(&file).unwrap();
    assert_eq!(reopened.benefits.account_auto_claim.get("a"), Some(&true));
    assert_eq!(
        reopened.benefits.settings_revision(),
        saved.benefits.settings_revision()
    );
    assert_eq!(reopened.search.max_results, 3);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_persistence_never_publishes_or_changes_the_permission() {
    let mut initial = legacy();
    initial
        .benefits
        .account_auto_claim
        .insert("a".into(), false);
    let input = change(&initial, true);
    let revision = initial.benefits.settings_revision();
    let current = RwLock::new(initial);
    assert!(commit_config_with(
        &current,
        ConfigCommit::AccountAutoClaim(&input),
        |_| Err("owned disk failure".into()),
        |_| panic!("failed persistence must not publish")
    )
    .is_err());
    assert!(!current.read().benefits.account_auto_claim_allowed("a"));
    assert_eq!(current.read().benefits.settings_revision(), revision);
}

#[test]
fn stale_revision_wrong_platform_and_unknown_slot_cannot_save() {
    let initial = legacy();
    let valid = change(&initial, false);
    let current = RwLock::new(initial.clone());
    for mode in 0..3 {
        let mut input = valid.clone();
        match mode {
            0 => input.expected_revision = "0".repeat(64),
            1 => input.platform = "qoder_cn".into(),
            _ => input.account_id = "missing".into(),
        };
        assert!(commit_config_with(
            &current,
            ConfigCommit::AccountAutoClaim(&input),
            |_| panic!("invalid scope must not persist"),
            |_| panic!("invalid scope must not publish")
        )
        .is_err());
        assert_eq!(
            current.read().benefits.settings_revision(),
            initial.benefits.settings_revision()
        );
    }
    let mut raw = serde_json::to_value(valid).unwrap();
    raw["verified"] = serde_json::json!(true);
    assert!(serde_json::from_value::<AccountAutoClaimChange>(raw).is_err());
}

#[test]
fn concurrent_same_revision_allows_only_one_permission_commit() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };
    let mut initial = legacy();
    initial
        .benefits
        .account_auto_claim
        .insert("a".into(), false);
    let input = change(&initial, true);
    let current = Arc::new(RwLock::new(initial));
    let barrier = Arc::new(Barrier::new(3));
    let writes = Arc::new(AtomicUsize::new(0));
    let workers = (0..2)
        .map(|_| {
            let current = current.clone();
            let barrier = barrier.clone();
            let writes = writes.clone();
            let input = input.clone();
            std::thread::spawn(move || {
                barrier.wait();
                commit_config_with(
                    &current,
                    ConfigCommit::AccountAutoClaim(&input),
                    |_| {
                        writes.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                    |_| {},
                )
                .is_ok()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|ok| *ok)
            .count(),
        1
    );
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(current.read().benefits.account_auto_claim_allowed("a"));
}

#[test]
fn disabled_account_and_global_off_can_be_preconfigured_without_enabling_them() {
    let mut initial = legacy();
    initial.benefits.auto_claim = false;
    initial.benefits.enabled = false;
    initial.benefits.accounts[0].enabled = false;
    let input = change(&initial, false);
    let current = RwLock::new(initial);
    let saved = commit_config_with(
        &current,
        ConfigCommit::AccountAutoClaim(&input),
        |_| Ok(()),
        |_| {},
    )
    .unwrap();
    assert!(!saved.benefits.account_auto_claim_allowed("a"));
    assert!(
        !saved.benefits.auto_claim
            && !saved.benefits.enabled
            && !saved.benefits.accounts[0].enabled
    );
}
