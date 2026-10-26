use super::*;
use crate::proxy::trae::profile::ORIGIN;
use crate::proxy::trae::{
    catalog::selector_id,
    profile::{SourceFunction, TraeProfileId, MODEL_PATH},
    TraeModelClient,
};
use axum::extract::Request;
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

type CapturedRequest = (String, String, HeaderMap, Value);
struct Fixture {
    packets: parking_lot::Mutex<VecDeque<String>>,
    status: parking_lot::Mutex<StatusCode>,
    requests: parking_lot::Mutex<Vec<CapturedRequest>>,
    calls: AtomicUsize,
    bytewise: AtomicBool,
    stall: AtomicBool,
    dropped: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    hold: AtomicBool,
    tail: parking_lot::Mutex<Option<String>>,
}
struct Setup {
    state: Arc<GatewayState>,
    fixture: Arc<Fixture>,
    url: String,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
struct BodyLifetime(Arc<Fixture>);
impl Drop for BodyLifetime {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
impl Drop for Setup {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
fn frame(event: &str, value: Value) -> String {
    format!("event: {event}\ndata: {value}\n\n")
}
fn packet(text: &str, usage: Option<Value>) -> String {
    let mut value =
        frame("output", json!({"response":text})) + &frame("done", json!({"finish_reason":"stop"}));
    if let Some(usage) = usage {
        value += &frame("token_usage", usage);
    }
    value + "data: [DONE]\n\n"
}
async fn upstream(State(f): State<Arc<Fixture>>, request: Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().to_string();
    let headers = request.headers().clone();
    let bytes = axum::body::to_bytes(request.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap();
    f.requests
        .lock()
        .push((method, path.clone(), headers, body));
    f.calls.fetch_add(1, Ordering::SeqCst);
    f.entered.notify_one();
    if f.hold.load(Ordering::SeqCst) {
        f.release.notified().await;
    }
    if path == "/v1/chat/completions" {
        return Json(json!({"id":"legacy-control","model":"legacy","choices":[{"message":{"role":"assistant","content":"legacy-ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})).into_response();
    }
    let packet = f
        .packets
        .lock()
        .pop_front()
        .unwrap_or_else(|| packet("ok", None));
    let builder = Response::builder()
        .status(*f.status.lock())
        .header("content-type", "text/event-stream")
        .header("location", "/must-not-follow");
    if f.stall.load(Ordering::SeqCst) {
        let stream = async_stream::stream! {
            let _lifetime=BodyLifetime(f.clone());
            yield Ok::<_,std::io::Error>(Bytes::from(packet));
            let tail=f.tail.lock().take();
            if let Some(tail)=tail {f.release.notified().await;yield Ok(Bytes::from(tail));}
            else {std::future::pending::<()>().await;}
        };
        return builder.body(Body::from_stream(stream)).unwrap();
    }
    if f.bytewise.load(Ordering::SeqCst) {
        let stream = futures_util::stream::iter(
            packet
                .into_bytes()
                .into_iter()
                .map(|byte| Ok::<_, std::io::Error>(vec![byte])),
        );
        return builder.body(Body::from_stream(stream)).unwrap();
    }
    builder.body(Body::from(packet)).unwrap()
}
fn options(principal: &str, images: bool) -> Value {
    json!({"region":"cn","account_id":"native-fixture","principal_hash":principal,"profile_id":"cn-ordinary-3.4.3-aicc20260212-v1",
        "model_selections":{"native-exact":{"source_function":"solo_work_remote","selector_id":selector_id(TraeProfileId::CnOrdinary343Aicc20260212V1,SourceFunction::SoloWorkRemote,"Exact-A"),
        "config_name":"Exact-A","display_name":"Fixture model","prompt_max_tokens":100000,"max_tokens":8192,
        "declared_vision":null,"declared_tools":null,"declared_thinking":null,"reasoning_effort_options":[],
        "protocol_carriers":{"text":true,"tools":true,"images":true,"audio":false,"video":false},"account_model_evidence":"unverified",
        "catalog_revision":"12345678-1234-4234-9234-123456789abc","fetched_at_ms":1,"allow_undeclared_images":images}}})
}
async fn setup(images: bool) -> Setup {
    use base64::Engine;
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        base64::engine::general_purpose::STANDARD.encode([73; 32]),
    );
    let fixture = Arc::new(Fixture {
        packets: parking_lot::Mutex::new(VecDeque::new()),
        status: parking_lot::Mutex::new(StatusCode::OK),
        requests: parking_lot::Mutex::new(Vec::new()),
        calls: AtomicUsize::new(0),
        bytewise: AtomicBool::new(false),
        stall: AtomicBool::new(false),
        dropped: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        hold: AtomicBool::new(false),
        tail: parking_lot::Mutex::new(None),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(upstream).with_state(fixture.clone());
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let path = std::env::temp_dir().join(format!(
        "llmgw-owned-native-gateway-{}.db",
        uuid::Uuid::new_v4()
    ));
    let db = crate::db::Db::connect_path(&path).await.unwrap();
    eprintln!("owned Native WAL fixture: {}", path.display());
    let mut cfg = AppConfig {
        port: 0,
        bind: "127.0.0.1".into(),
        unified_key: "fixture-gateway-key".into(),
        failover_enabled: true,
        max_fallback_attempts: 8,
        compact_threshold_tokens: 1,
        ..Default::default()
    };
    cfg.cache.enabled = true;
    cfg.cascade.max_escalations = 3;
    cfg.smart_routing.enabled = true;
    cfg.smart_routing.prompt_refine.enabled = true;
    cfg.smart_routing.jev.base_url = origin.clone();
    cfg.benefits
        .accounts
        .push(crate::benefit_config::BenefitAccount {
            id: "native-fixture".into(),
            platform: "trae_cn".into(),
            label: "Owned native fixture".into(),
            enabled: true,
        });
    let mut state = GatewayState::new(db, cfg);
    let auth = state.trae_auth.as_ref().unwrap().clone();
    let binding = auth
        .seed_native_session_for_test(
            "fixture-access-secret",
            chrono::Utc::now().timestamp_millis() + 120_000,
        )
        .await;
    state.trae = Ok(Arc::new(TraeModelClient::for_test(
        auth,
        reqwest::Url::parse(&origin).unwrap(),
    )));
    let state = Arc::new(state);
    let mut runtime =
        crate::domain::AgentRuntime::new("native-runtime", "trae-model", "Native fixture");
    runtime.options = Some(options(&binding.principal_hash, images));
    repo::upsert_agent_runtime(state.db.pool(), &runtime)
        .await
        .unwrap();
    let provider:Provider=serde_json::from_value(json!({"id":"native-provider","name":"Native fixture","dialect":"openai","base_url":ORIGIN,"api_key_enc":"","enabled":true,"priority":0,"models":[{"alias":"native-exact","upstream":"Exact-A","context_window":100000,"supports_tools":true,"supports_vision":true,"supports_stream":true}],"rpm_limit":0,"intelligence":50,"note":null,"runtime_id":"native-runtime","created_at":chrono::Utc::now(),"updated_at":chrono::Utc::now()})).unwrap();
    repo::upsert_provider(state.db.pool(), &provider)
        .await
        .unwrap();
    state.reload_providers().await.unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let gateway_state = state.clone();
    let gateway_task = tokio::spawn(async move {
        let _shutdown = shutdown_tx;
        serve_ready(gateway_state, ready_tx, shutdown_rx)
            .await
            .unwrap();
    });
    let address = ready_rx.await.unwrap().unwrap();
    Setup {
        state,
        fixture,
        url: format!("http://{address}"),
        tasks: vec![upstream_task, gateway_task],
    }
}
fn request() -> Value {
    json!({"model":"native-exact","messages":[{"role":"system","content":"ORIGINAL system 中"},{"role":"user","content":"caller original"}],"max_tokens":42})
}
async fn post(t: &Setup, path: &str, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}{path}", t.url))
        .bearer_auth("fixture-gateway-key")
        .header("x-session-id", "caller-session")
        .json(body)
        .send()
        .await
        .unwrap()
}
async fn audit(t: &Setup) -> Value {
    let (raw, decision): (String, Option<String>) = sqlx::query_as(
        "SELECT attempts_json,route_decision_json FROM requests ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(t.state.db.pool())
    .await
    .unwrap();
    let marker: Value = serde_json::from_str(&raw).unwrap();
    if marker[0].get("native_exact").is_some() {
        let projected =
            serde_json::to_value(crate::audit::route_decision::project(decision.as_deref()))
                .unwrap();
        assert_eq!(projected["state"], "recorded");
        let plan = &projected["snapshot"];
        assert_eq!(plan["kind"], "exact_selection");
        assert_eq!(plan["phase"], "native_exact_selected");
        assert_eq!(plan["score_state"], "not_applicable");
        assert_eq!(plan["total_candidates"], 1);
        assert_eq!(
            plan["candidates"][0]["target"]["provider_id"],
            marker[0]["provider_id"]
        );
        assert_eq!(
            plan["candidates"][0]["target"]["configured_upstream"],
            marker[0]["model"]
        );
        assert!(plan.get("principal_hash").is_none() && plan.get("account_id").is_none());
    }
    marker
}
async fn ledger_setup(
    confirm: fn(&TraeUsageReport) -> Option<i64>,
) -> (Setup, crate::entitlement::VerifiedEntitlement) {
    ledger_setup_bound(confirm, "current").await
}
async fn ledger_setup_bound(
    confirm: fn(&TraeUsageReport) -> Option<i64>,
    mismatch: &str,
) -> (Setup, crate::entitlement::VerifiedEntitlement) {
    use sha2::{Digest, Sha256};
    let t = setup(false).await;
    let selection = resolve(&t.state, "native-exact").await.unwrap().unwrap();
    let auth = t.state.trae_auth.as_ref().unwrap();
    let captured = auth
        .prepare_model_authorization(&selection.options.binding())
        .await
        .unwrap();
    auth.model_authorization_current(&captured).await.unwrap();
    // Owned synthetic session only. This fixture's proxy epoch starts at0 and
    // unchanged None proxy configuration does not advance it. No key is read.
    let raw = repo::get_secret(
        t.state.db.pool(),
        "account-login:trae:cn:session:native-fixture",
    )
    .await
    .unwrap()
    .unwrap();
    let ciphertext_hash = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let source = format!(
        "{:x}",
        Sha256::digest(format!("trae-model-source-v1:{ciphertext_hash}:0").as_bytes())
    );
    assert_eq!(source, captured.source_fingerprint());
    let mut principal = captured.principal_hash().to_owned();
    if mismatch == "principal" {
        principal.replace_range(
            ..1,
            if captured.principal_hash().starts_with('0') {
                "1"
            } else {
                "0"
            },
        );
    }
    let proof = crate::entitlement::VerifiedEntitlement::artificial_dispatch(
        "artificial_trae_cn",
        "native-fixture",
        (principal, source),
        if mismatch == "model" {
            "Exact-B"
        } else {
            "Exact-A"
        },
        "chat",
        2,
    );
    t.state
        .cost_gate
        .accounts
        .install_artificial_native(&selection.provider, "Exact-A", proof.clone(), confirm)
        .await
        .unwrap();
    let mut cfg = t.state.cfg_snapshot();
    cfg.free_routing.enabled = true;
    t.state.update_cfg(cfg);
    (t, proof)
}

async fn ledger_success(stream: bool) {
    // Complete known zero remains a receipt, rather than an absent usage value.
    for counts in [
        json!({"prompt_tokens":10,"completion_tokens":2,"total_tokens":12,"credit":17}),
        json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"credit":0}),
    ] {
        let (t, proof) = ledger_setup(complete_request_receipt).await;
        t.fixture
            .packets
            .lock()
            .push_back(packet("owned-success", Some(counts)));
        let mut body = request();
        body["stream"] = json!(stream);
        let response = post(&t, "/v1/chat/completions", &body).await;
        let status = response.status();
        let text = response.text().await.unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "private raw native grant must reach final consumer: {text}"
        );
        assert!(text.contains("owned-success"));
        assert!(text.contains("complete"));
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
        {
            let requests = t.fixture.requests.lock();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].0, "POST");
            assert_eq!(requests[0].1, MODEL_PATH);
        }
        let balance = crate::entitlement::Ledger::new(t.state.db.pool().clone())
            .balance(&proof)
            .await
            .unwrap();
        assert_eq!((balance.held, balance.spent), (0, 1));
        let rows: Vec<(String, i64, Option<i64>)> =
            sqlx::query_as("SELECT phase,bound,confirmed_units FROM free_entitlement_reservations")
                .fetch_all(t.state.db.pool())
                .await
                .unwrap();
        assert_eq!(rows, vec![("settled".into(), 1, Some(1))]);
        let evidence = audit(&t).await;
        assert_eq!(evidence[0]["native_exact"]["attempt"], 1);
        assert_eq!(evidence[0]["native_exact"]["upstream_post_started"], true);
    }
}

#[tokio::test]
async fn trae_ledger_success_buffered_raw_gateway_single_settlement() {
    ledger_success(false).await;
}

#[tokio::test]
async fn trae_ledger_success_stream_raw_gateway_single_settlement() {
    ledger_success(true).await;
}

fn complete_request_receipt(report: &TraeUsageReport) -> Option<i64> {
    (report.state == crate::proxy::trae::sse::TraeUsageState::Complete).then_some(1)
}
async fn ledger_one(
    t: &Setup,
    proof: &crate::entitlement::VerifiedEntitlement,
    expected: (i64, i64, &str, Option<i64>),
) {
    let balance = crate::entitlement::Ledger::new(t.state.db.pool().clone())
        .balance(proof)
        .await
        .unwrap();
    assert_eq!((balance.held, balance.spent), (expected.0, expected.1));
    let rows: Vec<(String, i64, Option<i64>)> =
        sqlx::query_as("SELECT phase,bound,confirmed_units FROM free_entitlement_reservations")
            .fetch_all(t.state.db.pool())
            .await
            .unwrap();
    assert_eq!(rows, vec![(expected.2.into(), 1, expected.3)]);
    assert!(
        t.fixture
            .requests
            .lock()
            .iter()
            .all(|(method, path, _, _)| method == "POST" && path == MODEL_PATH),
        "no extra identity/refresh/catalog/legacy request"
    );
}

#[tokio::test]
async fn trae_ledger_complete_generation_principal_model_binding_refunds_only_presend() {
    for mismatch in ["source", "proxy-epoch", "principal", "model"] {
        let (t, proof) = ledger_setup_bound(complete_request_receipt, mismatch).await;
        let auth = t.state.trae_auth.as_ref().unwrap();
        let selection = resolve(&t.state, "native-exact").await.unwrap().unwrap();
        let before = auth
            .prepare_model_authorization(&selection.options.binding())
            .await
            .unwrap();
        if mismatch == "source" {
            auth.seed_native_session_for_test(
                "fixture-access-secret",
                chrono::Utc::now().timestamp_millis() + 120_000,
            )
            .await;
        } else if mismatch == "proxy-epoch" {
            // Only this owned auth object's proxy generation changes; no system
            // proxy or network request is involved.
            auth.set_proxy(Some("http://127.0.0.1:1"));
            auth.set_proxy(None);
        }
        if mismatch == "source" || mismatch == "proxy-epoch" {
            let after = auth
                .prepare_model_authorization(&selection.options.binding())
                .await
                .unwrap();
            assert_eq!(
                before
                    .headers(crate::proxy::trae::profile::NativeOperation::Model)
                    .unwrap()["x-ide-token"],
                after
                    .headers(crate::proxy::trae::profile::NativeOperation::Model)
                    .unwrap()["x-ide-token"]
            );
            assert_eq!(before.principal_hash(), after.principal_hash());
            assert_ne!(before.source_fingerprint(), after.source_fingerprint());
        }
        let response = post(&t, "/v1/chat/completions", &request()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{mismatch}");
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("native_entitlement_operation_unconfirmed"));
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
        ledger_one(&t, &proof, (0, 0, "cancelled", None)).await;
    }
}

#[tokio::test]
async fn trae_ledger_unknown_native_result_never_replays_or_falls_back() {
    for stream in [false, true] {
        for mode in ["http", "truncated"] {
            let (t, proof) = ledger_setup(complete_request_receipt).await;
            if mode == "http" {
                *t.fixture.status.lock() = StatusCode::SERVICE_UNAVAILABLE;
            } else {
                t.fixture
                    .packets
                    .lock()
                    .push_back(frame("output", json!({"response":"partial native"})));
            }
            let mut body = request();
            body["stream"] = json!(stream);
            let response = post(&t, "/v1/chat/completions", &body).await;
            assert_eq!(
                response.status(),
                if stream && mode == "truncated" {
                    StatusCode::OK
                } else {
                    StatusCode::BAD_GATEWAY
                }
            );
            let text = response.text().await.unwrap();
            assert!(text.contains("error"));
            assert!(!text.contains("data: [DONE]"));
            assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
            ledger_one(&t, &proof, (1, 0, "dispatched", None)).await;
            let fallback: i64 = sqlx::query_scalar("SELECT fallback_attempts FROM requests")
                .fetch_one(t.state.db.pool())
                .await
                .unwrap();
            assert_eq!(fallback, 0);
            let marker = audit(&t).await;
            assert_eq!(marker.as_array().unwrap().len(), 1);
            assert_eq!(marker[0]["native_exact"]["upstream_post_started"], true);
        }
    }
}

#[tokio::test]
async fn trae_ledger_typed_missing_partial_and_untrusted_receipts_keep_full_hold() {
    for mode in ["missing", "partial", "untrusted", "generic-only"] {
        let (t, proof) = ledger_setup(if mode == "untrusted" {
            |_| None
        } else {
            complete_request_receipt
        })
        .await;
        if mode == "generic-only" {
            let selection = resolve(&t.state, "native-exact").await.unwrap().unwrap();
            t.state
                .cost_gate
                .accounts
                .install_artificial(&selection.provider, "Exact-A", proof.clone(), |_| Some(1))
                .await
                .unwrap();
        }
        let usage = match mode {
            "missing" => None,
            "partial" => Some(json!({"prompt_tokens":3})),
            _ => {
                Some(json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5,"credit":17}))
            }
        };
        t.fixture
            .packets
            .lock()
            .push_back(packet("owned receipt", usage));
        let response = post(&t, "/v1/chat/completions", &request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        response.bytes().await.unwrap();
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
        ledger_one(&t, &proof, (1, 0, "dispatched", None)).await;
        let marker = audit(&t).await;
        let evidence = &marker[0]["native_exact"]["usage_evidence"];
        assert_eq!(
            evidence["state"],
            match mode {
                "missing" => "missing",
                "partial" => "partial",
                _ => "complete",
            }
        );
        if mode == "missing" || mode == "partial" {
            assert!(evidence["completion_tokens"].is_null());
            assert_eq!(marker[0]["native_exact"]["scalar_usage_placeholder"], true);
        }
    }
}

#[tokio::test]
async fn trae_ledger_storage_failures_reject_and_preserve_uncertain_dispatch() {
    for phase in ["reserve", "dispatch", "settle"] {
        let (t, proof) = ledger_setup(complete_request_receipt).await;
        t.fixture.packets.lock().push_back(packet(
            "owned receipt",
            Some(json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5})),
        ));
        let trigger=match phase {
            "reserve"=>"CREATE TRIGGER owned_reserve_failure BEFORE INSERT ON free_entitlement_reservations BEGIN SELECT RAISE(ABORT,'owned reserve failure'); END",
            "dispatch"=>"CREATE TRIGGER owned_dispatch_failure BEFORE UPDATE ON free_entitlement_reservations WHEN NEW.phase='dispatched' BEGIN SELECT RAISE(ABORT,'owned dispatch failure'); END",
            _=>"CREATE TRIGGER owned_settle_failure BEFORE UPDATE ON free_entitlement_budgets WHEN NEW.spent>OLD.spent BEGIN SELECT RAISE(ABORT,'owned settle failure'); END",
        };
        sqlx::query(trigger)
            .execute(t.state.db.pool())
            .await
            .unwrap();
        let response = post(&t, "/v1/chat/completions", &request()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{phase}");
        response.bytes().await.unwrap();
        assert_eq!(
            t.fixture.calls.load(Ordering::SeqCst),
            usize::from(phase == "settle")
        );
        if phase == "reserve" {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM free_entitlement_reservations")
                    .fetch_one(t.state.db.pool())
                    .await
                    .unwrap();
            assert_eq!(count, 0);
            let balance = crate::entitlement::Ledger::new(t.state.db.pool().clone())
                .balance(&proof)
                .await
                .unwrap();
            assert_eq!((balance.held, balance.spent), (0, 0));
        } else {
            ledger_one(
                &t,
                &proof,
                (
                    1,
                    0,
                    if phase == "dispatch" {
                        "reserved"
                    } else {
                        "dispatched"
                    },
                    None,
                ),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn trae_ledger_same_token_replacement_after_post_keeps_unknown_hold() {
    for stream in [false, true] {
        let (t, proof) = ledger_setup(complete_request_receipt).await;
        t.fixture.hold.store(true, Ordering::SeqCst);
        let url = t.url.clone();
        let mut body = request();
        body["stream"] = json!(stream);
        let task = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{url}/v1/chat/completions"))
                .bearer_auth("fixture-gateway-key")
                .json(&body)
                .send()
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(3), t.fixture.entered.notified())
            .await
            .unwrap();
        ledger_one(&t, &proof, (1, 0, "dispatched", None)).await;
        t.state
            .trae_auth
            .as_ref()
            .unwrap()
            .seed_native_session_for_test(
                "fixture-access-secret",
                chrono::Utc::now().timestamp_millis() + 120_000,
            )
            .await;
        t.fixture.release.notify_one();
        let response = task.await.unwrap();
        assert_eq!(
            response.status(),
            if stream {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            }
        );
        let text = response.text().await.unwrap();
        assert!(text.contains("native_model_session_changed"));
        assert!(!text.contains("data: [DONE]"));
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
        ledger_one(&t, &proof, (1, 0, "dispatched", None)).await;
    }
}

#[tokio::test]
async fn trae_ledger_stream_cancel_releases_reader_but_keeps_full_reservation() {
    let (t, proof) = ledger_setup(complete_request_receipt).await;
    t.fixture.stall.store(true, Ordering::SeqCst);
    t.fixture.packets.lock().push_back(
        frame("output", json!({"response":"owned prefix"}))
            + &frame("token_usage", json!({"prompt_tokens":3})),
    );
    let mut body = request();
    body["stream"] = json!(true);
    let response = post(&t, "/v1/chat/completions", &body).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut reader = response.bytes_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(3), reader.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // The existing redactor retains the fixture-secret prefix "fix". Cancel
    // after the exact safe output, while that unresolved suffix remains held.
    let text = String::from_utf8_lossy(&chunk);
    let first: Value = serde_json::from_str(text.strip_prefix("data: ").unwrap().trim()).unwrap();
    assert_eq!(first["choices"][0]["delta"]["content"], "owned pre");
    assert!(!text.contains("owned prefix"));
    ledger_one(&t, &proof, (1, 0, "dispatched", None)).await;
    drop(reader);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE status=499")
                .fetch_one(t.state.db.pool())
                .await
                .unwrap();
            if count == 1
                && t.state.agent_concurrency.lock().peek() == 0
                && t.fixture.dropped.load(Ordering::SeqCst) == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    ledger_one(&t, &proof, (1, 0, "dispatched", None)).await;
}

#[tokio::test]
async fn native_free_policy_rejects_early_exact_ingress_without_any_account_or_legacy_post() {
    let t = setup(false).await;
    let mut cfg = t.state.cfg_snapshot();
    cfg.free_routing.enabled = true;
    t.state.update_cfg(cfg);
    for stream in [false, true] {
        let mut body = request();
        body["stream"] = json!(stream);
        let response = post(&t, "/v1/chat/completions", &body).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let value: Value = response.json().await.unwrap();
        assert_eq!(value["error"]["code"], "free_policy_rejected");
        assert_eq!(
            value["error"]["reason"],
            "account_hard_stop_and_reservation_unverified"
        );
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
    let mut cfg = t.state.cfg_snapshot();
    cfg.free_routing.enabled = false;
    t.state.update_cfg(cfg);
    assert!(post(&t, "/v1/chat/completions", &request())
        .await
        .status()
        .is_success());
    assert_eq!(
        t.fixture.calls.load(Ordering::SeqCst),
        1,
        "owned account fixture proves the existing non-free path remains operational"
    );
}

#[tokio::test]
async fn native_exact_two_turn_http_preserves_history_tools_images_and_only_two_posts() {
    let t = setup(true).await;
    t.fixture.bytewise.store(true, Ordering::SeqCst);
    let calls = json!([{"index":0,"id":"call-a","type":"function","function_call":{"name":"lookup","arguments":"{\"key\":\"a\"}"}},{"index":1,"id":"call-b","type":"function","function_call":{"name":"lookup","arguments":"{\"key\":\"b\"}"}}]);
    t.fixture.packets.lock().push_back(
        frame(
            "output",
            json!({"reasoning_content":"思考\nexact 🧭", "tool_calls":calls}),
        ) + &frame("done", json!({"finish_reason":"tool_calls"}))
            + "data: [DONE]\n\n",
    );
    t.fixture.packets.lock().push_back(packet(
        "第二轮完整",
        Some(json!({"prompt_tokens":20,"completion_tokens":4,"total_tokens":24})),
    ));
    let mut first = request();
    first["tools"] = json!([{"type":"function","function":{"name":"lookup","parameters":{"type":"object","properties":{"key":{"type":"string"}}}}}]);
    first["parallel_tool_calls"] = json!(true);
    first["messages"][1]["content"] = json!([{"type":"text","text":"caller original"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aGVsbG8=","detail":"high"}}]);
    let response = post(&t, "/v1/chat/completions", &first).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-session-id"], "caller-session");
    assert_eq!(response.headers()["x-fallback-attempts"], "1");
    let reply: Value = response.json().await.unwrap();
    assert_eq!(reply["usage_status"], "missing");
    assert_eq!(reply["usage"], json!({}));
    assert_eq!(
        reply["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        audit(&t).await[0]["native_exact"]["usage_evidence"]["state"],
        "missing"
    );
    let mut second = first.clone();
    let messages = second["messages"].as_array_mut().unwrap();
    messages.push(reply["choices"][0]["message"].clone());
    messages.push(json!({"role":"tool","tool_call_id":"call-a","content":"result a"}));
    messages.push(json!({"role":"tool","tool_call_id":"call-b","content":"result b"}));
    let response = post(&t, "/v1/chat/completions", &second).await;
    assert_eq!(response.status(), 200);
    let reply: Value = response.json().await.unwrap();
    assert_eq!(reply["choices"][0]["message"]["content"], "第二轮完整");
    assert_eq!(reply["usage"]["total_tokens"], 24);
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 2);
    let rows = repo::query_requests(t.state.db.pool(), &Default::default())
        .await
        .unwrap()
        .0;
    assert_eq!(rows.len(), 2);
    for row in rows {
        let row = row.to_json();
        assert_eq!(
            row["route_decision"]["state"], "recorded",
            "raw native must preserve its own exact choice rather than a reconstructed ranked plan"
        );
        assert_eq!(row["route_decision"]["snapshot"]["kind"], "exact_selection");
        assert_eq!(
            row["route_decision"]["snapshot"]["score_state"],
            "not_applicable"
        );
        assert_eq!(
            row["route_decision"]["snapshot"]["candidates"][0]["target"]["configured_upstream"],
            "Exact-A"
        );
    }
    {
        let requests = t.fixture.requests.lock();
        assert_eq!(requests.len(), 2);
        for (_, path, headers, body) in requests.iter() {
            assert_eq!(path, MODEL_PATH);
            assert_eq!(headers["accept"], "text/event-stream");
            assert_eq!(headers["x-ide-token"], "fixture-access-secret");
            assert!(!headers.contains_key("x-machine-id"));
            assert_eq!(body["function"], "solo_work_remote");
            assert_eq!(body["config_name"], "Exact-A");
            assert_eq!(body["model"], "Exact-A");
            assert_eq!(body["max_tokens"], 42);
            assert_eq!(body["stream"], true);
            assert!(body.get("allow_undeclared_images").is_none());
        }
        let system =
            json!({"role":"system","content":[{"type":"text","text":"ORIGINAL system 中"}]});
        assert_eq!(
            requests[0].3["messages"],
            json!([system, first["messages"][1]])
        );
        let wire = &requests[1].3["messages"];
        assert_eq!(wire[0], system);
        assert_eq!(wire[1], first["messages"][1]);
        assert_eq!(wire.as_array().unwrap().len(), 5);
        assert_eq!(
            wire[3],
            json!({"role":"tool","tool_call_id":"call-a","content":[{"type":"text","text":"result a"}]})
        );
        assert_eq!(
            wire[4],
            json!({"role":"tool","tool_call_id":"call-b","content":[{"type":"text","text":"result b"}]})
        );
        assert_eq!(
            wire[2],
            json!({"role":"assistant","reasoning_content":"思考\nexact 🧭","content":[{"type":"text","text":""}],"tool_calls":[{"id":"call-a","type":"function","function_call":{"name":"lookup","arguments":"{\"key\":\"a\"}"}},{"id":"call-b","type":"function","function_call":{"name":"lookup","arguments":"{\"key\":\"b\"}"}}]})
        );
    }
    assert_eq!(
        audit(&t).await[0]["native_exact"]["usage_evidence"]["total_tokens"],
        24
    );
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(t.state.db.pool())
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    let logs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
        .fetch_one(t.state.db.pool())
        .await
        .unwrap();
    assert_eq!(logs, 2);
    assert!(t.state.legacy_providers().is_empty());
    assert_eq!(t.state.agent_concurrency.lock().peek(), 0);
}
#[tokio::test]
async fn usage_missing_partial_zero_complete_remains_truthful_in_http_and_audit() {
    let t = setup(false).await;
    for (usage, status, expected) in [
        (None, "missing", json!({})),
        (
            Some(json!({"prompt_tokens":7})),
            "partial",
            json!({"prompt_tokens":7}),
        ),
        (
            Some(json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0})),
            "complete",
            json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
        ),
        (
            Some(
                json!({"prompt_tokens":8,"completion_tokens":2,"total_tokens":10,"cache_read_tokens":0,"reasoning_tokens":1}),
            ),
            "complete",
            json!({"prompt_tokens":8,"completion_tokens":2,"total_tokens":10,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":1}}),
        ),
    ] {
        t.fixture.packets.lock().push_back(packet("hello", usage));
        let response = post(&t, "/v1/chat/completions", &request()).await;
        let http_status = response.status();
        let response: Value = response.json().await.unwrap();
        assert_eq!(http_status, 200, "{response}");
        assert_eq!(response["usage_status"], status);
        assert_eq!(response["usage"], expected);
        let marker = audit(&t).await;
        assert_eq!(marker[0]["native_exact"]["usage_evidence"]["state"], status);
        assert_eq!(
            marker[0]["native_exact"]["scalar_usage_placeholder"],
            status != "complete"
        );
        assert_eq!(marker[0]["native_exact"]["price_status"], "unknown");
        let null_cost: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM requests WHERE cost IS NOT NULL OR currency IS NOT NULL",
        )
        .fetch_one(t.state.db.pool())
        .await
        .unwrap();
        assert_eq!(null_cost, 0);
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 4);
}
#[tokio::test]
async fn other_dialects_passthrough_and_lossy_fields_reject_before_any_post() {
    let t = setup(false).await;
    for (path, body) in [
        (
            "/v1/responses",
            json!({"model":"native-exact","input":[{"type":"compaction_trigger"}]}),
        ),
        ("/v1/messages", request()),
        ("/v1/messages/count_tokens", request()),
        ("/api/chat", request()),
        (
            "/v1beta/models/native-exact:generateContent",
            json!({"contents":[]}),
        ),
        (
            "/v1/embeddings",
            json!({"model":"native-exact","input":"x"}),
        ),
        (
            "/v1/images/generations",
            json!({"model":"native-exact","prompt":"x"}),
        ),
        (
            "/v1/audio/speech",
            json!({"model":"native-exact","input":"x"}),
        ),
        (
            "/gw/agent/run",
            json!({"runtime":"native-runtime","model":"native-exact","prompt":"do not execute"}),
        ),
    ] {
        assert_eq!(post(&t, path, &body).await.status(), 400, "{path}");
    }
    let mut body = request();
    body["unknown_field"] = json!(true);
    assert_eq!(post(&t, "/v1/chat/completions", &body).await.status(), 400);
    let mut body = request();
    body["messages"][1]["content"] = json!([{"type":"image_url","image_url":{"url":"https://example.test/image.png","detail":"high"}}]);
    assert_eq!(post(&t, "/v1/chat/completions", &body).await.status(), 400);
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn responses_instructions_h11_native_guard_precedes_type_validation() {
    let t = setup(false).await;
    for instruction in [
        json!("valid instruction"),
        json!(false),
        json!({"invalid":"private"}),
        Value::Null,
        json!(""),
    ] {
        let response = post(
            &t,
            "/v1/responses",
            &json!({"model":"native-exact","input":"original","instructions":instruction}),
        )
        .await;
        assert_eq!(response.status(), 400);
        let body = response.text().await.unwrap();
        assert!(
            body.contains("native_exact_only_supports_openai_chat_completions"),
            "{body}"
        );
        assert!(!body.contains("invalid_instructions"));
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_messages")
        .fetch_one(t.state.db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0);
}
#[tokio::test]
async fn native_stream_waits_for_late_usage_and_does_not_finish_on_error_or_eof() {
    let t = setup(false).await;
    let mut body = request();
    body["stream"] = json!(true);
    t.fixture
        .packets
        .lock()
        .push_back(packet("中stream", Some(json!({"prompt_tokens":1}))));
    let response = post(&t, "/v1/chat/completions", &body).await;
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    assert!(text.contains("中stream"));
    assert!(text.contains("\"usage_status\":\"partial\""));
    assert!(text.ends_with("data: [DONE]\n\n"));
    for packet in [
        frame("output", json!({"response":"visible"})),
        frame("done", json!({"finish_reason":"stop"}))
            + &frame("error", json!({"message":"fixture-access-secret"})),
    ] {
        t.fixture.packets.lock().push_back(packet);
        let text = post(&t, "/v1/chat/completions", &body)
            .await
            .text()
            .await
            .unwrap();
        assert!(!text.contains("[DONE]"));
        assert!(text.contains("event: error"));
        assert!(!text.contains("fixture-access-secret"));
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 3);
    assert_ne!(audit(&t).await[0]["status"], 200);
}
#[tokio::test]
async fn rejected_statuses_have_exactly_one_attempt_and_never_auth_confirm_or_follow_redirect() {
    let t = setup(false).await;
    for (index, status) in [401, 403, 429, 307, 500, 503].into_iter().enumerate() {
        *t.fixture.status.lock() = StatusCode::from_u16(status).unwrap();
        let response = post(&t, "/v1/chat/completions", &request()).await;
        assert_eq!(response.status(), 502);
        let text = response.text().await.unwrap();
        assert!(text.contains(&format!("native_model_http_status_{status}")));
        assert!(!text.contains("fixture-access-secret"));
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), index + 1);
    }
}
#[tokio::test]
async fn options_binding_disabled_and_proxy_fail_closed_without_post() {
    let t = setup(false).await;
    let original = repo::get_agent_runtime(t.state.db.pool(), "native-runtime")
        .await
        .unwrap()
        .unwrap();
    for mutate in 0..7 {
        let mut runtime = original.clone();
        let options = runtime.options.as_mut().unwrap();
        match mutate {
            0 => {
                options["headers"] = json!({"x":"y"});
            }
            1 => {
                options["principal_hash"] = json!("c".repeat(64));
            }
            2 => {
                options["region"] = json!("global");
            }
            3 => {
                runtime.enabled = false;
            }
            4 => {
                options["model_selections"]["native-exact"]["selector_id"] = json!("d".repeat(64));
            }
            5 => {
                options["model_selections"]["native-exact"]["protocol_carriers"]["audio"] =
                    json!(true);
            }
            _ => {
                options["model_selections"]["native-exact"]["account_model_evidence"] =
                    json!("verified");
            }
        }
        assert!(repo::upsert_agent_runtime(t.state.db.pool(), &runtime)
            .await
            .is_err());
        // This owned corruption fixture exercises the independent call-time gate.
        sqlx::query("UPDATE agent_runtimes SET options_json=?,enabled=? WHERE id='native-runtime'")
            .bind(runtime.options.as_ref().map(serde_json::Value::to_string))
            .bind(runtime.enabled)
            .execute(t.state.db.pool())
            .await
            .unwrap();
        assert!(post(&t, "/v1/chat/completions", &request())
            .await
            .status()
            .is_client_error());
    }
    sqlx::query("UPDATE agent_runtimes SET options_json=?,enabled=? WHERE id='native-runtime'")
        .bind(original.options.as_ref().map(serde_json::Value::to_string))
        .bind(original.enabled)
        .execute(t.state.db.pool())
        .await
        .unwrap();
    let mut cfg = t.state.cfg_snapshot();
    cfg.http_proxy = Some("broken://invalid".into());
    t.state.update_cfg(cfg);
    assert!(post(&t, "/v1/chat/completions", &request())
        .await
        .status()
        .is_client_error());
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn overrides_cannot_change_sealed_identity_or_explicit_limit() {
    let t = setup(false).await;
    let original = repo::list_providers(t.state.db.pool())
        .await
        .unwrap()
        .remove(0);
    for overrides in [
        json!({"extra_body":{"config_name":"other"}}),
        json!({"extra_body":{"function":"other"}}),
        json!({"extra_body":{"max_tokens":99}}),
        json!({"extra_headers":[{"name":"x-custom","value":"value"}]}),
    ] {
        let mut provider = original.clone();
        provider.models[0].overrides = Some(serde_json::from_value(overrides.clone()).unwrap());
        assert!(repo::upsert_provider(t.state.db.pool(), &provider)
            .await
            .is_err());
        // Explicitly corrupt this owned fixture to retain the independent
        // runtime guard test after native saves gained their own validation.
        sqlx::query("UPDATE models SET overrides_json=? WHERE provider_id='native-provider' AND alias='native-exact'")
            .bind(overrides.to_string()).execute(t.state.db.pool()).await.unwrap();
        t.state.reload_providers().await.unwrap();
        assert_eq!(
            post(&t, "/v1/chat/completions", &request()).await.status(),
            400
        );
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
    let mut provider = original;
    provider.models[0].overrides = Some(serde_json::from_value(json!({"max_tokens":99})).unwrap());
    repo::upsert_provider(t.state.db.pool(), &provider)
        .await
        .unwrap();
    t.state.reload_providers().await.unwrap();
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        200
    );
    assert_eq!(t.fixture.requests.lock()[0].3["max_tokens"], 42);
    assert!(t.fixture.requests.lock()[0].3.get("temperature").is_none());
}
#[test]
fn all_reserved_names_and_unknown_nested_options_are_rejected() {
    for name in [
        "auto", "priority", "balanced", "smartest", "fastest", "reliable", "custom", "smart",
        "cascade",
    ] {
        let mut value = options(&"a".repeat(64), false);
        let selected = value["model_selections"]["native-exact"].take();
        value["model_selections"] = json!({name:selected});
        assert!(trae_options(Some(&value)).is_err());
    }
    let mut value = options(&"a".repeat(64), false);
    value["model_selections"]["native-exact"]["command"] = json!("forbidden");
    assert!(trae_options(Some(&value)).is_err());
}
#[tokio::test]
async fn explicit_terminal_clean_eof_succeeds_bare_eof_and_terminal_then_error_fail() {
    let t = setup(false).await;
    t.fixture.packets.lock().push_back(
        frame("output", json!({"response":"native terminal"}))
            + &frame("done", json!({"finish_reason":"stop"})),
    );
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        200
    );
    for value in [
        frame("output", json!({"response":"no terminal"})),
        frame("done", json!({"finish_reason":"stop"})) + &frame("error", json!({"error":"late"})),
    ] {
        t.fixture.packets.lock().push_back(value);
        assert_ne!(
            post(&t, "/v1/chat/completions", &request()).await.status(),
            200
        );
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn generation_and_disable_races_reject_publication_without_replay() {
    for generation in [true, false] {
        let t = setup(false).await;
        t.fixture.hold.store(true, Ordering::SeqCst);
        let url = t.url.clone();
        let task = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{url}/v1/chat/completions"))
                .bearer_auth("fixture-gateway-key")
                .json(&request())
                .send()
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(3), t.fixture.entered.notified())
            .await
            .unwrap();
        if generation {
            t.state
                .trae_auth
                .as_ref()
                .unwrap()
                .seed_native_session_for_test(
                    "fixture-replaced-access",
                    chrono::Utc::now().timestamp_millis() + 120_000,
                )
                .await;
        } else {
            let mut runtime = repo::get_agent_runtime(t.state.db.pool(), "native-runtime")
                .await
                .unwrap()
                .unwrap();
            runtime.enabled = false;
            repo::trae_registration::save_native_runtime_settings(t.state.db.pool(), &runtime)
                .await
                .unwrap();
        }
        t.fixture.release.notify_one();
        let response = task.await.unwrap();
        assert_ne!(response.status(), 200);
        let text = response.text().await.unwrap();
        assert!(!text.contains("\"content\":\"ok\""));
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
        assert_eq!(audit(&t).await[0]["ok"], false);
    }
}
#[tokio::test]
async fn cancellation_drops_response_reader_releases_permit_and_records_partial_status() {
    let t = setup(false).await;
    t.fixture.stall.store(true, Ordering::SeqCst);
    t.fixture.packets.lock().push_back(
        frame("output", json!({"response":"first"}))
            + &frame("token_usage", json!({"prompt_tokens":3})),
    );
    let mut body = request();
    body["stream"] = json!(true);
    let response = post(&t, "/v1/chat/completions", &body).await;
    assert_eq!(response.status(), 200);
    let mut reader = response.bytes_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(3), reader.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&chunk).contains("first"));
    drop(reader);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE status=499")
                .fetch_one(t.state.db.pool())
                .await
                .unwrap();
            if count == 1
                && t.state.agent_concurrency.lock().peek() == 0
                && t.fixture.dropped.load(Ordering::SeqCst) == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let marker = audit(&t).await;
    assert_eq!(marker[0]["ok"], false);
    assert_eq!(
        marker[0]["native_exact"]["usage_evidence"]["state"],
        "partial"
    );
    assert_eq!(
        marker[0]["native_exact"]["usage_evidence"]["prompt_tokens"],
        3
    );
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn overall_timeout_is_bounded_without_retry_and_expiry_has_zero_post() {
    let t = setup(false).await;
    let mut cfg = t.state.cfg_snapshot();
    cfg.upstream_timeout_secs = 1;
    t.state.update_cfg(cfg);
    t.fixture.stall.store(true, Ordering::SeqCst);
    t.fixture
        .packets
        .lock()
        .push_back(frame("output", json!({"response":"prefix"})));
    let response = tokio::time::timeout(
        Duration::from_secs(4),
        post(&t, "/v1/chat/completions", &request()),
    )
    .await
    .unwrap();
    let status = response.status();
    let error = response.text().await.unwrap();
    assert_eq!(status, 504, "{error}");
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(t.state.agent_concurrency.lock().peek(), 0);
    t.state
        .trae_auth
        .as_ref()
        .unwrap()
        .seed_native_session_for_test(
            "fixture-expired-access",
            chrono::Utc::now().timestamp_millis() - 1,
        )
        .await;
    assert_ne!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        200
    );
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
}
// Legacy success audits run after the HTTP response. This routing control changes
// provider identity sequentially, so await the preceding committed audit before
// editing; it is not a generic editor-versus-background-writer concurrency test.
async fn wait_for_legacy_control_audit(t: &Setup, trace: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE trace_id=? AND status=200")
                    .bind(trace)
                    .fetch_one(t.state.db.pool())
                    .await
                    .unwrap();
            if count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn duplicate_alias_is_ambiguous_auto_excludes_native_and_legacy_provider_still_works() {
    let t = setup(false).await;
    let mut cfg = t.state.cfg_snapshot();
    cfg.smart_routing.enabled = false;
    cfg.compact_threshold_tokens = u32::MAX;
    cfg.cascade.max_escalations = 0;
    t.state.update_cfg(cfg);
    let mut provider = repo::list_providers(t.state.db.pool())
        .await
        .unwrap()
        .remove(0);
    provider.id = "legacy-provider".into();
    provider.name = "Legacy control".into();
    provider.runtime_id = None;
    // Use a second owned HTTP listener with the same fixture to avoid exposing a
    // production endpoint override merely for a control test.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    provider.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(upstream)
        .with_state(t.fixture.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    provider.models[0].alias = "legacy".into();
    provider.models[0].upstream = "legacy".into();
    repo::upsert_provider(t.state.db.pool(), &provider)
        .await
        .unwrap();
    t.state.reload_providers().await.unwrap();
    let mut body = request();
    body["model"] = json!("legacy");
    let response = post(&t, "/v1/chat/completions", &body).await;
    assert_eq!(response.status(), 200);
    let trace = response.headers()["x-trace-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let value: Value = response.json().await.unwrap();
    wait_for_legacy_control_audit(&t, &trace).await;
    assert_eq!(value["choices"][0]["message"]["content"], "legacy-ok");
    let native_calls = t
        .fixture
        .requests
        .lock()
        .iter()
        .filter(|(_, path, _, _)| path == MODEL_PATH)
        .count();
    assert_eq!(native_calls, 0);
    assert_eq!(t.state.legacy_providers().len(), 1);
    provider.models[0].alias = "Exact-A".into();
    repo::upsert_provider(t.state.db.pool(), &provider)
        .await
        .unwrap();
    t.state.reload_providers().await.unwrap();
    let mut collision = request();
    collision["model"] = json!("Exact-A");
    let response = post(&t, "/v1/chat/completions", &collision).await;
    assert_eq!(response.status(), 200);
    let trace = response.headers()["x-trace-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["choices"][0]["message"]["content"], "legacy-ok");
    wait_for_legacy_control_audit(&t, &trace).await;
    assert_eq!(
        t.fixture
            .requests
            .lock()
            .iter()
            .filter(|(_, path, _, _)| path == MODEL_PATH)
            .count(),
        0
    );
    provider.models[0].alias = "native-exact".into();
    repo::upsert_provider(t.state.db.pool(), &provider)
        .await
        .unwrap();
    t.state.reload_providers().await.unwrap();
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        400
    );
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 2);
    let mut explicit = request();
    explicit["model"] = json!("native-provider:native-exact");
    assert_eq!(
        post(&t, "/v1/chat/completions", &explicit).await.status(),
        200
    );
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 3);
    task.abort();
}
#[tokio::test]
async fn unified_auth_quotas_rate_capacity_and_strict_money_gate_are_preserved() {
    let t = setup(false).await;
    let unauth = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", t.url))
        .json(&request())
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);
    let mut cfg = t.state.cfg_snapshot();
    cfg.agent.quota.daily_limit = 1;
    t.state.update_cfg(cfg);
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        200
    );
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        429
    );
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    let t = setup(false).await;
    let mut provider = repo::list_providers(t.state.db.pool())
        .await
        .unwrap()
        .remove(0);
    provider.rpm_limit = 1;
    repo::upsert_provider(t.state.db.pool(), &provider)
        .await
        .unwrap();
    t.state.reload_providers().await.unwrap();
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        200
    );
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        429
    );
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    let t = setup(false).await;
    let mut provider = repo::list_providers(t.state.db.pool())
        .await
        .unwrap()
        .remove(0);
    provider.models[0].context_window = 1;
    assert!(repo::upsert_provider(t.state.db.pool(), &provider)
        .await
        .is_err());
    // Keep the capacity gate's invalid persisted-state fixture isolated from
    // the public save policy; no real account database is involved.
    sqlx::query("UPDATE models SET context_window=1 WHERE provider_id='native-provider' AND alias='native-exact'")
        .execute(t.state.db.pool()).await.unwrap();
    t.state.reload_providers().await.unwrap();
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        400
    );
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
    let t = setup(false).await;
    let now = chrono::Utc::now();
    t.state.remote_access_keys.write().push(RemoteAccessKey {
        id: "budget-key".into(),
        label: "fixture".into(),
        key_hash: String::new(),
        enabled: true,
        rpm_limit: 100,
        monthly_budget_micros: 10,
        budget_currency: "usd".into(),
        allowed_models: vec!["other".into()],
        created_at: now,
        updated_at: now,
    });
    let selection = resolve(&t.state, "native-exact").await.unwrap().unwrap();
    let response = dispatch_native_exact(
        t.state.clone(),
        selection,
        request(),
        &HeaderMap::new(),
        Some("remote-key:budget-key".into()),
    )
    .await;
    assert_eq!(response.status(), 403);
    t.state.remote_access_keys.write()[0].allowed_models.clear();
    let selection = resolve(&t.state, "native-exact").await.unwrap().unwrap();
    let response = dispatch_native_exact(
        t.state.clone(),
        selection,
        request(),
        &HeaderMap::new(),
        Some("remote-key:budget-key".into()),
    )
    .await;
    assert_eq!(response.status(), 400);
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn auth_gate_wait_counts_toward_deadline_and_disable_before_send_has_zero_post() {
    for timeout in [false, true] {
        let t = setup(false).await;
        if timeout {
            let mut cfg = t.state.cfg_snapshot();
            cfg.upstream_timeout_secs = 1;
            t.state.update_cfg(cfg);
        }
        let auth = t.state.trae_auth.as_ref().unwrap().clone();
        let guard = auth.model_gate_for_test().await;
        let url = t.url.clone();
        let task = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{url}/v1/chat/completions"))
                .bearer_auth("fixture-gateway-key")
                .json(&request())
                .send()
                .await
                .unwrap()
        });
        if timeout {
            let response = tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.status(), 504);
            drop(guard);
        } else {
            tokio::time::timeout(Duration::from_secs(3), async {
                while t.state.agent_concurrency.lock().peek() != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let mut cfg = t.state.cfg_snapshot();
            cfg.benefits.accounts[0].enabled = false;
            t.state.update_cfg(cfg);
            drop(guard);
            assert_ne!(task.await.unwrap().status(), 200);
        }
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn failed_same_chunk_usage_keeps_last_committed_partial_counts_in_audit() {
    let t = setup(false).await;
    for invalid in [
        json!({"completion_tokens":-1}),
        json!({"completion_tokens":2,"total_tokens":1}),
        json!({"prompt_tokens":9,"completion_tokens":2}),
    ] {
        t.fixture.packets.lock().push_back(
            frame("token_usage", json!({"prompt_tokens":3})) + &frame("token_usage", invalid),
        );
        let response = post(&t, "/v1/chat/completions", &request()).await;
        assert_ne!(response.status(), 200);
        let marker = audit(&t).await;
        let evidence = &marker[0]["native_exact"]["usage_evidence"];
        assert_eq!(evidence["state"], "partial");
        assert_eq!(evidence["prompt_tokens"], 3);
        assert!(evidence["completion_tokens"].is_null());
        assert!(evidence["total_tokens"].is_null());
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn audit_failure_is_visible_to_http_and_stream_never_emits_success_done() {
    let t = setup(false).await;
    sqlx::query("CREATE TRIGGER native_fixture_reject_audit BEFORE INSERT ON requests BEGIN SELECT RAISE(ABORT, 'owned fixture audit failure'); END").execute(t.state.db.pool()).await.unwrap();
    let response = post(&t, "/v1/chat/completions", &request()).await;
    assert_eq!(response.status(), 500);
    let body = response.text().await.unwrap();
    assert!(body.contains("native_exact_audit_write_failed"));
    assert!(!body.contains("owned fixture audit failure"));
    assert!(
        t.state.audit_failure().is_some(),
        "synchronous native audit failure must also reach the close-status consumer"
    );
    let mut body = request();
    body["stream"] = json!(true);
    let text = post(&t, "/v1/chat/completions", &body)
        .await
        .text()
        .await
        .unwrap();
    assert!(text.contains("native_exact_audit_write_failed"));
    assert!(text.contains("event: error"));
    assert!(!text.contains("[DONE]"));
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn configured_reference_price_is_an_immutable_safe_snapshot_with_unknown_actual_cost() {
    let t = setup(false).await;
    let mut provider = repo::list_providers(t.state.db.pool())
        .await
        .unwrap()
        .remove(0);
    provider.models[0].price = Some(
        serde_json::from_value(json!({"prompt":1.2,"completion":3.4,"currency":"usd"})).unwrap(),
    );
    repo::upsert_provider(t.state.db.pool(), &provider)
        .await
        .unwrap();
    t.state.reload_providers().await.unwrap();
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        200
    );
    let before = audit(&t).await;
    assert_eq!(
        before[0]["native_exact"]["configured_price_snapshot"]["prompt"],
        json!(1.2)
    );
    assert_eq!(before[0]["native_exact"]["price_status"], "unknown");
    provider.models[0].price.as_mut().unwrap().prompt = 9.8;
    repo::upsert_provider(t.state.db.pool(), &provider)
        .await
        .unwrap();
    let after = audit(&t).await;
    assert_eq!(after, before);
    let cost: Option<f64> =
        sqlx::query_scalar("SELECT cost FROM requests ORDER BY id DESC LIMIT 1")
            .fetch_one(t.state.db.pool())
            .await
            .unwrap();
    assert!(cost.is_none());
}
#[tokio::test]
async fn native_account_must_exist_uniquely_enabled_and_cn_but_benefits_service_can_be_off() {
    let t = setup(false).await;
    let original = t.state.cfg_snapshot();
    assert!(!original.benefits.enabled);
    assert_eq!(
        post(&t, "/v1/chat/completions", &request()).await.status(),
        200
    );
    for invalid in 0..5 {
        let mut cfg = original.clone();
        match invalid {
            0 => cfg.benefits.accounts.clear(),
            1 => cfg.benefits.accounts[0].enabled = false,
            2 => cfg.benefits.accounts[0].platform = "qoder".into(),
            3 => cfg.benefits.accounts.push(cfg.benefits.accounts[0].clone()),
            _ => cfg.benefits.accounts[0].platform = "trae_global".into(),
        }
        t.state.update_cfg(cfg);
        assert_eq!(
            post(&t, "/v1/chat/completions", &request()).await.status(),
            400
        );
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn account_disabled_during_stream_stops_next_publication_without_replay() {
    let t = setup(false).await;
    t.fixture.stall.store(true, Ordering::SeqCst);
    t.fixture
        .packets
        .lock()
        .push_back(frame("output", json!({"response":"before-disable"})));
    *t.fixture.tail.lock() = Some(packet("after-disable", None));
    let mut body = request();
    body["stream"] = json!(true);
    let mut stream = post(&t, "/v1/chat/completions", &body).await.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains("before-disable"));
    let mut cfg = t.state.cfg_snapshot();
    cfg.benefits.accounts[0].enabled = false;
    t.state.update_cfg(cfg);
    t.fixture.release.notify_one();
    let mut rest = String::new();
    while let Some(chunk) = stream.next().await {
        rest.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
    }
    assert!(rest.contains("native_account_unavailable"));
    assert!(!rest.contains("after-disable"));
    assert!(!rest.contains("[DONE]"));
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
}
fn client_logical_channels(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut channels = std::collections::BTreeMap::<String, String>::new();
    for line in text.lines().filter_map(|line| line.strip_prefix("data: ")) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let delta = &value["choices"][0]["delta"];
        for (field, channel) in [
            ("content", "content"),
            ("reasoning_content", "reasoning"),
            ("refusal", "refusal"),
        ] {
            if let Some(text) = delta[field].as_str() {
                channels.entry(channel.into()).or_default().push_str(text);
            }
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for call in calls {
                if let Some(args) = call["function"]["arguments"].as_str() {
                    channels
                        .entry(format!("tool:{}", call["index"].as_u64().unwrap()))
                        .or_default()
                        .push_str(args);
                }
            }
        }
    }
    channels
}
#[tokio::test]
async fn interleaved_private_channel_fails_http_stream_and_nonstream_once_with_partial_usage() {
    let t = setup(false).await;
    let mut calls = 0;
    for attack in 0..3 {
        for stream in [true, false] {
            let mut body = request();
            body["stream"] = json!(stream);
            let frames = if attack == 2 {
                body["tools"] = json!([{"type":"function","function":{"name":"lookup","parameters":{"type":"object","properties":{"q":{"type":"string"}}}}}]);
                frame(
                    "output",
                    json!({"tool_calls":[{"index":0,"id":"call-a","function_call":{"name":"lookup","arguments":"{\"q\":\"fixture-"}}]}),
                ) + &frame(
                    "output",
                    json!({"tool_calls":[{"index":1,"id":"call-b","function_call":{"name":"lookup","arguments":"{\"q\":\"ordinary\"}"}}]}),
                ) + &frame("token_usage", json!({"prompt_tokens":3}))
                    + &frame(
                        "output",
                        json!({"tool_calls":[{"index":0,"function_call":{"arguments":"access-secret\"}"}}]}),
                    )
                    + &frame("done", json!({"finish_reason":"tool_calls"}))
            } else {
                let (channel, other) = if attack == 0 {
                    ("response", "reasoning_content")
                } else {
                    ("reasoning_content", "response")
                };
                frame("output", json!({channel:"fixture-"}))
                    + &frame("output", json!({other:"ordinary"}))
                    + &frame("token_usage", json!({"prompt_tokens":3}))
                    + &frame("output", json!({channel:"access-secret"}))
                    + &frame("done", json!({"finish_reason":"stop"}))
            };
            t.fixture
                .packets
                .lock()
                .push_back(frames + "data: [DONE]\n\n");
            let response = post(&t, "/v1/chat/completions", &body).await;
            assert_eq!(response.status(), if stream { 200 } else { 502 });
            let text = response.text().await.unwrap();
            assert!(!text.contains("fixture-access-secret"));
            assert!(!text.contains("[DONE]"));
            assert!(text.contains("native_sensitive_reflection"));
            assert!(client_logical_channels(&text)
                .values()
                .all(|value| !value.contains("fixture-access-secret")));
            let marker = audit(&t).await;
            assert_eq!(marker[0]["ok"], false);
            assert_eq!(
                marker[0]["native_exact"]["usage_evidence"]["state"],
                "partial"
            );
            assert_eq!(
                marker[0]["native_exact"]["usage_evidence"]["prompt_tokens"],
                3
            );
            assert!(marker[0]["native_exact"]["usage_evidence"]["completion_tokens"].is_null());
            calls += 1;
            assert_eq!(t.fixture.calls.load(Ordering::SeqCst), calls);
        }
    }
}
#[tokio::test]
async fn safe_interleaved_content_reasoning_and_tool_indices_preserve_http_outputs() {
    let t = setup(false).await;
    for (index, stream) in [true, false].into_iter().enumerate() {
        let calls = json!([{"index":0,"id":"call-a","type":"function","function_call":{"name":"lookup","arguments":"{\"q\":\"zero\"}"}},
            {"index":1,"id":"call-b","type":"function","function_call":{"name":"lookup","arguments":"{\"q\":\"one\"}"}}]);
        t.fixture.packets.lock().push_back(
            frame("output", json!({"response":"safe 中文"}))
                + &frame("output", json!({"reasoning_content":"safe 推理"}))
                + &frame("output", json!({"tool_calls":calls}))
                + &frame("done", json!({"finish_reason":"tool_calls"})),
        );
        let mut body = request();
        body["stream"] = json!(stream);
        body["tools"] = json!([{"type":"function","function":{"name":"lookup","parameters":{"type":"object","properties":{"q":{"type":"string"}}}}}]);
        let response = post(&t, "/v1/chat/completions", &body).await;
        assert_eq!(response.status(), 200);
        let text = response.text().await.unwrap();
        if stream {
            assert!(text.ends_with("data: [DONE]\n\n"));
            let channels = client_logical_channels(&text);
            assert_eq!(channels["content"], "safe 中文");
            assert_eq!(channels["reasoning"], "safe 推理");
            assert_eq!(channels["tool:0"], "{\"q\":\"zero\"}");
            assert_eq!(channels["tool:1"], "{\"q\":\"one\"}");
        } else {
            let value: Value = serde_json::from_str(&text).unwrap();
            let message = &value["choices"][0]["message"];
            assert_eq!(message["content"], "safe 中文");
            assert_eq!(message["reasoning_content"], "safe 推理");
            assert_eq!(message["tool_calls"][0]["id"], "call-a");
            assert_eq!(
                message["tool_calls"][0]["function"]["arguments"],
                "{\"q\":\"zero\"}"
            );
            assert_eq!(message["tool_calls"][1]["id"], "call-b");
            assert_eq!(
                message["tool_calls"][1]["function"]["arguments"],
                "{\"q\":\"one\"}"
            );
        }
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), index + 1);
        assert_eq!(audit(&t).await[0]["ok"], true);
    }
}
#[tokio::test]
async fn cancellation_with_held_prefix_discards_pending_output_but_retains_usage_snapshot() {
    let t = setup(false).await;
    t.fixture.stall.store(true, Ordering::SeqCst);
    t.fixture.packets.lock().push_back(
        frame("output", json!({"response":"visible fixture-"}))
            + &frame("token_usage", json!({"prompt_tokens":3})),
    );
    let mut body = request();
    body["stream"] = json!(true);
    let mut stream = post(&t, "/v1/chat/completions", &body).await.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first = std::str::from_utf8(&first).unwrap();
    assert_eq!(client_logical_channels(first)["content"], "visible ");
    assert!(!first.contains("fixture-"));
    drop(stream);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE status=499")
                .fetch_one(t.state.db.pool())
                .await
                .unwrap();
            if count == 1
                && t.state.agent_concurrency.lock().peek() == 0
                && t.fixture.dropped.load(Ordering::SeqCst) == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let marker = audit(&t).await;
    assert_eq!(marker[0]["ok"], false);
    assert_eq!(
        marker[0]["native_exact"]["usage_evidence"]["state"],
        "partial"
    );
    assert_eq!(
        marker[0]["native_exact"]["usage_evidence"]["prompt_tokens"],
        3
    );
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn finalfix_sparse_tool_indexes_do_not_invent_calls() {
    let t = setup(false).await;
    let mut body = request();
    body["tools"] =
        json!([{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}]);
    t.fixture.packets.lock().push_back(frame("output", json!({"tool_calls":[{"index":3,"id":"call-three","function_call":{"name":"lookup","arguments":"{}"}}]})) + &frame("done",json!({"finish_reason":"tool_calls"})) + "data: [DONE]\n\n");
    let response = post(&t, "/v1/chat/completions", &body).await;
    assert_eq!(response.status(), 200);
    let reply: Value = response.json().await.unwrap();
    assert_eq!(
        reply["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "only actual index 3 exists"
    );
    assert_eq!(
        reply["choices"][0]["message"]["tool_calls"][0]["id"],
        "call-three"
    );
}

#[tokio::test]
async fn finalfix_controlled_connection_retirement_contrast() {
    let memory = crate::db::Db::connect_in_memory().await.unwrap();
    memory
        .pool()
        .acquire()
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let error = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM requests")
        .fetch_one(memory.pool())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no such table"));
    let path = std::env::temp_dir().join(format!(
        "llmgw-owned-native-retirement-{}.db",
        uuid::Uuid::new_v4()
    ));
    let disk = crate::db::Db::connect_path(&path).await.unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(disk.pool())
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    // Retire every pooled connection, then reopen through the production initializer.
    disk.pool().close().await;
    let reopened = crate::db::Db::connect_path(&path).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
        .fetch_one(reopened.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    reopened.pool().close().await;
    eprintln!("owned retirement contrast fixture: {}", path.display());
}

#[test]
fn finalfix_audit_shutdown_does_not_recursively_spawn() {
    const NAME: &str =
        "proxy::server::native_exact::tests::finalfix_audit_shutdown_does_not_recursively_spawn";
    if std::env::var_os("LLMGW_NATIVE_AUDIT_SHUTDOWN_CHILD").is_none() {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let output = command
            .args(["--exact", NAME, "--nocapture"])
            .env("LLMGW_NATIVE_AUDIT_SHUTDOWN_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "shutdown child {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let t = runtime.block_on(setup(false));
    let selection = runtime
        .block_on(resolve(&t.state, "native-exact"))
        .unwrap()
        .unwrap();
    let tps_source = t
        .state
        .health
        .tps_lease(&selection.provider, &selection.model.upstream);
    let audit = Audit {
        decision: crate::audit::route_decision::Snapshot::native_exact(
            t.state.cfg_snapshot().routing_strategy,
            &selection.provider.id,
            &selection.model.alias,
            &selection.selected.config_name,
        ),
        state: t.state.clone(),
        selection,
        client: None,
        session: "shutdown-fixture".into(),
        trace: "shutdown-fixture".into(),
        started: Instant::now(),
        tps_source,
        posted: Arc::new(AtomicBool::new(false)),
        report: TraeUsageReport::default(),
        status: 499,
        error: None,
        recorded: false,
        evidence: Arc::new(parking_lot::Mutex::new(TraeUsageReport::default())),
    };
    runtime.spawn(async move {
        let _audit = audit;
        std::future::pending::<()>().await;
    });
    runtime.block_on(tokio::task::yield_now());
    drop(t);
    drop(runtime);
}

#[tokio::test]
async fn finalfix_cancel_acquire_memory_vs_wal_schema_lifetime() {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    for durable in [false, true] {
        let armed = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let path = std::env::temp_dir().join(format!(
            "llmgw-owned-native-cancel-{}.db",
            uuid::Uuid::new_v4()
        ));
        let opts = if durable {
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
        } else {
            SqliteConnectOptions::new().in_memory(true)
        };
        let flag = armed.clone();
        let signal = entered.clone();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .before_acquire(move |_, _| {
                let flag = flag.clone();
                let signal = signal.clone();
                Box::pin(async move {
                    if flag.load(Ordering::SeqCst) {
                        signal.notify_one();
                        std::future::pending::<()>().await;
                    }
                    Ok(true)
                })
            })
            .connect_with(opts)
            .await
            .unwrap();
        crate::db::migrations::run(&pool).await.unwrap();
        armed.store(true, Ordering::SeqCst);
        let acquiring = tokio::spawn(pool.acquire());
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        acquiring.abort();
        assert!(acquiring.await.unwrap_err().is_cancelled());
        armed.store(false, Ordering::SeqCst);
        let result = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM requests")
            .fetch_one(&pool)
            .await;
        if durable {
            assert_eq!(result.unwrap(), 0);
        } else {
            assert!(result.unwrap_err().to_string().contains("no such table"));
        }
        pool.close().await;
        if durable {
            eprintln!("owned cancellation contrast fixture: {}", path.display());
        }
    }
}

#[tokio::test]
async fn finalfix_reasoning_only_followup_preserves_exact_private_history() {
    for stream in [false, true] {
        let t = setup(false).await;
        t.fixture.packets.lock().push_back(
            frame("output", json!({"reasoning_content":"only\n推理 🧭"}))
                + &frame("done", json!({"finish_reason":"stop"}))
                + "data: [DONE]\n\n",
        );
        let mut body = request();
        body["stream"] = json!(stream);
        let response = post(&t, "/v1/chat/completions", &body).await;
        assert_eq!(response.status(), 200);
        let assistant = if stream {
            let text = response.text().await.unwrap();
            let channels = client_logical_channels(&text);
            assert_eq!(channels["reasoning"], "only\n推理 🧭");
            assert!(text.contains("[DONE]"));
            json!({"role":"assistant","content":"","reasoning_content":channels["reasoning"]})
        } else {
            response.json::<Value>().await.unwrap()["choices"][0]["message"].clone()
        };
        assert_eq!(assistant["reasoning_content"], "only\n推理 🧭");
        body["messages"]
            .as_array_mut()
            .unwrap()
            .extend([assistant, json!({"role":"user","content":"followup"})]);
        body["stream"] = json!(false);
        assert_eq!(post(&t, "/v1/chat/completions", &body).await.status(), 200);
        let requests = t.fixture.requests.lock();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].3["messages"][2],
            json!({"role":"assistant","content":[{"type":"text","text":""}],"reasoning_content":"only\n推理 🧭"})
        );
        assert_eq!(
            requests[1].3["messages"][3],
            json!({"role":"user","content":[{"type":"text","text":"followup"}]})
        );
    }
}

#[tokio::test]
async fn finalfix_reasoning_null_empty_unicode_reach_wire_without_visible_mix() {
    let t = setup(false).await;
    for reasoning in [Value::Null, json!(""), json!(" exact\n推理 🧭 ")] {
        let mut body = request();
        body["messages"].as_array_mut().unwrap().extend([
            json!({"role":"assistant","content":"visible","reasoning_content":reasoning}),
            json!({"role":"user","content":"followup"}),
        ]);
        assert_eq!(post(&t, "/v1/chat/completions", &body).await.status(), 200);
        let requests = t.fixture.requests.lock();
        let wire = &requests.last().unwrap().3["messages"][2];
        assert!(wire.as_object().unwrap().contains_key("reasoning_content"));
        assert_eq!(wire["reasoning_content"], reasoning);
        assert_eq!(wire["content"], json!([{"type":"text","text":"visible"}]));
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn finalfix_sparse_fragmented_stream_nonstream_and_tool_results_agree() {
    for indices in [vec![3], vec![0, 3], vec![2, 0, 1]] {
        let mut expected:Vec<_>=indices.iter().map(|i| json!({"id":format!("call-{i}"),"type":"function","function":{"name":"lookup","arguments":format!("{{\"q\":\"{i}\"}}")}})).collect();
        expected.sort_by_key(|v| v["id"].as_str().unwrap().to_owned());
        for streaming in [false, true] {
            let t = setup(false).await;
            let mut packets = String::new();
            for i in &indices {
                packets += &frame(
                    "output",
                    json!({"tool_calls":[{"index":i,"id":format!("call-{i}"),"function_call":{"name":"lookup","arguments":"{\"q\":\""}}]}),
                );
            }
            for i in indices.iter().rev() {
                packets += &frame(
                    "output",
                    json!({"tool_calls":[{"index":i,"function_call":{"arguments":format!("{i}\"}}")}}]}),
                );
            }
            packets += &frame("done", json!({"finish_reason":"tool_calls"}));
            packets += "data: [DONE]\n\n";
            t.fixture.packets.lock().push_back(packets);
            let mut body = request();
            body["stream"] = json!(streaming);
            body["tools"] = json!([{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}]);
            let response = post(&t, "/v1/chat/completions", &body).await;
            assert_eq!(response.status(), 200);
            let assistant = if streaming {
                let text = response.text().await.unwrap();
                assert!(text.contains("[DONE]"));
                let mut collected = std::collections::BTreeMap::<u64, Value>::new();
                for line in text.lines().filter_map(|l| l.strip_prefix("data: ")) {
                    if line == "[DONE]" {
                        continue;
                    }
                    let chunk: Value = serde_json::from_str(line).unwrap();
                    if let Some(calls) = chunk["choices"][0]["delta"]["tool_calls"].as_array() {
                        for call in calls {
                            let i = call["index"].as_u64().unwrap();
                            let acc = collected
                                .entry(i)
                                .or_insert(json!({"type":"function","function":{"arguments":""}}));
                            if let Some(id) = call.get("id") {
                                acc["id"] = id.clone();
                            }
                            if let Some(name) = call["function"].get("name") {
                                acc["function"]["name"] = name.clone();
                            }
                            if let Some(fragment) = call["function"]["arguments"].as_str() {
                                acc["function"]["arguments"] = json!(
                                    acc["function"]["arguments"].as_str().unwrap().to_owned()
                                        + fragment
                                );
                            }
                        }
                    }
                }
                json!({"role":"assistant","content":"","tool_calls":collected.into_values().collect::<Vec<_>>()})
            } else {
                response.json::<Value>().await.unwrap()["choices"][0]["message"].clone()
            };
            assert_eq!(assistant["tool_calls"], json!(expected));
            let messages = body["messages"].as_array_mut().unwrap();
            messages.push(assistant);
            for call in &expected {
                messages.push(json!({"role":"tool","tool_call_id":call["id"],"content":format!("result {}",call["id"])}));
            }
            body["stream"] = json!(false);
            assert_eq!(post(&t, "/v1/chat/completions", &body).await.status(), 200);
            assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 2);
            let requests = t.fixture.requests.lock();
            let wire = &requests[1].3;
            assert_eq!(
                wire["messages"].as_array().unwrap().len(),
                3 + expected.len()
            );
            for (j, call) in expected.iter().enumerate() {
                assert_eq!(wire["messages"][2]["tool_calls"][j]["id"], call["id"]);
                assert_eq!(
                    wire["messages"][2]["tool_calls"][j]["function_call"],
                    call["function"]
                );
                assert_eq!(wire["messages"][3 + j]["tool_call_id"], call["id"]);
            }
            assert_eq!(wire["model"], "Exact-A");
        }
    }
}

#[tokio::test]
async fn finalfix_tool_conflicts_never_become_successful_phantoms() {
    for streaming in [false, true] {
        for conflict in [
            json!({"index":3,"id":"changed","function_call":{"arguments":"{}"}}),
            json!({"index":4,"id":"call-three","function_call":{"name":"lookup","arguments":"{}"}}),
            json!({"index":3,"function_call":{"name":"other","arguments":"{}"}}),
        ] {
            let t = setup(false).await;
            t.fixture.packets.lock().push_back(frame("output",json!({"tool_calls":[{"index":3,"id":"call-three","function_call":{"name":"lookup","arguments":""}}]}))+&frame("output",json!({"tool_calls":[conflict]}))+&frame("done",json!({"finish_reason":"tool_calls"}))+"data: [DONE]\n\n");
            let mut body = request();
            body["stream"] = json!(streaming);
            body["tools"] = json!([{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}]);
            let response = post(&t, "/v1/chat/completions", &body).await;
            if streaming {
                assert_eq!(response.status(), 200);
                let text = response.text().await.unwrap();
                assert!(text.contains("error"));
                assert!(!text.contains("[DONE]"));
            } else {
                assert_eq!(response.status(), 502);
            }
            assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn native_completion_tps_needs_no_manual_health_warmup() {
    for streaming in [false, true] {
        let t = setup(false).await;
        assert_eq!(
            t.state
                .health
                .get("native-provider", "Exact-A")
                .last_checked_at,
            0
        );
        t.fixture.packets.lock().push_back(packet(
            "owned-rate",
            Some(json!({
                "prompt_tokens":2,"completion_tokens":8,"total_tokens":10
            })),
        ));
        t.fixture.hold.store(true, Ordering::SeqCst);
        let mut body = request();
        body["stream"] = json!(streaming);
        let release = async {
            tokio::time::timeout(Duration::from_secs(5), t.fixture.entered.notified())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            t.fixture.release.notify_one();
        };
        let (response, ()) = tokio::join!(post(&t, "/v1/chat/completions", &body), release);
        assert_eq!(response.status(), 200);
        let text = response.text().await.unwrap();
        assert!(text.contains("owned-rate"));
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
        let got = t.state.health.get("native-provider", "Exact-A");
        eprintln!(
            "owned native completion stream={streaming} posts=1 samples={} checked={}",
            got.tps_samples, got.last_checked_at
        );
        assert_eq!(
            got.tps_samples, 1,
            "successful raw-native completion must record its first valid call-window sample"
        );
        assert!(got.avg_tps > 0.0);
        assert!(got.last_checked_at > 0);
    }
}

#[tokio::test]
async fn native_completion_unknown_usage_records_success_without_zero_sample() {
    for streaming in [false, true] {
        let t = setup(false).await;
        t.fixture
            .packets
            .lock()
            .push_back(packet("owned-no-usage", None));
        let mut body = request();
        body["stream"] = json!(streaming);
        let response = post(&t, "/v1/chat/completions", &body).await;
        assert_eq!(response.status(), 200);
        let text = response.text().await.unwrap();
        assert!(text.contains("owned-no-usage"));
        assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
        let got = t.state.health.get("native-provider", "Exact-A");
        assert_eq!(
            got.tps_samples, 0,
            "missing usage must not become a measured zero"
        );
        assert_eq!(got.avg_tps, 0.0);
        assert!(
            got.last_checked_at > 0,
            "completed transport still has a real success observation"
        );
    }
}

/// Desktop production DTO -> authenticated local gateway -> selected native upstream.
/// Synthetic sessions and loopback only, not actual vendor/account acceptance.
#[tokio::test]
async fn six_region_desktop_trae_selected_multiturn_keeps_reasoning_and_usage() {
    let t = setup(false).await;
    t.fixture.packets.lock().push_back(
        frame(
            "output",
            json!({"reasoning_content":"private owned reasoning"}),
        ) + &packet(
            "first desktop answer",
            Some(json!({"prompt_tokens":5,"completion_tokens":2,"total_tokens":7})),
        ),
    );
    t.fixture.packets.lock().push_back(packet(
        "second desktop answer",
        Some(json!({"prompt_tokens":9,"completion_tokens":3,"total_tokens":12})),
    ));
    let session = uuid::Uuid::new_v4().to_string();
    let mut wire = json!({"model":"native-provider:native-exact","session_id":session,"max_tokens":42,"messages":[{"role":"user","content":"owned first question"}]});
    let address = t.url.strip_prefix("http://").unwrap().parse().unwrap();
    let first = crate::desktop_chat::send(
        &t.state.cfg_snapshot(),
        address,
        serde_json::from_value(wire.clone()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(first.content, "first desktop answer");
    assert_eq!(first.reasoning.as_deref(), Some("private owned reasoning"));
    assert_eq!(first.usage.as_ref().unwrap()["total_tokens"], 7);
    assert_eq!(first.fallback_attempts, 0);
    wire["messages"].as_array_mut().unwrap().extend([
        json!({"role":"assistant","content":first.content,"reasoning_content":first.reasoning}),
        json!({"role":"user","content":"owned followup"}),
    ]);
    let second = crate::desktop_chat::send(
        &t.state.cfg_snapshot(),
        address,
        serde_json::from_value(wire).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(second.content, "second desktop answer");
    assert_eq!(second.usage.as_ref().unwrap()["total_tokens"], 12);
    assert_eq!(second.session_id, session);
    assert_eq!(second.fallback_attempts, 0);
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 2);
    {
        let requests = t.fixture.requests.lock();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].3["messages"][1]["reasoning_content"],
            "private owned reasoning"
        );
        assert_eq!(
            requests[1].3["messages"][2]["content"][0]["text"],
            "owned followup"
        );
    }
    t.state
        .drain_audits(std::time::Duration::from_secs(3))
        .await
        .unwrap();
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT status,attempts_json FROM requests ORDER BY id")
            .fetch_all(t.state.db.pool())
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    for (status, raw) in rows {
        assert_eq!(status, 200);
        let attempts: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(attempts.as_array().unwrap().len(), 1);
        assert_eq!(attempts[0]["provider_id"], "native-provider");
    }
}

#[tokio::test]
async fn six_region_desktop_reasoning_limits_and_legacy_zero_extra_dispatch() {
    let t = setup(false).await;
    let address = t.url.strip_prefix("http://").unwrap().parse().unwrap();
    let base = json!({"model":"native-provider:native-exact","session_id":uuid::Uuid::new_v4().to_string(),"max_tokens":42,"messages":[{"role":"user","content":"legacy desktop text"}]});
    let old: crate::desktop_chat::ChatInput = serde_json::from_value(base.clone()).unwrap();
    assert!(serde_json::to_value(&old).unwrap()["messages"][0]
        .get("reasoning_content")
        .is_none());
    let mut wrong = base.clone();
    wrong["messages"][0]["reasoning_content"] = json!("not assistant history");
    assert!(crate::desktop_chat::send(
        &t.state.cfg_snapshot(),
        address,
        serde_json::from_value(wrong).unwrap()
    )
    .await
    .is_err());
    let mut big = base.clone();
    big["messages"] = json!([{"role":"assistant","content":"","reasoning_content":"x".repeat(262144)},{"role":"user","content":"more"}]);
    assert!(crate::desktop_chat::send(
        &t.state.cfg_snapshot(),
        address,
        serde_json::from_value(big).unwrap()
    )
    .await
    .is_err());
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 0);
    let reply = crate::desktop_chat::send(&t.state.cfg_snapshot(), address, old)
        .await
        .unwrap();
    assert_eq!(reply.content, "ok");
    assert!(reply.reasoning.is_none());
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    assert!(t.fixture.requests.lock()[0].3["messages"][0]
        .get("reasoning_content")
        .is_none());
}

#[tokio::test]
async fn desktop_stream_review_trae_exact_opt_in_preserves_native_terminal() {
    let t = setup(false).await;
    for (usage, expected) in [
        (None, None),
        (
            Some(json!({"prompt_tokens":7})),
            Some(json!({"prompt_tokens":7})),
        ),
        (
            Some(json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0})),
            Some(json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0})),
        ),
    ] {
        t.fixture
            .packets
            .lock()
            .push_back(packet("native answer", usage.clone()));
        let mut ordinary = request();
        ordinary["stream"] = json!(true);
        let wire = post(&t, "/v1/chat/completions", &ordinary)
            .await
            .text()
            .await
            .unwrap();
        assert!(wire.contains("data: [DONE]"));
        assert!(!wire.contains("llmgw_reply_observation"));
        t.fixture
            .packets
            .lock()
            .push_back(packet("native answer", usage));
        let cfg = t.state.cfg_snapshot();
        let mut deltas = Vec::new();
        let result = crate::desktop_chat::stream::send(
            &cfg,
            t.url.trim_start_matches("http://").parse().unwrap(),
            crate::desktop_chat::ChatInput {
                personal_test: false,
                model: "native-exact".into(),
                session_id: uuid::Uuid::new_v4().to_string(),
                max_tokens: 42,
                messages: vec![crate::desktop_chat::ChatMessage {
                    role: "user".into(),
                    content: "synthetic native".into(),
                    reasoning_content: None,
                }],
            },
            uuid::Uuid::new_v4(),
            |event| {
                deltas.push(event.text);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(deltas.concat(), "native answer");
        assert_eq!(result.finish_reason.as_deref(), Some("stop"));
        assert_eq!(result.usage, expected);
    }
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn conversation_feedback_accepts_actual_native_audit_writer() {
    let t = setup(false).await;
    let response = post(&t, "/v1/chat/completions", &request()).await;
    assert_eq!(response.status(), StatusCode::OK);
    response.text().await.unwrap();
    let summary =
        repo::conversation_feedback::summary(t.state.db.pool(), &Default::default(), None)
            .await
            .unwrap();
    assert_eq!(summary.matched_requests, 1);
    assert_eq!(summary.grouped_matching_requests, 1);
    assert_eq!(summary.total_tasks, 1);
    assert_eq!(summary.tasks[0].task.session_id, "caller-session");
    let detail = repo::conversation_feedback::requests(
        t.state.db.pool(),
        &summary.tasks[0].task,
        &Default::default(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(detail.total_requests, 1);
    assert_eq!(detail.rows[0].status, Some(200));
    let projected = serde_json::to_string(&summary).unwrap();
    assert!(!projected.contains("native-fixture"));
    assert!(!projected.contains("fixture-access-secret"));
}
