use super::*;
use crate::proxy::trae::{profile::*, TraeModelClient};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
type ModelPolicySnapshot = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
);
struct Fixture {
    catalog: AtomicUsize,
    model: AtomicUsize,
}
struct Setup {
    gateway: Arc<GatewayState>,
    config: parking_lot::RwLock<AppConfig>,
    fixture: Arc<Fixture>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Setup {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn setup() -> Setup {
    use base64::Engine;
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        base64::engine::general_purpose::STANDARD.encode([73; 32]),
    );
    let fixture = Arc::new(Fixture {
        catalog: AtomicUsize::new(0),
        model: AtomicUsize::new(0),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app=axum::Router::new().fallback(|axum::extract::State(f):axum::extract::State<Arc<Fixture>>,request:axum::extract::Request|async move {
  use axum::response::IntoResponse;
  assert_eq!(request.method(),"POST");
  assert_eq!(request.headers()["x-ide-token"],"fixture-access-secret");
  let path=request.uri().path().to_owned();
  let bytes=axum::body::to_bytes(request.into_body(),1024*1024).await.unwrap();
  let body:Value=serde_json::from_slice(&bytes).unwrap();
  if path==CATALOG_PATH {
   assert_eq!(body,SourceFunction::SoloWorkRemote.catalog_body());
   f.catalog.fetch_add(1,Ordering::SeqCst);
   axum::Json(json!({"config_info_list":[{"config_name":"Exact-A","max_tokens":8192,"reasoning_effort_options":["low","high"]},{"config_name":"Exact-B"}]})).into_response()
  } else {
   assert_eq!(path,MODEL_PATH);assert_eq!(body["model"],"Exact-A");assert_eq!(body["reasoning_effort"],"high");assert!(body.get("allow_undeclared_images").is_none());f.model.fetch_add(1,Ordering::SeqCst);
   ([ ("content-type","text/event-stream") ],"event: output\ndata: {\"response\":\"registered-ok\"}\n\nevent: done\ndata: {\"finish_reason\":\"stop\"}\n\ndata: [DONE]\n\n").into_response()
  }
 }).with_state(fixture.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let path = std::env::temp_dir().join(format!(
        "llmgw-owned-registration-{}.db",
        uuid::Uuid::new_v4()
    ));
    let db = crate::db::Db::connect_path(&path).await.unwrap();
    eprintln!("owned SQLite fixture: {}", path.display());
    let mut cfg = AppConfig {
        port: 0,
        bind: "127.0.0.1".into(),
        unified_key: "fixture-gateway-key".into(),
        ..Default::default()
    };
    cfg.benefits.enabled = true;
    cfg.benefits
        .accounts
        .push(crate::benefit_config::BenefitAccount {
            id: "native-fixture".into(),
            platform: "trae_cn".into(),
            label: "Owned fixture".into(),
            enabled: true,
        });
    let mut gateway = GatewayState::new(db, cfg.clone());
    let auth = gateway.trae_auth.as_ref().unwrap().clone();
    auth.seed_native_session_for_test(
        "fixture-access-secret",
        chrono::Utc::now().timestamp_millis() + 120000,
    )
    .await;
    gateway.trae = Ok(Arc::new(TraeModelClient::for_test(
        auth,
        reqwest::Url::parse(&origin).unwrap(),
    )));
    Setup {
        gateway: Arc::new(gateway),
        config: parking_lot::RwLock::new(cfg),
        fixture,
        task,
    }
}
async fn directory(t: &Setup) -> TraeNativeCatalogView {
    catalog(&t.gateway, &t.config, "native-fixture", "solo_work_remote")
        .await
        .unwrap()
}
fn selection(v: &TraeNativeCatalogView, index: usize, alias: &str) -> NativeSelectionInput {
    NativeSelectionInput {
        selector_id: v.entries[index].selector_id.clone(),
        alias: alias.into(),
        allow_undeclared_images: false,
    }
}
#[tokio::test]
async fn registration_manual_catalog_and_atomic_roundtrip() {
    let t = setup().await;
    let v = directory(&t).await;
    let r = register(
        &t.gateway,
        &t.config,
        "native-fixture",
        v.profile_id.as_str(),
        &v.catalog_revision,
        vec![selection(&v, 0, "trae-cn-exact")],
    )
    .await
    .unwrap();
    assert_eq!(r.model_count, 1);
    let p = repo::list_providers(t.gateway.db.pool()).await.unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].models[0].upstream, "Exact-A");
    assert_eq!(p[0].models[0].context_window, 0);
    assert_eq!(p[0].base_url, ORIGIN);
    assert!(p[0].api_key_enc.is_empty());
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[test]
fn registration_selection_rejects_unknown_private_arguments() {
    assert!(serde_json::from_value::<NativeSelectionInput>(json!({"selector_id":"x","alias":"x","allow_undeclared_images":false,"principal_hash":"secret"})).is_err());
}

async fn register_one(
    t: &Setup,
    v: &TraeNativeCatalogView,
    index: usize,
    alias: &str,
) -> Result<NativeRegistrationResult, String> {
    register(
        &t.gateway,
        &t.config,
        "native-fixture",
        v.profile_id.as_str(),
        &v.catalog_revision,
        vec![selection(v, index, alias)],
    )
    .await
}
async fn counts(t: &Setup) -> (i64, i64, i64) {
    let pool = t.gateway.db.pool();
    (
        sqlx::query_scalar("SELECT COUNT(*) FROM providers")
            .fetch_one(pool)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtimes")
            .fetch_one(pool)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM models")
            .fetch_one(pool)
            .await
            .unwrap(),
    )
}
async fn requested(
    t: &Setup,
    v: &TraeNativeCatalogView,
    alias: &str,
) -> (TraeRuntimeOptions, crate::proxy::trae::NativeCatalogTicket) {
    let bound = binding(&t.gateway, &t.config, "native-fixture")
        .await
        .unwrap();
    let ticket = t
        .gateway
        .trae
        .as_ref()
        .unwrap()
        .consume_cached_catalog(
            &bound,
            SourceFunction::SoloWorkRemote,
            &v.catalog_revision,
            &v.entries[0].selector_id,
        )
        .await
        .unwrap();
    let request = TraeRuntimeOptions {
        region: "cn".into(),
        account_id: bound.account_id.clone(),
        principal_hash: bound.principal_hash.clone(),
        profile_id: bound.profile_id.as_str().into(),
        model_selections: BTreeMap::from([(
            alias.into(),
            TraeModelSelection::from_ticket(&ticket, false),
        )]),
    };
    (request, ticket)
}
#[tokio::test]
async fn registration_saved_alias_reaches_authenticated_gateway_exactly_once() {
    let t = setup().await;
    let v = directory(&t).await;
    let r = register_one(&t, &v, 0, "trae-cn-exact").await.unwrap();
    assert_eq!(r.model_count, 1);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
    let mut call_cfg = t.gateway.cfg_snapshot();
    call_cfg.benefits.enabled = false;
    t.gateway.update_cfg(call_cfg);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let gw = t.gateway.clone();
    let task = tokio::spawn(async move {
        crate::proxy::server::serve_ready(gw, ready_tx, shutdown_rx)
            .await
            .unwrap()
    });
    let address = ready_rx.await.unwrap().unwrap();
    let response=reqwest::Client::new().post(format!("http://{address}/v1/chat/completions")).bearer_auth("fixture-gateway-key").json(&json!({"model":"trae-cn-exact","messages":[{"role":"user","content":"explicit caller request"}],"reasoning_effort":"high"})).send().await.unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("registered-ok"), "{body}");
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 1);
    let _ = shutdown_tx.send(());
    task.await.unwrap();
}
#[tokio::test]
async fn registration_idempotent_selected_count_and_preserves_other_rows_policy_defaults() {
    let t = setup().await;
    let v = directory(&t).await;
    let r = register(
        &t.gateway,
        &t.config,
        "native-fixture",
        v.profile_id.as_str(),
        &v.catalog_revision,
        vec![selection(&v, 0, "trae-cn-a"), selection(&v, 1, "trae-cn-b")],
    )
    .await
    .unwrap();
    assert_eq!(r.model_count, 2);
    let cfg = serde_json::to_value(t.config.read().clone()).unwrap();
    *t.gateway.active.write() = Some("unrelated-default".into());
    let pool = t.gateway.db.pool();
    sqlx::query("UPDATE providers SET enabled=0,name='User provider',priority=77 WHERE id=?")
        .bind(&r.provider_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE agent_runtimes SET enabled=0,label='User runtime' WHERE id=?")
        .bind(&r.runtime_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE models SET enabled=0,price_json=?,overrides_json=?,local_json=?,capabilities_json=? WHERE provider_id=? AND alias='trae-cn-a'").bind("{\"prompt\":1.25,\"completion\":4.5,\"currency\":\"usd\",\"source\":\"manual\"}").bind("{\"max_tokens\":12}").bind("{\"runtime\":\"fixture-owned\"}").bind("{\"owned_capabilities\":1}").bind(&r.provider_id).execute(pool).await.unwrap();
    let before:Vec<ModelPolicySnapshot>=sqlx::query_as("SELECT alias,upstream,price_json,overrides_json,local_json,capabilities_json,enabled FROM models ORDER BY alias").fetch_all(pool).await.unwrap();
    let again = register_one(&t, &v, 0, "trae-cn-a").await.unwrap();
    assert_eq!(again.model_count, 1);
    assert_eq!(again.provider_id, r.provider_id);
    assert_eq!(again.runtime_id, r.runtime_id);
    assert!(again
        .warnings
        .iter()
        .any(|v| v == "native_existing_model_settings_preserved"));
    assert!(again
        .warnings
        .iter()
        .any(|v| v == "native_existing_runtime_disabled"));
    let after:Vec<ModelPolicySnapshot>=sqlx::query_as("SELECT alias,upstream,price_json,overrides_json,local_json,capabilities_json,enabled FROM models ORDER BY alias").fetch_all(pool).await.unwrap();
    assert_eq!(before, after);
    assert_eq!(counts(&t).await, (1, 1, 2));
    assert_eq!(serde_json::to_value(t.config.read().clone()).unwrap(), cfg);
    assert_eq!(
        t.gateway.active.read().as_deref(),
        Some("unrelated-default")
    );
    let rt = repo::get_agent_runtime(pool, &r.runtime_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!rt.enabled);
    assert_eq!(rt.label, "User runtime");
    assert_eq!(
        crate::agent_upstream::runtime::trae_options(rt.options.as_ref())
            .unwrap()
            .model_selections
            .len(),
        2
    );
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn registration_bad_selections_and_foreign_revision_are_local_no_writes() {
    let t = setup().await;
    let v = directory(&t).await;
    let valid = selection(&v, 0, "trae-cn-a");
    for selections in [
        vec![],
        vec![valid.clone(), valid.clone()],
        vec![valid.clone(); 513],
        vec![selection(&v, 0, "auto")],
        vec![selection(&v, 0, "bad alias")],
        vec![NativeSelectionInput {
            selector_id: "a".repeat(64),
            ..valid.clone()
        }],
    ] {
        assert!(register(
            &t.gateway,
            &t.config,
            "native-fixture",
            v.profile_id.as_str(),
            &v.catalog_revision,
            selections
        )
        .await
        .is_err());
        assert_eq!(counts(&t).await, (0, 0, 0));
    }
    for (profile, revision) in [
        ("unknown", v.catalog_revision.clone()),
        (v.profile_id.as_str(), uuid::Uuid::new_v4().to_string()),
        (v.profile_id.as_str(), "invalid".into()),
    ] {
        assert!(register(
            &t.gateway,
            &t.config,
            "native-fixture",
            profile,
            &revision,
            vec![valid.clone()]
        )
        .await
        .is_err());
    }
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn registration_expired_tickets_never_refetch() {
    let t = setup().await;
    let v = directory(&t).await;
    t.gateway
        .trae
        .as_ref()
        .unwrap()
        .expire_cached_catalogs_for_test();
    assert!(register_one(&t, &v, 0, "trae-cn-a").await.is_err());
    assert_eq!(counts(&t).await, (0, 0, 0));
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn registration_changed_generation_cannot_consume_old_revision() {
    let t = setup().await;
    let v = directory(&t).await;
    t.gateway
        .trae_auth
        .as_ref()
        .unwrap()
        .seed_native_session_for_test(
            "fixture-new-access",
            chrono::Utc::now().timestamp_millis() + 120000,
        )
        .await;
    assert!(register_one(&t, &v, 0, "trae-cn-a").await.is_err());
    assert_eq!(counts(&t).await, (0, 0, 0));
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn registration_wrong_disabled_removed_global_duplicate_accounts_zero_http() {
    for mode in 0..6 {
        let t = setup().await;
        match mode {
            0 => t.config.write().benefits.enabled = false,
            1 => t.config.write().benefits.accounts[0].enabled = false,
            2 => t.config.write().benefits.accounts[0].platform = "trae_global".into(),
            3 => t.config.write().benefits.accounts.clear(),
            4 => {
                let a = t.config.read().benefits.accounts[0].clone();
                t.config.write().benefits.accounts.push(a);
            }
            _ => {}
        }
        let account = if mode == 5 {
            "missing"
        } else {
            "native-fixture"
        };
        assert!(catalog(&t.gateway, &t.config, account, "solo_work_remote")
            .await
            .is_err());
        assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 0);
        assert_eq!(counts(&t).await, (0, 0, 0));
    }
}
#[tokio::test]
async fn registration_unsupported_function_and_expired_auth_zero_http() {
    let t = setup().await;
    assert!(
        catalog(&t.gateway, &t.config, "native-fixture", "solo_agent_remote")
            .await
            .is_err()
    );
    t.gateway
        .trae_auth
        .as_ref()
        .unwrap()
        .seed_native_session_for_test("fixture-expired", chrono::Utc::now().timestamp_millis() - 1)
        .await;
    assert!(
        catalog(&t.gateway, &t.config, "native-fixture", "solo_work_remote")
            .await
            .is_err()
    );
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 0);
    assert_eq!(counts(&t).await, (0, 0, 0));
}
#[tokio::test]
async fn registration_alias_conflicts_include_disabled_legacy_alias_and_upstream() {
    for upstream in [false, true] {
        let t = setup().await;
        let v = directory(&t).await;
        sqlx::query("INSERT INTO providers(id,name,dialect,base_url,api_key_enc,enabled,priority,rpm_limit,intelligence,created_at,updated_at) VALUES('foreign','Foreign','openai','http://127.0.0.1','','0',0,0,0,?,?)").bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(t.gateway.db.pool()).await.unwrap();
        sqlx::query("INSERT INTO models(id,provider_id,alias,upstream,enabled) VALUES('foreign-row','foreign',?,?,0)").bind(if upstream{"other"}else{"trae-cn-a"}).bind(if upstream{"trae-cn-a"}else{"Foreign-A"}).execute(t.gateway.db.pool()).await.unwrap();
        assert!(register_one(&t, &v, 0, "trae-cn-a").await.is_err());
        assert_eq!(counts(&t).await, (1, 0, 1));
        assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn registration_hash_identity_collision_and_model_primary_key_never_replace() {
    for mode in 0..3 {
        let t = setup().await;
        let v = directory(&t).await;
        let (request, _) = requested(&t, &v, "trae-cn-a").await;
        let (pid, rid) = repo::trae_registration::native_ids(&request.binding());
        match mode {
            0 => {
                sqlx::query("INSERT INTO agent_runtimes(id,kind,label,options_json,enabled,created_at,updated_at) VALUES(?,'cli','Foreign','{}',1,?,?)").bind(&rid).bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(t.gateway.db.pool()).await.unwrap();
            }
            1 => {
                sqlx::query("INSERT INTO providers(id,name,dialect,base_url,api_key_enc,enabled,priority,rpm_limit,intelligence,created_at,updated_at) VALUES(?,'Foreign','openai','http://127.0.0.1','',1,0,0,0,?,?)").bind(&pid).bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(t.gateway.db.pool()).await.unwrap();
            }
            _ => {
                sqlx::query("INSERT INTO providers(id,name,dialect,base_url,api_key_enc,enabled,priority,rpm_limit,intelligence,created_at,updated_at) VALUES('foreign','Foreign','openai','http://127.0.0.1','',1,0,0,0,?,?)").bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(t.gateway.db.pool()).await.unwrap();
                sqlx::query("INSERT INTO models(id,provider_id,alias,upstream) VALUES(?,'foreign','unrelated','Foreign-A')").bind(format!("{pid}:trae-cn-a")).execute(t.gateway.db.pool()).await.unwrap();
            }
        }
        let before = counts(&t).await;
        assert!(register_one(&t, &v, 0, "trae-cn-a").await.is_err());
        assert_eq!(counts(&t).await, before);
        assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn registration_alias_cannot_change_selector_or_function() {
    let t = setup().await;
    let v = directory(&t).await;
    register_one(&t, &v, 0, "trae-cn-a").await.unwrap();
    let before = counts(&t).await;
    assert!(register_one(&t, &v, 1, "trae-cn-a").await.is_err());
    assert_eq!(counts(&t).await, before);
    let p = repo::list_providers(t.gateway.db.pool()).await.unwrap();
    assert_eq!(p[0].models[0].upstream, "Exact-A");
}
#[tokio::test]
async fn registration_commit_barrier_invalidations_rollback_all_rows() {
    for mode in 0..3 {
        let t = setup().await;
        let v = directory(&t).await;
        let (request, ticket) = requested(&t, &v, "trae-cn-a").await;
        let auth = t.gateway.trae_auth.as_ref().unwrap();
        let gate = auth
            .lock_model_registration(ticket.binding(), ticket.generation())
            .await
            .unwrap();
        let result =
            repo::trae_registration::register_native(t.gateway.db.pool(), &request, || {
                match mode {
                    0 => t.config.write().benefits.accounts[0].enabled = false,
                    1 => auth.set_proxy(Some("http://127.0.0.1:9")),
                    _ => t
                        .gateway
                        .trae
                        .as_ref()
                        .unwrap()
                        .expire_cached_catalogs_for_test(),
                }
                require_trae_catalog_account(&t.config.read().benefits, "native-fixture")?;
                gate.current()?;
                t.gateway
                    .trae
                    .as_ref()
                    .unwrap()
                    .registration_ticket_current(&ticket)
            })
            .await;
        drop(gate);
        assert!(result.is_err());
        assert_eq!(counts(&t).await, (0, 0, 0));
        assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn registration_auth_mutation_gate_serializes_callback_and_commit() {
    let t = setup().await;
    let v = directory(&t).await;
    let (request, ticket) = requested(&t, &v, "trae-cn-a").await;
    let auth = t.gateway.trae_auth.as_ref().unwrap().clone();
    let gate = auth
        .lock_model_registration(ticket.binding(), ticket.generation())
        .await
        .unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let auth2 = auth.clone();
    let writer = tokio::spawn(async move {
        let _ = entered_tx.send(());
        let _guard = auth2.model_gate_for_test().await;
        auth2
            .seed_native_session_for_test(
                "fixture-new-access",
                chrono::Utc::now().timestamp_millis() + 120000,
            )
            .await;
    });
    entered_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!writer.is_finished());
    repo::trae_registration::register_native(t.gateway.db.pool(), &request, || {
        gate.current()?;
        assert!(!writer.is_finished());
        Ok(())
    })
    .await
    .unwrap();
    drop(gate);
    writer.await.unwrap();
    assert_eq!(counts(&t).await, (1, 1, 1));
    assert!(auth
        .lock_model_registration(ticket.binding(), ticket.generation())
        .await
        .is_err());
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn registration_concurrent_idempotent_merges_keep_one_binding() {
    let t = setup().await;
    let v = directory(&t).await;
    let (a, b) = tokio::join!(
        register_one(&t, &v, 0, "trae-cn-a"),
        register_one(&t, &v, 0, "trae-cn-a")
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.provider_id, b.provider_id);
    assert_eq!(a.model_count, 1);
    assert_eq!(b.model_count, 1);
    assert_eq!(counts(&t).await, (1, 1, 1));
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn registration_generic_binding_and_provider_guards_preserve_native_identity() {
    let t = setup().await;
    let v = directory(&t).await;
    let r = register_one(&t, &v, 0, "trae-cn-a").await.unwrap();
    let pool = t.gateway.db.pool();
    assert!(repo::set_provider_runtime(pool, &r.provider_id, None)
        .await
        .is_err());
    assert!(
        repo::set_provider_runtime(pool, &r.provider_id, Some(&r.runtime_id))
            .await
            .unwrap()
    );
    let original = repo::list_providers(pool).await.unwrap().remove(0);
    for mode in 0..7 {
        let mut p = original.clone();
        match mode {
            0 => p.base_url = "https://foreign.invalid".into(),
            1 => p.api_key_enc = "fake".into(),
            2 => p.runtime_id = None,
            3 => p.models.clear(),
            4 => p.models[0].upstream = "Foreign".into(),
            5 => p.models[0].alias = "changed".into(),
            _ => {
                p.models[0].overrides = Some(
                    serde_json::from_value(
                        json!({"extra_headers":[{"name":"x-local","value":"no"}]}),
                    )
                    .unwrap(),
                )
            }
        };
        assert!(repo::upsert_provider(pool, &p).await.is_err());
    }
    let mut safe = original.clone();
    safe.name = "User name".into();
    safe.enabled = false;
    safe.models[0].enabled = false;
    safe.models[0].overrides = Some(serde_json::from_value(json!({"max_tokens":12})).unwrap());
    repo::upsert_provider_config(pool, &safe).await.unwrap();
    let saved = repo::list_providers(pool).await.unwrap().remove(0);
    assert_eq!(saved.name, "User name");
    assert_eq!(saved.runtime_id, Some(r.runtime_id));
    assert!(!saved.models[0].enabled);
    assert_eq!(saved.models[0].upstream, "Exact-A");
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[test]
fn registration_binding_hash_covers_account_principal_region_and_profile() {
    let a = TraeModelBinding {
        region: TraeRegion::Cn,
        account_id: "one".into(),
        principal_hash: "a".repeat(64),
        profile_id: TraeProfileId::CnOrdinary343Aicc20260212V1,
    };
    let id = repo::trae_registration::native_ids(&a);
    let mut b = a.clone();
    b.account_id = "two".into();
    assert_ne!(id, repo::trae_registration::native_ids(&b));
    b = a.clone();
    b.principal_hash = "b".repeat(64);
    assert_ne!(id, repo::trae_registration::native_ids(&b));
    b = a;
    b.region = TraeRegion::Global;
    assert_ne!(id, repo::trae_registration::native_ids(&b));
}

#[test]
fn registration_flat_ipc_arguments_reject_secrets_host_and_unknown_keys() {
    for good in [json!({"accountId":"native-fixture","sourceFunction":"solo_work_remote"})] {
        let body = tauri::ipc::InvokeBody::Json(good.clone());
        validate_ipc(&body, &["accountId", "sourceFunction"]).unwrap();
        for key in ["token", "host", "principalHash", "unknown"] {
            let mut bad = good.clone();
            bad[key] = json!("forbidden");
            assert!(validate_ipc(
                &tauri::ipc::InvokeBody::Json(bad),
                &["accountId", "sourceFunction"]
            )
            .is_err());
        }
    }
}
#[tokio::test]
async fn registration_discovery_uses_only_saved_metadata_even_if_disabled() {
    let t = setup().await;
    let v = directory(&t).await;
    let r = register_one(&t, &v, 0, "trae-cn-a").await.unwrap();
    let pool = t.gateway.db.pool();
    sqlx::query("UPDATE agent_runtimes SET enabled=0 WHERE id=?")
        .bind(&r.runtime_id)
        .execute(pool)
        .await
        .unwrap();
    let p = repo::list_providers(pool).await.unwrap().remove(0);
    let rt = repo::get_agent_runtime(pool, &r.runtime_id)
        .await
        .unwrap()
        .unwrap();
    let input = model_catalog::DiscoveryInput {
        provider_id: Some(p.id.clone()),
        dialect: Dialect::OpenAI,
        base_url: p.base_url.clone(),
        api_key: String::new(),
    };
    let result = saved_discovery(&input, &p, &rt).unwrap();
    assert_eq!(result.models.len(), 1);
    assert_eq!(result.models[0].id, "Exact-A");
    assert_eq!(result.models[0].context_window, 0);
    assert_eq!(result.models[0].supports_thinking, None);
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn registration_reasoning_validator_closed_values_and_no_capability_claim() {
    let t = setup().await;
    let v = directory(&t).await;
    let (mut request, _) = requested(&t, &v, "trae-cn-a").await;
    assert_eq!(
        request.model_selections["trae-cn-a"].reasoning_effort_options,
        vec!["low", "high"]
    );
    let raw = serde_json::to_value(&request).unwrap();
    crate::agent_upstream::runtime::trae_options(Some(&raw)).unwrap();
    for values in [vec!["high", "high"], vec!["auto"], vec!["HIGH"]] {
        request
            .model_selections
            .get_mut("trae-cn-a")
            .unwrap()
            .reasoning_effort_options = values.iter().map(|v| v.to_string()).collect();
        let raw = serde_json::to_value(&request).unwrap();
        assert!(crate::agent_upstream::runtime::trae_options(Some(&raw)).is_err());
    }
    request
        .model_selections
        .get_mut("trae-cn-a")
        .unwrap()
        .reasoning_effort_options = ["minimal", "low", "medium", "high", "xhigh"]
        .iter()
        .map(|v| v.to_string())
        .collect();
    let raw = serde_json::to_value(&request).unwrap();
    crate::agent_upstream::runtime::trae_options(Some(&raw)).unwrap();
    assert_eq!(
        request.model_selections["trae-cn-a"].declared_thinking,
        None
    );
}

#[tokio::test]
async fn registration_sql_insert_failure_rolls_back_new_and_existing_merge() {
    for existing in [false, true] {
        let t = setup().await;
        let v = directory(&t).await;
        if existing {
            register_one(&t, &v, 0, "trae-cn-a").await.unwrap();
        }
        let before = counts(&t).await;
        let before_options: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT id,options_json FROM agent_runtimes ORDER BY id")
                .fetch_all(t.gateway.db.pool())
                .await
                .unwrap();
        sqlx::query("CREATE TRIGGER fixture_reject_b BEFORE INSERT ON models WHEN NEW.upstream='Exact-B' BEGIN SELECT RAISE(ABORT,'owned fixture failure'); END").execute(t.gateway.db.pool()).await.unwrap();
        let result = register(
            &t.gateway,
            &t.config,
            "native-fixture",
            v.profile_id.as_str(),
            &v.catalog_revision,
            vec![selection(&v, 0, "trae-cn-a"), selection(&v, 1, "trae-cn-b")],
        )
        .await;
        assert!(result.is_err());
        assert_eq!(counts(&t).await, before);
        let after_options: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT id,options_json FROM agent_runtimes ORDER BY id")
                .fetch_all(t.gateway.db.pool())
                .await
                .unwrap();
        assert_eq!(before_options, after_options);
        assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
    }
}
async fn fixture_credential(t: &Setup) -> Value {
    let raw = repo::get_secret(
        t.gateway.db.pool(),
        "account-login:trae:cn:session:native-fixture",
    )
    .await
    .unwrap()
    .unwrap();
    serde_json::from_str(&crypto::decrypt(&raw).unwrap()).unwrap()
}
#[tokio::test]
async fn registration_changed_principal_gets_distinct_binding_keeps_old_runtime() {
    let t = setup().await;
    let v = directory(&t).await;
    let old = register_one(&t, &v, 0, "trae-cn-old").await.unwrap();
    let original = repo::get_agent_runtime(t.gateway.db.pool(), &old.runtime_id)
        .await
        .unwrap()
        .unwrap();
    let mut credential = fixture_credential(&t).await;
    credential["uid"] = json!("other-native-owner");
    credential["snapshot"]["identity_hash"] = json!(format!(
        "{:x}",
        Sha256::digest(b"trae_cn:other-native-owner")
    ));
    let enc = crypto::encrypt(&credential.to_string()).unwrap();
    repo::set_secret(
        t.gateway.db.pool(),
        "account-login:trae:cn:session:native-fixture",
        &enc,
    )
    .await
    .unwrap();
    assert!(register_one(&t, &v, 0, "trae-cn-old").await.is_err());
    let new_view = directory(&t).await;
    let new = register_one(&t, &new_view, 0, "trae-cn-new").await.unwrap();
    assert_ne!(old.provider_id, new.provider_id);
    assert_ne!(old.runtime_id, new.runtime_id);
    assert_eq!(counts(&t).await, (2, 2, 2));
    let old_now = repo::get_agent_runtime(t.gateway.db.pool(), &old.runtime_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old_now.options, original.options);
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 2);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn registration_pending_relogin_zero_http_and_no_ticket_reuse() {
    let t = setup().await;
    let v = directory(&t).await;
    let credential = fixture_credential(&t).await;
    let pending = json!({"account":"native-fixture","id":"fixture-pending","deadline":chrono::Utc::now().timestamp_millis()+120000,"baseline":null,"expected_identity_hash":null,"state":"waiting","device":credential["device"],"material":{"code_verifier":"fixture","code_challenge":"fixture","challenge_method":"S256","private_key":"fixture","public_key":"fixture"}});
    repo::set_secret(
        t.gateway.db.pool(),
        "account-login:trae:cn:pending:fixture-pending",
        &crypto::encrypt(&pending.to_string()).unwrap(),
    )
    .await
    .unwrap();
    assert!(
        catalog(&t.gateway, &t.config, "native-fixture", "solo_work_remote")
            .await
            .is_err()
    );
    assert!(register_one(&t, &v, 0, "trae-cn-a").await.is_err());
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 1);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
    assert_eq!(counts(&t).await, (0, 0, 0));
}
#[tokio::test]
async fn registration_explicit_image_candidate_policy_is_local_not_capability() {
    let t = setup().await;
    let v = directory(&t).await;
    let mut selected = selection(&v, 0, "trae-cn-images");
    selected.allow_undeclared_images = true;
    let r = register(
        &t.gateway,
        &t.config,
        "native-fixture",
        v.profile_id.as_str(),
        &v.catalog_revision,
        vec![selected],
    )
    .await
    .unwrap();
    let rt = repo::get_agent_runtime(t.gateway.db.pool(), &r.runtime_id)
        .await
        .unwrap()
        .unwrap();
    let options = crate::agent_upstream::runtime::trae_options(rt.options.as_ref()).unwrap();
    let selected = &options.model_selections["trae-cn-images"];
    assert!(selected.allow_undeclared_images);
    assert_eq!(selected.declared_vision, None);
    let mut denied = serde_json::to_value(&options).unwrap();
    denied["model_selections"]["trae-cn-images"]["declared_vision"] = json!(false);
    assert!(crate::agent_upstream::runtime::trae_options(Some(&denied)).is_err());
    let p = repo::list_providers(t.gateway.db.pool())
        .await
        .unwrap()
        .remove(0);
    assert!(!p.models[0].supports_vision);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn registration_runtime_editor_cannot_repoint_or_overwrite_selections() {
    let t = setup().await;
    let v = directory(&t).await;
    let r = register_one(&t, &v, 0, "trae-cn-a").await.unwrap();
    let pool = t.gateway.db.pool();
    let original = repo::get_agent_runtime(pool, &r.runtime_id)
        .await
        .unwrap()
        .unwrap();
    let mut safe = original.clone();
    safe.enabled = false;
    safe.label = "User runtime".into();
    repo::trae_registration::save_native_runtime_settings(pool, &safe)
        .await
        .unwrap();
    for mode in 0..3 {
        let mut bad = safe.clone();
        match mode {
            0 => bad.options.as_mut().unwrap()["principal_hash"] = json!("b".repeat(64)),
            1 => bad.kind = "cli".into(),
            _ => bad.options.as_mut().unwrap()["host"] = json!("https://foreign.invalid"),
        };
        assert!(
            repo::trae_registration::save_native_runtime_settings(pool, &bad)
                .await
                .is_err()
        );
    }
    register_one(&t, &v, 1, "trae-cn-b").await.unwrap();
    assert!(
        repo::trae_registration::save_native_runtime_settings(pool, &safe)
            .await
            .is_err()
    );
    let current = repo::get_agent_runtime(pool, &r.runtime_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        crate::agent_upstream::runtime::trae_options(current.options.as_ref())
            .unwrap()
            .model_selections
            .len(),
        2
    );
    assert!(!current.enabled);
    assert_eq!(current.label, "User runtime");
}

#[tokio::test]
async fn registration_after_commit_account_change_returns_saved_unavailable_warning() {
    let t = setup().await;
    let v = directory(&t).await;
    let result = register_impl(
        &t.gateway,
        &t.config,
        "native-fixture",
        v.profile_id.as_str(),
        &v.catalog_revision,
        vec![selection(&v, 0, "trae-cn-a")],
        || {
            t.config.write().benefits.accounts[0].enabled = false;
            let mut cfg = t.gateway.cfg_snapshot();
            cfg.benefits.accounts[0].enabled = false;
            t.gateway.update_cfg(cfg);
        },
    )
    .await
    .unwrap();
    assert_eq!(result.model_count, 1);
    assert!(result
        .warnings
        .iter()
        .any(|w| w == "native_registration_saved_account_currently_unavailable"));
    assert_eq!(counts(&t).await, (1, 1, 1));
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn registration_corrupt_saved_native_options_return_error_without_panic_or_writes() {
    for raw in [None, Some("{"), Some("{}")] {
        let t = setup().await;
        let v = directory(&t).await;
        let r = register_one(&t, &v, 0, "trae-cn-a").await.unwrap();
        let pool = t.gateway.db.pool();
        let p = repo::list_providers(pool).await.unwrap().remove(0);
        let rt = repo::get_agent_runtime(pool, &r.runtime_id)
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE agent_runtimes SET options_json=? WHERE id=?")
            .bind(raw)
            .bind(&r.runtime_id)
            .execute(pool)
            .await
            .unwrap();
        let before = counts(&t).await;
        assert!(register_one(&t, &v, 0, "trae-cn-a").await.is_err());
        assert!(repo::upsert_provider(pool, &p).await.is_err());
        assert!(
            repo::trae_registration::save_native_runtime_settings(pool, &rt)
                .await
                .is_err()
        );
        assert_eq!(counts(&t).await, before);
        let after: Option<String> =
            sqlx::query_scalar("SELECT options_json FROM agent_runtimes WHERE id=?")
                .bind(&r.runtime_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(after.as_deref(), raw);
        assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
    }
}

type MutationRuntimeRow = (String, String, String, Option<String>, i64, String, String);
type MutationSnapshot = (
    Vec<MutationRuntimeRow>,
    Vec<(String, Option<String>)>,
    Vec<(String, String, String)>,
);
async fn mutation_snapshot(t: &Setup) -> MutationSnapshot {
    let p = t.gateway.db.pool();
    (sqlx::query_as("SELECT id,kind,label,options_json,enabled,created_at,updated_at FROM agent_runtimes ORDER BY id").fetch_all(p).await.unwrap(),sqlx::query_as("SELECT id,runtime_id FROM providers ORDER BY id").fetch_all(p).await.unwrap(),sqlx::query_as("SELECT id,alias,upstream FROM models ORDER BY id").fetch_all(p).await.unwrap())
}
#[tokio::test]
async fn mutation_generic_save_cannot_overwrite_committed_native_before_cache_publication() {
    let t = setup().await;
    let v = directory(&t).await;
    let (request, _) = requested(&t, &v, "trae-cn-a").await;
    let (_, rid, _) =
        repo::trae_registration::register_native(t.gateway.db.pool(), &request, || Ok(()))
            .await
            .unwrap();
    assert!(!t.gateway.is_native_runtime(&rid));
    let before = mutation_snapshot(&t).await;
    let ordinary = crate::domain::AgentRuntime::new(&rid, "codex", "ordinary payload");
    let result = repo::upsert_agent_runtime(t.gateway.db.pool(), &ordinary).await;
    assert!(
        result.is_err(),
        "generic save replaced committed native runtime while cache was unpublished"
    );
    assert_eq!(mutation_snapshot(&t).await, before);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn mutation_delete_rechecks_reference_after_stale_command_precheck() {
    let t = setup().await;
    let v = directory(&t).await;
    let (request, _) = requested(&t, &v, "trae-cn-a").await;
    let (_, rid) = repo::trae_registration::native_ids(&request.binding());
    assert!(repo::providers_using_runtime(t.gateway.db.pool(), &rid)
        .await
        .unwrap()
        .is_empty());
    repo::trae_registration::register_native(t.gateway.db.pool(), &request, || Ok(()))
        .await
        .unwrap();
    let before = mutation_snapshot(&t).await;
    let result = repo::delete_agent_runtime(t.gateway.db.pool(), &rid).await;
    assert!(
        result.is_err(),
        "delete used stale unreferenced precheck and left a dangling provider binding"
    );
    assert_eq!(mutation_snapshot(&t).await, before);
    assert_eq!(counts(&t).await, (1, 1, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutation_registration_writer_first_serializes_queued_generic_save_and_delete() {
    let t = setup().await;
    let v = directory(&t).await;
    let (request, _) = requested(&t, &v, "trae-cn-a").await;
    let (_, rid) = repo::trae_registration::native_ids(&request.binding());
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let gw = t.gateway.clone();
    let reg = tokio::spawn(async move {
        repo::trae_registration::register_native(gw.db.pool(), &request, move || {
            signal.notify_one();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| "owned commit barrier timeout".to_string())?;
            Ok(())
        })
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let (saved_tx, saved_rx) = tokio::sync::oneshot::channel();
    let pool = t.gateway.db.pool().clone();
    let ordinary = crate::domain::AgentRuntime::new(&rid, "codex", "queued ordinary");
    let save = tokio::spawn(async move {
        let _ = saved_tx.send(());
        repo::upsert_agent_runtime(&pool, &ordinary).await
    });
    let (deleted_tx, deleted_rx) = tokio::sync::oneshot::channel();
    let pool = t.gateway.db.pool().clone();
    let id = rid.clone();
    let delete = tokio::spawn(async move {
        let _ = deleted_tx.send(());
        repo::delete_agent_runtime(&pool, &id).await
    });
    saved_rx.await.unwrap();
    deleted_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!save.is_finished());
    assert!(!delete.is_finished());
    release_tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), reg)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(save.await.unwrap().is_err());
    assert!(delete.await.unwrap().is_err());
    assert_eq!(counts(&t).await, (1, 1, 1));
    let runtime = repo::get_agent_runtime(t.gateway.db.pool(), &rid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.kind, "trae-model");
    assert!(!t.gateway.is_native_runtime(&rid));
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn mutation_ordinary_writer_first_rejects_registration_preserves_crud_and_created_at() {
    let t = setup().await;
    let v = directory(&t).await;
    let (request, _) = requested(&t, &v, "trae-cn-a").await;
    let (_, rid) = repo::trae_registration::native_ids(&request.binding());
    let mut ordinary = crate::domain::AgentRuntime::new(&rid, "codex", "first ordinary");
    repo::upsert_agent_runtime(t.gateway.db.pool(), &ordinary)
        .await
        .unwrap();
    let created = repo::get_agent_runtime(t.gateway.db.pool(), &rid)
        .await
        .unwrap()
        .unwrap()
        .created_at;
    ordinary.label = "updated ordinary".into();
    ordinary.enabled = false;
    ordinary.options = Some(json!({"owned_setting":true}));
    ordinary.created_at += chrono::Duration::days(1);
    repo::upsert_agent_runtime(t.gateway.db.pool(), &ordinary)
        .await
        .unwrap();
    let saved = repo::get_agent_runtime(t.gateway.db.pool(), &rid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.created_at, created);
    assert_eq!(saved.label, "updated ordinary");
    assert!(!saved.enabled);
    let before = mutation_snapshot(&t).await;
    assert!(
        repo::trae_registration::register_native(t.gateway.db.pool(), &request, || Ok(()))
            .await
            .is_err()
    );
    assert_eq!(mutation_snapshot(&t).await, before);
    assert_eq!(counts(&t).await, (0, 1, 0));
    assert!(repo::delete_agent_runtime(t.gateway.db.pool(), &rid)
        .await
        .unwrap());
    assert!(!repo::delete_agent_runtime(t.gateway.db.pool(), &rid)
        .await
        .unwrap());
    assert_eq!(counts(&t).await, (0, 0, 0));
}
#[tokio::test]
async fn mutation_delete_first_allows_registration_to_recreate_complete_native_binding() {
    let t = setup().await;
    let v = directory(&t).await;
    let (request, _) = requested(&t, &v, "trae-cn-a").await;
    let (_, rid) = repo::trae_registration::native_ids(&request.binding());
    let mut orphan = crate::domain::AgentRuntime::new(&rid, "trae-model", "owned orphan");
    orphan.options = Some(serde_json::to_value(&request).unwrap());
    repo::upsert_agent_runtime(t.gateway.db.pool(), &orphan)
        .await
        .unwrap();
    assert!(repo::delete_agent_runtime(t.gateway.db.pool(), &rid)
        .await
        .unwrap());
    assert_eq!(counts(&t).await, (0, 0, 0));
    repo::trae_registration::register_native(t.gateway.db.pool(), &request, || Ok(()))
        .await
        .unwrap();
    assert_eq!(counts(&t).await, (1, 1, 1));
    let dangling:i64=sqlx::query_scalar("SELECT COUNT(*) FROM providers p LEFT JOIN agent_runtimes r ON r.id=p.runtime_id WHERE p.runtime_id IS NOT NULL AND r.id IS NULL").fetch_one(t.gateway.db.pool()).await.unwrap();
    assert_eq!(dangling, 0);
    assert!(repo::delete_agent_runtime(t.gateway.db.pool(), &rid)
        .await
        .is_err());
}
#[tokio::test]
async fn mutation_existing_native_rejects_generic_same_kind_but_keeps_safe_settings_channel() {
    let t = setup().await;
    let v = directory(&t).await;
    let r = register_one(&t, &v, 0, "trae-cn-a").await.unwrap();
    let mut native = repo::get_agent_runtime(t.gateway.db.pool(), &r.runtime_id)
        .await
        .unwrap()
        .unwrap();
    let before = mutation_snapshot(&t).await;
    native.label = "safe label".into();
    native.enabled = false;
    assert!(repo::upsert_agent_runtime(t.gateway.db.pool(), &native)
        .await
        .is_err());
    assert_eq!(mutation_snapshot(&t).await, before);
    repo::trae_registration::save_native_runtime_settings(t.gateway.db.pool(), &native)
        .await
        .unwrap();
    let saved = repo::get_agent_runtime(t.gateway.db.pool(), &r.runtime_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.label, "safe label");
    assert!(!saved.enabled);
    assert_eq!(saved.options, native.options);
    assert_eq!(counts(&t).await, (1, 1, 1));
}
#[tokio::test]
async fn mutation_referenced_ordinary_runtime_is_also_protected_from_delete() {
    let t = setup().await;
    let runtime = crate::domain::AgentRuntime::new("owned-ordinary", "codex", "owned");
    repo::upsert_agent_runtime(t.gateway.db.pool(), &runtime)
        .await
        .unwrap();
    sqlx::query("INSERT INTO providers(id,name,dialect,base_url,api_key_enc,enabled,priority,rpm_limit,intelligence,runtime_id,created_at,updated_at) VALUES('owned-p','Owned','openai','http://127.0.0.1','',1,0,0,0,'owned-ordinary',?,?)").bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(t.gateway.db.pool()).await.unwrap();
    let before = mutation_snapshot(&t).await;
    assert!(
        repo::delete_agent_runtime(t.gateway.db.pool(), "owned-ordinary")
            .await
            .is_err()
    );
    assert_eq!(mutation_snapshot(&t).await, before);
}
#[tokio::test]
async fn mutation_corrupt_native_options_still_cannot_be_replaced_by_generic_payload() {
    let t = setup().await;
    let v = directory(&t).await;
    let r = register_one(&t, &v, 0, "trae-cn-a").await.unwrap();
    sqlx::query("UPDATE agent_runtimes SET options_json=NULL WHERE id=?")
        .bind(&r.runtime_id)
        .execute(t.gateway.db.pool())
        .await
        .unwrap();
    let before = mutation_snapshot(&t).await;
    let payload = crate::domain::AgentRuntime::new(&r.runtime_id, "codex", "ordinary");
    assert!(repo::upsert_agent_runtime(t.gateway.db.pool(), &payload)
        .await
        .is_err());
    assert_eq!(mutation_snapshot(&t).await, before);
}

#[tokio::test]
async fn finalfix_provider_only_delete_recovers_in_two_explicit_subsets() {
    let t = setup().await;
    let v = directory(&t).await;
    let r = register(
        &t.gateway,
        &t.config,
        "native-fixture",
        v.profile_id.as_str(),
        &v.catalog_revision,
        vec![selection(&v, 0, "restore-a"), selection(&v, 1, "restore-b")],
    )
    .await
    .unwrap();
    sqlx::query("UPDATE agent_runtimes SET label='user label',enabled=0 WHERE id=?")
        .bind(&r.runtime_id)
        .execute(t.gateway.db.pool())
        .await
        .unwrap();
    repo::delete_provider(t.gateway.db.pool(), &r.provider_id)
        .await
        .unwrap();
    assert_eq!(counts(&t).await, (0, 1, 0));
    for (index, alias) in [(0, "restore-a"), (1, "restore-b")] {
        let v = directory(&t).await;
        let restored = register_one(&t, &v, index, alias)
            .await
            .expect("fresh explicit same-binding recovery");
        assert_eq!(restored.runtime_id, r.runtime_id);
        assert!(restored
            .warnings
            .contains(&"native_existing_runtime_disabled".into()));
        assert_eq!(counts(&t).await, (1, 1, (index + 1) as i64));
    }
    let runtime = repo::list_agent_runtimes(t.gateway.db.pool())
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(runtime.label, "user label");
    assert!(!runtime.enabled);
    assert_eq!(
        runtime.options.unwrap()["model_selections"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 3);
    assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn finalfix_provider_delete_whole_restore_and_collision_controls() {
    for mode in 0..4 {
        let t = setup().await;
        let v = directory(&t).await;
        let result = register(
            &t.gateway,
            &t.config,
            "native-fixture",
            v.profile_id.as_str(),
            &v.catalog_revision,
            vec![selection(&v, 0, "restore-a"), selection(&v, 1, "restore-b")],
        )
        .await
        .unwrap();
        let pool = t.gateway.db.pool();
        if mode == 3 {
            sqlx::query("UPDATE models SET upstream='corrupt' WHERE alias='restore-b'")
                .execute(pool)
                .await
                .unwrap();
        } else {
            repo::delete_provider(pool, &result.provider_id)
                .await
                .unwrap();
            if mode == 1 {
                sqlx::query("INSERT INTO providers(id,name,dialect,base_url,api_key_enc,enabled,priority,rpm_limit,intelligence,runtime_id,created_at,updated_at) VALUES('foreign','Foreign','openai','http://127.0.0.1','',1,0,0,0,?,?,?)")
                    .bind(&result.runtime_id).bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(pool).await.unwrap();
            }
            if mode == 2 {
                sqlx::query("INSERT INTO providers(id,name,dialect,base_url,api_key_enc,enabled,priority,rpm_limit,intelligence,created_at,updated_at) VALUES('foreign','Foreign','openai','http://127.0.0.1','',1,0,0,0,?,?)").bind(chrono::Utc::now()).bind(chrono::Utc::now()).execute(pool).await.unwrap();
                sqlx::query("INSERT INTO models(id,provider_id,alias,upstream) VALUES('foreign-model','foreign','restore-a','Other')").execute(pool).await.unwrap();
            }
        }
        let before = counts(&t).await;
        let v = directory(&t).await;
        let restored = register(
            &t.gateway,
            &t.config,
            "native-fixture",
            v.profile_id.as_str(),
            &v.catalog_revision,
            vec![selection(&v, 0, "restore-a"), selection(&v, 1, "restore-b")],
        )
        .await;
        if mode == 0 {
            assert_eq!(restored.unwrap().model_count, 2);
            assert_eq!(counts(&t).await, (1, 1, 2));
        } else {
            assert!(restored.is_err());
            assert_eq!(counts(&t).await, before);
        }
        assert_eq!(t.fixture.catalog.load(Ordering::SeqCst), 2);
        assert_eq!(t.fixture.model.load(Ordering::SeqCst), 0);
    }
}
