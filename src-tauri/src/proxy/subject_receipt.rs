//! Purpose/session-limited dispatch subject correlation. Never an auth or fee proof.
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{future::Future, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Profile {
    #[serde(rename = "workbuddy-cn-model-v1")]
    WorkBuddyCn,
    #[serde(rename = "workbuddy-global-model-v1")]
    WorkBuddyGlobal,
    #[serde(rename = "qoder-cn-model-v1")]
    QoderCn,
    #[serde(rename = "qoder-global-model-v1")]
    QoderGlobal,
}
impl Profile {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WorkBuddyCn => "workbuddy-cn-model-v1",
            Self::WorkBuddyGlobal => "workbuddy-global-model-v1",
            Self::QoderCn => "qoder-cn-model-v1",
            Self::QoderGlobal => "qoder-global-model-v1",
        }
    }
    pub(crate) fn region(self) -> &'static str {
        match self {
            Self::WorkBuddyCn | Self::QoderCn => "cn",
            Self::WorkBuddyGlobal | Self::QoderGlobal => "global",
        }
    }
}

// Contains only existing local identity facts, NOT the authorization object.
// Never serialized directly, even by AttemptRecord; Debug is deliberately redacted.
#[derive(Clone)]
pub(crate) struct CapturedSubject {
    profile: Profile,
    slot: String,
    principal: String,
    provider: String,
    model: String,
}
impl std::fmt::Debug for CapturedSubject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CapturedSubject(<private>)")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    version: u8,
    purpose: String,
    profile_id: Profile,
    scope_digest: String,
    binding_digest: String,
}
fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.contains("://") && !s.chars().any(char::is_control)
}
fn hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}
fn trace(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= crate::trace::MAX_TRACE_ID_LEN
        && s.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
}
fn digest<T: Serialize>(tuple: &T) -> Option<String> {
    serde_json::to_vec(tuple)
        .ok()
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
}
fn binding(
    scope: &str,
    profile: Profile,
    session: &str,
    trace: &str,
    provider: &str,
    model: &str,
    attempt: usize,
) -> Option<String> {
    digest(&(
        "llmgw:conversation-feedback:attempt-binding:v1",
        profile.as_str(),
        scope,
        session,
        trace,
        provider,
        model,
        attempt,
    ))
}
impl CapturedSubject {
    // Called only by the original audit serializer with this attempt's row context.
    // No globally stable principal or credential-generation digest is persisted.
    pub(crate) fn project(
        &self,
        session: &str,
        trace_id: &str,
        provider: &str,
        model: &str,
        attempt: usize,
    ) -> Option<Receipt> {
        if !identifier(session)
            || !trace(trace_id)
            || self.provider != provider
            || self.model != model
        {
            return None;
        }
        let scope_digest = digest(&(
            "llmgw:conversation-feedback:subject-scope:v1",
            session,
            self.profile.as_str(),
            &self.slot,
            &self.principal,
        ))?;
        let binding_digest = binding(
            &scope_digest,
            self.profile,
            session,
            trace_id,
            provider,
            model,
            attempt,
        )?;
        Some(Receipt {
            version: 1,
            purpose: "conversation_feedback".into(),
            profile_id: self.profile,
            scope_digest,
            binding_digest,
        })
    }
}
impl Receipt {
    pub(crate) fn validate(
        &self,
        session: &str,
        trace_id: &str,
        provider: &str,
        model: &str,
        attempt: usize,
    ) -> bool {
        self.version == 1
            && self.purpose == "conversation_feedback"
            && hex(&self.scope_digest)
            && hex(&self.binding_digest)
            && identifier(session)
            && trace(trace_id)
            && identifier(provider)
            && identifier(model)
            && binding(
                &self.scope_digest,
                self.profile_id,
                session,
                trace_id,
                provider,
                model,
                attempt,
            )
            .as_deref()
                == Some(self.binding_digest.as_str())
    }
    pub(crate) fn profile(&self) -> Profile {
        self.profile_id
    }
    pub(crate) fn scope(&self) -> &str {
        &self.scope_digest
    }
}

enum Captures {
    Empty,
    One(CapturedSubject),
    Ambiguous,
}
struct Context {
    provider: String,
    model: String,
    captures: Captures,
}
#[derive(Clone)]
pub(crate) struct Observation(Arc<Mutex<Context>>);
tokio::task_local! { static CURRENT: Observation; }
impl Observation {
    pub(crate) fn new(provider: String, model: String) -> Self {
        Self(Arc::new(Mutex::new(Context {
            provider,
            model,
            captures: Captures::Empty,
        })))
    }
    // Allocate synchronously: the returned future must not contain a large sender.
    pub(crate) fn scope<F: Future>(&self, future: F) -> impl Future<Output = F::Output> {
        CURRENT.scope(self.clone(), Box::pin(future))
    }
    pub(crate) fn captured(&self) -> Option<CapturedSubject> {
        match &self.0.lock().captures {
            Captures::One(value) => Some(value.clone()),
            _ => None,
        }
    }
}
// Existing transport authorization-current fence calls this synchronously just
// before its POST. Absence/invalid input/repeated capture is unknown, never a guess.
pub(crate) fn capture(profile: Profile, slot: &str, principal: &str) {
    let _ = CURRENT.try_with(|scope| {
        let mut state = scope.0.lock();
        state.captures = match &state.captures {
            Captures::Empty
                if identifier(slot)
                    && hex(principal)
                    && identifier(&state.provider)
                    && identifier(&state.model) =>
            {
                Captures::One(CapturedSubject {
                    profile,
                    slot: slot.into(),
                    principal: principal.into(),
                    provider: state.provider.clone(),
                    model: state.model.clone(),
                })
            }
            _ => Captures::Ambiguous,
        };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    const TRACE: &str = "0123456789abcdef0123456789abcdef";
    async fn receipt(profile: Profile, slot: &str, principal: &str, session: &str) -> Receipt {
        let observation = Observation::new("provider".into(), "model".into());
        observation
            .scope(async {
                capture(profile, slot, principal);
            })
            .await;
        observation
            .captured()
            .unwrap()
            .project(session, TRACE, "provider", "model", 0)
            .unwrap()
    }
    #[tokio::test]
    async fn opaque_subject_receipt_is_purpose_session_region_slot_and_principal_limited() {
        let principal = "a".repeat(64);
        let a = receipt(Profile::WorkBuddyCn, "slot-a", &principal, "session-a").await;
        let refreshed = receipt(Profile::WorkBuddyCn, "slot-a", &principal, "session-a").await;
        assert_eq!(
            a.scope(),
            refreshed.scope(),
            "credential hashes are not an input"
        );
        for other in [
            receipt(Profile::WorkBuddyGlobal, "slot-a", &principal, "session-a").await,
            receipt(Profile::QoderCn, "slot-a", &principal, "session-a").await,
            receipt(Profile::QoderGlobal, "slot-a", &principal, "session-a").await,
            receipt(Profile::WorkBuddyCn, "slot-b", &principal, "session-a").await,
            receipt(Profile::WorkBuddyCn, "slot-a", &"b".repeat(64), "session-a").await,
            receipt(Profile::WorkBuddyCn, "slot-a", &principal, "session-b").await,
        ] {
            assert_ne!(a.scope(), other.scope());
        }
        let public = serde_json::to_string(&a).unwrap();
        for private in [
            "slot-a",
            principal.as_str(),
            "credential",
            "principal",
            "session-a",
            TRACE,
            "provider",
        ] {
            assert!(!public.contains(private));
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&public)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            5
        );
        assert!(
            !public.contains("subject-scope:v1"),
            "derivation inputs/domain do not become public metadata"
        );
    }
    #[tokio::test]
    async fn opaque_subject_receipt_binds_exact_attempt_and_rejects_unknown_contracts() {
        let r = receipt(Profile::QoderCn, "slot", &"a".repeat(64), "session").await;
        assert!(r.validate("session", TRACE, "provider", "model", 0));
        assert!(!r.validate("other", TRACE, "provider", "model", 0));
        assert!(!r.validate(
            "session",
            "ffffffffffffffffffffffffffffffff",
            "provider",
            "model",
            0
        ));
        assert!(!r.validate("session", TRACE, "other", "model", 0));
        assert!(!r.validate("session", TRACE, "provider", "other", 0));
        assert!(!r.validate("session", TRACE, "provider", "model", 1));
        for (key, value) in [
            ("version", serde_json::json!(2)),
            ("purpose", serde_json::json!("other-purpose")),
        ] {
            let mut v = serde_json::to_value(&r).unwrap();
            v[key] = value;
            assert!(!serde_json::from_value::<Receipt>(v)
                .unwrap()
                .validate("session", TRACE, "provider", "model", 0));
        }
        let mut v = serde_json::to_value(&r).unwrap();
        v["authorization"] = serde_json::json!("private");
        assert!(serde_json::from_value::<Receipt>(v).is_err());
    }
    #[tokio::test]
    async fn opaque_subject_receipt_context_rejects_ambiguity_missing_and_spawn_inheritance() {
        capture(Profile::WorkBuddyCn, "no-scope", &"a".repeat(64));
        let obs = Observation::new("provider".into(), "model".into());
        assert!(obs.captured().is_none());
        obs.scope(async {
            tokio::spawn(async { capture(Profile::WorkBuddyCn, "spawn", &"a".repeat(64)) })
                .await
                .unwrap();
            assert!(obs.captured().is_none());
            capture(Profile::WorkBuddyCn, "slot", &"a".repeat(64));
            assert!(obs.captured().is_some());
            capture(Profile::WorkBuddyCn, "slot", &"a".repeat(64));
        })
        .await;
        assert!(obs.captured().is_none());
        let invalid = Observation::new("provider".into(), "model".into());
        invalid
            .scope(async {
                capture(Profile::WorkBuddyCn, "slot", "missing");
                capture(Profile::WorkBuddyCn, "slot", &"a".repeat(64));
            })
            .await;
        assert!(invalid.captured().is_none());
    }
    #[tokio::test]
    async fn opaque_subject_receipt_concurrent_nested_and_cancelled_snapshots_never_follow_current_subject(
    ) {
        let a = Observation::new("provider".into(), "model".into());
        let b = Observation::new("provider".into(), "model".into());
        tokio::join!(
            a.scope(async {
                tokio::task::yield_now().await;
                capture(Profile::QoderCn, "a", &"a".repeat(64));
            }),
            b.scope(async {
                capture(Profile::QoderCn, "b", &"b".repeat(64));
                tokio::task::yield_now().await;
            })
        );
        let first = a.captured().unwrap();
        assert_ne!(
            first
                .project("session", TRACE, "provider", "model", 0)
                .unwrap()
                .scope(),
            b.captured()
                .unwrap()
                .project("session", TRACE, "provider", "model", 0)
                .unwrap()
                .scope()
        );
        let outer = Observation::new("provider".into(), "model".into());
        outer
            .scope(async {
                b.scope(async {
                    capture(Profile::QoderCn, "other", &"c".repeat(64));
                })
                .await;
                assert!(outer.captured().is_none());
            })
            .await;
        let cancel = Observation::new("provider".into(), "model".into());
        let mut future = Box::pin(cancel.scope(async {
            capture(Profile::QoderCn, "a", &"a".repeat(64));
            std::future::pending::<()>().await;
        }));
        assert!(futures_util::poll!(future.as_mut()).is_pending());
        drop(future);
        assert!(cancel.captured().is_some());
        assert_eq!(
            first
                .project("session", TRACE, "provider", "model", 0)
                .unwrap()
                .scope(),
            cancel
                .captured()
                .unwrap()
                .project("session", TRACE, "provider", "model", 0)
                .unwrap()
                .scope()
        );
        assert!(first
            .project("session", TRACE, "wrong-provider", "model", 0)
            .is_none());
        assert_eq!(format!("{first:?}"), "CapturedSubject(<private>)");
    }
}

#[cfg(test)]
mod lifecycle_tests;
