//! Final account dispatch owns a durable hold, never an automatic Drop refund.
//! Production has no issuer yet. Explicit artificial grants exist only in tests.
use super::{Ledger, Rejection, Reservation, VerifiedEntitlement};
use crate::{
    domain::{Provider, Usage},
    error::{GatewayError, Result},
};
use sqlx::Row;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

struct Grant {
    proof: Arc<VerifiedEntitlement>,
    runtime: String,
    // An issuer-authorized interpretation of a validated terminal receipt.
    // No conversion from price, remaining balance or an estimated token count.
    confirm: fn(&Usage) -> Option<i64>,
    // Native reports retain Complete/Partial/Missing and optional fields.
    // Do not flatten unknown native evidence into Usage's scalar zeros.
    confirm_native: Option<fn(&crate::proxy::trae::sse::TraeUsageReport) -> Option<i64>>,
}
#[cfg(test)]
type GrantKey = (String, String, String);

pub(crate) struct DispatchGate {
    ledger: Ledger,
    #[cfg(test)]
    reservation_barrier: parking_lot::RwLock<Option<Arc<tokio::sync::Barrier>>>,
    #[cfg(test)]
    reservation_visits: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    grants: parking_lot::RwLock<std::collections::HashMap<GrantKey, Arc<Grant>>>,
}
fn denied() -> GatewayError {
    crate::cost_policy::rejected("account_hard_stop_and_reservation_unverified")
}
fn storage(error: Rejection) -> GatewayError {
    tracing::warn!(reason=?error, "account entitlement operation unconfirmed");
    crate::cost_policy::rejected("account_entitlement_operation_unconfirmed")
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
impl DispatchGate {
    pub(crate) fn new(pool: sqlx::SqlitePool) -> Self {
        Self {
            ledger: Ledger::new(pool),
            #[cfg(test)]
            reservation_barrier: parking_lot::RwLock::new(None),
            #[cfg(test)]
            reservation_visits: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            grants: parking_lot::RwLock::new(std::collections::HashMap::new()),
        }
    }
    fn resolve(&self, provider: &Provider, selector: &str, purpose: &str) -> Option<Arc<Grant>> {
        // No production issuer: persisted DB rows, config and logins cannot mint this.
        let _ = selector;
        #[cfg(not(test))]
        let grant: Option<Arc<Grant>> = None;
        #[cfg(test)]
        let grant = self
            .grants
            .read()
            .get(&(provider.id.clone(), selector.into(), purpose.into()))
            .cloned();
        grant.filter(|grant| {
            provider.runtime_id.as_deref() == Some(grant.runtime.as_str())
                && grant.proof.purpose == purpose
        })
    }
    /// Preview is non-consuming and never replaces the final atomic reservation.
    pub(crate) async fn eligible(
        &self,
        provider: &Provider,
        selector: &str,
        purpose: &str,
    ) -> Result<bool> {
        let Some(grant) = self.resolve(provider, selector, purpose) else {
            return Ok(false);
        };
        let mut connection = self
            .ledger
            .pool
            .acquire()
            .await
            .map_err(|e| storage(e.into()))?;
        let binding = Ledger::current(&mut connection, &grant.proof)
            .await
            .map_err(storage)?;
        grant
            .proof
            .fresh(&now, binding.get("last_clock"))
            .map_err(storage)?;
        drop(connection);
        let balance = self.ledger.balance(&grant.proof).await.map_err(storage)?;
        grant.proof.fresh(&now, 0).map_err(storage)?;
        Ok(!balance.blocked
            && balance
                .ceiling
                .checked_sub(balance.spent)
                .and_then(|value| value.checked_sub(balance.held))
                .is_some_and(|available| available >= grant.proof.request_bound))
    }
    pub(crate) async fn reserve(
        &self,
        provider: &Provider,
        selector: &str,
        purpose: &str,
    ) -> Result<Arc<Permit>> {
        let grant = self
            .resolve(provider, selector, purpose)
            .ok_or_else(denied)?;
        #[cfg(test)]
        {
            let barrier = self.reservation_barrier.read().clone();
            if let Some(barrier) = barrier {
                if self.reservation_visits.fetch_add(1, Ordering::AcqRel) < 2 {
                    barrier.wait().await;
                }
            }
        }
        let attempt = uuid::Uuid::new_v4().to_string();
        let reservation = self
            .ledger
            .reserve(&grant.proof, &attempt, now)
            .await
            .map_err(storage)?;
        Ok(Arc::new(Permit {
            ledger: self.ledger.clone(),
            grant,
            reservation,
            phase: AtomicU8::new(0),
        }))
    }
    #[cfg(test)]
    pub(crate) fn set_reservation_barrier(&self, barrier: Arc<tokio::sync::Barrier>) {
        self.reservation_visits.store(0, Ordering::Release);
        *self.reservation_barrier.write() = Some(barrier);
    }
    #[cfg(test)]
    pub(crate) async fn install_artificial(
        &self,
        provider: &Provider,
        selector: &str,
        proof: VerifiedEntitlement,
        confirm: fn(&Usage) -> Option<i64>,
    ) -> std::result::Result<(), Rejection> {
        let runtime = provider.runtime_id.clone().ok_or(Rejection::Unverified)?;
        self.ledger.publish(&proof, now).await?;
        let key = (provider.id.clone(), selector.into(), proof.purpose.clone());
        self.grants.write().insert(
            key,
            Arc::new(Grant {
                proof: Arc::new(proof),
                runtime,
                confirm,
                confirm_native: None,
            }),
        );
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn install_artificial_native(
        &self,
        provider: &Provider,
        selector: &str,
        proof: VerifiedEntitlement,
        confirm: fn(&crate::proxy::trae::sse::TraeUsageReport) -> Option<i64>,
    ) -> std::result::Result<(), Rejection> {
        let runtime = provider.runtime_id.clone().ok_or(Rejection::Unverified)?;
        self.ledger.publish(&proof, now).await?;
        self.grants.write().insert(
            (provider.id.clone(), selector.into(), proof.purpose.clone()),
            Arc::new(Grant {
                proof: Arc::new(proof),
                runtime,
                confirm: |_| None,
                confirm_native: Some(confirm),
            }),
        );
        Ok(())
    }
}
/// A reserved capability follows the exact physical send and returned stream.
/// Dropping it, including cancellation during commit, never guesses a refund.
pub(crate) struct Permit {
    ledger: Ledger,
    grant: Arc<Grant>,
    reservation: Reservation,
    // 0 reserved, 1 dispatch intent may have committed, 2 confirmed settlement.
    phase: AtomicU8,
}
impl Permit {
    pub(crate) async fn before_send(
        &self,
        fingerprint: &str,
        principal: &str,
        physical_model: &str,
    ) -> Result<()> {
        if fingerprint != self.grant.proof.credential_fingerprint
            || principal != self.grant.proof.principal
            || physical_model != self.grant.proof.model
        {
            return Err(storage(Rejection::StaleBinding));
        }
        self.phase
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| storage(Rejection::InvalidTransition).no_replay())?;
        self.ledger
            .dispatch(&self.grant.proof, &self.reservation, now)
            .await
            .map_err(|error| storage(error).no_replay())
    }
    pub(crate) async fn settle_usage(&self, usage: Option<&Usage>) -> Result<()> {
        let Some(units) = usage.and_then(self.grant.confirm) else {
            return Ok(());
        };
        self.ledger
            .settle(&self.reservation, units)
            .await
            .map_err(|error| storage(error).no_replay())?;
        self.phase.store(2, Ordering::Release);
        Ok(())
    }
    pub(crate) async fn settle_native_receipt(
        &self,
        report: &crate::proxy::trae::sse::TraeUsageReport,
    ) -> Result<()> {
        let Some(units) = self
            .grant
            .confirm_native
            .and_then(|confirm| confirm(report))
        else {
            return Ok(());
        };
        self.ledger
            .settle(&self.reservation, units)
            .await
            .map_err(|error| storage(error).no_replay())?;
        self.phase.store(2, Ordering::Release);
        Ok(())
    }
    pub(crate) async fn failed(&self, error: GatewayError) -> GatewayError {
        if self.phase.load(Ordering::Acquire) == 0 {
            // Explicit known pre-send error only. Drop/cancellation does not run this.
            if let Err(failure) = self.ledger.cancel(&self.reservation).await {
                return storage(failure).no_replay();
            }
            return error;
        }
        // No authoritative terminal: retain the full hold and prohibit replay/fallback.
        error.no_replay()
    }
}
