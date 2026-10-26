use super::{commit_config, ConfigCommit};
use crate::{
    config::CatalogMaintenanceConfig,
    db::repo::catalog::{self, InventoryPage, ScanStatus, ScanTrigger},
    model_catalog::maintenance::{Manager, ScanReply},
    AppState,
};
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
pub struct MaintenanceView {
    pub settings: CatalogMaintenanceConfig,
    pub statuses: Vec<ScanStatus>,
}

#[tauri::command]
pub async fn catalog_maintenance_status(
    state: State<'_, AppState>,
) -> Result<MaintenanceView, String> {
    let settings = { state.config.read().catalog_maintenance.clone() };
    let statuses = catalog::list_status(state.db.pool())
        .await
        .map_err(|_| "目录维护状态读取失败，已保留持久记录".to_string())?;
    Ok(MaintenanceView { settings, statuses })
}

#[tauri::command]
pub async fn save_catalog_maintenance(
    state: State<'_, AppState>,
    settings: CatalogMaintenanceConfig,
) -> Result<MaintenanceView, String> {
    settings.validate().map_err(|e| e.to_string())?;
    let providers = crate::db::repo::list_providers(state.db.pool())
        .await
        .map_err(|_| "无法核验供应商列表".to_string())?;
    if settings
        .provider_ids
        .iter()
        .any(|id| !providers.iter().any(|p| &p.id == id))
    {
        return Err("维护范围包含不存在的供应商，请取消选择后保存".into());
    }
    let saved = commit_config(&state, ConfigCommit::CatalogMaintenance(&settings))?;
    let statuses = catalog::list_status(state.db.pool())
        .await
        .map_err(|_| "配置已保存，状态读取失败，请刷新本地状态".to_string())?;
    Ok(MaintenanceView {
        settings: saved.catalog_maintenance,
        statuses,
    })
}

#[tauri::command]
pub async fn scan_catalog_maintenance(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<ScanReply, String> {
    let mut reply = state
        .gateway
        .catalog
        .request(&provider_id, ScanTrigger::Manual)
        .await
        .map_err(|_| "目录扫描存储或供应商不可用，持久状态已保留".to_string())?;
    if Manager::publish_pending(&state.gateway).await.is_err() {
        reply.failure_kind = Some("runtime_publish_pending".into());
    }
    Ok(reply)
}

#[tauri::command]
pub fn cancel_catalog_maintenance(state: State<'_, AppState>, provider_id: String) -> bool {
    state.gateway.catalog.cancel(&provider_id)
}

#[tauri::command]
pub async fn catalog_inventory(
    state: State<'_, AppState>,
    provider_id: String,
    query: String,
    free_name_only: bool,
    offset: usize,
) -> Result<InventoryPage, String> {
    catalog::list_inventory(
        state.db.pool(),
        &provider_id,
        &query,
        free_name_only,
        offset,
    )
    .await
    .map_err(|_| "目录库存读取失败，请检查查询或存储状态".into())
}
