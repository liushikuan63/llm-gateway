//! Per-Gateway protected sender ownership and nonsecret local state projection.
use super::{
    stack_exchange_admission::{Admission, Block, Recovery, View},
    stack_exchange_sender::{Dispatch, Failed, FailureKind, ProtectedSender},
};
use crate::db::Db;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalState {
    Ready,
    Waiting,
    Reserved,
    OutcomeUnknown,
    Exhausted,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusView {
    pub schema_version: u8,
    pub state: LocalState,
    pub not_before_utc: Option<String>,
    /// Native u64 is a decimal string at the JavaScript boundary, never a
    /// rounded balance or an invented zero for absent information.
    pub shared_remaining: Option<String>,
    pub quota_observed_at_utc: Option<String>,
    pub can_recover_unsent: bool,
    pub reason: Option<&'static str>,
}
impl StatusView {
    fn unavailable(reason: &'static str) -> Self {
        Self {
            schema_version: 1,
            state: LocalState::Unavailable,
            not_before_utc: None,
            shared_remaining: None,
            quota_observed_at_utc: None,
            can_recover_unsent: false,
            reason: Some(reason),
        }
    }
}
fn utc(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|v| v.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
}
/// Local may-attempt is not a provider availability or account balance claim.
pub fn project(view: View, now_ms: i64) -> StatusView {
    if now_ms < 0 || utc(now_ms).is_none() {
        return StatusView::unavailable("clock");
    }
    let times = [
        view.local_not_before_ms,
        view.server_not_before_ms,
        view.quota_observed_ms,
    ];
    if times
        .into_iter()
        .flatten()
        .any(|v| v < 0 || utc(v).is_none())
        || view.shared_remaining.is_some() != view.quota_observed_ms.is_some()
        || view.exhausted != (view.shared_remaining == Some(0))
        || (view.can_recover_unsent && (!view.needs_observation || view.exhausted))
    {
        return StatusView::unavailable("invalid_state");
    }
    if view.quota_observed_ms.is_some_and(|at| at > now_ms) {
        return StatusView::unavailable("clock");
    }
    let until = view
        .local_not_before_ms
        .into_iter()
        .chain(view.server_not_before_ms)
        .max();
    let (state, reason) = if view.exhausted {
        (LocalState::Exhausted, Some("quota_exhausted"))
    } else if view.needs_observation && view.can_recover_unsent {
        (LocalState::Reserved, Some("not_authorized"))
    } else if view.needs_observation {
        (LocalState::OutcomeUnknown, Some("outcome_unknown"))
    } else if until.is_some_and(|at| at > now_ms) {
        (LocalState::Waiting, Some("wait"))
    } else {
        (LocalState::Ready, None)
    };
    StatusView {
        schema_version: 1,
        state,
        not_before_utc: until.and_then(utc),
        shared_remaining: view.shared_remaining.map(|n| n.to_string()),
        quota_observed_at_utc: view.quota_observed_ms.and_then(utc),
        can_recover_unsent: state == LocalState::Reserved && view.can_recover_unsent,
        reason,
    }
}
struct Entry {
    proxy: Option<String>,
    interval_ms: u32,
    sender: Arc<ProtectedSender>,
}
pub struct StackResources {
    db: Db,
    cached: tokio::sync::Mutex<Option<Entry>>,
}
impl StackResources {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            cached: tokio::sync::Mutex::new(None),
        }
    }
    pub async fn sender(
        &self,
        proxy: Option<&str>,
        interval_ms: u32,
    ) -> Result<Arc<ProtectedSender>, Failed> {
        // Reject invalid new policy before touching a working cached producer.
        if interval_ms > 3_600_000 {
            return Err(Failed {
                kind: FailureKind::Admission(Block::InvalidPolicy),
                dispatch: Dispatch::NotStarted,
            });
        }
        if let Some(raw) = proxy {
            super::egress::validate_proxy(raw).map_err(|_| Failed {
                kind: FailureKind::Client,
                dispatch: Dispatch::NotStarted,
            })?;
        }
        let mut cached = self.cached.lock().await;
        if let Some(entry) = cached.as_ref() {
            if entry.proxy.as_deref() == proxy
                && entry.interval_ms == interval_ms
                && entry.sender.is_active().await
            {
                return Ok(entry.sender.clone());
            }
            // Keep this strong entry until retirement ACK. Cancellation during
            // the await leaves it available for a later drain to finish; no
            // orphan metadata owner or detached HTTP producer is created.
            entry.sender.retire().await;
        }
        let sender = Arc::new(ProtectedSender::with_min_interval(
            self.db.clone(),
            proxy,
            interval_ms,
        )?);
        *cached = Some(Entry {
            proxy: proxy.map(str::to_owned),
            interval_ms,
            sender: sender.clone(),
        });
        Ok(sender)
    }
    /// SELECT-only state. Does not construct a transport or query upstream.
    pub async fn state(&self) -> StatusView {
        match Admission::new(self.db.clone()).view().await {
            Ok(view) => project(view, chrono::Utc::now().timestamp_millis()),
            Err(kind) => StatusView::unavailable(match kind {
                Block::InvalidState => "invalid_state",
                Block::Clock => "clock",
                _ => "storage",
            }),
        }
    }
    pub async fn recover_unsent(
        &self,
        proxy: Option<&str>,
        interval_ms: u32,
    ) -> Result<Recovery, Failed> {
        self.sender(proxy, interval_ms)
            .await?
            .recover_unsent()
            .await
            .map_err(|kind| Failed {
                kind: FailureKind::Admission(kind),
                dispatch: Dispatch::NotStarted,
            })
    }
    /// Drain existing producers only, not a permanent ban on later explicit
    /// work after a Gateway restart. Callers own the total drain deadline.
    pub async fn drain(&self) {
        let mut cached = self.cached.lock().await;
        if let Some(entry) = cached.as_ref() {
            entry.sender.retire().await;
        }
        *cached = None;
    }
}

#[cfg(test)]
mod tests {
    use super::super::stack_exchange::{Controls, SharedQuota};
    use super::*;
    use tokio::time::{Duration, Instant};
    fn empty() -> View {
        View {
            local_not_before_ms: None,
            server_not_before_ms: None,
            shared_remaining: None,
            quota_observed_ms: None,
            needs_observation: false,
            exhausted: false,
            can_recover_unsent: false,
        }
    }
    #[test]
    fn stack_scope_projection_preserves_unknown_zero_and_native_integer_precision() {
        let unknown = project(empty(), 1_000);
        assert_eq!(unknown.state, LocalState::Ready);
        assert_eq!(unknown.shared_remaining, None);
        let mut zero = empty();
        zero.shared_remaining = Some(0);
        zero.quota_observed_ms = Some(1_000);
        zero.exhausted = true;
        let zero = project(zero, 1_000);
        assert_eq!(zero.state, LocalState::Exhausted);
        assert_eq!(zero.shared_remaining.as_deref(), Some("0"));
        assert!(zero.quota_observed_at_utc.is_some());
        let mut large = empty();
        large.shared_remaining = Some(u64::MAX);
        large.quota_observed_ms = Some(1_000);
        let json = serde_json::to_value(project(large, 1_000)).unwrap();
        assert_eq!(json["shared_remaining"], "18446744073709551615");
        for forbidden in ["ledger_id", "attempt_id", "query", "credential", "account"] {
            assert!(!json.to_string().contains(forbidden));
        }
    }
    #[test]
    fn stack_scope_projection_distinguishes_reserved_unknown_and_wait_without_reset() {
        let mut pending = empty();
        pending.needs_observation = true;
        pending.can_recover_unsent = true;
        assert_eq!(project(pending.clone(), 1_000).state, LocalState::Reserved);
        pending.can_recover_unsent = false;
        let unknown = project(pending, 86_401_000);
        assert_eq!(unknown.state, LocalState::OutcomeUnknown);
        assert!(!unknown.can_recover_unsent);
        let mut wait = empty();
        wait.local_not_before_ms = Some(2_000);
        wait.server_not_before_ms = Some(3_000);
        let waiting = project(wait.clone(), 1_000);
        assert_eq!(waiting.state, LocalState::Waiting);
        assert_eq!(
            waiting.not_before_utc.as_deref(),
            Some("1970-01-01T00:00:03Z")
        );
        let ended = project(wait, 4_000);
        assert_eq!(ended.state, LocalState::Ready);
        assert_eq!(ended.shared_remaining, None);
    }
    #[tokio::test]
    async fn stack_scope_corrupt_state_is_unavailable_without_repair_or_fake_zero() {
        let db = Db::connect_in_memory().await.unwrap();
        sqlx::query("INSERT INTO meta(key,value,updated_at) VALUES('search.stack_exchange.admission.v1','{}','owned')").execute(db.pool()).await.unwrap();
        let store = StackResources::new(db.clone());
        let v = store.state().await;
        assert_eq!(v.state, LocalState::Unavailable);
        assert_eq!(v.reason, Some("invalid_state"));
        assert_eq!(v.shared_remaining, None);
        assert!(!v.can_recover_unsent);
        let raw: String = sqlx::query_scalar(
            "SELECT value FROM meta WHERE key='search.stack_exchange.admission.v1'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(raw, "{}");
    }
    #[tokio::test]
    async fn stack_scope_cache_reuses_policy_and_retires_replacement_and_drain() {
        let db = Db::connect_in_memory().await.unwrap();
        let store = StackResources::new(db.clone());
        let a = store.sender(None, 0).await.unwrap();
        let same = store.sender(None, 0).await.unwrap();
        assert!(Arc::ptr_eq(&a, &same));
        let b = store.sender(None, 3_600_000).await.unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(!a.is_active().await);
        assert!(b.is_active().await);
        assert!(matches!(
            store
                .sender(Some("http://owned:private@127.0.0.1:1"), 0)
                .await,
            Err(Failed {
                kind: FailureKind::Client,
                ..
            })
        ));
        assert!(
            b.is_active().await,
            "invalid configuration must not retire valid current client"
        );
        assert!(matches!(
            store.sender(None, 3_600_001).await,
            Err(Failed {
                kind: FailureKind::Admission(Block::InvalidPolicy),
                ..
            })
        ));
        assert!(b.is_active().await);
        store.drain().await;
        assert!(!b.is_active().await);
        let c = store.sender(None, 3_600_000).await.unwrap();
        assert!(!Arc::ptr_eq(&b, &c));
        assert!(c.is_active().await);
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meta WHERE key='search.stack_exchange.admission.v1'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(
            rows, 0,
            "construct/read/drain must not manufacture quota/reservations"
        );
    }
    #[tokio::test]
    async fn stack_scope_recovery_consumer_keeps_authorized_and_zero_blocked() {
        for phase in ["reserved", "authorized", "zero"] {
            let db = Db::connect_in_memory().await.unwrap();
            let gate = Admission::new(db.clone());
            let r = gate
                .reserve(Instant::now() + Duration::from_secs(9))
                .await
                .unwrap();
            if phase != "reserved" {
                let a = gate.authorize(r).await.unwrap();
                if phase == "zero" {
                    let observed = chrono::Utc::now().timestamp_millis();
                    gate.finish(
                        a,
                        &Controls {
                            shared_quota: SharedQuota::Exhausted,
                            ..Default::default()
                        },
                        observed,
                    )
                    .await
                    .unwrap();
                }
            }
            let store = StackResources::new(db.clone());
            let before: String = sqlx::query_scalar(
                "SELECT value FROM meta WHERE key='search.stack_exchange.admission.v1'",
            )
            .fetch_one(db.pool())
            .await
            .unwrap();
            let result = store.recover_unsent(None, 0).await;
            if phase == "reserved" {
                assert_eq!(store.state().await.state, LocalState::Waiting);
                assert_eq!(result.unwrap(), Recovery::RecoveredUnsent);
            } else {
                let expected = if phase == "zero" {
                    Block::Exhausted
                } else {
                    Block::Unresolved
                };
                assert_eq!(
                    result.unwrap_err(),
                    Failed {
                        kind: FailureKind::Admission(expected),
                        dispatch: Dispatch::NotStarted
                    }
                );
                let after: String = sqlx::query_scalar(
                    "SELECT value FROM meta WHERE key='search.stack_exchange.admission.v1'",
                )
                .fetch_one(db.pool())
                .await
                .unwrap();
                assert_eq!(before, after);
            }
        }
    }
}
