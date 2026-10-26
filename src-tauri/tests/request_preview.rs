//! Supplied-shape production preview vs owned loopback dispatch. No real model,
//! catalog or account observations: catalog rows below are explicit fixtures.
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use futures_util::FutureExt;
use llm_gateway_lib::{
    config::{AppConfig, RoutingStrategy},
    db::{repo, repo::catalog, Db},
    domain::{Provider, RemoteAccessKey},
    model_catalog::observation::decode_openrouter,
    proxy::server::{GatewayState, RequestPreviewInput},
    proxy_control::gateway::GatewayService,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

struct Owned {
    state: Arc<GatewayState>,
    service: GatewayService,
    base: String,
    calls: Calls,
    upstream: tokio::task::JoinHandle<()>,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.upstream.abort();
    }
}
fn input(body: Value, strategy: Option<RoutingStrategy>, key: Option<&str>) -> RequestPreviewInput {
    serde_json::from_value(json!({"request":body,"strategy":strategy,"access_key_id":key})).unwrap()
}
fn text() -> Value {
    json!({"model":"auto","messages":[{"role":"user","content":"owned request preview"}],"max_tokens":16})
}

#[tokio::test]
async fn n2runtime_buffered_preview_matches_owned_dispatch_runtime_refusal() {
    let f = Owned::new().await;
    let result = std::panic::AssertUnwindSafe(async {
        let mut body = text();
        body["model"] = json!("text:owned-model");
        let normal = f
            .state
            .preview_routing_request(input(body.clone(), None, None))
            .await
            .unwrap();
        assert!(normal
            .rows
            .iter()
            .any(|r| r.provider_id == "text" && r.eligible));
        let reply = f.send(body.clone()).await;
        assert!(reply.status().is_success());
        reply.text().await.unwrap();
        assert_eq!(f.calls.lock().unwrap().len(), 1);
        // Configuration validity is a positive preview path, not login proof.
        // A synthetic account id is never prepared or invoked by this check.
        let mut configured = llm_gateway_lib::domain::AgentRuntime::new(
            "owned-configured-runtime",
            "workbuddy-model",
            "Owned local binding",
        );
        configured.options = Some(json!({"account_id":"owned-no-real-account"}));
        repo::upsert_agent_runtime(f.state.db.pool(), &configured)
            .await
            .unwrap();
        let mut provider = repo::list_providers(f.state.db.pool())
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.id == "text")
            .unwrap();
        provider.runtime_id = Some(configured.id.clone());
        repo::upsert_provider(f.state.db.pool(), &provider)
            .await
            .unwrap();
        f.state.reload_providers().await.unwrap();
        for stream in [false, true] {
            body["stream"] = json!(stream);
            let p = f
                .state
                .preview_routing_request(input(body.clone(), None, None))
                .await
                .unwrap();
            assert!(p.rows.iter().any(|r| r.provider_id == "text" && r.eligible));
            assert_eq!(f.calls.lock().unwrap().len(), 1);
        }
        for runtime in ["owned-missing-runtime", "owned-disabled-runtime"] {
            for stream in [false, true] {
                // A failed dispatch legitimately starts a health cooldown.
                // Each negative variant owns fresh state instead of bypassing
                // that policy or inheriting a previous variant's rejection.
                let owned = Owned::new().await;
                let checked = std::panic::AssertUnwindSafe(async {
                    let mut disabled = llm_gateway_lib::domain::AgentRuntime::new(
                        "owned-disabled-runtime",
                        "codex",
                        "Owned disabled configuration",
                    );
                    disabled.enabled = false;
                    repo::upsert_agent_runtime(owned.state.db.pool(), &disabled)
                        .await
                        .unwrap();
                    let mut p = repo::list_providers(owned.state.db.pool())
                        .await
                        .unwrap()
                        .into_iter()
                        .find(|p| p.id == "text")
                        .unwrap();
                    p.runtime_id = Some(runtime.into());
                    repo::upsert_provider(owned.state.db.pool(), &p)
                        .await
                        .unwrap();
                    owned.state.reload_providers().await.unwrap();
                    let mut request = text();
                    request["model"] = json!("text:owned-model");
                    request["stream"] = json!(stream);
                    let preview = owned
                        .state
                        .preview_routing_request(input(request.clone(), None, None))
                        .await
                        .unwrap();
                    let reply = owned.send(request).await;
                    let status = reply.status();
                    let error: Value = reply.json().await.unwrap();
                    assert_eq!(status, 400);
                    assert_eq!(error["error"]["type"], "model_capability_unavailable");
                    assert_eq!(
                        owned.calls.lock().unwrap().len(),
                        0,
                        "no account/identity/CLI/model request after local runtime refusal"
                    );
                    let candidate = preview
                        .rows
                        .iter()
                        .find(|r| r.provider_id == "text")
                        .unwrap();
                    assert!(
                        !candidate.eligible,
                        "buffered preview retained an actually rejected runtime: {runtime}"
                    );
                    assert_eq!(candidate.exclusion_reason, Some("runtime_unavailable"));
                    assert!(candidate.ordinal.is_none());
                    assert!(candidate.score.is_none());
                })
                .catch_unwind()
                .await;
                owned.finish(checked).await;
            }
        }
    })
    .catch_unwind()
    .await;
    f.finish(result).await;
}
fn session_input(body: Value, id: &str, include: bool, key: Option<&str>) -> RequestPreviewInput {
    serde_json::from_value(
        json!({"request":body,"session_id":id,"include_session":include,"access_key_id":key}),
    )
    .unwrap()
}
async fn stored_message(f: &Owned, id: &str, content: Value, compacted: bool) {
    sqlx::query("INSERT INTO session_messages(session_id,role,content,content_json,compacted,created_at) VALUES(?,'user','owned projection',?,?,'2026-10-20T00:00:00Z')")
        .bind(id).bind(content.to_string()).bind(i64::from(compacted)).execute(f.state.db.pool()).await.unwrap();
}
async fn deny_preview_writes(f: &Owned, enable: bool) {
    for table in ["sessions", "session_messages", "requests", "usage_daily"] {
        for action in ["INSERT", "UPDATE", "DELETE"] {
            let name = format!("owned_readonly_{table}_{action}");
            let sql = if enable {
                format!("CREATE TRIGGER {name} BEFORE {action} ON {table} BEGIN SELECT RAISE(ABORT,'owned preview attempted write'); END;")
            } else {
                format!("DROP TRIGGER {name}")
            };
            sqlx::raw_sql(&sql)
                .execute(f.state.db.pool())
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn n2ctx_rehydrates_deduplicates_and_never_writes_before_matching_actual_dispatch() {
    let f = Owned::new().await;
    let result=std::panic::AssertUnwindSafe(async {
        let id="owned-context";
        repo::get_or_create_session(f.state.db.pool(),id).await.unwrap();
        let image=json!([{"type":"text","text":"owned retained image"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]);
        stored_message(&f,id,json!("owned compacted history"),true).await;
        stored_message(&f,id,image.clone(),false).await;
        sqlx::query("UPDATE sessions SET summary='owned private summary',title='owned title',sticky_provider_id='vision',sticky_model='vision-physical',sticky_expires_at=? WHERE id=?")
            .bind(chrono::Utc::now().timestamp()+3600).bind(id).execute(f.state.db.pool()).await.unwrap();
        let body=json!({"model":"auto","messages":[{"role":"user","content":"owned compacted history"},{"role":"user","content":image},{"role":"user","content":"owned final tail"}],"max_tokens":16});
        let before=f.counts().await;
        let metadata: String=sqlx::query_scalar("SELECT json_object('title',title,'updated_at',updated_at,'summary',summary,'expires',sticky_expires_at,'tokens',total_tokens,'compact',compact_count) FROM sessions WHERE id=?").bind(id).fetch_one(f.state.db.pool()).await.unwrap();
        deny_preview_writes(&f,true).await;
        let preview=f.state.preview_routing_request(session_input(body.clone(),id,true,None)).await.unwrap();
        assert_eq!(preview.mode,"offline_session_context");
        let c=preview.context.as_ref().unwrap();
        assert_eq!(c.status,"found");assert_eq!(c.history_messages,Some(1));assert_eq!(c.replay_messages,Some(2));assert_eq!(c.summary_present,Some(true));assert_eq!(c.history_capped,Some(false));
        assert_eq!(preview.rows.iter().find(|r|r.ordinal==Some(1)).unwrap().provider_id,"vision");
        let encoded=serde_json::to_string(&preview).unwrap();
        for secret in ["owned private summary","owned retained image","owned compacted history","owned final tail"] {assert!(!encoded.contains(secret));}
        let missing=f.state.preview_routing_request(session_input(text(),"owned-absent",true,None)).await.unwrap();
        assert_eq!(missing.context.unwrap().status,"missing");
        assert_eq!(f.counts().await,before);assert!(f.calls.lock().unwrap().is_empty());
        let after:String=sqlx::query_scalar("SELECT json_object('title',title,'updated_at',updated_at,'summary',summary,'expires',sticky_expires_at,'tokens',total_tokens,'compact',compact_count) FROM sessions WHERE id=?").bind(id).fetch_one(f.state.db.pool()).await.unwrap();assert_eq!(metadata,after);
        deny_preview_writes(&f,false).await;
        let response=reqwest::Client::builder().no_proxy().build().unwrap().post(format!("{}/v1/chat/completions",f.base)).bearer_auth("owned-preview-key").header("x-session-id",id).json(&body).send().await.unwrap();
        assert!(response.status().is_success());assert!(response.text().await.unwrap().contains("owned preview reply"));
        let posts=f.calls.lock().unwrap();assert_eq!(posts.len(),1);assert_eq!(posts[0].0,"vision");
        let messages=posts[0].1["messages"].as_array().unwrap();assert_eq!(messages.len(),3);assert!(messages[0]["content"].as_str().unwrap().contains("owned private summary"));assert_eq!(messages[1]["content"],image);assert_eq!(messages[2]["content"],"owned final tail");
    }).catch_unwind().await;
    f.finish(result).await;
}

#[tokio::test]
async fn n2ctx_sticky_expiry_active_override_and_key_namespaces_follow_existing_routing() {
    let f = Owned::new().await;
    let result=std::panic::AssertUnwindSafe(async {
        let id="owned-shared-logical";
        repo::get_or_create_session(f.state.db.pool(),id).await.unwrap();
        sqlx::query("UPDATE sessions SET sticky_provider_id='vision',sticky_model='vision-physical',sticky_expires_at=? WHERE id=?").bind(chrono::Utc::now().timestamp()+3600).bind(id).execute(f.state.db.pool()).await.unwrap();
        let p=f.state.preview_routing_request(session_input(text(),id,false,None)).await.unwrap();assert!(p.context.is_none());assert_eq!(p.rows[0].provider_id,"text");
        let p=f.state.preview_routing_request(session_input(text(),id,true,None)).await.unwrap();assert_eq!(p.rows[0].provider_id,"vision");assert_eq!(p.context.unwrap().sticky_status,"eligible");
        let response=reqwest::Client::builder().no_proxy().build().unwrap().post(format!("{}/v1/chat/completions",f.base)).bearer_auth("owned-preview-key").header("x-session-id",id).json(&text()).send().await.unwrap();assert!(response.status().is_success());response.text().await.unwrap();assert_eq!(f.calls.lock().unwrap().len(),1);assert_eq!(f.calls.lock().unwrap()[0].0,"vision");
        *f.state.active.write()=Some("text".into());
        let p=f.state.preview_routing_request(session_input(text(),id,true,None)).await.unwrap();assert_eq!(p.rows[0].provider_id,"text");*f.state.active.write()=None;
        sqlx::query("UPDATE sessions SET sticky_expires_at=0 WHERE id=?").bind(id).execute(f.state.db.pool()).await.unwrap();
        let p=f.state.preview_routing_request(session_input(text(),id,true,None)).await.unwrap();assert_eq!(p.context.unwrap().sticky_status,"inactive");assert_eq!(p.rows[0].provider_id,"text");
        for (key,n) in [("owned-key-a",1),("owned-key-b",2)] {
            let scoped=llm_gateway_lib::context::scoped_session_id(id,Some(&format!("remote-key:{key}")));
            repo::get_or_create_session(f.state.db.pool(),&scoped).await.unwrap();for _ in 0..n {stored_message(&f,&scoped,json!("owned namespace history"),false).await;}
            f.state.remote_access_keys.write().push(RemoteAccessKey{id:key.into(),label:"owned".into(),key_hash:"owned-not-a-credential".into(),enabled:true,rpm_limit:0,monthly_budget_micros:0,budget_currency:String::new(),allowed_models:vec!["auto".into()],created_at:chrono::Utc::now(),updated_at:chrono::Utc::now()});
            let p=f.state.preview_routing_request(session_input(text(),id,true,Some(key))).await.unwrap();let c=p.context.unwrap();assert_eq!(c.scope,"selected_access_key");assert_eq!(c.history_messages,Some(n));assert_eq!(c.summary_present,Some(false));
        }
        repo::get_or_create_session(f.state.db.pool(),"only-local").await.unwrap();
        let p=f.state.preview_routing_request(session_input(text(),"only-local",true,Some("owned-key-a"))).await.unwrap();assert_eq!(p.context.unwrap().status,"missing");
        let mut body=text();body["user"]=json!("owned user-derived scope");
        let decoded=serde_json::from_value::<llm_gateway_lib::domain::ChatRequest>({let mut b=body.clone();b["stream"]=json!(false);b}).unwrap();
        let derived=llm_gateway_lib::context::derive_session_id(&decoded,None);repo::get_or_create_session(f.state.db.pool(),&derived).await.unwrap();stored_message(&f,&derived,json!("owned derived history"),false).await;
        let mut derived_input=session_input(body,"",true,None);derived_input.session_id=None;
        let p=f.state.preview_routing_request(derived_input).await.unwrap();assert_eq!(p.context.unwrap().history_messages,Some(1));
        f.state.cfg.write().budget.enabled=true;f.state.remote_access_keys.write()[0].allowed_models=vec!["forbidden".into()];
        let p=f.state.preview_routing_request(session_input(text(),id,true,Some("owned-key-a"))).await.unwrap();let c=p.context.unwrap();assert_eq!(c.status,"key_policy_blocked");assert_eq!(c.history_messages,None);assert_eq!(p.gate_issue,Some("access_key_model_not_allowed"));
        assert!(f.state.preview_routing_request(session_input(text(),id,true,Some("unknown-key"))).await.is_err());assert_eq!(f.calls.lock().unwrap().len(),1);
    }).catch_unwind().await;
    f.finish(result).await;
}

#[tokio::test]
async fn n2ctx_history_cap_and_storage_error_are_explicit_without_implicit_reads() {
    let f = Owned::new().await;
    let result=std::panic::AssertUnwindSafe(async {
        let id="owned-capped";repo::get_or_create_session(f.state.db.pool(),id).await.unwrap();
        sqlx::query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1001) INSERT INTO session_messages(session_id,role,content,created_at) SELECT ?,'user','owned capped row','2026-10-20T00:00:00Z' FROM n").bind(id).execute(f.state.db.pool()).await.unwrap();
        deny_preview_writes(&f,true).await;
        let p=f.state.preview_routing_request(session_input(text(),id,true,None)).await.unwrap();let c=p.context.unwrap();assert_eq!(c.history_capped,Some(true));assert_eq!(c.history_messages,Some(1000));assert_eq!(c.replay_messages,Some(1000));
        deny_preview_writes(&f,false).await;
        sqlx::raw_sql("DROP TABLE session_messages").execute(f.state.db.pool()).await.unwrap();
        let p=f.state.preview_routing_request(session_input(text(),id,false,None)).await.unwrap();assert!(p.context.is_none());
        assert!(f.state.preview_routing_request(session_input(text(),id,true,None)).await.is_err());assert!(f.calls.lock().unwrap().is_empty());
    }).catch_unwind().await;
    f.finish(result).await;
}
impl Owned {
    async fn new() -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let record = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app=Router::new().route("/:target/v1/chat/completions",post(|Path(target):Path<String>,State(record):State<Calls>,Json(body):Json<Value>|async move{
   record.lock().unwrap().push((target,body.clone()));
   if body["stream"]==true {
    let data=format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"owned-preview","choices":[{"index":0,"delta":{"content":"owned preview reply"},"finish_reason":null}]}),json!({"id":"owned-preview","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}));
    return ([("content-type","text/event-stream")],data).into_response();
   }
   Json(json!({"id":"owned-preview","model":body["model"],"choices":[{"index":0,"message":{"role":"assistant","content":"owned preview reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}})).into_response()
  })).fallback(|State(record):State<Calls>,request:axum::extract::Request|async move{
   record.lock().unwrap().push((request.uri().path().to_string(),Value::Null));axum::http::StatusCode::BAD_REQUEST
  }).with_state(record);
        let upstream = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let db = Db::connect_in_memory().await.unwrap();
        for (id, priority, window, tools, vision, stream, dialect) in [
            ("text", 0, 32768, false, false, true, "openai"),
            ("vision", 10, 65536, true, true, true, "openai"),
            ("small", 5, 32, true, true, true, "openai"),
            ("buffered", 50, 32768, true, true, false, "openai"),
            ("anthropic", 90, 32768, true, true, true, "anthropic"),
        ] {
            let p:Provider=serde_json::from_value(json!({"id":id,"name":id,"base_url":format!("{origin}/{id}/v1"),"api_key_enc":"","dialect":dialect,"enabled":true,"priority":priority,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"owned-model","upstream":format!("{id}-physical"),"enabled":true,"context_window":window,"supports_tools":tools,"supports_vision":vision,"supports_audio":vision,"supports_video":vision,"supports_thinking":false,"supports_stream":stream,"model_type":"chat","price":null,"overrides":null}]})).unwrap();
            repo::upsert_provider(db.pool(), &p).await.unwrap();
        }
        let mut cfg = AppConfig {
            port: 0,
            unified_key: "owned-preview-key".into(),
            routing_strategy: RoutingStrategy::Priority,
            ..Default::default()
        };
        cfg.cache.enabled = false;
        cfg.dedup.mode = llm_gateway_lib::proxy::dedup::DedupMode::Off;
        cfg.smart_routing.enabled = true;
        cfg.smart_routing.classifier = llm_gateway_lib::config::SmartClassifier::Jev;
        cfg.smart_routing.jev.base_url = format!("{origin}/jev/v1");
        cfg.smart_routing.jev.model = "owned-classifier".into();
        cfg.search.enabled = false;
        let state = Arc::new(GatewayState::new(db, cfg));
        state.reload_providers().await.unwrap();
        let service = GatewayService::new(state.clone());
        let started = service.set_enabled(true).await.unwrap();
        let base = format!("http://{}", started.bound_addr.unwrap());
        Self {
            state,
            service,
            base,
            calls,
            upstream,
        }
    }
    async fn counts(&self) -> Vec<i64> {
        let mut n = Vec::new();
        for table in ["requests", "sessions", "session_messages", "usage_daily"] {
            n.push(
                sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(self.state.db.pool())
                    .await
                    .unwrap(),
            );
        }
        n
    }
    async fn send(&self, body: Value) -> reqwest::Response {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{}/v1/chat/completions", self.base))
            .bearer_auth("owned-preview-key")
            .json(&body)
            .send()
            .await
            .unwrap()
    }
    async fn finish(&self, result: std::thread::Result<()>) {
        self.service.set_enabled(false).await.unwrap();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
    async fn qualify(&self, id: &str) {
        let p = repo::list_providers(self.state.db.pool())
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap();
        let observed=decode_openrouter(&json!({"data":[{"id":p.models[0].upstream,"context_length":p.models[0].context_window,"architecture":{"input_modalities":["text"],"output_modalities":["text"]},"pricing":{"prompt":"0","completion":"0","request":"0","image":"0","web_search":"0","internal_reasoning":"0","input_cache_read":"0","input_cache_write":"0"}}]})).unwrap();
        let entry = &observed.models[0];
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query("INSERT INTO catalog_inventory(provider_id,upstream_id,target_fingerprint,observation_generation,observed_at,source_contract,fee_status,fee_evidence_json,fee_expires_at,model_json,lifecycle,last_seen_run_id,last_complete_run_id) VALUES(?,?,?,1,?,'openrouter_full_v1','free',?,?,?,'active','owned','owned')")
   .bind(&p.id).bind(&entry.model.id).bind(catalog::target_fingerprint(&p)).bind(now).bind(serde_json::to_string(&entry.fee_evidence).unwrap()).bind(now+catalog::DAY_MS).bind(serde_json::to_string(&entry.model).unwrap()).execute(self.state.db.pool()).await.unwrap();
    }
}
#[tokio::test]
async fn actual_buffered_and_stream_image_tool_dispatch_match_preview_with_zero_preview_sideeffects(
) {
    let f = Owned::new().await;
    let result=std::panic::AssertUnwindSafe(async{
  let mut body=text();body["messages"][0]["content"]=json!([{"type":"text","text":"owned image"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]);body["tools"]=json!([{"type":"function","function":{"name":"owned_tool","parameters":{"type":"object"}}}]);
  for stream in [false,true]{
   body["stream"]=json!(stream);let before=f.counts().await;let posts=f.calls.lock().unwrap().len();let health=serde_json::to_value(f.state.health.snapshot()).unwrap();
   let preview=f.state.preview_routing_request(input(body.clone(),None,None)).await.unwrap();
   assert_eq!(f.counts().await,before);assert_eq!(f.calls.lock().unwrap().len(),posts);assert_eq!(serde_json::to_value(f.state.health.snapshot()).unwrap(),health);
   let first=preview.rows.iter().find(|r|r.ordinal==Some(1)).unwrap();assert_eq!(first.provider_id,"vision");
   assert_eq!(preview.rows.iter().find(|r|r.provider_id=="text").unwrap().exclusion_reason,Some("model_capability_missing"));
   let response=f.send(body.clone()).await;assert!(response.status().is_success());let output=response.text().await.unwrap();assert!(output.contains("owned preview reply"));
   assert_eq!(f.calls.lock().unwrap().len(),posts+1);assert_eq!(f.calls.lock().unwrap().last().unwrap().0,"vision");
  }
 }).catch_unwind().await;
    f.finish(result).await;
}
#[tokio::test]
async fn request_shape_context_key_and_strategy_guards_are_local_and_legacy_preview_compatible() {
    let f = Owned::new().await;
    let result = std::panic::AssertUnwindSafe(async {
        let before = f.counts().await;
        let old = serde_json::to_value(f.state.explain_routing("auto").await.unwrap()).unwrap();
        let mut audio = text();
        audio["messages"][0]["content"] =
            json!([{"type":"input_audio","input_audio":{"data":"AAAA","format":"wav"}}]);
        let a = f
            .state
            .preview_routing_request(input(audio, None, None))
            .await
            .unwrap();
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.provider_id == "anthropic")
                .unwrap()
                .exclusion_reason,
            Some("protocol_media_unsupported")
        );
        let p = f
            .state
            .preview_routing_request(input(text(), None, None))
            .await
            .unwrap();
        assert_eq!(
            p.rows
                .iter()
                .find(|r| r.provider_id == "small")
                .unwrap()
                .exclusion_reason,
            Some("context_window_insufficient")
        );
        let mut huge = text();
        huge["max_tokens"] = json!(100_000);
        let p = f
            .state
            .preview_routing_request(input(huge, None, None))
            .await
            .unwrap();
        assert!(p.rows.iter().all(|r| !r.eligible));
        let p = f
            .state
            .preview_routing_request(input(text(), Some(RoutingStrategy::Smart), None))
            .await
            .unwrap();
        assert!(p.intent.is_some());
        assert_eq!(
            f.state.cfg.read().routing_strategy,
            RoutingStrategy::Priority
        );
        let key = RemoteAccessKey {
            id: "owned-key-id".into(),
            label: "owned metadata".into(),
            key_hash: "owned-no-plaintext".into(),
            enabled: true,
            rpm_limit: 0,
            monthly_budget_micros: 0,
            budget_currency: String::new(),
            allowed_models: vec!["explicit-only".into()],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        f.state.remote_access_keys.write().push(key);
        f.state.cfg.write().budget.enabled = true;
        let p = f
            .state
            .preview_routing_request(input(text(), None, Some("owned-key-id")))
            .await
            .unwrap();
        assert_eq!(p.gate_issue, Some("access_key_model_not_allowed"));
        assert!(p.rows.iter().all(|r| !r.eligible && r.score.is_none()));
        f.state.remote_access_keys.write()[0].allowed_models = vec!["auto".into()];
        let allowed = f
            .state
            .preview_routing_request(input(text(), None, Some("owned-key-id")))
            .await
            .unwrap();
        assert!(allowed.gate_issue.is_none());
        assert!(
            allowed.rows.iter().any(|r| r.eligible),
            "an explicitly allowed key must not be unconditionally rejected"
        );
        assert!(f
            .state
            .preview_routing_request(input(text(), None, Some("missing")))
            .await
            .is_err());
        assert_eq!(
            f.calls.lock().unwrap().len(),
            0,
            "no model, Jev, search, identity or catalog request"
        );
        assert_eq!(f.counts().await, before);
        assert_eq!(
            serde_json::to_value(f.state.explain_routing("auto").await.unwrap()).unwrap(),
            old
        );
    })
    .catch_unwind()
    .await;
    f.finish(result).await;
}
#[tokio::test]
async fn preview_does_not_consume_the_last_owned_request_quota() {
    let f = Owned::new().await;
    let result = std::panic::AssertUnwindSafe(async {
        for mut p in repo::list_providers(f.state.db.pool()).await.unwrap() {
            p.enabled = p.id == "text";
            p.rpm_limit = 1;
            repo::upsert_provider(f.state.db.pool(), &p).await.unwrap();
        }
        f.state.reload_providers().await.unwrap();
        for _ in 0..5 {
            let p = f
                .state
                .preview_routing_request(input(text(), None, None))
                .await
                .unwrap();
            assert_eq!(p.rows.iter().filter(|r| r.eligible).count(), 1);
        }
        let response = f.send(text()).await;
        assert!(response.status().is_success());
        response.text().await.unwrap();
        assert_eq!(f.calls.lock().unwrap().len(), 1);
        let p = f
            .state
            .preview_routing_request(input(text(), None, None))
            .await
            .unwrap();
        assert!(p.rows.iter().all(|r| !r.eligible));
        assert_eq!(f.calls.lock().unwrap().len(), 1);
    })
    .catch_unwind()
    .await;
    f.finish(result).await;
}
#[tokio::test]
async fn qualified_free_preview_keeps_turn_sequence_and_closed_fee_boundaries() {
    let mut sequences = Vec::new();
    for previews in [0, 5] {
        let f = Owned::new().await;
        let mut sequence = Vec::new();
        let result = std::panic::AssertUnwindSafe(async {
            f.state.cfg.write().free_routing.enabled = true;
            let denied = f
                .state
                .preview_routing_request(input(text(), None, None))
                .await
                .unwrap();
            assert_eq!(denied.gate_issue, Some("free_policy_rejected"));
            assert!(denied.rows.iter().all(|r| !r.eligible));
            f.qualify("text").await;
            f.qualify("vision").await;
            let before = f.counts().await;
            for _ in 0..previews {
                let p = f
                    .state
                    .preview_routing_request(input(text(), None, None))
                    .await
                    .unwrap();
                assert_eq!(p.cost_policy, "free_only");
                assert_eq!(p.rows.iter().filter(|r| r.eligible).count(), 2);
                assert!(p.rows.iter().all(|r| r.ordinal.is_none()));
            }
            assert_eq!(f.counts().await, before);
            assert_eq!(f.calls.lock().unwrap().len(), 0);
            let mut hosted = text();
            hosted["tools"] = json!([{"type":"web_search"}]);
            let p = f
                .state
                .preview_routing_request(input(hosted, None, None))
                .await
                .unwrap();
            assert_eq!(p.gate_issue, Some("free_wire_shape_unverified"));
            assert!(p.rows.iter().all(|r| !r.eligible));
            for _ in 0..3 {
                let response = f.send(text()).await;
                assert!(
                    response.status().is_success(),
                    "{}",
                    response.text().await.unwrap()
                );
                response.text().await.unwrap();
            }
            sequence = f
                .calls
                .lock()
                .unwrap()
                .iter()
                .map(|(p, _)| p.clone())
                .collect::<Vec<_>>();
            assert_eq!(sequence.len(), 3);
        })
        .catch_unwind()
        .await;
        f.finish(result).await;
        sequences.push(sequence);
    }
    assert_eq!(
        sequences[0], sequences[1],
        "five request previews must not advance free weighted turns"
    );
}

#[tokio::test]
async fn n2ctx_classification_precedes_local_trimming_like_actual_dispatch() {
    let f = Owned::new().await;
    let result=std::panic::AssertUnwindSafe(async {
        for mut p in repo::list_providers(f.state.db.pool()).await.unwrap() {
            for m in &mut p.models {m.context_window=4096;}
            repo::upsert_provider(f.state.db.pool(),&p).await.unwrap();
        }
        f.state.reload_providers().await.unwrap();
        {let mut cfg=f.state.cfg.write();cfg.routing_strategy=RoutingStrategy::Smart;cfg.smart_routing.classifier=llm_gateway_lib::config::SmartClassifier::Heuristic;cfg.smart_routing.prompt_refine.enabled=false;}
        let body=json!({"model":"auto","messages":[{"role":"user","content":"x".repeat(30_000)},{"role":"assistant","content":"owned older reply"},{"role":"user","content":"hi"}],"max_tokens":16,"tools":[{"type":"function","function":{"name":"owned_tool","parameters":{"type":"object"}}}]});
        let preview=f.state.preview_routing_request(input(body.clone(),None,None)).await.unwrap();
        assert!(preview.trimmed_messages>0);
        let response=f.send(body).await;
        assert!(response.status().is_success());
        assert_eq!(response.headers()["x-route-intent"],"reasoning","actual full input uses the long-tool hard rule before trimming");
        response.text().await.unwrap();
        assert_eq!(f.calls.lock().unwrap().len(),1);
        assert_eq!(preview.intent.unwrap().class,"reasoning","preview must classify the same pre-trim input as actual dispatch");
    }).catch_unwind().await;
    f.finish(result).await;
}
