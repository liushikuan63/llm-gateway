use base64::{engine::general_purpose::STANDARD, Engine};
use llm_gateway_lib::{
    benefit_login::decode_channel,
    benefits::ClaimPlatform,
    client_session::{contains_credential, request_headers, SessionCredential},
};
use serde_json::json;
fn raw() -> String {
    json!({"schema_version":1,"platform":"qoder_cn","headers":{"authorization":"Bearer synthetic-session-secret","cosy-machinetoken":"synthetic-device-secret","cosy-machineos":"Windows"},"snapshot":{"display_name":"离线测试","identity_hash":"fixture-id"}}).to_string()
}
#[test]
fn delegated_context_is_platform_bound_and_headers_are_sensitive() {
    let r = raw();
    assert!(SessionCredential::parse(&r, ClaimPlatform::Qoder).is_err());
    let h = request_headers(&r, ClaimPlatform::QoderCn).unwrap();
    assert!(h["authorization"].is_sensitive());
    assert!(h["cosy-machinetoken"].is_sensitive());
    assert!(
        request_headers("legacy-synthetic", ClaimPlatform::Qoder).unwrap()["authorization"]
            .is_sensitive()
    );
}

#[test]
fn trae_native_authorization_is_bound_to_platform_and_client_source() {
    let raw=json!({"schema_version":1,"platform":"trae_cn","request_source":1,"headers":{"authorization":"Cloud-IDE-JWT synthetic-trae-secret"}}).to_string();
    let session = SessionCredential::parse(&raw, ClaimPlatform::TraeCn).unwrap();
    assert_eq!(session.request_source, Some(1));
    assert!(SessionCredential::parse(&raw, ClaimPlatform::Qoder).is_err());
    assert!(contains_credential(
        b"synthetic-trae-secret",
        &raw,
        ClaimPlatform::TraeCn
    ));
    assert!(request_headers(&raw, ClaimPlatform::TraeCn).unwrap()["authorization"].is_sensitive());
    for source in [0, 3] {
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["request_source"] = json!(source);
        assert!(SessionCredential::parse(&value.to_string(), ClaimPlatform::TraeCn).is_err());
    }
    let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    value["headers"]["cosy-machinetoken"] = json!("synthetic-secret");
    assert!(SessionCredential::parse(&value.to_string(), ClaimPlatform::TraeCn).is_err());
}
#[test]
fn disallowed_headers_and_crlf_are_rejected_without_secret_echo() {
    for (key, value) in [
        ("cookie", "private-cookie"),
        ("host", "evil.invalid"),
        ("authorization", "Bearer private\r\nInjected: yes"),
    ] {
        let mut v: serde_json::Value = serde_json::from_str(&raw()).unwrap();
        v["headers"][key] = json!(value);
        let error = SessionCredential::parse(&v.to_string(), ClaimPlatform::QoderCn)
            .err()
            .unwrap();
        assert!(!error.contains(value));
    }
    assert!(SessionCredential::parse(&" ".repeat(32769), ClaimPlatform::QoderCn).is_err());
}
#[test]
fn reflected_secrets_are_rejected_but_public_os_is_not() {
    let r = raw();
    assert!(contains_credential(
        b"synthetic-session-secret",
        &r,
        ClaimPlatform::QoderCn
    ));
    assert!(contains_credential(
        b"synthetic-device-secret",
        &r,
        ClaimPlatform::QoderCn
    ));
    assert!(!contains_credential(b"Windows", &r, ClaimPlatform::QoderCn));
    assert!(contains_credential(
        b"anything",
        "{invalid",
        ClaimPlatform::QoderCn
    ));
}
#[test]
fn import_channel_requires_exactly_one_bounded_platform_payload() {
    let line = format!("LLMGW_SESSION:{}", STANDARD.encode(raw()));
    assert!(decode_channel(&line, ClaimPlatform::QoderCn).is_ok());
    assert!(decode_channel(&line, ClaimPlatform::Qoder).is_err());
    assert!(decode_channel(&format!("{line}\n{line}"), ClaimPlatform::QoderCn).is_err());
    assert!(decode_channel("LLMGW_SESSION:invalid", ClaimPlatform::QoderCn).is_err());
}

#[test]
fn native_claim_decoder_requires_identity_transition_and_one_mutation() {
    use llm_gateway_lib::benefit_login::decode_native_outcome;
    let value = json!({"campaignId":"daily_2026-10-14","campaignKey":"trae_daily_checkin","status":"CLAIMED","replayed":false,"benefit":{"amount":100,"kind":"CREDITS"},"native_claim_calls":1,"before_checked_in":false,"after_checked_in":true,"native_http_status":200,"native_code":0,"identity_verified":true,"request_source":1});
    let output = |v: &serde_json::Value| format!("LLMGW_TRAE_CLAIM:{v}");
    assert!(decode_native_outcome(&output(&value), "daily_2026-10-14", 1).is_ok());
    for (field, bad) in [
        ("identity_verified", json!(false)),
        ("request_source", json!(2)),
        ("native_claim_calls", json!(2)),
        ("before_checked_in", json!(true)),
        ("after_checked_in", json!(false)),
        ("native_code", json!(9074)),
        ("native_http_status", json!(401)),
    ] {
        let mut changed = value.clone();
        changed[field] = bad;
        assert!(decode_native_outcome(&output(&changed), "daily_2026-10-14", 1).is_err());
    }
    assert!(decode_native_outcome(&output(&value), "daily_2026-10-15", 1).is_err());
    assert!(decode_native_outcome(
        &format!("{}\n{}", output(&value), output(&value)),
        "daily_2026-10-14",
        1
    )
    .is_err());
    let mut replay = value.clone();
    replay["replayed"] = json!(true);
    replay["native_claim_calls"] = json!(0);
    replay["before_checked_in"] = json!(true);
    assert!(decode_native_outcome(&output(&replay), "daily_2026-10-14", 1).is_ok());
    let mut absent_code = value;
    absent_code["native_code"] = serde_json::Value::Null;
    assert!(decode_native_outcome(&output(&absent_code), "daily_2026-10-14", 1).is_ok());
}
