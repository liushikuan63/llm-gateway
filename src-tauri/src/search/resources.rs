//! Per-Gateway, explicitly enabled search reuse. No detached tasks or persistence.
use super::executor::SearchOutcome;
use crate::config::SearchConfig;
use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use parking_lot::Mutex;
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};

pub const MAX_ENTRIES: usize = 128;
pub const MAX_INFLIGHT: usize = 32;
pub const MAX_WAITERS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchReuse {
    Fresh,
    Shared,
    Cached,
    Bypass,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchResourceStats {
    pub schema_version: u8,
    pub entries: usize,
    pub inflight: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub coalesced: u64,
    pub queries_started: u64,
    pub rejected: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::SearchBackendKind, search::backend::SearchResult};
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Notify;
    fn cfg() -> SearchConfig {
        SearchConfig {
            backend: SearchBackendKind::FreeAggregate,
            query_cache_ttl_secs: 10,
            coalesce_queries: true,
            ..Default::default()
        }
    }
    fn key(n: u16) -> Key {
        let mut k = [0; 32];
        k[..2].copy_from_slice(&n.to_be_bytes());
        k
    }
    fn full(label: &str) -> SearchOutcome {
        SearchOutcome {
            backend: SearchBackendKind::FreeAggregate,
            hits: 1,
            error: None,
            results: vec![SearchResult {
                title: label.into(),
                url: "https://example.test/owned".into(),
                snippet: "owned".into(),
                score: 0.0,
            }],
            result_sources: vec!["bing_rss".into()],
            failed_sources: vec![],
            reuse: None,
            retrieved_at_utc: None,
            page_text: None,
            requested_locale: None,
            unstructured_reference: None,
        }
    }
    async fn spin_until(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned coordination did not reach its barrier");
    }
    struct Dropped(Arc<AtomicUsize>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn stack_scope_gateway_resources_are_database_bound_and_shared_not_global() {
        assert!(SearchResources::new(cfg()).stack_exchange().is_none());
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let resources = SearchResources::with_database(cfg(), db.clone());
        let store = resources
            .stack_exchange()
            .expect("DB-backed resource required");
        assert!(Arc::ptr_eq(
            &store,
            &resources.clone().stack_exchange().unwrap()
        ));
        let other = SearchResources::with_database(cfg(), db.clone())
            .stack_exchange()
            .unwrap();
        assert!(!Arc::ptr_eq(&store, &other));
        assert_eq!(
            store.state().await.state,
            super::super::stack_exchange_scope::LocalState::Ready
        );
        assert_eq!(store.state().await.shared_remaining, None);
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meta WHERE key='search.stack_exchange.admission.v1'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(
            rows, 0,
            "constructing and reading resources must not reserve or invent quota"
        );
    }
    #[tokio::test]
    async fn stack_scope_gateway_drain_times_out_then_resumes_retirement_without_orphaning() {
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let resources = SearchResources::with_database(cfg(), db.clone());
        let store = resources
            .stack_exchange()
            .expect("DB-backed resource required");
        let sender = store.sender(None, 0).await.unwrap();
        // One-connection in-memory pool is the observable barrier: the real
        // recovery producer holds active ownership while its SELECT awaits it.
        let connection = db.pool().acquire().await.unwrap();
        let recovery = sender.recover_unsent();
        tokio::pin!(recovery);
        assert!(futures_util::poll!(&mut recovery).is_pending());
        assert!(resources
            .drain_source_writes(Duration::from_millis(10))
            .await
            .is_err());
        drop(connection);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), &mut recovery)
                .await
                .unwrap()
                .unwrap(),
            super::super::stack_exchange_admission::Recovery::NotNeeded
        );
        resources
            .drain_source_writes(Duration::from_secs(3))
            .await
            .unwrap();
        assert!(
            !sender.is_active().await,
            "Gateway drain must retire the cached producer"
        );
        let next = store.sender(None, 0).await.unwrap();
        assert!(!Arc::ptr_eq(&sender, &next));
        assert!(
            next.is_active().await,
            "a later explicit Gateway session may construct a new owner"
        );
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meta WHERE key='search.stack_exchange.admission.v1'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn ttl_lru_config_and_unsuccessful_outcomes_keep_distinct_boundaries() {
        let cfg = cfg();
        let r = SearchResources::new(cfg.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let c = calls.clone();
            let value = r
                .run(key(1), &cfg, move || async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    full("one")
                })
                .await;
            assert_eq!(value.hits, 1);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(r.stats().cache_hits, 1);
        let expires = r.state.lock().cache[&key(1)].expires;
        r.run(key(1), &cfg, || async {
            panic!("cache hit must not poll a factory")
        })
        .await;
        assert_eq!(
            r.state.lock().cache[&key(1)].expires,
            expires,
            "read must not extend TTL"
        );
        r.state.lock().cache.get_mut(&key(1)).unwrap().expires =
            Instant::now() - Duration::from_secs(1);
        let c = calls.clone();
        r.run(key(1), &cfg, move || async move {
            c.fetch_add(1, Ordering::SeqCst);
            full("fresh")
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        for n in 2..130 {
            r.run(key(n), &cfg, || async { full("bounded") }).await;
        }
        assert_eq!(r.stats().entries, MAX_ENTRIES);
        r.run(key(2), &cfg, || async {
            panic!("recent key should survive")
        })
        .await;
        r.run(key(130), &cfg, || async { full("last") }).await;
        {
            let s = r.state.lock();
            assert!(s.cache.contains_key(&key(2)));
            assert!(!s.cache.contains_key(&key(3)));
            assert_eq!(s.cache.len(), MAX_ENTRIES);
        }
        let mut changed = cfg.clone();
        changed.max_results = 6;
        r.reconfigure(&changed);
        assert_eq!(r.stats().entries, 0);
        let old = r
            .run(key(1), &cfg, || async { full("old captured config") })
            .await;
        assert_eq!(old.reuse, Some(SearchReuse::Bypass));
        assert_eq!(r.stats().entries, 0);
        for mode in ["partial", "failed"] {
            let c = Arc::new(AtomicUsize::new(0));
            for _ in 0..2 {
                let counter = c.clone();
                r.run(key(200), &changed, move || async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    if mode == "failed" {
                        SearchOutcome::failed(SearchBackendKind::FreeAggregate, "owned failure")
                    } else {
                        let mut p = full("partial");
                        p.failed_sources = vec!["duckduckgo".into()];
                        p
                    }
                })
                .await;
            }
            assert_eq!(c.load(Ordering::SeqCst), 2);
        }
        assert_eq!(r.stats().entries, 0);
    }

    #[tokio::test]
    async fn cancelled_first_waiter_leaves_shared_work_and_last_waiter_drops_it() {
        let mut cfg = cfg();
        cfg.query_cache_ttl_secs = 0;
        let r = Arc::new(SearchResources::new(cfg.clone()));
        let release = Arc::new(Notify::new());
        let started = Arc::new(Notify::new());
        let drops = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let first = {
            let r = r.clone();
            let cfg = cfg.clone();
            let release = release.clone();
            let started = started.clone();
            let drops = drops.clone();
            let calls = calls.clone();
            tokio::spawn(async move {
                r.run(key(1), &cfg, move || async move {
                    let _guard = Dropped(drops);
                    calls.fetch_add(1, Ordering::SeqCst);
                    started.notify_one();
                    release.notified().await;
                    full("shared")
                })
                .await
            })
        };
        started.notified().await;
        let second = {
            let r = r.clone();
            let cfg = cfg.clone();
            tokio::spawn(async move {
                r.run(key(1), &cfg, || async {
                    panic!("duplicate source operation")
                })
                .await
            })
        };
        spin_until(|| r.stats().coalesced == 1).await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        release.notify_one();
        assert_eq!(second.await.unwrap().reuse, Some(SearchReuse::Shared));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(r.stats().inflight, 0);
        let entered = Arc::new(Notify::new());
        let last = {
            let r = r.clone();
            let cfg = cfg.clone();
            let entered = entered.clone();
            let drops = drops.clone();
            tokio::spawn(async move {
                r.run(key(2), &cfg, move || async move {
                    let _guard = Dropped(drops);
                    entered.notify_one();
                    std::future::pending::<SearchOutcome>().await
                })
                .await
            })
        };
        entered.notified().await;
        last.abort();
        assert!(last.await.unwrap_err().is_cancelled());
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(r.stats().inflight, 0);
        assert_eq!(r.stats().entries, 0);
    }

    #[tokio::test]
    async fn rotation_blocks_late_fill_and_bounds_active_queries_across_epochs() {
        let cfg = cfg();
        let r = Arc::new(SearchResources::new(cfg.clone()));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let old = {
            let r = r.clone();
            let cfg = cfg.clone();
            let entered = entered.clone();
            let release = release.clone();
            tokio::spawn(async move {
                r.run(key(1), &cfg, move || async move {
                    entered.notify_one();
                    release.notified().await;
                    full("old")
                })
                .await
            })
        };
        entered.notified().await;
        let mut next = cfg.clone();
        next.max_results = 6;
        r.reconfigure(&next);
        release.notify_one();
        old.await.unwrap();
        assert_eq!(r.stats().entries, 0);
        let fresh = r.run(key(1), &next, || async { full("new") }).await;
        let hit = r
            .run(key(1), &next, || async {
                panic!("new value must be cached")
            })
            .await;
        assert_eq!(fresh.results, hit.results);
        assert_eq!(hit.results[0].title, "new");
        let started = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for n in 10..10 + MAX_INFLIGHT as u16 {
            let r = r.clone();
            let cfg = next.clone();
            let started = started.clone();
            tasks.push(tokio::spawn(async move {
                r.run(key(n), &cfg, move || async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<SearchOutcome>().await
                })
                .await
            }));
        }
        spin_until(|| started.load(Ordering::SeqCst) == MAX_INFLIGHT).await;
        let busy = r
            .run(key(900), &next, || async {
                panic!("capacity must reject before dispatch")
            })
            .await;
        assert!(busy.error.is_some());
        next.max_results = 7;
        r.reconfigure(&next);
        assert_eq!(r.stats().inflight, MAX_INFLIGHT);
        assert!(r
            .run(key(901), &next, || async {
                panic!("old epochs still consume capacity")
            })
            .await
            .error
            .is_some());
        for t in tasks {
            t.abort();
            assert!(t.await.unwrap_err().is_cancelled());
        }
        assert_eq!(r.stats().inflight, 0);
        let mut waiters = Vec::new();
        for _ in 0..MAX_WAITERS {
            let r = r.clone();
            let cfg = next.clone();
            waiters.push(tokio::spawn(async move {
                r.run(key(902), &cfg, || async {
                    std::future::pending::<SearchOutcome>().await
                })
                .await
            }));
        }
        spin_until(|| r.stats().coalesced >= MAX_WAITERS as u64 - 1).await;
        assert!(r
            .run(key(902), &next, || async {
                panic!("waiter limit must not dispatch")
            })
            .await
            .error
            .is_some());
        for t in waiters {
            t.abort();
            assert!(t.await.unwrap_err().is_cancelled());
        }
        assert_eq!(r.stats().inflight, 0);
    }
}

type Key = [u8; 32];
struct Entry {
    value: SearchOutcome,
    expires: Instant,
}
struct Flight {
    id: Arc<()>,
    epoch: Arc<()>,
    key: Key,
    future: Shared<BoxFuture<'static, SearchOutcome>>,
    waiters: AtomicUsize,
}
struct State {
    cfg: SearchConfig,
    epoch: Arc<()>,
    cache: HashMap<Key, Entry>,
    lru: VecDeque<Key>,
    active: Vec<Arc<Flight>>,
    stats: SearchResourceStats,
}
#[derive(Clone)]
pub struct SearchResources {
    state: Arc<Mutex<State>>,
    cooldowns: Option<Arc<super::cooldown::Cooldowns>>,
    stack_exchange: Option<Arc<super::stack_exchange_scope::StackResources>>,
    egress: Arc<super::egress::ClientPool>,
    pub(super) page_text_permits: Arc<tokio::sync::Semaphore>,
}

// A registry reference cannot keep an abandoned operation alive indefinitely.
// Shared polls only on behalf of live callers; the last caller removes it and
// drops both HTTP futures. A cancelled first caller is not a special leader.
struct Waiter {
    state: Weak<Mutex<State>>,
    flight: Arc<Flight>,
}
impl Drop for Waiter {
    fn drop(&mut self) {
        if self.flight.waiters.fetch_sub(1, Ordering::AcqRel) == 1 {
            if let Some(state) = self.state.upgrade() {
                state
                    .lock()
                    .active
                    .retain(|f| !Arc::ptr_eq(&f.id, &self.flight.id));
            }
        }
    }
}

impl SearchResources {
    pub fn new(cfg: SearchConfig) -> Self {
        Self {
            cooldowns: None,
            stack_exchange: None,
            page_text_permits: Arc::new(tokio::sync::Semaphore::new(2)),
            egress: Arc::new(super::egress::ClientPool::default()),
            state: Arc::new(Mutex::new(State {
                cfg,
                epoch: Arc::new(()),
                cache: HashMap::new(),
                lru: VecDeque::new(),
                active: Vec::new(),
                stats: SearchResourceStats {
                    schema_version: 1,
                    ..Default::default()
                },
            })),
        }
    }
    pub fn with_database(cfg: SearchConfig, db: crate::db::Db) -> Self {
        let mut value = Self::new(cfg);
        value.cooldowns = Some(super::cooldown::Cooldowns::new(db.clone()));
        value.stack_exchange = Some(Arc::new(super::stack_exchange_scope::StackResources::new(
            db,
        )));
        value
    }
    /// Database ownership is required; callers without it cannot construct an
    /// unprotected Stack Overflow sender. Reading this handle performs no I/O.
    pub fn stack_exchange(&self) -> Option<Arc<super::stack_exchange_scope::StackResources>> {
        self.stack_exchange.clone()
    }
    pub fn supported_scopes(&self) -> Vec<crate::config::FreeSearchScope> {
        use crate::config::FreeSearchScope as S;
        let mut scopes = vec![S::Web, S::NpmPackages, S::HackerNews];
        if self.stack_exchange.is_some() {
            scopes.push(S::StackOverflow);
        }
        scopes
    }
    pub(crate) fn cooldowns(&self) -> Option<Arc<super::cooldown::Cooldowns>> {
        self.cooldowns.clone()
    }
    pub(super) fn free_client(&self, cfg: &SearchConfig) -> Result<reqwest::Client, &'static str> {
        self.egress.client(cfg.free_proxy_url.as_deref())
    }
    pub async fn drain_source_writes(&self, deadline: Duration) -> Result<(), String> {
        let started = Instant::now();
        // One total budget, not one fresh deadline per source. A cancelled
        // retirement retains the cached strong producer for a later drain.
        tokio::time::timeout(deadline, async {
            if let Some(source) = &self.stack_exchange {
                source.drain().await;
            }
            if let Some(source) = &self.cooldowns {
                source
                    .drain(deadline.saturating_sub(started.elapsed()))
                    .await
            } else {
                Ok(())
            }
        })
        .await
        .unwrap_or_else(|_| Err("搜索源仍有本地工作尚未退场，不能确认保护状态已保存".into()))
    }
    pub async fn source_protection(
        &self,
        scope: crate::config::FreeSearchScope,
    ) -> Option<super::cooldown::ProtectionView> {
        if scope == crate::config::FreeSearchScope::StackOverflow {
            return None; // Separate protected-state contract, never Web status.
        }
        let store = self.cooldowns.as_ref()?;
        let backend = super::free_aggregate::FreeAggregateBackend::default();
        if scope == crate::config::FreeSearchScope::HackerNews {
            let target = super::cooldown::Target::new(
                super::cooldown::Source::HackerNews,
                &backend.hn_base_url,
            )
            .ok()?;
            return Some(super::cooldown::ProtectionView {
                schema_version: 1,
                sources: vec![store.view(&target).await],
            });
        }
        if scope == crate::config::FreeSearchScope::NpmPackages {
            let target = super::cooldown::Target::new(
                super::cooldown::Source::NpmRegistry,
                &backend.npm_base_url,
            )
            .ok()?;
            return Some(super::cooldown::ProtectionView {
                schema_version: 1,
                sources: vec![store.view(&target).await],
            });
        }
        let bing =
            super::cooldown::Target::new(super::cooldown::Source::BingRss, &backend.bing_base_url)
                .ok()?;
        let ddg = super::cooldown::Target::new(
            super::cooldown::Source::Duckduckgo,
            &backend.ddg_base_url,
        )
        .ok()?;
        let (a, b) = tokio::join!(store.view(&bing), store.view(&ddg));
        Some(super::cooldown::ProtectionView {
            schema_version: 1,
            sources: vec![a, b],
        })
    }
    pub(crate) fn captures_current(&self, cfg: &SearchConfig) -> bool {
        self.state.lock().cfg == *cfg
    }
    pub fn reconfigure(&self, cfg: &SearchConfig) {
        let mut state = self.state.lock();
        let changed_proxy = state.cfg.free_proxy_url != cfg.free_proxy_url;
        if state.cfg != *cfg {
            state.cfg = cfg.clone();
            state.epoch = Arc::new(());
            state.cache.clear();
            state.lru.clear();
        }
        drop(state);
        if changed_proxy {
            self.egress.clear();
        }
        // Old callers retain their captured query. Active operations remain in
        // the admission bound, but can neither join nor populate the new epoch.
    }
    pub fn stats(&self) -> SearchResourceStats {
        let state = self.state.lock();
        let mut stats = state.stats.clone();
        stats.entries = state
            .cache
            .values()
            .filter(|e| e.expires > Instant::now())
            .count();
        stats.inflight = state.active.len();
        stats
    }
    pub async fn run<F, Fut>(&self, key: Key, cfg: &SearchConfig, operation: F) -> SearchOutcome
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = SearchOutcome> + Send + 'static,
    {
        if cfg.backend != crate::config::SearchBackendKind::FreeAggregate
            || (cfg.query_cache_ttl_secs == 0 && !cfg.coalesce_queries)
        {
            return operation().await;
        }
        let (flight, reuse) = {
            let mut state = self.state.lock();
            let current = state.cfg == *cfg;
            let cache_allowed = current && cfg.query_cache_ttl_secs > 0;
            if cache_allowed {
                if let Some(entry) = state.cache.get(&key) {
                    if entry.expires > Instant::now() {
                        let mut value = entry.value.clone();
                        value.reuse = Some(SearchReuse::Cached);
                        state.lru.retain(|k| *k != key);
                        state.lru.push_back(key);
                        state.stats.cache_hits = state.stats.cache_hits.saturating_add(1);
                        return value;
                    }
                    state.cache.remove(&key);
                    state.lru.retain(|k| *k != key);
                }
            }
            let existing = if current && cfg.coalesce_queries {
                state
                    .active
                    .iter()
                    .find(|f| {
                        f.key == key
                            && Arc::ptr_eq(&f.epoch, &state.epoch)
                            && f.waiters.load(Ordering::Acquire) > 0
                    })
                    .cloned()
            } else {
                None
            };
            if let Some(flight) = existing {
                if flight.waiters.load(Ordering::Acquire) >= MAX_WAITERS {
                    state.stats.rejected = state.stats.rejected.saturating_add(1);
                    return SearchOutcome::failed(cfg.backend, "相同检索的等待数已达本机上限");
                }
                flight.waiters.fetch_add(1, Ordering::AcqRel);
                state.stats.coalesced = state.stats.coalesced.saturating_add(1);
                (flight, SearchReuse::Shared)
            } else {
                if state.active.len() >= MAX_INFLIGHT {
                    state.stats.rejected = state.stats.rejected.saturating_add(1);
                    return SearchOutcome::failed(
                        cfg.backend,
                        "本机共享检索已繁忙，本次未发起检索",
                    );
                }
                let id = Arc::new(());
                let epoch = state.epoch.clone();
                let weak = Arc::downgrade(&self.state);
                let done_id = id.clone();
                let done_epoch = epoch.clone();
                let ttl = Duration::from_secs(u64::from(cfg.query_cache_ttl_secs.min(3600)));
                let future = async move {
                    let mut value = operation().await;
                    // Reuse the actual query observation; wrapper scheduling or
                    // cache insertion must not make old metadata look newer.
                    value.retrieved_at_utc.get_or_insert_with(|| {
                        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                    });
                    if let Some(state) = weak.upgrade() {
                        let mut state = state.lock();
                        state.active.retain(|f| !Arc::ptr_eq(&f.id, &done_id));
                        if cache_allowed
                            && Arc::ptr_eq(&state.epoch, &done_epoch)
                            && value.error.is_none()
                            && value.failed_sources.is_empty()
                            && !value.results.is_empty()
                        {
                            state.cache.insert(
                                key,
                                Entry {
                                    value: value.clone(),
                                    expires: Instant::now() + ttl,
                                },
                            );
                            state.lru.retain(|k| *k != key);
                            state.lru.push_back(key);
                            while state.cache.len() > MAX_ENTRIES {
                                if let Some(old) = state.lru.pop_front() {
                                    state.cache.remove(&old);
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                    value
                }
                .boxed()
                .shared();
                let flight = Arc::new(Flight {
                    id,
                    epoch,
                    key,
                    future,
                    waiters: AtomicUsize::new(1),
                });
                state.active.push(flight.clone());
                state.stats.queries_started = state.stats.queries_started.saturating_add(1);
                if cache_allowed {
                    state.stats.cache_misses = state.stats.cache_misses.saturating_add(1);
                }
                (
                    flight,
                    if current {
                        SearchReuse::Fresh
                    } else {
                        SearchReuse::Bypass
                    },
                )
            }
        };
        let _waiter = Waiter {
            state: Arc::downgrade(&self.state),
            flight: flight.clone(),
        };
        let mut value = flight.future.clone().await;
        value.reuse = Some(reuse);
        value
    }
}
