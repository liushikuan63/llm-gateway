//! Owned scheduler fault injection; never registers an actual Windows task.
use super::coordinate;
use crate::benefit_config::{BenefitAccount, BenefitsConfig};
use std::sync::{Arc, Mutex};

fn policy(mode: &str) -> BenefitsConfig {
    let mut cfg = BenefitsConfig {
        enabled: true,
        auto_claim: true,
        scheduler_mode: mode.into(),
        auto_claim_after_hour: 12,
        auto_claim_after_minute: 5,
        accounts: vec![BenefitAccount {
            id: "a".into(),
            platform: "trae_cn".into(),
            label: "Owned A".into(),
            enabled: true,
        }],
        ..BenefitsConfig::default()
    };
    cfg.account_claim_times.insert("a".into(), "00:05".into());
    cfg.account_auto_claim.insert("a".into(), true);
    cfg
}

#[derive(Default)]
struct Observed {
    applied: Option<BenefitsConfig>,
    plans: Vec<BenefitsConfig>,
    commits: usize,
    events: Vec<&'static str>,
}

async fn run(
    previous: BenefitsConfig,
    planned: BenefitsConfig,
    errors: Vec<bool>,
    persist_fails: bool,
) -> (Result<(), String>, Observed) {
    let observed = Arc::new(Mutex::new(Observed {
        applied: Some(previous.clone()),
        ..Observed::default()
    }));
    let scheduler = observed.clone();
    let commit = observed.clone();
    let latest = previous.clone();
    let result = coordinate(
        &previous,
        &planned,
        false,
        move |plan| {
            let mut state = scheduler.lock().unwrap();
            let index = state.plans.len();
            // Deliberately model Register-ScheduledTask succeeding before its
            // readback fails. A pre-mutation failure would miss the regression.
            state.applied = Some(plan.clone());
            state.plans.push(plan);
            state.events.push("register");
            std::future::ready(if errors.get(index).copied().unwrap_or(false) {
                Err("owned task readback failure".into())
            } else {
                Ok(())
            })
        },
        move || {
            let mut state = commit.lock().unwrap();
            state.commits += 1;
            state.events.push("commit");
            if persist_fails {
                Err("owned disk failure".into())
            } else {
                Ok(())
            }
        },
        move || latest.clone(),
    )
    .await;
    (
        result,
        Arc::try_unwrap(observed)
            .ok()
            .unwrap()
            .into_inner()
            .unwrap(),
    )
}

#[tokio::test]
async fn changed_task_then_readback_failure_restores_previous_without_commit() {
    let previous = policy("windows_task");
    let mut planned = previous.clone();
    planned.account_auto_claim.insert("a".into(), false);
    assert_eq!(previous.trigger_minutes(), vec![5, 725]);
    assert_eq!(planned.trigger_minutes(), vec![725]);
    let (result, observed) = run(previous.clone(), planned.clone(), vec![true, false], false).await;
    assert!(result.is_err());
    assert_eq!(observed.plans, vec![planned, previous.clone()]);
    assert_eq!(observed.applied, Some(previous));
    assert_eq!(observed.commits, 0);
    assert_eq!(observed.events, vec!["register", "register"]);
}

#[tokio::test]
async fn initial_registration_and_restore_failure_reports_unconfirmed_task() {
    let previous = policy("windows_task");
    let mut planned = previous.clone();
    planned.account_auto_claim.insert("a".into(), false);
    let (result, observed) = run(previous, planned, vec![true, true], false).await;
    assert!(result.unwrap_err().contains("后台任务恢复未确认"));
    assert_eq!(observed.plans.len(), 2);
    assert_eq!(observed.commits, 0);
}

#[tokio::test]
async fn failed_persistence_restores_previous_plan() {
    let previous = policy("windows_task");
    let mut planned = previous.clone();
    planned.auto_claim_after_minute = 6;
    let (result, observed) = run(previous.clone(), planned.clone(), vec![], true).await;
    assert_eq!(result.unwrap_err(), "owned disk failure");
    assert_eq!(observed.plans, vec![planned, previous.clone()]);
    assert_eq!(observed.applied, Some(previous));
    assert_eq!(observed.commits, 1);
    assert_eq!(observed.events, vec!["register", "commit", "register"]);
}

#[tokio::test]
async fn persistence_and_restore_failure_reports_unconfirmed_task() {
    let previous = policy("windows_task");
    let mut planned = previous.clone();
    planned.auto_claim_after_minute = 6;
    let (result, observed) = run(previous, planned, vec![false, true], true).await;
    assert!(result.unwrap_err().contains("后台任务恢复未确认"));
    assert_eq!(observed.plans.len(), 2);
    assert_eq!(observed.commits, 1);
}

#[tokio::test]
async fn app_to_windows_task_registers_before_commit() {
    let previous = policy("app");
    let mut planned = previous.clone();
    planned.scheduler_mode = "windows_task".into();
    let (result, observed) = run(previous, planned.clone(), vec![], false).await;
    result.unwrap();
    assert_eq!(observed.plans, vec![planned]);
    assert_eq!(observed.commits, 1);
    assert_eq!(observed.events, vec!["register", "commit"]);
}

#[tokio::test]
async fn windows_task_to_app_updates_task_before_commit() {
    let previous = policy("windows_task");
    let mut planned = previous.clone();
    planned.scheduler_mode = "app".into();
    let (result, observed) = run(previous, planned.clone(), vec![], false).await;
    result.unwrap();
    assert_eq!(observed.plans, vec![planned]);
    assert_eq!(observed.commits, 1);
    assert_eq!(observed.events, vec!["register", "commit"]);
}

#[tokio::test]
async fn app_only_permission_change_never_registers_a_task() {
    let previous = policy("app");
    let mut planned = previous.clone();
    planned.account_auto_claim.insert("a".into(), false);
    let (result, observed) = run(previous, planned, vec![true], false).await;
    result.unwrap();
    assert!(observed.plans.is_empty());
    assert_eq!(observed.commits, 1);
}

#[tokio::test]
async fn unchanged_windows_policy_does_not_rewrite_a_task() {
    let previous = policy("windows_task");
    let (result, observed) = run(previous.clone(), previous, vec![true], false).await;
    result.unwrap();
    assert!(observed.plans.is_empty());
    assert_eq!(observed.commits, 1);
}

#[tokio::test]
async fn persistence_failure_recovers_from_latest_authoritative_config() {
    let previous = policy("windows_task");
    let mut planned = previous.clone();
    planned.account_auto_claim.insert("a".into(), false);
    let mut latest = previous.clone();
    latest.auto_claim_after_minute = 7;
    let plans = Arc::new(Mutex::new(Vec::new()));
    let seen = plans.clone();
    let expected = latest.clone();
    let result: Result<(), String> = coordinate(
        &previous,
        &planned,
        false,
        move |plan| {
            seen.lock().unwrap().push(plan);
            std::future::ready(Ok(()))
        },
        || Err("owned disk failure".into()),
        move || latest.clone(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(*plans.lock().unwrap(), vec![planned, expected]);
}

#[tokio::test]
async fn initial_registration_failure_recovers_from_latest_authoritative_config() {
    let previous = policy("windows_task");
    let mut planned = previous.clone();
    planned.account_auto_claim.insert("a".into(), false);
    let mut latest = previous.clone();
    latest.auto_claim_after_minute = 7;
    let expected = latest.clone();
    let plans = Arc::new(Mutex::new(Vec::new()));
    let seen = plans.clone();
    let result: Result<(), String> = coordinate(
        &previous,
        &planned,
        false,
        move |plan| {
            let mut plans = seen.lock().unwrap();
            plans.push(plan);
            std::future::ready(if plans.len() == 1 {
                Err("owned failure after applying task".into())
            } else {
                Ok(())
            })
        },
        || panic!("failed registration must not persist"),
        move || latest.clone(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(*plans.lock().unwrap(), vec![planned, expected]);
}

#[tokio::test]
async fn pending_task_registration_must_complete_before_commit() {
    use tokio::{
        sync::oneshot,
        time::{timeout, Duration},
    };
    let previous = policy("windows_task");
    let mut planned = previous.clone();
    planned.account_auto_claim.insert("a".into(), false);
    let (started_tx, started_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = events.clone();
    let mut worker = tokio::spawn(async move {
        let mut started = Some(started_tx);
        let mut finish = Some(finish_rx);
        let register_events = recorded.clone();
        let latest = previous.clone();
        coordinate(
            &previous,
            &planned,
            false,
            move |_plan| {
                let started = started.take().unwrap();
                let finish = finish.take().unwrap();
                let events = register_events.clone();
                async move {
                    started.send(()).unwrap();
                    finish
                        .await
                        .map_err(|_| "owned barrier cancelled".to_string())?;
                    events.lock().unwrap().push("register_completed");
                    Ok(())
                }
            },
            move || {
                recorded.lock().unwrap().push("commit");
                Ok(())
            },
            move || latest.clone(),
        )
        .await
    });
    let started = timeout(Duration::from_secs(5), started_rx).await;
    if started.is_err() {
        worker.abort();
        panic!("registration did not reach the owned barrier");
    }
    started.unwrap().unwrap();
    assert!(
        events.lock().unwrap().is_empty(),
        "pending registration must not publish or persist"
    );
    finish_tx.send(()).unwrap();
    let completed = timeout(Duration::from_secs(5), &mut worker).await;
    if completed.is_err() {
        worker.abort();
        panic!("registration did not finish after release");
    }
    completed.unwrap().unwrap().unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        vec!["register_completed", "commit"]
    );
}

#[tokio::test]
async fn explicit_cli_install_repairs_an_unchanged_task_before_commit() {
    let previous = policy("windows_task");
    let events = Arc::new(Mutex::new(Vec::new()));
    let registered = events.clone();
    let committed = events.clone();
    let latest = previous.clone();
    coordinate(
        &previous,
        &previous,
        true,
        move |_plan| {
            registered.lock().unwrap().push("register");
            std::future::ready(Ok(()))
        },
        move || {
            committed.lock().unwrap().push("commit");
            Ok(())
        },
        move || latest.clone(),
    )
    .await
    .unwrap();
    assert_eq!(*events.lock().unwrap(), vec!["register", "commit"]);
}
