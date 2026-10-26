//! Actual command macro decoding and owned SQLite; no native window or boot.
use super::*;

#[test]
fn s6_declaration_production_ipc_roundtrip_and_legacy_compatibility() {
    let h = OwnedIpc::new();
    let mut input = json!({"id":"s6-owned-ipc","name":"Owned S6 IPC","dialect":"openai",
        "base_url":"http://127.0.0.1:1/v1","api_key":"","enabled":true,"priority":10,
        "rpm_limit":0,"intelligence":50,"note":null,
        "models":[{"alias":"m","upstream":"physical","context_window":8192,
            "supports_tools":false,"supports_vision":false,"supports_stream":true}],
        "capability_declarations":{"m":{"binding":"new","revision":"new","fields":{"tools":"unsupported"}}}});
    assert_eq!(
        h.invoke("upsert_provider", json!({"input":input.clone()}))
            .unwrap(),
        "s6-owned-ipc"
    );
    let current = h.invoke("list_providers", json!({})).unwrap();
    let declaration = &current[0]["capability_declarations"]["m"];
    assert_eq!(declaration["fields"]["tools"]["state"], "unsupported");
    assert_eq!(current[0]["models"][0]["supports_tools"], false);
    input["capability_declarations"]["m"] = json!({"binding":declaration["binding"],"revision":declaration["revision"],"fields":{"tools":"unknown"}});
    h.invoke("upsert_provider", json!({"input":input.clone()}))
        .unwrap();
    assert!(
        h.invoke("upsert_provider", json!({"input":input.clone()}))
            .is_err(),
        "old captured revision must not save again"
    );
    input
        .as_object_mut()
        .unwrap()
        .remove("capability_declarations");
    input["name"] = json!("Old client keeps declarations");
    h.invoke("upsert_provider", json!({"input":input.clone()}))
        .unwrap();
    let changes: i64 = tauri::async_runtime::block_on(
        sqlx::query_scalar("SELECT total_changes()").fetch_one(h.db.pool()),
    )
    .unwrap();
    let read = h.invoke("list_providers", json!({})).unwrap();
    assert_eq!(
        read[0]["capability_declarations"]["m"]["fields"]["tools"]["state"],
        "unknown"
    );
    let after: i64 = tauri::async_runtime::block_on(
        sqlx::query_scalar("SELECT total_changes()").fetch_one(h.db.pool()),
    )
    .unwrap();
    assert_eq!(
        changes, after,
        "read projection must not mint or save metadata"
    );
    let state = h._app.state::<AppState>();
    assert!(!state.gateway.providers.read()[0].models[0].supports_tools);
    input["capability_declarations"] =
        json!({"m":{"binding":"new","revision":"new","fields":{"tools":"supplier_verified"}}});
    assert!(h.invoke("upsert_provider", json!({"input":input})).is_err());
}
