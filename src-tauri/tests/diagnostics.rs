//! Offline regressions for the local diagnostics allowlist and its absence of side effects.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use llm_gateway_lib::cache::{CacheKeyInput, Lookup};
use llm_gateway_lib::config::AppConfig;
use llm_gateway_lib::db::{self, repo};
use llm_gateway_lib::diagnostics::{
    collect, export_json, DiagnosticStatus, DiagnosticsReport, ProxyMode, VpnDiagnostics,
};
use llm_gateway_lib::domain::{Dialect, ModelRef, ModelType, Provider, RemoteAccessKey};
use llm_gateway_lib::error::GatewayError;
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use serde_json::{json, Value};
use tokio::net::TcpListener;

const SECRET: &str = "fixture-diagnostics-secret";
const PROVIDER_NAME: &str = "fixture-private-provider-name";
const REQUEST_BODY: &str = "fixture-private-request-body";
const PRIVATE_PATH: &str = "C:\\fixture-private-user-path\\kernel.exe";
const SUBSCRIPTION: &str =
    "https://fixture-private-subscription.invalid/secret-query?token=private";

fn known_stopped_vpn() -> Option<VpnDiagnostics> {
    Some(VpnDiagnostics {
        running: false,
        mixed_port: 17890,
        has_error: false,
    })
}

fn check(report: &DiagnosticsReport, id: &str) -> DiagnosticStatus {
    report
        .checks
        .iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("missing diagnostic check {id}"))
        .status
}

fn assert_redacted(text: &str) {
    for private in [
        SECRET,
        PROVIDER_NAME,
        REQUEST_BODY,
        PRIVATE_PATH,
        "fixture-private-user-path",
        SUBSCRIPTION,
        "fixture-private-subscription",
        "fixture-private-provider-id",
        "fixture-private-model-id",
        "fixture-private-key-label",
        "fixture-private-key-id",
        "fixture-private-key-hash",
        "fixture-private-session-id",
        "fixture-private-currency",
    ] {
        assert!(!text.contains(private), "report exposed {private}");
    }
}

fn model(name: &str, enabled: bool) -> ModelRef {
    ModelRef {
        enabled,
        alias: name.into(),
        upstream: name.into(),
        context_window: 32768,
        supports_tools: true,
        supports_vision: false,
        supports_audio: false,
        supports_video: false,
        supports_thinking: false,
        supports_stream: true,
        model_type: ModelType::Chat,
        upstream_path: None,
        price: None,
        overrides: None,
        local: None,
        capabilities: None,
    }
}

fn provider(id: &str, base_url: String, enabled: bool, models: Vec<ModelRef>) -> Provider {
    let now = chrono::Utc::now();
    Provider {
        id: id.into(),
        name: PROVIDER_NAME.into(),
        dialect: Dialect::OpenAI,
        base_url,
        api_key_enc: SECRET.into(),
        enabled,
        priority: 0,
        models,
        rpm_limit: 0,
        intelligence: 80,
        note: Some(format!("{PRIVATE_PATH} {SUBSCRIPTION}")),
        runtime_id: None,
        created_at: now,
        updated_at: now,
    }
}

fn access_key(id: &str, enabled: bool, budget: i64, currency: &str) -> RemoteAccessKey {
    let now = chrono::Utc::now();
    RemoteAccessKey {
        id: id.into(),
        label: "fixture-private-key-label".into(),
        key_hash: format!("fixture-private-key-hash-{id}"),
        enabled,
        rpm_limit: 60,
        monthly_budget_micros: budget,
        budget_currency: currency.into(),
        allowed_models: vec!["fixture-private-model-id".into()],
        created_at: now,
        updated_at: now,
    }
}

async fn blank_gateway(cfg: AppConfig) -> GatewayState {
    GatewayState::new(db::Db::connect_in_memory().await.unwrap(), cfg)
}

async fn session_snapshot(gateway: &GatewayState) -> Value {
    let sessions = repo::list_sessions(gateway.db.pool(), 100).await.unwrap();
    let messages = repo::recent_all_messages_with_content(
        gateway.db.pool(),
        "fixture-private-session-id",
        100,
    )
    .await
    .unwrap();
    json!({
        "sessions": sessions,
        "messages": messages.into_iter().map(|message| message.message).collect::<Vec<_>>()
    })
}

#[tokio::test]
async fn report_and_export_are_allowlisted_and_do_not_change_local_state_or_call_upstream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mock_calls = calls.clone();
    let mock = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(move || {
                let calls = mock_calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    "mock upstream"
                }
            }),
        )
        .await
        .unwrap();
    });
    let mut cfg = AppConfig {
        unified_key: SECRET.into(),
        catalog_feed_url: Some(SUBSCRIPTION.into()),
        ..AppConfig::default()
    };
    cfg.cache.enabled = true;
    cfg.cache.ttl_secs = 1;
    cfg.remote_mode.public_url = Some(SUBSCRIPTION.into());
    cfg.smart_routing.jev.auto_start.exe_path = PRIVATE_PATH.into();
    let gateway = blank_gateway(cfg.clone()).await;
    repo::upsert_provider(
        gateway.db.pool(),
        &provider(
            "fixture-private-provider-id",
            format!("http://{address}/{SECRET}"),
            true,
            vec![model("fixture-private-model-id", true)],
        ),
    )
    .await
    .unwrap();
    repo::create_remote_access_key(
        gateway.db.pool(),
        &access_key(
            "fixture-private-key-id",
            true,
            1_000_000,
            "fixture-private-currency",
        ),
    )
    .await
    .unwrap();
    gateway.reload_providers().await.unwrap();
    repo::get_or_create_session(gateway.db.pool(), "fixture-private-session-id")
        .await
        .unwrap();
    repo::append_message(
        gateway.db.pool(),
        "fixture-private-session-id",
        "user",
        REQUEST_BODY,
        None,
        Some("fixture-private-provider-id"),
        Some("fixture-private-model-id"),
        7,
        0,
    )
    .await
    .unwrap();
    gateway.health.record_failure(
        "fixture-private-provider-id",
        "fixture-private-model-id",
        &GatewayError::Other(anyhow::anyhow!("{SECRET} {SUBSCRIPTION} {PRIVATE_PATH}")),
    );
    let request = json!({"messages": [{"role": "user", "content": REQUEST_BODY}]});
    let input = CacheKeyInput {
        provider_id: "fixture-private-provider-id",
        routed_model: "fixture-private-model-id",
        request: &request,
        session_id: "fixture-private-session-id",
        stream: false,
        multimodal: false,
        search_injected: false,
        upstream_status: None,
        response_has_tool_calls: false,
    };
    let Lookup::Miss(miss) = gateway.cache.lookup(&cfg.cache, &input) else {
        panic!("expected empty cache MISS");
    };
    assert!(gateway.cache.store(&miss, json!({"answer": SECRET})));
    // A diagnostic using stats()/lookup would delete this entry and violate read-only behavior.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let before_config = serde_json::to_value(gateway.cfg_snapshot()).unwrap();
    let before_session = session_snapshot(&gateway).await;
    let before_health = serde_json::to_value(gateway.health.snapshot()).unwrap();
    let before_cache = serde_json::to_value(gateway.cache.snapshot_stats()).unwrap();
    let before_generation = gateway.cache.generation();
    assert_eq!(before_cache["entries"], 1);

    let report = collect(&cfg, &gateway, known_stopped_vpn()).await;
    let encoded = serde_json::to_value(&report).unwrap();
    assert_redacted(&encoded.to_string());
    assert_eq!(report.summary.cache_entries, 1);
    assert_eq!(report.summary.cache_misses, 1);
    assert_eq!(check(&report, "upstream_health"), DiagnosticStatus::Warning);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        before_config,
        serde_json::to_value(gateway.cfg_snapshot()).unwrap()
    );
    assert_eq!(before_session, session_snapshot(&gateway).await);
    assert_eq!(
        before_health,
        serde_json::to_value(gateway.health.snapshot()).unwrap()
    );
    assert_eq!(
        before_cache,
        serde_json::to_value(gateway.cache.snapshot_stats()).unwrap()
    );
    assert_eq!(before_generation, gateway.cache.generation());
    assert_eq!(encoded.as_object().unwrap().len(), 5);
    assert_eq!(encoded["summary"].as_object().unwrap().len(), 15);
    assert_eq!(report.schema_version, 1);
    assert!(chrono::DateTime::parse_from_rfc3339(&report.generated_at).is_ok());
    assert_keys(
        &encoded,
        &[
            "schema_version",
            "generated_at",
            "overall",
            "checks",
            "summary",
        ],
    );
    assert_keys(
        &encoded["summary"],
        &[
            "providers_total",
            "providers_enabled",
            "providers_disabled",
            "models_enabled",
            "cache_enabled",
            "cache_capacity",
            "cache_ttl_secs",
            "cache_entries",
            "cache_hits",
            "cache_misses",
            "remote_keys_enabled",
            "budgeted_keys",
            "budget_currency_counts",
            "vpn_running",
            "proxy_mode",
        ],
    );
    for diagnostic in encoded["checks"].as_array().unwrap() {
        assert_eq!(diagnostic.as_object().unwrap().len(), 4);
        assert_keys(diagnostic, &["id", "status", "title", "detail"]);
        assert!(matches!(
            diagnostic["status"].as_str().unwrap(),
            "ok" | "warning" | "error" | "unknown"
        ));
    }
    for currency in encoded["summary"]["budget_currency_counts"]
        .as_array()
        .unwrap()
    {
        assert_keys(currency, &["currency", "count"]);
    }
    let output = TemporaryReport(
        std::env::temp_dir().join(format!("llmgw-diagnostics-{}.json", uuid::Uuid::new_v4())),
    );
    export_json(&report, &output.0).unwrap();
    let exported = std::fs::read_to_string(&output.0).unwrap();
    assert_redacted(&exported);
    assert_eq!(encoded, serde_json::from_str::<Value>(&exported).unwrap());
    mock.abort();
}

fn assert_keys(value: &Value, expected: &[&str]) {
    let actual: std::collections::BTreeSet<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected: std::collections::BTreeSet<_> = expected.iter().copied().collect();
    assert_eq!(actual, expected);
}

struct TemporaryReport(PathBuf);

impl Drop for TemporaryReport {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn another_local_server_on_configured_port_is_not_reported_as_a_healthy_gateway() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mock_calls = calls.clone();
    let mock = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(move || {
                let calls = mock_calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    "healthy unrelated core"
                }
            }),
        )
        .await
        .unwrap();
    });
    let cfg = AppConfig {
        port: address.port(),
        ..AppConfig::default()
    };
    let gateway = blank_gateway(cfg.clone()).await;
    let report = collect(&cfg, &gateway, known_stopped_vpn()).await;
    assert_eq!(check(&report, "listener"), DiagnosticStatus::Unknown);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains(&address.to_string()));
    mock.abort();
}

#[tokio::test]
async fn actual_listener_changes_require_restart_and_cancelled_listener_becomes_unknown() {
    let cfg = AppConfig {
        port: 0,
        ..AppConfig::default()
    };
    let gateway = Arc::new(blank_gateway(cfg.clone()).await);
    let serve_state = gateway.clone();
    let task = tokio::spawn(async move { serve(serve_state).await });
    for _ in 0..200 {
        if gateway.listener_snapshot().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(gateway.listener_snapshot().is_some());
    assert_eq!(
        check(
            &collect(&cfg, &gateway, known_stopped_vpn()).await,
            "listener"
        ),
        DiagnosticStatus::Ok
    );
    let next = AppConfig {
        port: 1,
        ..cfg.clone()
    };
    gateway.update_cfg(next.clone());
    assert_eq!(
        check(
            &collect(&next, &gateway, known_stopped_vpn()).await,
            "listener"
        ),
        DiagnosticStatus::Warning
    );
    gateway.update_cfg(cfg.clone());
    assert_eq!(
        check(
            &collect(&cfg, &gateway, known_stopped_vpn()).await,
            "listener"
        ),
        DiagnosticStatus::Ok
    );
    let mut public_url_changed = cfg.clone();
    public_url_changed.remote_mode.public_url = Some(SUBSCRIPTION.into());
    gateway.update_cfg(public_url_changed.clone());
    assert_eq!(
        check(
            &collect(&public_url_changed, &gateway, known_stopped_vpn()).await,
            "listener"
        ),
        DiagnosticStatus::Warning
    );
    gateway.update_cfg(cfg.clone());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(gateway.listener_snapshot().is_none());
    assert_eq!(
        check(
            &collect(&cfg, &gateway, known_stopped_vpn()).await,
            "listener"
        ),
        DiagnosticStatus::Unknown
    );
}

#[tokio::test]
async fn budgets_count_enabled_keys_and_only_export_canonical_currency_buckets() {
    let mut cfg = AppConfig::default();
    cfg.remote_mode.enabled = true;
    cfg.remote_mode.public_url = Some(SUBSCRIPTION.into());
    let gateway = blank_gateway(cfg.clone()).await;
    let no_keys = collect(&cfg, &gateway, known_stopped_vpn()).await;
    assert_eq!(check(&no_keys, "remote_access"), DiagnosticStatus::Error);
    assert_eq!(check(&no_keys, "budget"), DiagnosticStatus::Ok);
    for key in [
        access_key("usd", true, 100, " USD "),
        access_key("cny", true, 100, "cNy"),
        access_key("unknown", true, 100, "fixture-private-currency"),
        access_key("unlimited", true, 0, "usd"),
        access_key("disabled", false, 100, "usd"),
    ] {
        repo::create_remote_access_key(gateway.db.pool(), &key)
            .await
            .unwrap();
    }
    let report = collect(&cfg, &gateway, known_stopped_vpn()).await;
    assert_eq!(report.summary.remote_keys_enabled, 4);
    assert_eq!(check(&report, "remote_access"), DiagnosticStatus::Ok);
    assert_eq!(report.summary.budgeted_keys, 3);
    assert_eq!(
        serde_json::to_value(&report.summary.budget_currency_counts).unwrap(),
        json!([
            {"currency":"USD", "count":1},
            {"currency":"CNY", "count":1},
            {"currency":"unknown", "count":1}
        ])
    );
    assert_eq!(check(&report, "budget"), DiagnosticStatus::Warning);
    assert_redacted(&serde_json::to_string(&report).unwrap());
}

#[tokio::test]
async fn provider_counts_include_disabled_rows_but_models_only_from_enabled_providers() {
    let cfg = AppConfig::default();
    let gateway = blank_gateway(cfg.clone()).await;
    for provider in [
        provider(
            "enabled",
            SUBSCRIPTION.into(),
            true,
            vec![model("one", true), model("two", false)],
        ),
        provider(
            "disabled",
            SUBSCRIPTION.into(),
            false,
            vec![model("three", true)],
        ),
    ] {
        repo::upsert_provider(gateway.db.pool(), &provider)
            .await
            .unwrap();
    }
    let report = collect(&cfg, &gateway, known_stopped_vpn()).await;
    assert_eq!(report.summary.providers_total, 2);
    assert_eq!(report.summary.providers_enabled, 1);
    assert_eq!(report.summary.providers_disabled, 1);
    assert_eq!(report.summary.models_enabled, 1);
    assert_eq!(check(&report, "providers"), DiagnosticStatus::Ok);
    assert_redacted(&serde_json::to_string(&report).unwrap());
}

#[tokio::test]
async fn managed_proxy_stopped_unknown_and_invalid_states_are_distinguished_without_connections() {
    let gateway = blank_gateway(AppConfig::default()).await;
    let mut cfg = AppConfig::default();
    let report = collect(&cfg, &gateway, None).await;
    assert_eq!(report.summary.vpn_running, None);
    assert_eq!(
        serde_json::to_value(&report).unwrap()["summary"]["vpn_running"],
        Value::Null
    );
    assert_eq!(check(&report, "vpn"), DiagnosticStatus::Unknown);
    assert_eq!(report.summary.proxy_mode, ProxyMode::None);
    cfg.http_proxy = Some("http://127.0.0.1:17890/".into());
    let stopped = collect(&cfg, &gateway, known_stopped_vpn()).await;
    assert_eq!(stopped.summary.proxy_mode, ProxyMode::ManagedVpn);
    assert_eq!(stopped.summary.vpn_running, Some(false));
    assert_eq!(check(&stopped, "proxy"), DiagnosticStatus::Error);
    let running = collect(
        &cfg,
        &gateway,
        Some(VpnDiagnostics {
            running: true,
            mixed_port: 17890,
            has_error: false,
        }),
    )
    .await;
    assert_eq!(running.summary.proxy_mode, ProxyMode::ManagedVpn);
    assert_eq!(running.summary.vpn_running, Some(true));
    assert_eq!(check(&running, "proxy"), DiagnosticStatus::Ok);
    cfg.http_proxy = Some("http://127.0.0.1:7890".into());
    let external = collect(&cfg, &gateway, known_stopped_vpn()).await;
    assert_eq!(external.summary.proxy_mode, ProxyMode::External);
    assert_eq!(check(&external, "proxy"), DiagnosticStatus::Unknown);
    cfg.http_proxy = Some(format!(
        "http://{SECRET}:private@127.0.0.1:17890/{REQUEST_BODY}"
    ));
    let invalid = collect(&cfg, &gateway, known_stopped_vpn()).await;
    assert_eq!(invalid.summary.proxy_mode, ProxyMode::Invalid);
    assert_eq!(check(&invalid, "proxy"), DiagnosticStatus::Error);
    assert_redacted(&serde_json::to_string(&invalid).unwrap());
}

#[tokio::test]
async fn database_and_export_errors_do_not_echo_sql_or_user_paths() {
    let cfg = AppConfig::default();
    let gateway = blank_gateway(cfg.clone()).await;
    gateway.db.pool().close().await;
    let report = collect(&cfg, &gateway, None).await;
    assert_eq!(check(&report, "providers"), DiagnosticStatus::Error);
    assert_eq!(check(&report, "remote_access"), DiagnosticStatus::Error);
    assert_eq!(check(&report, "budget"), DiagnosticStatus::Unknown);
    assert_eq!(report.overall, DiagnosticStatus::Error);
    let encoded = serde_json::to_string(&report).unwrap();
    for forbidden in [
        "SELECT",
        "Sqlite",
        "PoolClosed",
        "pool closed",
        "remote_access_keys",
    ] {
        assert!(
            !encoded.contains(forbidden),
            "raw database detail leaked: {forbidden}"
        );
    }
    assert_redacted(&encoded);
    let destination = std::env::temp_dir()
        .join(format!(
            "fixture-private-user-path-{}",
            uuid::Uuid::new_v4()
        ))
        .join("report.json");
    let error = export_json(&report, &destination).unwrap_err();
    assert!(!error.contains("fixture-private-user-path"));
    assert!(!error.contains(&destination.to_string_lossy().to_string()));
}
