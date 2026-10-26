use super::*;
use std::sync::{Arc, Barrier};

#[derive(Clone)]
struct FakeBackend(Arc<Mutex<FakeState>>);

struct FakeState {
    current: SystemProxySnapshot,
    writes: usize,
    fail_after_write: bool,
    refuse_write: bool,
    fail_read_after_write: bool,
    fail_rollback: bool,
    external_before_write: Option<SystemProxySnapshot>,
    external_after_write: Option<SystemProxySnapshot>,
}

impl FakeBackend {
    fn new(snapshot: SystemProxySnapshot) -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            current: snapshot,
            writes: 0,
            fail_after_write: false,
            refuse_write: false,
            fail_read_after_write: false,
            fail_rollback: false,
            external_before_write: None,
            external_after_write: None,
        })))
    }
}

impl SystemProxyBackend for FakeBackend {
    fn snapshot(&self) -> Result<SystemProxySnapshot, SystemProxyError> {
        let state = self.0.lock();
        if state.fail_read_after_write && state.writes > 0 {
            return Err(SystemProxyError::Windows {
                operation: "query",
                code: 5,
            });
        }
        Ok(state.current.clone())
    }
    fn write_if_unchanged(
        &self,
        expected: &SystemProxySnapshot,
        desired: &SystemProxySnapshot,
    ) -> Result<(), SystemProxyError> {
        let mut state = self.0.lock();
        if let Some(external) = state.external_before_write.take() {
            state.current = external;
        }
        if &state.current != expected {
            return Err(SystemProxyError::ExternalChange);
        }
        state.writes += 1;
        if state.fail_rollback && state.writes > 1 {
            return Err(SystemProxyError::Windows {
                operation: "set",
                code: 5,
            });
        }
        if state.refuse_write {
            return Ok(());
        }
        state.current = desired.clone();
        if let Some(external) = state.external_after_write.take() {
            state.current = external;
        }
        if std::mem::take(&mut state.fail_after_write) {
            return Err(SystemProxyError::Windows {
                operation: "notify",
                code: 5,
            });
        }
        Ok(())
    }
}

fn settings(flags: u32) -> SystemProxySnapshot {
    SystemProxySnapshot {
        flags,
        server: "127.0.0.1:7890".into(),
        bypass: "<local>;private.example".into(),
        pac_url: "https://private.example/proxy.pac?secret=hidden".into(),
    }
}

#[test]
fn construction_and_status_only_read_existing_external_proxy() {
    let original = settings(DIRECT | MANUAL | PAC | AUTO_DETECT);
    let fake = FakeBackend::new(original.clone());
    let manager = SystemProxyManager::new(fake.clone());
    let status = manager.status().unwrap();
    assert!(status.enabled);
    assert_eq!(status.mode, SystemProxyMode::Mixed);
    assert_eq!(status.source, SystemProxySource::External);
    assert!(fake.snapshot().unwrap() == original);
    assert_eq!(fake.0.lock().writes, 0);
    let json = serde_json::to_string(&status).unwrap();
    for private in ["127.0.0.1", "private.example", "hidden", "secret"] {
        assert!(!json.contains(private));
        assert!(!format!("{status:?}").contains(private));
    }
}

#[test]
fn shutdown_restores_exact_active_owned_proxy_and_releases_ownership() {
    let original = settings(DIRECT | PAC | AUTO_DETECT);
    let fake = FakeBackend::new(original.clone());
    let manager = SystemProxyManager::new(fake.clone());
    manager.set_enabled(true, Some("127.0.0.1:9080")).unwrap();
    assert!(manager.shutdown_restore_owned().unwrap());
    assert!(fake.snapshot().unwrap() == original);
    assert_eq!(fake.0.lock().writes, 2);
    assert!(manager.last_written_snapshot().is_none());
    assert_eq!(
        manager.status().unwrap().source,
        SystemProxySource::External
    );
    assert!(manager.state.lock().restore.as_ref() == Some(&original));
    assert!(!manager.shutdown_restore_owned().unwrap());
    assert_eq!(fake.0.lock().writes, 2);
}

#[test]
fn shutdown_keeps_user_manual_off_without_reenabling_original_pac() {
    let fake = FakeBackend::new(settings(DIRECT | PAC | AUTO_DETECT));
    let manager = SystemProxyManager::new(fake.clone());
    manager.set_enabled(true, Some("127.0.0.1:9080")).unwrap();
    manager.set_enabled(false, None).unwrap();
    let disabled = manager.snapshot().unwrap();
    assert!(!manager.shutdown_restore_owned().unwrap());
    assert!(fake.snapshot().unwrap() == disabled);
    assert_eq!(fake.0.lock().writes, 2);
    assert!(!manager.status().unwrap().enabled);
}

#[test]
fn shutdown_preserves_external_override_and_never_reclaims_it() {
    let fake = FakeBackend::new(settings(DIRECT | PAC | AUTO_DETECT));
    let manager = SystemProxyManager::new(fake.clone());
    manager.set_enabled(true, Some("127.0.0.1:9080")).unwrap();
    let mut external = settings(DIRECT | MANUAL);
    external.server = "external.example:8090".into();
    fake.0.lock().current = external.clone();
    assert!(!manager.shutdown_restore_owned().unwrap());
    assert!(fake.snapshot().unwrap() == external);
    assert_eq!(fake.0.lock().writes, 1);
    assert_eq!(
        manager.status().unwrap().source,
        SystemProxySource::External
    );
}

#[test]
fn shutdown_without_ownership_blocks_pending_activation_and_planning() {
    let fake = FakeBackend::new(settings(DIRECT));
    let manager = SystemProxyManager::new(fake.clone());
    let before = manager.snapshot().unwrap();
    let pending = manager
        .planned_snapshot_for_set_enabled(true, Some("127.0.0.1:9080"), &before, &[19100])
        .unwrap();
    assert!(pending.is_enabled());
    assert!(!manager.shutdown_restore_owned().unwrap());
    assert_eq!(
        manager
            .set_enabled_if_unchanged_for_gateway_ports(
                true,
                Some("127.0.0.1:9080"),
                &before,
                &[19100]
            )
            .unwrap_err(),
        SystemProxyError::ShuttingDown
    );
    assert!(matches!(
        manager.planned_snapshot_for_set_enabled(true, Some("127.0.0.1:9080"), &before, &[19100]),
        Err(SystemProxyError::ShuttingDown)
    ));
    assert!(matches!(
        manager.restore_snapshot_for_set_enabled(true, None, &before),
        Err(SystemProxyError::ShuttingDown)
    ));
    assert_eq!(
        manager.restore_if_unchanged(&pending, &before).unwrap_err(),
        SystemProxyError::ShuttingDown
    );
    assert!(fake.snapshot().unwrap() == before);
    assert_eq!(fake.0.lock().writes, 0);
}

#[test]
fn shutdown_query_and_restore_failures_keep_recovery_and_reject_new_writes() {
    let original = settings(DIRECT | PAC | AUTO_DETECT);
    let fake = FakeBackend::new(original.clone());
    let manager = SystemProxyManager::new(fake.clone());
    manager.set_enabled(true, Some("127.0.0.1:9080")).unwrap();
    let owned = manager.last_written_snapshot().unwrap();
    fake.0.lock().fail_read_after_write = true;
    assert!(matches!(
        manager.shutdown_restore_owned(),
        Err(SystemProxyError::Windows {
            operation: "query",
            ..
        })
    ));
    assert_eq!(fake.0.lock().writes, 1);
    assert!(manager.last_written_snapshot().as_ref() == Some(&owned));
    assert!(manager.state.lock().restore.as_ref() == Some(&original));
    assert_eq!(
        manager.set_enabled(false, None).unwrap_err(),
        SystemProxyError::ShuttingDown
    );

    fake.0.lock().fail_read_after_write = false;
    fake.0.lock().refuse_write = true;
    assert_eq!(
        manager.shutdown_restore_owned().unwrap_err(),
        SystemProxyError::Unverified
    );
    assert!(fake.snapshot().unwrap() == owned);
    assert!(manager.last_written_snapshot().as_ref() == Some(&owned));
    assert!(manager.state.lock().restore.as_ref() == Some(&original));
    assert_eq!(fake.0.lock().writes, 2);
}

#[test]
fn disabling_all_modes_preserves_configuration_and_enabling_restores_it() {
    let original = settings(DIRECT | MANUAL | PAC | AUTO_DETECT);
    let fake = FakeBackend::new(original.clone());
    let manager = SystemProxyManager::new(fake.clone());
    let disabled = manager.set_enabled(false, None).unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.mode, SystemProxyMode::Direct);
    let after = fake.snapshot().unwrap();
    assert_eq!(after.flags, DIRECT);
    assert_eq!(after.server, original.server);
    assert_eq!(after.bypass, original.bypass);
    assert_eq!(after.pac_url, original.pac_url);
    let enabled = manager.set_enabled(true, None).unwrap();
    assert!(enabled.enabled);
    assert_eq!(enabled.mode, SystemProxyMode::Mixed);
    assert!(fake.snapshot().unwrap() == original);
    assert_eq!(fake.0.lock().writes, 2);
}

#[test]
fn configured_manual_cycle_preserves_original_pac_and_autodetection_recovery() {
    let original = settings(DIRECT | PAC | AUTO_DETECT);
    let fake = FakeBackend::new(original.clone());
    let manager = SystemProxyManager::new(fake.clone());
    assert_eq!(
        manager
            .set_enabled(true, Some("127.0.0.1:9080"))
            .unwrap()
            .mode,
        SystemProxyMode::Manual
    );
    let owned = manager.snapshot().unwrap();
    let recovery = manager.restore_snapshot_for_change(&owned).unwrap();
    assert!(recovery == original);
    assert!(!manager.set_enabled(false, None).unwrap().enabled);
    // Clearing the configured override must restore original PAC/AUTO, not the override.
    assert_eq!(
        manager.set_enabled(true, None).unwrap().mode,
        SystemProxyMode::Mixed
    );
    assert!(fake.snapshot().unwrap() == original);
    assert_eq!(fake.0.lock().writes, 3);

    manager.set_enabled(false, None).unwrap();
    let persisted = serde_json::to_string(&recovery).unwrap();
    let restarted = SystemProxyManager::new(fake.clone());
    restarted
        .seed_restore_snapshot(serde_json::from_str(&persisted).unwrap())
        .unwrap();
    assert!(restarted.set_enabled(true, None).unwrap().enabled);
    assert!(fake.snapshot().unwrap() == original);
}

#[test]
fn closing_external_override_captures_latest_external_recovery() {
    let fake = FakeBackend::new(settings(DIRECT | PAC | AUTO_DETECT));
    let manager = SystemProxyManager::new(fake.clone());
    manager.set_enabled(true, Some("127.0.0.1:9080")).unwrap();
    let mut external = settings(DIRECT | MANUAL | AUTO_DETECT);
    external.server = "external.example:8090".into();
    external.pac_url = "https://external.example/new.pac".into();
    fake.0.lock().current = external.clone();
    let recovery = manager.restore_snapshot_for_change(&external).unwrap();
    assert!(recovery == external);
    assert!(!manager.set_enabled(false, None).unwrap().enabled);
    assert!(manager.set_enabled(true, None).unwrap().enabled);
    assert!(fake.snapshot().unwrap() == external);
    assert_eq!(fake.0.lock().writes, 3);
}

#[test]
fn configured_enable_after_external_override_updates_memory_and_durable_target() {
    let fake = FakeBackend::new(settings(DIRECT | PAC | AUTO_DETECT));
    let manager = SystemProxyManager::new(fake.clone());
    manager.set_enabled(true, Some("127.0.0.1:9080")).unwrap();
    let mut external = settings(DIRECT | PAC | AUTO_DETECT);
    external.server = "external.example:8090".into();
    external.pac_url = "https://external.example/new.pac".into();
    fake.0.lock().current = external.clone();
    let planned = manager
        .restore_snapshot_for_set_enabled(true, Some("127.0.0.1:9090"), &external)
        .unwrap();
    assert!(planned == external);
    manager.set_enabled(true, Some("127.0.0.1:9090")).unwrap();
    let owned = manager.snapshot().unwrap();
    assert!(manager.restore_snapshot_for_change(&owned).unwrap() == external);
    manager.set_enabled(false, None).unwrap();
    manager.set_enabled(true, None).unwrap();
    assert!(fake.snapshot().unwrap() == external);
    assert_eq!(fake.0.lock().writes, 4);
}

#[test]
fn loaded_ownership_preserves_pac_restore_across_restart_without_claiming_other_settings() {
    let original = settings(DIRECT | PAC | AUTO_DETECT);
    let fake = FakeBackend::new(original.clone());
    let first = SystemProxyManager::new(fake.clone());
    first.set_enabled(true, Some("127.0.0.1:9080")).unwrap();
    let owned = first.last_written_snapshot().unwrap();
    let restore = first.restore_snapshot_for_change(&owned).unwrap();
    let restart = SystemProxyManager::new(fake.clone());
    restart.seed_restore_snapshot(restore).unwrap();
    restart.seed_last_written_snapshot(owned.clone()).unwrap();
    assert_eq!(fake.0.lock().writes, 1);
    assert_eq!(
        restart.status().unwrap().source,
        SystemProxySource::Application
    );
    restart.set_enabled(false, None).unwrap();
    restart.set_enabled(true, None).unwrap();
    assert!(fake.snapshot().unwrap() == original);

    let stale_restart = SystemProxyManager::new(fake.clone());
    stale_restart.seed_restore_snapshot(original).unwrap();
    stale_restart.seed_last_written_snapshot(owned).unwrap();
    let mut external = settings(DIRECT | MANUAL);
    external.server = "external.example:8090".into();
    fake.0.lock().current = external.clone();
    assert_eq!(
        stale_restart.status().unwrap().source,
        SystemProxySource::External
    );
    assert!(
        stale_restart
            .restore_snapshot_for_change(&external)
            .unwrap()
            == external
    );
}

#[test]
fn legacy_loaded_restore_uses_same_durable_target_through_repeated_cycles() {
    let original = settings(DIRECT | PAC | AUTO_DETECT);
    let fake = FakeBackend::new(settings(DIRECT));
    let manager = SystemProxyManager::new(fake.clone());
    manager.seed_restore_snapshot(original.clone()).unwrap();
    let before = manager.snapshot().unwrap();
    assert!(
        manager
            .restore_snapshot_for_set_enabled(true, None, &before)
            .unwrap()
            == original
    );
    manager.set_enabled(true, None).unwrap();
    manager.set_enabled(false, None).unwrap();
    manager.set_enabled(true, None).unwrap();
    assert!(fake.snapshot().unwrap() == original);
    assert_eq!(fake.0.lock().writes, 3);
}

#[test]
fn prewrite_planned_ownership_survives_restart_without_postwrite_persistence() {
    let original = settings(DIRECT | PAC | AUTO_DETECT);
    let fake = FakeBackend::new(original.clone());
    let manager = SystemProxyManager::new(fake.clone());
    let before = manager.snapshot().unwrap();
    let restore = manager
        .restore_snapshot_for_set_enabled(true, Some("127.0.0.1:9080"), &before)
        .unwrap();
    let planned = manager
        .planned_snapshot_for_set_enabled(true, Some("127.0.0.1:9080"), &before, &[19100, 19000])
        .unwrap();
    assert_eq!(fake.0.lock().writes, 0);
    let durable_prewrite = serde_json::to_string(&(restore, planned.clone())).unwrap();
    manager
        .set_enabled_if_unchanged_for_gateway_ports(
            true,
            Some("127.0.0.1:9080"),
            &before,
            &[19100, 19000],
        )
        .unwrap();
    assert!(fake.snapshot().unwrap() == planned);
    assert!(manager.last_written_snapshot().unwrap() == planned);

    // Use only the record saved before the OS write, as if the process crashed immediately.
    let (saved_restore, saved_owned): (SystemProxySnapshot, SystemProxySnapshot) =
        serde_json::from_str(&durable_prewrite).unwrap();
    let restarted = SystemProxyManager::new(fake.clone());
    restarted.seed_restore_snapshot(saved_restore).unwrap();
    restarted.seed_last_written_snapshot(saved_owned).unwrap();
    assert_eq!(
        restarted.status().unwrap().source,
        SystemProxySource::Application
    );
    let owned = restarted.snapshot().unwrap();
    assert!(
        restarted
            .restore_snapshot_for_set_enabled(false, None, &owned)
            .unwrap()
            == original
    );
    restarted.set_enabled(false, None).unwrap();
    restarted.set_enabled(true, None).unwrap();
    assert!(fake.snapshot().unwrap() == original);
}

#[test]
fn configured_proxy_enables_manual_without_enabling_saved_pac() {
    let fake = FakeBackend::new(settings(DIRECT));
    let manager = SystemProxyManager::new(fake.clone());
    let enabled = manager.set_enabled(true, Some("127.0.0.1:9080")).unwrap();
    assert!(enabled.enabled);
    assert_eq!(enabled.mode, SystemProxyMode::Manual);
    assert_eq!(fake.snapshot().unwrap().server, "127.0.0.1:9080");
    assert!(!manager.set_enabled(false, None).unwrap().enabled);
}

#[test]
fn unknown_address_and_gateway_api_address_do_not_write() {
    let mut empty = settings(DIRECT);
    empty.server.clear();
    let fake = FakeBackend::new(empty);
    let manager = SystemProxyManager::new(fake.clone());
    assert_eq!(
        manager.set_enabled(true, None).unwrap_err(),
        SystemProxyError::MissingServer
    );
    for invalid in [
        "127.0.0.1:15721",
        "http=127.0.0.1:15721",
        "user:secret@proxy:8080",
        "https://proxy:443/path",
        "proxy:0",
        "proxy:65536",
        "proxy:8080;",
        "socks5://proxy:1080",
        ".:8080",
        "-:8080",
        "999.1.1.1:8080",
        "proxy:+8080",
    ] {
        assert_eq!(
            manager.set_enabled(true, Some(invalid)).unwrap_err(),
            SystemProxyError::InvalidServer
        );
    }
    assert_eq!(fake.0.lock().writes, 0);
    assert!(!manager.status().unwrap().enabled);
}

#[test]
fn persisted_snapshot_restores_pac_and_autodetection_after_restart() {
    let original = settings(DIRECT | PAC | AUTO_DETECT);
    let encoded = serde_json::to_string(&original).unwrap();
    let decoded: SystemProxySnapshot = serde_json::from_str(&encoded).unwrap();
    let fake = FakeBackend::new(settings(DIRECT));
    let manager = SystemProxyManager::new(fake.clone());
    manager.seed_restore_snapshot(decoded).unwrap();
    assert_eq!(fake.0.lock().writes, 0);
    assert!(manager.set_enabled(true, None).unwrap().enabled);
    assert!(fake.snapshot().unwrap() == original);
}

#[test]
fn write_failure_after_mutation_rolls_back_exact_configuration() {
    let original = settings(DIRECT | PAC);
    let fake = FakeBackend::new(original.clone());
    fake.0.lock().fail_after_write = true;
    let manager = SystemProxyManager::new(fake.clone());
    assert!(matches!(
        manager.set_enabled(false, None),
        Err(SystemProxyError::Windows {
            operation: "notify",
            code: 5
        })
    ));
    assert!(fake.snapshot().unwrap() == original);
    assert_eq!(fake.0.lock().writes, 2);
    assert!(manager.status().unwrap().error.is_some());
}

#[test]
fn setter_success_without_readback_match_is_failure() {
    let original = settings(DIRECT | MANUAL);
    let fake = FakeBackend::new(original.clone());
    fake.0.lock().refuse_write = true;
    let manager = SystemProxyManager::new(fake.clone());
    assert_eq!(
        manager.set_enabled(false, None).unwrap_err(),
        SystemProxyError::Unverified
    );
    assert!(fake.snapshot().unwrap() == original);
    assert!(manager.status().unwrap().enabled);
}

#[test]
fn external_change_before_write_is_not_overwritten() {
    let fake = FakeBackend::new(settings(DIRECT | MANUAL));
    let mut external = settings(DIRECT | PAC);
    external.server = "other:8080".into();
    fake.0.lock().external_before_write = Some(external.clone());
    let manager = SystemProxyManager::new(fake.clone());
    assert_eq!(
        manager.set_enabled(false, None).unwrap_err(),
        SystemProxyError::ExternalChange
    );
    assert!(fake.snapshot().unwrap() == external);
    assert_eq!(fake.0.lock().writes, 0);
}

#[test]
fn external_change_after_write_is_not_destroyed_by_rollback() {
    let fake = FakeBackend::new(settings(DIRECT | MANUAL));
    let mut external = settings(DIRECT | PAC);
    external.server = "other:8080".into();
    fake.0.lock().external_after_write = Some(external.clone());
    let manager = SystemProxyManager::new(fake.clone());
    assert_eq!(
        manager.set_enabled(false, None).unwrap_err(),
        SystemProxyError::ExternalChange
    );
    assert!(fake.snapshot().unwrap() == external);
    assert_eq!(fake.0.lock().writes, 1);
}

#[test]
fn durable_backup_guard_and_restore_refuse_stale_external_state() {
    let fake = FakeBackend::new(settings(DIRECT | MANUAL));
    let manager = SystemProxyManager::new(fake.clone());
    let backup = manager.snapshot().unwrap();
    manager
        .set_enabled_if_unchanged(false, None, &backup)
        .unwrap();
    let applied = manager.snapshot().unwrap();
    assert!(
        manager
            .restore_if_unchanged(&backup, &applied)
            .unwrap()
            .enabled
    );
    assert!(fake.snapshot().unwrap() == backup);
    fake.0.lock().current = settings(DIRECT | PAC);
    assert_eq!(
        manager.restore_if_unchanged(&backup, &applied).unwrap_err(),
        SystemProxyError::ExternalChange
    );
    assert_eq!(
        manager
            .set_enabled_if_unchanged(false, None, &backup)
            .unwrap_err(),
        SystemProxyError::ExternalChange
    );
    assert_eq!(fake.0.lock().writes, 2);
}

#[test]
fn tray_and_ui_concurrent_requests_are_serialized_and_idempotent() {
    let fake = FakeBackend::new(settings(DIRECT | MANUAL | PAC));
    let manager = Arc::new(SystemProxyManager::new(fake.clone()));
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let manager = manager.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                manager.set_enabled(false, None).unwrap()
            })
        })
        .collect();
    barrier.wait();
    for handle in handles {
        assert!(!handle.join().unwrap().enabled);
    }
    assert_eq!(fake.0.lock().writes, 1);
    assert!(manager.set_enabled(true, None).unwrap().enabled);
    assert_eq!(manager.status().unwrap().mode, SystemProxyMode::Mixed);
}

#[test]
fn server_validation_accepts_ipv6_and_protocol_mappings() {
    for valid in [
        "127.0.0.1:7890",
        "proxy.example:8080",
        "[::1]:7890",
        "http=127.0.0.1:7890;https=[::1]:7890",
        "http://proxy.example:8080",
        "socks=127.0.0.1:1080",
    ] {
        assert!(valid_server(valid), "valid server was rejected");
    }
}

#[test]
fn public_validation_rejects_the_dynamic_llm_listener_port() {
    assert!(validate_server("http=127.0.0.1:7890;https=[::1]:7890", 19123).is_ok());
    assert_eq!(
        validate_server("http=127.0.0.1:7890;https=[::1]:19123", 19123).unwrap_err(),
        SystemProxyError::InvalidServer
    );
    assert_eq!(
        validate_server("127.0.0.1:19123", 19123).unwrap_err(),
        SystemProxyError::InvalidServer
    );
}

#[test]
fn gateway_port_guard_applies_to_seeded_restore_and_cached_manual_address() {
    let mut restore = settings(DIRECT | MANUAL);
    restore.server = "127.0.0.1:19123".into();
    assert_eq!(
        restore.validate_for_gateway(19123).unwrap_err(),
        SystemProxyError::InvalidServer
    );
    let fake = FakeBackend::new(settings(DIRECT));
    let manager = SystemProxyManager::new(fake.clone());
    manager.seed_restore_snapshot(restore.clone()).unwrap();
    let before = manager.snapshot().unwrap();
    assert_eq!(
        manager
            .set_enabled_if_unchanged_for_gateway(true, None, &before, 19123)
            .unwrap_err(),
        SystemProxyError::InvalidServer
    );
    assert_eq!(
        manager
            .set_enabled_if_unchanged_for_gateway(true, Some("127.0.0.1:19123"), &before, 19123)
            .unwrap_err(),
        SystemProxyError::InvalidServer
    );
    assert_eq!(fake.0.lock().writes, 0);

    restore.flags = DIRECT;
    fake.0.lock().current = restore.clone();
    let manager = SystemProxyManager::new(fake.clone());
    assert_eq!(
        manager
            .set_enabled_if_unchanged_for_gateway(true, None, &restore, 19123)
            .unwrap_err(),
        SystemProxyError::InvalidServer
    );
    assert_eq!(fake.0.lock().writes, 0);
    // An unused cached manual address does not invalidate a legitimate PAC-only restore.
    restore.flags = DIRECT | PAC;
    assert!(restore.validate_for_gateway(19123).is_ok());
}

#[test]
fn port_drift_guard_rejects_configured_and_actual_listener_in_every_manual_target() {
    let fake = FakeBackend::new(settings(DIRECT));
    let manager = SystemProxyManager::new(fake.clone());
    let forbidden = [19100, 19000];
    let before = manager.snapshot().unwrap();
    for port in forbidden {
        let server = format!("127.0.0.1:{port}");
        assert!(matches!(
            manager.planned_snapshot_for_set_enabled(true, Some(&server), &before, &forbidden),
            Err(SystemProxyError::InvalidServer)
        ));
        assert_eq!(
            manager
                .set_enabled_if_unchanged_for_gateway_ports(
                    true,
                    Some(&server),
                    &before,
                    &forbidden
                )
                .unwrap_err(),
            SystemProxyError::InvalidServer
        );
    }

    let mut original = settings(DIRECT | MANUAL);
    original.server = "127.0.0.1:19000".into();
    manager.seed_restore_snapshot(original.clone()).unwrap();
    assert_eq!(
        manager
            .set_enabled_if_unchanged_for_gateway_ports(true, None, &before, &forbidden)
            .unwrap_err(),
        SystemProxyError::InvalidServer
    );

    original.flags = DIRECT;
    fake.0.lock().current = original.clone();
    let fresh = SystemProxyManager::new(fake.clone());
    assert_eq!(
        fresh
            .set_enabled_if_unchanged_for_gateway_ports(true, None, &original, &forbidden)
            .unwrap_err(),
        SystemProxyError::InvalidServer
    );
    assert_eq!(fake.0.lock().writes, 0);
    assert!(!fresh.status().unwrap().enabled);
}

#[test]
fn failed_readback_does_not_attempt_an_unverified_rollback() {
    let fake = FakeBackend::new(settings(DIRECT | MANUAL));
    fake.0.lock().fail_read_after_write = true;
    let manager = SystemProxyManager::new(fake.clone());
    assert_eq!(
        manager.set_enabled(false, None).unwrap_err(),
        SystemProxyError::Unverified
    );
    assert_eq!(fake.0.lock().writes, 1);
    assert_eq!(fake.0.lock().current.flags, DIRECT);
}

#[test]
fn uncertain_write_seeded_from_durable_plan_recovers_pac_in_same_process() {
    let original = settings(DIRECT | PAC | AUTO_DETECT);
    let fake = FakeBackend::new(original.clone());
    let manager = SystemProxyManager::new(fake.clone());
    let before = manager.snapshot().unwrap();
    let restore = manager
        .restore_snapshot_for_set_enabled(true, Some("127.0.0.1:9080"), &before)
        .unwrap();
    let planned = manager
        .planned_snapshot_for_set_enabled(true, Some("127.0.0.1:9080"), &before, &[19100, 19000])
        .unwrap();
    fake.0.lock().fail_read_after_write = true;
    assert_eq!(
        manager
            .set_enabled_if_unchanged_for_gateway_ports(
                true,
                Some("127.0.0.1:9080"),
                &before,
                &[19100, 19000]
            )
            .unwrap_err(),
        SystemProxyError::Unverified
    );
    // The coordinator retains the encrypted record and seeds its two opaque snapshots.
    manager.seed_restore_snapshot(restore).unwrap();
    manager.seed_last_written_snapshot(planned).unwrap();
    fake.0.lock().fail_read_after_write = false;
    assert_eq!(
        manager.status().unwrap().source,
        SystemProxySource::Application
    );
    assert_eq!(manager.status().unwrap().mode, SystemProxyMode::Manual);
    manager.set_enabled(false, None).unwrap();
    manager.set_enabled(true, None).unwrap();
    assert!(fake.snapshot().unwrap() == original);
    assert_eq!(fake.0.lock().writes, 3);
}

#[test]
fn failed_rollback_is_reported_and_not_retried_without_bound() {
    let fake = FakeBackend::new(settings(DIRECT | MANUAL));
    {
        let mut state = fake.0.lock();
        state.fail_after_write = true;
        state.fail_rollback = true;
    }
    let manager = SystemProxyManager::new(fake.clone());
    assert_eq!(
        manager.set_enabled(false, None).unwrap_err(),
        SystemProxyError::RollbackFailed
    );
    assert_eq!(fake.0.lock().writes, 2);
    assert!(!manager.status().unwrap().enabled);
    assert!(manager
        .status()
        .unwrap()
        .error
        .unwrap()
        .contains("恢复原状态也失败"));
}

#[test]
fn malformed_persisted_snapshot_is_rejected_without_writes() {
    let fake = FakeBackend::new(settings(DIRECT));
    let manager = SystemProxyManager::new(fake.clone());
    let mut invalid = settings(0xffff);
    assert_eq!(
        manager.seed_restore_snapshot(invalid.clone()).unwrap_err(),
        SystemProxyError::InvalidSnapshot
    );
    invalid.flags = DIRECT;
    invalid.pac_url = "hidden\0injection".into();
    assert_eq!(
        manager.seed_restore_snapshot(invalid).unwrap_err(),
        SystemProxyError::InvalidSnapshot
    );
    assert_eq!(fake.0.lock().writes, 0);
}
