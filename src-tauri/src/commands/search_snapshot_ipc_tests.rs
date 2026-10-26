//! Existing config/provider snapshot command boundary; never whole-file restore.
use super::*;

#[test]
fn search_snapshot_apply_keeps_current_admission_meta_not_old_payload() {
    use crate::search::stack_exchange_admission::{Admission, Block};
    let supplied = PathBuf::from(
        std::env::var("LLMGW_DATA_DIR").expect("owned snapshot data directory required"),
    );
    assert!(supplied.is_absolute());
    let dir = supplied.canonicalize().unwrap();
    let standard_owned = dir.starts_with(std::env::temp_dir().canonicalize().unwrap())
        && dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("llmgw-personal-test-"));
    let exact_owned =
        PathBuf::from("E:/codex/2026-10-14/task/work/stack-exchange-scope-v1/snapshot-owned-data");
    let dedicated_owned = exact_owned
        .canonicalize()
        .is_ok_and(|expected| expected == dir)
        && dir.join("owned-run.json").is_file();
    assert!(
        standard_owned || dedicated_owned,
        "owned snapshot fixture directory required"
    );
    assert_eq!(
        crate::config::config_path()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap(),
        dir,
        "validate the actual save target before invoking a write"
    );
    const KEY: &str = "search.stack_exchange.admission.v1";
    for phase in ["reserved", "authorized", "exhausted"] {
        let h = OwnedIpc::new();
        let gate = Admission::new(h.db.clone());
        let reservation = tauri::async_runtime::block_on(
            gate.reserve(tokio::time::Instant::now() + std::time::Duration::from_secs(9)),
        )
        .unwrap();
        let old_value: String = tauri::async_runtime::block_on(
            sqlx::query_scalar("SELECT value FROM meta WHERE key=?")
                .bind(KEY)
                .fetch_one(h.db.pool()),
        )
        .unwrap();
        let old_limit = h._app.state::<AppState>().config.read().search.max_results;
        let id = h
            .invoke("create_snapshot", json!({"name":"owned search boundary"}))
            .unwrap();
        let id = id.as_str().unwrap().to_owned();
        tauri::async_runtime::block_on(async {
            let snap = repo::get_snapshot(h.db.pool(), &id).await.unwrap().unwrap();
            assert!(
                snap.payload.get("meta").is_none(),
                "native config snapshots must not copy admission facts"
            );
            let mut payload = snap.payload;
            // An ignored legacy/foreign field must not become a whole-DB restore.
            payload["meta"] = json!({KEY:serde_json::from_str::<Value>(&old_value).unwrap()});
            sqlx::query("UPDATE snapshots SET payload=? WHERE id=?")
                .bind(payload.to_string())
                .bind(&id)
                .execute(h.db.pool())
                .await
                .unwrap();
        });
        if phase != "reserved" {
            let a = tauri::async_runtime::block_on(gate.authorize(reservation)).unwrap();
            if phase == "exhausted" {
                let observed = chrono::Utc::now().timestamp_millis();
                let controls = crate::search::stack_exchange::inspect(
                    r#"{"items":[],"quota_remaining":0}"#,
                    1,
                    observed,
                )
                .unwrap()
                .controls;
                tauri::async_runtime::block_on(gate.finish(a, &controls, observed)).unwrap();
            }
        } else {
            drop(reservation);
            // Owned wait fixture: applying old config must not shorten a newer
            // policy record even if authorization has not started yet.
            tauri::async_runtime::block_on(async {
                let mut row: Value = serde_json::from_str(&old_value).unwrap();
                row["server_not_before_ms"] =
                    (chrono::Utc::now().timestamp_millis() + 120_000).into();
                sqlx::query("UPDATE meta SET value=? WHERE key=?")
                    .bind(row.to_string())
                    .bind(KEY)
                    .execute(h.db.pool())
                    .await
                    .unwrap();
            });
        }
        let before: (String, String) = tauri::async_runtime::block_on(
            sqlx::query_as("SELECT value,updated_at FROM meta WHERE key=?")
                .bind(KEY)
                .fetch_one(h.db.pool()),
        )
        .unwrap();
        let view_before = tauri::async_runtime::block_on(gate.view()).unwrap();
        h._app.state::<AppState>().config.write().search.max_results =
            if old_limit == 10 { 1 } else { 10 };
        let applied = h.invoke("apply_snapshot", json!({"id":id})).unwrap();
        assert_eq!(applied["config"]["search"]["max_results"], old_limit);
        assert_eq!(
            h._app.state::<AppState>().config.read().search.max_results,
            old_limit,
            "actual config restore must occur"
        );
        let after: (String, String) = tauri::async_runtime::block_on(
            sqlx::query_as("SELECT value,updated_at FROM meta WHERE key=?")
                .bind(KEY)
                .fetch_one(h.db.pool()),
        )
        .unwrap();
        assert_eq!(
            before, after,
            "config/provider snapshot must not roll back {phase} admission record"
        );
        assert_eq!(
            tauri::async_runtime::block_on(gate.view()).unwrap(),
            view_before
        );
        if phase == "authorized" {
            assert_eq!(
                tauri::async_runtime::block_on(gate.recover_unsent()).unwrap_err(),
                Block::Unresolved
            );
        }
        if phase == "exhausted" {
            assert_eq!(
                tauri::async_runtime::block_on(gate.recover_unsent()).unwrap_err(),
                Block::Exhausted
            );
        }
        let requests: i64 = tauri::async_runtime::block_on(
            sqlx::query_scalar("SELECT count(*) FROM requests").fetch_one(h.db.pool()),
        )
        .unwrap();
        let secrets: i64 = tauri::async_runtime::block_on(
            sqlx::query_scalar("SELECT count(*) FROM app_secrets").fetch_one(h.db.pool()),
        )
        .unwrap();
        assert_eq!(requests, 0);
        assert_eq!(secrets, 0);
    }
}
