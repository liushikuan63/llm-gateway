use super::*;
use crate::{
    config::{FreeSearchScope, SearchBackendKind},
    search::stack_exchange_admission::Admission,
};
fn selected() -> OwnedIpc {
    OwnedIpc::with_gateway(|g| {
        let mut c = g.cfg_snapshot();
        c.search.backend = SearchBackendKind::FreeAggregate;
        c.search.free_search_scope = FreeSearchScope::StackOverflow;
        g.update_cfg(c);
    })
}
fn raw(h: &OwnedIpc) -> String {
    tauri::async_runtime::block_on(
        sqlx::query_scalar("SELECT value FROM meta WHERE key='search.stack_exchange.admission.v1'")
            .fetch_one(h.db.pool()),
    )
    .unwrap()
}
fn row_count(h: &OwnedIpc, table: &str) -> i64 {
    assert!(["meta", "requests", "app_secrets"].contains(&table));
    tauri::async_runtime::block_on(
        sqlx::query_scalar(&format!("SELECT count(*) FROM {table}")).fetch_one(h.db.pool()),
    )
    .unwrap()
}
#[test]
fn stack_state_ipc_projects_only_selected_protection_without_reserving_or_querying() {
    let legacy = OwnedIpc::new();
    let v = legacy.invoke("get_search_settings", json!({})).unwrap();
    assert!(
        v["free_search_scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("stack_overflow")),
        "DB-backed production capability required"
    );
    assert!(v.get("stack_overflow_state").is_none());
    let h = selected();
    let before = row_count(&h, "meta");
    let v = h.invoke("get_search_settings", json!({})).unwrap();
    assert_eq!(v["stack_overflow_state"]["schema_version"], 1);
    assert_eq!(v["stack_overflow_state"]["state"], "ready");
    assert_eq!(v["stack_overflow_state"]["shared_remaining"], Value::Null);
    assert!(
        v.get("source_protection").is_none(),
        "never project Web source state for Stack Overflow"
    );
    assert_eq!(row_count(&h, "meta"), before);
    for table in ["requests", "app_secrets"] {
        assert_eq!(row_count(&h, table), 0);
    }
    let key = "search.stack_exchange.admission.v1";
    tauri::async_runtime::block_on(
        sqlx::query("INSERT INTO meta(key,value,updated_at) VALUES(?,?,'owned')")
            .bind(key)
            .bind("invalid-owned-state")
            .execute(h.db.pool()),
    )
    .unwrap();
    let v = h.invoke("get_search_settings", json!({})).unwrap();
    assert_eq!(v["stack_overflow_state"]["state"], "unavailable");
    assert_eq!(v["stack_overflow_state"]["shared_remaining"], Value::Null);
    assert_eq!(raw(&h), "invalid-owned-state");
}
#[test]
fn stack_state_ipc_recovery_requires_active_scope_and_proven_unsent_preserves_ledger_restrictions()
{
    let inactive = OwnedIpc::new();
    assert!(inactive
        .invoke("recover_stack_overflow_unsent", json!({}))
        .is_err());
    for phase in ["reserved", "authorized", "zero", "legacy"] {
        let h = selected();
        let gate = Admission::new(h.db.clone());
        let r = tauri::async_runtime::block_on(
            gate.reserve(tokio::time::Instant::now() + std::time::Duration::from_secs(9)),
        )
        .unwrap();
        if phase == "authorized" || phase == "zero" {
            let a = tauri::async_runtime::block_on(gate.authorize(r)).unwrap();
            if phase == "zero" {
                let now = chrono::Utc::now().timestamp_millis();
                let controls = crate::search::stack_exchange::inspect(
                    r#"{"items":[],"quota_remaining":0}"#,
                    1,
                    now,
                )
                .unwrap()
                .controls;
                tauri::async_runtime::block_on(gate.finish(a, &controls, now)).unwrap();
            }
        }
        if phase == "legacy" {
            let mut v: Value = serde_json::from_str(&raw(&h)).unwrap();
            v["schema_version"] = json!(1);
            v.as_object_mut().unwrap().remove("phase");
            tauri::async_runtime::block_on(
                sqlx::query(
                    "UPDATE meta SET value=? WHERE key='search.stack_exchange.admission.v1'",
                )
                .bind(v.to_string())
                .execute(h.db.pool()),
            )
            .unwrap();
        }
        let before = raw(&h);
        let response = h.invoke("recover_stack_overflow_unsent", json!({}));
        if phase == "reserved" {
            let v = response.expect(
                "registered production recovery must recover only proven no-send reservation",
            );
            assert_eq!(v["state"], "waiting");
            assert_eq!(v["can_recover_unsent"], false);
            assert_eq!(v["shared_remaining"], Value::Null);
            let b: Value = serde_json::from_str(&before).unwrap();
            let a: Value = serde_json::from_str(&raw(&h)).unwrap();
            assert_ne!(b["attempt_id"], a["attempt_id"]);
            assert_eq!(a["phase"], "not_sent");
            for key in [
                "ledger_id",
                "created_ms",
                "local_not_before_ms",
                "server_not_before_ms",
                "reported_remaining",
                "quota_observed_ms",
            ] {
                assert_eq!(b[key], a[key], "{key}");
            }
        } else {
            assert_eq!(
                response.unwrap_err(),
                json!("stack_overflow_recovery_not_proven")
            );
            assert_eq!(raw(&h), before);
        }
        for table in ["requests", "app_secrets"] {
            assert_eq!(row_count(&h, table), 0);
        }
    }
}

#[test]
fn stack_state_ipc_explicit_scope_save_uses_actual_command_and_owned_config_only() {
    let supplied = PathBuf::from(
        std::env::var("LLMGW_DATA_DIR").expect("owned save runner data directory required"),
    );
    assert!(supplied.is_absolute());
    let dir = supplied.canonicalize().unwrap();
    let standard = dir.starts_with(std::env::temp_dir().canonicalize().unwrap())
        && dir
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|v| v.starts_with("llmgw-personal-test-"));
    let exact =
        PathBuf::from("E:/codex/2026-10-14/task/work/stack-exchange-routing-v1/state-owned-data");
    assert!(
        standard
            || (exact.canonicalize().is_ok_and(|v| v == dir)
                && dir.join("owned-run.json").is_file())
    );
    assert_eq!(
        crate::config::config_path()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap(),
        dir,
        "validate actual write target first"
    );
    let h = OwnedIpc::new();
    let mut input = h.invoke("get_search_settings", json!({})).unwrap();
    input["backend"] = json!("free_aggregate");
    input["free_search_scope"] = json!("stack_overflow");
    input["api_key"] = Value::Null;
    input["clear_api_key"] = json!(false);
    let saved = h
        .invoke("update_search_settings", json!({"input":input}))
        .expect("actual command must accept new explicit source");
    assert_eq!(saved["free_search_scope"], "stack_overflow");
    assert_eq!(saved["stack_overflow_state"]["state"], "ready");
    let state = h._app.state::<AppState>();
    assert_eq!(
        state.config.read().search.free_search_scope,
        FreeSearchScope::StackOverflow
    );
    assert_eq!(
        state.gateway.cfg_snapshot().search.free_search_scope,
        FreeSearchScope::StackOverflow
    );
    let stored = std::fs::read_to_string(crate::config::config_path()).unwrap();
    assert!(stored.contains("free_search_scope = \"stack_overflow\""));
    for table in ["requests", "app_secrets"] {
        assert_eq!(row_count(&h, table), 0);
    }
    assert!(tauri::async_runtime::block_on(
        sqlx::query_scalar::<_, String>(
            "SELECT value FROM meta WHERE key='search.stack_exchange.admission.v1'"
        )
        .fetch_optional(h.db.pool())
    )
    .unwrap()
    .is_none());
}
