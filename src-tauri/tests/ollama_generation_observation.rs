//! Production Gateway and owned Ollama-shape evidence, never live inference.
use axum::{body::Body, response::IntoResponse, routing::post, Json, Router};
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
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};
struct Owned {
    db: Db,
    service: GatewayService,
    state: Arc<GatewayState>,
    base: String,
    calls: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<Value>>>,
    hold_first: Arc<std::sync::atomic::AtomicBool>,
    first_started: Arc<tokio::sync::Notify>,
    release_first: Arc<tokio::sync::Notify>,
    task: JoinHandle<()>,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Owned {
    async fn new(report: Value, ending: &'static str, cache: bool) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(std::sync::Mutex::new(vec![]));
        let hold_first = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first_started = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn({
            let calls = calls.clone();
            let requests = requests.clone();
            let hold_first = hold_first.clone();
            let first_started = first_started.clone();
            let release_first = release_first.clone();
            async move {
                axum::serve(listener,Router::new().route("/api/chat",post(move |Json(request):Json<Value>|{let calls=calls.clone();let requests=requests.clone();let hold_first=hold_first.clone();let first_started=first_started.clone();let release_first=release_first.clone();let report=report.clone();async move {
   requests.lock().unwrap().push(request.clone());
   if hold_first.swap(false,Ordering::SeqCst) { first_started.notify_one();release_first.notified().await; }
   calls.fetch_add(1,Ordering::SeqCst);let mut final_report=json!({"model":"owned-physical","message":{"role":"assistant","content":"owned"},"done":true,"done_reason":"stop","prompt_eval_count":3});
   for (k,v) in report.as_object().unwrap(){final_report[k]=v.clone();}
   if request["stream"]==true {
    let first=json!({"model":"owned-physical","message":{"role":"assistant","content":"owned"},"done":false});
    let last=match ending {"missing"=>String::new(),"error"=>format!("{}\n",json!({"error":"owned-private-inline-marker","done":true,"eval_count":18,"eval_duration":2000000000u64})),_=>format!("{final_report}\n")};
    return ([("content-type","application/x-ndjson")],Body::from(format!("{first}\r\n{last}"))).into_response();
   }
   Json(final_report).into_response()
  }}))).await.unwrap();
            }
        });
        let db = Db::connect_in_memory().await.unwrap();
        let p:Provider=serde_json::from_value(json!({"id":"owned-ollama","name":"owned-ollama","base_url":origin,"api_key_enc":"","dialect":"ollama","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"owned-generation","upstream":"owned-physical","enabled":true,"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","price":null}]})).unwrap();
        repo::upsert_provider(db.pool(), &p).await.unwrap();
        let mut cfg = AppConfig {
            port: 0,
            unified_key: "owned-generation-key".into(),
            ..Default::default()
        };
        cfg.cache.enabled = cache;
        cfg.failover_enabled = false;
        cfg.search.enabled = false;
        cfg.smart_routing.enabled = false;
        cfg.dedup.mode = llm_gateway_lib::proxy::dedup::DedupMode::Off;
        let state = Arc::new(GatewayState::new(db.clone(), cfg));
        state.reload_providers().await.unwrap();
        let service = GatewayService::new(state.clone());
        let started = service.set_enabled(true).await.unwrap();
        let base = format!("http://{}", started.bound_addr.unwrap());
        Self {
            db,
            service,
            state,
            base,
            calls,
            requests,
            hold_first,
            first_started,
            release_first,
            task,
        }
    }
    async fn send(&self, stream: bool) -> (String, String) {
        let response=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap().post(format!("{}/v1/chat/completions",self.base)).bearer_auth("owned-generation-key").json(&json!({"model":"owned-generation","messages":[{"role":"user","content":"owned"}],"stream":stream,"max_tokens":32})).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let trace = response
            .headers()
            .get("x-trace-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        (trace, response.text().await.unwrap())
    }
    async fn audit(&self, trace: &str) -> (i64, Value) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let row: Option<(i64, Option<String>)> =
                sqlx::query_as("SELECT status,attempts_json FROM requests WHERE trace_id=?")
                    .bind(trace)
                    .fetch_optional(self.db.pool())
                    .await
                    .unwrap();
            if let Some((status, Some(row))) = row {
                return (status, serde_json::from_str(&row).unwrap());
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    async fn stop(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut s = self.service.set_enabled(false).await.unwrap();
        while s.running {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
            s = self.service.status().await;
        }
        assert!(s.last_error.is_none());
        assert!(self.state.listener_snapshot().is_none());
    }
}
fn valid() -> Value {
    json!({"eval_count":18,"eval_duration":2000000000u64,"total_duration":3000000000u64,"load_duration":500000000u64,"prompt_eval_duration":250000000u64})
}
#[tokio::test]
async fn native_generation_read_models_keep_exact_derived_rate_without_rewriting_audit() {
    for stream in [false, true] {
        let h = Owned::new(
            json!({"eval_count":1,"eval_duration":u64::MAX}),
            "complete",
            false,
        )
        .await;
        let (trace, _) = h.send(stream).await;
        let (status, _) = h.audit(&trace).await;
        let raw: String = sqlx::query_scalar("SELECT attempts_json FROM requests WHERE trace_id=?")
            .bind(&trace)
            .fetch_one(h.db.pool())
            .await
            .unwrap();
        let (rows, count) = repo::query_requests(h.db.pool(), &Default::default())
            .await
            .unwrap();
        let recent = repo::recent_requests(h.db.pool(), 5).await.unwrap();
        let exported = repo::query_requests_for_export(h.db.pool(), &Default::default())
            .await
            .unwrap();
        let after: String =
            sqlx::query_scalar("SELECT attempts_json FROM requests WHERE trace_id=?")
                .bind(&trace)
                .fetch_one(h.db.pool())
                .await
                .unwrap();
        let expected = 1e9 / u64::MAX as f64;
        let actual = rows[0].attempts[0]["generation"]["rate_tokens_per_second"]
            .as_f64()
            .unwrap();
        let fixture = json!({"scope":"owned loopback/SQLite production readback only", "stream":stream,
            "status":status,"post_count":h.calls.load(Ordering::SeqCst),"query":rows,"recent":recent,
            "stored_attempts_json":raw,"raw_unchanged":raw==after,
            "expected_bits":expected.to_bits(),"query_bits":actual.to_bits()});
        if let Ok(dir) = std::env::var("LLMGW_OWNED_GENERATION_READBACK_DIR") {
            let dir = std::path::PathBuf::from(dir);
            assert!(
                dir.is_absolute()
                    && dir
                        .to_string_lossy()
                        .replace('\\', "/")
                        .contains("/native-rotation6-v1/s4-generation-readback-v1/audit-ui-")
            );
            let name = if stream {
                "stream.json"
            } else {
                "buffered.json"
            };
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(name))
                .unwrap();
            f.write_all(serde_json::to_string_pretty(&fixture).unwrap().as_bytes())
                .unwrap();
        }
        h.stop().await;
        assert_eq!(status, 200);
        assert_eq!(count, 1);
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            raw, after,
            "readonly projection must not rewrite original audit"
        );
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "valid persisted generation report must remain usable through actual audit query"
        );
        assert_eq!(
            exported[0].attempts[0]["generation"]["rate_tokens_per_second"]
                .as_f64()
                .unwrap()
                .to_bits(),
            expected.to_bits()
        );
        assert_eq!(
            recent[0]["attempts"][0]["generation"]["rate_tokens_per_second"]
                .as_f64()
                .unwrap()
                .to_bits(),
            expected.to_bits()
        );
    }
}
#[tokio::test]
async fn reported_ollama_eval_time_is_persisted_without_substituting_local_elapsed() {
    for stream in [false, true] {
        let h = Owned::new(valid(), "complete", false).await;
        let (trace, body) = h.send(stream).await;
        assert!(body.contains("owned"));
        let (status, attempts) = h.audit(&trace).await;
        assert_eq!(status, 200);
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
        assert_eq!(attempts.as_array().unwrap().len(), 1);
        assert!(attempts[0].get("generation").is_some(),"actual Ollama final report must survive per-attempt audit, not disappear behind local tps");
        let g = &attempts[0]["generation"];
        assert_eq!(g["source"], "ollama_native_eval");
        assert_eq!(g["state"], "reported");
        assert_eq!(g["output_tokens"], 18);
        assert_eq!(g["eval_duration_ns"], "2000000000");
        assert_eq!(g["rate_tokens_per_second"], 9.0);
        assert_eq!(
            g["timing_mode"],
            if stream { "streaming" } else { "buffered" }
        );
        assert_eq!(g["load_duration_ns"], "500000000");
        h.stop().await;
    }
}
#[tokio::test]
async fn missing_invalid_and_zero_reports_keep_wire_compatibility_and_truthful_metric() {
    for stream in [false, true] {
        for (report, count, rate, reason) in [
            (
                json!({"eval_count":0,"eval_duration":2000000000u64}),
                Some(0),
                Some(0.0),
                None,
            ),
            (
                json!({"eval_count":18}),
                Some(18),
                None,
                Some("eval_duration_missing"),
            ),
            (
                json!({"eval_duration":2000000000u64}),
                None,
                None,
                Some("output_tokens_missing"),
            ),
            (
                json!({"eval_count":18,"eval_duration":0}),
                Some(18),
                None,
                Some("eval_duration_zero"),
            ),
            (
                json!({"eval_count":18,"eval_duration":-1}),
                Some(18),
                None,
                Some("eval_duration_invalid"),
            ),
        ] {
            let h = Owned::new(report, "complete", false).await;
            let (trace, body) = h.send(stream).await;
            let (status, a) = h.audit(&trace).await;
            assert_eq!(status, 200);
            let g = &a[0]["generation"];
            assert_eq!(g["output_tokens"], json!(count));
            assert_eq!(g["rate_tokens_per_second"], json!(rate));
            assert_eq!(g["reason"], json!(reason));
            assert_eq!(h.calls.load(Ordering::SeqCst), 1);
            if !stream && count.is_none() {
                let wire: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(
                    wire["usage"]["completion_tokens"], 0,
                    "ordinary legacy wire remains0; private metric must remain unknown"
                );
            }
            h.stop().await;
        }
    }
}
#[tokio::test]
async fn incomplete_error_and_cache_hit_are_not_new_valid_generation_samples() {
    for ending in ["missing", "error"] {
        let h = Owned::new(valid(), ending, false).await;
        let (trace, body) = h.send(true).await;
        let (status, a) = h.audit(&trace).await;
        assert_eq!(status, 502);
        assert!(!body.contains("owned-private-inline-marker"));
        assert_eq!(a[0]["generation"]["state"], "not_completed");
        assert!(a[0]["generation"]["rate_tokens_per_second"].is_null());
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
        h.stop().await;
    }
    let h = Owned::new(valid(), "complete", true).await;
    let (first, _) = h.send(false).await;
    let (second, _) = h.send(false).await;
    let (_, a) = h.audit(&first).await;
    let (_, b) = h.audit(&second).await;
    assert_eq!(a[0]["generation"]["rate_tokens_per_second"], 9.0);
    assert_eq!(
        b,
        json!([]),
        "cache has no new HTTP attempt or inherited generation report"
    );
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    h.stop().await;
}

#[tokio::test]
async fn final_send_observation_context_changes_with_actual_bound_config_not_model_name() {
    for stream in [false, true] {
        let h = Owned::new(valid(), "complete", false).await;
        h.hold_first.store(true, Ordering::SeqCst);
        let (first, ()) = tokio::join!(h.send(stream), async {
            tokio::time::timeout(Duration::from_secs(3), h.first_started.notified())
                .await
                .unwrap();
            h.state.cfg.write().ollama_options.default_num_ctx = 4096;
            h.release_first.notify_one();
        });
        let (a, _) = first;
        let (b, _) = h.send(stream).await;
        let (sa, aa) = h.audit(&a).await;
        let (sb, bb) = h.audit(&b).await;
        h.stop().await;
        assert_eq!((sa, sb), (200, 200));
        assert_eq!(h.calls.load(Ordering::SeqCst), 2);
        let actual = h.requests.lock().unwrap();
        assert_eq!(actual[0]["options"]["num_ctx"], 32768);
        assert_eq!(actual[1]["options"]["num_ctx"], 4096);
        assert_eq!(actual[0]["model"], "owned-physical");
        assert_eq!(actual[1]["model"], "owned-physical");
        let ca = &aa[0]["observation_context"];
        let cb = &bb[0]["observation_context"];
        assert_eq!(
            ca["scope"], "ollama_wire_projection_v1",
            "actual HTTP observation must capture immutable final configuration projection"
        );
        assert_eq!(cb["scope"], "ollama_wire_projection_v1");
        assert_eq!(ca["physical_model"], "owned-physical");
        assert_eq!(cb["physical_model"], "owned-physical");
        assert!(ca["model_revision"].is_null() && cb["model_revision"].is_null());
        assert_ne!(
            ca["fingerprint_sha256"], cb["fingerprint_sha256"],
            "same model name cannot merge actual num_ctx changes"
        );
        assert_eq!(aa[0]["generation"]["rate_tokens_per_second"], 9.0);
        assert_eq!(bb[0]["generation"]["rate_tokens_per_second"], 9.0);
    }
}
