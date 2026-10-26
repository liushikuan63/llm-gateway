//! Actual production command macros/argument decoding + owned SQLite, with
//! Tauri MockRuntime only. No initialize_backend, OS window, proxy or live CLI.
use super::*;
use crate::{db::Db, proxy_control::gateway::GatewayService};
use serde_json::{json, Value};
use std::path::PathBuf;
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime};

#[path = "search_snapshot_ipc_tests.rs"]
mod search_snapshot_ipc_tests;

type SearchReadyEvents = Arc<(parking_lot::Mutex<Vec<(u32, Value)>>, parking_lot::Condvar)>;

#[path = "trae_web_flow_ipc_tests.rs"]
mod trae_web_flow;

#[path = "s6_declaration_ipc_tests.rs"]
mod s6_declaration_ipc_tests;

#[path = "provider_connection_ipc_tests.rs"]
mod provider_connection_ipc_tests;

#[test]
fn saved_trae_names_are_projected_by_production_list_providers_ipc_readonly() {
    use crate::proxy::trae::{catalog::web::selector, profile::*};
    let h = OwnedIpc::new();
    let mut expected = Vec::new();
    for (region, profile) in [
        ("cn", TraeProfileId::CnWebRawV1),
        ("global", TraeProfileId::GlobalWebRawSgV1),
    ] {
        let physical = RawModelIdentity {
            model_name: "owned-physical__dev".into(),
            config_source: 1,
        };
        let name = format!("Owned {region} display");
        let alias = format!("owned-{region}-raw");
        let options = json!({"region":region,"account_id":"owned-web","principal_hash":"a".repeat(64),"profile_id":profile.as_str(),"model_selections":{alias.clone():{
            "raw_model":physical,"source_function":"chat_v3","selector_id":selector(profile,SourceFunction::ChatV3,"Exact-config",&physical),
            "config_name":"Exact-config","display_name":name,"prompt_max_tokens":null,"max_tokens":128,
            "declared_vision":null,"declared_tools":null,"declared_thinking":null,"reasoning_effort_options":[],
            "protocol_carriers":{"text":true,"tools":true,"images":true,"audio":false,"video":false},"account_model_evidence":"unverified",
            "catalog_revision":"12345678-1234-4234-9234-123456789abc","fetched_at_ms":1,"allow_undeclared_images":false}}});
        let options = crate::agent_upstream::runtime::trae_options(Some(&options)).unwrap();
        // Artificial unverified metadata in the existing owned in-memory fixture.
        // No login, issuer, transport factory or real credential is supplied.
        let id = tauri::async_runtime::block_on(async {
            repo::trae_registration::register_native(h.db.pool(), &options, || Ok(()))
                .await
                .unwrap()
                .0
        });
        expected.push((id, alias, name, profile.origin()));
    }
    let before = tauri::async_runtime::block_on(repo::list_providers(h.db.pool())).unwrap();
    let views = h.invoke("list_providers", json!({})).unwrap();
    let views = views.as_array().unwrap();
    assert_eq!(views.len(), 2);
    for (id, alias, name, origin) in expected {
        let view = views.iter().find(|v| v["id"] == id).unwrap();
        assert_eq!(view["base_url"], origin);
        assert_eq!(view["model_display_names"][&alias], name);
        assert_eq!(view["models"][0]["alias"], alias);
        assert_eq!(view["models"][0]["upstream"], "Exact-config");
    }
    let after = tauri::async_runtime::block_on(repo::list_providers(h.db.pool())).unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    let requests: i64 = tauri::async_runtime::block_on(
        sqlx::query_scalar("SELECT COUNT(*) FROM requests").fetch_one(h.db.pool()),
    )
    .unwrap();
    assert_eq!(requests, 0);
}

#[test]
fn trae_web_raw_global_provider_edit_keeps_exact_profile_origin() {
    use crate::proxy::trae::{catalog::web::selector, profile::*};
    let h = OwnedIpc::new();
    let physical = RawModelIdentity {
        model_name: "Physical-B__dev".into(),
        config_source: 1,
    };
    let options = json!({"region":"global","account_id":"owned-web","principal_hash":"a".repeat(64),"profile_id":"global-web-raw-sg-v1","model_selections":{"owned-raw":{
        "raw_model":physical,"source_function":"chat_v3","selector_id":selector(TraeProfileId::GlobalWebRawSgV1,SourceFunction::ChatV3,"Exact-config",&physical),
        "config_name":"Exact-config","display_name":"Owned physical B","prompt_max_tokens":null,"max_tokens":128,
        "declared_vision":null,"declared_tools":null,"declared_thinking":null,"reasoning_effort_options":[],
        "protocol_carriers":{"text":true,"tools":true,"images":true,"audio":false,"video":false},"account_model_evidence":"unverified",
        "catalog_revision":"12345678-1234-4234-9234-123456789abc","fetched_at_ms":1,"allow_undeclared_images":false}}});
    let options = crate::agent_upstream::runtime::trae_options(Some(&options)).unwrap();
    // Editing contract only: owned DB metadata, no authentication/transport fixture.
    let id = tauri::async_runtime::block_on(async {
        repo::trae_registration::register_native(h.db.pool(), &options, || Ok(()))
            .await
            .unwrap()
            .0
    });
    let saved = tauri::async_runtime::block_on(repo::list_providers(h.db.pool()))
        .unwrap()
        .into_iter()
        .find(|p| p.id == id)
        .unwrap();
    let mut input = serde_json::to_value(saved).unwrap();
    input["api_key"] = json!("");
    input["name"] = json!("Renamed owned Global raw");
    let result = h.invoke("upsert_provider", json!({"input":input}));
    assert!(
        result.is_ok(),
        "Global profile must remain editable without changing its fixed origin: {result:?}"
    );
    input["base_url"] = json!(ORIGIN);
    assert!(h.invoke("upsert_provider", json!({"input":input})).is_err());
    let saved = tauri::async_runtime::block_on(repo::list_providers(h.db.pool()))
        .unwrap()
        .into_iter()
        .find(|p| p.id == id)
        .unwrap();
    assert_eq!(saved.base_url, TraeProfileId::GlobalWebRawSgV1.origin());
    assert_eq!(saved.name, "Renamed owned Global raw");
    let no_auth = h
        .invoke(
            "trae_web_model_catalog",
            json!({"accountId":"owned-web","region":"global"}),
        )
        .unwrap_err();
    assert!(no_auth
        .as_str()
        .is_some_and(|e| e == "native_web_login_required"));
}

#[test]
fn trae_web_start_production_ipc_is_reachable_without_fake_device() {
    let h = OwnedIpc::new();
    let start = h
        .invoke(
            "trae_web_login_start",
            json!({"region":"cn","accountId":"owned-web","consent":true,"requestId":"00000000-0000-4000-8000-000000000001"}),
        )
        .expect("explicit web login must not require a pre-existing native device");
    let url = reqwest::Url::parse(start["authorization_url"].as_str().unwrap()).unwrap();
    assert_eq!(url.origin().ascii_serialization(), "https://www.trae.cn");
    assert_eq!(url.path(), "/authorization");
    let fields: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(fields.get("client_id").unwrap(), "ono9krqynydwx5");
    assert_eq!(
        fields.get("login_trace_id").unwrap(),
        start["login_id"].as_str().unwrap()
    );
    for key in [
        "device_id",
        "machine_id",
        "device_model",
        "os_version",
        "code_challenge",
    ] {
        assert!(
            !fields.contains_key(key),
            "web route must not fabricate hardware or PKCE"
        );
    }
    let cancelled = h.invoke(
        "trae_web_login_cancel",
        json!({"region":"cn","accountId":"owned-web","loginId":start["login_id"]}),
    );
    assert!(cancelled.is_ok());
    let status = h
        .invoke(
            "trae_web_login_status",
            json!({"region":"cn","accountId":"owned-web"}),
        )
        .unwrap();
    assert_eq!(
        status["pending_login"]["request_id"],
        "00000000-0000-4000-8000-000000000001"
    );
    assert!(status["store_country"].is_null());
    let refused = h
        .invoke(
            "trae_web_login_refresh",
            json!({"region":"global","accountId":"owned-web","consent":false}),
        )
        .unwrap_err();
    assert!(
        refused
            .as_str()
            .is_some_and(|value| value.contains("明确授权")),
        "refresh must reach the registered production command and enforce consent"
    );
}

struct OwnedIpc {
    // Drop view before app; no event loop or real WebView2 is created.
    view: tauri::WebviewWindow<MockRuntime>,
    _app: tauri::App<MockRuntime>,
    db: Db,
    root: PathBuf,
    search_ready: SearchReadyEvents,
}
impl OwnedIpc {
    fn new() -> Self {
        Self::with_gateway(|_| {})
    }
    fn with_gateway(configure: impl FnOnce(&mut GatewayState)) -> Self {
        let root =
            std::env::temp_dir().join(format!("llmgw-owned-feedback-ipc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let db = tauri::async_runtime::block_on(Db::connect_in_memory()).unwrap();
        let mut gateway = GatewayState::new(db.clone(), AppConfig::default());
        configure(&mut gateway);
        let gateway = Arc::new(gateway);
        let state = AppState {
            desktop_chat_controls: Arc::new(crate::desktop_chat::RequestControls::default()),
            manual_search_controls: Arc::new(crate::desktop_chat::RequestControls::default()),
            client_usage_jobs: Err("unused in owned IPC test".into()),
            gateway_service: Arc::new(GatewayService::new(gateway.clone())),
            system_proxy: crate::proxy_control::system::SystemProxyManager::default(),
            proxy_commands: tokio::sync::Mutex::new(()),
            db: db.clone(),
            // Match production's two independent config locks, including writes.
            config: Arc::new(parking_lot::RwLock::new(gateway.cfg_snapshot())),
            gateway,
            adapters: Arc::new(crate::agent_upstream::AdapterRegistry::new()),
            vpn: Arc::new(crate::vpn::VpnManager::new(root.join("vpn"))),
            bridges: Arc::new(crate::client_bridges::BridgeManager::new(
                root.join("bridges"),
                root.join("resources"),
            )),
            xray: Arc::new(crate::vpn_xray::XrayManager::new(
                root.join("xray"),
                root.join("resources"),
            )),
            vpn_commands: tokio::sync::Mutex::new(()),
            client_commands: tokio::sync::Mutex::new(()),
            benefit_settings_commands: tokio::sync::Mutex::new(()),
        };
        // Exact command functions used by production registration, not wrappers
        // returning fixture DTOs. Manage owned state without boot/plugin setup.
        let search_ready = Arc::new((
            parking_lot::Mutex::new(Vec::new()),
            parking_lot::Condvar::new(),
        ));
        let ready_events = search_ready.clone();
        let app = mock_builder()
            .channel_interceptor(move |_, callback, _, body| {
                if let tauri::ipc::InvokeResponseBody::Json(value) = body {
                    let mut events = ready_events.0.lock();
                    assert!(events.len() < 128, "owned ready-event bound");
                    events.push((callback.0, serde_json::from_str(value).unwrap()));
                    ready_events.1.notify_all();
                }
                true
            })
            .manage(state)
            .invoke_handler(tauri::generate_handler![
                trae_web_login_start,
                trae_web_login_status,
                trae_web_login_poll,
                trae_web_login_cancel,
                trae_web_login_refresh,
                trae_web_model_catalog,
                register_trae_provider,
                test_provider,
                list_providers,
                list_models,
                upsert_provider,
                get_request_feedback,
                set_request_feedback,
                get_feedback_coverage,
                get_conversation_feedback,
                get_conversation_requests,
                query_requests,
                preview_routing_request,
                create_snapshot,
                apply_snapshot,
                get_search_settings,
                recover_stack_overflow_unsent,
                update_search_settings,
                update_config,
                test_search_backend,
                cancel_search_backend,
                get_remote_key_budget_snapshot,
                import_capabilities,
                export_capabilities,
                price_observations,
                list_sessions_page,
            ])
            .build(mock_context(noop_assets()))
            .unwrap();
        let view = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        Self {
            view,
            _app: app,
            db,
            root,
            search_ready,
        }
    }
    fn seed(&self, status: i64) -> i64 {
        tauri::async_runtime::block_on(async {
            sqlx::query("INSERT INTO requests(ts,requested_model,routed_provider,routed_model,status,trace_id,route_intent,route_classifier) VALUES (1,'owned','owned','physical',?,'same-trace','simple','rule') RETURNING id")
                .bind(status).fetch_one(self.db.pool()).await.unwrap().get("id")
        })
    }
    fn invoke(&self, command: &str, args: Value) -> Result<Value, Value> {
        get_ipc_response(
            &self.view,
            tauri::webview::InvokeRequest {
                cmd: command.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "http://tauri.localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(args),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
        .map(|b| b.deserialize::<Value>().unwrap())
    }
}
impl Drop for OwnedIpc {
    fn drop(&mut self) {
        tauri::async_runtime::block_on(self.db.pool().close());
        eprintln!("OWNED_FEEDBACK_IPC_ROOT={}", self.root.display());
    }
}

#[test]
fn budget_snapshot_production_ipc_is_readonly_exact_and_uses_runtime_config() {
    let h = OwnedIpc::new();
    tauri::async_runtime::block_on(async {
        sqlx::query("INSERT INTO remote_access_keys(id,label,key_hash,enabled,rpm_limit,monthly_budget_micros,budget_currency,allowed_models,created_at,updated_at) VALUES ('owned-budget','owned budget','owned-private-hash',1,60,100,'USD','','2026-10-20T00:00:00Z','2026-10-20T00:00:00Z')").execute(h.db.pool()).await.unwrap();
        let month = crate::budget::month_start_secs(chrono::Utc::now().timestamp());
        sqlx::query(
            "INSERT INTO budget_key_history(key_id,complete_from_month) VALUES('owned-budget',?)",
        )
        .bind(month)
        .execute(h.db.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO budget_holds(hold_id,gateway_request_uuid,call_ordinal,key_id,admission_month,currency,state,reserved_micros,reported_reference_micros,target_price_json,created_at,updated_at) VALUES ('owned-budget-hold','owned-budget-request',0,'owned-budget',?,'usd','dispatched',4,NULL,'owned-target-not-returned',0,0)").bind(month).execute(h.db.pool()).await.unwrap();
    });
    let before: i64 = tauri::async_runtime::block_on(
        sqlx::query_scalar("SELECT total_changes()").fetch_one(h.db.pool()),
    )
    .unwrap();
    // Runtime and UI-config locks intentionally differ: read the actual runtime.
    h._app.state::<AppState>().config.write().budget.enabled = false;
    let value = h
        .invoke(
            "get_remote_key_budget_snapshot",
            json!({"id":"owned-budget"}),
        )
        .unwrap();
    assert_eq!(value["budget_enabled"], true);
    assert_eq!(value["outstanding_micros"], "4");
    assert_eq!(value["known_spend_micros"], "0");
    assert_eq!(value["total_micros"], "4");
    assert_eq!(value["remaining_micros"], "96");
    assert_eq!(value["mode"], "reference_headroom");
    let wire = value.to_string();
    assert!(
        !wire.contains("private-hash")
            && !wire.contains("target-not-returned")
            && !wire.contains("key_hash")
    );
    h._app
        .state::<AppState>()
        .gateway
        .cfg
        .write()
        .budget
        .enabled = false;
    assert_eq!(
        h.invoke(
            "get_remote_key_budget_snapshot",
            json!({"id":"owned-budget"})
        )
        .unwrap()["mode"],
        "disabled"
    );
    let after: i64 = tauri::async_runtime::block_on(
        sqlx::query_scalar("SELECT total_changes()").fetch_one(h.db.pool()),
    )
    .unwrap();
    assert_eq!(after, before);
}

#[test]
fn budget_snapshot_ipc_rejects_missing_key_wrong_shape_and_storage_errors() {
    let h = OwnedIpc::new();
    assert!(h
        .invoke("get_remote_key_budget_snapshot", json!({}))
        .is_err());
    assert!(h
        .invoke("get_remote_key_budget_snapshot", json!({"id":4}))
        .is_err());
    assert!(h
        .invoke("get_remote_key_budget_snapshot", json!({"id":"missing"}))
        .unwrap_err()
        .to_string()
        .contains("不存在"));
    tauri::async_runtime::block_on(
        sqlx::query("DROP TABLE remote_access_keys").execute(h.db.pool()),
    )
    .unwrap();
    let error = h
        .invoke("get_remote_key_budget_snapshot", json!({"id":"missing"}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("暂时无法读取"));
    assert!(!error.contains("SELECT") && !error.contains("key_hash"));
}

#[test]
fn search_resource_commands_commit_and_preserve_legacy_partial_inputs() {
    // The feature runner always supplies a newly created owned data directory.
    // Fail before any save if someone invokes this against a formal profile.
    let supplied =
        PathBuf::from(std::env::var("LLMGW_DATA_DIR").expect("owned IPC runner data dir required"));
    assert!(supplied.is_absolute());
    let dir = supplied.canonicalize().unwrap();
    assert!(dir.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    assert!(dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("llmgw-personal-test-"));
    let path = crate::config::config_path();
    assert_eq!(
        path.parent().unwrap().canonicalize().unwrap(),
        dir,
        "validate the actual save target before invoking any write"
    );
    let f = OwnedIpc::new();
    let before = f.invoke("get_search_settings", json!({})).unwrap();
    assert_eq!(before["query_cache_ttl_secs"], 0);
    assert_eq!(before["coalesce_queries"], false);
    assert_eq!(before["source_min_interval_ms"], 0);
    assert_eq!(before["resources"]["schema_version"], 1);
    let mut input = json!({"enabled":true,"backend":"free_aggregate","searxng_url":null,"max_results":5,"timeout_ms":3000,"inject_as":"system","api_key":null,"clear_api_key":false,"query_cache_ttl_secs":10,"coalesce_queries":true,"source_min_interval_ms":60000});
    let saved = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert_eq!(saved["query_cache_ttl_secs"], 10);
    assert_eq!(saved["coalesce_queries"], true);
    let app = f._app.state::<AppState>();
    let cfg = app.gateway.cfg_snapshot();
    assert_eq!(cfg.search.query_cache_ttl_secs, 10);
    assert_eq!(cfg.search.source_min_interval_ms, 60000);
    assert_eq!(app.config.read().search, cfg.search);
    tauri::async_runtime::block_on(app.gateway.search_resources.run(
        [0; 32],
        &cfg.search,
        || async {
            crate::search::executor::SearchOutcome {
                backend: crate::config::SearchBackendKind::FreeAggregate,
                hits: 1,
                error: None,
                results: vec![crate::search::backend::SearchResult {
                    title: "owned seed".into(),
                    url: "https://example.test/owned".into(),
                    snippet: "owned".into(),
                    score: 0.0,
                }],
                result_sources: vec!["bing_rss".into()],
                failed_sources: vec![],
                reuse: None,
                retrieved_at_utc: None,
                page_text: None,
                requested_locale: None,
                unstructured_reference: None,
            }
        },
    ));
    assert_eq!(app.gateway.search_resources.stats().entries, 1);
    let store = app.gateway.search_resources.cooldowns().unwrap();
    let target = crate::search::cooldown::Target::new(
        crate::search::cooldown::Source::BingRss,
        "https://cn.bing.com",
    )
    .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::block_on(store.remember(target, now + 60000, now)).unwrap();
    let observed = f.invoke("get_search_settings", json!({})).unwrap();
    assert_eq!(observed["source_protection"]["schema_version"], 1);
    assert_eq!(
        observed["source_protection"]["sources"][0]["state"],
        "cooldown"
    );
    input
        .as_object_mut()
        .unwrap()
        .remove("query_cache_ttl_secs");
    input.as_object_mut().unwrap().remove("coalesce_queries");
    input
        .as_object_mut()
        .unwrap()
        .remove("source_min_interval_ms");
    input["max_results"] = json!(6);
    let legacy = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert_eq!(legacy["query_cache_ttl_secs"], 10);
    assert_eq!(
        legacy["source_protection"]["sources"][0]["state"], "cooldown",
        "settings writes must not clear known source protection"
    );
    assert_eq!(legacy["coalesce_queries"], true);
    assert_eq!(legacy["source_min_interval_ms"], 60000);
    assert_eq!(app.gateway.search_resources.stats().entries, 0);
    assert_eq!(path.parent().unwrap().canonicalize().unwrap(), dir);
    assert!(!std::fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    let bytes = std::fs::read(&path).unwrap();
    let persisted: AppConfig = toml::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(persisted.search.query_cache_ttl_secs, 10);
    assert!(persisted.search.coalesce_queries);
    assert_eq!(persisted.search.source_min_interval_ms, 60000);
    input["source_min_interval_ms"] = json!(3_600_001);
    assert!(f
        .invoke("update_search_settings", json!({"input":input}))
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(app.config.read().search.source_min_interval_ms, 60000);
    assert_eq!(
        app.gateway.cfg_snapshot().search.source_min_interval_ms,
        60000
    );
    input
        .as_object_mut()
        .unwrap()
        .remove("source_min_interval_ms");
    input["query_cache_ttl_secs"] = json!(3601);
    assert!(f
        .invoke("update_search_settings", json!({"input":input}))
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(app.config.read().search.query_cache_ttl_secs, 10);
    assert_eq!(app.gateway.cfg_snapshot().search.query_cache_ttl_secs, 10);
    input["query_cache_ttl_secs"] = json!(10);
    input["free_search_scope"] = json!("npm_packages");
    let scoped = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert_eq!(scoped["free_search_scope"], "npm_packages");
    assert_eq!(
        scoped["source_protection"]["sources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        scoped["source_protection"]["sources"][0]["source"],
        "npm_registry"
    );
    assert_eq!(
        app.config.read().search.free_search_scope,
        crate::config::FreeSearchScope::NpmPackages
    );
    assert_eq!(
        app.gateway.cfg_snapshot().search.free_search_scope,
        crate::config::FreeSearchScope::NpmPackages
    );
    let persisted: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        persisted.search.free_search_scope,
        crate::config::FreeSearchScope::NpmPackages
    );
    input.as_object_mut().unwrap().remove("free_search_scope");
    let legacy = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert_eq!(legacy["free_search_scope"], "npm_packages");
    let scoped_bytes = std::fs::read(&path).unwrap();
    input["free_search_scope"] = json!("all");
    assert!(f
        .invoke("update_search_settings", json!({"input":input}))
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), scoped_bytes);
    input.as_object_mut().unwrap().remove("free_search_scope");
    let other_config = |cfg: &AppConfig| {
        let mut value = serde_json::to_value(cfg).unwrap();
        value.as_object_mut().unwrap().remove("search");
        value
    };
    let other_before = other_config(&app.config.read());
    input["free_proxy_url"] = json!("http://127.0.0.1:39093");
    let proxy = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert_eq!(proxy["free_proxy_url"], "http://127.0.0.1:39093");
    assert_eq!(
        app.config.read().search.free_proxy_url.as_deref(),
        Some("http://127.0.0.1:39093")
    );
    assert_eq!(
        app.gateway.cfg_snapshot().search.free_proxy_url.as_deref(),
        Some("http://127.0.0.1:39093")
    );
    let persisted: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        persisted.search.free_proxy_url.as_deref(),
        Some("http://127.0.0.1:39093")
    );
    assert_eq!(other_config(&persisted), other_before);
    input.as_object_mut().unwrap().remove("free_proxy_url");
    let unchanged = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert_eq!(
        unchanged["free_proxy_url"], "http://127.0.0.1:39093",
        "omitted is not explicit clear"
    );
    let proxy_bytes = std::fs::read(&path).unwrap();
    input["free_proxy_url"] = json!("http://owned:never@127.0.0.1:39093");
    assert!(f
        .invoke("update_search_settings", json!({"input":input}))
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), proxy_bytes);
    assert_eq!(
        app.gateway.cfg_snapshot().search.free_proxy_url.as_deref(),
        Some("http://127.0.0.1:39093")
    );
    input["free_proxy_url"] = serde_json::Value::Null;
    let cleared = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert!(cleared["free_proxy_url"].is_null());
    assert!(app.config.read().search.free_proxy_url.is_none());
    assert!(app.gateway.cfg_snapshot().search.free_proxy_url.is_none());
    let persisted: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(persisted.search.free_proxy_url.is_none());
    assert_eq!(other_config(&persisted), other_before);
    input["free_search_scope"] = json!("hacker_news");
    let hn = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert_eq!(hn["free_search_scope"], "hacker_news");
    assert_eq!(
        hn["free_search_scopes"],
        json!(["web", "npm_packages", "hacker_news", "stack_overflow"])
    );
    assert_eq!(
        hn["source_protection"]["sources"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        hn["source_protection"]["sources"][0]["source"],
        "hacker_news"
    );
    assert_eq!(
        app.config.read().search.free_search_scope,
        crate::config::FreeSearchScope::HackerNews
    );
    assert_eq!(
        app.gateway.cfg_snapshot().search.free_search_scope,
        crate::config::FreeSearchScope::HackerNews
    );
    let persisted: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        persisted.search.free_search_scope,
        crate::config::FreeSearchScope::HackerNews
    );
    assert_eq!(other_config(&persisted), other_before);
    input.as_object_mut().unwrap().remove("free_search_scope");
    input["free_search_scopes"] = json!(["unsupported"]);
    let legacy = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert_eq!(legacy["free_search_scope"], "hacker_news");
    assert_eq!(
        legacy["free_search_scopes"],
        json!(["web", "npm_packages", "hacker_news", "stack_overflow"]),
        "supported capabilities come from the database-backed runtime, not client fields"
    );
}

#[test]
fn request_decision_query_command_returns_safe_per_row_projection() {
    let f = OwnedIpc::new();
    let old = f.seed(200);
    let bad = f.seed(499);
    let preparing = f.seed(499);
    tauri::async_runtime::block_on(async {
        sqlx::query("UPDATE requests SET route_decision_json='invalid private content' WHERE id=?")
            .bind(bad)
            .execute(f.db.pool())
            .await
            .unwrap();
        let snapshot = crate::audit::route_decision::Snapshot::not_reached(
            crate::config::RoutingStrategy::Priority,
            false,
        )
        .to_storage()
        .unwrap();
        sqlx::query("UPDATE requests SET route_decision_json=? WHERE id=?")
            .bind(snapshot)
            .bind(preparing)
            .execute(f.db.pool())
            .await
            .unwrap();
    });
    let page = f
        .invoke(
            "query_requests",
            json!({"filter":{"trace_id":"same-trace"}}),
        )
        .unwrap();
    assert_eq!(page["total"], 3);
    let rows = page["rows"].as_array().unwrap();
    let find = |id| rows.iter().find(|row| row["id"] == id).unwrap();
    assert_eq!(find(old)["route_decision"]["state"], "unobserved");
    assert_eq!(find(bad)["route_decision"]["state"], "invalid");
    assert_eq!(
        find(preparing)["route_decision"]["snapshot"]["kind"],
        "not_reached"
    );
    assert!(!page.to_string().contains("invalid private content"));
    assert_eq!(
        f.invoke("get_request_feedback", json!({"requestId":old}))
            .unwrap()["revision"],
        0
    );
}

#[test]
fn production_feedback_commands_decode_frontend_shapes_and_commit_only_owned_state() {
    let f = OwnedIpc::new();
    let a = f.seed(200);
    let b = f.seed(499);
    let first = f
        .invoke("get_request_feedback", json!({"requestId":a}))
        .unwrap();
    assert_eq!(first["revision"], 0);
    assert!(first["outcome"].is_null());
    let saved=f.invoke("set_request_feedback",json!({"input":{"request_id":a,"expected_revision":0,"confirmed_class":"vision","outcome":"did_not_meet_need"}})).unwrap();
    assert_eq!(saved["request_id"], a);
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["status"], 200);
    assert_eq!(saved["source"], "local_user");
    assert_eq!(
        f.invoke("get_request_feedback", json!({"requestId":b}))
            .unwrap()["revision"],
        0
    );
    assert_eq!(f.invoke("set_request_feedback",json!({"input":{"request_id":a,"expected_revision":0,"confirmed_class":null,"outcome":null}})).unwrap_err(),json!("feedback_conflict"));
    let coverage = f
        .invoke(
            "get_feedback_coverage",
            json!({"filter":{"limit":1,"offset":999}}),
        )
        .unwrap();
    assert_eq!(coverage["requests"], 2);
    assert_eq!(coverage["annotated"], 1);
    assert_eq!(coverage["did_not_meet_need"], 1);
    assert_eq!(coverage["missing_outcome"], 1);
    let clear=f.invoke("set_request_feedback",json!({"input":{"request_id":a,"expected_revision":1,"confirmed_class":null,"outcome":null}})).unwrap();
    assert_eq!(clear["revision"], 2);
    let n: i64 = tauri::async_runtime::block_on(
        sqlx::query_scalar("SELECT COUNT(*) FROM request_feedback").fetch_one(f.db.pool()),
    )
    .unwrap();
    assert_eq!(n, 1);
    assert!(f
        ._app
        .state::<AppState>()
        .gateway
        .listener_snapshot()
        .is_none());
}

#[test]
fn session_preview_command_decodes_opt_in_and_uses_owned_readonly_history() {
    let f = OwnedIpc::new();
    tauri::async_runtime::block_on(async {
        repo::get_or_create_session(f.db.pool(), "owned-preview-session")
            .await
            .unwrap();
        sqlx::query("UPDATE sessions SET summary='owned private IPC summary' WHERE id='owned-preview-session'").execute(f.db.pool()).await.unwrap();
    });
    // Free mode with no candidates is an explicit closed guard, independent of
    // whether the opt-in snapshot is found. No model/issuer is installed here.
    let value=f.invoke("preview_routing_request",json!({"input":{"request":{"model":"free","messages":[{"role":"user","content":"owned offline"}]},"include_session":true,"session_id":"owned-preview-session"}})).unwrap();
    assert_eq!(value["mode"], "offline_session_context");
    assert_eq!(value["context"]["status"], "found");
    assert_eq!(value["context"]["history_messages"], 0);
    assert_eq!(value["context"]["summary_present"], true);
    assert_eq!(
        value["order_semantics"],
        "free_weight_only_no_turn_prediction"
    );
    assert!(!value.to_string().contains("owned private IPC summary"));
    assert!(f.invoke("preview_routing_request",json!({"input":{"request":{"model":"free","messages":[{"role":"user","content":"owned offline"}]},"include_session":"yes"}})).is_err());
    for (table, expected) in [
        ("sessions", 1),
        ("requests", 0),
        ("session_messages", 0),
        ("usage_daily", 0),
    ] {
        let n: i64 = tauri::async_runtime::block_on(
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}")).fetch_one(f.db.pool()),
        )
        .unwrap();
        assert_eq!(n, expected);
    }
    assert!(f
        ._app
        .state::<AppState>()
        .gateway
        .listener_snapshot()
        .is_none());
}

#[test]
fn production_ipc_rejects_wrong_shapes_forged_provenance_and_preserves_safe_storage_errors() {
    let f = OwnedIpc::new();
    let id = f.seed(200);
    let bad = f
        .invoke("get_request_feedback", json!({"request_id":id}))
        .unwrap_err();
    assert!(bad.as_str().unwrap().contains("requestId"));
    let forged=f.invoke("set_request_feedback",json!({"input":{"request_id":id,"expected_revision":0,"confirmed_class":null,"outcome":null,"source":"trusted_issuer"}})).unwrap_err();
    assert!(forged.as_str().unwrap().contains("source"));
    assert_eq!(
        f.invoke(
            "get_request_feedback",
            json!({"requestId":9_007_199_254_740_992_i64})
        )
        .unwrap_err(),
        json!("feedback_invalid_input")
    );
    assert_eq!(
        f.invoke("get_request_feedback", json!({"requestId":id+100}))
            .unwrap_err(),
        json!("feedback_request_not_found")
    );
    tauri::async_runtime::block_on(sqlx::raw_sql("CREATE TRIGGER owned_ipc_failure BEFORE INSERT ON request_feedback BEGIN SELECT RAISE(ABORT,'owned internal message'); END;").execute(f.db.pool())).unwrap();
    assert_eq!(f.invoke("set_request_feedback",json!({"input":{"request_id":id,"expected_revision":0,"confirmed_class":null,"outcome":"met_need"}})).unwrap_err(),json!("feedback_storage_failed"));
    assert_eq!(
        f.invoke("get_request_feedback", json!({"requestId":id}))
            .unwrap()["revision"],
        0
    );
}

#[test]
fn request_shape_preview_command_uses_the_real_decoder_and_has_no_dispatch_or_audit() {
    let f = OwnedIpc::new();
    let provider:Provider=serde_json::from_value(json!({"id":"owned-preview-ipc","name":"owned preview","base_url":"http://127.0.0.1:1/v1","api_key_enc":"","dialect":"openai","enabled":true,"priority":0,"rpm_limit":1,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"owned","upstream":"physical","enabled":true,"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","price":null,"overrides":null}]})).unwrap();
    tauri::async_runtime::block_on(repo::upsert_provider(f.db.pool(), &provider)).unwrap();
    tauri::async_runtime::block_on(f._app.state::<AppState>().gateway.reload_providers()).unwrap();
    // Typical nonstream request omits stream; the preview-only decoder supplies false.
    let plain=f.invoke("preview_routing_request",json!({"input":{"request":{"model":"auto","messages":[{"role":"user","content":"owned offline IPC"}],"max_tokens":16}}})).unwrap();
    assert_eq!(plain["mode"], "offline_supplied_messages");
    assert_eq!(plain["rows"][0]["eligible"], true);
    assert_eq!(plain["rows"][0]["ordinal"], 1);
    let image=f.invoke("preview_routing_request",json!({"input":{"request":{"model":"auto","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}],"max_tokens":16},"access_key_id":null,"strategy":"priority"}})).unwrap();
    assert_eq!(image["rows"][0]["eligible"], false);
    assert_eq!(
        image["rows"][0]["exclusion_reason"],
        "model_capability_missing"
    );
    assert!(image["rows"][0]["score"].is_null());
    for table in ["requests", "sessions", "session_messages", "usage_daily"] {
        let n: i64 = tauri::async_runtime::block_on(
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}")).fetch_one(f.db.pool()),
        )
        .unwrap();
        assert_eq!(n, 0);
    }
    assert!(f
        ._app
        .state::<AppState>()
        .gateway
        .listener_snapshot()
        .is_none());
}

fn seed_capability_transfer(h: &OwnedIpc) {
    tauri::async_runtime::block_on(async {
        let models=[("a",false),("b",true)].into_iter().map(|(alias,enabled)|json!({"alias":alias,"upstream":format!("physical-{alias}"),"enabled":enabled,"model_type":"chat","context_window":32768,"supports_tools":true,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"price":{"prompt":2.0,"completion":3.0,"currency":"usd","source":"manual","tiers":[],"rules":[]}})).collect::<Vec<_>>();
        let p:crate::domain::Provider=serde_json::from_value(json!({"id":"owned-cap","name":"Owned Cap","base_url":"https://owned.invalid","api_key_enc":"","dialect":"openai","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"models":models,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z"})).unwrap();
        repo::upsert_provider(h.db.pool(), &p).await.unwrap();
        let mut ledger = crate::capability::CapabilitySet::new();
        ledger.insert(
            crate::capability::Dimension::Coding,
            crate::domain::CapabilitySource::Manual,
            0.9,
        );
        repo::write_capability_ledger(h.db.pool(), "owned-cap", "a", &ledger)
            .await
            .unwrap();
        h._app
            .state::<AppState>()
            .gateway
            .reload_providers()
            .await
            .unwrap();
    });
}

#[test]
fn capability_transfer_parse_failure_does_not_partially_replace_earlier_model() {
    let h = OwnedIpc::new();
    seed_capability_transfer(&h);
    let before = h.invoke("export_capabilities", json!({})).unwrap();
    let result = h.invoke(
        "import_capabilities",
        json!({"payload":r#"{"owned-cap/a":{"coding":{"manual":0.3}},"owned-cap/b":[]}"#}),
    );
    assert!(
        result.is_err(),
        "malformed known-model ledger must report failure"
    );
    let after = h.invoke("export_capabilities", json!({})).unwrap();
    assert_eq!(
        after, before,
        "production import failure cannot leave the earlier known model committed"
    );
}

#[test]
fn capability_transfer_storage_failure_rolls_back_all_model_ledgers() {
    let h = OwnedIpc::new();
    seed_capability_transfer(&h);
    let before = h.invoke("export_capabilities", json!({})).unwrap();
    tauri::async_runtime::block_on(sqlx::query("CREATE TRIGGER owned_capability_import_fail BEFORE UPDATE OF capability_sources_json ON models WHEN NEW.alias='b' BEGIN SELECT RAISE(ABORT,'owned import write fault'); END").execute(h.db.pool())).unwrap();
    assert!(h.invoke("import_capabilities",json!({"payload":r#"{"owned-cap/a":{"coding":{"manual":0.3}},"owned-cap/b":{"reasoning":{"catalog":0.6}}}"#})).is_err());
    assert_eq!(
        h.invoke("export_capabilities", json!({})).unwrap(),
        before,
        "owned write fault must rollback the earlier model in same import"
    );
}

#[test]
fn capability_transfer_success_commits_skips_zero_and_runtime_view_without_other_config_changes() {
    let h = OwnedIpc::new();
    seed_capability_transfer(&h);
    let before = tauri::async_runtime::block_on(repo::list_providers(h.db.pool())).unwrap();
    let result=h.invoke("import_capabilities",json!({"payload":r#"{"owned-cap/a":{"values":{"coding":{"manual":0.0,"catalog":0.5,"future_source":0.9},"future_dimension":{"manual":0.2}}},"owned-cap/b":{"reasoning":{"measured":0.8}},"absent/model":{}}"#})).unwrap();
    assert_eq!(result["written"], 2);
    assert_eq!(result["skipped"], json!(["absent/model"]));
    assert_eq!(result["skipped_dimensions"], json!(["future_dimension"]));
    assert_eq!(result["skipped_sources"], json!(["future_source"]));
    let exported = h.invoke("export_capabilities", json!({})).unwrap();
    let data: Value = serde_json::from_str(exported.as_str().unwrap()).unwrap();
    assert_eq!(
        data["owned-cap/a"]["values"]["coding"]["manual"]["value"],
        0.0
    );
    assert_eq!(
        data["owned-cap/a"]["values"]["coding"]["catalog"]["value"],
        0.5
    );
    let after = tauri::async_runtime::block_on(repo::list_providers(h.db.pool())).unwrap();
    for alias in ["a", "b"] {
        let a = before[0].models.iter().find(|m| m.alias == alias).unwrap();
        let b = after[0].models.iter().find(|m| m.alias == alias).unwrap();
        assert_eq!(a.enabled, b.enabled);
        assert_eq!(
            serde_json::to_value(&a.price).unwrap(),
            serde_json::to_value(&b.price).unwrap()
        );
        assert_eq!(a.upstream, b.upstream);
    }
    let state = h._app.state::<AppState>();
    let runtime = state.gateway.providers.read();
    let a = after[0].models.iter().find(|m| m.alias == "a").unwrap();
    assert_eq!(a.capabilities.as_ref().unwrap().coding, Some(0.0));
    assert!(!a.enabled);
    assert!(
        runtime[0].models.iter().all(|m| m.alias != "a"),
        "disabled models stay out of the actual routable view"
    );
    let b = runtime[0].models.iter().find(|m| m.alias == "b").unwrap();
    assert_eq!(b.capabilities.as_ref().unwrap().reasoning, Some(0.8));
}

#[test]
fn capability_transfer_export_is_one_snapshot_during_atomic_import() {
    let h = OwnedIpc::new();
    seed_capability_transfer(&h);
    let before = h.invoke("export_capabilities", json!({})).unwrap();
    tauri::async_runtime::block_on(async {
        let arrived = Arc::new(tokio::sync::Notify::new());
        let proceed = Arc::new(tokio::sync::Notify::new());
        let db = h.db.clone();
        let signal = arrived.clone();
        let release = proceed.clone();
        let writer = tokio::spawn(async move {
            signal.notified().await;
            let a = crate::capability::CapabilitySet::import_json(r#"{"coding":{"manual":{"value":0.3,"observed_at":"2020-01-01T00:00:00Z","sample_count":0}}}"#)
                .unwrap()
                .0;
            let b = crate::capability::CapabilitySet::import_json(
                r#"{"reasoning":{"catalog":{"value":0.6,"sample_count":12}}}"#,
            )
            .unwrap()
            .0;
            assert_eq!(
                repo::write_capability_ledgers_atomic(
                    db.pool(),
                    &[
                        ("owned-cap".into(), "a".into(), a),
                        ("owned-cap".into(), "b".into(), b)
                    ]
                )
                .await
                .unwrap(),
                vec![true, true]
            );
            release.notify_one();
        });
        // The same production command future as IPC, with an owned scheduling
        // seam only under cfg(test). No replacement exporter or test issuer.
        let pending = CAPABILITY_EXPORT_READ_BARRIER.scope(
            (arrived, proceed),
            export_capabilities(h._app.state::<AppState>()),
        );
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), pending).await;
        let exported = match result {
            Ok(Ok(value)) => value,
            other => {
                writer.abort();
                panic!("owned concurrent export did not complete: {other:?}");
            }
        };
        writer.await.unwrap();
        let after = export_capabilities(h._app.state::<AppState>())
            .await
            .unwrap();
        assert_ne!(
            before,
            json!(after),
            "normal atomic import really changed both models"
        );
        assert!(json!(exported) == before || exported == after, "one exported file must be either whole old or whole new ledger, never an impossible mixed snapshot");
    });
}

#[test]
fn capability_transfer_s1_metadata_commit_invalid_and_write_fault_atomicity() {
    let h = OwnedIpc::new();
    seed_capability_transfer(&h);
    let before = tauri::async_runtime::block_on(repo::list_providers(h.db.pool())).unwrap();
    let payload = r#"{"owned-cap/a":{"coding":{"manual":{"value":0.75,"observed_at":"2020-01-01T08:00:00+08:00","sample_count":0}}},"owned-cap/b":{"math":{"measured":{"value":0.8,"sample_count":12}}}}"#;
    let result = h
        .invoke("import_capabilities", json!({"payload":payload}))
        .unwrap();
    assert_eq!(result["written"], 2);
    let snapshot = h.invoke("export_capabilities", json!({})).unwrap();
    let data: Value = serde_json::from_str(snapshot.as_str().unwrap()).unwrap();
    assert_eq!(
        data["owned-cap/a"]["values"]["coding"]["manual"]["observed_at"],
        "2020-01-01T00:00:00Z"
    );
    assert_eq!(
        data["owned-cap/a"]["values"]["coding"]["manual"]["sample_count"],
        0
    );
    assert_eq!(
        data["owned-cap/b"]["values"]["math"]["measured"]["sample_count"],
        12
    );
    let after = tauri::async_runtime::block_on(repo::list_providers(h.db.pool())).unwrap();
    for (a, b) in before[0].models.iter().zip(after[0].models.iter()) {
        assert_eq!(a.enabled, b.enabled);
        assert_eq!(a.price, b.price);
        assert_eq!(a.upstream, b.upstream);
    }
    assert!(h.invoke("import_capabilities",json!({"payload":r#"{"owned-cap/a":{"coding":{"manual":{"value":0.2,"sample_count":1}}},"owned-cap/b":{"math":{"measured":{"value":0.9,"sample_count":1.5}}}}"#})).is_err());
    assert_eq!(
        h.invoke("export_capabilities", json!({})).unwrap(),
        snapshot
    );
    assert!(h.invoke("import_capabilities", json!({"payload":r#"{"owned-cap/a":{"coding":{"manual":{"value":0.2,"sample_count":1}}},"owned-cap/b":{"math":{"measured":{"sample_count":-1}}}}"#})).is_err());
    assert_eq!(
        h.invoke("export_capabilities", json!({})).unwrap(),
        snapshot
    );
    tauri::async_runtime::block_on(sqlx::query("CREATE TRIGGER owned_s1_observation_fault BEFORE UPDATE OF capability_sources_json ON models WHEN NEW.alias='b' BEGIN SELECT RAISE(ABORT,'owned S1 write fault'); END").execute(h.db.pool())).unwrap();
    assert!(h.invoke("import_capabilities",json!({"payload":r#"{"owned-cap/a":{"coding":{"manual":{"value":0.2,"sample_count":1}}},"owned-cap/b":{"math":{"measured":{"value":0.9,"sample_count":2}}}}"#})).is_err());
    assert_eq!(
        h.invoke("export_capabilities", json!({})).unwrap(),
        snapshot
    );
    assert!(h
        ._app
        .state::<AppState>()
        .gateway
        .listener_snapshot()
        .is_none());
}

#[test]
fn price_record_ipc_reads_owned_records_without_secret_or_mutation() {
    let h = OwnedIpc::new();
    seed_capability_transfer(&h);
    let key = repo::price_observation::key("owned-cap", "a");
    let before = tauri::async_runtime::block_on(repo::meta_get(h.db.pool(), &key)).unwrap();
    let result = h.invoke("price_observations", json!({})).unwrap();
    assert_eq!(result.as_array().unwrap().len(), 2);
    assert_eq!(result[0]["provider_id"], "owned-cap");
    assert_eq!(result[0]["origin"], "manual_save");
    assert!(result[0].get("binding").is_none());
    assert!(result[0].get("provider_target").is_none());
    assert!(result[0].get("api_key_enc").is_none());
    assert_eq!(
        tauri::async_runtime::block_on(repo::meta_get(h.db.pool(), &key)).unwrap(),
        before
    );
    assert!(h
        ._app
        .state::<AppState>()
        .gateway
        .listener_snapshot()
        .is_none());
}

#[test]
fn price_record_snapshot_restore_clears_only_owned_namespace_in_same_transaction() {
    let h = OwnedIpc::new();
    seed_capability_transfer(&h);
    tauri::async_runtime::block_on(async {
        repo::meta_set(h.db.pool(), "priceXobservationYv1:unrelated", "keep")
            .await
            .unwrap();
        let providers = repo::list_providers(h.db.pool()).await.unwrap();
        replace_snapshot_providers(h.db.pool(), &providers)
            .await
            .unwrap();
        assert!(repo::price_observation::snapshot(h.db.pool())
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            repo::meta_get(h.db.pool(), "priceXobservationYv1:unrelated")
                .await
                .unwrap()
                .as_deref(),
            Some("keep")
        );
        assert_eq!(
            repo::list_providers(h.db.pool()).await.unwrap()[0]
                .models
                .len(),
            2
        );
    });
}

#[test]
fn ux7_session_page_command_decodes_nested_input_and_preserves_legacy_and_readonly_state() {
    let h = OwnedIpc::new();
    tauri::async_runtime::block_on(async {
        for i in 0..130 {
            sqlx::query("INSERT INTO sessions(id,title,created_at,updated_at) VALUES(?,?,?,?)")
                .bind(format!("owned-{i:03}"))
                .bind(if i == 0 {
                    "历史分页目标".into()
                } else {
                    format!("Owned {i}")
                })
                .bind("2026-10-20T00:00:00Z")
                .bind("2026-10-20T00:00:00Z")
                .execute(h.db.pool())
                .await
                .unwrap();
        }
    });
    let legacy =
        tauri::async_runtime::block_on(list_sessions(h._app.state::<AppState>(), Some(100)))
            .unwrap();
    assert_eq!(legacy.len(), 100);
    let result=h.invoke("list_sessions_page",json!({"input":{"query":"历史分页目标","limit":50,"cursor":null,"selected_id":"owned-129"}})).unwrap();
    assert_eq!(result["total_count"], 1);
    assert_eq!(result["rows"][0]["id"], "owned-000");
    assert_eq!(result["selected"]["id"], "owned-129");
    assert_eq!(result["has_more"], false);
    assert!(result["next_cursor"].is_null());
    let first = h
        .invoke(
            "list_sessions_page",
            json!({"input":{"query":"","limit":50}}),
        )
        .unwrap();
    assert_eq!(first["rows"].as_array().unwrap().len(), 50);
    let next = h
        .invoke(
            "list_sessions_page",
            json!({"input":{"query":"","cursor":first["next_cursor"],"limit":50}}),
        )
        .unwrap();
    assert_ne!(first["rows"][0]["id"], next["rows"][0]["id"]);
    assert!(h
        .invoke(
            "list_sessions_page",
            json!({"input":{"query":"changed","cursor":first["next_cursor"]}})
        )
        .is_err());
    let count: i64 = tauri::async_runtime::block_on(
        sqlx::query_scalar("SELECT COUNT(*) FROM sessions").fetch_one(h.db.pool()),
    )
    .unwrap();
    assert_eq!(count, 130);
    assert!(h
        ._app
        .state::<AppState>()
        .gateway
        .listener_snapshot()
        .is_none());
}

#[test]
fn page_text_production_ipc_opt_in_legacy_and_full_snapshot_save() {
    let supplied = PathBuf::from(std::env::var("LLMGW_DATA_DIR").expect("owned IPC data dir"));
    let dir = supplied.canonicalize().unwrap();
    assert!(dir.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    assert!(dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("llmgw-personal-test-"));
    let path = crate::config::config_path();
    assert_eq!(path.parent().unwrap().canonicalize().unwrap(), dir);
    let f = OwnedIpc::new();
    let before = f.invoke("get_search_settings", json!({})).unwrap();
    assert_eq!(before["page_text_enabled"], false);
    let mut input = json!({"enabled":true,"backend":"free_aggregate","searxng_url":null,"max_results":5,"timeout_ms":3000,"inject_as":"system","page_text_enabled":true});
    let saved = f
        .invoke("update_search_settings", json!({"input":input}))
        .unwrap();
    assert_eq!(saved["page_text_enabled"], true);
    let app = f._app.state::<AppState>();
    assert!(app.config.read().search.page_text_enabled);
    assert!(app.gateway.cfg_snapshot().search.page_text_enabled);
    let persisted: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(persisted.search.page_text_enabled);
    input.as_object_mut().unwrap().remove("page_text_enabled");
    input["max_results"] = json!(4);
    assert_eq!(
        f.invoke("update_search_settings", json!({"input":input}))
            .unwrap()["page_text_enabled"],
        true
    );
    // Old complete snapshots also preserve this narrow opt-in owner.
    let mut stale = app.config.read().clone();
    stale.search.page_text_enabled = false;
    let saved = commit_config_with(
        &app.config,
        ConfigCommit::Save(&stale),
        |_| Ok(()),
        |cfg| app.gateway.update_cfg(cfg.clone()),
    )
    .unwrap();
    assert!(saved.search.page_text_enabled);
    input["page_text_enabled"] = json!(false);
    assert_eq!(
        f.invoke("update_search_settings", json!({"input":input}))
            .unwrap()["page_text_enabled"],
        false
    );
    assert!(!app.gateway.cfg_snapshot().search.page_text_enabled);
}

#[test]
fn page_text_generic_config_production_ipc_keeps_narrow_owner() {
    let supplied = PathBuf::from(std::env::var("LLMGW_DATA_DIR").expect("owned IPC data dir"));
    let dir = supplied.canonicalize().unwrap();
    assert!(dir.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    assert!(dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("llmgw-personal-test-"));
    let path = crate::config::config_path();
    assert_eq!(path.parent().unwrap().canonicalize().unwrap(), dir);
    let f = OwnedIpc::new();
    let app = f._app.state::<AppState>();
    let benefits = app.config.read().benefits.clone();
    assert!(benefits.accounts.is_empty());
    assert!(!benefits.enabled && !benefits.auto_claim);
    assert_eq!(benefits.scheduler_mode, "app");
    assert!(app.gateway.listener_snapshot().is_none());
    let mut narrow = json!({"enabled":true,"backend":"free_aggregate","searxng_url":null,"max_results":5,"timeout_ms":3000,"inject_as":"system","page_text_enabled":true});
    f.invoke("update_search_settings", json!({"input":narrow}))
        .unwrap();
    for (n, field) in [Some(false), None].into_iter().enumerate() {
        let mut stale = serde_json::to_value(app.config.read().clone()).unwrap();
        stale["analytics_retention_days"] = json!(31 + n);
        if let Some(value) = field {
            stale["search"]["page_text_enabled"] = json!(value);
        } else {
            stale["search"]
                .as_object_mut()
                .unwrap()
                .remove("page_text_enabled");
        }
        let parsed: AppConfig = serde_json::from_value(stale.clone()).unwrap();
        assert!(!crate::benefit_task::task_change(
            &benefits,
            &parsed.benefits
        ));
        assert_eq!(parsed.benefits, benefits);
        // Real generic command argument decoding, normalization, coordinated
        // save and both publication locks; no direct commit helper shortcut.
        let result = f.invoke("update_config", json!({"cfg":stale})).unwrap();
        assert_eq!(result["restart_required"], false);
        assert_eq!(result["config"]["analytics_retention_days"], json!(31 + n));
        assert_eq!(result["config"]["search"]["page_text_enabled"], true);
        assert!(app.config.read().search.page_text_enabled);
        assert!(app.gateway.cfg_snapshot().search.page_text_enabled);
        let persisted: AppConfig =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(persisted.search.page_text_enabled);
        assert_eq!(persisted.benefits, benefits);
        assert!(app.gateway.listener_snapshot().is_none());
    }
    let mut stale_enabled = serde_json::to_value(app.config.read().clone()).unwrap();
    assert_eq!(stale_enabled["search"]["page_text_enabled"], true);
    narrow["page_text_enabled"] = json!(false);
    f.invoke("update_search_settings", json!({"input":narrow}))
        .unwrap();
    stale_enabled["analytics_retention_days"] = json!(33);
    let result = f
        .invoke("update_config", json!({"cfg":stale_enabled}))
        .unwrap();
    assert_eq!(result["config"]["search"]["page_text_enabled"], false);
    assert!(!app.config.read().search.page_text_enabled);
    assert!(!app.gateway.cfg_snapshot().search.page_text_enabled);
    let persisted: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(!persisted.search.page_text_enabled);
    assert_eq!(persisted.benefits, benefits);
    assert!(app.gateway.listener_snapshot().is_none());
}

#[test]
fn search_completion_facts_production_ipc_uses_owned_legacy_source() {
    use axum::{extract::Query, response::IntoResponse, routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = PathBuf::from(std::env::var("LLMGW_DATA_DIR").expect("owned IPC data dir"))
        .canonicalize()
        .unwrap();
    assert!(dir.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    assert!(dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("llmgw-personal-test-"));
    assert_eq!(
        crate::config::config_path()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap(),
        dir
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let (origin, task) = tauri::async_runtime::block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app=Router::new().route("/search",get(move |Query(query):Query<std::collections::HashMap<String,String>>|{let calls=counted.clone();async move {
            calls.fetch_add(1,Ordering::SeqCst);
            let text=query.get("q").unwrap();
            if text=="owned rejected" {return axum::http::StatusCode::UNAUTHORIZED.into_response();}
            assert!(text=="owned completed"||text=="owned empty");
            axum::Json(json!({"results":if text=="owned completed"{vec![json!({"title":"owned source","url":"https://owned-index.invalid/meta","content":"metadata only"})]}else{vec![]}})).into_response()
        }}));
        (
            origin,
            tauri::async_runtime::spawn(async move { axum::serve(listener, app).await.unwrap() }),
        )
    });
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let f = OwnedIpc::new();
        let input = json!({"enabled":true,"backend":"sear_xng","searxng_url":origin,"max_results":5,"timeout_ms":1000,"inject_as":"user","query_cache_ttl_secs":30,"coalesce_queries":true,"page_text_enabled":false});
        f.invoke("update_search_settings", json!({"input":input}))
            .unwrap();
        for (i, (text, hits)) in [("owned completed", 1), ("owned empty", 0)]
            .into_iter()
            .enumerate()
        {
            let before = chrono::Utc::now().timestamp();
            let value = f
                .invoke("test_search_backend", json!({"text":text}))
                .unwrap();
            assert_eq!(value["error"], Value::Null);
            assert_eq!(value["hits"], json!(hits));
            assert_eq!(value["reuse"], "fresh");
            let at = chrono::DateTime::parse_from_rfc3339(
                value["retrieved_at_utc"]
                    .as_str()
                    .expect("normal/empty completed IPC result reports observed time"),
            )
            .unwrap()
            .timestamp();
            assert!(at >= before && at <= chrono::Utc::now().timestamp());
            assert_eq!(calls.load(Ordering::SeqCst), i + 1);
        }
        let rejected = f
            .invoke("test_search_backend", json!({"text":"owned rejected"}))
            .unwrap();
        assert!(rejected["error"].as_str().unwrap().contains("不会"));
        assert_eq!(rejected["hits"], 0);
        assert!(rejected.get("retrieved_at_utc").is_none());
        assert!(rejected.get("reuse").is_none());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "three explicit queries, no retry or fallback"
        );
    }));
    task.abort();
    let _ = tauri::async_runtime::block_on(task);
    if let Err(panic) = checked {
        std::panic::resume_unwind(panic);
    }
}

// Controlled manual search: real production command macros, MockRuntime and
// owned HTTP/SQLite only. No native app startup, credential or test issuer.
fn invoke_search_view(
    view: &tauri::WebviewWindow<MockRuntime>,
    command: &str,
    args: Value,
) -> Result<Value, Value> {
    get_ipc_response(
        view,
        tauri::webview::InvokeRequest {
            cmd: command.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "http://tauri.localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.into(),
        },
    )
    .map(|value| value.deserialize::<Value>().unwrap())
}

fn manual_search_ready(h: &OwnedIpc, id: &str) {
    let mut events = h.search_ready.0.lock();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !events.iter().any(|(_, value)| value["request_id"] == id) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "owned ready notification missing");
        h.search_ready.1.wait_for(&mut events, left);
    }
}

async fn manual_search_origin() -> (
    String,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{extract::Query, response::IntoResponse, routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let changed = Arc::new(tokio::sync::Notify::new());
    struct BodyOwner(Arc<AtomicUsize>, Arc<tokio::sync::Notify>);
    impl Drop for BodyOwner {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
            self.1.notify_waiters();
        }
    }
    let c = calls.clone();
    let a = active.clone();
    let n = changed.clone();
    let app = Router::new().route("/search", get(move |Query(q): Query<std::collections::HashMap<String,String>>| {
        let c=c.clone();let a=a.clone();let n=n.clone();async move {
            c.fetch_add(1,Ordering::SeqCst);
            if q.get("q").is_some_and(|v| v.contains("slow")) {
                let stream=async_stream::stream! {
                    a.fetch_add(1,Ordering::SeqCst);let _owner=BodyOwner(a,n.clone());n.notify_waiters();
                    yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from_static(b"{\"results\":["));
                    std::future::pending::<()>().await;
                };
                return axum::http::Response::builder().status(200).body(axum::body::Body::from_stream(stream)).unwrap();
            }
            axum::Json(json!({"results":[{"title":"owned manual source","url":"https://owned.invalid/search","content":"owned metadata"}]})).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (origin, calls, active, changed, task)
}

fn configure_manual_search(h: &OwnedIpc, origin: &str) {
    let app = h._app.state::<AppState>();
    let mut cfg = app.config.write();
    cfg.search.enabled = true;
    cfg.search.backend = crate::config::SearchBackendKind::SearXng;
    cfg.search.searxng_url = Some(origin.to_owned());
    cfg.search.timeout_ms = 2000;
}

#[test]
fn manual_search_production_ipc_cancel_owner_concurrency_and_terminal() {
    use std::sync::atomic::Ordering;
    let (origin, calls, active, changed, server) =
        tauri::async_runtime::block_on(manual_search_origin());
    let h = OwnedIpc::new();
    configure_manual_search(&h, &origin);
    assert_eq!(
        h.invoke("get_search_settings", json!({})).unwrap()["manual_search_cancel_supported"],
        true
    );
    let id = uuid::Uuid::new_v4().to_string();
    let view = h.view.clone();
    let pending_id = id.clone();
    let pending = std::thread::spawn(move || {
        invoke_search_view(
            &view,
            "test_search_backend",
            json!({"text":"owned slow","requestId":pending_id,"onReady":"__CHANNEL__:771"}),
        )
    });
    manual_search_ready(&h, &id);
    tauri::async_runtime::block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let notified = changed.notified();
                if active.load(Ordering::SeqCst) == 1 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    });
    let other = tauri::WebviewWindowBuilder::new(&h._app, "other", Default::default())
        .build()
        .unwrap();
    assert_eq!(
        invoke_search_view(&other, "cancel_search_backend", json!({"requestId":id})).unwrap(),
        false
    );
    assert!(h
        .invoke(
            "test_search_backend",
            json!({"text":"owned duplicate","requestId":id,"onReady":"__CHANNEL__:772"})
        )
        .is_err());
    let fast_id = uuid::Uuid::new_v4().to_string();
    let fast = h
        .invoke(
            "test_search_backend",
            json!({"text":"owned fast","requestId":fast_id,"onReady":"__CHANNEL__:773"}),
        )
        .unwrap();
    assert_eq!(fast["hits"], 1);
    assert_eq!(
        h.invoke("cancel_search_backend", json!({"requestId":fast_id}))
            .unwrap(),
        false
    );
    let chat = h
        ._app
        .state::<AppState>()
        .desktop_chat_controls
        .register("main", &id)
        .unwrap();
    assert_eq!(
        h.invoke("cancel_search_backend", json!({"requestId":id}))
            .unwrap(),
        true
    );
    assert_eq!(
        pending.join().unwrap().unwrap_err(),
        "search_backend_cancelled"
    );
    assert_eq!(
        h.invoke("cancel_search_backend", json!({"requestId":id}))
            .unwrap(),
        false
    );
    assert!(h
        ._app
        .state::<AppState>()
        .desktop_chat_controls
        .cancel("main", &id)
        .unwrap());
    drop(chat);
    tauri::async_runtime::block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let notified = changed.notified();
                if active.load(Ordering::SeqCst) == 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    });
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "slow+fast only, no duplicate dispatch or replay"
    );
    server.abort();
}

#[test]
fn manual_search_preparation_db_wait_cancel_zero_http() {
    use futures_util::FutureExt;
    use std::sync::atomic::Ordering;
    let (origin, calls, _, _, server) = tauri::async_runtime::block_on(manual_search_origin());
    let h = OwnedIpc::new();
    configure_manual_search(&h, &origin);
    tauri::async_runtime::block_on(async {
        let connection = h.db.pool().acquire().await.unwrap(); // this owned pool has one connection
        let webview: tauri::Webview<MockRuntime> = h.view.as_ref().clone();
        let window = webview.window();
        let id = uuid::Uuid::new_v4().to_string();
        let pending = test_search_backend(
            window,
            webview,
            h._app.state::<AppState>(),
            "owned preparation".into(),
            Some(id.clone()),
            Some("__CHANNEL__:774".parse().unwrap()),
        );
        tokio::pin!(pending);
        assert!(
            (&mut pending).now_or_never().is_none(),
            "poll actual command through readiness into its held DB wait"
        );
        assert!(h
            .search_ready
            .0
            .lock()
            .iter()
            .any(|(_, value)| value["request_id"] == id));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            h.invoke("cancel_search_backend", json!({"requestId":id}))
                .unwrap(),
            true
        );
        assert_eq!(pending.await.unwrap_err(), "search_backend_cancelled");
        assert_eq!(
            h.invoke("cancel_search_backend", json!({"requestId":id}))
                .unwrap(),
            false
        );
        drop(connection);
        tokio::task::yield_now().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "cancelling preparation must not leave an HTTP continuation"
        );
    });
    server.abort();
}

#[test]
fn manual_search_production_ipc_early_failure_and_legacy_call() {
    use std::sync::atomic::Ordering;
    let (origin, calls, _, _, server) = tauri::async_runtime::block_on(manual_search_origin());
    let h = OwnedIpc::new();
    assert!(h
        .invoke(
            "test_search_backend",
            json!({"text":"owned invalid","requestId":"bad","onReady":"__CHANNEL__:775"})
        )
        .is_err());
    let id = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        h.invoke(
            "test_search_backend",
            json!({"text":"owned disabled","requestId":id,"onReady":"__CHANNEL__:776"})
        )
        .unwrap_err(),
        "联网搜索尚未启用"
    );
    assert_eq!(
        h.invoke("cancel_search_backend", json!({"requestId":id}))
            .unwrap(),
        false
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    configure_manual_search(&h, &origin);
    assert_eq!(
        h.invoke("test_search_backend", json!({"text":"owned legacy"}))
            .unwrap()["hits"],
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[test]
fn locale_production_ipc_narrow_save_and_legacy_contract() {
    let supplied = PathBuf::from(std::env::var("LLMGW_DATA_DIR").expect("owned data directory"));
    let dir = supplied.canonicalize().unwrap();
    assert!(dir.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    assert!(dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("llmgw-personal-test-"));
    let path = crate::config::config_path();
    assert_eq!(path.parent().unwrap().canonicalize().unwrap(), dir);
    let h = OwnedIpc::new();
    let before = h.invoke("get_search_settings", json!({})).unwrap();
    assert_eq!(before["free_search_locale"], "source_default");
    assert_eq!(
        before["free_search_locales"],
        json!(["source_default", "cn_zh", "us_en", "gb_en"])
    );
    let mut input = json!({"enabled":true,"backend":"free_aggregate","searxng_url":null,"max_results":5,"timeout_ms":3000,"inject_as":"system","free_search_locale":"us_en"});
    assert_eq!(
        h.invoke("update_search_settings", json!({"input":input}))
            .unwrap()["free_search_locale"],
        "us_en"
    );
    let app = h._app.state::<AppState>();
    assert_eq!(
        app.config.read().search.free_search_locale,
        crate::config::FreeSearchLocale::UsEn
    );
    assert_eq!(
        app.gateway.cfg_snapshot().search.free_search_locale,
        crate::config::FreeSearchLocale::UsEn
    );
    let persisted: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        persisted.search.free_search_locale,
        crate::config::FreeSearchLocale::UsEn
    );
    input.as_object_mut().unwrap().remove("free_search_locale");
    assert_eq!(
        h.invoke("update_search_settings", json!({"input":input}))
            .unwrap()["free_search_locale"],
        "us_en"
    );
    let mut stale = serde_json::to_value(app.config.read().clone()).unwrap();
    stale["search"]["free_search_locale"] = json!("source_default");
    h.invoke("update_config", json!({"cfg":stale})).unwrap();
    assert_eq!(
        app.config.read().search.free_search_locale,
        crate::config::FreeSearchLocale::UsEn
    );
    input["free_search_locale"] = json!("auto");
    assert!(h
        .invoke("update_search_settings", json!({"input":input}))
        .is_err());
    assert_eq!(
        app.gateway.cfg_snapshot().search.free_search_locale,
        crate::config::FreeSearchLocale::UsEn
    );
    input["free_search_locale"] = json!("source_default");
    assert_eq!(
        h.invoke("update_search_settings", json!({"input":input}))
            .unwrap()["free_search_locale"],
        "source_default"
    );
}

#[path = "conversation_feedback_ipc_tests.rs"]
mod conversation_feedback_ipc_tests;

#[path = "stack_state_ipc_tests.rs"]
mod stack_state_ipc_tests;
