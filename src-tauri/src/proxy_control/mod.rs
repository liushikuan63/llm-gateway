pub mod gateway;
pub mod system;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

pub const SYSTEM_RESTORE_SECRET: &str = "system-proxy-restore-v1";

#[derive(Serialize, Deserialize)]
struct RecoveryRecord {
    restore: system::SystemProxySnapshot,
    before: system::SystemProxySnapshot,
    #[serde(default)]
    owned: Option<system::SystemProxySnapshot>,
}

pub fn decode_restore(raw: &str) -> Result<system::SystemProxySnapshot, String> {
    serde_json::from_str::<RecoveryRecord>(raw)
        .map(|v| v.restore)
        .or_else(|_| serde_json::from_str::<system::SystemProxySnapshot>(raw))
        .map_err(|_| "系统代理恢复信息格式无效".into())
}

pub fn load_recovery<B: system::SystemProxyBackend>(
    manager: &system::SystemProxyManager<B>,
    raw: &str,
) -> Result<(), String> {
    if let Ok(record) = serde_json::from_str::<RecoveryRecord>(raw) {
        manager.seed_recovery_snapshots(record.restore, Some(record.before), record.owned)
    } else {
        // Same legacy decoder/format; no second publication if validation fails.
        let restore = decode_restore(raw)?;
        manager.seed_recovery_snapshots(restore, None, None)
    }
    .map_err(|error| error.to_string())
}

pub fn gateway_ports(state: &crate::AppState) -> Vec<u16> {
    configured_and_bound_ports(
        state.config.read().port,
        state
            .gateway
            .listener_snapshot()
            .map(|v| v.bound_addr.port()),
    )
}

fn configured_and_bound_ports(configured: u16, bound: Option<u16>) -> Vec<u16> {
    let mut ports = vec![configured];
    if let Some(bound) = bound {
        if !ports.contains(&bound) {
            ports.push(bound);
        }
    }
    ports
}

#[derive(Clone, Serialize)]
pub struct SystemControlStatus {
    pub supported: bool,
    pub enabled: Option<bool>,
    pub mode: String,
    pub manual_server: Option<String>,
    pub source: String,
    pub error: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct ProxyControlStatus {
    pub gateway: gateway::GatewayServiceStatus,
    pub system: SystemControlStatus,
    pub startup: crate::config::ProxyControlConfig,
}

pub async fn status(state: &crate::AppState) -> ProxyControlStatus {
    let system = match state.system_proxy.status() {
        Ok(value) => SystemControlStatus {
            supported: value.supported,
            enabled: Some(value.enabled),
            mode: serde_json::to_value(value.mode)
                .unwrap_or_default()
                .as_str()
                .unwrap_or("unknown")
                .into(),
            manual_server: value.manual_server,
            source: serde_json::to_value(value.source)
                .unwrap_or_default()
                .as_str()
                .unwrap_or("external")
                .into(),
            error: value.error,
        },
        Err(error) => SystemControlStatus {
            supported: cfg!(windows),
            enabled: None,
            mode: "unknown".into(),
            manual_server: None,
            source: "external".into(),
            error: Some(error.to_string()),
        },
    };
    ProxyControlStatus {
        gateway: state.gateway_service.status().await,
        system,
        startup: state.config.read().proxy_control.clone(),
    }
}

pub async fn set_gateway(
    app: &tauri::AppHandle,
    enabled: bool,
) -> Result<ProxyControlStatus, String> {
    let state = app
        .try_state::<crate::AppState>()
        .ok_or("后端仍在初始化，请稍后重试")?;
    let _guard = state.proxy_commands.lock().await;
    mark_busy(app);
    let result = state.gateway_service.set_enabled(enabled).await;
    let current = status(&state).await;
    publish(app, &current);
    result.map(|_| current)
}

pub async fn set_system(
    app: &tauri::AppHandle,
    enabled: bool,
) -> Result<ProxyControlStatus, String> {
    let state = app
        .try_state::<crate::AppState>()
        .ok_or("后端仍在初始化，请稍后重试")?;
    let _guard = state.proxy_commands.lock().await;
    mark_busy(app);
    let result = change_system(&state, enabled).await;
    let current = status(&state).await;
    publish(app, &current);
    result.map(|_| current)
}

async fn change_system(state: &crate::AppState, enabled: bool) -> Result<(), String> {
    let cfg = state.config.read().clone();
    let ports = gateway_ports(state);
    let server = cfg
        .proxy_control
        .system_proxy_server
        .as_deref()
        .filter(|v| !v.trim().is_empty());
    if let Some(server) = server.filter(|_| enabled) {
        for port in &ports {
            system::validate_server(server, *port).map_err(|e| e.to_string())?;
        }
    }
    let before = state.system_proxy.snapshot().map_err(|e| e.to_string())?;
    if !enabled && !before.is_enabled() {
        return Ok(());
    }
    // Enabling an already active external configuration needs no write or ownership claim.
    if enabled && before.is_enabled() && server.is_none() {
        return Ok(());
    }
    // Commit the encrypted recovery state before changing any Windows option.
    // Retain both the ownership-aware restore target and the exact pre-write state.
    let old = crate::db::repo::get_secret(state.db.pool(), SYSTEM_RESTORE_SECRET)
        .await
        .map_err(|e| e.to_string())?;
    let restore = state
        .system_proxy
        .restore_snapshot_for_set_enabled(enabled, server, &before)
        .map_err(|e| e.to_string())?;
    let planned = state
        .system_proxy
        .planned_snapshot_for_set_enabled(enabled, server, &before, &ports)
        .map_err(|e| e.to_string())?;
    let record = RecoveryRecord {
        restore: restore.clone(),
        before: before.clone(),
        owned: Some(planned),
    };
    {
        let raw = serde_json::to_string(&record).map_err(|_| "无法序列化系统代理恢复信息")?;
        let encrypted = crate::crypto::encrypt(&raw).map_err(|_| "无法加密系统代理恢复信息")?;
        crate::db::repo::set_secret(state.db.pool(), SYSTEM_RESTORE_SECRET, &encrypted)
            .await
            .map_err(|_| "无法保存系统代理恢复信息，系统设置未更改")?;
    }
    let result = state
        .system_proxy
        .set_enabled_if_unchanged_for_gateway_ports(enabled, server, &before, &ports);
    if let Err(error) = &result {
        // An unverified write or failed rollback may already have changed Windows.
        // Keep the durable pre-write snapshot rather than erase its only recovery copy.
        if matches!(
            error,
            system::SystemProxyError::Unverified | system::SystemProxyError::RollbackFailed
        ) {
            let _ = state.system_proxy.seed_restore_snapshot(restore);
            if let Some(owned) = record.owned {
                let _ = state.system_proxy.seed_last_written_snapshot(owned);
            }
        }
        restore_backup_after_error(state.db.pool(), old.as_deref(), error).await?;
    }
    result.map(|_| ()).map_err(|e| e.to_string())
}

async fn restore_backup_after_error(
    pool: &sqlx::SqlitePool,
    old: Option<&str>,
    error: &system::SystemProxyError,
) -> Result<(), String> {
    if !matches!(
        error,
        system::SystemProxyError::Unverified | system::SystemProxyError::RollbackFailed
    ) {
        if let Some(old) = old {
            crate::db::repo::set_secret(pool, SYSTEM_RESTORE_SECRET, old)
                .await
                .map_err(|_| "代理切换失败，恢复信息保存也失败")?;
        } else {
            crate::db::repo::delete_secret(pool, SYSTEM_RESTORE_SECRET)
                .await
                .map_err(|_| "代理切换失败，恢复信息清理也失败")?;
        }
    }
    Ok(())
}

pub fn publish(app: &tauri::AppHandle, status: &ProxyControlStatus) {
    sync_tray(app, status);
    let _ = app.emit("proxy-control-changed", status);
}

pub fn mark_busy(app: &tauri::AppHandle) {
    if let Some(tray) = app.try_state::<crate::ProxyTray>() {
        let _ = tray.gateway.set_enabled(false);
        let _ = tray.system.set_enabled(false);
    }
}

pub fn sync_tray(app: &tauri::AppHandle, status: &ProxyControlStatus) {
    if let Some(tray) = app.try_state::<crate::ProxyTray>() {
        let _ = tray.gateway.set_checked(status.gateway.running);
        let _ = tray.system.set_checked(status.system.enabled == Some(true));
        let _ = tray.gateway.set_enabled(true);
        let _ = tray
            .system
            .set_enabled(status.system.supported && status.system.enabled.is_some());
        let _ = tray.system.set_text(if status.system.enabled.is_none() {
            "系统网络代理（状态读取失败）"
        } else {
            "系统网络代理"
        });
    }
}

/// Read-only monitoring keeps tray checks current when another network client changes Windows.
pub async fn monitor(app: tauri::AppHandle) {
    let mut previous = None;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let Some(state) = app.try_state::<crate::AppState>() else {
            continue;
        };
        let _guard = state.proxy_commands.lock().await;
        let current = status(&state).await;
        let encoded = serde_json::to_value(&current).ok();
        if previous != encoded {
            publish(&app, &current);
            previous = encoded;
        } else {
            sync_tray(&app, &current);
        }
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    #[test]
    fn saved_port_change_does_not_allow_current_llm_listener_as_system_proxy() {
        let ports = configured_and_bound_ports(19100, Some(19000));
        assert_eq!(ports, vec![19100, 19000]);
        for target in [
            "127.0.0.1:19000",
            "http=127.0.0.1:19100;https=127.0.0.1:7890",
        ] {
            assert!(ports
                .iter()
                .any(|port| system::validate_server(target, *port).is_err()));
        }
        assert!(ports
            .iter()
            .all(|port| system::validate_server("127.0.0.1:7890", *port).is_ok()));
        assert_eq!(configured_and_bound_ports(19100, None), vec![19100]);
    }

    struct FakeBackend(std::sync::Arc<parking_lot::Mutex<system::SystemProxySnapshot>>);
    impl system::SystemProxyBackend for FakeBackend {
        fn snapshot(&self) -> Result<system::SystemProxySnapshot, system::SystemProxyError> {
            Ok(self.0.lock().clone())
        }
        fn write_if_unchanged(
            &self,
            expected: &system::SystemProxySnapshot,
            desired: &system::SystemProxySnapshot,
        ) -> Result<(), system::SystemProxyError> {
            let mut current = self.0.lock();
            if *current != *expected {
                return Err(system::SystemProxyError::ExternalChange);
            }
            *current = desired.clone();
            Ok(())
        }
    }

    fn snapshot(flags: u32, port: u16) -> system::SystemProxySnapshot {
        serde_json::from_value(serde_json::json!({"flags":flags,"server":format!("127.0.0.1:{port}"),"bypass":"<local>","pac_url":"https://fixture.invalid/proxy.pac"})).unwrap()
    }

    #[test]
    fn persisted_ownership_preserves_original_restore_across_restart_and_external_change() {
        let original = snapshot(13, 7890);
        let owned = snapshot(3, 7891);
        let record = RecoveryRecord {
            restore: original.clone(),
            before: original.clone(),
            owned: Some(owned.clone()),
        };
        let raw = serde_json::to_string(&record).unwrap();
        let actual = std::sync::Arc::new(parking_lot::Mutex::new(owned.clone()));
        let manager = system::SystemProxyManager::new(FakeBackend(actual.clone()));
        load_recovery(&manager, &raw).unwrap();
        assert_eq!(
            manager.status().unwrap().source,
            system::SystemProxySource::Application
        );
        manager.set_enabled(false, None).unwrap();
        assert!(
            manager
                .restore_snapshot_for_change(&manager.snapshot().unwrap())
                .unwrap()
                == original
        );
        manager.set_enabled(true, None).unwrap();
        assert!(manager.snapshot().unwrap() == original);
        let external = snapshot(3, 7892);
        *actual.lock() = external.clone();
        assert_eq!(
            manager.status().unwrap().source,
            system::SystemProxySource::External
        );
        assert!(manager.restore_snapshot_for_change(&external).unwrap() == external);
        let legacy = serde_json::to_string(&original).unwrap();
        assert!(decode_restore(&legacy).unwrap() == original);
        assert!(decode_restore("{}").is_err());
    }

    #[tokio::test]
    async fn uncertain_os_write_retains_latest_durable_recovery_for_restart() {
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        for error in [
            system::SystemProxyError::Unverified,
            system::SystemProxyError::RollbackFailed,
        ] {
            for old in [None, Some("older-encrypted-recovery")] {
                crate::db::repo::set_secret(
                    db.pool(),
                    SYSTEM_RESTORE_SECRET,
                    "latest-encrypted-pre-write",
                )
                .await
                .unwrap();
                restore_backup_after_error(db.pool(), old, &error)
                    .await
                    .unwrap();
                assert_eq!(
                    crate::db::repo::get_secret(db.pool(), SYSTEM_RESTORE_SECRET)
                        .await
                        .unwrap()
                        .as_deref(),
                    Some("latest-encrypted-pre-write")
                );
            }
        }
    }

    #[tokio::test]
    async fn verified_rejection_restores_previous_recovery_without_creating_record() {
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        crate::db::repo::set_secret(db.pool(), SYSTEM_RESTORE_SECRET, "new-record")
            .await
            .unwrap();
        restore_backup_after_error(
            db.pool(),
            Some("old-record"),
            &system::SystemProxyError::ExternalChange,
        )
        .await
        .unwrap();
        assert_eq!(
            crate::db::repo::get_secret(db.pool(), SYSTEM_RESTORE_SECRET)
                .await
                .unwrap()
                .as_deref(),
            Some("old-record")
        );
        restore_backup_after_error(db.pool(), None, &system::SystemProxyError::InvalidServer)
            .await
            .unwrap();
        assert!(
            crate::db::repo::get_secret(db.pool(), SYSTEM_RESTORE_SECRET)
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[cfg(test)]
mod recovery_load_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct NoWriteBackend {
        current: Arc<parking_lot::Mutex<system::SystemProxySnapshot>>,
        writes: Arc<AtomicUsize>,
    }
    impl system::SystemProxyBackend for NoWriteBackend {
        fn snapshot(&self) -> Result<system::SystemProxySnapshot, system::SystemProxyError> {
            Ok(self.current.lock().clone())
        }
        fn write_if_unchanged(
            &self,
            _: &system::SystemProxySnapshot,
            _: &system::SystemProxySnapshot,
        ) -> Result<(), system::SystemProxyError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Err(system::SystemProxyError::Unsupported)
        }
    }
    fn value(flags: u32, port: u16) -> system::SystemProxySnapshot {
        serde_json::from_value(serde_json::json!({"flags":flags,"server":format!("127.0.0.1:{port}"),"bypass":"<local>","pac_url":"https://owned-fixture.invalid/proxy.pac"})).unwrap()
    }
    fn fixture(
        current: system::SystemProxySnapshot,
    ) -> (system::SystemProxyManager<NoWriteBackend>, Arc<AtomicUsize>) {
        let writes = Arc::new(AtomicUsize::new(0));
        (
            system::SystemProxyManager::new(NoWriteBackend {
                current: Arc::new(parking_lot::Mutex::new(current)),
                writes: writes.clone(),
            }),
            writes,
        )
    }
    #[test]
    fn v3_recovery_load_invalid_owned_cannot_publish_partial_restore() {
        let owned = value(3, 7891);
        let original = value(13, 7890);
        let replacement = value(13, 7892);
        let (manager, writes) = fixture(owned.clone());
        let good = serde_json::to_string(&RecoveryRecord {
            restore: original.clone(),
            before: original.clone(),
            owned: Some(owned.clone()),
        })
        .unwrap();
        load_recovery(&manager, &good).unwrap();
        assert!(manager.restore_snapshot_for_change(&owned).unwrap() == original);
        assert_eq!(
            manager.status().unwrap().source,
            system::SystemProxySource::Application
        );
        let invalid = serde_json::to_string(&RecoveryRecord {
            restore: replacement,
            before: original.clone(),
            owned: Some(value(0xffff, 7893)),
        })
        .unwrap();
        assert!(load_recovery(&manager, &invalid).is_err());
        assert!(
            manager.restore_snapshot_for_change(&owned).unwrap() == original,
            "a rejected ownership field cannot replace the prior restore target"
        );
        assert!(manager.last_written_snapshot().unwrap() == owned);
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn v3_recovery_load_invalid_before_rejects_whole_record_without_writes() {
        for invalid_before in [true, false] {
            let current = value(1, 7891);
            let (manager, writes) = fixture(current.clone());
            let invalid = serde_json::to_string(&RecoveryRecord {
                restore: if invalid_before {
                    value(13, 7890)
                } else {
                    value(0xffff, 7890)
                },
                before: if invalid_before {
                    value(0xffff, 7893)
                } else {
                    current.clone()
                },
                owned: Some(value(3, 7891)),
            })
            .unwrap();
            assert!(
                load_recovery(&manager, &invalid).is_err(),
                "every snapshot in the accepted record must pass the existing validation"
            );
            assert!(manager.last_written_snapshot().is_none());
            assert!(manager.restore_snapshot_for_change(&current).unwrap() == current);
            assert_eq!(writes.load(Ordering::SeqCst), 0);
        }
    }
    #[test]
    fn v3_recovery_load_valid_and_legacy_keep_existing_no_write_controls() {
        let restore = value(13, 7890);
        let owned = value(3, 7891);
        let (manager, writes) = fixture(owned.clone());
        let raw = serde_json::to_string(&RecoveryRecord {
            restore: restore.clone(),
            before: restore.clone(),
            owned: Some(owned.clone()),
        })
        .unwrap();
        load_recovery(&manager, &raw).unwrap();
        assert_eq!(
            manager.status().unwrap().source,
            system::SystemProxySource::Application
        );
        assert!(manager.restore_snapshot_for_change(&owned).unwrap() == restore);
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        let (legacy, writes) = fixture(value(1, 7891));
        load_recovery(&legacy, &serde_json::to_string(&restore).unwrap()).unwrap();
        assert!(legacy.last_written_snapshot().is_none());
        assert_eq!(
            legacy.status().unwrap().source,
            system::SystemProxySource::External
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }
}
