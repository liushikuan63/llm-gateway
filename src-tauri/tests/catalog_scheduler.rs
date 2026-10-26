use async_trait::async_trait;
use llm_gateway_lib::{
    config::{AppConfig, CatalogMaintenanceConfig},
    db::{
        repo::{
            self,
            catalog::{self, ScanTrigger, DAY_MS, LEASE_MS},
        },
        Db,
    },
    domain::Provider,
    model_catalog::{
        maintenance::{CatalogReader, Clock, Manager},
        observation::{decode_openrouter, CatalogObservation, ReadFailure},
    },
    proxy::server::GatewayState,
};
use std::sync::{
    atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{Notify, Semaphore};

struct TestClock {
    wall: AtomicI64,
    mono: AtomicU64,
    after_read: AtomicBool,
    finished_snapshot: Notify,
}
impl TestClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            wall: AtomicI64::new(chrono::Utc::now().timestamp_millis()),
            mono: AtomicU64::new(0),
            after_read: AtomicBool::new(false),
            finished_snapshot: Notify::new(),
        })
    }
    fn advance(&self, ms: i64) {
        self.wall.fetch_add(ms, Ordering::SeqCst);
        self.mono.fetch_add(ms as u64, Ordering::SeqCst);
    }
}
impl Clock for TestClock {
    fn wall_ms(&self) -> i64 {
        let result = self.wall.load(Ordering::SeqCst);
        if self.after_read.swap(false, Ordering::SeqCst) {
            self.finished_snapshot.notify_one();
        }
        result
    }
    fn monotonic_ms(&self) -> u64 {
        self.mono.load(Ordering::SeqCst)
    }
}

struct FixtureReader {
    calls: AtomicUsize,
    entered: Notify,
    release: Semaphore,
    blocked: bool,
    clock: Arc<TestClock>,
}
impl FixtureReader {
    fn new(clock: Arc<TestClock>, blocked: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release: Semaphore::new(0),
            blocked,
            clock,
        })
    }
}
#[async_trait]
impl CatalogReader for FixtureReader {
    async fn read(&self, _: &Provider, _: Option<&str>) -> Result<CatalogObservation, ReadFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if self.blocked {
            self.release.acquire().await.unwrap().forget();
        }
        self.clock.after_read.store(true, Ordering::SeqCst);
        Ok(decode_openrouter(&serde_json::json!({"data":[{
            "id":"owned/model:free","context_length":32768,
            "architecture":{"input_modalities":["text"],"output_modalities":["text"]},
            "pricing":{"prompt":"0","completion":"0","request":"0","image":"0","web_search":"0","internal_reasoning":"0","input_cache_read":"0","input_cache_write":"0"}
        }]})).unwrap())
    }
}

async fn fixture(enabled: bool) -> (Db, Arc<parking_lot::RwLock<AppConfig>>, Arc<TestClock>) {
    let db = Db::connect_in_memory().await.unwrap();
    // An official target identity is a pure fixture: only FixtureReader runs.
    sqlx::query("INSERT INTO providers(id,name,base_url,api_key_enc,created_at,updated_at) VALUES('owned','Owned','https://openrouter.ai/api/v1','','2026-10-18T00:00:00Z','2026-10-18T00:00:00Z')").execute(db.pool()).await.unwrap();
    let cfg = Arc::new(parking_lot::RwLock::new(AppConfig {
        catalog_maintenance: CatalogMaintenanceConfig {
            enabled,
            provider_ids: vec!["owned".into()],
        },
        ..Default::default()
    }));
    (db, cfg, TestClock::new())
}

#[tokio::test]
async fn startup_periodic_manual_and_restart_share_durable_rolling_due() {
    let (db, cfg, clock) = fixture(false).await;
    let reader = FixtureReader::new(clock.clone(), false);
    let manager =
        Manager::with_dependencies(db.clone(), cfg.clone(), reader.clone(), clock.clone());
    assert!(manager.startup().await.unwrap().is_empty());
    assert!(manager.tick().await.unwrap().is_empty());
    assert_eq!(reader.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        manager
            .request("owned", ScanTrigger::Manual)
            .await
            .unwrap()
            .status,
        "complete"
    );
    assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
    cfg.write().catalog_maintenance.enabled = true;
    manager.configuration_changed();
    let restarted =
        Manager::with_dependencies(db.clone(), cfg.clone(), reader.clone(), clock.clone());
    assert_eq!(restarted.startup().await.unwrap()[0].status, "not_due");
    clock.advance(DAY_MS - 1);
    assert_eq!(restarted.tick().await.unwrap()[0].status, "not_due");
    assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
    clock.advance(1);
    assert_eq!(restarted.tick().await.unwrap()[0].status, "complete");
    assert_eq!(reader.calls.load(Ordering::SeqCst), 2);
    let status = catalog::list_status(db.pool()).await.unwrap().remove(0);
    assert_eq!(status.next_due_at, Some(clock.wall_ms() + DAY_MS));
    assert!(status.runtime_pending);
}

#[tokio::test]
async fn manual_and_startup_are_single_flight_and_configuration_change_cancels_without_apply() {
    let (db, cfg, clock) = fixture(true).await;
    let reader = FixtureReader::new(clock.clone(), true);
    let manager = Arc::new(Manager::with_dependencies(
        db.clone(),
        cfg.clone(),
        reader.clone(),
        clock,
    ));
    let task = {
        let m = manager.clone();
        tokio::spawn(async move { m.request("owned", ScanTrigger::Manual).await.unwrap() })
    };
    reader.entered.notified().await;
    assert_eq!(manager.startup().await.unwrap()[0].status, "busy");
    assert_eq!(
        manager
            .request("owned", ScanTrigger::Manual)
            .await
            .unwrap()
            .status,
        "busy"
    );
    assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
    cfg.write().catalog_maintenance.enabled = false;
    manager.configuration_changed();
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.status, "cancelled");
    assert!(!manager.is_running("owned"));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_inventory")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let state = catalog::list_status(db.pool()).await.unwrap().remove(0);
    assert_eq!(state.failure_kind.as_deref(), Some("cancelled"));
    assert_eq!(state.last_complete_success_at, None);
}

#[tokio::test]
async fn expired_lease_after_waiting_for_database_cannot_publish_observation() {
    let (db, cfg, clock) = fixture(true).await;
    let reader = FixtureReader::new(clock.clone(), true);
    let manager = Arc::new(Manager::with_dependencies(
        db.clone(),
        cfg.clone(),
        reader.clone(),
        clock.clone(),
    ));
    let task = {
        let m = manager.clone();
        tokio::spawn(async move { m.request("owned", ScanTrigger::Manual).await.unwrap() })
    };
    reader.entered.notified().await;
    // The in-memory pool has exactly one connection. Hold it after the network
    // read starts, then advance both clocks after the result timestamp is read.
    let blocked = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    reader.release.add_permits(1);
    clock.finished_snapshot.notified().await;
    clock.advance(LEASE_MS);
    blocked.rollback().await.unwrap();
    let result = task.await.unwrap();
    assert_eq!(result.status, "superseded");
    assert!(!result.summary.unwrap().applied);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_inventory")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        catalog::list_status(db.pool()).await.unwrap()[0].last_complete_success_at,
        None
    );
    // The replacement owner can still finish; expiry is not a permanently stuck state.
    let reader = FixtureReader::new(clock.clone(), false);
    let replacement = Manager::with_dependencies(db.clone(), cfg, reader.clone(), clock);
    assert_eq!(replacement.startup().await.unwrap()[0].status, "complete");
    assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        repo::list_models_of(db.pool(), "owned")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn clock_jump_during_read_fails_closed_without_updating_inventory() {
    let (db, cfg, clock) = fixture(true).await;
    let reader = FixtureReader::new(clock.clone(), true);
    let manager = Arc::new(Manager::with_dependencies(
        db.clone(),
        cfg,
        reader.clone(),
        clock.clone(),
    ));
    let task = {
        let m = manager.clone();
        tokio::spawn(async move { m.request("owned", ScanTrigger::Manual).await.unwrap() })
    };
    reader.entered.notified().await;
    clock.wall.fetch_add(61_000, Ordering::SeqCst);
    reader.release.add_permits(1);
    let result = task.await.unwrap();
    assert_eq!(result.status, "clock_anomaly");
    let state = catalog::list_status(db.pool()).await.unwrap().remove(0);
    assert!(state.clock_anomaly);
    assert_eq!(state.failure_kind.as_deref(), Some("clock_changed"));
    assert_eq!(state.last_complete_success_at, None);
}

#[tokio::test]
async fn failed_runtime_publication_stays_durable_and_retry_does_not_repeat_supplier_read() {
    let (db, cfg, clock) = fixture(false).await;
    let reader = FixtureReader::new(clock.clone(), false);
    let manager = Manager::with_dependencies(db.clone(), cfg.clone(), reader.clone(), clock);
    manager.request("owned", ScanTrigger::Manual).await.unwrap();
    let before = catalog::list_status(db.pool()).await.unwrap().remove(0);
    assert!(!catalog::acknowledge_runtime(
        db.pool(),
        "owned",
        before.run_id.as_deref().unwrap(),
        before.fence - 1
    )
    .await
    .unwrap());
    assert!(catalog::list_status(db.pool()).await.unwrap()[0].runtime_pending);
    let gateway = GatewayState::new(db.clone(), cfg.read().clone());
    sqlx::query("ALTER TABLE agent_runtimes RENAME TO owned_unavailable_runtimes")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(Manager::publish_pending(&gateway).await.is_err());
    assert!(catalog::list_status(db.pool()).await.unwrap()[0].runtime_pending);
    sqlx::query("ALTER TABLE owned_unavailable_runtimes RENAME TO agent_runtimes")
        .execute(db.pool())
        .await
        .unwrap();
    Manager::publish_pending(&gateway).await.unwrap();
    assert!(!catalog::list_status(db.pool()).await.unwrap()[0].runtime_pending);
    assert_eq!(
        gateway.providers.read()[0].models[0].upstream,
        "owned/model:free"
    );
    assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
    assert!(!catalog::acknowledge_runtime(
        db.pool(),
        "owned",
        before.run_id.as_deref().unwrap(),
        before.fence - 1
    )
    .await
    .unwrap());
}

#[test]
fn legacy_config_defaults_off_and_invalid_selected_ids_are_rejected() {
    let old: AppConfig = toml::from_str("catalog_auto_update = true").unwrap();
    assert!(!old.catalog_maintenance.enabled);
    assert!(old.catalog_maintenance.provider_ids.is_empty());
    for ids in [
        vec!["".into()],
        vec![" owned".into()],
        vec!["owned".into(), "owned".into()],
        vec!["x".repeat(513)],
    ] {
        assert!(CatalogMaintenanceConfig {
            enabled: true,
            provider_ids: ids
        }
        .validate()
        .is_err());
    }
    let expected = CatalogMaintenanceConfig {
        enabled: true,
        provider_ids: vec!["owned".into()],
    };
    let value = toml::to_string(&AppConfig {
        catalog_maintenance: expected.clone(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        toml::from_str::<AppConfig>(&value)
            .unwrap()
            .catalog_maintenance,
        expected
    );
}

#[tokio::test]
async fn local_inventory_search_uses_name_features_without_promoting_paid_models_or_mutating_scans()
{
    let (db, _, clock) = fixture(false).await;
    let p = repo::list_providers(db.pool()).await.unwrap().remove(0);
    let now = clock.wall_ms();
    let lease = match catalog::claim_scan(db.pool(), &p, "inventory", now, ScanTrigger::Manual)
        .await
        .unwrap()
    {
        catalog::ClaimOutcome::Acquired(l) => l,
        other => panic!("{other:?}"),
    };
    let rows:Vec<_>=(0..102).map(|i|serde_json::json!({"id":format!("owned/model-{i:03}:free"),"name":format!("Free candidate {i:03}"),"context_length":32768,"architecture":{"input_modalities":["text"],"output_modalities":["text"]},"pricing":{"prompt":"1","completion":"0","request":"0","image":"0","web_search":"0","internal_reasoning":"0","input_cache_read":"0","input_cache_write":"0"}})).collect();
    let observation = decode_openrouter(&serde_json::json!({"data":rows})).unwrap();
    let result = catalog::apply_observation(db.pool(), &lease, &observation, now)
        .await
        .unwrap();
    assert_eq!(result.configured, 0);
    assert!(repo::list_models_of(db.pool(), "owned")
        .await
        .unwrap()
        .is_empty());
    let before = serde_json::to_value(catalog::list_status(db.pool()).await.unwrap()).unwrap();
    let first = catalog::list_inventory(db.pool(), "owned", "FREE", true, 0)
        .await
        .unwrap();
    assert_eq!(first.total, 102);
    assert_eq!(first.entries.len(), 100);
    assert!(first.entries.iter().all(|entry| entry.free_name_hint
        && entry.fee_status == "paid"
        && entry.aliases.is_empty()));
    let last = catalog::list_inventory(db.pool(), "owned", "", true, 100)
        .await
        .unwrap();
    assert_eq!(last.entries.len(), 2);
    assert_eq!(last.entries[0].upstream_id, "owned/model-100:free");
    let exact = catalog::list_inventory(db.pool(), "owned", " candidate 101 ", false, 0)
        .await
        .unwrap();
    assert_eq!(exact.total, 1);
    assert_eq!(
        serde_json::to_value(catalog::list_status(db.pool()).await.unwrap()).unwrap(),
        before,
        "search must not claim, refresh or advance a scan"
    );
    assert!(catalog::list_inventory(db.pool(), "owned", "x\n", false, 0)
        .await
        .is_err());
    assert!(catalog::list_inventory(db.pool(), "owned", "", false, 4097)
        .await
        .is_err());
}

#[tokio::test]
async fn cancelled_final_checkpoint_rolls_back_inventory_models_and_complete_watermark() {
    let (db, _, clock) = fixture(false).await;
    let p = repo::list_providers(db.pool()).await.unwrap().remove(0);
    let now = clock.wall_ms();
    let lease = match catalog::claim_scan(db.pool(), &p, "cancel-commit", now, ScanTrigger::Manual)
        .await
        .unwrap()
    {
        catalog::ClaimOutcome::Acquired(l) => l,
        other => panic!("{other:?}"),
    };
    let reader = FixtureReader::new(clock, false);
    let observation = reader.read(&p, None).await.unwrap();
    let checkpoints = AtomicUsize::new(0);
    let result = catalog::apply_observation_checked(db.pool(), &lease, &observation, || {
        if checkpoints.fetch_add(1, Ordering::SeqCst) == 0 {
            Some(now)
        } else {
            None
        }
    })
    .await
    .unwrap();
    assert!(!result.applied);
    assert_eq!(checkpoints.load(Ordering::SeqCst), 2);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_inventory")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(repo::list_models_of(db.pool(), "owned")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        catalog::list_status(db.pool()).await.unwrap()[0].last_complete_success_at,
        None
    );
}

#[tokio::test]
async fn stale_pricing_refresh_cannot_overwrite_manual_prices_saved_after_planning() {
    let (db, cfg, clock) = fixture(false).await;
    let reader = FixtureReader::new(clock.clone(), false);
    Manager::with_dependencies(db.clone(), cfg, reader, clock)
        .request("owned", ScanTrigger::Manual)
        .await
        .unwrap();
    let before = repo::list_providers(db.pool()).await.unwrap().remove(0);
    let mut proposed = before.models[0].price.clone().unwrap();
    proposed.prompt = 1.0;
    let mut edited = before.clone();
    let manual = edited.models[0].price.as_mut().unwrap();
    manual.prompt = 99.0;
    manual.source = llm_gateway_lib::domain::PriceSource::Manual;
    edited.models[0].enabled = false;
    repo::upsert_provider_config(db.pool(), &edited)
        .await
        .unwrap();
    assert!(
        !repo::update_model_price(db.pool(), &before, &before.models[0], &proposed)
            .await
            .unwrap(),
        "a stale feed plan cannot overwrite newer manual intent"
    );
    let after = repo::list_models_of(db.pool(), "owned")
        .await
        .unwrap()
        .remove(0);
    assert_eq!(after.price, edited.models[0].price);
    assert!(!after.enabled);
}

#[tokio::test]
async fn pricing_cas_accepts_current_catalog_but_rejects_rebound_alias_and_rotated_target() {
    let (db, cfg, clock) = fixture(false).await;
    let reader = FixtureReader::new(clock.clone(), false);
    Manager::with_dependencies(db.clone(), cfg, reader, clock)
        .request("owned", ScanTrigger::Manual)
        .await
        .unwrap();
    let p = repo::list_providers(db.pool()).await.unwrap().remove(0);
    let mut proposed = p.models[0].price.clone().unwrap();
    proposed.prompt = 1.0;
    assert!(
        repo::update_model_price(db.pool(), &p, &p.models[0], &proposed)
            .await
            .unwrap()
    );
    let fresh = repo::list_providers(db.pool()).await.unwrap().remove(0);
    assert_eq!(fresh.models[0].price.as_ref().unwrap().prompt, 1.0);
    sqlx::query("UPDATE providers SET api_key_enc='owned-generation-2' WHERE id='owned'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        !repo::update_model_price(db.pool(), &fresh, &fresh.models[0], &proposed)
            .await
            .unwrap()
    );
    let rebound = repo::list_providers(db.pool()).await.unwrap().remove(0);
    sqlx::query("UPDATE models SET upstream='owned/other-model' WHERE provider_id='owned'")
        .execute(db.pool())
        .await
        .unwrap();
    proposed.prompt = 3.0;
    assert!(
        !repo::update_model_price(db.pool(), &rebound, &rebound.models[0], &proposed)
            .await
            .unwrap()
    );
    assert_eq!(
        repo::list_models_of(db.pool(), "owned").await.unwrap()[0]
            .price
            .as_ref()
            .unwrap()
            .prompt,
        1.0
    );
}
