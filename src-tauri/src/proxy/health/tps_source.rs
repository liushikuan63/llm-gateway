//! In-memory ownership of existing TPS references, never a model revision or
//! account/entitlement proof. No request content or credential digest is exposed.
use super::HealthRegistry;
use crate::domain::Provider;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};

pub(super) struct Target {
    provider: String,
    model: String,
    fingerprint: [u8; 32],
}
pub(super) type Sources = HashMap<String, Option<Arc<Target>>>;

#[derive(Clone)]
pub(crate) struct TpsLease {
    key: String,
    owner: Arc<Target>,
}
fn part(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}
fn provider_fingerprint(provider: &Provider) -> [u8; 32] {
    let mut hash = Sha256::new();
    part(&mut hash, b"known_tps_target_v1");
    part(&mut hash, provider.id.as_bytes());
    part(&mut hash, format!("{:?}", provider.dialect).as_bytes());
    part(&mut hash, provider.base_url.as_bytes());
    // Existing encrypted configuration only; never decrypt or publish it here.
    part(&mut hash, provider.api_key_enc.as_bytes());
    part(&mut hash, &[u8::from(provider.runtime_id.is_some())]);
    part(
        &mut hash,
        provider.runtime_id.as_deref().unwrap_or("").as_bytes(),
    );
    hash.finalize().into()
}
fn fingerprint(provider: &Provider, model: &str) -> [u8; 32] {
    model_fingerprint(provider_fingerprint(provider), model)
}
fn model_fingerprint(provider: [u8; 32], model: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(provider);
    part(&mut hash, model.as_bytes());
    hash.finalize().into()
}
fn same(a: Option<&Option<Arc<Target>>>, b: Option<&Option<Arc<Target>>>) -> bool {
    match (a, b) {
        (Some(Some(a)), Some(Some(b))) => Arc::ptr_eq(a, b),
        (None, None) | (Some(None), Some(None)) => true,
        _ => false,
    }
}
pub(super) fn readable(
    sources: &Sources,
    provider_id: &str,
    model: &str,
    provider: Option<&Provider>,
) -> bool {
    match sources.get(&HealthRegistry::key(provider_id, model)) {
        None => true,        // Preserve standalone, unbound HealthRegistry callers.
        Some(None) => false, // Ambiguous legacy string key cannot name a source.
        Some(Some(target)) => {
            target.provider == provider_id
                && target.model == model
                && provider.map_or(true, |p| target.fingerprint == fingerprint(p, model))
        }
    }
}
impl HealthRegistry {
    pub(crate) fn sync_tps_sources(&self, providers: &[Provider]) {
        let mut current = self.tps_sources.write();
        let mut next: Sources = HashMap::new();
        for provider in providers {
            let source = provider_fingerprint(provider);
            // Match Router's existing empty-provider fallback; no discovery.
            for model in provider
                .models
                .iter()
                .map(|m| m.upstream.as_str())
                .chain(provider.models.is_empty().then_some("default"))
            {
                let key = Self::key(&provider.id, model);
                let fp = model_fingerprint(source, model);
                if let Some(slot) = next.get_mut(&key) {
                    if slot.as_ref().map_or(true, |t| t.fingerprint != fp) {
                        *slot = None;
                    }
                    continue;
                }
                let owner = current
                    .get(&key)
                    .and_then(Option::as_ref)
                    .filter(|t| t.fingerprint == fp)
                    .cloned()
                    .unwrap_or_else(|| {
                        Arc::new(Target {
                            provider: provider.id.clone(),
                            model: model.to_owned(),
                            fingerprint: fp,
                        })
                    });
                next.insert(key, Some(owner));
            }
        }
        // Publication and metric commits share this lock, including reset. Arc
        // identity prevents A->B->A/delete-recreate from reviving an old owner.
        for key in current.keys().chain(next.keys()) {
            if !same(current.get(key), next.get(key)) {
                if let Some(entry) = self.inner.get(key) {
                    let mut entry = entry.write();
                    entry.avg_tps = 0.0;
                    entry.tps_samples = 0;
                    entry.last_tps_at = None;
                }
            }
        }
        *current = next;
    }
    pub(crate) fn tps_lease(&self, provider: &Provider, model: &str) -> Option<TpsLease> {
        let key = Self::key(&provider.id, model);
        let sources = self.tps_sources.read();
        let target = sources.get(&key)?.as_ref()?;
        if target.provider != provider.id
            || target.model != model
            || target.fingerprint != fingerprint(provider, model)
        {
            return None;
        }
        Some(TpsLease {
            key,
            owner: target.clone(),
        })
    }
    pub(crate) fn record_bound_tps(
        &self,
        lease: Option<&TpsLease>,
        completion_tokens: u32,
        latency_ms: u32,
    ) {
        let Some(lease) = lease else { return };
        let sources = self.tps_sources.read();
        let Some(Some(current)) = sources.get(&lease.key) else {
            return;
        };
        if Arc::ptr_eq(current, &lease.owner) {
            self.record_tps(
                &current.provider,
                &current.model,
                completion_tokens,
                latency_ms,
            );
        }
    }

    pub(crate) fn record_bound_success(
        &self,
        lease: Option<&TpsLease>,
        latency_ms: u32,
        completion_tokens: Option<u32>,
    ) {
        let Some(lease) = lease else { return };
        let sources = self.tps_sources.read();
        let Some(Some(current)) = sources.get(&lease.key) else {
            return;
        };
        if !Arc::ptr_eq(current, &lease.owner) {
            return;
        }
        // A completed native transport is a real success even without usage.
        // Keep publication out of both writes; missing usage is never a zero.
        self.record_success(&current.provider, &current.model, latency_ms);
        if let Some(tokens) = completion_tokens {
            self.record_tps(&current.provider, &current.model, tokens, latency_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider() -> Provider {
        serde_json::from_value(serde_json::json!({"id":"owned-profile","name":"owned-profile",
            "base_url":"http://127.0.0.1:1/v1","api_key_enc":"","dialect":"openai","enabled":true,
            "priority":1,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z",
            "updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"public","upstream":"physical",
                "context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true}]})).unwrap()
    }
    fn sample(h: &HealthRegistry, p: &Provider, owner: Option<&TpsLease>) {
        h.record_success(&p.id, "physical", 1000);
        h.record_bound_tps(owner, 8, 1000);
    }
    #[test]
    fn retired_completion_cannot_create_new_source_health_or_zero_usage_sample() {
        let h = HealthRegistry::new();
        let a = provider();
        h.sync_tps_sources(std::slice::from_ref(&a));
        let old = h.tps_lease(&a, "physical").unwrap();
        let mut b = a.clone();
        b.base_url = "http://127.0.0.1:2/v1".into();
        h.sync_tps_sources(std::slice::from_ref(&b));
        h.record_bound_success(Some(&old), 1000, Some(8));
        assert_eq!(h.get(&b.id, "physical").last_checked_at, 0);
        assert_eq!(h.get(&b.id, "physical").tps_samples, 0);
        let current = h.tps_lease(&b, "physical").unwrap();
        h.record_bound_success(Some(&current), 1000, None);
        assert!(h.get(&b.id, "physical").last_checked_at > 0);
        assert_eq!(h.get(&b.id, "physical").tps_samples, 0);
        h.record_bound_success(Some(&current), 1000, Some(8));
        assert_eq!(h.get(&b.id, "physical").tps_samples, 1);
    }
    #[test]
    fn source_views_do_not_borrow_new_target_reference_for_old_candidate() {
        let h = HealthRegistry::new();
        let a = provider();
        h.sync_tps_sources(std::slice::from_ref(&a));
        let old = h.tps_lease(&a, "physical").unwrap();
        for _ in 0..5 {
            sample(&h, &a, Some(&old));
        }
        let mut b = a.clone();
        b.base_url = "http://127.0.0.1:2/v1".into();
        h.sync_tps_sources(std::slice::from_ref(&b));
        assert!(h.tps_lease(&a, "physical").is_none());
        let new = h.tps_lease(&b, "physical").unwrap();
        for _ in 0..5 {
            sample(&h, &b, Some(&new));
        }
        sample(&h, &a, Some(&old));
        assert_eq!(h.get_for_source(&a, "physical").tps_samples, 0);
        assert_eq!(h.get_for_source(&b, "physical").tps_samples, 5);
        assert_eq!(h.get(&b.id, "physical").tps_samples, 5);
    }
    #[test]
    fn source_aba_delete_recreate_and_new_registry_do_not_restore_old_owner() {
        let h = HealthRegistry::new();
        let a = provider();
        h.sync_tps_sources(std::slice::from_ref(&a));
        let old = h.tps_lease(&a, "physical").unwrap();
        sample(&h, &a, Some(&old));
        let mut b = a.clone();
        b.base_url = "http://127.0.0.1:2/v1".into();
        h.sync_tps_sources(&[b]);
        h.sync_tps_sources(std::slice::from_ref(&a));
        sample(&h, &a, Some(&old));
        assert_eq!(h.get(&a.id, "physical").tps_samples, 0);
        let current = h.tps_lease(&a, "physical").unwrap();
        h.sync_tps_sources(&[]);
        h.sync_tps_sources(std::slice::from_ref(&a));
        sample(&h, &a, Some(&current));
        assert_eq!(h.get(&a.id, "physical").tps_samples, 0);
        let fresh = HealthRegistry::new();
        fresh.sync_tps_sources(std::slice::from_ref(&a));
        sample(&fresh, &a, Some(&old));
        assert_eq!(fresh.get(&a.id, "physical").tps_samples, 0);
        let current = fresh.tps_lease(&a, "physical").unwrap();
        sample(&fresh, &a, Some(&current));
        assert_eq!(fresh.get(&a.id, "physical").tps_samples, 1);
    }
    #[test]
    fn aliases_share_physical_source_but_ambiguous_legacy_key_has_no_owner() {
        let h = HealthRegistry::new();
        let mut a = provider();
        h.sync_tps_sources(std::slice::from_ref(&a));
        let owner = h.tps_lease(&a, "physical").unwrap();
        sample(&h, &a, Some(&owner));
        let mut alias = a.models[0].clone();
        alias.alias = "sibling".into();
        a.models.push(alias);
        a.note = Some("owned note".into());
        h.sync_tps_sources(std::slice::from_ref(&a));
        sample(&h, &a, Some(&owner));
        assert_eq!(h.get(&a.id, "physical").tps_samples, 2);
        let mut b = a.clone();
        a.id = "owned::part".into();
        b.id = "owned".into();
        for m in &mut b.models {
            m.upstream = "part::physical".into();
        }
        h.sync_tps_sources(&[a.clone(), b.clone()]);
        assert!(h.tps_lease(&a, "physical").is_none());
        assert!(h.tps_lease(&b, "part::physical").is_none());
        assert_eq!(h.get_for_source(&a, "physical").tps_samples, 0);
        assert_eq!(h.get_for_source(&b, "part::physical").tps_samples, 0);
    }
}
