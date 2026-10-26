//! Shared APP/headless task-plan coordination; no system task is touched by tests.
use crate::benefit_config::BenefitsConfig;
use std::future::Future;

pub(crate) fn task_change(previous: &BenefitsConfig, planned: &BenefitsConfig) -> bool {
    previous != planned
        && (previous.scheduler_mode == "windows_task" || planned.scheduler_mode == "windows_task")
}

pub(crate) async fn coordinate<T, R, F, C, L>(
    previous: &BenefitsConfig,
    planned: &BenefitsConfig,
    force_register: bool,
    mut register: R,
    commit: C,
    latest: L,
) -> Result<T, String>
where
    R: FnMut(BenefitsConfig) -> F,
    F: Future<Output = Result<(), String>>,
    C: FnOnce() -> Result<T, String>,
    L: Fn() -> BenefitsConfig,
{
    if !force_register && !task_change(previous, planned) {
        return commit();
    }
    // Register-ScheduledTask may have changed the task before its readback
    // fails. Restore the latest durable policy even when registration itself
    // returns an error; never infer that an error means no system mutation.
    if let Err(error) = register(planned.clone()).await {
        if register(latest()).await.is_err() {
            return Err("权益设置未保存；后台任务恢复未确认，请刷新检查".into());
        }
        return Err(error);
    }
    match commit() {
        Ok(saved) => Ok(saved),
        Err(error) => {
            if register(latest()).await.is_err() {
                return Err("权益设置未保存；后台任务恢复未确认，请刷新检查".into());
            }
            Err(error)
        }
    }
}
