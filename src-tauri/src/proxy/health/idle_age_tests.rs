//! Owned fake-clock regression; no wall clock sleeps or external requests.
use super::*;
use parking_lot::Mutex;
use std::{sync::Arc, time::Duration};

fn fixture() -> (HealthRegistry, Arc<Mutex<Instant>>, Instant) {
    let base = Instant::now();
    let clock = Arc::new(Mutex::new(base));
    let read = clock.clone();
    let h = HealthRegistry::with_tps_clock(Arc::new(move || *read.lock()));
    (h, clock, base)
}
fn warm(h: &HealthRegistry) {
    for _ in 0..5 {
        record_success_with_throughput(h, "owned", "physical", 1000, 20);
    }
}
#[test]
fn idle_age_at_boundary_is_unknown_and_new_sample_restarts_cohort() {
    let (h, clock, base) = fixture();
    warm(&h);
    assert_eq!(h.get("owned", "physical").tps_samples, 5);
    *clock.lock() = base + Duration::from_secs(86399);
    assert_eq!(h.get("owned", "physical").avg_tps, 20.0);
    *clock.lock() = base + Duration::from_secs(86400);
    assert_eq!(
        h.get("owned", "physical").tps_samples,
        0,
        "expired reference must be unknown"
    );
    record_success_with_throughput(&h, "owned", "physical", 1000, 80);
    let new = h.get("owned", "physical");
    assert_eq!(new.tps_samples, 1);
    assert_eq!(new.avg_tps, 80.0, "new cohort must not inherit old EWMA");
}
#[test]
fn idle_age_health_and_invalid_samples_never_refresh_reference() {
    let (h, clock, base) = fixture();
    warm(&h);
    *clock.lock() = base + Duration::from_secs(86399);
    h.record_success("owned", "physical", 1000);
    h.record_tps("owned", "physical", 0, 1000);
    h.record_tps("owned", "physical", 1, 0);
    h.record_tps("owned", "physical", 2001, 1000);
    assert_eq!(h.get("owned", "physical").tps_samples, 5);
    *clock.lock() = base + Duration::from_secs(86400);
    assert_eq!(
        h.get("owned", "physical").tps_samples,
        0,
        "health timestamp cannot extend TPS lifetime"
    );
}

fn owned_provider() -> crate::domain::Provider {
    serde_json::from_value(serde_json::json!({"id":"owned", "name":"owned", "base_url":"http://127.0.0.1:1/v1",
        "api_key_enc":"", "dialect":"openai", "enabled":true, "priority":1,"rpm_limit":0,"intelligence":50,
        "runtime_id":null,"created_at":"2026-10-21T00:00:00Z","updated_at":"2026-10-21T00:00:00Z",
        "models":[{"alias":"public","upstream":"physical","context_window":32768,"supports_stream":true,
        "supports_tools":false,"supports_vision":false}]})).unwrap()
}
#[test]
fn idle_age_is_idle_not_rolling_and_clock_unknown_is_not_fresh() {
    let (h, clock, base) = fixture();
    assert_eq!(
        h.get("owned", "physical").tps_reference.unwrap().state,
        TpsReferenceState::NoSample
    );
    warm(&h);
    *clock.lock() = base + Duration::from_secs(86399);
    h.record_tps("owned", "physical", 40, 1000);
    *clock.lock() = base + Duration::from_secs(86400);
    let current = h.get("owned", "physical");
    assert_eq!(
        current.tps_samples, 6,
        "active cohort is not a rolling24h count"
    );
    assert_eq!(current.avg_tps, 24.0, "existing EWMA remains0.8/0.2");
    assert_eq!(current.tps_reference.unwrap().age_seconds, Some(1));
    *clock.lock() = base;
    let unknown = h.get("owned", "physical");
    assert_eq!(unknown.tps_samples, 0);
    assert_eq!(
        unknown.tps_reference.unwrap().state,
        TpsReferenceState::ClockUnknown
    );
    h.record_tps("owned", "physical", 60, 1000);
    assert_eq!(h.get("owned", "physical").tps_samples, 1);
    assert_eq!(h.get("owned", "physical").avg_tps, 60.0);
    let fresh = HealthRegistry::new();
    assert_eq!(
        fresh.get("owned", "physical").tps_reference.unwrap().state,
        TpsReferenceState::NoSample
    );
}
#[test]
fn idle_age_same_source_saves_do_not_refresh_and_retired_source_discloses_no_count() {
    let (h, clock, base) = fixture();
    let a = owned_provider();
    h.sync_tps_sources(std::slice::from_ref(&a));
    let owner = h.tps_lease(&a, "physical").unwrap();
    h.record_bound_success(Some(&owner), 1000, Some(20));
    *clock.lock() = base + Duration::from_secs(86399);
    let mut saved = a.clone();
    saved.note = Some("owned note".into());
    saved.models[0].price = Some(
        serde_json::from_value(serde_json::json!({"prompt":1,"completion":2,"currency":"usd"}))
            .unwrap(),
    );
    h.sync_tps_sources(std::slice::from_ref(&saved));
    h.record_bound_success(Some(&owner), 1000, None);
    h.record_failure(
        &a.id,
        "physical",
        &crate::error::GatewayError::Timeout("owned".into()),
    );
    h.reset(&a.id, "physical");
    *clock.lock() = base + Duration::from_secs(86400);
    assert_eq!(
        h.get_for_source(&saved, "physical")
            .tps_reference
            .unwrap()
            .state,
        TpsReferenceState::Expired
    );
    let snapshot = h.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].tps_samples, 0);
    assert_eq!(
        snapshot[0].tps_reference.as_ref().unwrap().age_seconds,
        Some(86400)
    );
    saved.base_url = "http://127.0.0.1:2/v1".into();
    h.sync_tps_sources(std::slice::from_ref(&saved));
    h.record_bound_success(Some(&owner), 1000, Some(200));
    let mismatch = h.get_for_source(&a, "physical").tps_reference.unwrap();
    assert_eq!(mismatch.state, TpsReferenceState::SourceMismatch);
    assert_eq!(mismatch.cohort_sample_count, None);
    assert_eq!(mismatch.age_seconds, None);
    let current = h.get_for_source(&saved, "physical").tps_reference.unwrap();
    assert_eq!(current.state, TpsReferenceState::NoSample);
    assert_eq!(current.age_seconds, None);
}
#[test]
fn idle_age_concurrent_first_valid_commits_reset_once_with_current_owner() {
    let (h, clock, base) = fixture();
    let h = Arc::new(h);
    let p = owned_provider();
    h.sync_tps_sources(std::slice::from_ref(&p));
    let owner = h.tps_lease(&p, "physical").unwrap();
    for _ in 0..5 {
        h.record_bound_success(Some(&owner), 1000, Some(20));
    }
    *clock.lock() = base + Duration::from_secs(86400);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let h = h.clone();
        let owner = owner.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            h.record_bound_tps(Some(&owner), 80, 1000);
        }));
    }
    barrier.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    let view = h.get_for_source(&p, "physical");
    assert_eq!(view.tps_samples, 2);
    assert_eq!(view.avg_tps, 80.0);
    let meta = view.tps_reference.unwrap();
    assert_eq!(meta.state, TpsReferenceState::Current);
    assert_eq!(meta.age_seconds, Some(0));
    assert_eq!(meta.cohort_sample_count, Some(2));
}
#[test]
fn idle_age_readonly_metadata_old_pareto_dto_is_compatible() {
    let legacy = serde_json::json!({"id":"owned/public","label":"owned","quality":null,"speed":null,"unit_price":null,"currency":null});
    let point: crate::router::pareto::ParetoPointView =
        serde_json::from_value(legacy.clone()).unwrap();
    assert!(point.speed_reference.is_none());
    assert_eq!(serde_json::to_value(point).unwrap(), legacy);
    let (h, _, _) = fixture();
    let value = serde_json::to_value(h.get("owned", "physical")).unwrap();
    assert_eq!(value["tps_reference"]["schema_version"], 1);
    assert_eq!(value["tps_reference"]["max_idle_seconds"], 86400);
    assert!(value["tps_reference"].get("fingerprint").is_none());
    assert!(value["tps_reference"].get("key").is_none());
}

#[tokio::test]
async fn idle_age_owned_gateway_cache_ranking_pareto_and_new_cohort() {
    use crate::{
        config::AppConfig,
        db::{repo, Db},
        proxy::server::GatewayState,
        proxy_control::gateway::GatewayService,
        router::{ratelimit::RateLimiter, Router as GatewayRouter},
    };
    use axum::{extract::State, routing::post, Json, Router};
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let posts = Arc::new(AtomicUsize::new(0));
    let upstream=Router::new().route("/v1/chat/completions",post(|State(posts):State<Arc<AtomicUsize>>| async move {
        posts.fetch_add(1,Ordering::SeqCst);tokio::time::sleep(Duration::from_millis(20)).await;
        Json(json!({"id":"owned-idle","model":"physical","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"owned"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":8,"total_tokens":9}}))
    })).with_state(posts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}/v1", listener.local_addr().unwrap());
    struct Stop(tokio::task::JoinHandle<()>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _upstream = Stop(tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap()
    }));
    let db = Db::connect_in_memory().await.unwrap();
    let mut p = owned_provider();
    p.base_url = origin;
    repo::upsert_provider(db.pool(), &p).await.unwrap();
    let (h, clock, base) = fixture();
    let h = Arc::new(h);
    let mut cfg = AppConfig {
        port: 0,
        unified_key: "owned-idle-key".into(),
        http_proxy: None,
        ..Default::default()
    };
    cfg.search.enabled = false;
    cfg.smart_routing.enabled = false;
    cfg.failover_enabled = false;
    cfg.cache.enabled = true;
    cfg.dedup.mode = crate::proxy::dedup::DedupMode::Off;
    cfg.cost_routing.enabled = true;
    cfg.cost_routing.efficiency_weight = 1.0;
    cfg.cost_routing.min_efficiency_samples = 5;
    let mut state = GatewayState::new(db, cfg.clone());
    state.health = h.clone();
    state.router = Arc::new(GatewayRouter::new(Arc::new(RateLimiter::new()), h.clone()));
    let state = Arc::new(state);
    state.reload_providers().await.unwrap();
    let service = GatewayService::new(state.clone());
    let running = service.set_enabled(true).await.unwrap();
    let url = format!("http://{}/v1/chat/completions", running.bound_addr.unwrap());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for i in 0..6 {
        if i == 5 {
            *clock.lock() = base + Duration::from_secs(86399);
        }
        let input = if i == 5 { 4 } else { i };
        let response=client.post(&url).bearer_auth("owned-idle-key").json(&json!({"model":"public","messages":[{"role":"user","content":format!("owned-{input}")}]})).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["choices"][0]["message"]["content"], "owned");
    }
    assert_eq!(
        posts.load(Ordering::SeqCst),
        5,
        "cache hit sends no sixth POST"
    );
    assert_eq!(
        h.get("owned", "physical")
            .tps_reference
            .unwrap()
            .age_seconds,
        Some(86399)
    );
    assert!(state
        .router
        .pareto_view(std::slice::from_ref(&p), &cfg, 0)
        .points[0]
        .speed
        .is_some());
    *clock.lock() = base + Duration::from_secs(86400);
    let expired = state.router.pareto_view(std::slice::from_ref(&p), &cfg, 0);
    assert!(expired.points[0].speed.is_none());
    assert_eq!(
        expired.points[0].speed_reference.as_ref().unwrap().state,
        TpsReferenceState::Expired
    );
    let explanation = state
        .router
        .explain_candidates(std::slice::from_ref(&p), &cfg, "public")
        .unwrap();
    let efficiency = explanation[0]
        .breakdown
        .factors
        .iter()
        .find(|f| f.name == "efficiency")
        .unwrap();
    assert!(efficiency.raw.is_none());
    assert_eq!(
        explanation[0].tps_reference.as_ref().unwrap().state,
        TpsReferenceState::Expired
    );
    let response = client
        .post(&url)
        .bearer_auth("owned-idle-key")
        .json(&json!({"model":"public","messages":[{"role":"user","content":"owned-new-cohort"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _: Value = response.json().await.unwrap();
    assert_eq!(posts.load(Ordering::SeqCst), 6);
    assert_eq!(h.get("owned", "physical").tps_samples, 1);
    assert!(state
        .router
        .pareto_view(std::slice::from_ref(&p), &cfg, 0)
        .points[0]
        .speed
        .is_none());
    cfg.cost_routing.min_efficiency_samples = 1;
    assert!(state
        .router
        .pareto_view(std::slice::from_ref(&p), &cfg, 0)
        .points[0]
        .speed
        .is_some());
    cfg.cost_routing.min_efficiency_samples = 3;
    for _ in 0..2 {
        h.record_tps("owned", "physical", 8, 1000);
    }
    assert!(state
        .router
        .pareto_view(std::slice::from_ref(&p), &cfg, 0)
        .points[0]
        .speed
        .is_some());
    service.set_enabled(false).await.unwrap();
}
