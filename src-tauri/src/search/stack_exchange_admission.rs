//! Fixed Stack Overflow method ledger. A reservation is not a sent/billed receipt.
//! Protected consumer uses this ledger; no live scope or automatic quota reset.
use super::stack_exchange::Controls;
use crate::db::Db;
use std::sync::Arc;
use tokio::time::Instant;

const KEY: &str = "search.stack_exchange.admission.v1";
const METHOD_INTERVAL_MS: i64 = 60_000;
const MAX_STATE_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    Storage,
    InvalidState,
    InvalidPolicy,
    Clock,
    Deadline,
    Unresolved,
    Exhausted,
    Wait(i64),
    ForeignAttempt,
    Retired,
}
#[derive(Debug, Clone)]
struct Ticket {
    owner: Arc<tokio::sync::RwLock<bool>>,
    ledger_id: String,
    id: String,
    admitted_ms: i64,
    valid_until_ms: i64,
    deadline: Instant,
}
#[derive(Debug)]
pub struct Reservation(Ticket);
#[derive(Debug)]
pub struct Attempt(Ticket);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Current,
    Stale,
    ControlsUnresolved,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    NotNeeded,
    RecoveredUnsent,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub local_not_before_ms: Option<i64>,
    pub server_not_before_ms: Option<i64>,
    pub shared_remaining: Option<u64>,
    pub quota_observed_ms: Option<i64>,
    pub needs_observation: bool,
    pub exhausted: bool,
    pub can_recover_unsent: bool,
}
#[derive(Clone)]
pub struct Admission {
    owner: Arc<tokio::sync::RwLock<bool>>,
    db: Db,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    interval_ms: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Reserved,
    Authorized,
    Observed,
    NotSent,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<Phase>,
    ledger_id: String,
    attempt_id: String,
    created_ms: i64,
    updated_ms: i64,
    local_not_before_ms: i64,
    server_not_before_ms: Option<i64>,
    pending: bool,
    reported_remaining: Option<u64>,
    quota_observed_ms: Option<i64>,
    exhausted: bool,
}

fn valid_ms(ms: i64) -> bool {
    ms >= 0 && chrono::DateTime::from_timestamp_millis(ms).is_some()
}
fn valid_id(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|v| v.get_version_num() == 4 && v.to_string() == s)
}
impl Record {
    fn valid(&self) -> bool {
        let phase_valid = match (self.schema_version, self.phase) {
            // A legacy pending row contains no no-send evidence. Read it, but
            // never promote its absence of phase into Reserved authority.
            (1, None) => self.pending || self.reported_remaining.is_some(),
            (2, Some(Phase::Reserved)) => {
                self.pending
                    && !self.exhausted
                    && self
                        .quota_observed_ms
                        .map_or(true, |at| at <= self.created_ms)
            }
            (2, Some(Phase::Authorized)) => self.pending,
            (2, Some(Phase::Observed)) => !self.pending && self.reported_remaining.is_some(),
            (2, Some(Phase::NotSent)) => {
                !self.pending
                    && !self.exhausted
                    && self
                        .quota_observed_ms
                        .map_or(true, |at| at <= self.created_ms)
            }
            _ => false,
        };
        phase_valid
            && valid_id(&self.ledger_id)
            && valid_id(&self.attempt_id)
            && valid_ms(self.created_ms)
            && valid_ms(self.updated_ms)
            && self.updated_ms >= self.created_ms
            && valid_ms(self.local_not_before_ms)
            && self.local_not_before_ms >= self.created_ms
            && self.server_not_before_ms.map_or(true, valid_ms)
            && self
                .quota_observed_ms
                .map_or(true, |v| valid_ms(v) && v <= self.updated_ms)
            && self.reported_remaining.is_some() == self.quota_observed_ms.is_some()
            && self.exhausted == (self.reported_remaining == Some(0))
    }
}

impl Admission {
    pub fn new(db: Db) -> Self {
        Self {
            owner: Arc::new(tokio::sync::RwLock::new(true)),
            db,
            interval_ms: METHOD_INTERVAL_MS,
            clock: Arc::new(|| chrono::Utc::now().timestamp_millis()),
        }
    }
    /// User policy may lengthen the conservative minute, never shorten it.
    pub fn with_min_interval(db: Db, interval_ms: u32) -> Result<Self, Block> {
        if interval_ms > 3_600_000 {
            return Err(Block::InvalidPolicy);
        }
        let mut gate = Self::new(db);
        gate.interval_ms = i64::from(interval_ms).max(METHOD_INTERVAL_MS);
        Ok(gate)
    }
    /// Irreversibly close this owner and all its clones to mutation/authorization.
    /// Awaiting waits for admitted DB operations, even if their public caller was
    /// cancelled. This does not cancel external HTTP or quiesce other DB users;
    /// callers must stop those producers before restoring/replacing a database.
    /// Dropping this future, runtime shutdown or task panic is not proof of a
    /// completed retirement. View remains available for read-only diagnostics.
    pub async fn retire(&self) {
        *self.owner.write().await = false;
    }

    async fn supervised<T, F, Fut>(&self, operation: F) -> Result<T, Block>
    where
        T: Send + 'static,
        F: FnOnce(Self) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Block>> + Send + 'static,
    {
        let active = self.owner.clone().read_owned().await;
        if !*active {
            return Err(Block::Retired);
        }
        let gate = self.clone();
        // A SQLite worker can continue a submitted command after its caller is
        // dropped. Detaching this private task preserves the guard through the
        // DB acknowledgment. No HTTP/sender work is spawned here. The caller
        // cannot obtain an abort handle to the supervising task.
        tokio::spawn(async move {
            let _active = active;
            operation(gate).await
        })
        .await
        .map_err(|_| Block::Storage)?
    }
    /// Explicit DB-only recovery of a current, non-rolled-back ledger's
    /// never-authorized reservation. No quota fact/wait is refunded or reset.
    /// Restored/imported snapshots require their own restore fence: an old
    /// Reserved snapshot can predate a later authorization. This API cannot
    /// detect arbitrary rollback and must not be treated as a restore protocol.
    pub async fn recover_unsent(&self) -> Result<Recovery, Block> {
        self.supervised(|gate| async move { gate.recover_unsent_inner().await })
            .await
    }
    async fn recover_unsent_inner(&self) -> Result<Recovery, Block> {
        for _ in 0..16 {
            let Some((raw, mut record)) = self.read().await? else {
                return Ok(Recovery::NotNeeded);
            };
            let now = self.now()?;
            Self::check_clock(&record, now)?;
            if record.exhausted {
                return Err(Block::Exhausted);
            }
            if !record.pending {
                return Ok(Recovery::NotNeeded);
            }
            if record.schema_version != 2 || record.phase != Some(Phase::Reserved) {
                return Err(Block::Unresolved);
            }
            // CAS invalidates every unconsumed old Reservation before admitting
            // another request. A racing authorization can win instead, in which
            // case its Authorized row is never recoverable as unsent.
            record.attempt_id = uuid::Uuid::new_v4().to_string();
            record.phase = Some(Phase::NotSent);
            record.pending = false;
            record.updated_ms = now;
            if self.compare_store(Some(&raw), &record).await? {
                return Ok(Recovery::RecoveredUnsent);
            }
        }
        Err(Block::Storage)
    }
    fn now(&self) -> Result<i64, Block> {
        let now = (self.clock)();
        if valid_ms(now) {
            Ok(now)
        } else {
            Err(Block::Clock)
        }
    }
    async fn read(&self) -> Result<Option<(String, Record)>, Block> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT substr(value,1,1025) FROM meta WHERE key=?")
                .bind(KEY)
                .fetch_optional(self.db.pool())
                .await
                .map_err(|_| Block::Storage)?;
        raw.map(|raw| {
            if raw.len() > MAX_STATE_BYTES || !raw.trim_start().starts_with('{') {
                return Err(Block::InvalidState);
            }
            let record: Record = serde_json::from_str(&raw).map_err(|_| Block::InvalidState)?;
            if !record.valid() {
                return Err(Block::InvalidState);
            }
            Ok((raw, record))
        })
        .transpose()
    }
    async fn compare_store(&self, old: Option<&str>, value: &Record) -> Result<bool, Block> {
        if !value.valid() {
            return Err(Block::InvalidState);
        }
        let raw = serde_json::to_string(value).map_err(|_| Block::InvalidState)?;
        if raw.len() > MAX_STATE_BYTES {
            return Err(Block::InvalidState);
        }
        let rows = if let Some(old) = old {
            sqlx::query("UPDATE meta SET value=?,updated_at=? WHERE key=? AND value=?")
                .bind(raw)
                .bind(chrono::Utc::now())
                .bind(KEY)
                .bind(old)
                .execute(self.db.pool())
                .await
        } else {
            sqlx::query("INSERT OR IGNORE INTO meta(key,value,updated_at) VALUES(?,?,?)")
                .bind(KEY)
                .bind(raw)
                .bind(chrono::Utc::now())
                .execute(self.db.pool())
                .await
        }
        .map_err(|_| Block::Storage)?
        .rows_affected();
        Ok(rows == 1)
    }
    fn check_clock(record: &Record, now: i64) -> Result<(), Block> {
        if record.updated_ms > now {
            Err(Block::Clock)
        } else {
            Ok(())
        }
    }
    /// Persist a conservative method-wide reservation. No query/digest is stored.
    /// Pending is not a lease: dropping the caller never proves no bytes were sent.
    pub async fn reserve(&self, deadline: Instant) -> Result<Reservation, Block> {
        self.supervised(move |gate| async move { gate.reserve_inner(deadline).await })
            .await
    }
    async fn reserve_inner(&self, deadline: Instant) -> Result<Reservation, Block> {
        for _ in 0..16 {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(Block::Deadline)?;
            if remaining.is_zero() || remaining > std::time::Duration::from_secs(10) {
                return Err(Block::Deadline);
            }
            let old = self.read().await?;
            let now = self.now()?;
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(Block::Deadline)?;
            let window = i64::try_from(remaining.as_millis()).map_err(|_| Block::Deadline)?;
            if !(1..=10_000).contains(&window) {
                return Err(Block::Deadline);
            }
            let valid_until_ms = now
                .checked_add(window)
                .filter(|v| valid_ms(*v))
                .ok_or(Block::Clock)?;
            if let Some((_, record)) = &old {
                Self::check_clock(record, now)?;
                if record.exhausted {
                    return Err(Block::Exhausted);
                }
                if record.pending {
                    return Err(Block::Unresolved);
                }
                let until = record
                    .local_not_before_ms
                    .max(record.server_not_before_ms.unwrap_or(0));
                if until > now {
                    return Err(Block::Wait(until));
                }
            }
            let id = uuid::Uuid::new_v4().to_string();
            let until = now
                .checked_add(self.interval_ms)
                .filter(|v| valid_ms(*v))
                .ok_or(Block::Clock)?;
            let mut record = old.as_ref().map_or_else(
                || Record {
                    schema_version: 2,
                    phase: Some(Phase::Reserved),
                    ledger_id: uuid::Uuid::new_v4().to_string(),
                    attempt_id: id.clone(),
                    created_ms: now,
                    updated_ms: now,
                    local_not_before_ms: until,
                    server_not_before_ms: None,
                    pending: true,
                    reported_remaining: None,
                    quota_observed_ms: None,
                    exhausted: false,
                },
                |(_, record)| record.clone(),
            );
            record.attempt_id = id.clone();
            record.created_ms = now;
            record.updated_ms = now;
            record.local_not_before_ms = until;
            record.pending = true;
            record.schema_version = 2;
            record.phase = Some(Phase::Reserved);
            if self
                .compare_store(old.as_ref().map(|(raw, _)| raw.as_str()), &record)
                .await?
            {
                // Commit acknowledgement may outlive the dispatch window. Keep
                // the durable reservation but never hand out an expired ticket.
                if Instant::now() >= deadline || self.now()? > valid_until_ms {
                    return Err(Block::Deadline);
                }
                return Ok(Reservation(Ticket {
                    owner: self.owner.clone(),
                    ledger_id: record.ledger_id,
                    id,
                    admitted_ms: now,
                    valid_until_ms,
                    deadline,
                }));
            }
        }
        Err(Block::Storage)
    }
    /// Consume ownership exactly once and recheck after the DB read. This is
    /// permission for a future sender, not an upstream-started or billing event.
    pub async fn authorize(&self, reservation: Reservation) -> Result<Attempt, Block> {
        self.supervised(move |gate| async move { gate.authorize_inner(reservation).await })
            .await
    }
    async fn authorize_inner(&self, reservation: Reservation) -> Result<Attempt, Block> {
        let ticket = reservation.0;
        if !Arc::ptr_eq(&self.owner, &ticket.owner) {
            return Err(Block::ForeignAttempt);
        }
        for _ in 0..16 {
            let (raw, mut record) = self.read().await?.ok_or(Block::ForeignAttempt)?;
            let now = self.now()?;
            Self::check_clock(&record, now)?;
            if record.ledger_id != ticket.ledger_id
                || record.attempt_id != ticket.id
                || !record.pending
            {
                return Err(Block::ForeignAttempt);
            }
            if record.schema_version != 2 || record.phase != Some(Phase::Reserved) {
                return Err(Block::Unresolved);
            }
            if now < ticket.admitted_ms {
                return Err(Block::Clock);
            }
            if now > ticket.valid_until_ms || Instant::now() >= ticket.deadline {
                return Err(Block::Deadline);
            }
            if record.exhausted {
                return Err(Block::Exhausted);
            }
            if let Some(until) = record.server_not_before_ms.filter(|v| *v > now) {
                return Err(Block::Wait(until));
            }
            // Persist the ambiguous dispatch boundary BEFORE releasing Attempt.
            // Cancellation or an expired post-commit window must not turn this
            // into a Reserved record or a known zero-send outcome.
            record.phase = Some(Phase::Authorized);
            record.updated_ms = now;
            if self.compare_store(Some(&raw), &record).await? {
                if Instant::now() >= ticket.deadline || self.now()? > ticket.valid_until_ms {
                    return Err(Block::Deadline);
                }
                return Ok(Attempt(ticket));
            }
        }
        Err(Block::Storage)
    }
    /// Persist a valid response-header wait before awaiting its body. This
    /// neither changes quota nor resolves/refunds the current attempt. Only the
    /// private sender consumes this non-cloning public Attempt capability.
    pub(super) async fn note_backoff(
        &self,
        attempt: &Attempt,
        until: i64,
        observed_ms: i64,
    ) -> Result<(), Block> {
        let ticket = attempt.0.clone();
        self.supervised(move |gate| async move {
            gate.note_backoff_inner(ticket, until, observed_ms).await
        })
        .await
    }
    async fn note_backoff_inner(
        &self,
        ticket: Ticket,
        until: i64,
        observed_ms: i64,
    ) -> Result<(), Block> {
        if !Arc::ptr_eq(&self.owner, &ticket.owner) {
            return Err(Block::ForeignAttempt);
        }
        for _ in 0..16 {
            let (raw, mut record) = self.read().await?.ok_or(Block::ForeignAttempt)?;
            let now = self.now()?;
            Self::check_clock(&record, now)?;
            if record.ledger_id != ticket.ledger_id {
                return Err(Block::ForeignAttempt);
            }
            if !valid_ms(until)
                || !valid_ms(observed_ms)
                || observed_ms < ticket.admitted_ms
                || observed_ms > now
                || until < observed_ms
            {
                return Err(Block::Clock);
            }
            record.server_not_before_ms = Some(
                record
                    .server_not_before_ms
                    .map_or(until, |old| old.max(until)),
            );
            record.updated_ms = now;
            if self.compare_store(Some(&raw), &record).await? {
                return Ok(());
            }
        }
        Err(Block::Storage)
    }
    /// A valid response can clear the unresolved barrier; missing/malformed
    /// controls cannot. Known backoff is retained even for unusable hits/errors.
    /// Only the current attempt can change quota. Stale valid backoff may
    /// tighten waiting, but stale quota (including zero) cannot poison a new attempt.
    pub async fn finish(
        &self,
        attempt: Attempt,
        controls: &Controls,
        observed_ms: i64,
    ) -> Result<Completion, Block> {
        let controls = controls.clone();
        self.supervised(move |gate| async move {
            gate.finish_inner(attempt, &controls, observed_ms).await
        })
        .await
    }
    async fn finish_inner(
        &self,
        attempt: Attempt,
        controls: &Controls,
        observed_ms: i64,
    ) -> Result<Completion, Block> {
        let ticket = attempt.0;
        // Instance ownership is checked before any DB access. Ledger UUIDs
        // alone can be copied with a backup and are not an authentication fence.
        if !Arc::ptr_eq(&self.owner, &ticket.owner) {
            return Err(Block::ForeignAttempt);
        }
        for _ in 0..16 {
            let (raw, mut record) = self.read().await?.ok_or(Block::ForeignAttempt)?;
            let now = self.now()?;
            Self::check_clock(&record, now)?;
            if record.ledger_id != ticket.ledger_id {
                return Err(Block::ForeignAttempt);
            }
            if !valid_ms(observed_ms) || observed_ms < ticket.admitted_ms || observed_ms > now {
                return Err(Block::Clock);
            }
            let current = record.schema_version == 2
                && record.phase == Some(Phase::Authorized)
                && record.pending
                && record.attempt_id == ticket.id;
            let mut malformed = controls.malformed;
            if let Some(until) = controls.backoff_until_ms {
                if valid_ms(until) {
                    record.server_not_before_ms = Some(
                        record
                            .server_not_before_ms
                            .map_or(until, |old| old.max(until)),
                    );
                } else {
                    malformed = true;
                }
            }
            let known = match controls.shared_quota {
                super::stack_exchange::SharedQuota::Unknown => false,
                super::stack_exchange::SharedQuota::Exhausted => {
                    if current {
                        record.exhausted = true;
                        record.reported_remaining = Some(0);
                        record.quota_observed_ms = Some(observed_ms);
                    }
                    true
                }
                super::stack_exchange::SharedQuota::ReportedRemaining(0) => {
                    malformed = true;
                    false
                }
                super::stack_exchange::SharedQuota::ReportedRemaining(n) => {
                    if current && !malformed && !record.exhausted {
                        record.reported_remaining = Some(n);
                        record.quota_observed_ms = Some(observed_ms);
                    }
                    true
                }
            };
            if current {
                // Pending blocks all earlier dispatch. Starting a fresh full
                // interval after observing the result also covers delayed
                // authorize/send and delayed reservation acknowledgements.
                let until = now
                    .checked_add(self.interval_ms)
                    .filter(|v| valid_ms(*v))
                    .ok_or(Block::Clock)?;
                record.local_not_before_ms = record.local_not_before_ms.max(until);
                record.pending = malformed || !known;
                record.phase = Some(if record.pending {
                    Phase::Authorized
                } else {
                    Phase::Observed
                });
            }
            record.updated_ms = now;
            if self.compare_store(Some(&raw), &record).await? {
                if !current {
                    return Ok(Completion::Stale);
                }
                if malformed || !known {
                    return Ok(Completion::ControlsUnresolved);
                }
                if Instant::now() >= ticket.deadline || self.now()? > ticket.valid_until_ms {
                    return Ok(Completion::Stale);
                }
                return Ok(Completion::Current);
            }
        }
        Err(Block::Storage)
    }
    /// Recreating Admission rejects old in-memory tickets, but does not stop old
    /// owners. Restore callers must quiesce external work and await their old
    /// owner's retirement; arbitrary rollback beneath a live owner is undetected.
    /// Read-only local facts; no claim about provider availability or an
    /// account balance. Ownership IDs never enter this projection.
    pub async fn view(&self) -> Result<View, Block> {
        let old = self.read().await?;
        let now = self.now()?;
        if let Some((_, r)) = old {
            Self::check_clock(&r, now)?;
            Ok(View {
                local_not_before_ms: Some(r.local_not_before_ms),
                server_not_before_ms: r.server_not_before_ms,
                shared_remaining: r.reported_remaining,
                quota_observed_ms: r.quota_observed_ms,
                needs_observation: r.pending,
                exhausted: r.exhausted,
                can_recover_unsent: r.schema_version == 2
                    && r.phase == Some(Phase::Reserved)
                    && r.pending
                    && !r.exhausted,
            })
        } else {
            Ok(View {
                local_not_before_ms: None,
                server_not_before_ms: None,
                shared_remaining: None,
                quota_observed_ms: None,
                needs_observation: false,
                exhausted: false,
                can_recover_unsent: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::Duration;

    fn with_clock(db: Db, now: Arc<AtomicI64>) -> Admission {
        Admission {
            owner: Arc::new(tokio::sync::RwLock::new(true)),
            db,
            interval_ms: METHOD_INTERVAL_MS,
            clock: Arc::new(move || now.load(Ordering::SeqCst)),
        }
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(9)
    }
    fn response(now: i64, remaining: Option<u64>, backoff: Option<u64>) -> Controls {
        let mut value = serde_json::json!({"items":[]});
        if let Some(n) = remaining {
            value["quota_remaining"] = n.into();
        }
        if let Some(n) = backoff {
            value["backoff"] = n.into();
        }
        super::super::stack_exchange::inspect(&value.to_string(), 1, now)
            .unwrap()
            .controls
    }
    fn duplicate_for_stale_callback(a: &Attempt) -> Attempt {
        // Only test code can construct/copy private ownership fields.
        Attempt(Ticket {
            owner: a.0.owner.clone(),
            ledger_id: a.0.ledger_id.clone(),
            id: a.0.id.clone(),
            admitted_ms: a.0.admitted_ms,
            valid_until_ms: a.0.valid_until_ms,
            deadline: a.0.deadline,
        })
    }
    async fn raw(db: &Db) -> String {
        sqlx::query_scalar("SELECT value FROM meta WHERE key=?")
            .bind(KEY)
            .fetch_one(db.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn reservation_and_observation_are_separate_and_have_no_query_or_account_payload() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let permit = gate.reserve(deadline()).await.unwrap();
        assert!(gate.view().await.unwrap().needs_observation);
        let attempt = gate.authorize(permit).await.unwrap();
        assert_eq!(
            gate.finish(attempt, &response(1_000, Some(5), None), 1_000)
                .await
                .unwrap(),
            Completion::Current
        );
        let state = gate.view().await.unwrap();
        assert_eq!(state.shared_remaining, Some(5));
        assert_eq!(state.quota_observed_ms, Some(1_000));
        assert!(!state.needs_observation);
        assert!(!state.exhausted);
        assert_eq!(
            gate.reserve(deadline()).await.unwrap_err(),
            Block::Wait(61_000)
        );
        let stored = raw(&db).await;
        for forbidden in [
            "query",
            "question",
            "token",
            "credential",
            "account",
            "owner",
            "error_message",
            "body",
        ] {
            assert!(
                !stored.contains(forbidden),
                "unexpected persisted payload class {forbidden}"
            );
        }
    }

    #[tokio::test]
    async fn dropped_reservation_and_recreated_gate_do_not_refund_or_expire_unknown_outcome() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        drop(gate.reserve(deadline()).await.unwrap());
        now.store(1_000 + 86_400_000, Ordering::SeqCst);
        let restarted = with_clock(db, now);
        assert_eq!(
            restarted.reserve(deadline()).await.unwrap_err(),
            Block::Unresolved
        );
        let state = restarted.view().await.unwrap();
        assert!(state.needs_observation);
        assert_eq!(state.shared_remaining, None);
        assert!(!state.exhausted);
    }

    #[tokio::test]
    async fn server_backoff_survives_api_failure_and_late_callback_cannot_shorten_or_raise_quota() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db, now.clone());
        let attempt = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let old = duplicate_for_stale_callback(&attempt);
        let parsed = super::super::stack_exchange::inspect(
            r#"{"backoff":300,"quota_remaining":3,"error_id":502,"error_message":"PRIVATE_ERROR"}"#,
            1,
            1_000,
        )
        .unwrap();
        assert!(parsed.results.is_err());
        assert_eq!(
            gate.finish(attempt, &parsed.controls, 1_000).await.unwrap(),
            Completion::Current
        );
        now.store(2_000, Ordering::SeqCst);
        assert_eq!(
            gate.finish(old, &response(2_000, Some(999), Some(1)), 2_000)
                .await
                .unwrap(),
            Completion::Stale
        );
        now.store(70_000, Ordering::SeqCst);
        let state = gate.view().await.unwrap();
        assert_eq!(state.server_not_before_ms, Some(301_000));
        assert_eq!(state.shared_remaining, Some(3));
        assert_eq!(
            gate.reserve(deadline()).await.unwrap_err(),
            Block::Wait(301_000)
        );
    }

    #[tokio::test]
    async fn zero_shared_quota_never_infers_a_reset_or_clears_on_missing_or_late_fields() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let a = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let old = duplicate_for_stale_callback(&a);
        assert_eq!(
            gate.finish(a, &response(1_000, Some(0), None), 1_000)
                .await
                .unwrap(),
            Completion::Current
        );
        now.store(86_401_000, Ordering::SeqCst);
        gate.finish(old, &response(86_401_000, None, None), 86_401_000)
            .await
            .unwrap();
        let restarted = with_clock(db, now);
        let state = restarted.view().await.unwrap();
        assert!(state.exhausted);
        assert_eq!(state.shared_remaining, Some(0));
        assert_eq!(state.quota_observed_ms, Some(1_000));
        assert_eq!(
            restarted.reserve(deadline()).await.unwrap_err(),
            Block::Exhausted
        );
    }

    #[tokio::test]
    async fn unknown_controls_retain_known_backoff_and_durable_unresolved_barrier() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let a = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        assert_eq!(
            gate.finish(a, &response(1_000, None, Some(120)), 1_000)
                .await
                .unwrap(),
            Completion::ControlsUnresolved
        );
        now.store(999_000, Ordering::SeqCst);
        let other = with_clock(db, now);
        let state = other.view().await.unwrap();
        assert!(state.needs_observation);
        assert_eq!(state.shared_remaining, None);
        assert_eq!(state.server_not_before_ms, Some(121_000));
        assert_eq!(
            other.reserve(deadline()).await.unwrap_err(),
            Block::Unresolved
        );
    }

    #[tokio::test]
    async fn two_owned_file_pools_have_one_reservation_and_reopen_keeps_the_observation() {
        let base = std::env::temp_dir();
        let dir = base.join(format!(
            "llmgw-stack-admission-owned-{}",
            uuid::Uuid::new_v4()
        ));
        let path = dir.join("owned.sqlite");
        let db1 = Db::connect_path(&path).await.unwrap();
        let db2 = Db::connect_path(&path).await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let a = with_clock(db1.clone(), now.clone());
        let b = with_clock(db2.clone(), now.clone());
        let (r1, r2) = tokio::join!(a.reserve(deadline()), b.reserve(deadline()));
        assert_eq!(usize::from(r1.is_ok()) + usize::from(r2.is_ok()), 1);
        let (winner, permit) = match (r1, r2) {
            (Ok(r), Err(Block::Unresolved | Block::Wait(_))) => (&a, r),
            (Err(Block::Unresolved | Block::Wait(_)), Ok(r)) => (&b, r),
            other => panic!("unexpected CAS result {other:?}"),
        };
        let attempt = winner.authorize(permit).await.unwrap();
        winner
            .finish(attempt, &response(1_000, Some(3), None), 1_000)
            .await
            .unwrap();
        db1.pool().close().await;
        db2.pool().close().await;
        let db3 = Db::connect_path(&path).await.unwrap();
        now.store(70_000, Ordering::SeqCst);
        let reopened = with_clock(db3.clone(), now.clone());
        assert_eq!(reopened.view().await.unwrap().shared_remaining, Some(3));
        drop(reopened.reserve(deadline()).await.unwrap());
        db3.pool().close().await;
        let db4 = Db::connect_path(&path).await.unwrap();
        let resumed = with_clock(db4.clone(), now);
        assert!(resumed.view().await.unwrap().can_recover_unsent);
        let original: serde_json::Value = serde_json::from_str(&raw(&db4).await).unwrap();
        assert_eq!(
            resumed.recover_unsent().await.unwrap(),
            Recovery::RecoveredUnsent
        );
        let recovered: serde_json::Value = serde_json::from_str(&raw(&db4).await).unwrap();
        assert_ne!(original["attempt_id"], recovered["attempt_id"]);
        assert_eq!(recovered["phase"], "not_sent");
        assert_eq!(resumed.view().await.unwrap().shared_remaining, Some(3));
        assert_eq!(
            recovered["local_not_before_ms"],
            original["local_not_before_ms"]
        );
        db4.pool().close().await;
        assert_eq!(dir.parent(), Some(base.as_path()));
        assert!(dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("llmgw-stack-admission-owned-"));
        // Preserve the owned fixture until this test process has exited.
        // A close acknowledgement is not proof that Windows permits immediate
        // unlink; external cleanup uses this receipt and never retries blindly.
        if let Some(receipt) = std::env::var_os("LLMGW_ADMISSION_FIXTURE_RECEIPT") {
            let evidence = serde_json::json!({"directory":dir,"business_assertions_passed":true,"pool_close_acknowledged":true,"cleanup":"separate after test process exit; no OS-handle proof inferred"});
            std::fs::write(receipt, serde_json::to_vec(&evidence).unwrap()).unwrap();
        } else {
            eprintln!("owned admission DB fixture retained: {}", dir.display());
        }
    }

    #[tokio::test]
    async fn failed_completion_write_leaves_other_gate_blocked_instead_of_losing_server_wait() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let attempt = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER owned_stack_write_failure BEFORE UPDATE ON meta WHEN OLD.key = 'search.stack_exchange.admission.v1' BEGIN SELECT RAISE(FAIL, 'owned failure'); END").execute(db.pool()).await.unwrap();
        assert_eq!(
            gate.finish(attempt, &response(1_000, Some(5), Some(120)), 1_000)
                .await
                .unwrap_err(),
            Block::Storage
        );
        now.store(500_000, Ordering::SeqCst);
        assert_eq!(
            with_clock(db, now).reserve(deadline()).await.unwrap_err(),
            Block::Unresolved
        );
    }

    #[tokio::test]
    async fn foreign_capability_clock_and_corrupt_record_fail_closed() {
        let db1 = Db::connect_in_memory().await.unwrap();
        let db2 = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let a = with_clock(db1, now.clone());
        let b = with_clock(db2.clone(), now.clone());
        let foreign = a
            .authorize(a.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let own = b.reserve(deadline()).await.unwrap();
        let before = raw(&db2).await;
        assert_eq!(
            b.finish(foreign, &response(1_000, Some(1), None), 1_000)
                .await
                .unwrap_err(),
            Block::ForeignAttempt
        );
        assert_eq!(raw(&db2).await, before);
        now.store(500, Ordering::SeqCst);
        assert_eq!(b.authorize(own).await.unwrap_err(), Block::Clock);
        now.store(1_000, Ordering::SeqCst);
        for bad in ["{}".to_owned(), "x".repeat(MAX_STATE_BYTES + 1)] {
            sqlx::query("UPDATE meta SET value=? WHERE key=?")
                .bind(bad)
                .bind(KEY)
                .execute(db2.pool())
                .await
                .unwrap();
            assert_eq!(
                b.reserve(deadline()).await.unwrap_err(),
                Block::InvalidState
            );
        }
        assert_eq!(
            b.reserve(Instant::now() - Duration::from_millis(1))
                .await
                .unwrap_err(),
            Block::Deadline
        );
    }
    #[tokio::test]
    async fn lifecycle_delayed_authorization_keeps_a_full_post_response_interval() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db, now.clone());
        let r = gate.reserve(deadline()).await.unwrap();
        now.store(8_000, Ordering::SeqCst);
        let a = gate.authorize(r).await.unwrap();
        now.store(9_000, Ordering::SeqCst);
        gate.finish(a, &response(9_000, Some(5), None), 9_000)
            .await
            .unwrap();
        now.store(68_000, Ordering::SeqCst);
        assert_eq!(
            gate.reserve(deadline()).await.unwrap_err(),
            Block::Wait(69_000)
        );
    }
    #[tokio::test]
    async fn lifecycle_old_callback_cannot_release_or_raise_a_new_pending_attempt() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let a = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let old = duplicate_for_stale_callback(&a);
        gate.finish(a, &response(1_000, Some(5), None), 1_000)
            .await
            .unwrap();
        now.store(70_000, Ordering::SeqCst);
        let fresh = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let before: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        now.store(72_000, Ordering::SeqCst);
        assert_eq!(
            gate.finish(old, &response(72_000, Some(999), Some(1)), 72_000)
                .await
                .unwrap(),
            Completion::Stale
        );
        let after: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        assert_eq!(before["attempt_id"], after["attempt_id"]);
        assert_eq!(after["pending"], true);
        let view = gate.view().await.unwrap();
        assert!(view.needs_observation);
        assert_eq!(view.shared_remaining, Some(5));
        assert_eq!(view.quota_observed_ms, Some(1_000));
        now.store(73_000, Ordering::SeqCst);
        assert_eq!(
            gate.finish(fresh, &response(73_000, Some(4), None), 73_000)
                .await
                .unwrap(),
            Completion::Current
        );
        assert_eq!(gate.view().await.unwrap().shared_remaining, Some(4));
    }
    #[tokio::test]
    async fn record_boundary_nonpending_unknown_is_not_an_initial_empty_ledger() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        drop(gate.reserve(deadline()).await.unwrap());
        let mut value: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        value["pending"] = false.into();
        value["reported_remaining"] = serde_json::Value::Null;
        value["quota_observed_ms"] = serde_json::Value::Null;
        value["exhausted"] = false.into();
        sqlx::query("UPDATE meta SET value=? WHERE key=?")
            .bind(value.to_string())
            .bind(KEY)
            .execute(db.pool())
            .await
            .unwrap();
        now.store(70_000, Ordering::SeqCst);
        assert_eq!(
            gate.reserve(deadline()).await.unwrap_err(),
            Block::InvalidState
        );
    }
    #[tokio::test]
    async fn record_boundary_positional_array_cannot_supply_valid_completed_state() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let a = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        gate.finish(a, &response(1_000, Some(5), None), 1_000)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        let arr = serde_json::json!([
            v["schema_version"],
            v["ledger_id"],
            v["attempt_id"],
            v["created_ms"],
            v["updated_ms"],
            v["local_not_before_ms"],
            v["server_not_before_ms"],
            v["pending"],
            v["reported_remaining"],
            v["quota_observed_ms"],
            v["exhausted"]
        ]);
        sqlx::query("UPDATE meta SET value=? WHERE key=?")
            .bind(arr.to_string())
            .bind(KEY)
            .execute(db.pool())
            .await
            .unwrap();
        now.store(70_000, Ordering::SeqCst);
        assert_eq!(
            gate.reserve(deadline()).await.unwrap_err(),
            Block::InvalidState
        );
    }
    #[tokio::test]
    async fn record_boundary_old_zero_quota_cannot_poison_new_pending_or_current_facts() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db, now.clone());
        let a = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let old = duplicate_for_stale_callback(&a);
        gate.finish(a, &response(1_000, Some(5), None), 1_000)
            .await
            .unwrap();
        now.store(70_000, Ordering::SeqCst);
        let fresh = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        now.store(72_000, Ordering::SeqCst);
        assert_eq!(
            gate.finish(old, &response(72_000, Some(0), None), 72_000)
                .await
                .unwrap(),
            Completion::Stale
        );
        let view = gate.view().await.unwrap();
        assert!(view.needs_observation);
        assert!(
            !view.exhausted,
            "old quota0 cannot permanently poison a newer attempt"
        );
        assert_eq!(view.shared_remaining, Some(5));
        now.store(73_000, Ordering::SeqCst);
        gate.finish(fresh, &response(73_000, Some(4), None), 73_000)
            .await
            .unwrap();
        assert_eq!(gate.view().await.unwrap().shared_remaining, Some(4));
    }
    #[tokio::test]
    async fn owner_boundary_copied_row_cannot_authorize_another_instance_reservation() {
        let da = Db::connect_in_memory().await.unwrap();
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let a = with_clock(da.clone(), now.clone());
        let b = with_clock(db.clone(), now);
        let r = a.reserve(deadline()).await.unwrap();
        let copied = raw(&da).await;
        sqlx::query("INSERT INTO meta(key,value,updated_at) VALUES(?,?,?)")
            .bind(KEY)
            .bind(&copied)
            .bind(chrono::Utc::now())
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(b.authorize(r).await.unwrap_err(), Block::ForeignAttempt);
        assert_eq!(raw(&db).await, copied);
    }
    #[tokio::test]
    async fn owner_boundary_copied_row_cannot_accept_another_instance_completion() {
        let da = Db::connect_in_memory().await.unwrap();
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let a = with_clock(da.clone(), now.clone());
        let b = with_clock(db.clone(), now);
        let attempt = a
            .authorize(a.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let copied = raw(&da).await;
        sqlx::query("INSERT INTO meta(key,value,updated_at) VALUES(?,?,?)")
            .bind(KEY)
            .bind(&copied)
            .bind(chrono::Utc::now())
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(
            b.finish(attempt, &response(1_000, Some(3), None), 1_000)
                .await
                .unwrap_err(),
            Block::ForeignAttempt
        );
        assert_eq!(raw(&db).await, copied);
    }
    #[tokio::test]
    async fn owner_boundary_cloned_admission_preserves_the_same_lifecycle_owner() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let a = with_clock(db, now);
        let r = a.reserve(deadline()).await.unwrap();
        let clone = a.clone();
        let attempt = clone.authorize(r).await.unwrap();
        assert_eq!(
            a.finish(attempt, &response(1_000, Some(3), None), 1_000)
                .await
                .unwrap(),
            Completion::Current
        );
        assert_eq!(clone.view().await.unwrap().shared_remaining, Some(3));
    }
    #[tokio::test]
    async fn retirement_fresh_owner_closes_all_clones_without_creating_or_deleting_ledger() {
        let db = Db::connect_in_memory().await.unwrap();
        let gate = with_clock(db.clone(), Arc::new(AtomicI64::new(1_000)));
        let clone = gate.clone();
        gate.retire().await;
        clone.retire().await;
        assert_eq!(clone.reserve(deadline()).await.unwrap_err(), Block::Retired);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM meta WHERE key=?")
            .bind(KEY)
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0);
        assert!(!gate.view().await.unwrap().needs_observation);
    }
    #[tokio::test]
    async fn retirement_authorize_rejects_old_reservation_before_closed_db_access() {
        let db = Db::connect_in_memory().await.unwrap();
        let gate = with_clock(db.clone(), Arc::new(AtomicI64::new(1_000)));
        let reserved = gate.reserve(deadline()).await.unwrap();
        let clone = gate.clone();
        gate.retire().await;
        db.pool().close().await;
        assert_eq!(clone.authorize(reserved).await.unwrap_err(), Block::Retired);
    }
    #[tokio::test]
    async fn retirement_late_completion_cannot_modify_synthetically_restored_pending_record() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let attempt = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let late = duplicate_for_stale_callback(&attempt);
        let pending = raw(&db).await;
        gate.finish(attempt, &response(1_000, Some(5), None), 1_000)
            .await
            .unwrap();
        gate.retire().await;
        // Owned synthetic restore only. Restore callers must quiesce external senders first.
        sqlx::query("UPDATE meta SET value=? WHERE key=?")
            .bind(&pending)
            .bind(KEY)
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(
            gate.finish(late, &response(1_000, Some(0), Some(10)), 1_000)
                .await
                .unwrap_err(),
            Block::Retired
        );
        assert_eq!(raw(&db).await, pending);
        let new = with_clock(db, now);
        assert_eq!(
            new.reserve(deadline()).await.unwrap_err(),
            Block::Unresolved
        );
        assert!(new.view().await.unwrap().needs_observation);
    }
    #[tokio::test]
    async fn retirement_new_owner_can_use_existing_observed_state_without_refunding_zero() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let attempt = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        gate.finish(attempt, &response(1_000, Some(5), Some(120)), 1_000)
            .await
            .unwrap();
        let before = raw(&db).await;
        gate.retire().await;
        assert_eq!(raw(&db).await, before);
        let new = with_clock(db, now.clone());
        assert_eq!(
            new.reserve(deadline()).await.unwrap_err(),
            Block::Wait(121_000)
        );
        now.store(130_000, Ordering::SeqCst);
        let a = new
            .authorize(new.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        new.finish(a, &response(130_000, Some(0), None), 130_000)
            .await
            .unwrap();
        new.retire().await;
        now.store(1_000_000, Ordering::SeqCst);
        assert_eq!(
            with_clock(new.db.clone(), now)
                .reserve(deadline())
                .await
                .unwrap_err(),
            Block::Exhausted
        );
        assert_eq!(gate.reserve(deadline()).await.unwrap_err(), Block::Retired);
    }
    #[tokio::test]
    async fn retirement_waits_for_in_flight_reservation_before_returning_closed() {
        let db = Db::connect_in_memory().await.unwrap();
        let gate = with_clock(db.clone(), Arc::new(AtomicI64::new(1_000)));
        let connection = db.pool().acquire().await.unwrap();
        let mut reserve = Box::pin(gate.reserve(deadline()));
        assert!(matches!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(reserve.as_mut().poll(cx))).await,
            std::task::Poll::Pending
        ));
        let mut retire = Box::pin(gate.retire());
        assert!(
            matches!(
                std::future::poll_fn(|cx| std::task::Poll::Ready(retire.as_mut().poll(cx))).await,
                std::task::Poll::Pending
            ),
            "must await active DB operation, not merely flip a flag"
        );
        drop(connection);
        let ticket = reserve.await.unwrap();
        retire.await;
        assert_eq!(gate.authorize(ticket).await.unwrap_err(), Block::Retired);
        assert!(
            gate.view().await.unwrap().needs_observation,
            "retire never refunds unresolved reservations"
        );
    }
    #[tokio::test]
    async fn retirement_waits_for_in_flight_completion_then_blocks_late_mutation() {
        let db = Db::connect_in_memory().await.unwrap();
        let gate = with_clock(db.clone(), Arc::new(AtomicI64::new(1_000)));
        let a = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let late = duplicate_for_stale_callback(&a);
        let controls = response(1_000, Some(4), None);
        let connection = db.pool().acquire().await.unwrap();
        let mut finish = Box::pin(gate.finish(a, &controls, 1_000));
        assert!(matches!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(finish.as_mut().poll(cx))).await,
            std::task::Poll::Pending
        ));
        let mut retire = Box::pin(gate.retire());
        assert!(matches!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(retire.as_mut().poll(cx))).await,
            std::task::Poll::Pending
        ));
        drop(connection);
        assert_eq!(finish.await.unwrap(), Completion::Current);
        retire.await;
        let before = raw(&db).await;
        assert_eq!(
            gate.finish(late, &response(1_000, Some(0), None), 1_000)
                .await
                .unwrap_err(),
            Block::Retired
        );
        assert_eq!(raw(&db).await, before);
        assert_eq!(gate.view().await.unwrap().shared_remaining, Some(4));
    }

    #[tokio::test]
    async fn retirement_caller_abort_after_sqlite_worker_entry_still_waits_for_write_ack() {
        struct ReleaseWorker(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
        impl Drop for ReleaseWorker {
            fn drop(&mut self) {
                let (lock, cv) = &*self.0;
                *lock.lock().unwrap() = true;
                cv.notify_all();
            }
        }
        let db = Db::connect_in_memory().await.unwrap();
        let gate = with_clock(db.clone(), Arc::new(AtomicI64::new(1_000)));
        let latch = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let release = ReleaseWorker(latch.clone());
        let timed_out = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let timeout_flag = timed_out.clone();
        let (entered, seen) = tokio::sync::oneshot::channel();
        let mut entered = Some(entered);
        let mut connection = db.pool().acquire().await.unwrap();
        {
            let mut handle = connection.lock_handle().await.unwrap();
            // Real owned SQLite worker callback, not an admission mock. Runs
            // inside INSERT before its acknowledgment reaches the caller.
            handle.set_update_hook(move |_| {
                if let Some(tx) = entered.take() {
                    let _ = tx.send(());
                    let (lock, cv) = &*latch;
                    let result = cv
                        .wait_timeout_while(
                            lock.lock().unwrap(),
                            Duration::from_secs(10),
                            |released| !*released,
                        )
                        .unwrap();
                    timeout_flag.store(result.1.timed_out(), Ordering::SeqCst);
                }
            });
        }
        drop(connection);
        let caller_gate = gate.clone();
        let caller = tokio::spawn(async move { caller_gate.reserve(deadline()).await });
        tokio::time::timeout(Duration::from_secs(3), seen)
            .await
            .unwrap()
            .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let mut retire = Box::pin(gate.retire());
        let pending = matches!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(retire.as_mut().poll(cx))).await,
            std::task::Poll::Pending
        );
        // Release before asserting, including red paths; never strand a worker.
        drop(release);
        assert!(
            pending,
            "cancelled caller is not proof the submitted SQLite write ended"
        );
        tokio::time::timeout(Duration::from_secs(3), retire)
            .await
            .unwrap();
        assert!(
            !timed_out.load(Ordering::SeqCst),
            "barrier release, not timeout, must complete the worker"
        );
        assert!(
            gate.view().await.unwrap().needs_observation,
            "a committed but unreturned reservation remains unresolved"
        );
        assert_eq!(gate.reserve(deadline()).await.unwrap_err(), Block::Retired);
    }
    #[tokio::test]
    async fn unsent_recovery_persists_phase_before_giving_dispatch_capability() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let r = gate.reserve(deadline()).await.unwrap();
        let reserved: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        assert_eq!(reserved["schema_version"], 2);
        assert_eq!(reserved["phase"], "reserved");
        assert!(gate.view().await.unwrap().can_recover_unsent);
        let _a = gate.authorize(r).await.unwrap();
        let auth: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        assert_eq!(auth["phase"], "authorized");
        assert_eq!(auth["pending"], true);
        assert!(!gate.view().await.unwrap().can_recover_unsent);
        let before = raw(&db).await;
        now.store(86_401_000, Ordering::SeqCst);
        assert_eq!(gate.recover_unsent().await.unwrap_err(), Block::Unresolved);
        assert_eq!(raw(&db).await, before);
    }
    #[tokio::test]
    async fn unsent_recovery_revokes_old_reservation_without_refund_or_wait_reset() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let old = gate.reserve(deadline()).await.unwrap();
        let before: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        assert_eq!(
            gate.recover_unsent().await.unwrap(),
            Recovery::RecoveredUnsent
        );
        let after: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        assert_eq!(after["phase"], "not_sent");
        assert_eq!(after["pending"], false);
        assert_ne!(before["attempt_id"], after["attempt_id"]);
        for field in [
            "ledger_id",
            "created_ms",
            "local_not_before_ms",
            "server_not_before_ms",
            "reported_remaining",
            "quota_observed_ms",
        ] {
            assert_eq!(before[field], after[field], "must retain {field}");
        }
        assert_eq!(
            gate.authorize(old).await.unwrap_err(),
            Block::ForeignAttempt
        );
        assert_eq!(
            gate.reserve(deadline()).await.unwrap_err(),
            Block::Wait(61_000)
        );
        let once = raw(&db).await;
        assert_eq!(gate.recover_unsent().await.unwrap(), Recovery::NotNeeded);
        assert_eq!(raw(&db).await, once);
        now.store(70_000, Ordering::SeqCst);
        let _fresh = gate.reserve(deadline()).await.unwrap();
        assert!(gate.view().await.unwrap().can_recover_unsent);
    }
    #[tokio::test]
    async fn unsent_recovery_preserves_previous_quota_and_monotonic_server_wait() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let a = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        let old = duplicate_for_stale_callback(&a);
        gate.finish(a, &response(1_000, Some(5), None), 1_000)
            .await
            .unwrap();
        now.store(70_000, Ordering::SeqCst);
        drop(gate.reserve(deadline()).await.unwrap());
        gate.note_backoff(&old, 200_000, 70_000).await.unwrap();
        let before = gate.view().await.unwrap();
        assert_eq!(
            gate.recover_unsent().await.unwrap(),
            Recovery::RecoveredUnsent
        );
        let after = gate.view().await.unwrap();
        assert_eq!(after.shared_remaining, before.shared_remaining);
        assert_eq!(after.shared_remaining, Some(5));
        assert_eq!(after.quota_observed_ms, Some(1_000));
        assert_eq!(after.server_not_before_ms, Some(200_000));
        assert_eq!(after.local_not_before_ms, before.local_not_before_ms);
        assert!(!after.needs_observation);
        assert!(!after.can_recover_unsent);
        assert_eq!(
            gate.reserve(deadline()).await.unwrap_err(),
            Block::Wait(200_000)
        );
    }
    #[tokio::test]
    async fn unsent_recovery_legacy_pending_and_exhausted_never_gain_authority_from_time() {
        for exhausted in [false, true] {
            let db = Db::connect_in_memory().await.unwrap();
            let now = Arc::new(AtomicI64::new(1_000));
            let gate = with_clock(db.clone(), now.clone());
            drop(gate.reserve(deadline()).await.unwrap());
            let mut value: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
            value["schema_version"] = 1.into();
            value.as_object_mut().unwrap().remove("phase");
            if exhausted {
                value["pending"] = false.into();
                value["reported_remaining"] = 0.into();
                value["quota_observed_ms"] = 1_000.into();
                value["exhausted"] = true.into();
            }
            sqlx::query("UPDATE meta SET value=? WHERE key=?")
                .bind(value.to_string())
                .bind(KEY)
                .execute(db.pool())
                .await
                .unwrap();
            let before = raw(&db).await;
            now.store(86_401_000, Ordering::SeqCst);
            assert!(!gate.view().await.unwrap().can_recover_unsent);
            assert_eq!(
                gate.recover_unsent().await.unwrap_err(),
                if exhausted {
                    Block::Exhausted
                } else {
                    Block::Unresolved
                }
            );
            assert_eq!(raw(&db).await, before);
        }
    }
    #[tokio::test]
    async fn unsent_recovery_inconsistent_or_future_phase_never_repairs_corrupt_state() {
        let db = Db::connect_in_memory().await.unwrap();
        let gate = with_clock(db.clone(), Arc::new(AtomicI64::new(1_000)));
        drop(gate.reserve(deadline()).await.unwrap());
        let base: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        for (version, phase, pending) in [
            (2, serde_json::Value::Null, true),
            (1, "reserved".into(), true),
            (3, "reserved".into(), true),
            (2, "not_sent".into(), true),
            (2, "reserved".into(), false),
            (2, "observed".into(), false),
            (2, "authorized".into(), false),
            (2, "guessed".into(), true),
        ] {
            let mut value = base.clone();
            value["schema_version"] = version.into();
            value["phase"] = phase;
            value["pending"] = pending.into();
            sqlx::query("UPDATE meta SET value=? WHERE key=?")
                .bind(value.to_string())
                .bind(KEY)
                .execute(db.pool())
                .await
                .unwrap();
            let before = raw(&db).await;
            assert_eq!(
                gate.recover_unsent().await.unwrap_err(),
                Block::InvalidState
            );
            assert_eq!(raw(&db).await, before);
        }
    }
    #[tokio::test]
    async fn unsent_recovery_storage_failure_cannot_issue_attempt_or_claim_recovered() {
        let db = Db::connect_in_memory().await.unwrap();
        let gate = with_clock(db.clone(), Arc::new(AtomicI64::new(1_000)));
        let r = gate.reserve(deadline()).await.unwrap();
        let before = raw(&db).await;
        sqlx::query("CREATE TRIGGER owned_unsent_failure BEFORE UPDATE ON meta WHEN OLD.key='search.stack_exchange.admission.v1' BEGIN SELECT RAISE(FAIL,'owned failure'); END").execute(db.pool()).await.unwrap();
        assert_eq!(gate.authorize(r).await.unwrap_err(), Block::Storage);
        assert_eq!(raw(&db).await, before);
        assert_eq!(gate.recover_unsent().await.unwrap_err(), Block::Storage);
        assert_eq!(raw(&db).await, before);
        sqlx::query("DROP TRIGGER owned_unsent_failure")
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(
            gate.recover_unsent().await.unwrap(),
            Recovery::RecoveredUnsent
        );
    }
    #[tokio::test]
    async fn unsent_recovery_and_authorization_have_only_one_cas_winner() {
        let db = Db::connect_in_memory().await.unwrap();
        let gate = with_clock(db.clone(), Arc::new(AtomicI64::new(1_000)));
        let r = gate.reserve(deadline()).await.unwrap();
        let (auth, recover) = tokio::join!(gate.authorize(r), gate.recover_unsent());
        match (auth, recover) {
            (Ok(a), Err(Block::Unresolved)) => {
                assert_eq!(gate.recover_unsent().await.unwrap_err(), Block::Unresolved);
                gate.finish(a, &response(1_000, Some(3), None), 1_000)
                    .await
                    .unwrap();
                assert_eq!(gate.view().await.unwrap().shared_remaining, Some(3));
            }
            (Err(Block::ForeignAttempt), Ok(Recovery::RecoveredUnsent)) => {
                assert!(!gate.view().await.unwrap().needs_observation);
                assert_eq!(gate.view().await.unwrap().shared_remaining, None);
            }
            other => panic!("invalid authority/recovery race outcome {other:?}"),
        }
    }
    #[tokio::test]
    async fn unsent_recovery_cancelled_authorize_after_sqlite_update_remains_unknown() {
        struct Release(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
        impl Drop for Release {
            fn drop(&mut self) {
                let (m, cv) = &*self.0;
                *m.lock().unwrap() = true;
                cv.notify_all();
            }
        }
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let r = gate.reserve(deadline()).await.unwrap();
        let latch = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let release = Release(latch.clone());
        let timed_out = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = timed_out.clone();
        let (entered, seen) = tokio::sync::oneshot::channel();
        let mut entered = Some(entered);
        let mut conn = db.pool().acquire().await.unwrap();
        {
            let mut handle = conn.lock_handle().await.unwrap();
            handle.set_update_hook(move |_| {
                if let Some(tx) = entered.take() {
                    let _ = tx.send(());
                    let (m, cv) = &*latch;
                    let out = cv
                        .wait_timeout_while(
                            m.lock().unwrap(),
                            Duration::from_secs(10),
                            |released| !*released,
                        )
                        .unwrap();
                    flag.store(out.1.timed_out(), Ordering::SeqCst);
                }
            });
        }
        drop(conn);
        let caller_gate = gate.clone();
        let caller = tokio::spawn(async move { caller_gate.authorize(r).await });
        tokio::time::timeout(Duration::from_secs(3), seen)
            .await
            .unwrap()
            .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let mut retire = Box::pin(gate.retire());
        let waiting = matches!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(retire.as_mut().poll(cx))).await,
            std::task::Poll::Pending
        );
        drop(release);
        assert!(waiting);
        tokio::time::timeout(Duration::from_secs(3), retire)
            .await
            .unwrap();
        assert!(!timed_out.load(Ordering::SeqCst));
        let restarted = with_clock(db.clone(), now);
        let before = raw(&db).await;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&before).unwrap()["phase"],
            "authorized"
        );
        assert_eq!(
            restarted.recover_unsent().await.unwrap_err(),
            Block::Unresolved
        );
        assert_eq!(raw(&db).await, before);
    }
    #[tokio::test]
    async fn unsent_recovery_noop_and_legacy_observed_upgrade_preserve_history_and_retirement() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        assert_eq!(gate.recover_unsent().await.unwrap(), Recovery::NotNeeded);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM meta WHERE key=?")
            .bind(KEY)
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0);
        let a = gate
            .authorize(gate.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        gate.finish(a, &response(1_000, Some(5), None), 1_000)
            .await
            .unwrap();
        let mut legacy: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        legacy["schema_version"] = 1.into();
        legacy.as_object_mut().unwrap().remove("phase");
        sqlx::query("UPDATE meta SET value=? WHERE key=?")
            .bind(legacy.to_string())
            .bind(KEY)
            .execute(db.pool())
            .await
            .unwrap();
        let before = raw(&db).await;
        assert_eq!(gate.recover_unsent().await.unwrap(), Recovery::NotNeeded);
        assert_eq!(raw(&db).await, before);
        now.store(70_000, Ordering::SeqCst);
        drop(gate.reserve(deadline()).await.unwrap());
        let upgraded: serde_json::Value = serde_json::from_str(&raw(&db).await).unwrap();
        assert_eq!(upgraded["schema_version"], 2);
        assert_eq!(upgraded["phase"], "reserved");
        assert_eq!(upgraded["reported_remaining"], 5);
        gate.retire().await;
        assert_eq!(gate.recover_unsent().await.unwrap_err(), Block::Retired);
    }

    #[tokio::test]
    async fn unsent_recovery_authorization_commit_after_dispatch_window_expiry_cannot_refund() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let gate = with_clock(db.clone(), now.clone());
        let r = gate.reserve(deadline()).await.unwrap();
        let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let flag = entered.clone();
        let clock = now.clone();
        let mut conn = db.pool().acquire().await.unwrap();
        {
            let mut handle = conn.lock_handle().await.unwrap();
            handle.set_update_hook(move |_| {
                flag.fetch_add(1, Ordering::SeqCst);
                clock.store(20_000, Ordering::SeqCst);
            });
        }
        drop(conn);
        assert_eq!(gate.authorize(r).await.unwrap_err(), Block::Deadline);
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        let before = raw(&db).await;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&before).unwrap()["phase"],
            "authorized"
        );
        assert_eq!(gate.recover_unsent().await.unwrap_err(), Block::Unresolved);
        assert_eq!(raw(&db).await, before);
        assert_eq!(entered.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn stack_scope_user_interval_is_applied_to_reservation_and_post_response_wait() {
        let db = Db::connect_in_memory().await.unwrap();
        let now = Arc::new(AtomicI64::new(1_000));
        let mut gate = Admission::with_min_interval(db, 180_000).unwrap();
        let clock = now.clone();
        gate.clock = Arc::new(move || clock.load(Ordering::SeqCst));
        let r = gate.reserve(deadline()).await.unwrap();
        assert_eq!(
            gate.view().await.unwrap().local_not_before_ms,
            Some(181_000)
        );
        let a = gate.authorize(r).await.unwrap();
        now.store(2_000, Ordering::SeqCst);
        gate.finish(a, &response(2_000, Some(5), None), 2_000)
            .await
            .unwrap();
        assert_eq!(
            gate.view().await.unwrap().local_not_before_ms,
            Some(182_000)
        );
    }
    #[tokio::test]
    async fn stack_scope_smaller_later_policy_cannot_shorten_saved_wait_and_invalid_policy_rejects()
    {
        let db = Db::connect_in_memory().await.unwrap();
        assert!(matches!(
            Admission::with_min_interval(db.clone(), 3_600_001),
            Err(Block::InvalidPolicy)
        ));
        let now = Arc::new(AtomicI64::new(1_000));
        let mut long = Admission::with_min_interval(db.clone(), 3_600_000).unwrap();
        let clock = now.clone();
        long.clock = Arc::new(move || clock.load(Ordering::SeqCst));
        let a = long
            .authorize(long.reserve(deadline()).await.unwrap())
            .await
            .unwrap();
        long.finish(a, &response(1_000, Some(5), None), 1_000)
            .await
            .unwrap();
        now.store(70_000, Ordering::SeqCst);
        let short = with_clock(db, now);
        assert_eq!(
            short.reserve(deadline()).await.unwrap_err(),
            Block::Wait(3_601_000)
        );
    }
}
