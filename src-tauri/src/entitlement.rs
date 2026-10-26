//! Conservative local accounting for a future verified hard-stop adapter.
//!
//! No IPC/config/DB row can construct a VerifiedEntitlement. Current real
//! adapters do not issue one: account free routing remains unsupported. Tests
//! issue explicitly artificial proofs to exercise the durable engine only.
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqliteRow, Row, SqliteConnection, SqlitePool};

const MAX_UNITS: i64 = i64::MAX / 4;
type Result<T> = std::result::Result<T, Rejection>;

fn fingerprint(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

/// An adapter-issued window ceiling, never a floating-point remaining balance.
/// Fields intentionally private and not Deserialize. A real adapter must bind
/// current subject, credential epoch, exact model/use, unit, and an enforceable
/// no-paid-overflow contract before an issuance path may be added.
#[derive(Clone, Debug)]
pub struct VerifiedEntitlement {
    platform: String,
    slot: String,
    principal: String,
    credential_epoch: i64,
    credential_fingerprint: String,
    revision: i64,
    snapshot_fingerprint: String,
    pool: String,
    window: String,
    unit: String,
    capacity: i64,
    model: String,
    purpose: String,
    request_bound: i64,
    observed_at: i64,
    expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Unverified,
    InvalidProof,
    StaleBinding,
    Expired,
    ClockAnomaly,
    Exhausted,
    ConflictingAttempt,
    InvalidTransition,
    UsageExceedsBound,
    Storage,
}

impl From<sqlx::Error> for Rejection {
    fn from(_: sqlx::Error) -> Self {
        Self::Storage
    }
}

impl VerifiedEntitlement {
    fn validate(&self) -> Result<()> {
        let text = [
            &self.platform,
            &self.slot,
            &self.principal,
            &self.credential_fingerprint,
            &self.snapshot_fingerprint,
            &self.pool,
            &self.window,
            &self.unit,
            &self.model,
            &self.purpose,
        ];
        let fingerprints = [
            &self.principal,
            &self.credential_fingerprint,
            &self.snapshot_fingerprint,
        ];
        if text.iter().any(|s| s.trim().is_empty() || s.len() > 256)
            || fingerprints.iter().any(|value| {
                value.len() != 64
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || self.credential_epoch <= 0
            || self.revision <= 0
            || !(0..=MAX_UNITS).contains(&self.capacity)
            || !(1..=MAX_UNITS).contains(&self.request_bound)
            || self.observed_at < 0
            || self.expires_at <= self.observed_at
        {
            return Err(Rejection::InvalidProof);
        }
        Ok(())
    }

    fn binding(&self) -> String {
        fingerprint(&[&self.platform, &self.slot])
    }

    // Slot/credential/model aliases must not duplicate the same cloud pool.
    fn budget(&self) -> String {
        fingerprint(&[
            &self.platform,
            &self.principal,
            &self.pool,
            &self.window,
            &self.unit,
        ])
    }

    fn fresh(&self, clock: &impl Fn() -> i64, last_clock: i64) -> Result<i64> {
        let now = clock();
        if now < last_clock {
            return Err(Rejection::ClockAnomaly);
        }
        if now < self.observed_at || now >= self.expires_at {
            return Err(Rejection::Expired);
        }
        Ok(now)
    }

    fn matches_binding(&self, row: &SqliteRow) -> bool {
        row.get::<i64, _>("epoch") == self.credential_epoch
            && row.get::<String, _>("credential_fingerprint") == self.credential_fingerprint
            && row.get::<String, _>("principal") == self.principal
            && row.get::<i64, _>("revision") == self.revision
            && row.get::<String, _>("snapshot_fingerprint") == self.snapshot_fingerprint
            && row.get::<i64, _>("observed_at") == self.observed_at
            && row.get::<i64, _>("expires_at") == self.expires_at
            && row.get::<i64, _>("revoked") == 0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Balance {
    pub ceiling: i64,
    pub held: i64,
    pub spent: i64,
    pub blocked: bool,
}

#[derive(Debug)]
pub struct Reservation {
    id: String,
    budget: String,
    binding: String,
    bound: i64,
}

/// No Drop refund: process death and uncertain HTTP delivery cannot prove that
/// a supplier did not charge. Explicit cancellation is legal only pre-dispatch.
#[derive(Clone)]
pub struct Ledger {
    pool: SqlitePool,
}

impl Ledger {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Publish only an adapter-issued proof. A newer credential fences earlier
    /// work without resetting the stable principal/window accounting budget.
    pub async fn publish(
        &self,
        proof: &VerifiedEntitlement,
        clock: impl Fn() -> i64,
    ) -> Result<()> {
        proof.validate()?;
        let binding = proof.binding();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let old = sqlx::query("SELECT * FROM free_entitlement_bindings WHERE id=?")
            .bind(&binding)
            .fetch_optional(&mut *tx)
            .await?;
        let last_clock = if let Some(row) = old.as_ref() {
            let epoch: i64 = row.get("epoch");
            let revision: i64 = row.get("revision");
            if proof.credential_epoch < epoch
                || (proof.credential_epoch == epoch && proof.revision < revision)
                || (proof.credential_epoch == epoch
                    && proof.credential_fingerprint
                        != row.get::<String, _>("credential_fingerprint"))
                || (proof.credential_epoch == epoch
                    && proof.principal != row.get::<String, _>("principal"))
                || proof.observed_at < row.get::<i64, _>("observed_at")
                || (proof.credential_epoch == epoch
                    && proof.revision == revision
                    && !proof.matches_binding(row))
            {
                return Err(Rejection::StaleBinding);
            }
            row.get("last_clock")
        } else {
            0
        };
        let now = proof.fresh(&clock, last_clock)?;
        // Increasing an observed remaining balance is not a new grant. Keep the
        // lower ceiling until a distinct, adapter-proven entitlement window.
        sqlx::query("INSERT INTO free_entitlement_budgets(id,ceiling) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET ceiling=MIN(ceiling,excluded.ceiling)")
            .bind(proof.budget()).bind(proof.capacity).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO free_entitlement_bindings(id,principal,epoch,credential_fingerprint,revision,snapshot_fingerprint,observed_at,expires_at,last_clock) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET principal=excluded.principal,epoch=excluded.epoch,credential_fingerprint=excluded.credential_fingerprint,revision=excluded.revision,snapshot_fingerprint=excluded.snapshot_fingerprint,observed_at=excluded.observed_at,expires_at=excluded.expires_at,last_clock=excluded.last_clock,revoked=0")
            .bind(&binding).bind(&proof.principal).bind(proof.credential_epoch).bind(&proof.credential_fingerprint)
            .bind(proof.revision).bind(&proof.snapshot_fingerprint).bind(proof.observed_at).bind(proof.expires_at).bind(now)
            .execute(&mut *tx).await?;
        proof.fresh(&clock, now)?;
        tx.commit().await?;
        proof.fresh(&clock, now)?;
        Ok(())
    }

    async fn current(
        conn: &mut SqliteConnection,
        proof: &VerifiedEntitlement,
    ) -> Result<SqliteRow> {
        let row = sqlx::query("SELECT * FROM free_entitlement_bindings WHERE id=?")
            .bind(proof.binding())
            .fetch_optional(conn)
            .await?
            .ok_or(Rejection::Unverified)?;
        if !proof.matches_binding(&row) {
            return Err(Rejection::StaleBinding);
        }
        Ok(row)
    }

    async fn advance_clock(conn: &mut SqliteConnection, binding: &str, now: i64) -> Result<()> {
        sqlx::query("UPDATE free_entitlement_bindings SET last_clock=? WHERE id=?")
            .bind(now)
            .bind(binding)
            .execute(conn)
            .await?;
        Ok(())
    }

    /// Reserve the adapter-proven worst-case bound, not an estimated token cost.
    /// An attempt ID is single use, including after cancellation or settlement.
    pub async fn reserve(
        &self,
        proof: &VerifiedEntitlement,
        attempt: &str,
        clock: impl Fn() -> i64,
    ) -> Result<Reservation> {
        proof.validate()?;
        if attempt.is_empty() || attempt.len() > 256 {
            return Err(Rejection::InvalidProof);
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let binding = Self::current(&mut tx, proof).await?;
        let budget = proof.budget();
        let row = sqlx::query(
            "SELECT ceiling,held,spent,blocked FROM free_entitlement_budgets WHERE id=?",
        )
        .bind(&budget)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Rejection::Unverified)?;
        let duplicate: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM free_entitlement_reservations WHERE attempt=?",
        )
        .bind(fingerprint(&[attempt]))
        .fetch_one(&mut *tx)
        .await?;
        let now = proof.fresh(&clock, binding.get("last_clock"))?;
        if duplicate != 0 {
            return Err(Rejection::ConflictingAttempt);
        }
        let available = row
            .get::<i64, _>("ceiling")
            .checked_sub(row.get("held"))
            .and_then(|n| n.checked_sub(row.get("spent")))
            .unwrap_or(0);
        if row.get::<i64, _>("blocked") != 0 || available < proof.request_bound {
            return Err(Rejection::Exhausted);
        }
        let reservation = Reservation {
            id: uuid::Uuid::new_v4().to_string(),
            budget,
            binding: proof.binding(),
            bound: proof.request_bound,
        };
        sqlx::query("UPDATE free_entitlement_budgets SET held=held+? WHERE id=?")
            .bind(reservation.bound)
            .bind(&reservation.budget)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO free_entitlement_reservations(id,budget,binding,attempt,epoch,credential_fingerprint,revision,snapshot_fingerprint,model,purpose,unit,bound,phase) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,'reserved')")
            .bind(&reservation.id).bind(&reservation.budget).bind(&reservation.binding).bind(fingerprint(&[attempt]))
            .bind(proof.credential_epoch).bind(&proof.credential_fingerprint).bind(proof.revision).bind(&proof.snapshot_fingerprint)
            .bind(&proof.model).bind(&proof.purpose).bind(&proof.unit).bind(reservation.bound).execute(&mut *tx).await?;
        Self::advance_clock(&mut tx, &reservation.binding, now).await?;
        proof.fresh(&clock, now)?;
        tx.commit().await?;
        proof.fresh(&clock, now)?;
        Ok(reservation)
    }

    async fn reservation(
        conn: &mut SqliteConnection,
        reservation: &Reservation,
    ) -> Result<SqliteRow> {
        sqlx::query("SELECT * FROM free_entitlement_reservations WHERE id=? AND budget=? AND binding=? AND bound=?")
            .bind(&reservation.id).bind(&reservation.budget).bind(&reservation.binding).bind(reservation.bound)
            .fetch_optional(conn).await?.ok_or(Rejection::InvalidTransition)
    }

    /// Persist dispatch intent BEFORE any wire send. This is deliberately not
    /// idempotent permission: retrying dispatch must never authorize a second POST.
    /// The caller still needs its final live identity/config/expiry wire guard;
    /// this engine is not a replacement for that adapter consumer.
    pub async fn dispatch(
        &self,
        proof: &VerifiedEntitlement,
        reservation: &Reservation,
        clock: impl Fn() -> i64,
    ) -> Result<()> {
        proof.validate()?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let binding = Self::current(&mut tx, proof).await?;
        let row = Self::reservation(&mut tx, reservation).await?;
        let budget = sqlx::query(
            "SELECT ceiling,held,spent,blocked FROM free_entitlement_budgets WHERE id=?",
        )
        .bind(&reservation.budget)
        .fetch_one(&mut *tx)
        .await?;
        let now = proof.fresh(&clock, binding.get("last_clock"))?;
        // Another slot may have lowered this shared pool after reservation.
        // Holds are retained until explicit cancellation/confirmed settlement;
        // their existence cannot override the current lower hard limit.
        let unallocated = budget
            .get::<i64, _>("ceiling")
            .checked_sub(budget.get("held"))
            .and_then(|remaining| remaining.checked_sub(budget.get("spent")));
        if budget.get::<i64, _>("blocked") != 0
            || unallocated.map_or(true, |remaining| remaining < 0)
        {
            return Err(Rejection::Exhausted);
        }
        if row.get::<String, _>("phase") != "reserved"
            || reservation.budget != proof.budget()
            || reservation.binding != proof.binding()
            || row.get::<i64, _>("epoch") != proof.credential_epoch
            || row.get::<String, _>("credential_fingerprint") != proof.credential_fingerprint
            || row.get::<i64, _>("revision") != proof.revision
            || row.get::<String, _>("snapshot_fingerprint") != proof.snapshot_fingerprint
            || row.get::<String, _>("model") != proof.model
            || row.get::<String, _>("purpose") != proof.purpose
            || row.get::<String, _>("unit") != proof.unit
            || reservation.bound != proof.request_bound
        {
            return Err(Rejection::InvalidTransition);
        }
        sqlx::query("UPDATE free_entitlement_reservations SET phase='dispatched' WHERE id=?")
            .bind(&reservation.id)
            .execute(&mut *tx)
            .await?;
        Self::advance_clock(&mut tx, &reservation.binding, now).await?;
        proof.fresh(&clock, now)?;
        tx.commit().await?;
        // If commit waited past expiry, reject with the full hold retained.
        proof.fresh(&clock, now)?;
        Ok(())
    }

    pub async fn revoke(&self, proof: &VerifiedEntitlement) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("SELECT * FROM free_entitlement_bindings WHERE id=?")
            .bind(proof.binding())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(Rejection::Unverified)?;
        if row.get::<i64, _>("epoch") != proof.credential_epoch
            || row.get::<String, _>("credential_fingerprint") != proof.credential_fingerprint
            || row.get::<i64, _>("revision") != proof.revision
            || row.get::<String, _>("snapshot_fingerprint") != proof.snapshot_fingerprint
        {
            return Err(Rejection::StaleBinding);
        }
        sqlx::query("UPDATE free_entitlement_bindings SET revoked=1 WHERE id=?")
            .bind(proof.binding())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn cancel(&self, reservation: &Reservation) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = Self::reservation(&mut tx, reservation).await?;
        match row.get::<String, _>("phase").as_str() {
            "cancelled" => return Ok(()),
            "reserved" => (),
            _ => return Err(Rejection::InvalidTransition),
        }
        let changed =
            sqlx::query("UPDATE free_entitlement_budgets SET held=held-? WHERE id=? AND held>=?")
                .bind(reservation.bound)
                .bind(&reservation.budget)
                .bind(reservation.bound)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        if changed != 1 {
            return Err(Rejection::Storage);
        }
        sqlx::query("UPDATE free_entitlement_reservations SET phase='cancelled' WHERE id=?")
            .bind(&reservation.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Only use confirmed supplier usage with the same integer unit. A missing,
    /// cancelled stream or ambiguous result MUST leave the full bound held.
    pub async fn settle(&self, reservation: &Reservation, confirmed_units: i64) -> Result<()> {
        if confirmed_units < 0 {
            return Err(Rejection::InvalidProof);
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = Self::reservation(&mut tx, reservation).await?;
        let phase: String = row.get("phase");
        if phase == "settled"
            && row.get::<Option<i64>, _>("confirmed_units") == Some(confirmed_units)
        {
            return Ok(());
        }
        if phase != "dispatched" {
            return Err(Rejection::InvalidTransition);
        }
        if confirmed_units > reservation.bound {
            // A breached charge bound invalidates the hard-stop assumption for
            // the whole shared pool. Never free the hold or admit another send.
            sqlx::query("UPDATE free_entitlement_budgets SET blocked=1 WHERE id=?")
                .bind(&reservation.budget)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Err(Rejection::UsageExceedsBound);
        }
        let changed = sqlx::query("UPDATE free_entitlement_budgets SET held=held-?,spent=spent+? WHERE id=? AND held>=? AND spent<=?")
            .bind(reservation.bound).bind(confirmed_units).bind(&reservation.budget).bind(reservation.bound)
            .bind(MAX_UNITS-confirmed_units).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(Rejection::Storage);
        }
        sqlx::query(
            "UPDATE free_entitlement_reservations SET phase='settled',confirmed_units=? WHERE id=?",
        )
        .bind(confirmed_units)
        .bind(&reservation.id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Accounting-only snapshot; it does not establish eligibility, consume a
    /// permit, or validate the current clock/identity for a physical send.
    pub async fn balance(&self, proof: &VerifiedEntitlement) -> Result<Balance> {
        let row = sqlx::query(
            "SELECT ceiling,held,spent,blocked FROM free_entitlement_budgets WHERE id=?",
        )
        .bind(proof.budget())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(Rejection::Unverified)?;
        Ok(Balance {
            ceiling: row.get("ceiling"),
            held: row.get("held"),
            spent: row.get("spent"),
            blocked: row.get::<i64, _>("blocked") != 0,
        })
    }

    /// Recover an owned attempt after restart. The persisted phase controls what
    /// is allowed; recovering a handle never grants permission to send again.
    pub async fn lookup_attempt(&self, attempt: &str) -> Result<Reservation> {
        if attempt.is_empty() || attempt.len() > 256 {
            return Err(Rejection::InvalidProof);
        }
        let row = sqlx::query(
            "SELECT id,budget,binding,bound FROM free_entitlement_reservations WHERE attempt=?",
        )
        .bind(fingerprint(&[attempt]))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(Rejection::InvalidTransition)?;
        Ok(Reservation {
            id: row.get("id"),
            budget: row.get("budget"),
            binding: row.get("binding"),
            bound: row.get("bound"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc,
    };

    fn artificial(slot: &str, principal: &str, capacity: i64) -> VerifiedEntitlement {
        VerifiedEntitlement {
            platform: "artificial_offline".into(),
            slot: slot.into(),
            principal: fingerprint(&[principal]),
            credential_epoch: 1,
            credential_fingerprint: fingerprint(&["owned-credential-generation-1"]),
            revision: 1,
            snapshot_fingerprint: fingerprint(&["owned-artificial-hard-stop-snapshot"]),
            pool: "shared-all-chat-models".into(),
            window: "owned-window-1".into(),
            unit: "requests".into(),
            capacity,
            model: "owned-model".into(),
            purpose: "chat".into(),
            request_bound: 1,
            observed_at: 100,
            expires_at: 1000,
        }
    }

    #[tokio::test]
    async fn same_principal_slots_share_capacity_and_only_pre_dispatch_cancel_refunds() {
        let db = Db::connect_in_memory().await.unwrap();
        let ledger = Ledger::new(db.pool().clone());
        let a = artificial("a", "subject-A", 1);
        let b = artificial("b", "subject-A", 1);
        ledger
            .publish(&a, || 200)
            .await
            .expect("offline proof registration must be supported");
        ledger.publish(&b, || 200).await.unwrap();
        let hold = ledger.reserve(&a, "first", || 200).await.unwrap();
        assert_eq!(
            ledger.reserve(&b, "second", || 200).await.unwrap_err(),
            Rejection::Exhausted
        );
        ledger.cancel(&hold).await.unwrap();
        ledger.cancel(&hold).await.unwrap();
        let send = ledger.reserve(&b, "third", || 200).await.unwrap();
        ledger.dispatch(&b, &send, || 200).await.unwrap();
        assert_eq!(
            ledger.cancel(&send).await,
            Err(Rejection::InvalidTransition)
        );
        assert_eq!(
            ledger.balance(&a).await.unwrap(),
            Balance {
                ceiling: 1,
                held: 1,
                spent: 0,
                blocked: false
            }
        );
        ledger.settle(&send, 1).await.unwrap();
        ledger.settle(&send, 1).await.unwrap();
        assert_eq!(
            ledger.settle(&send, 0).await,
            Err(Rejection::InvalidTransition)
        );
        assert_eq!(
            ledger.balance(&a).await.unwrap(),
            Balance {
                ceiling: 1,
                held: 0,
                spent: 1,
                blocked: false
            }
        );
        assert_eq!(
            ledger.reserve(&a, "fourth", || 200).await.unwrap_err(),
            Rejection::Exhausted
        );
        let other = artificial("c", "subject-B", 1);
        ledger.publish(&other, || 200).await.unwrap();
        assert!(ledger
            .reserve(&other, "other-principal", || 200)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn credential_rotation_fences_old_proof_without_resetting_same_subject_usage() {
        let db = Db::connect_in_memory().await.unwrap();
        let ledger = Ledger::new(db.pool().clone());
        let old = artificial("a", "subject-A", 2);
        ledger.publish(&old, || 200).await.unwrap();
        let held = ledger
            .reserve(&old, "before-rotation", || 200)
            .await
            .unwrap();
        let mut new = old.clone();
        new.credential_epoch += 1;
        new.credential_fingerprint = fingerprint(&["owned-generation-2"]);
        new.capacity = 100;
        ledger.publish(&new, || 210).await.unwrap();
        assert_eq!(
            ledger.publish(&old, || 211).await,
            Err(Rejection::StaleBinding)
        );
        assert_eq!(
            ledger.reserve(&old, "old", || 211).await.unwrap_err(),
            Rejection::StaleBinding
        );
        assert_eq!(
            ledger.dispatch(&old, &held, || 211).await,
            Err(Rejection::StaleBinding)
        );
        assert_eq!(
            ledger.balance(&new).await.unwrap(),
            Balance {
                ceiling: 2,
                held: 1,
                spent: 0,
                blocked: false
            }
        );
        ledger.cancel(&held).await.unwrap();
        let send = ledger
            .reserve(&new, "after-rotation", || 211)
            .await
            .unwrap();
        ledger.dispatch(&new, &send, || 211).await.unwrap();
        ledger.settle(&send, 1).await.unwrap();
        assert_eq!(ledger.balance(&new).await.unwrap().spent, 1);
    }

    #[tokio::test]
    async fn lower_shared_ceiling_invalidates_previously_held_dispatch_without_refund() {
        let db = Db::connect_in_memory().await.unwrap();
        let ledger = Ledger::new(db.pool().clone());
        let a = artificial("a", "subject-A", 2);
        ledger.publish(&a, || 200).await.unwrap();
        let first = ledger.reserve(&a, "a1", || 200).await.unwrap();
        let second = ledger.reserve(&a, "a2", || 200).await.unwrap();
        let b = artificial("b", "subject-A", 1);
        ledger.publish(&b, || 201).await.unwrap();
        assert_eq!(
            ledger.dispatch(&a, &first, || 201).await,
            Err(Rejection::Exhausted),
            "another slot's lower current shared ceiling must stop outstanding admission"
        );
        assert_eq!(ledger.balance(&a).await.unwrap().held, 2);
        ledger.cancel(&second).await.unwrap();
        ledger.dispatch(&a, &first, || 202).await.unwrap();
        ledger.settle(&first, 1).await.unwrap();
        assert_eq!(
            ledger.reserve(&a, "a3", || 202).await.unwrap_err(),
            Rejection::Exhausted
        );
    }

    #[tokio::test]
    async fn waiting_for_database_expiry_blocks_reservation_and_dispatch_using_fresh_clock() {
        let db = Db::connect_in_memory().await.unwrap();
        let ledger = Ledger::new(db.pool().clone());
        let mut proof = artificial("a", "subject-A", 2);
        proof.expires_at = 210;
        ledger.publish(&proof, || 200).await.unwrap();
        let clock = Arc::new(AtomicI64::new(200));
        let connection = db.pool().acquire().await.unwrap();
        let mut reserve = Box::pin(ledger.reserve(&proof, "expired-while-waiting", || {
            clock.load(Ordering::SeqCst)
        }));
        assert!(matches!(
            futures_util::poll!(reserve.as_mut()),
            std::task::Poll::Pending
        ));
        clock.store(211, Ordering::SeqCst);
        drop(connection);
        assert_eq!(reserve.await.unwrap_err(), Rejection::Expired);
        assert_eq!(ledger.balance(&proof).await.unwrap().held, 0);
        proof.revision += 1;
        proof.snapshot_fingerprint = fingerprint(&["owned-new-snapshot"]);
        proof.observed_at = 211;
        proof.expires_at = 500;
        ledger.publish(&proof, || 211).await.unwrap();
        let hold = ledger
            .reserve(&proof, "fresh-then-waits", || 211)
            .await
            .unwrap();
        let connection = db.pool().acquire().await.unwrap();
        let mut dispatch =
            Box::pin(ledger.dispatch(&proof, &hold, || clock.load(Ordering::SeqCst)));
        assert!(matches!(
            futures_util::poll!(dispatch.as_mut()),
            std::task::Poll::Pending
        ));
        clock.store(501, Ordering::SeqCst);
        drop(connection);
        assert_eq!(dispatch.await, Err(Rejection::Expired));
        assert_eq!(ledger.balance(&proof).await.unwrap().held, 1);
        ledger.cancel(&hold).await.unwrap();
        assert_eq!(ledger.balance(&proof).await.unwrap().held, 0);
    }

    #[tokio::test]
    async fn dispatch_commit_expiry_retains_unknown_hold_and_cannot_be_replayed() {
        let db = Db::connect_in_memory().await.unwrap();
        let ledger = Ledger::new(db.pool().clone());
        let proof = artificial("a", "subject-A", 1);
        ledger.publish(&proof, || 200).await.unwrap();
        let hold = ledger
            .reserve(&proof, "post-commit-expiry", || 200)
            .await
            .unwrap();
        let calls = AtomicUsize::new(0);
        assert_eq!(
            ledger
                .dispatch(&proof, &hold, || {
                    if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                        200
                    } else {
                        1001
                    }
                })
                .await,
            Err(Rejection::Expired)
        );
        let recovered = ledger.lookup_attempt("post-commit-expiry").await.unwrap();
        assert_eq!(
            ledger.cancel(&recovered).await,
            Err(Rejection::InvalidTransition)
        );
        assert_eq!(
            ledger.dispatch(&proof, &recovered, || 201).await,
            Err(Rejection::InvalidTransition)
        );
        assert_eq!(ledger.balance(&proof).await.unwrap().held, 1);
    }

    #[tokio::test]
    async fn exact_model_use_unit_bound_settlement_and_overrun_block_shared_pool() {
        let db = Db::connect_in_memory().await.unwrap();
        let ledger = Ledger::new(db.pool().clone());
        let mut proof = artificial("a", "subject-A", 10);
        proof.unit = "integer-tokens-with-proven-upper-bound".into();
        proof.request_bound = 8;
        ledger.publish(&proof, || 200).await.unwrap();
        let hold = ledger.reserve(&proof, "tokens", || 200).await.unwrap();
        assert_eq!(
            ledger.settle(&hold, 0).await,
            Err(Rejection::InvalidTransition)
        );
        for field in ["model", "purpose", "unit", "bound"] {
            let mut wrong = proof.clone();
            match field {
                "model" => wrong.model = "other-model".into(),
                "purpose" => wrong.purpose = "summary".into(),
                "unit" => wrong.unit = "requests".into(),
                _ => wrong.request_bound = 7,
            }
            assert_eq!(
                ledger.dispatch(&wrong, &hold, || 200).await,
                Err(Rejection::InvalidTransition)
            );
        }
        ledger.dispatch(&proof, &hold, || 200).await.unwrap();
        assert_eq!(
            ledger.dispatch(&proof, &hold, || 200).await,
            Err(Rejection::InvalidTransition)
        );
        ledger.settle(&hold, 3).await.unwrap();
        assert_eq!(
            ledger.balance(&proof).await.unwrap(),
            Balance {
                ceiling: 10,
                held: 0,
                spent: 3,
                blocked: false
            }
        );
        assert_eq!(
            ledger
                .reserve(&proof, "too-large", || 200)
                .await
                .unwrap_err(),
            Rejection::Exhausted
        );
        proof.request_bound = 7;
        let next = ledger.reserve(&proof, "overrun", || 200).await.unwrap();
        ledger.dispatch(&proof, &next, || 200).await.unwrap();
        assert_eq!(
            ledger.settle(&next, 8).await,
            Err(Rejection::UsageExceedsBound)
        );
        assert_eq!(
            ledger.balance(&proof).await.unwrap(),
            Balance {
                ceiling: 10,
                held: 7,
                spent: 3,
                blocked: true
            }
        );
        ledger.settle(&next, 0).await.unwrap();
        ledger.publish(&proof, || 201).await.unwrap();
        assert_eq!(
            ledger
                .reserve(&proof, "still-blocked", || 201)
                .await
                .unwrap_err(),
            Rejection::Exhausted
        );
    }

    #[tokio::test]
    async fn revocation_revision_clock_and_duplicate_attempts_fail_closed() {
        let db = Db::connect_in_memory().await.unwrap();
        let ledger = Ledger::new(db.pool().clone());
        let old = artificial("a", "subject-A", 2);
        ledger.publish(&old, || 200).await.unwrap();
        let hold = ledger
            .reserve(&old, "one-use-attempt", || 200)
            .await
            .unwrap();
        assert_eq!(
            ledger
                .reserve(&old, "one-use-attempt", || 200)
                .await
                .unwrap_err(),
            Rejection::ConflictingAttempt
        );
        ledger.revoke(&old).await.unwrap();
        ledger.revoke(&old).await.unwrap();
        assert_eq!(
            ledger.publish(&old, || 201).await,
            Err(Rejection::StaleBinding)
        );
        assert_eq!(
            ledger.dispatch(&old, &hold, || 201).await,
            Err(Rejection::StaleBinding)
        );
        let mut new = old.clone();
        new.revision += 1;
        new.observed_at = 201;
        new.snapshot_fingerprint = fingerprint(&["owned-revised-proof"]);
        ledger.publish(&new, || 201).await.unwrap();
        assert_eq!(
            ledger.dispatch(&new, &hold, || 201).await,
            Err(Rejection::InvalidTransition)
        );
        ledger.cancel(&hold).await.unwrap();
        assert_eq!(
            ledger
                .reserve(&new, "one-use-attempt", || 201)
                .await
                .unwrap_err(),
            Rejection::ConflictingAttempt
        );
        assert_eq!(
            ledger
                .reserve(&new, "clock-backward", || 199)
                .await
                .unwrap_err(),
            Rejection::ClockAnomaly
        );
        let mut conflict = new.clone();
        conflict.snapshot_fingerprint = fingerprint(&["same-revision-other-body"]);
        assert_eq!(
            ledger.publish(&conflict, || 202).await,
            Err(Rejection::StaleBinding)
        );
        for invalid in [-1, MAX_UNITS + 1] {
            let mut bad = new.clone();
            bad.capacity = invalid;
            assert_eq!(
                ledger.publish(&bad, || 202).await,
                Err(Rejection::InvalidProof)
            );
        }
        assert_eq!(ledger.balance(&new).await.unwrap().spent, 0);
        assert!(ledger
            .reserve(&new, "valid-new-attempt", || 202)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn actual_legacy_file_upgrade_split_pool_concurrency_and_unknown_restart_are_durable() {
        struct OwnedDir(std::path::PathBuf);
        impl Drop for OwnedDir {
            fn drop(&mut self) {
                for file in ["owned.db", "owned.db-wal", "owned.db-shm"] {
                    let _ = std::fs::remove_file(self.0.join(file));
                }
                let _ = std::fs::remove_dir(&self.0);
            }
        }
        let dir = OwnedDir(
            std::env::temp_dir().join(format!("llmgw-owned-entitlement-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&dir.0).unwrap();
        let path = dir.0.join("owned.db");
        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let legacy = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../tests/fixtures/pre_catalog_schema.sql"))
            .execute(&legacy)
            .await
            .unwrap();
        let pre: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'free_entitlement_%' OR name LIKE 'catalog_%'").fetch_one(&legacy).await.unwrap();
        assert_eq!(pre, 0, "real legacy schema before production Db migration");
        legacy.close().await;
        let db_a = Db::connect_path(&path).await.unwrap();
        let db_b = Db::connect_path(&path).await.unwrap();
        crate::db::migrations::run(db_a.pool()).await.unwrap();
        let preserved: (String, i64, String) = sqlx::query_as(
            "SELECT alias,enabled,price_json FROM models WHERE id='owned:custom-alias'",
        )
        .fetch_one(db_a.pool())
        .await
        .unwrap();
        assert_eq!(
            preserved,
            (
                "custom-alias".into(),
                0,
                "{\"prompt\":4,\"completion\":5,\"currency\":\"usd\"}".into()
            )
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT content FROM session_messages WHERE session_id='legacy-history'"
            )
            .fetch_one(db_a.pool())
            .await
            .unwrap(),
            "owned pre-catalog history"
        );
        let a = artificial("a", "subject-A", 1);
        let mut b = artificial("b", "subject-A", 1);
        b.model = "another-model-sharing-same-pool".into();
        let ledger_a = Ledger::new(db_a.pool().clone());
        let ledger_b = Ledger::new(db_b.pool().clone());
        ledger_a.publish(&a, || 200).await.unwrap();
        ledger_b.publish(&b, || 200).await.unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let (r_a, r_b) = tokio::join!(
            async {
                barrier.wait().await;
                ledger_a.reserve(&a, "concurrent-a", || 200).await
            },
            async {
                barrier.wait().await;
                ledger_b.reserve(&b, "concurrent-b", || 200).await
            }
        );
        let (winner, proof, attempt) = match (r_a, r_b) {
            (Ok(hold), Err(Rejection::Exhausted)) => (hold, &a, "concurrent-a"),
            (Err(Rejection::Exhausted), Ok(hold)) => (hold, &b, "concurrent-b"),
            other => panic!("exactly one split-pool reservation must win: {other:?}"),
        };
        ledger_a.dispatch(proof, &winner, || 200).await.unwrap();
        assert_eq!(ledger_b.balance(&b).await.unwrap().held, 1);
        let pre_dispatch = artificial("pre-dispatch", "subject-B", 1);
        ledger_a.publish(&pre_dispatch, || 200).await.unwrap();
        let not_sent = ledger_a
            .reserve(&pre_dispatch, "reserved-before-restart", || 200)
            .await
            .unwrap();
        drop(not_sent);
        drop((ledger_a, ledger_b, winner));
        db_a.pool().close().await;
        db_b.pool().close().await;
        let reopened = Db::connect_path(&path).await.unwrap();
        let ledger = Ledger::new(reopened.pool().clone());
        assert_eq!(
            ledger.balance(&a).await.unwrap(),
            Balance {
                ceiling: 1,
                held: 1,
                spent: 0,
                blocked: false
            }
        );
        let recovered = ledger.lookup_attempt(attempt).await.unwrap();
        assert_eq!(
            ledger.cancel(&recovered).await,
            Err(Rejection::InvalidTransition)
        );
        let mut fresh = a.clone();
        fresh.revision += 1;
        fresh.observed_at = 2000;
        fresh.expires_at = 3000;
        fresh.snapshot_fingerprint = fingerprint(&["owned-restart-snapshot"]);
        ledger.publish(&fresh, || 2000).await.unwrap();
        assert_eq!(
            ledger
                .reserve(&fresh, "after-expiry-and-restart", || 2000)
                .await
                .unwrap_err(),
            Rejection::Exhausted
        );
        ledger.settle(&recovered, 1).await.unwrap();
        assert_eq!(ledger.balance(&fresh).await.unwrap().spent, 1);
        assert_eq!(
            ledger.balance(&pre_dispatch).await.unwrap().held,
            1,
            "restart alone never releases even a pre-dispatch hold"
        );
        let not_sent = ledger
            .lookup_attempt("reserved-before-restart")
            .await
            .unwrap();
        ledger.cancel(&not_sent).await.unwrap();
        assert_eq!(ledger.balance(&pre_dispatch).await.unwrap().held, 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM catalog_model_policy WHERE managed_by='user'"
            )
            .fetch_one(reopened.pool())
            .await
            .unwrap(),
            2
        );
        drop(ledger);
        reopened.pool().close().await;
    }

    #[tokio::test]
    async fn subject_switch_requires_new_credential_epoch_and_does_not_borrow_another_budget() {
        let db = Db::connect_in_memory().await.unwrap();
        let ledger = Ledger::new(db.pool().clone());
        let old = artificial("slot", "subject-A", 1);
        ledger.publish(&old, || 200).await.unwrap();
        let hold = ledger
            .reserve(&old, "subject-A-sent", || 200)
            .await
            .unwrap();
        ledger.dispatch(&old, &hold, || 200).await.unwrap();
        let mut other = artificial("slot", "subject-B", 1);
        other.revision += 1;
        other.snapshot_fingerprint = fingerprint(&["owned-other-principal-observation"]);
        assert_eq!(
            ledger.publish(&other, || 201).await,
            Err(Rejection::StaleBinding)
        );
        other.credential_epoch += 1;
        other.credential_fingerprint = fingerprint(&["owned-B-credential"]);
        ledger.publish(&other, || 201).await.unwrap();
        assert!(ledger
            .reserve(&other, "subject-B-fresh", || 201)
            .await
            .is_ok());
        assert_eq!(ledger.balance(&old).await.unwrap().held, 1);
        assert_eq!(
            ledger.publish(&old, || 202).await,
            Err(Rejection::StaleBinding)
        );
        ledger.settle(&hold, 1).await.unwrap();
        assert_eq!(ledger.balance(&old).await.unwrap().spent, 1);
        assert_eq!(ledger.balance(&other).await.unwrap().spent, 0);
    }
}

#[path = "entitlement_dispatch.rs"]
pub(crate) mod dispatch;

#[cfg(test)]
impl VerifiedEntitlement {
    /// Artificial loopback contract only; no production issuer or IPC constructor.
    pub(crate) fn artificial_dispatch(
        platform: &str,
        slot: &str,
        identity: (String, String),
        model: &str,
        purpose: &str,
        capacity: i64,
    ) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            platform: platform.into(),
            slot: slot.into(),
            principal: identity.0,
            credential_epoch: 1,
            credential_fingerprint: identity.1,
            revision: 1,
            snapshot_fingerprint: fingerprint(&["explicit-artificial-request-hard-stop"]),
            pool: "owned-shared-request-pool".into(),
            window: "owned-window".into(),
            unit: "requests".into(),
            capacity,
            model: model.into(),
            purpose: purpose.into(),
            request_bound: 1,
            observed_at: now - 1,
            expires_at: now + 3600,
        }
    }

    /// Test-only purpose variant of the exact same observed credential/catalog
    /// snapshot. Re-publishing identical revision/fingerprint with fresh
    /// timestamps is a stale binding; tests must not accidentally mint one.
    pub(crate) fn artificial_dispatch_same_snapshot(
        current: &Self,
        model: &str,
        purpose: &str,
    ) -> Self {
        let mut proof = current.clone();
        proof.model = model.into();
        proof.purpose = purpose.into();
        proof
    }
}
