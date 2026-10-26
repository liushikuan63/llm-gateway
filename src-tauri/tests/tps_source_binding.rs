//! Known-config TPS ownership through production Gateway and owned HTTP only.
use axum::{extract::State, routing::post, Json, Router};
use llm_gateway_lib::{
    config::AppConfig,
    db::{repo, Db},
    domain::Provider,
    proxy::server::GatewayState,
    proxy_control::gateway::GatewayService,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

struct Upstream {
    posts: AtomicUsize,
    hold: AtomicBool,
    entered: Notify,
    release: Notify,
    label: &'static str,
}
struct OwnedUpstream {
    state: Arc<Upstream>,
    origin: String,
    task: JoinHandle<()>,
}
impl Drop for OwnedUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn model(State(s): State<Arc<Upstream>>, Json(input): Json<Value>) -> Json<Value> {
    assert!(matches!(
        input["model"].as_str(),
        Some("physical" | "default")
    ));
    s.posts.fetch_add(1, Ordering::SeqCst);
    if s.hold.swap(false, Ordering::SeqCst) {
        s.entered.notify_one();
        s.release.notified().await;
    }
    // A real positive, bounded elapsed time keeps the existing >0ms TPS rule.
    tokio::time::sleep(Duration::from_millis(20)).await;
    Json(
        json!({"id":"owned-tps","object":"chat.completion","model":"physical",
        "choices":[{"index":0,"message":{"role":"assistant","content":s.label},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":1,"completion_tokens":8,"total_tokens":9}}),
    )
}
impl OwnedUpstream {
    async fn new(label: &'static str) -> Self {
        let state = Arc::new(Upstream {
            posts: AtomicUsize::new(0),
            hold: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
            label,
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/chat/completions", post(model))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            state,
            origin,
            task,
        }
    }
}
struct Owned {
    a: OwnedUpstream,
    b: OwnedUpstream,
    db: Db,
    provider: Provider,
    state: Arc<GatewayState>,
    service: GatewayService,
    base: String,
}
impl Owned {
    async fn new() -> Self {
        let a = OwnedUpstream::new("owned-A").await;
        let b = OwnedUpstream::new("owned-B").await;
        let db = Db::connect_in_memory().await.unwrap();
        let provider: Provider = serde_json::from_value(json!({"id":"owned-tps-source","name":"owned-tps-source",
            "base_url":a.origin,"api_key_enc":"","dialect":"openai","enabled":true,"priority":1,"rpm_limit":0,
            "intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z",
            "models":[{"alias":"public","upstream":"physical","enabled":true,"context_window":32768,
                "supports_tools":false,"supports_vision":false,"supports_stream":true,"model_type":"chat","price":null}]})).unwrap();
        repo::upsert_provider(db.pool(), &provider).await.unwrap();
        let mut cfg = AppConfig {
            port: 0,
            unified_key: "owned-tps-gateway-key".into(),
            http_proxy: None,
            ..Default::default()
        };
        cfg.cache.enabled = false;
        cfg.search.enabled = false;
        cfg.smart_routing.enabled = false;
        cfg.failover_enabled = false;
        cfg.dedup.mode = llm_gateway_lib::proxy::dedup::DedupMode::Off;
        let state = Arc::new(GatewayState::new(db.clone(), cfg));
        state.reload_providers().await.unwrap();
        let service = GatewayService::new(state.clone());
        let started = service.set_enabled(true).await.unwrap();
        let base = format!("http://{}", started.bound_addr.unwrap());
        Self {
            a,
            b,
            db,
            provider,
            state,
            service,
            base,
        }
    }
    async fn call_at(base: String, text: String) -> Value {
        Self::call_model_at(base, "public".into(), text).await
    }
    async fn call_model_at(base: String, selected: String, text: String) -> Value {
        let response = reqwest::Client::builder().no_proxy().build().unwrap()
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth("owned-tps-gateway-key")
            .json(&json!({"model":selected,"stream":false,"messages":[{"role":"user","content":text}]}))
            .send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        response.json().await.unwrap()
    }
    async fn call(&self, text: &str) -> Value {
        Self::call_at(self.base.clone(), text.into()).await
    }
    async fn warm(&self) {
        for i in 0..5 {
            let body = self.call(&format!("owned-warm-{i}")).await;
            assert_eq!(body["choices"][0]["message"]["content"], "owned-A");
        }
        let health = self.state.health.get(&self.provider.id, "physical");
        assert_eq!(health.tps_samples, 5);
        assert!(health.avg_tps > 0.0);
        assert_eq!(self.a.state.posts.load(Ordering::SeqCst), 5);
        assert_eq!(self.b.state.posts.load(Ordering::SeqCst), 0);
    }
    async fn publish(&self, provider: &Provider) {
        repo::upsert_provider_config(self.db.pool(), provider)
            .await
            .unwrap();
        self.state.reload_providers().await.unwrap();
    }
}

#[tokio::test]
async fn known_endpoint_change_retires_old_tps_but_note_only_keeps_it() {
    let f = Owned::new().await;
    f.warm().await;
    let mut provider = f.provider.clone();
    provider.note = Some("owned note only".into());
    f.publish(&provider).await;
    assert_eq!(f.state.health.get(&provider.id, "physical").tps_samples, 5);
    provider.models[0].price = Some(
        serde_json::from_value(json!({
            "prompt":2.0,"completion":4.0,"currency":"usd"
        }))
        .unwrap(),
    );
    f.publish(&provider).await;
    assert_eq!(f.state.health.get(&provider.id, "physical").tps_samples, 5);
    provider.base_url = f.b.origin.clone();
    f.publish(&provider).await;
    let after = f.state.health.get(&provider.id, "physical");
    eprintln!(
        "owned target replacement: A_POST=5 B_POST=0 old_samples=5 current_samples={}",
        after.tps_samples
    );
    assert_eq!(
        after.tps_samples, 0,
        "new HTTP source cannot borrow old source TPS"
    );
    assert_eq!(after.avg_tps, 0.0);
    let body = f.call("owned-new-source").await;
    assert_eq!(body["choices"][0]["message"]["content"], "owned-B");
    assert_eq!(f.state.health.get(&provider.id, "physical").tps_samples, 1);
    assert_eq!(f.a.state.posts.load(Ordering::SeqCst), 5);
    assert_eq!(f.b.state.posts.load(Ordering::SeqCst), 1);
    f.service.set_enabled(false).await.unwrap();
}

#[tokio::test]
async fn delete_recreate_same_target_does_not_revive_dispatched_metric_owner() {
    let f = Owned::new().await;
    f.warm().await;
    f.a.state.hold.store(true, Ordering::SeqCst);
    let old = tokio::spawn(Owned::call_at(f.base.clone(), "owned-before-delete".into()));
    tokio::time::timeout(Duration::from_secs(5), f.a.state.entered.notified())
        .await
        .unwrap();
    repo::delete_provider(f.db.pool(), &f.provider.id)
        .await
        .unwrap();
    f.state.reload_providers().await.unwrap();
    f.publish(&f.provider).await;
    let new = f.call("owned-after-recreate").await;
    assert_eq!(new["choices"][0]["message"]["content"], "owned-A");
    f.a.state.release.notify_one();
    assert_eq!(
        old.await.unwrap()["choices"][0]["message"]["content"],
        "owned-A"
    );
    assert_eq!(
        f.state.health.get(&f.provider.id, "physical").tps_samples,
        1
    );
    assert_eq!(f.a.state.posts.load(Ordering::SeqCst), 7);
    assert_eq!(f.b.state.posts.load(Ordering::SeqCst), 0);
    f.service.set_enabled(false).await.unwrap();
}

#[tokio::test]
async fn changed_owned_credential_configuration_retires_old_reference_without_probe() {
    let f = Owned::new().await;
    f.warm().await;
    let mut provider = f.provider.clone();
    provider.api_key_enc = llm_gateway_lib::crypto::encrypt("owned-fixture-second-key").unwrap();
    f.publish(&provider).await;
    assert_eq!(f.state.health.get(&provider.id, "physical").tps_samples, 0);
    assert_eq!(f.a.state.posts.load(Ordering::SeqCst), 5);
    assert_eq!(f.b.state.posts.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.call("owned-new-credential").await["choices"][0]["message"]["content"],
        "owned-A"
    );
    assert_eq!(f.state.health.get(&provider.id, "physical").tps_samples, 1);
    assert_eq!(f.a.state.posts.load(Ordering::SeqCst), 6);
    f.service.set_enabled(false).await.unwrap();
}

#[tokio::test]
async fn dispatched_old_source_completes_but_cannot_update_new_source_tps() {
    let f = Owned::new().await;
    f.warm().await;
    f.a.state.hold.store(true, Ordering::SeqCst);
    let old = tokio::spawn(Owned::call_at(f.base.clone(), "owned-held-old".into()));
    tokio::time::timeout(Duration::from_secs(5), f.a.state.entered.notified())
        .await
        .unwrap();
    let mut provider = f.provider.clone();
    provider.base_url = f.b.origin.clone();
    f.publish(&provider).await;
    let new = f.call("owned-new-while-old-held").await;
    assert_eq!(new["choices"][0]["message"]["content"], "owned-B");
    f.a.state.release.notify_one();
    let old = old.await.unwrap();
    assert_eq!(old["choices"][0]["message"]["content"], "owned-A");
    let samples = f.state.health.get(&provider.id, "physical").tps_samples;
    eprintln!(
        "owned late completion: A_POST={} B_POST={} current_samples={samples}",
        f.a.state.posts.load(Ordering::SeqCst),
        f.b.state.posts.load(Ordering::SeqCst)
    );
    assert_eq!(
        samples, 1,
        "old dispatched completion cannot contaminate new source"
    );
    assert_eq!(f.a.state.posts.load(Ordering::SeqCst), 6);
    assert_eq!(f.b.state.posts.load(Ordering::SeqCst), 1);
    f.service.set_enabled(false).await.unwrap();
}

#[tokio::test]
async fn empty_model_provider_keeps_legacy_default_target_tps_sampling() {
    let f = Owned::new().await;
    let mut provider = f.provider.clone();
    provider.models.clear();
    f.publish(&provider).await;
    for i in 0..5 {
        let body =
            Owned::call_model_at(f.base.clone(), "auto".into(), format!("owned-default-{i}")).await;
        assert_eq!(body["choices"][0]["message"]["content"], "owned-A");
    }
    let samples = f.state.health.get(&provider.id, "default").tps_samples;
    eprintln!("owned implicit target: A_POST=5 B_POST=0 default_samples={samples}");
    assert_eq!(f.a.state.posts.load(Ordering::SeqCst), 5);
    assert_eq!(f.b.state.posts.load(Ordering::SeqCst), 0);
    assert_eq!(
        samples, 5,
        "existing implicit default model must keep valid TPS sampling"
    );
    f.service.set_enabled(false).await.unwrap();
}

#[tokio::test]
async fn pareto_old_provider_snapshot_cannot_borrow_new_target_speed() {
    let f = Owned::new().await;
    f.warm().await;
    let mut current = f.provider.clone();
    current.base_url = f.b.origin.clone();
    f.publish(&current).await;
    for i in 0..5 {
        assert_eq!(
            f.call(&format!("owned-new-pareto-{i}")).await["choices"][0]["message"]["content"],
            "owned-B"
        );
    }
    let cfg = f.state.cfg_snapshot();
    let new = f
        .state
        .router
        .pareto_view(std::slice::from_ref(&current), &cfg, 0);
    let old = f
        .state
        .router
        .pareto_view(std::slice::from_ref(&f.provider), &cfg, 0);
    assert!(new.points[0].speed.is_some());
    eprintln!(
        "owned Pareto sources: old_speed={:?} current_speed={:?}",
        old.points[0].speed, new.points[0].speed
    );
    assert!(
        old.points[0].speed.is_none(),
        "old configured endpoint cannot borrow current endpoint TPS in comparison"
    );
    assert_eq!(f.a.state.posts.load(Ordering::SeqCst), 5);
    assert_eq!(f.b.state.posts.load(Ordering::SeqCst), 5);
    f.service.set_enabled(false).await.unwrap();
}
