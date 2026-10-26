use super::{benefit_settings, ConfigCommit, ConfigUpdateResult};
use crate::{benefit_config::AccountAutoClaimChange, AppState};
use tauri::State;

/// A production preference commit, not an entitlement/identity assertion.
#[tauri::command]
pub async fn save_benefit_account_auto_claim(
    state: State<'_, AppState>,
    change: AccountAutoClaimChange,
) -> Result<ConfigUpdateResult, String> {
    // Serialize this task-plan/persistence pair with ordinary settings saves.
    // No parking_lot guard survives an await.
    let _gate = state.benefit_settings_commands.lock().await;
    let previous = state.config.read().clone();
    let mut planned = previous.clone();
    planned.benefits.apply_account_auto_claim_change(&change)?;
    let saved = benefit_settings::commit(
        &state,
        ConfigCommit::AccountAutoClaim(&change),
        &planned.benefits,
    )
    .await?;
    Ok(ConfigUpdateResult {
        config: saved,
        restart_required: false,
        restart_reasons: Vec::new(),
    })
}
