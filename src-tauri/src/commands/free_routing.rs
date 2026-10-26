use super::{commit_config, ConfigCommit};
use crate::{config::FreeRoutingConfig, AppState};
use tauri::State;

#[tauri::command]
pub fn save_free_routing(
    state: State<'_, AppState>,
    settings: FreeRoutingConfig,
) -> Result<FreeRoutingConfig, String> {
    Ok(commit_config(&state, ConfigCommit::FreeRouting(&settings))?.free_routing)
}
