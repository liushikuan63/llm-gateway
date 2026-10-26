//! Production command macro contract, owned SQLite only; no window/backend boot.
use super::*;

#[test]
fn conversation_feedback_read_commands_preserve_scope_and_private_marker_fields() {
    let h = OwnedIpc::new();
    let principal = "a".repeat(64);
    let trace = "123456789abcdef0123456789abcdef01";
    let marker = json!([{"provider_id":"owned","model":"physical","trace_id":trace,"attempt":1,"native_exact":{"version":1,"profile_id":"cn-web-raw-v1","account_id":"private-ipc-slot","principal_hash":principal,"source_function":"chat_v3","attempt":1,"upstream_post_started":false}}]);
    let decision = json!({"schema_version":1,"kind":"exact_selection","phase":"native_exact_selected","captured_at_unix_ms":1,"configured_strategy":"priority","personal_test":false,"classifier":null,"intent":null,"sticky":null,"sticky_unavailable":false,"active_promoted":false,"free_selector":false,"cascade_order_applied":false,"total_candidates":1,"truncated":false,"omitted_candidates":0,"score_state":"not_applicable","candidates":[{"ordinal":0,"target":{"provider_id":"owned","model_alias":"alias","configured_upstream":"physical","exact_match":true}}]});
    let id:i64=tauri::async_runtime::block_on(sqlx::query_scalar("INSERT INTO requests(ts,session_id,requested_model,routed_provider,routed_model,status,trace_id,route_intent,attempts_json,route_decision_json) VALUES(1,'ipc-session','alias','owned','physical',499,?,'native_exact',?,?) RETURNING id").bind(trace).bind(marker.to_string()).bind(decision.to_string()).fetch_one(h.db.pool())).unwrap();
    let summary = h
        .invoke(
            "get_conversation_feedback",
            json!({"filter":{},"page":null}),
        )
        .unwrap();
    assert_eq!(summary["total_tasks"], 1);
    assert_eq!(summary["tasks"][0]["counts"]["cancelled"], 1);
    assert!(!summary.to_string().contains("private-ipc-slot"));
    assert!(!summary.to_string().contains(&principal));
    let task = summary["tasks"][0]["task"].clone();
    let rows = h
        .invoke(
            "get_conversation_requests",
            json!({"task":task,"filter":{},"afterId":null,"limit":1}),
        )
        .unwrap();
    assert_eq!(rows["rows"][0]["id"], id);
    assert_eq!(rows["rows"][0]["status"], 499);
    assert!(rows["rows"][0]["outcome"].is_null());
    assert_eq!(h.invoke("get_conversation_requests",json!({"task":summary["tasks"][0]["task"],"filter":{},"afterId":9007199254740991i64,"limit":1})).unwrap_err(),json!("task_cursor_invalid"));
    assert_eq!(
        h.invoke(
            "get_conversation_feedback",
            json!({"filter":{"from_ts":2,"to_ts":1},"page":null})
        )
        .unwrap_err(),
        json!("task_invalid_input")
    );
    assert_eq!(
        tauri::async_runtime::block_on(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM requests").fetch_one(h.db.pool())
        )
        .unwrap(),
        1
    );
}
