//! Serialized task-plan and durable preference coordination.
use super::{commit_config, ConfigCommit};
use crate::benefit_task::{coordinate, task_change};
use crate::{benefit_config::BenefitsConfig, config::AppConfig, AppState};

/// Callers hold AppState.benefit_settings_commands throughout planning and
/// commit. Other narrow configuration commits cannot change benefits.
pub(super) async fn commit(
    state: &AppState,
    change: ConfigCommit<'_>,
    planned: &BenefitsConfig,
) -> Result<AppConfig, String> {
    let previous = state.config.read().benefits.clone();
    let paths = if task_change(&previous, planned) {
        Some((
            crate::config::app_data_dir(),
            crate::benefit_login::resource_root()?,
        ))
    } else {
        None
    };
    coordinate(
        &previous,
        planned,
        false,
        |settings| {
            let paths = paths.clone();
            async move {
                let (data, resources) = paths.ok_or("后台任务路径未准备")?;
                crate::benefit_task::register(&settings, &data, &resources).await
            }
        },
        || commit_config(state, change),
        || state.config.read().benefits.clone(),
    )
    .await
}

#[cfg(test)]
#[path = "benefit_settings_tests.rs"]
mod tests;
