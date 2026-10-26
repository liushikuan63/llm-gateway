//! Owned metric scheduling on the same context/input/scoring functions used by
//! production ranking and explanation. No request, account or supplier call.
use super::*;
use serde_json::json;

fn owned() -> (Router, Arc<HealthRegistry>, AppConfig, Vec<Candidate>) {
    let health = Arc::new(HealthRegistry::new());
    let router = Router::new(Arc::new(RateLimiter::new()), health.clone());
    let provider: Provider = serde_json::from_value(json!({
        "id":"owned-tps", "name":"owned-tps", "dialect":"openai", "base_url":"http://127.0.0.1:1/v1", "api_key_enc":"", "enabled":true,
        "priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z",
        "models":[{"alias":"a","upstream":"physical-a","context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true},
                  {"alias":"alias-a","upstream":"physical-a","context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true},
                  {"alias":"b","upstream":"physical-b","context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true}]
    })).unwrap();
    let candidates = router.resolve("auto", &[provider]).unwrap();
    let mut cfg = AppConfig::default();
    cfg.cost_routing.enabled = true;
    cfg.cost_routing.efficiency_weight = 1.0;
    cfg.cost_routing.min_efficiency_samples = 5;
    (router, health, cfg, candidates)
}
fn samples(health: &HealthRegistry, model: &str, tokens: u32, count: usize) {
    health.record_success("owned-tps", model, 1000);
    for _ in 0..count {
        health.record_tps("owned-tps", model, tokens, 1000);
    }
}

#[test]
fn same_scoring_context_keeps_range_and_candidate_tps_after_live_completion() {
    let (router, health, cfg, candidates) = owned();
    samples(&health, "physical-a", 10, 5);
    samples(&health, "physical-b", 100, 5);
    let a = candidates.iter().find(|c| c.model.alias == "a").unwrap();
    let alias = candidates
        .iter()
        .find(|c| c.model.alias == "alias-a")
        .unwrap();
    let ctx = router.cost_context(&candidates, &cfg, None, 0, 0);
    let w = Weights::with_cost_routing(
        Weights::for_strategy(RoutingStrategy::Balanced),
        &cfg.cost_routing,
    );
    assert_eq!(ctx.tps_range, Some((10.0, 100.0)));
    let before = router.score_of(a, &w, None, &ctx, TaskDomain::General);
    let default_before = router.score_of(
        a,
        &Weights::for_strategy(RoutingStrategy::Balanced),
        None,
        &ctx,
        TaskDomain::General,
    );
    health.record_tps("owned-tps", "physical-a", 1000, 1000);
    assert_eq!(
        health.get("owned-tps", "physical-a").avg_tps,
        208.0,
        "new real registry sample was recorded"
    );
    let after = router.score_of(a, &w, None, &ctx, TaskDomain::General);
    assert_eq!(
        after.to_bits(),
        before.to_bits(),
        "an in-flight completion cannot change the same sort/explain scoring context"
    );
    assert_eq!(ctx.candidate_tps(a), Some(10.0));
    assert_eq!(
        ctx.candidate_tps(alias),
        Some(10.0),
        "aliases share the captured physical source"
    );
    let fresh = router.cost_context(&candidates, &cfg, None, 0, 0);
    assert_eq!(fresh.candidate_tps(a), Some(208.0));
    assert_eq!(fresh.tps_range, Some((100.0, 208.0)));
    assert_ne!(
        router
            .score_of(a, &w, None, &fresh, TaskDomain::General)
            .to_bits(),
        before.to_bits(),
        "the next decision must observe the actual new sample"
    );
    assert_eq!(
        router
            .score_of(
                a,
                &Weights::for_strategy(RoutingStrategy::Balanced),
                None,
                &fresh,
                TaskDomain::General
            )
            .to_bits(),
        default_before.to_bits(),
        "default efficiency weight0 preserves exact score"
    );
}

#[test]
fn sample_threshold_crossing_only_enters_the_next_scoring_context() {
    let (router, health, cfg, candidates) = owned();
    samples(&health, "physical-a", 10, 4);
    samples(&health, "physical-b", 100, 5);
    let a = candidates.iter().find(|c| c.model.alias == "a").unwrap();
    let ctx = router.cost_context(&candidates, &cfg, None, 0, 0);
    assert_eq!(ctx.candidate_tps(a), None);
    health.record_tps("owned-tps", "physical-a", 10, 1000);
    assert_eq!(health.get("owned-tps", "physical-a").tps_samples, 5);
    assert_eq!(
        router
            .score_input(a, &ctx, None, TaskDomain::General)
            .candidate_tps,
        None,
        "insufficient capture must stay unknown, never acquire late eligibility"
    );
    let fresh = router.cost_context(&candidates, &cfg, None, 0, 0);
    assert_eq!(fresh.candidate_tps(a), Some(10.0));
    assert_eq!(fresh.tps_range, Some((10.0, 100.0)));
}

#[test]
fn idle_age_context_captures_reference_and_metadata_together() {
    use std::time::{Duration, Instant};
    let (_, _, cfg, candidates) = owned();
    let base = Instant::now();
    let clock = Arc::new(parking_lot::Mutex::new(base));
    let read = clock.clone();
    let h = Arc::new(HealthRegistry::with_tps_clock(Arc::new(move || {
        *read.lock()
    })));
    let router = Router::new(Arc::new(RateLimiter::new()), h.clone());
    let a = &candidates[0];
    samples(&h, &a.model.upstream, 20, 5);
    let captured = router.cost_context(&candidates, &cfg, None, 0, 0);
    assert_eq!(captured.candidate_tps(a), Some(20.0));
    assert_eq!(
        captured.candidate_tps_reference(a).unwrap().state,
        crate::domain::TpsReferenceState::Current
    );
    *clock.lock() = base + Duration::from_secs(86400);
    let next = router.cost_context(&candidates, &cfg, None, 0, 0);
    assert_eq!(next.candidate_tps(a), None);
    assert_eq!(
        next.candidate_tps_reference(a).unwrap().state,
        crate::domain::TpsReferenceState::Expired
    );
    assert_eq!(captured.candidate_tps(a), Some(20.0));
    assert_eq!(
        captured.candidate_tps_reference(a).unwrap().state,
        crate::domain::TpsReferenceState::Current
    );
}

// One real readonly entry crosses the idle boundary between its own captures.
// The stable-clock positive calls use identical providers/configuration.
fn s8_expiring_registry() -> (
    Arc<HealthRegistry>,
    Arc<std::sync::atomic::AtomicBool>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::{Duration, Instant},
    };
    let base = Instant::now();
    let armed = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let flag = armed.clone();
    let seen = reads.clone();
    let h = Arc::new(HealthRegistry::with_tps_clock(Arc::new(move || {
        if flag.load(Ordering::SeqCst) && seen.fetch_add(1, Ordering::SeqCst) >= 2 {
            base + Duration::from_secs(86400)
        } else {
            base
        }
    })));
    (h, armed, reads)
}
#[test]
fn s8_legacy_explanation_keeps_the_tps_snapshot_used_by_its_ranking() {
    use std::sync::atomic::Ordering;
    let (_, _, cfg, candidates) = owned();
    let p = candidates[0].provider.clone();
    let (h, armed, reads) = s8_expiring_registry();
    let router = Router::new(Arc::new(RateLimiter::new()), h.clone());
    samples(&h, "physical-a", 10, 5);
    samples(&h, "physical-b", 100, 5);
    let normal = router
        .explain_candidates(std::slice::from_ref(&p), &cfg, "auto")
        .unwrap();
    assert!(normal.iter().all(|x| x
        .breakdown
        .factors
        .iter()
        .find(|f| f.name == "efficiency")
        .unwrap()
        .raw
        .is_some()));
    armed.store(true, Ordering::SeqCst);
    reads.store(0, Ordering::SeqCst);
    let actual = router
        .explain_candidates(std::slice::from_ref(&p), &cfg, "auto")
        .unwrap();
    eprintln!(
        "owned single legacy explanation TPS reads={} factors={:?}",
        reads.load(Ordering::SeqCst),
        actual
            .iter()
            .map(|x| x
                .breakdown
                .factors
                .iter()
                .find(|f| f.name == "efficiency")
                .unwrap()
                .raw)
            .collect::<Vec<_>>()
    );
    for row in &actual {
        let prior = normal.iter().find(|r| r.model == row.model).unwrap();
        assert_eq!(
            row.breakdown
                .factors
                .iter()
                .find(|f| f.name == "efficiency")
                .unwrap()
                .raw,
            prior
                .breakdown
                .factors
                .iter()
                .find(|f| f.name == "efficiency")
                .unwrap()
                .raw,
            "explanation must use its ranking's captured reference"
        );
        assert_eq!(
            row.tps_reference.as_ref().unwrap().state,
            crate::domain::TpsReferenceState::Current
        );
    }
    let next = router
        .explain_candidates(std::slice::from_ref(&p), &cfg, "auto")
        .unwrap();
    assert!(next.iter().all(|r| r
        .breakdown
        .factors
        .iter()
        .find(|f| f.name == "efficiency")
        .unwrap()
        .raw
        .is_none()));
    assert!(next
        .iter()
        .all(|r| r.tps_reference.as_ref().unwrap().state
            == crate::domain::TpsReferenceState::Expired));
}
#[tokio::test]
async fn s8_actual_gateway_preview_explains_the_same_tps_decision_once() {
    use crate::{
        db::{repo, Db},
        proxy::server::{GatewayState, RequestPreviewInput},
    };
    use std::sync::atomic::Ordering;
    let (_, _, cfg, candidates) = owned();
    let p = candidates[0].provider.clone();
    let (h, armed, reads) = s8_expiring_registry();
    let db = Db::connect_in_memory().await.unwrap();
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    let mut state = GatewayState::new(db, cfg);
    state.health = h.clone();
    state.router = Arc::new(Router::new(state.limiter.clone(), h.clone()));
    state.reload_providers().await.unwrap();
    samples(&h, "physical-a", 10, 5);
    samples(&h, "physical-b", 100, 5);
    let input = || {
        serde_json::from_value::<RequestPreviewInput>(json!({"request":{"model":"auto","messages":[{"role":"user","content":"owned preview"}]},"access_key_id":null,"strategy":null,"include_session":false,"session_id":null})).unwrap()
    };
    let normal = state.preview_routing_request(input()).await.unwrap();
    assert!(normal.rows.iter().all(|r| r.eligible));
    assert!(normal.rows.iter().all(|r| r
        .breakdown
        .as_ref()
        .unwrap()
        .factors
        .iter()
        .find(|f| f.name == "efficiency")
        .unwrap()
        .raw
        .is_some()));
    armed.store(true, Ordering::SeqCst);
    reads.store(0, Ordering::SeqCst);
    let actual = state.preview_routing_request(input()).await.unwrap();
    for row in &actual.rows {
        let prior = normal.rows.iter().find(|r| r.model == row.model).unwrap();
        assert_eq!(row.ordinal, prior.ordinal);
        assert_eq!(
            row.breakdown
                .as_ref()
                .unwrap()
                .factors
                .iter()
                .find(|f| f.name == "efficiency")
                .unwrap()
                .raw,
            prior
                .breakdown
                .as_ref()
                .unwrap()
                .factors
                .iter()
                .find(|f| f.name == "efficiency")
                .unwrap()
                .raw,
            "preview ordinal and factors must share one captured decision"
        );
    }
    let next = state.preview_routing_request(input()).await.unwrap();
    assert!(next.rows.iter().all(|r| r
        .breakdown
        .as_ref()
        .unwrap()
        .factors
        .iter()
        .find(|f| f.name == "efficiency")
        .unwrap()
        .raw
        .is_none()));
}

#[test]
fn s8_v2_observer_preserves_order_and_exact_score_bits_for_fixed_state() {
    use crate::router::{ranking_observation, score};
    for strategy in [
        RoutingStrategy::Priority,
        RoutingStrategy::Balanced,
        RoutingStrategy::Smartest,
        RoutingStrategy::Fastest,
        RoutingStrategy::Reliable,
        RoutingStrategy::Custom,
        RoutingStrategy::Smart,
        RoutingStrategy::Cascade,
    ] {
        let (router, _, mut cfg, mut candidates) = owned();
        cfg.routing_strategy = strategy;
        candidates[0].provider.priority = 3;
        candidates[1].provider.intelligence = 95;
        candidates[2].provider.priority = 0;
        let off = router.rank_with_snapshot(
            candidates.clone(),
            &cfg,
            Default::default(),
            Some(("owned-tps", "physical-a")),
            None,
            0,
            TaskDomain::General,
        );
        let on = router.rank_observed(
            candidates,
            &cfg,
            Default::default(),
            Some(("owned-tps", "physical-a")),
            None,
            0,
            TaskDomain::General,
        );
        assert!(off.observation.is_none());
        assert_eq!(
            off.candidates
                .iter()
                .map(ranking_observation::identity)
                .collect::<Vec<_>>(),
            on.candidates
                .iter()
                .map(ranking_observation::identity)
                .collect::<Vec<_>>()
        );
        for candidate in &on.candidates {
            let actual = on
                .observation
                .as_ref()
                .unwrap()
                .get(&ranking_observation::identity(candidate))
                .unwrap();
            // Control is the old scalar exit on exactly the same fixed captured
            // context; this assertion catches factor association/rounding drift.
            let input = router.score_input(candidate, &off.cost, off.intent, off.domain);
            assert_eq!(
                actual.total.to_bits(),
                score::score(candidate, &input, &off.weights).to_bits()
            );
        }
    }
}
