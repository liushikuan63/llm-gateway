use super::*;
use serde_json::{json, Value};

fn contract() -> Contract {
    Contract::new(&json!({"query": "array methods"})).unwrap()
}

fn initialization() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "jsonrpc":"2.0", "id":1,
        "result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{}},
                  "serverInfo":{"name":"synthetic-mdn","version":"fixture"}}
    }))
    .unwrap()
}

fn ready_search() -> Contract {
    let mut c = contract();
    c.initialize_request().unwrap();
    c.accept_initialize_json(&initialization(), None).unwrap();
    c.initialized_notification().unwrap();
    c.accept_initialized_status(202, b"").unwrap();
    c
}

fn awaiting_search() -> Contract {
    let mut c = ready_search();
    c.search_request().unwrap();
    c
}

fn text_response(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"jsonrpc":"2.0","id":2,
        "result":{"content":[{"type":"text","text":text}]}}))
    .unwrap()
}

#[test]
fn valid_public_query_has_a_bounded_contract() {
    assert!(Contract::new(&json!({"query":"array methods"})).is_ok());
}

#[test]
fn parsed_input_has_exact_string_field_and_local_unicode_bounds() {
    for v in [
        Value::Null,
        json!([]),
        json!("array methods"),
        json!({}),
        json!({"query":1}),
        json!({"query":null}),
        json!({"query":""}),
        json!({"query":"  "}),
        json!({"query":"x\ny"}),
        json!({"query":"x\0y"}),
        json!({"query":"x", "tool":"get-doc"}),
        json!({"query":"a".repeat(257)}),
    ] {
        assert!(Contract::new(&v).is_err());
    }
    assert!(Contract::new(&json!({"query":"a".repeat(256)})).is_ok());
    assert!(Contract::new(&json!({"query":"🦀".repeat(256)})).is_ok());
    assert!(Contract::new(&json!({"query":"🦀".repeat(257)})).is_err());
}

#[test]
fn descriptions_have_fixed_destination_headers_and_software_identity_only() {
    let mut c = contract();
    let r = c.initialize_request().unwrap();
    assert_eq!(r.endpoint(), "https://mcp.mdn.mozilla.net/");
    assert_eq!(r.http_method(), "POST");
    assert_eq!(
        r.headers(),
        &[
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2025-03-26"),
            ("X-Moz-1st-Party-Data-Opt-Out", "1"),
        ]
    );
    assert_eq!(
        r.body(),
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{},
        "clientInfo":{"name":"LLM-Gateway-MDN-Contract","version":env!("CARGO_PKG_VERSION")}}})
    );
    assert!(!format!("{r:?}").contains("array methods"));
}

#[test]
fn serial_plan_returns_opaque_reference_without_fabricated_document_count() {
    let mut c = contract();
    assert_eq!(c.phase(), Phase::Created);
    c.initialize_request().unwrap();
    assert_eq!(c.phase(), Phase::AwaitInitialize);
    c.accept_initialize_json(&initialization(), None).unwrap();
    let notification = c.initialized_notification().unwrap();
    assert_eq!(
        notification.body(),
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    );
    c.accept_initialized_status(202, b"").unwrap();
    let request = c.search_request().unwrap();
    assert_eq!(
        request.body(),
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":"search","arguments":{"query":"array methods"}}})
    );
    assert_eq!(request.endpoint(), ENDPOINT);
    let text = "# Synthetic title\n`path`: `/en-US/docs/Web/JavaScript`\n\nSynthetic text.";
    let reference = c.accept_search_json(&text_response(text), None).unwrap();
    assert_eq!(reference.untrusted_text(), text);
    assert_eq!(reference.document_count(), None);
    assert_eq!(c.phase(), Phase::Complete);
    assert!(!format!("{reference:?}").contains("Synthetic title"));
    assert!(c.search_request().is_err());
    assert_eq!(c.phase(), Phase::Blocked);
}

#[test]
fn out_of_order_or_duplicate_descriptor_permanently_blocks_contract() {
    let mut c = contract();
    assert!(c.search_request().is_err());
    assert_eq!(c.phase(), Phase::Blocked);
    assert!(c.initialize_request().is_err());
    let mut c = contract();
    c.initialize_request().unwrap();
    assert!(c.initialize_request().is_err());
    assert!(c.accept_initialize_json(&initialization(), None).is_err());
    let mut c = ready_search();
    assert!(c.initialized_notification().is_err());
    assert!(c.search_request().is_err());
}

#[test]
fn initialization_rejects_id_version_capability_and_session_without_reopening() {
    let base: Value = serde_json::from_slice(&initialization()).unwrap();
    for mode in [
        "id",
        "string_id",
        "version",
        "no_tools",
        "server_request",
        "session",
    ] {
        let mut value = base.clone();
        match mode {
            "id" => value["id"] = json!(2),
            "string_id" => value["id"] = json!("1"),
            "version" => value["result"]["protocolVersion"] = json!("2099-01-01"),
            "no_tools" => value["result"]["capabilities"] = json!({}),
            "server_request" => {
                value["method"] = json!("sampling/createMessage");
            }
            _ => {}
        }
        let mut c = contract();
        c.initialize_request().unwrap();
        let session = (mode == "session").then_some("PRIVATE_SESSION_VALUE");
        assert!(
            c.accept_initialize_json(&serde_json::to_vec(&value).unwrap(), session)
                .is_err(),
            "{mode}"
        );
        assert_eq!(c.phase(), Phase::Blocked);
        assert!(c.initialized_notification().is_err());
    }
}

#[test]
fn initialized_requires_supplied_202_and_empty_body_before_search_description() {
    for (status, body) in [
        (200, b"".as_slice()),
        (202, b"{}".as_slice()),
        (403, b"PRIVATE".as_slice()),
    ] {
        let mut c = contract();
        c.initialize_request().unwrap();
        c.accept_initialize_json(&initialization(), None).unwrap();
        c.initialized_notification().unwrap();
        assert!(c.accept_initialized_status(status, body).is_err());
        assert_eq!(c.phase(), Phase::Blocked);
        assert!(c.search_request().is_err());
    }
}

#[test]
fn response_envelope_never_accepts_remote_errors_mismatched_ids_or_batches() {
    for body in [
        r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"x"}]}}"#,
        r#"{"jsonrpc":"2.0","id":"2","result":{"content":[{"type":"text","text":"x"}]}}"#,
        r#"{"jsonrpc":"1.0","id":2,"result":{"content":[{"type":"text","text":"x"}]}}"#,
        r#"{"jsonrpc":"2.0","id":2,"error":{"code":-1,"message":"PRIVATE"}}"#,
        r#"{"jsonrpc":"2.0","id":2,"result":{},"error":null}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","result":{}}"#,
        r#"[{"jsonrpc":"2.0","id":2,"result":{}}]"#,
        "data: {\"jsonrpc\":\"2.0\"}\n\n",
    ] {
        let mut c = awaiting_search();
        let error = c.accept_search_json(body.as_bytes(), None).unwrap_err();
        assert!(!error.to_string().contains("PRIVATE"));
        assert_eq!(c.phase(), Phase::Blocked);
    }
}

#[test]
fn raw_response_duplicate_known_keys_are_rejected_before_value_conversion() {
    for body in [
        r#"{"jsonrpc":"2.0","id":2,"id":2,"result":{"content":[{"type":"text","text":"x"}]}}"#,
        r#"{"jsonrpc":"2.0","id":2,"result":{"content":[],"content":[{"type":"text","text":"x"}]}}"#,
        r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"x","text":"y"}]}}"#,
    ] {
        let mut c = awaiting_search();
        assert!(c.accept_search_json(body.as_bytes(), None).is_err());
        assert_eq!(c.phase(), Phase::Blocked);
    }
}

#[test]
fn tool_result_accepts_only_one_bounded_nonempty_text_block() {
    for result in [
        json!({"content":[]}),
        json!({"content":[{"type":"image","data":"PRIVATE"}]}),
        json!({"content":[{"type":"text","text":"x"},{"type":"text","text":"y"}]}),
        json!({"content":[{"type":"text","text":1}]}),
        json!({"content":[{"type":"text","text":"x"}],"isError":true}),
        json!({"content":[{"type":"text","text":"x"}],"structuredContent":{}}),
        json!({"content":[{"type":"text","text":""}]}),
        json!({"content":[{"type":"text","text":" \n"}]}),
        json!({"content":[{"type":"text","text":"x\0y"}]}),
        json!({"content":[{"type":"text","text":"a".repeat(MAX_TEXT_BYTES+1)}]}),
    ] {
        let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":2,"result":result})).unwrap();
        let mut c = awaiting_search();
        assert!(c.accept_search_json(&body, None).is_err());
        assert_eq!(c.phase(), Phase::Blocked);
    }
    let mut c = awaiting_search();
    assert_eq!(
        c.accept_search_json(&text_response(&"a".repeat(MAX_TEXT_BYTES)), None)
            .unwrap()
            .untrusted_text()
            .len(),
        MAX_TEXT_BYTES
    );
}

#[test]
fn response_byte_limit_applies_before_json_and_sessions_are_not_saved() {
    let mut c = awaiting_search();
    assert!(c
        .accept_search_json(&vec![b' '; MAX_REPLY_BYTES + 1], None)
        .is_err());
    assert_eq!(c.phase(), Phase::Blocked);
    let mut c = awaiting_search();
    assert!(c
        .accept_search_json(&text_response("x"), Some("PRIVATE_SESSION"))
        .is_err());
    assert_eq!(c.phase(), Phase::Blocked);
}

#[test]
fn indistinguishable_document_boundaries_remain_opaque_and_count_unknown() {
    let first = "# First\n`path`: `/en-US/docs/Web/API/Fetch_API`\n\nSummary A.";
    let second = "# Second\n`path`: `/en-US/docs/Web/JavaScript`\n\nSummary B.";
    let two_docs_text = format!("{first}\n\n{second}");
    let one_summary_text =
        format!("# First\n`path`: `/en-US/docs/Web/API/Fetch_API`\n\nSummary A.\n\n{second}");
    assert_eq!(two_docs_text, one_summary_text);
    let mut c = awaiting_search();
    let r = c
        .accept_search_json(&text_response(&one_summary_text), None)
        .unwrap();
    assert_eq!(r.untrusted_text(), one_summary_text);
    assert_eq!(r.document_count(), None);
    let mut c = awaiting_search();
    assert_eq!(
        c.accept_search_json(&text_response("No results found, perhaps try again."), None)
            .unwrap()
            .document_count(),
        None
    );
}

#[test]
fn query_and_text_do_not_leak_in_debug_or_fixed_errors() {
    let mut c = Contract::new(&json!({"query":"PRIVATE_SYNTHETIC_QUERY"})).unwrap();
    assert!(!format!("{c:?}").contains("PRIVATE_SYNTHETIC_QUERY"));
    let request = c.initialize_request().unwrap();
    assert!(!format!("{request:?}").contains("PRIVATE_SYNTHETIC_QUERY"));
    c.accept_initialize_json(&initialization(), None).unwrap();
    c.initialized_notification().unwrap();
    c.accept_initialized_status(202, b"").unwrap();
    let request = c.search_request().unwrap();
    assert!(!format!("{request:?}").contains("PRIVATE_SYNTHETIC_QUERY"));
    let reference = c
        .accept_search_json(&text_response("PRIVATE_SYNTHETIC_REFERENCE"), None)
        .unwrap();
    assert!(!format!("{reference:?}").contains("PRIVATE_SYNTHETIC_REFERENCE"));
}

#[test]
fn codec_does_not_advertise_runtime_search_support_or_change_defaults() {
    assert!(serde_json::from_str::<crate::config::FreeSearchScope>("\"mdn_mcp\"").is_err());
    assert_eq!(
        crate::config::FreeSearchScope::default(),
        crate::config::FreeSearchScope::Web
    );
}

// Review regressions: serde's default struct sequence form is not JSON-RPC.
#[test]
fn review_rejects_positional_envelope() {
    let body =
        serde_json::to_vec(&json!(["2.0",2,{"content":[{"type":"text","text":"synthetic"}]}]))
            .unwrap();
    let mut c = awaiting_search();
    assert!(c.accept_search_json(&body, None).is_err());
    assert_eq!(c.phase(), Phase::Blocked);
}

#[test]
fn review_rejects_positional_tool_result() {
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":2,
        "result":[[{"type":"text","text":"synthetic"}]]}))
    .unwrap();
    let mut c = awaiting_search();
    assert!(c.accept_search_json(&body, None).is_err());
}

#[test]
fn review_rejects_positional_text_block() {
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":2,
        "result":{"content":[["text","synthetic"]]}}))
    .unwrap();
    let mut c = awaiting_search();
    assert!(c.accept_search_json(&body, None).is_err());
}

#[test]
fn review_rejects_positional_initialize_result() {
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,
        "result":[PROTOCOL_VERSION,{"tools":{}},{"name":"synthetic","version":"fixture"}]}))
    .unwrap();
    let mut c = contract();
    c.initialize_request().unwrap();
    assert!(c.accept_initialize_json(&body, None).is_err());
}

#[test]
fn review_rejects_positional_server_identity() {
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,
        "result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{}},
            "serverInfo":["synthetic","fixture"]}}))
    .unwrap();
    let mut c = contract();
    c.initialize_request().unwrap();
    assert!(c.accept_initialize_json(&body, None).is_err());
}

#[test]
fn review_rejects_duplicate_consumed_tools_before_value_overwrite() {
    for capabilities in [
        r#"{"tools":null,"tools":{}}"#,
        r#"{"tools":{},"tools":null}"#,
        r#"{"tools":{},"tools":{}}"#,
    ] {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"{PROTOCOL_VERSION}","capabilities":{capabilities},"serverInfo":{{"name":"synthetic","version":"fixture"}}}}}}"#
        );
        let mut c = contract();
        c.initialize_request().unwrap();
        assert!(c.accept_initialize_json(body.as_bytes(), None).is_err());
        assert_eq!(c.phase(), Phase::Blocked);
    }
}

#[test]
fn review_unknown_capabilities_never_become_actions_but_tools_requires_object() {
    let mut init: Value = serde_json::from_slice(&initialization()).unwrap();
    init["result"]["capabilities"]["sampling"] = json!({"PRIVATE_IGNORED":"not permission"});
    let mut c = contract();
    c.initialize_request().unwrap();
    assert!(c
        .accept_initialize_json(&serde_json::to_vec(&init).unwrap(), None)
        .is_ok());
    for unsupported in [Value::Null, json!([]), json!(true)] {
        let mut v = init.clone();
        v["result"]["capabilities"]["tools"] = unsupported;
        let mut c = contract();
        c.initialize_request().unwrap();
        assert!(c
            .accept_initialize_json(&serde_json::to_vec(&v).unwrap(), None)
            .is_err());
    }
}
