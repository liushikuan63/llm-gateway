//! Immutable inherited cost policy. Current configuration can strengthen it but
//! cannot downgrade free requests or their spawned summary/fallback work.
use crate::{
    config::AppConfig,
    db::{repo::catalog, Db},
    domain::Provider,
    error::{GatewayError, Result},
    router::score::Candidate,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestCostPolicy {
    #[default]
    Unrestricted,
    FreeOnly,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::repo,
        domain::{ChatRequest, Provider},
        proxy::upstream::UpstreamClient,
    };
    use axum::{routing::post, Json, Router};
    use serde_json::json;
    use std::{
        sync::atomic::{AtomicI64, AtomicUsize, Ordering},
        task::Poll,
        time::Duration,
    };

    pub(super) fn provider(id: &str, base: String) -> Provider {
        serde_json::from_value(json!({"id":id,"name":id,"base_url":base,"api_key_enc":"","dialect":"openai","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"created_at":"2026-10-18T00:00:00Z","updated_at":"2026-10-18T00:00:00Z","models":[{"alias":"model:free","upstream":"model:free","context_window":32768,"enabled":true,"supports_tools":true,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","upstream_path":null,"price":{"prompt":0,"completion":0,"currency":"usd","source":"manual"},"overrides":null}]})).unwrap()
    }

    #[tokio::test]
    async fn final_wire_wait_refreshes_clock_expired_is_zero_posts_fresh_is_one_unknown_is_zero() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let free_posts = Arc::new(AtomicUsize::new(0));
        let paid_posts = Arc::new(AtomicUsize::new(0));
        let free_count = free_posts.clone();
        let paid_count = paid_posts.clone();
        let app=Router::new().route("/free/v1/chat/completions",post(move|Json(body):Json<serde_json::Value>|{let count=free_count.clone();async move{
            use axum::response::IntoResponse;
            count.fetch_add(1,Ordering::SeqCst);
            if body["stream"]==true {
                let stream=async_stream::stream!{
                    yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"held\"},\"finish_reason\":null}]}\n\n"));
                    std::future::pending::<()>().await;
                };
                return axum::http::Response::builder().header("content-type","text/event-stream").body(axum::body::Body::from_stream(stream)).unwrap().into_response();
            }
            Json(json!({"id":"owned","choices":[{"message":{"role":"assistant","content":"owned free answer"},"finish_reason":"stop"}]})).into_response()
        }}))
            .route("/unknown/v1/chat/completions",post(move||{let count=paid_count.clone();async move{count.fetch_add(1,Ordering::SeqCst);Json(json!({"choices":[]}))}}));
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        struct Cleanup(tokio::task::JoinHandle<()>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _cleanup = Cleanup(task);
        let db = Db::connect_in_memory().await.unwrap();
        let p = provider("free", format!("{origin}/free/v1"));
        let unknown = provider("unknown", format!("{origin}/unknown/v1"));
        repo::upsert_provider(db.pool(), &p).await.unwrap();
        repo::upsert_provider(db.pool(), &unknown).await.unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let observation=crate::model_catalog::observation::decode_openrouter(&json!({"data":[{"id":"model:free","context_length":32768,"architecture":{"input_modalities":["text"],"output_modalities":["text"]},"pricing":{"prompt":"0","completion":"0","request":"0","image":"0","web_search":"0","internal_reasoning":"0","input_cache_read":"0","input_cache_write":"0"}}]})).unwrap();
        sqlx::query("INSERT INTO catalog_inventory(provider_id,upstream_id,target_fingerprint,observed_at,source_contract,fee_status,fee_expires_at,model_json,last_seen_run_id,last_complete_run_id) VALUES('free','model:free',?,?,'openrouter_full_v1','free',?,?,'owned','owned')")
            .bind(catalog::target_fingerprint(&p)).bind(now).bind(now+10).bind(serde_json::to_string(&observation.models[0].model).unwrap()).execute(db.pool()).await.unwrap();
        let mut cfg = AppConfig::default();
        cfg.free_routing.enabled = true;
        let clock = Arc::new(AtomicI64::new(now));
        let read_clock = clock.clone();
        let gate = Arc::new(CostGate::with_clock(
            db.clone(),
            Arc::new(parking_lot::RwLock::new(cfg)),
            Arc::new(move || read_clock.load(Ordering::SeqCst)),
        ));
        let client = UpstreamClient::new();
        client.set_cost_gate(gate);
        let request:ChatRequest=serde_json::from_value(json!({"model":"model:free","messages":[{"role":"user","content":"owned"}],"stream":false})).unwrap();
        let defaults = crate::config::OllamaOptionsConfig::default();
        let connection = db.pool().acquire().await.unwrap();
        let mut send = Box::pin(client.call(
            &p,
            &request,
            "model:free",
            Duration::from_secs(2),
            &defaults,
        ));
        assert!(
            matches!(futures_util::poll!(send.as_mut()), Poll::Pending),
            "drive the real wire future until it waits for the held single DB connection"
        );
        clock.store(now + 11, Ordering::SeqCst);
        drop(connection);
        assert!(
            matches!(send.await,Err(GatewayError::FreePolicyRejected{reason}) if reason=="fee_expired_or_future")
        );
        assert_eq!(free_posts.load(Ordering::SeqCst), 0);
        sqlx::query("UPDATE catalog_inventory SET fee_expires_at=? WHERE provider_id='free'")
            .bind(now + catalog::DAY_MS)
            .execute(db.pool())
            .await
            .unwrap();
        client
            .call(
                &p,
                &request,
                "model:free",
                Duration::from_secs(2),
                &defaults,
            )
            .await
            .unwrap();
        assert_eq!(free_posts.load(Ordering::SeqCst), 1);
        assert!(
            matches!(
                client
                    .call(
                        &unknown,
                        &request,
                        "model:free",
                        Duration::from_secs(2),
                        &defaults
                    )
                    .await,
                Err(GatewayError::FreePolicyRejected { .. })
            ),
            "manual numeric zero and a free suffix do not establish a fee fact"
        );
        assert_eq!(paid_posts.load(Ordering::SeqCst), 0);
        let mut streaming = request.clone();
        streaming.stream = true;
        let mut stream = client
            .call_stream(
                &p,
                &streaming,
                "model:free",
                Duration::from_secs(2),
                &defaults,
            )
            .await
            .unwrap();
        use futures_util::StreamExt;
        assert!(
            matches!(stream.next().await,Some(Ok(crate::proxy::upstream::UpstreamEvent::Delta(text))) if text=="held")
        );
        assert_eq!(free_posts.load(Ordering::SeqCst), 2);
        assert!(
            matches!(client.call(&p,&request,"model:free",Duration::from_secs(2),&defaults).await,Err(GatewayError::FreePolicyRejected{reason}) if reason=="free_model_already_inflight")
        );
        assert_eq!(free_posts.load(Ordering::SeqCst), 2);
        drop(stream);
        client
            .call(
                &p,
                &request,
                "model:free",
                Duration::from_secs(2),
                &defaults,
            )
            .await
            .unwrap();
        assert_eq!(free_posts.load(Ordering::SeqCst), 3);
        assert_eq!(paid_posts.load(Ordering::SeqCst), 0);
    }

    fn candidate(p: Provider) -> Candidate {
        Candidate {
            model: p.models[0].clone(),
            provider: p,
            requested_model: "free".into(),
            exact_match: false,
            virtual_strategy: None,
        }
    }

    #[test]
    fn free_weights_keep_small_samples_neutral_and_use_the_same_local_headroom_as_admission() {
        let health = Arc::new(crate::proxy::health::HealthRegistry::new());
        let limiter = Arc::new(crate::router::ratelimit::RateLimiter::new());
        let router = crate::router::Router::new(limiter.clone(), health.clone());
        let mut a = provider("a", "http://127.0.0.1:9/v1".into());
        let mut b = provider("b", "http://127.0.0.1:9/v1".into());
        a.rpm_limit = 10;
        b.rpm_limit = 10;
        for _ in 0..4 {
            health.record_success("a", "model:free", 100);
            health.record_tps("a", "model:free", 10, 100);
            assert!(!FreeSelector::weight(&health.get("a", "model:free"), 1.0).metrics_used);
        }
        assert_eq!(
            FreeSelector::weight(&health.get("a", "model:free"), 1.0).value,
            1.0
        );
        health.record_success("a", "model:free", 100);
        health.record_tps("a", "model:free", 10, 100);
        let full = FreeSelector::weight(
            &health.get("a", "model:free"),
            router.quota_headroom(&a, &a.models[0]),
        );
        assert!(full.metrics_used);
        assert_eq!(full.usable_completion_samples, 5);
        for _ in 0..9 {
            limiter.consume("a::model:free", 0);
        }
        let remaining = router.quota_headroom(&a, &a.models[0]);
        assert!((remaining - 0.1).abs() < 0.001);
        let low = FreeSelector::weight(&health.get("a", "model:free"), remaining);
        assert!(low.value > 0.0 && low.value < full.value);
        limiter.consume("a::model:free", 0);
        assert_eq!(
            FreeSelector::weight(
                &health.get("a", "model:free"),
                router.quota_headroom(&a, &a.models[0])
            )
            .value,
            0.0
        );
        let mut eligible = vec![candidate(a.clone()), candidate(b.clone())];
        FreeSelector::default()
            .select(&mut eligible, &health, &router, Some(("a", "model:free")))
            .unwrap();
        assert_eq!(eligible.len(), 1);
        assert_eq!(
            eligible[0].provider.id, "b",
            "sticky/fair floor cannot resurrect exhausted quota"
        );
        health.record_failure("b", "model:free", &GatewayError::Timeout("owned".into()));
        let ranked = router.rank(
            vec![candidate(a), candidate(b)],
            &AppConfig::default(),
            Default::default(),
            None,
        );
        assert!(
            ranked.is_empty(),
            "zero quota and cooling candidates are hard-filtered before free weighting"
        );
    }

    #[test]
    fn bounded_free_distribution_is_neutral_when_cold_and_tracks_qualified_weights_when_measured() {
        let health = Arc::new(crate::proxy::health::HealthRegistry::new());
        let limiter = Arc::new(crate::router::ratelimit::RateLimiter::new());
        let router = crate::router::Router::new(limiter, health.clone());
        let a = provider("a", "http://127.0.0.1:9/v1".into());
        let b = provider("b", "http://127.0.0.1:9/v1".into());
        let selector = FreeSelector::default();
        health.record_success("a", "model:free", 100);
        health.record_tps("a", "model:free", 10, 100);
        let mut cold = [0, 0];
        for _ in 0..100 {
            let mut choices = vec![candidate(a.clone()), candidate(b.clone())];
            selector
                .select(&mut choices, &health, &router, None)
                .unwrap();
            cold[usize::from(choices[0].provider.id == "b")] += 1;
        }
        assert_eq!(cold, [50, 50], "a single sample remains neutral");
        for _ in 0..5 {
            health.record_success("a", "model:free", 100);
            health.record_tps("a", "model:free", 10, 100);
            health.record_success("b", "model:free", 1000);
            health.record_tps("b", "model:free", 10, 1000);
        }
        let wa = FreeSelector::weight(&health.get("a", "model:free"), 1.0).value;
        let wb = FreeSelector::weight(&health.get("b", "model:free"), 1.0).value;
        let mut measured = [0, 0];
        for _ in 0..400 {
            let mut choices = vec![candidate(a.clone()), candidate(b.clone())];
            selector
                .select(&mut choices, &health, &router, None)
                .unwrap();
            measured[usize::from(choices[0].provider.id == "b")] += 1;
        }
        assert!((f64::from(measured[0]) - 400.0 * wa / (wa + wb)).abs() < 2.0);
        assert!(measured[0] > measured[1] && measured[1] > 0);
    }
}
#[cfg(test)]
mod half_open_tests {
    use super::tests::provider;
    use super::*;
    use axum::{response::IntoResponse, routing::post, Json, Router};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};

    #[tokio::test]
    async fn actual_half_open_user_request_has_one_free_inflight_permit_and_no_probe_or_paid_post()
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let paid = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(true));
        let hold = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (calls, bad, wait, signal, proceed) = (
            hits.clone(),
            fail.clone(),
            hold.clone(),
            entered.clone(),
            release.clone(),
        );
        let paid_calls = paid.clone();
        let app=Router::new().route("/free/v1/chat/completions",post(move||{let(calls,bad,wait,signal,proceed)=(calls.clone(),bad.clone(),wait.clone(),signal.clone(),proceed.clone());async move{
            calls.fetch_add(1,Ordering::SeqCst);signal.notify_one();
            if bad.load(Ordering::SeqCst){return(axum::http::StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":{"message":"owned initial failure"}}))).into_response();}
            if wait.load(Ordering::SeqCst){proceed.acquire().await.unwrap().forget();}
            Json(json!({"id":"owned","model":"model:free","choices":[{"index":0,"message":{"role":"assistant","content":"owned recovery"},"finish_reason":"stop"}]})).into_response()
        }})).route("/paid/v1/chat/completions",post(move||{let calls=paid_calls.clone();async move{calls.fetch_add(1,Ordering::SeqCst);Json(json!({"choices":[]}))}}));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        struct Cleanup(tokio::task::JoinHandle<()>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _cleanup = Cleanup(server);
        let db = Db::connect_in_memory().await.unwrap();
        let p = provider("free", format!("{origin}/free/v1"));
        let mut other = provider("paid", format!("{origin}/paid/v1"));
        other.priority = 0;
        for (p, fee) in [(&p, "free"), (&other, "paid")] {
            crate::db::repo::upsert_provider(db.pool(), p)
                .await
                .unwrap();
            let observation=crate::model_catalog::observation::decode_openrouter(&json!({"data":[{"id":"model:free","context_length":32768,"architecture":{"input_modalities":["text"],"output_modalities":["text"]},"pricing":{"prompt":"0","completion":"0","request":"0","image":"0","web_search":"0","internal_reasoning":"0","input_cache_read":"0","input_cache_write":"0"}}]})).unwrap();
            let now = chrono::Utc::now().timestamp_millis();
            sqlx::query("INSERT INTO catalog_inventory(provider_id,upstream_id,target_fingerprint,observed_at,source_contract,fee_status,fee_expires_at,model_json,last_seen_run_id,last_complete_run_id) VALUES(?,'model:free',?,?,'openrouter_full_v1',?,?,?,'owned','owned')")
                .bind(&p.id).bind(catalog::target_fingerprint(p)).bind(now).bind(fee).bind(now+catalog::DAY_MS).bind(serde_json::to_string(&observation.models[0].model).unwrap()).execute(db.pool()).await.unwrap();
        }
        let clock = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp()));
        let read = clock.clone();
        let health = Arc::new(crate::proxy::health::HealthRegistry::with_clock(Arc::new(
            move || read.load(Ordering::SeqCst),
        )));
        let cfg = AppConfig {
            port: 0,
            unified_key: "owned-half-open-key".into(),
            ..Default::default()
        };
        let mut gateway = crate::proxy::server::GatewayState::new(db, cfg);
        gateway.health = health.clone();
        gateway.router = Arc::new(crate::router::Router::new(
            gateway.limiter.clone(),
            health.clone(),
        ));
        let gateway = Arc::new(gateway);
        gateway.reload_providers().await.unwrap();
        let service = crate::proxy_control::gateway::GatewayService::new(gateway);
        let started = service.set_enabled(true).await.unwrap();
        let url = format!("http://{}/v1/chat/completions", started.bound_addr.unwrap());
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let send = || {
            let (client, url) = (client.clone(), url.clone());
            async move {
                client.post(url).bearer_auth("owned-half-open-key").json(&json!({"model":"free","messages":[{"role":"user","content":"owned real user request"}]})).send().await.unwrap()
            }
        };
        let response = send().await;
        assert!(!response.status().is_success());
        response.text().await.unwrap();
        entered.notified().await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(!health.is_available("free", "model:free"));
        let response = send().await;
        assert!(!response.status().is_success());
        response.text().await.unwrap();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "cooldown sends no health/model probe"
        );
        let expiry = health.get("free", "model:free").cooldown_until;
        clock.store(expiry, Ordering::SeqCst);
        assert!(health.allow_half_open("free", "model:free"));
        fail.store(false, Ordering::SeqCst);
        hold.store(true, Ordering::SeqCst);
        let active = tokio::spawn(send());
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let denied = send().await;
        assert_eq!(denied.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(denied.headers()[reqwest::header::RETRY_AFTER], "1");
        assert_eq!(
            denied.json::<serde_json::Value>().await.unwrap()["error"]["reason"],
            "free_qualified_models_busy"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        hold.store(false, Ordering::SeqCst);
        release.add_permits(1);
        let recovered = active.await.unwrap();
        assert!(recovered.status().is_success());
        recovered.text().await.unwrap();
        let final_response = send().await;
        assert!(final_response.status().is_success());
        final_response.text().await.unwrap();
        service.set_enabled(false).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        assert_eq!(paid.load(Ordering::SeqCst), 0);
    }
}

impl RequestCostPolicy {
    pub fn resolve(config: &AppConfig, requested: &str) -> Self {
        if config.free_routing.enabled || requested.trim() == "free" {
            Self::FreeOnly
        } else {
            Self::Unrestricted
        }
    }
    pub fn strengthen(self, config: &AppConfig) -> Self {
        if self == Self::FreeOnly || config.free_routing.enabled {
            Self::FreeOnly
        } else {
            self
        }
    }
    pub fn is_free(self) -> bool {
        self == Self::FreeOnly
    }
}
pub fn rejected(reason: &str) -> GatewayError {
    GatewayError::FreePolicyRejected {
        reason: reason.into(),
    }
}

pub fn route_error(error: GatewayError, policy: RequestCostPolicy) -> GatewayError {
    if policy.is_free() && matches!(error, GatewayError::ModelNotFound(_)) {
        rejected("no_qualified_free_model; configure a fresh authoritative free catalog")
    } else {
        error
    }
}

pub fn is_busy_error(error: &GatewayError) -> bool {
    matches!(error,GatewayError::FreePolicyRejected{reason} if matches!(reason.as_str(),"free_qualified_models_busy"|"free_model_already_inflight"|"free_execution_capacity_unavailable"))
}

pub fn reason_label(reason: &str) -> &str {
    match reason {
        "catalog_exact_zero" => "可信完整目录确认零费用",
        "fee_unknown" | "empty_provider_fee_unknown" => "没有可靠费用事实",
        "fee_paid_or_unknown" => "收费或费用未知",
        "fee_expired_or_future" => "费用观察过期或时钟异常",
        "account_hard_stop_and_reservation_unverified" => "账号硬停及原子余额预留证据待适配",
        "free_qualified_models_busy" | "free_model_already_inflight" => {
            "合格模型正在执行，稍后重试"
        }
        "model_disabled" | "user_opted_out" => "模型已禁用或移除",
        "model_soft_offline" => "模型已软下线",
        "provider_target_changed_or_disabled" | "fee_target_changed" => "供应商或凭据已变更",
        "fee_source_or_completeness_unverified" => "来源或完整性未确证",
        "fee_use_or_context_unverified" => "用途或上下文未确证",
        "clock_anomaly" => "本地时钟异常",
        "custom_endpoint_or_identity_unverified" => "自定义请求路径或身份头未确证",
        "model_identity_changed_or_missing" | "model_send_configuration_changed" => {
            "模型发送配置已变更"
        }
        "configured_price_unverified" | "configured_price_conflicts_with_free" => {
            "配置价格无效或与零费用事实冲突"
        }
        "wire_model_override_unverified" => "覆盖参数改变了实际模型",
        "billing_extension_unverified" | "hosted_tool_billing_unverified" => {
            "扩展或托管工具的费用未确证"
        }
        _ => reason,
    }
}

/// Used for both configured-overrides preview and the fully assembled wire body.
pub(crate) fn wire_shape_issue(body: &serde_json::Value, model: &str) -> Option<&'static str> {
    if body
        .get("model")
        .is_some_and(|id| id.as_str() != Some(model))
    {
        return Some("wire_model_override_unverified");
    }
    const FIELDS: &[&str] = &[
        "model",
        "messages",
        "input",
        "instructions",
        "temperature",
        "top_p",
        "max_tokens",
        "max_completion_tokens",
        "max_output_tokens",
        "stop",
        "stream",
        "stream_options",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "response_format",
        "reasoning",
        "reasoning_effort",
        "thinking",
        "encoding_format",
        "dimensions",
        "prompt",
        "n",
        "size",
        "quality",
    ];
    if body.as_object().map_or(true, |object| {
        object.keys().any(|key| !FIELDS.contains(&key.as_str()))
    }) {
        return Some("billing_extension_unverified");
    }
    if body.get("tools").is_some_and(|tools| {
        !tools.is_null()
            && tools.as_array().map_or(true, |array| {
                array.iter().any(|tool| {
                    tool.get("type").and_then(serde_json::Value::as_str) != Some("function")
                })
            })
    }) {
        return Some("hosted_tool_billing_unverified");
    }
    None
}

pub struct CostGate {
    pub(crate) accounts: Arc<crate::entitlement::dispatch::DispatchGate>,
    db: Db,
    cfg: Arc<parking_lot::RwLock<AppConfig>>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    permits: parking_lot::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Semaphore>>>,
}
impl CostGate {
    pub fn new(db: Db, cfg: Arc<parking_lot::RwLock<AppConfig>>) -> Self {
        Self::with_clock(db, cfg, Arc::new(|| chrono::Utc::now().timestamp_millis()))
    }
    /// Rust dependency injection only; no request/IPC can select a clock or reader.
    pub fn with_clock(
        db: Db,
        cfg: Arc<parking_lot::RwLock<AppConfig>>,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        let accounts = Arc::new(crate::entitlement::dispatch::DispatchGate::new(
            db.pool().clone(),
        ));
        Self {
            accounts,
            db,
            cfg,
            clock,
            permits: parking_lot::Mutex::new(std::collections::HashMap::new()),
        }
    }
    pub fn effective(&self, inherited: RequestCostPolicy) -> RequestCostPolicy {
        inherited.strengthen(&self.cfg.read())
    }
    pub fn is_inflight(&self, provider_id: &str, model: &str) -> bool {
        self.permits
            .lock()
            .get(&format!("{provider_id}::{model}"))
            .is_some_and(|permit| permit.available_permits() == 0)
    }
    pub async fn filter(
        &self,
        candidates: &mut Vec<Candidate>,
        policy: RequestCostPolicy,
    ) -> Result<()> {
        self.filter_for(candidates, policy, "chat").await
    }
    pub(crate) async fn filter_for(
        &self,
        candidates: &mut Vec<Candidate>,
        policy: RequestCostPolicy,
        purpose: &str,
    ) -> Result<()> {
        if !self.effective(policy).is_free() {
            return Ok(());
        }
        let mut allowed = Vec::new();
        let mut physical = std::collections::HashSet::new();
        let mut qualified_busy = false;
        for mut candidate in candidates.drain(..) {
            if candidate.requested_model.trim() == "free"
                && candidate.provider.models.iter().any(|m| m.alias == "free")
            {
                return Err(rejected(
                    "reserved_free_name_conflict; use a provider-qualified alias or rename it",
                ));
            }
            let key = format!("{}::{}", candidate.provider.id, candidate.model.upstream);
            let busy = {
                self.permits
                    .lock()
                    .get(&key)
                    .is_some_and(|permit| permit.available_permits() == 0)
            };
            let eligible = if candidate.provider.runtime_id.is_some() {
                self.accounts
                    .eligible(&candidate.provider, &candidate.model.upstream, purpose)
                    .await?
            } else {
                catalog::free_eligibility_checked(
                    self.db.pool(),
                    &candidate.provider,
                    &candidate.model,
                    || (self.clock)(),
                )
                .await?
                .eligible
            };
            if eligible {
                if busy {
                    qualified_busy = true;
                    continue;
                }
                if !physical.insert(key) {
                    continue;
                }
                // Keep the selected alias/overrides exact even when several
                // configured aliases point to one physical upstream ID.
                // Project zero only AFTER the authoritative free decision so
                // audit/budget consumers share it. Stored user pricing is untouched.
                if candidate.provider.runtime_id.is_none() {
                    candidate.model.price = Some(crate::domain::ModelPrice {
                        prompt: 0.0,
                        completion: 0.0,
                        cache_read: Some(0.0),
                        cache_creation: Some(0.0),
                        currency: crate::domain::Currency::Usd,
                        tiers: Vec::new(),
                        rules: Vec::new(),
                        source: crate::domain::PriceSource::Catalog,
                    });
                }
                candidate.provider.models = vec![candidate.model.clone()];
                allowed.push(candidate);
            }
        }
        *candidates = allowed;
        if candidates.is_empty() {
            if qualified_busy {
                return Err(rejected("free_qualified_models_busy"));
            }
            return Err(rejected(
                "no_qualified_free_model; configure a fresh authoritative free catalog",
            ));
        }
        Ok(())
    }
    pub async fn before_wire(
        &self,
        p: &Provider,
        model: &str,
        body: &serde_json::Value,
        inherited: RequestCostPolicy,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>> {
        if !self.effective(inherited).is_free() {
            return Ok(None);
        }
        let semaphore = {
            let mut permits = self.permits.lock();
            if permits.len() >= 4096 {
                permits.retain(|_, value| Arc::strong_count(value) > 1);
            }
            let key = format!("{}::{model}", p.id);
            if !permits.contains_key(&key) && permits.len() >= 4096 {
                return Err(rejected("free_execution_capacity_unavailable"));
            }
            permits
                .entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
                .clone()
        };
        // Separate from legacy RPM accounting. The owned permit lasts through
        // the response body/stream and is released on completion or cancellation.
        let permit = semaphore
            .try_acquire_owned()
            .map_err(|_| rejected("free_model_already_inflight"))?;
        let selected: Vec<_> = p.models.iter().filter(|m| m.upstream == model).collect();
        if selected.len() != 1 {
            return Err(rejected("exact_selected_model_required"));
        }
        if let Some(reason) = wire_shape_issue(body, model) {
            return Err(rejected(reason));
        }
        let decision =
            catalog::free_eligibility_checked(self.db.pool(), p, selected[0], || (self.clock)())
                .await?;
        if !decision.eligible {
            return Err(rejected(&decision.reason));
        }
        Ok(Some(permit))
    }
}

#[derive(Default)]
pub struct FreeSelector {
    credits: parking_lot::Mutex<std::collections::HashMap<String, f64>>,
}
pub const FREE_MIN_USABLE_SAMPLES: u32 = 5;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreeWeight {
    pub value: f64,
    pub usable_completion_samples: u32,
    pub minimum_samples: u32,
    pub metrics_used: bool,
    pub local_quota_headroom: f32,
}
impl FreeSelector {
    pub fn weight(health: &crate::domain::ProviderHealth, headroom: f32) -> FreeWeight {
        // tps_samples requires positive completion usage, nonzero elapsed time
        // and plausible throughput. Missing/one-off data keeps a neutral factor.
        let metrics_used =
            health.tps_samples >= FREE_MIN_USABLE_SAMPLES && health.avg_latency_ms > 0;
        let metric = if metrics_used {
            let success = f64::from(health.success_rate).clamp(0.0, 1.0);
            let latency = 1000.0 / (1000.0 + f64::from(health.avg_latency_ms));
            0.5 + 1.5 * success * latency
        } else {
            1.0
        };
        let headroom = if headroom.is_finite() {
            headroom.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let value = if headroom == 0.0 {
            0.0
        } else {
            metric * (0.25 + 0.75 * f64::from(headroom))
        };
        FreeWeight {
            value,
            usable_completion_samples: health.tps_samples,
            minimum_samples: FREE_MIN_USABLE_SAMPLES,
            metrics_used,
            local_quota_headroom: headroom,
        }
    }
    /// Smooth weighted turns only after the existing hard health/context/quota
    /// filters. A positive floor never restores a rejected or cooling model.
    pub fn select(
        &self,
        candidates: &mut Vec<Candidate>,
        health: &crate::proxy::health::HealthRegistry,
        router: &crate::router::Router,
        sticky: Option<(&str, &str)>,
    ) -> Result<()> {
        candidates.retain(|c| router.quota_headroom(&c.provider, &c.model) > 0.0);
        if candidates.is_empty() {
            return Err(rejected("no_qualified_free_model_with_local_quota"));
        }
        if candidates.len() > 4096 {
            return Err(rejected("free_candidate_capacity_exceeded"));
        }
        if let Some((pid, model)) = sticky {
            if let Some(at) = candidates
                .iter()
                .position(|c| c.provider.id == pid && c.model.upstream == model)
            {
                let candidate = candidates.remove(at);
                candidates.insert(0, candidate);
                return Ok(());
            }
        }
        let mut credits = self.credits.lock();
        let mut keys = std::collections::HashSet::new();
        let mut total = 0.0;
        let mut best = None;
        for (index, candidate) in candidates.iter().enumerate() {
            let key = format!("{}::{}", candidate.provider.id, candidate.model.upstream);
            if !keys.insert(key.clone()) {
                continue;
            }
            let weight = Self::weight(
                &health.get(&candidate.provider.id, &candidate.model.upstream),
                router.quota_headroom(&candidate.provider, &candidate.model),
            )
            .value;
            total += weight;
            let credit = credits.entry(key.clone()).or_insert(0.0);
            *credit += weight;
            if best
                .as_ref()
                .map_or(true, |(_, _, current): &(usize, String, f64)| {
                    *credit > *current
                })
            {
                best = Some((index, key, *credit));
            }
        }
        credits.retain(|key, _| keys.contains(key));
        if let Some((index, key, _)) = best {
            *credits.get_mut(&key).expect("selected key exists") -= total;
            let selected = candidates.remove(index);
            candidates.insert(0, selected);
        }
        Ok(())
    }
}
