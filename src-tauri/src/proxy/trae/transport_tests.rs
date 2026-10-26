use super::*;
use crate::proxy::trae::profile::SourceFunction;
use crate::{crypto, db::Db, trae_auth::TraeAuth};

async fn authority() -> ModelAuthorization {
    authority_with_machine(None).await
}
async fn authority_with_machine(machine: Option<&str>) -> ModelAuthorization {
    use base64::Engine;
    std::env::set_var(
        "LLMGW_MASTER_KEY",
        base64::engine::general_purpose::STANDARD.encode([73; 32]),
    );
    let db = Db::connect_in_memory().await.unwrap();
    let auth = TraeAuth::new(db.pool().clone(), None).unwrap();
    let binding = auth
        .seed_native_session_for_test(
            "fixture-access-secret",
            chrono::Utc::now().timestamp_millis() + 120_000,
        )
        .await;
    if let Some(machine) = machine {
        let name = "account-login:trae:cn:session:native-fixture";
        let saved = crate::db::repo::get_secret(db.pool(), name)
            .await
            .unwrap()
            .unwrap();
        let mut value: Value = serde_json::from_str(&crypto::decrypt(&saved).unwrap()).unwrap();
        value["device"]["machine_id"] = json!(machine);
        crate::db::repo::set_secret(
            db.pool(),
            name,
            &crypto::encrypt(&value.to_string()).unwrap(),
        )
        .await
        .unwrap();
    }
    let captured = auth.prepare_model_authorization(&binding).await.unwrap();
    // Authority retains its credential snapshot; these redactor tests perform no HTTP.
    assert!(crypto::encrypt("owned fixture").is_ok());
    captured
}
fn reassembled_channels(events: &[TraeEvent]) -> std::collections::BTreeMap<String, String> {
    let mut channels = std::collections::BTreeMap::<String, String>::new();
    for event in events {
        match event {
            TraeEvent::Output(UpstreamEvent::Delta(value)) => channels
                .entry("content".into())
                .or_default()
                .push_str(value),
            TraeEvent::Output(UpstreamEvent::ReasoningDelta(value)) => channels
                .entry("reasoning".into())
                .or_default()
                .push_str(value),
            TraeEvent::Output(UpstreamEvent::RefusalDelta(value)) => channels
                .entry("refusal".into())
                .or_default()
                .push_str(value),
            TraeEvent::Output(UpstreamEvent::ToolCalls(value)) => {
                for call in value.as_array().unwrap() {
                    channels
                        .entry(format!("tool:{}", call["index"].as_u64().unwrap()))
                        .or_default()
                        .push_str(call["function"]["arguments"].as_str().unwrap_or(""));
                }
            }
            _ => (),
        }
    }
    channels
}
async fn channel_regression(sequence: Vec<UpstreamEvent>) {
    let authority = authority().await;
    let mut guard = SensitiveOutput::default();
    let mut published = Vec::new();
    let mut blocked = false;
    for event in sequence {
        match guard.push(TraeEvent::Output(event), &authority) {
            Ok(events) => published.extend(events),
            Err(_) => {
                blocked = true;
                break;
            }
        }
        // Model the real client's independent accumulation of each field/index.
        assert!(
            reassembled_channels(&published)
                .values()
                .all(|text| !text.contains("fixture-access-secret")),
            "private credential reached a reconstructed logical channel"
        );
    }
    assert!(
        blocked,
        "a complete per-channel credential must fail closed"
    );
    assert!(reassembled_channels(&published)
        .values()
        .all(|text| !text.contains("fixture-access-secret")));
    assert!(guard.pending.is_empty());
    assert!(guard.finish().is_err());
}
#[tokio::test]
async fn channel_regression_content_reasoning_content_cannot_publish_credential() {
    channel_regression(vec![
        UpstreamEvent::Delta("fixture-".into()),
        UpstreamEvent::ReasoningDelta("ordinary".into()),
        UpstreamEvent::Delta("access-secret".into()),
    ])
    .await;
}
#[tokio::test]
async fn channel_regression_reasoning_content_reasoning_cannot_publish_credential() {
    channel_regression(vec![
        UpstreamEvent::ReasoningDelta("fixture-".into()),
        UpstreamEvent::Delta("ordinary".into()),
        UpstreamEvent::ReasoningDelta("access-secret".into()),
    ])
    .await;
}
#[tokio::test]
async fn channel_regression_interleaved_tool_indices_cannot_publish_credential() {
    channel_regression(vec![
        UpstreamEvent::ToolCalls(
            json!([{"index":0,"id":"call-a","function":{"name":"f","arguments":"fixture-"}}]),
        ),
        UpstreamEvent::ToolCalls(
            json!([{"index":1,"id":"call-b","function":{"name":"g","arguments":"ordinary"}}]),
        ),
        UpstreamEvent::ToolCalls(json!([{"index":0,"function":{"arguments":"access-secret"}}])),
    ])
    .await;
}
fn text(events: &[TraeEvent]) -> String {
    events
        .iter()
        .filter_map(|v| {
            if let TraeEvent::Output(v) = v {
                Some(event_text(v))
            } else {
                None
            }
        })
        .collect()
}
#[tokio::test]
async fn ordinary_output_has_no_unconditional_secret_tail() {
    let authority = authority().await;
    let mut guard = SensitiveOutput::default();
    let output = guard
        .push(
            TraeEvent::Output(UpstreamEvent::Delta("ordinary UTF8 中 text".into())),
            &authority,
        )
        .unwrap();
    assert_eq!(text(&output), "ordinary UTF8 中 text");
    assert!(guard.pending.is_empty());
}
#[tokio::test]
async fn credential_split_across_logical_events_is_never_released() {
    let authority = authority().await;
    for split in 1.."fixture-access-secret".len() {
        let mut guard = SensitiveOutput::default();
        let first = format!("visible {}", &"fixture-access-secret"[..split]);
        let output = guard
            .push(TraeEvent::Output(UpstreamEvent::Delta(first)), &authority)
            .unwrap();
        assert_eq!(text(&output), "visible ");
        assert!(guard
            .push(
                TraeEvent::Output(UpstreamEvent::Delta(
                    "fixture-access-secret"[split..].into()
                )),
                &authority
            )
            .is_err());
    }
}
#[tokio::test]
async fn prefix_protection_spans_reasoning_and_tool_argument_channels() {
    let authority = authority().await;
    let mut guard = SensitiveOutput::default();
    assert!(guard
        .push(
            TraeEvent::Output(UpstreamEvent::ReasoningDelta("fixture-access-".into())),
            &authority
        )
        .unwrap()
        .is_empty());
    assert!(guard
        .push(
            TraeEvent::Output(UpstreamEvent::ToolCalls(
                json!([{"index":0,"id":"call-a","function":{"name":"f","arguments":"secret"}}])
            )),
            &authority
        )
        .is_err());
}
#[tokio::test]
async fn incomplete_prefix_flushes_only_at_valid_finish_and_preserves_utf8_order() {
    let authority = authority().await;
    let mut guard = SensitiveOutput::default();
    let mut output = guard
        .push(
            TraeEvent::Output(UpstreamEvent::Delta("中 fixture-".into())),
            &authority,
        )
        .unwrap();
    assert_eq!(text(&output), "中 ");
    output.extend(
        guard
            .push(
                TraeEvent::Output(UpstreamEvent::ReasoningDelta("ordinary".into())),
                &authority,
            )
            .unwrap(),
    );
    assert_eq!(text(&output), "中 ");
    output.extend(
        guard
            .push(
                TraeEvent::Output(UpstreamEvent::Delta("-safe".into())),
                &authority,
            )
            .unwrap(),
    );
    assert_eq!(text(&output), "中 fixture-ordinary-safe");
    assert!(guard
        .push(
            TraeEvent::Output(UpstreamEvent::Delta("fixture-".into())),
            &authority
        )
        .unwrap()
        .is_empty());
    assert_eq!(text(&guard.finish().unwrap()), "fixture-");
}
#[tokio::test]
async fn unicode_machine_prefix_is_held_across_independent_channels() {
    let authority = authority_with_machine(Some("机器-密钥")).await;
    let mut guard = SensitiveOutput::default();
    let first = guard
        .push(
            TraeEvent::Output(UpstreamEvent::Delta("可见 机器-".into())),
            &authority,
        )
        .unwrap();
    assert_eq!(text(&first), "可见 ");
    assert!(guard
        .push(
            TraeEvent::Output(UpstreamEvent::ReasoningDelta("普通推理".into())),
            &authority
        )
        .unwrap()
        .is_empty());
    assert!(guard
        .push(
            TraeEvent::Output(UpstreamEvent::Delta("密钥".into())),
            &authority
        )
        .is_err());
    assert!(guard.finish().is_err());
    assert!(guard.pending.is_empty());
}
#[tokio::test]
async fn ordinary_unicode_content_reasoning_refusal_and_tool_channels_stream_in_order() {
    let authority = authority().await;
    let mut guard = SensitiveOutput::default();
    let sequence = vec![
        UpstreamEvent::Delta("普通内容甲".into()),
        UpstreamEvent::ReasoningDelta("普通推理乙".into()),
        UpstreamEvent::RefusalDelta("普通拒绝丙".into()),
        UpstreamEvent::ToolCalls(
            json!([{"index":0,"id":"call-a","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"中文\"}"}}]),
        ),
    ];
    let mut outputs = Vec::new();
    for event in &sequence {
        let result = guard
            .push(TraeEvent::Output(event.clone()), &authority)
            .unwrap();
        assert_eq!(result.len(), 1);
        outputs.extend(result);
        assert!(guard.pending.is_empty());
    }
    assert_eq!(
        reassembled_channels(&outputs),
        std::collections::BTreeMap::from([
            ("content".into(), "普通内容甲".into()),
            ("reasoning".into(), "普通推理乙".into()),
            ("refusal".into(), "普通拒绝丙".into()),
            ("tool:0".into(), "{\"q\":\"中文\"}".into())
        ])
    );
    assert_eq!(
        text(&outputs),
        "普通内容甲普通推理乙普通拒绝丙{\"q\":\"中文\"}"
    );
    assert!(guard.finish().unwrap().is_empty());
}
#[tokio::test]
async fn false_tool_prefix_releases_before_eof_and_preserves_identity_once_and_order() {
    let authority = authority().await;
    let mut guard = SensitiveOutput::default();
    let mut output=guard.push(TraeEvent::Output(UpstreamEvent::ToolCalls(json!([{"index":0,"id":"call-a","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"fixture-"}}]))),&authority).unwrap();
    assert_eq!(text(&output), "{\"q\":\"");
    assert!(guard.push(TraeEvent::Output(UpstreamEvent::ToolCalls(json!([{"index":1,"id":"call-b","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"中文\"}"}}]))),&authority).unwrap().is_empty());
    output.extend(
        guard
            .push(
                TraeEvent::Output(UpstreamEvent::ToolCalls(
                    json!([{"index":0,"function":{"arguments":"safe\"}"}}]),
                )),
                &authority,
            )
            .unwrap(),
    );
    assert!(guard.pending.is_empty());
    let channels = reassembled_channels(&output);
    assert_eq!(channels["tool:0"], "{\"q\":\"fixture-safe\"}");
    assert_eq!(channels["tool:1"], "{\"q\":\"中文\"}");
    let calls: Vec<_> = output
        .iter()
        .filter_map(|v| {
            if let TraeEvent::Output(UpstreamEvent::ToolCalls(v)) = v {
                Some(&v[0])
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        calls
            .iter()
            .map(|v| v["index"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 0, 1, 0]
    );
    assert_eq!(
        calls
            .iter()
            .filter(|v| v["id"].as_str() == Some("call-a"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|v| v["function"]["name"].as_str() == Some("lookup"))
            .count(),
        2
    );
    assert!(guard.finish().unwrap().is_empty());
}
#[tokio::test]
async fn pending_prefix_preserves_usage_order_and_flushes_only_at_valid_finish() {
    let authority = authority().await;
    let mut guard = SensitiveOutput::default();
    assert!(guard
        .push(
            TraeEvent::Output(UpstreamEvent::Delta("fixture-".into())),
            &authority
        )
        .unwrap()
        .is_empty());
    let report = TraeUsageReport {
        state: super::super::sse::TraeUsageState::Partial,
        prompt_tokens: Some(3),
        ..Default::default()
    };
    assert!(guard
        .push(TraeEvent::Usage(report.clone()), &authority)
        .unwrap()
        .is_empty());
    let output = guard.finish().unwrap();
    assert_eq!(output.len(), 2);
    assert!(matches!(&output[0],TraeEvent::Output(UpstreamEvent::Delta(v)) if v=="fixture-"));
    assert!(matches!(&output[1],TraeEvent::Usage(v) if *v==report));
    assert!(guard.finish().is_err());
}
#[tokio::test]
async fn pending_queue_byte_event_and_tool_index_bounds_fail_closed() {
    let authority = authority().await;
    let mut guard = SensitiveOutput::default();
    assert!(guard
        .push(
            TraeEvent::Output(UpstreamEvent::Finish {
                finish_reason: Some("unexpected metadata".into())
            }),
            &authority
        )
        .is_err());
    assert!(guard.pending.is_empty());
    assert!(guard.finish().is_err());
    let mut guard = SensitiveOutput::default();
    guard
        .push(
            TraeEvent::Output(UpstreamEvent::Delta("fixture-".into())),
            &authority,
        )
        .unwrap();
    assert_eq!(
        guard
            .push(
                TraeEvent::Output(UpstreamEvent::ReasoningDelta(
                    "q".repeat(PENDING_BYTES_LIMIT)
                )),
                &authority
            )
            .unwrap_err(),
        "native_sensitive_pending_limit"
    );
    assert!(guard.pending.is_empty());
    assert!(guard.finish().is_err());
    let mut guard = SensitiveOutput::default();
    guard
        .push(
            TraeEvent::Output(UpstreamEvent::Delta("fixture-".into())),
            &authority,
        )
        .unwrap();
    for _ in 1..PENDING_EVENTS_LIMIT {
        assert!(guard
            .push(TraeEvent::Output(UpstreamEvent::Ping), &authority)
            .unwrap()
            .is_empty());
    }
    assert_eq!(
        guard
            .push(TraeEvent::Output(UpstreamEvent::Ping), &authority)
            .unwrap_err(),
        "native_sensitive_pending_limit"
    );
    assert!(guard.pending.is_empty());
    let mut guard = SensitiveOutput::default();
    assert!(guard
        .push(
            TraeEvent::Output(UpstreamEvent::ToolCalls(
                json!([{"index":64,"function":{"arguments":"safe"}}])
            )),
            &authority
        )
        .is_err());
    assert!(guard.pending.is_empty());
}
#[test]
fn model_origin_is_fixed_and_redirect_query_credentials_are_rejected() {
    assert_eq!(
        profile::model_url(None).unwrap().as_str(),
        format!("{}{}", profile::ORIGIN, profile::MODEL_PATH)
    );
    for url in [
        "https://evil.invalid/api/agent/v3/llm_utils_chat",
        "https://trae-api-cn.mchost.guru/api/agent/v3/llm_utils_chat?x=1",
        "https://trae-api-cn.mchost.guru/api/ide/v1/get_detail_param",
    ] {
        assert!(!profile::is_model_url(&reqwest::Url::parse(url).unwrap()));
    }
}
#[tokio::test]
async fn ticket_constructor_uses_actual_catalog_uuid_and_owned_snapshot_without_expiry() {
    let authority = authority().await;
    let binding = TraeModelBinding {
        region: profile::TraeRegion::Cn,
        account_id: "native-fixture".into(),
        principal_hash: "a".repeat(64),
        profile_id: profile::TraeProfileId::CnOrdinary343Aicc20260212V1,
    };
    let view = super::super::catalog::project(
        &binding,
        SourceFunction::SoloWorkRemote,
        &json!({"config_info_list":[{"config_name":"Exact-A"}]}),
    )
    .unwrap();
    let ticket = super::super::NativeCatalogTicket {
        binding: binding.clone(),
        generation: authority.generation(),
        function: SourceFunction::SoloWorkRemote,
        revision: view.catalog_revision.clone(),
        entry: view.entries[0].clone(),
        fetched_at_ms: 1,
    };
    let selected = TraeModelSelection::from_ticket(&ticket, true);
    let options = crate::agent_upstream::runtime::TraeRuntimeOptions {
        region: "cn".into(),
        account_id: binding.account_id,
        principal_hash: binding.principal_hash,
        profile_id: binding.profile_id.as_str().into(),
        model_selections: std::collections::BTreeMap::from([("native-exact".into(), selected)]),
    };
    let value = serde_json::to_value(options).unwrap();
    assert!(crate::agent_upstream::runtime::trae_options(Some(&value)).is_ok());
    assert_eq!(
        value["model_selections"]["native-exact"]["catalog_revision"],
        view.catalog_revision
    );
    assert_eq!(
        value["model_selections"]["native-exact"]["fetched_at_ms"],
        1
    );
}
