//! One coordinator for startup, due ticks and manual scans. Only database lease
//! owners perform I/O, and neither SQLite nor configuration guards cross it.

use super::observation::{self, CatalogObservation, ReadFailure};
use crate::{
    config::AppConfig,
    db::{
        repo::{
            self,
            catalog::{self, ApplySummary, ClaimOutcome, ScanTrigger},
        },
        Db,
    },
    domain::Provider,
    proxy::server::GatewayState,
};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};
use tokio::sync::{watch, Notify, Semaphore};

#[async_trait]
pub trait CatalogReader: Send + Sync {
    async fn read(
        &self,
        provider: &Provider,
        proxy: Option<&str>,
    ) -> Result<CatalogObservation, ReadFailure>;
}
struct BuiltinReader;
#[async_trait]
impl CatalogReader for BuiltinReader {
    async fn read(
        &self,
        p: &Provider,
        proxy: Option<&str>,
    ) -> Result<CatalogObservation, ReadFailure> {
        observation::observe(p, proxy).await
    }
}

pub trait Clock: Send + Sync {
    fn wall_ms(&self) -> i64;
    fn monotonic_ms(&self) -> u64;
}
struct SystemClock(Instant);
impl Clock for SystemClock {
    fn wall_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
    fn monotonic_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

struct Control {
    run_id: String,
    cancel: watch::Sender<bool>,
}
type Controls = Arc<Mutex<HashMap<String, Control>>>;
struct RunningGuard {
    controls: Controls,
    provider_id: String,
    run_id: String,
}
impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut controls = self.controls.lock();
        if controls
            .get(&self.provider_id)
            .is_some_and(|c| c.run_id == self.run_id)
        {
            controls.remove(&self.provider_id);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReply {
    pub status: String,
    pub summary: Option<ApplySummary>,
    pub failure_kind: Option<String>,
}
impl ScanReply {
    fn simple(status: &str) -> Self {
        Self {
            status: status.into(),
            summary: None,
            failure_kind: None,
        }
    }
}

pub struct Manager {
    db: Db,
    cfg: Arc<parking_lot::RwLock<AppConfig>>,
    owner: String,
    reader: Arc<dyn CatalogReader>,
    clock: Arc<dyn Clock>,
    slots: Arc<Semaphore>,
    controls: Controls,
    config_generation: AtomicU64,
    clock_quiet: Mutex<HashMap<String, (i64, u64)>>,
    wake: Arc<Notify>,
}
impl Manager {
    pub fn new(db: Db, cfg: Arc<parking_lot::RwLock<AppConfig>>) -> Self {
        Self::with_dependencies(
            db,
            cfg,
            Arc::new(BuiltinReader),
            Arc::new(SystemClock(Instant::now())),
        )
    }
    /// Rust-level dependency injection; no IPC or configuration selects a reader.
    pub fn with_dependencies(
        db: Db,
        cfg: Arc<parking_lot::RwLock<AppConfig>>,
        reader: Arc<dyn CatalogReader>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            db,
            cfg,
            owner: uuid::Uuid::new_v4().to_string(),
            reader,
            clock,
            slots: Arc::new(Semaphore::new(2)),
            controls: Arc::new(Mutex::new(HashMap::new())),
            config_generation: AtomicU64::new(0),
            clock_quiet: Mutex::new(HashMap::new()),
            wake: Arc::new(Notify::new()),
        }
    }
    pub fn cancel(&self, provider_id: &str) -> bool {
        self.controls
            .lock()
            .get(provider_id)
            .is_some_and(|c| c.cancel.send(true).is_ok())
    }
    pub fn configuration_changed(&self) {
        self.config_generation.fetch_add(1, Ordering::SeqCst);
        for control in self.controls.lock().values() {
            let _ = control.cancel.send(true);
        }
        self.clock_quiet.lock().clear();
        self.wake.notify_one();
    }
    pub fn is_running(&self, provider_id: &str) -> bool {
        self.controls.lock().contains_key(provider_id)
    }

    pub async fn request(
        &self,
        provider_id: &str,
        trigger: ScanTrigger,
    ) -> anyhow::Result<ScanReply> {
        let generation = self.config_generation.load(Ordering::SeqCst);
        let config = { self.cfg.read().clone() };
        if trigger != ScanTrigger::Manual
            && (!config.catalog_maintenance.enabled
                || !config
                    .catalog_maintenance
                    .provider_ids
                    .iter()
                    .any(|id| id == provider_id))
        {
            return Ok(ScanReply::simple("disabled"));
        }
        if self.is_running(provider_id) {
            return Ok(ScanReply::simple("busy"));
        }
        let _slot = match self.slots.clone().try_acquire_owned() {
            Ok(slot) => slot,
            Err(_) => return Ok(ScanReply::simple("busy")),
        };
        let provider = repo::list_providers(self.db.pool())
            .await?
            .into_iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| anyhow::anyhow!("catalog provider not found"))?;
        let started_wall = self.clock.wall_ms();
        let started_monotonic = self.clock.monotonic_ms();
        let mut claim = catalog::claim_scan_checked(
            self.db.pool(),
            &provider,
            &self.owner,
            || self.clock.wall_ms(),
            trigger,
        )
        .await?;
        if matches!(claim, ClaimOutcome::ClockAnomaly) {
            let state = catalog::list_status(self.db.pool())
                .await?
                .into_iter()
                .find(|s| s.provider_id == provider_id)
                .ok_or_else(|| anyhow::anyhow!("catalog clock state missing"))?;
            let quiet = {
                let mut clocks = self.clock_quiet.lock();
                let entry = clocks
                    .entry(provider_id.into())
                    .or_insert((state.fence, started_monotonic));
                if entry.0 != state.fence {
                    *entry = (state.fence, started_monotonic);
                }
                started_monotonic.saturating_sub(entry.1)
            };
            if quiet >= catalog::LEASE_MS as u64
                && catalog::recover_clock_after_quiet_interval(
                    self.db.pool(),
                    provider_id,
                    state.fence,
                    started_wall,
                    i64::try_from(quiet).unwrap_or(i64::MAX),
                )
                .await?
            {
                self.clock_quiet.lock().remove(provider_id);
                claim = catalog::claim_scan_checked(
                    self.db.pool(),
                    &provider,
                    &self.owner,
                    || self.clock.wall_ms(),
                    trigger,
                )
                .await?;
            }
        }
        let lease = match claim {
            ClaimOutcome::Acquired(lease) => lease,
            ClaimOutcome::NotDue => return Ok(ScanReply::simple("not_due")),
            ClaimOutcome::Busy => return Ok(ScanReply::simple("busy")),
            ClaimOutcome::Conflict => return Ok(ScanReply::simple("conflict")),
            ClaimOutcome::Disabled => return Ok(ScanReply::simple("disabled")),
            ClaimOutcome::ClockAnomaly => return Ok(ScanReply::simple("clock_anomaly")),
        };
        let (cancel, mut cancelled) = watch::channel(false);
        self.controls.lock().insert(
            provider_id.into(),
            Control {
                run_id: lease.run_id.clone(),
                cancel,
            },
        );
        let _running = RunningGuard {
            controls: self.controls.clone(),
            provider_id: provider_id.into(),
            run_id: lease.run_id.clone(),
        };
        if self.config_generation.load(Ordering::SeqCst) != generation {
            catalog::fail_scan_checked(self.db.pool(), &lease, "cancelled", None, || {
                self.clock.wall_ms()
            })
            .await?;
            return Ok(ScanReply::simple("cancelled"));
        }
        let started_wall = self.clock.wall_ms();
        let started_monotonic = self.clock.monotonic_ms();
        let result = tokio::select! {biased;
            _=cancelled.changed()=>Err(ReadFailure{kind:"cancelled".into(),retry_after_at:None}),
            result=tokio::time::timeout(Duration::from_secs(35),self.reader.read(&provider,config.http_proxy.as_deref()))=>result.unwrap_or_else(|_|Err(ReadFailure{kind:"timeout".into(),retry_after_at:None})),
        };
        let finished_wall = self.clock.wall_ms();
        let monotonic_elapsed = self.clock.monotonic_ms().saturating_sub(started_monotonic);
        let wall_elapsed = finished_wall.saturating_sub(started_wall);
        if wall_elapsed < 0
            || wall_elapsed.abs_diff(i64::try_from(monotonic_elapsed).unwrap_or(i64::MAX)) > 60_000
        {
            catalog::fail_scan_checked(self.db.pool(), &lease, "clock_changed", None, || {
                self.clock.wall_ms()
            })
            .await?;
            return Ok(ScanReply {
                status: "clock_anomaly".into(),
                summary: None,
                failure_kind: Some("clock_changed".into()),
            });
        }
        match result {
            Ok(observation) => {
                let summary = catalog::apply_observation_checked(
                    self.db.pool(),
                    &lease,
                    &observation,
                    || {
                        let wall = self.clock.wall_ms();
                        let elapsed = self.clock.monotonic_ms().saturating_sub(started_monotonic);
                        let wall_elapsed = wall.saturating_sub(started_wall);
                        if self.config_generation.load(Ordering::SeqCst) != generation
                            || *cancelled.borrow()
                            || wall_elapsed < 0
                            || wall_elapsed.abs_diff(i64::try_from(elapsed).unwrap_or(i64::MAX))
                                > 60_000
                        {
                            None
                        } else {
                            Some(wall)
                        }
                    },
                )
                .await?;
                Ok(ScanReply {
                    status: if summary.conflict {
                        "conflict"
                    } else if !summary.applied {
                        "superseded"
                    } else if observation.complete_success_eligible() {
                        "complete"
                    } else {
                        "incomplete"
                    }
                    .into(),
                    summary: Some(summary),
                    failure_kind: None,
                })
            }
            Err(error) => {
                catalog::fail_scan_checked(
                    self.db.pool(),
                    &lease,
                    &error.kind,
                    error.retry_after_at,
                    || self.clock.wall_ms(),
                )
                .await?;
                Ok(ScanReply {
                    status: if error.kind == "cancelled" {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into(),
                    summary: None,
                    failure_kind: Some(error.kind),
                })
            }
        }
    }

    pub async fn tick(&self) -> anyhow::Result<Vec<ScanReply>> {
        self.tick_for(ScanTrigger::Periodic).await
    }

    pub async fn startup(&self) -> anyhow::Result<Vec<ScanReply>> {
        self.tick_for(ScanTrigger::Startup).await
    }

    async fn tick_for(&self, trigger: ScanTrigger) -> anyhow::Result<Vec<ScanReply>> {
        let settings = { self.cfg.read().catalog_maintenance.clone() };
        if !settings.enabled {
            return Ok(Vec::new());
        }
        let mut replies = Vec::new();
        // A due tick never replays missed days. Durable next_due/retry and leases
        // arbitrate each provider, including another app process or a manual run.
        for id in settings.provider_ids {
            match self.request(&id, trigger).await {
                Ok(reply) => replies.push(reply),
                Err(_) => replies.push(ScanReply {
                    status: "failed".into(),
                    summary: None,
                    failure_kind: Some("storage_or_provider_unavailable".into()),
                }),
            }
        }
        Ok(replies)
    }

    pub async fn publish_pending(gateway: &GatewayState) -> anyhow::Result<()> {
        let pending: Vec<_> = catalog::list_status(gateway.db.pool())
            .await?
            .into_iter()
            .filter(|s| s.runtime_pending)
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        gateway.reload_providers().await?;
        for state in pending {
            if let Some(run) = state.run_id {
                catalog::acknowledge_runtime(
                    gateway.db.pool(),
                    &state.provider_id,
                    &run,
                    state.fence,
                )
                .await?;
            }
        }
        Ok(())
    }

    pub fn spawn_background(gateway: Weak<GatewayState>) {
        let Some(wake) = gateway.upgrade().map(|g| g.catalog.wake.clone()) else {
            return;
        };
        tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut startup = true;
            loop {
                tokio::select! { _=interval.tick()=>{}, _=wake.notified()=>{} }
                let Some(gateway) = gateway.upgrade() else {
                    break;
                };
                let result = if startup {
                    gateway.catalog.startup().await
                } else {
                    gateway.catalog.tick().await
                };
                startup = false;
                if result.is_err() {
                    tracing::warn!(
                        "catalog maintenance storage unavailable; durable state retained"
                    );
                }
                // Retrying publication never repeats the supplier read.
                if Self::publish_pending(&gateway).await.is_err() {
                    tracing::warn!("catalog persisted; runtime reload remains pending");
                }
            }
        });
    }
}
