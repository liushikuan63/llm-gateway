//! Nonsecret source admission reservations and acknowledged Retry-After state.
//! Existing meta rows only; no query, credential or supplier entitlement data.
use crate::db::Db;
use chrono::{Datelike, TimeZone, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    BingRss,
    Duckduckgo,
    NpmRegistry,
    HackerNews,
}
impl Source {
    pub fn code(self) -> &'static str {
        match self {
            Self::BingRss => "bing_rss",
            Self::Duckduckgo => "duckduckgo",
            Self::NpmRegistry => "npm_registry",
            Self::HackerNews => "hacker_news",
        }
    }
}
#[derive(Clone)]
pub struct Target {
    source: &'static str,
    key: String,
}
impl Target {
    pub(super) fn result_page(url: &reqwest::Url) -> Result<Self, Failure> {
        let mut normalized = url.clone();
        if let Some(domain) = url.domain().map(|d| d.trim_end_matches('.').to_owned()) {
            normalized.set_host(Some(&domain)).map_err(|_| Failure)?;
        }
        let origin = normalized.origin().ascii_serialization();
        // Reuse the physical source identity: an article path on an already
        // protected source must not escape its acknowledged wait.
        for (source, base) in [
            (Source::BingRss, "https://cn.bing.com"),
            (Source::Duckduckgo, "https://lite.duckduckgo.com"),
            (Source::NpmRegistry, "https://registry.npmjs.org"),
            (Source::HackerNews, "https://hn.algolia.com"),
        ] {
            if origin == base {
                return Self::new(source, base);
            }
        }
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(Failure);
        }
        let digest = Sha256::digest(origin.as_bytes());
        Ok(Self {
            source: "result_page",
            key: format!("search.free_cooldown.v1.result_page.{digest:x}"),
        })
    }

    /// Separate fixed logical service, not a legacy aggregate GET source.
    pub(super) fn mdn_mcp() -> Self {
        let digest = Sha256::digest(crate::search::mdn_mcp_contract::ENDPOINT.as_bytes());
        Self {
            source: "mdn_mcp",
            key: format!("search.source_cooldown.v1.mdn_mcp.{digest:x}"),
        }
    }

    /// Local spacing belongs to this transaction admission. Keep it separate
    /// from acknowledged service waits checked between all protocol POSTs.
    pub(super) fn mdn_local_transaction() -> Self {
        let mut target = Self::mdn_mcp();
        target.key.push_str(".local_transaction");
        target
    }

    pub fn new(source: Source, base: &str) -> Result<Self, Failure> {
        let mut url = reqwest::Url::parse(base).map_err(|_| Failure)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Failure);
        }
        url.set_path(match source {
            Source::BingRss => "/search",
            Source::Duckduckgo => "/lite/",
            Source::NpmRegistry => "/-/v1/search",
            Source::HackerNews => "/api/v1/search",
        });
        let digest = Sha256::digest(format!("{}\0{}", source.code(), url));
        Ok(Self {
            source: source.code(),
            key: format!("search.free_cooldown.v1.{}.{digest:x}", source.code()),
        })
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Failure;
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WaitReason {
    #[default]
    RetryAfter,
    LocalInterval,
    ServicePolicy,
}
impl WaitReason {
    fn code(self) -> &'static str {
        match self {
            Self::RetryAfter => "retry_after",
            Self::LocalInterval => "local_interval",
            Self::ServicePolicy => "service_policy",
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u8,
    not_before_ms: i64,
    observed_ms: i64,
    #[serde(default)]
    reason: WaitReason,
}
#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub source: &'static str,
    pub state: &'static str,
    pub not_before_utc: Option<String>,
    pub reason: Option<&'static str>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ProtectionView {
    pub schema_version: u8,
    pub sources: Vec<View>,
}

/// RFC9110 delay-seconds and the three HTTP-date forms. No guessed backoff.
pub fn retry_after(value: &str, now: i64) -> Option<i64> {
    let value = value.trim_matches(|c| c == ' ' || c == '\t');
    if value.is_empty() || value.len() > 128 || !value.is_ascii() {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        let delta = value.parse::<u64>().ok()?.checked_mul(1000)?;
        let until = now.checked_add(i64::try_from(delta).ok()?)?;
        return chrono::DateTime::from_timestamp_millis(until).map(|_| until);
    }
    let imf = chrono::NaiveDateTime::parse_from_str(value, "%a, %d %b %Y %H:%M:%S GMT").ok();
    let asctime = chrono::NaiveDateTime::parse_from_str(value, "%a %b %e %H:%M:%S %Y").ok();
    let obsolete = (|| {
        let (weekday, tail) = value.split_once(", ")?;
        let (date, time) = tail.split_once(' ')?;
        let parts: Vec<_> = date.split('-').collect();
        if parts.len() != 3 || parts[2].len() != 2 || !parts[2].bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let year_now = chrono::DateTime::from_timestamp_millis(now)?.year();
        let mut year = year_now.div_euclid(100) * 100 + parts[2].parse::<i32>().ok()?;
        if year > year_now + 50 {
            year -= 100;
        }
        chrono::NaiveDateTime::parse_from_str(
            &format!("{weekday}, {}-{}-{year} {time}", parts[0], parts[1]),
            "%A, %d-%b-%Y %H:%M:%S GMT",
        )
        .ok()
    })();
    imf.or(asctime)
        .or(obsolete)
        .map(|d| Utc.from_utc_datetime(&d).timestamp_millis())
}

pub struct Cooldowns {
    db: Db,
    memory: Mutex<HashMap<String, i64>>,
    pending: AtomicUsize,
    failed: AtomicBool,
    changed: Notify,
    permits: Arc<Semaphore>,
}
struct Completion {
    owner: Arc<Cooldowns>,
    settled: bool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.settled {
            self.owner.failed.store(true, Ordering::Release);
        }
        self.owner.pending.fetch_sub(1, Ordering::AcqRel);
        self.owner.changed.notify_waiters();
    }
}
impl Cooldowns {
    pub fn new(db: Db) -> Arc<Self> {
        Arc::new(Self {
            db,
            memory: Mutex::new(HashMap::new()),
            pending: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            changed: Notify::new(),
            permits: Arc::new(Semaphore::new(64)),
        })
    }
    async fn read_raw(&self, target: &Target) -> Result<Option<(String, Record)>, Failure> {
        // Bound corrupt metadata before deserialization; no full meta-table scan.
        let raw: Option<String> =
            sqlx::query_scalar("SELECT substr(value,1,1025) FROM meta WHERE key=?")
                .bind(&target.key)
                .fetch_optional(self.db.pool())
                .await
                .map_err(|_| Failure)?;
        raw.map(|raw| {
            if raw.len() > 1024 {
                return Err(Failure);
            }
            let r: Record = serde_json::from_str(&raw).map_err(|_| Failure)?;
            if r.schema_version != 1
                || r.observed_ms < 0
                || r.not_before_ms < r.observed_ms
                || chrono::DateTime::from_timestamp_millis(r.not_before_ms).is_none()
            {
                return Err(Failure);
            }
            Ok((raw, r))
        })
        .transpose()
    }
    async fn read(&self, target: &Target) -> Result<Option<Record>, Failure> {
        self.read_raw(target).await.map(|v| v.map(|(_, r)| r))
    }
    async fn compare_store(
        &self,
        target: &Target,
        old: Option<&str>,
        record: &Record,
    ) -> Result<bool, Failure> {
        let value = serde_json::to_string(record).map_err(|_| Failure)?;
        let changed = if let Some(old) = old {
            sqlx::query("UPDATE meta SET value=?,updated_at=? WHERE key=? AND value=?")
                .bind(value)
                .bind(Utc::now())
                .bind(&target.key)
                .bind(old)
                .execute(self.db.pool())
                .await
                .map_err(|_| Failure)?
                .rows_affected()
        } else {
            // Bound the new origin namespace without evicting future, corrupt
            // or pending constraints. At capacity refuse new targets. Legacy
            // four-source behavior is unchanged; no bulk meta cleanup.
            sqlx::query("INSERT OR IGNORE INTO meta(key,value,updated_at) SELECT ?,?,? WHERE ? != 'result_page' OR (SELECT COUNT(*) FROM meta WHERE key LIKE 'search.free_cooldown.v1.result_page.%') < 256")
                .bind(&target.key)
                .bind(value)
                .bind(Utc::now())
                .bind(target.source)
                .execute(self.db.pool())
                .await
                .map_err(|_| Failure)?
                .rows_affected()
        };
        Ok(changed == 1)
    }
    fn store_memory(&self, target: &Target, until: i64) -> Result<(), Failure> {
        let mut memory = self.memory.lock();
        if target.source == "result_page"
            && !memory.contains_key(&target.key)
            && memory
                .keys()
                .filter(|k| k.starts_with("search.free_cooldown.v1.result_page."))
                .count()
                >= 256
        {
            self.failed.store(true, Ordering::Release);
            return Err(Failure);
        }
        memory
            .entry(target.key.clone())
            .and_modify(|v| *v = (*v).max(until))
            .or_insert(until);
        Ok(())
    }
    fn memory_blocks(&self, target: &Target, now: i64) -> Result<bool, Failure> {
        if now < 0 || self.failed.load(Ordering::Acquire) {
            return Err(Failure);
        }
        Ok(self
            .memory
            .lock()
            .get(&target.key)
            .is_some_and(|until| *until > now))
    }
    /// Reserve operator time before dispatch. This is not a debit or evidence
    /// of actual wire timing; cancellation never refunds an admitted slot.
    pub(super) async fn reserve_page(
        &self,
        target: &Target,
        interval_ms: u32,
    ) -> Result<bool, Failure> {
        if target.source == "hacker_news" && interval_ms < 360 {
            self.reserve_service_policy(target, 360).await
        } else {
            self.reserve(target, interval_ms).await
        }
    }
    pub async fn reserve(&self, target: &Target, interval_ms: u32) -> Result<bool, Failure> {
        self.reserve_reason(target, interval_ms, WaitReason::LocalInterval)
            .await
    }
    pub(super) async fn reserve_service_policy(
        &self,
        target: &Target,
        interval_ms: u32,
    ) -> Result<bool, Failure> {
        self.reserve_reason(target, interval_ms, WaitReason::ServicePolicy)
            .await
    }
    async fn reserve_reason(
        &self,
        target: &Target,
        interval_ms: u32,
        reason: WaitReason,
    ) -> Result<bool, Failure> {
        if interval_ms > 3_600_000 {
            return Err(Failure);
        }
        for _ in 0..16 {
            let now = Utc::now().timestamp_millis();
            if self.memory_blocks(target, now)? {
                return Ok(false);
            }
            let old = self.read_raw(target).await?;
            // A DB-pool/read wait is a prerequisite, not elapsed time charged
            // to a new slot. Restrictions may also have arrived during it.
            let now = Utc::now().timestamp_millis();
            if self.memory_blocks(target, now)? {
                return Ok(false);
            }
            if old.as_ref().is_some_and(|(_, r)| r.not_before_ms > now) {
                return Ok(false);
            }
            if target.source == "result_page" && old.is_none() {
                let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM meta WHERE key LIKE 'search.free_cooldown.v1.result_page.%'")
                    .fetch_one(self.db.pool()).await.map_err(|_|Failure)?;
                if count >= 256 {
                    return Err(Failure);
                }
            }
            if interval_ms == 0 {
                return Ok(true);
            }
            let until = now.checked_add(i64::from(interval_ms)).ok_or(Failure)?;
            let value = Record {
                schema_version: 1,
                not_before_ms: until,
                observed_ms: now,
                reason,
            };
            if self
                .compare_store(target, old.as_ref().map(|(raw, _)| raw.as_str()), &value)
                .await?
            {
                // Commit acknowledgement may itself have waited. Do not
                // dispatch from an expired slot or hide a newly known rule
                // beneath this attempt's own (possibly longer) reservation.
                let confirmed_at = Utc::now().timestamp_millis();
                if self.memory_blocks(target, confirmed_at)? {
                    return Ok(false);
                }
                if until <= confirmed_at {
                    return Err(Failure);
                }
                self.store_memory(target, until)?;
                return Ok(true);
            }
        }
        Err(Failure)
    }
    pub async fn allowed(&self, target: &Target) -> Result<bool, Failure> {
        let now = Utc::now().timestamp_millis();
        if self.memory_blocks(target, now)? {
            return Ok(false);
        }
        let record = self.read(target).await?;
        let now = Utc::now().timestamp_millis();
        Ok(!self.memory_blocks(target, now)? && record.map_or(true, |r| r.not_before_ms <= now))
    }
    async fn extend(&self, target: &Target, until: i64, observed: i64) -> Result<(), Failure> {
        for _ in 0..16 {
            let old = self.read_raw(target).await?;
            if let Some((_, old)) = &old {
                if old.not_before_ms >= until {
                    return Ok(());
                }
            }
            let value = Record {
                schema_version: 1,
                not_before_ms: until,
                observed_ms: observed,
                reason: WaitReason::RetryAfter,
            };
            if self
                .compare_store(target, old.as_ref().map(|(raw, _)| raw.as_str()), &value)
                .await?
            {
                return Ok(());
            }
        }
        Err(Failure)
    }
    pub async fn remember(
        self: &Arc<Self>,
        target: Target,
        until: i64,
        observed: i64,
    ) -> Result<(), Failure> {
        if observed < 0
            || chrono::DateTime::from_timestamp_millis(observed).is_none()
            || chrono::DateTime::from_timestamp_millis(until).is_none()
        {
            return Err(Failure);
        }
        if until <= observed {
            return Ok(());
        }
        self.store_memory(&target, until)?;
        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            self.failed.store(true, Ordering::Release);
            Failure
        })?;
        self.pending.fetch_add(1, Ordering::AcqRel);
        let mut completion = Completion {
            owner: self.clone(),
            settled: false,
            _permit: permit,
        };
        let owner = self.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Only local storage is supervised beyond caller cancellation. No HTTP
        // work or retries are spawned; shutdown explicitly drains this group.
        tokio::spawn(async move {
            let result = owner.extend(&target, until, observed).await;
            if result.is_err() {
                owner.failed.store(true, Ordering::Release);
            }
            completion.settled = true;
            drop(completion);
            let _ = tx.send(result);
        });
        rx.await.map_err(|_| Failure)?
    }
    pub async fn drain(&self, deadline: Duration) -> Result<(), String> {
        tokio::time::timeout(deadline, async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.pending.load(Ordering::Acquire) == 0 {
                    return if self.failed.load(Ordering::Acquire) {
                        Err("检索源限制写入失败，不能确认保护状态已保存".into())
                    } else {
                        Ok(())
                    };
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| Err("关闭时仍有检索源保护状态尚未提交".into()))
    }
    pub async fn view(&self, target: &Target) -> View {
        let source = target.source;
        if self.failed.load(Ordering::Acquire) {
            return View {
                source,
                state: "unavailable",
                not_before_utc: None,
                reason: None,
            };
        }
        match self.read(target).await {
            Err(_) => View {
                source,
                state: "unavailable",
                not_before_utc: None,
                reason: None,
            },
            Ok(record) => {
                let reason = record.as_ref().map(|r| r.reason.code());
                let persisted = record.map(|r| r.not_before_ms);
                let until = persisted
                    .into_iter()
                    .chain(self.memory.lock().get(&target.key).copied())
                    .max();
                let state =
                    if let Some(until) = until.filter(|u| *u > Utc::now().timestamp_millis()) {
                        if persisted.is_some_and(|p| p >= until) {
                            "cooldown"
                        } else {
                            "cooldown_pending"
                        }
                    } else {
                        "not_blocked_locally"
                    };
                View {
                    source,
                    state,
                    reason: if state == "cooldown_pending" {
                        None
                    } else {
                        reason
                    },
                    not_before_utc: until
                        .and_then(chrono::DateTime::from_timestamp_millis)
                        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo;
    fn target() -> Target {
        Target::new(Source::BingRss, "https://example.test").unwrap()
    }
    async fn wait_pending(store: &Cooldowns) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while store.pending.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn admission_rechecks_a_shorter_known_rule_during_cas_wait() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let t = target();
        let held = db.pool().acquire().await.unwrap();
        let mut request = Box::pin(store.reserve(&t, 60000));
        assert!(futures_util::poll!(&mut request).is_pending());
        let mut barrier = Box::pin(db.pool().acquire());
        assert!(futures_util::poll!(&mut barrier).is_pending());
        drop(held);
        let held_cas = tokio::select! {r=&mut request=>panic!("fixture expected CAS acquisition behind barrier: {r:?}"),b=&mut barrier=>b.unwrap()};
        assert!(futures_util::poll!(&mut request).is_pending());
        let now = Utc::now().timestamp_millis();
        let mut known = Box::pin(store.remember(t.clone(), now + 30000, now));
        assert!(futures_util::poll!(&mut known).is_pending());
        drop(held_cas);
        let actual = request.await.unwrap();
        known.await.unwrap();
        store.drain(Duration::from_secs(3)).await.unwrap();
        assert!(!actual,"a newly observed source restriction must suppress dispatch even when shorter than the admitted operator interval");
    }
    #[tokio::test]
    async fn admission_starts_its_clock_after_pool_read_wait() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let target = target();
        let held = db.pool().acquire().await.unwrap();
        let mut request = Box::pin(store.reserve(&target, 60000));
        assert!(futures_util::poll!(&mut request).is_pending());
        tokio::time::sleep(Duration::from_millis(20)).await;
        let released_at = Utc::now().timestamp_millis();
        drop(held);
        assert!(request.await.unwrap());
        let record = store.read(&target).await.unwrap().unwrap();
        assert!(
            record.observed_ms >= released_at,
            "DB waiting must not consume the new interval: observed={} released={}",
            record.observed_ms,
            released_at
        );
        assert_eq!(record.not_before_ms - record.observed_ms, 60000);
    }
    #[tokio::test]
    async fn admission_rechecks_pending_rules_observed_during_read_wait() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let t = target();
        let now = Utc::now().timestamp_millis();
        let held = db.pool().acquire().await.unwrap();
        let mut request = Box::pin(store.reserve(&t, 0));
        assert!(futures_util::poll!(&mut request).is_pending());
        let mut barrier = Box::pin(db.pool().acquire());
        assert!(futures_util::poll!(&mut barrier).is_pending());
        let mut known = Box::pin(store.remember(t.clone(), now + 60000, now));
        assert!(futures_util::poll!(&mut known).is_pending());
        assert_eq!(store.pending.load(Ordering::Acquire), 1);
        drop(held);
        let actual = request.await.unwrap();
        let held_again = barrier.await.unwrap();
        drop(held_again);
        known.await.unwrap();
        store.drain(Duration::from_secs(3)).await.unwrap();
        assert!(!actual,"a known in-memory limit observed during DB wait must be rechecked before admitting interval0");
    }
    #[tokio::test]
    async fn admission_rejects_a_slot_expired_while_the_cas_waited_for_pool() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let t = target();
        let held = db.pool().acquire().await.unwrap();
        let mut request = Box::pin(store.reserve_service_policy(&t, 20));
        assert!(futures_util::poll!(&mut request).is_pending());
        let mut barrier = Box::pin(db.pool().acquire());
        assert!(futures_util::poll!(&mut barrier).is_pending());
        drop(held);
        let held_cas = tokio::select! {r=&mut request=>panic!("fixture expected write acquisition behind FIFO barrier, got {r:?}"),b=&mut barrier=>b.unwrap()};
        assert!(futures_util::poll!(&mut request).is_pending());
        tokio::time::sleep(Duration::from_millis(40)).await;
        drop(held_cas);
        let actual = request.await;
        assert!(
            actual.is_err(),
            "an already expired acknowledged slot must not grant source HTTP admission: {actual:?}"
        );
    }
    #[tokio::test]
    async fn source_snapshot_preserves_the_committed_millisecond_deadline() {
        let store = Cooldowns::new(Db::connect_in_memory().await.unwrap());
        let now = Utc::now().timestamp_millis().div_euclid(1000) * 1000;
        let until = now + 60123;
        store.remember(target(), until, now).await.unwrap();
        let view = store.view(&target()).await;
        let displayed =
            chrono::DateTime::parse_from_rfc3339(view.not_before_utc.as_deref().unwrap())
                .unwrap()
                .timestamp_millis();
        assert_eq!(
            displayed, until,
            "source snapshot must not claim a boundary earlier than the actual committed wait"
        );
    }
    #[tokio::test]
    async fn interface_policy_reason_is_durable_and_does_not_shorten_server_waits() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let t = Target::new(Source::HackerNews, "https://example.test").unwrap();
        assert!(store.reserve_service_policy(&t, 360).await.unwrap());
        let record = store.read(&t).await.unwrap().unwrap();
        assert_eq!(record.not_before_ms - record.observed_ms, 360);
        assert_eq!(record.reason.code(), "service_policy");
        let fresh = Cooldowns::new(db);
        assert_eq!(
            fresh.read(&t).await.unwrap().unwrap().reason.code(),
            "service_policy"
        );
        let now = Utc::now().timestamp_millis();
        fresh.remember(t.clone(), now + 120000, now).await.unwrap();
        assert!(!fresh.reserve_service_policy(&t, 360).await.unwrap());
        assert!(!fresh.reserve(&t, 0).await.unwrap());
        assert_eq!(fresh.view(&t).await.reason, Some("retry_after"));
    }
    #[tokio::test]
    async fn interval_reservations_are_atomic_across_independent_owners() {
        let db = Db::connect_in_memory().await.unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let mut jobs = Vec::new();
        for _ in 0..8 {
            let store = Cooldowns::new(db.clone());
            let barrier = barrier.clone();
            jobs.push(tokio::spawn(async move {
                barrier.wait().await;
                store.reserve(&target(), 60000).await.unwrap()
            }));
        }
        let mut admitted = 0;
        for job in jobs {
            admitted += usize::from(job.await.unwrap());
        }
        assert_eq!(
            admitted, 1,
            "independent memory overlays must share the atomic DB admission"
        );
        let reopened = Cooldowns::new(db.clone());
        assert!(!reopened.reserve(&target(), 0).await.unwrap());
        assert_eq!(
            reopened.view(&target()).await.reason,
            Some("local_interval")
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meta")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }
    #[tokio::test]
    async fn interval_zero_legacy_records_and_longer_server_deadlines_remain_distinct() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let t = target();
        assert!(store.reserve(&t, 0).await.unwrap());
        assert!(repo::meta_get(db.pool(), &t.key).await.unwrap().is_none());
        let now = Utc::now().timestamp_millis();
        // Legacy schema1 records have no reason field. An expired record must
        // not become a permanent block; a pending one still means Retry-After.
        repo::meta_set(db.pool(), &t.key, &serde_json::json!({"schema_version":1,"observed_ms":now-2000,"not_before_ms":now-1000}).to_string()).await.unwrap();
        assert_eq!(store.view(&t).await.reason, Some("retry_after"));
        assert!(store.reserve(&t, 60000).await.unwrap());
        assert_eq!(store.view(&t).await.reason, Some("local_interval"));
        store.remember(t.clone(), now + 120000, now).await.unwrap();
        store.remember(t.clone(), now + 1000, now).await.unwrap();
        assert_eq!(
            store.read(&t).await.unwrap().unwrap().not_before_ms,
            now + 120000
        );
        assert_eq!(store.view(&t).await.reason, Some("retry_after"));
        assert!(!Cooldowns::new(db).reserve(&t, 0).await.unwrap());
    }
    #[tokio::test]
    async fn failed_interval_commit_never_admits_a_slot() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        sqlx::raw_sql("CREATE TRIGGER owned_interval_failure BEFORE INSERT ON meta BEGIN SELECT RAISE(ABORT,'owned interval failure'); END;").execute(db.pool()).await.unwrap();
        assert!(store.reserve(&target(), 60000).await.is_err());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meta")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert!(store.reserve(&target(), 3_600_001).await.is_err());
        // 0 makes no new reservation, so the same write-only failure is irrelevant.
        assert!(store.reserve(&target(), 0).await.unwrap());
    }
    #[tokio::test]
    async fn cancelled_admitted_owner_does_not_refund_committed_time() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(async move {
            assert!(store.reserve(&target(), 60000).await.unwrap());
            tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        rx.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let new_owner = Cooldowns::new(db);
        assert!(!new_owner.reserve(&target(), 0).await.unwrap());
        assert_eq!(
            new_owner.view(&target()).await.reason,
            Some("local_interval")
        );
    }
    #[test]
    fn retry_after_accepts_seconds_and_all_http_dates_without_clipping_or_guessing() {
        let now = 784111717000;
        for h in [
            "60",
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(retry_after(h, now), Some(784111777000), "{h}");
        }
        assert_eq!(
            retry_after("Tuesday, 01-Jan-75 00:00:00 GMT", 1767225600000),
            Some(3313526400000)
        );
        assert_eq!(retry_after("0", now), Some(now));
        assert_eq!(retry_after(" 60\t", now), Some(now + 60000));
        for h in ["", "-1", "+1", "1.5", "tomorrow", "18446744073709551615"] {
            assert_eq!(retry_after(h, now), None);
        }
        assert_eq!(retry_after(&"1".repeat(129), now), None);
    }
    #[tokio::test]
    async fn durable_updates_are_extend_only_and_corrupt_records_fail_closed() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let t = target();
        let now = Utc::now().timestamp_millis();
        let mut jobs = Vec::new();
        for secs in [2, 60, 5, 120, 3, 90, 10, 1] {
            let s = store.clone();
            let t = t.clone();
            jobs.push(tokio::spawn(async move {
                s.remember(t, now + secs * 1000, now).await
            }));
        }
        for job in jobs {
            job.await.unwrap().unwrap();
        }
        store.drain(Duration::from_secs(3)).await.unwrap();
        let reopened = Cooldowns::new(db.clone());
        assert!(!reopened.allowed(&t).await.unwrap());
        assert_eq!(
            reopened.read(&t).await.unwrap().unwrap().not_before_ms,
            now + 120000
        );
        reopened.remember(t.clone(), now + 1000, now).await.unwrap();
        assert_eq!(
            reopened.read(&t).await.unwrap().unwrap().not_before_ms,
            now + 120000
        );
        assert_eq!(reopened.view(&t).await.state, "cooldown");
        let serialized = repo::meta_get(db.pool(), &t.key).await.unwrap().unwrap();
        assert!(!serialized.contains("example.test"));
        assert!(!serialized.contains("query"));
        repo::meta_set(db.pool(), &t.key, "owned-private-corrupt-record")
            .await
            .unwrap();
        let fresh = Cooldowns::new(db.clone());
        assert!(fresh.allowed(&t).await.is_err());
        assert_eq!(fresh.view(&t).await.state, "unavailable");
        repo::meta_set(db.pool(), &t.key, &"x".repeat(2048))
            .await
            .unwrap();
        assert!(fresh.allowed(&t).await.is_err());
    }
    #[tokio::test]
    async fn cancelled_caller_cannot_cancel_submitted_metadata_commit() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let t = target();
        let now = Utc::now().timestamp_millis();
        let held = db.pool().acquire().await.unwrap();
        let caller = {
            let s = store.clone();
            let t = t.clone();
            tokio::spawn(async move { s.remember(t, now + 60000, now).await })
        };
        wait_pending(&store).await;
        assert!(
            !store.allowed(&t).await.unwrap(),
            "pending local protection must work without waiting for the occupied DB"
        );
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(tokio::time::timeout(
            Duration::from_millis(20),
            store.drain(Duration::from_secs(3))
        )
        .await
        .is_err());
        drop(held);
        store.drain(Duration::from_secs(3)).await.unwrap();
        let new = Cooldowns::new(db.clone());
        assert_eq!(
            new.read(&t).await.unwrap().unwrap().not_before_ms,
            now + 60000
        );
        assert!(!new.allowed(&t).await.unwrap());
    }
    #[tokio::test]
    async fn storage_failure_is_not_a_clean_commit_or_source_availability() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = Cooldowns::new(db.clone());
        let t = target();
        let now = Utc::now().timestamp_millis();
        sqlx::raw_sql("CREATE TRIGGER owned_source_write_failure BEFORE INSERT ON meta BEGIN SELECT RAISE(ABORT,'owned private storage failure'); END;").execute(db.pool()).await.unwrap();
        assert!(store.remember(t.clone(), now + 60000, now).await.is_err());
        assert!(store.drain(Duration::from_secs(3)).await.is_err());
        assert!(store.allowed(&t).await.is_err());
        assert_eq!(store.view(&t).await.state, "unavailable");
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meta")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn source_failure_does_not_poison_model_request_audit_health() {
        let db = Db::connect_in_memory().await.unwrap();
        let gateway = crate::proxy::server::GatewayState::new(
            db.clone(),
            crate::config::AppConfig::default(),
        );
        let store = gateway.search_resources.cooldowns().unwrap();
        let now = Utc::now().timestamp_millis();
        sqlx::raw_sql("CREATE TRIGGER owned_source_failure BEFORE INSERT ON meta BEGIN SELECT RAISE(ABORT,'owned source failure'); END;").execute(db.pool()).await.unwrap();
        assert!(store.remember(target(), now + 60000, now).await.is_err());
        assert!(
            gateway.audit_failure().is_none(),
            "search-state failure must not be mislabeled as request-audit failure"
        );
        assert!(gateway
            .drain_audits(Duration::from_secs(3))
            .await
            .unwrap_err()
            .contains("检索源"));
    }

    #[tokio::test]
    async fn actual_gateway_stop_waits_for_the_cancelled_callers_local_commit() {
        let db = Db::connect_in_memory().await.unwrap();
        let gateway = Arc::new(crate::proxy::server::GatewayState::new(
            db.clone(),
            crate::config::AppConfig {
                port: 0,
                ..Default::default()
            },
        ));
        let service = Arc::new(crate::proxy_control::gateway::GatewayService::new(
            gateway.clone(),
        ));
        service.set_enabled(true).await.unwrap();
        let store = gateway.search_resources.cooldowns().unwrap();
        let now = Utc::now().timestamp_millis();
        let held = db.pool().acquire().await.unwrap();
        let caller = {
            let s = store.clone();
            tokio::spawn(async move { s.remember(target(), now + 60000, now).await })
        };
        wait_pending(&store).await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let mut stopped = {
            let service = service.clone();
            tokio::spawn(async move { service.set_enabled(false).await })
        };
        let waiting = tokio::time::timeout(Duration::from_millis(20), &mut stopped).await;
        drop(held);
        let done = tokio::time::timeout(Duration::from_secs(3), stopped)
            .await
            .unwrap()
            .unwrap();
        assert!(
            waiting.is_err(),
            "stop cannot report clean while the DB is deliberately occupied"
        );
        done.unwrap();
        assert!(gateway.listener_snapshot().is_none());
        assert_eq!(
            store.read(&target()).await.unwrap().unwrap().not_before_ms,
            now + 60000
        );
        store.drain(Duration::from_secs(3)).await.unwrap();
    }
}
