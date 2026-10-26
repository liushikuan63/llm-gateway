use super::*;
use catalog::{nesting_valid, project, selector_id};
use profile::{encode_component, TraeProfileId, TraeRegion};
use serde_json::{json, Value};
fn binding() -> TraeModelBinding {
    TraeModelBinding {
        region: TraeRegion::Cn,
        account_id: "test".into(),
        principal_hash: "a".repeat(64),
        profile_id: TraeProfileId::CnOrdinary343Aicc20260212V1,
    }
}
fn parse(list: Value) -> Result<TraeNativeCatalogView, String> {
    project(
        &binding(),
        SourceFunction::SoloWorkRemote,
        &json!({"config_info_list":list}),
    )
}
#[test]
fn empty_is_valid_but_missing_or_wrapped_envelope_is_invalid() {
    assert!(parse(json!([])).unwrap().entries.is_empty());
    for packet in [
        json!({}),
        json!({"data":{"config_info_list":[]}}),
        json!({"config_info_list":{}}),
        json!({"code":1,"config_info_list":[]}),
        json!({"code":"0","config_info_list":[]}),
        json!({"ResponseMetadata":{"Error":{}},"config_info_list":[]}),
        json!({"function":"solo_agent_remote","config_info_list":[]}),
    ] {
        assert!(project(&binding(), SourceFunction::SoloWorkRemote, &packet).is_err());
    }
}
#[test]
fn exact_selectors_preserve_config_display_and_unknown_capabilities() {
    let view = parse(json!([
        {"config_name":"custom_model_Gemini","display_config":{"display_name":"Visible"},
          "multimodal":true,"support_tools":true,"reasoning_effort_config":{"support_thinking":true,"options":["high"]}},
        {"config_name":"custom_model_gemini"}
    ]))
    .unwrap();
    assert_eq!(view.entries.len(), 2);
    let first = &view.entries[0];
    assert_eq!(first.config_name, "custom_model_Gemini");
    assert_eq!(first.display_name, "Visible");
    assert_eq!(first.declared_vision, None);
    assert_eq!(first.declared_tools, None);
    assert_eq!(first.declared_thinking, None);
    assert!(first.reasoning_effort_options.is_empty());
    assert_eq!(first.prompt_max_tokens, None);
    assert_eq!(first.account_model_evidence, "unverified");
    assert_ne!(first.selector_id, view.entries[1].selector_id);
    assert_eq!(
        first.selector_id,
        selector_id(
            binding().profile_id,
            SourceFunction::SoloWorkRemote,
            "custom_model_Gemini"
        )
    );
}
#[test]
fn duplicate_identical_dedupes_but_conflicting_exact_selector_fails() {
    assert_eq!(
        parse(json!([{"config_name":"A"},{"config_name":"A"}]))
            .unwrap()
            .entries
            .len(),
        1
    );
    assert_eq!(
        parse(json!([{"config_name":"A"},{"config_name":"A","max_tokens":10}])).unwrap_err(),
        "native_catalog_selector_conflict"
    );
}
#[test]
fn custom_and_unknown_source_are_filtered_structurally() {
    let mut entries = vec![
        json!({"config_name":"custom_model_builtin"}),
        json!({"config_name":"official","config_source":1}),
    ];
    for (i, fields) in [
        json!({"ak":"x"}),
        json!({"sk":"x"}),
        json!({"base_url":"https://invalid.test"}),
        json!({"client_connect":true}),
        json!({"auth_type":"unknown"}),
        json!({"custom":true}),
        json!({"config_source":2}),
        json!({"config_source":"1"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut entry = fields.as_object().unwrap().clone();
        entry.insert("config_name".into(), json!(format!("excluded{i}")));
        entries.push(Value::Object(entry));
    }
    let view = parse(Value::Array(entries)).unwrap();
    assert_eq!(view.entries.len(), 2);
    assert_eq!(view.excluded_count, 8);
    assert_eq!(view.warnings.len(), 1);
}
#[test]
fn all_details_must_agree_and_limits_remain_bounded() {
    let view = parse(json!([
        {"config_name":"agree","model_detail_list":[{"prompt_max_tokens":200,"max_tokens":20},{"prompt_max_tokens":200,"max_tokens":20}]},
        {"config_name":"conflict","model_detail_list":[{"max_tokens":20},{"max_tokens":30}]},
        {"config_name":"partial","model_detail_list":[{"max_tokens":20},{}]},
        {"config_name":"topconflict","max_tokens":30,"model_detail_list":[{"max_tokens":20}]}
    ])).unwrap();
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.excluded_count, 3);
    assert_eq!(view.entries[0].prompt_max_tokens, Some(200));
    assert_eq!(view.entries[0].max_tokens, Some(20));
    for value in [json!(0), json!(-1), json!(16_777_217), json!("10")] {
        assert!(parse(json!([{"config_name":"invalid","max_tokens":value}])).is_err());
    }
}
#[test]
fn entry_count_name_function_and_depth_limits_fail_closed() {
    let many: Vec<_> = (0..513)
        .map(|i| json!({"config_name":format!("A{i}")}))
        .collect();
    assert!(parse(json!(many)).is_err());
    for value in [
        json!({"config_name":""}),
        json!({"config_name":"x\n"}),
        json!({"config_name":"<script>"}),
        json!({"config_name":"x".repeat(257)}),
        json!({"config_name":"A","function":"other"}),
    ] {
        assert!(parse(json!([value])).is_err());
    }
    assert!(nesting_valid(br#"{"string":"[{}]"}"#));
    assert!(!nesting_valid(
        format!("{}0{}", "[".repeat(33), "]".repeat(33)).as_bytes()
    ));
    assert!(nesting_valid(
        format!("{}0{}", "[".repeat(32), "]".repeat(32)).as_bytes()
    ));
}
#[test]
fn component_encoding_and_test_origin_are_exact() {
    assert_eq!(
        encode_component("中 文/+!~*'()"),
        "%E4%B8%AD%20%E6%96%87%2F%2B!~*'()"
    );
    assert_eq!(
        profile::catalog_url(None).unwrap().as_str(),
        "https://trae-api-cn.mchost.guru/api/ide/v1/get_detail_param"
    );
    for value in [
        "http://localhost:1234",
        "https://127.0.0.1:1234",
        "http://127.0.0.1",
        "http://127.0.0.1:0",
        "http://user@127.0.0.1:1234",
        "http://127.0.0.1:1234/?x=1",
        "http://127.0.0.1:1234/#x",
        "http://127.0.0.1:1234/other",
    ] {
        assert!(profile::catalog_url(Some(&reqwest::Url::parse(value).unwrap())).is_err());
    }
}

#[test]
fn direct_reasoning_options_project_without_capability_claim() {
    let v=parse(json!([{"config_name":"A","reasoning_effort_options":["minimal","low","medium","high","xhigh"],"model_detail_list":[{"reasoning_effort_options":["minimal","low","medium","high","xhigh"]}]}])).unwrap();
    assert_eq!(
        v.entries[0].reasoning_effort_options,
        vec!["minimal", "low", "medium", "high", "xhigh"]
    );
    assert_eq!(v.entries[0].declared_thinking, None);
}

#[test]
fn direct_reasoning_invalid_duplicate_unknown_or_conflicting_controls_excluded() {
    for entry in [
        json!({"config_name":"A","reasoning_effort_options":["high","high"]}),
        json!({"config_name":"A","reasoning_effort_options":["auto"]}),
        json!({"config_name":"A","reasoning_effort_options":"high"}),
        json!({"config_name":"A","reasoning_effort_options":["high"],"model_detail_list":[{"reasoning_effort_options":["low"]}]}),
        json!({"config_name":"A","model_detail_list":[{"reasoning_effort_options":["high"]},{"reasoning_effort_options":["low"]}]}),
    ] {
        let v = parse(json!([entry])).unwrap();
        assert_eq!(v.entries.len(), 0);
        assert_eq!(v.excluded_count, 1);
        assert!(!v.warnings.is_empty());
    }
    let v=parse(json!([{"config_name":"A","model_detail_list":[{"reasoning_effort_options":["low","high"]}]}])).unwrap();
    assert_eq!(v.entries[0].reasoning_effort_options, vec!["low", "high"]);
}

fn duplicate_projection_base() -> Value {
    json!({"config_name":"duplicate-A","display_config":{"display_name":"Visible"},
        "prompt_max_tokens":8192,"max_tokens":2048,"reasoning_effort_options":["low","high"]})
}

#[test]
fn duplicate_projection_equal_consumed_fields_dedupe_in_both_orders() {
    let base = duplicate_projection_base();
    let single = parse(json!([base.clone()])).unwrap().entries.remove(0);
    let mut decorated = base.clone();
    decorated["opaque_catalog_annotation"] = json!({"label":"synthetic-opaque-canary","version":2});
    decorated["display_config"]["locale_hint"] = json!("synthetic-locale");
    let mut explicit_source = base.clone();
    explicit_source["config_source"] = json!(1);
    explicit_source["function"] = json!("solo_work_remote");
    explicit_source["custom"] = json!(false);
    let detail_limits = json!({"config_name":"duplicate-A","display_config":{"display_name":"Visible"},
        "model_detail_list":[{"prompt_max_tokens":8192,"max_tokens":2048,"reasoning_effort_options":["low","high"]}]});
    for variant in [decorated, explicit_source, detail_limits] {
        assert_eq!(
            parse(json!([variant.clone()])).unwrap().entries,
            vec![single.clone()]
        );
        for pair in [
            json!([base.clone(), variant.clone()]),
            json!([variant, base.clone()]),
        ] {
            let result = parse(pair).unwrap();
            assert_eq!(result.entries, vec![single.clone()]);
            assert_eq!(result.excluded_count, 0);
            assert!(result.warnings.is_empty());
        }
    }
    assert_eq!(
        parse(json!([{"config_name":"fallback"},
        {"config_name":"fallback","display_config":{"display_name":""}}]))
        .unwrap()
        .entries
        .len(),
        1
    );
}

#[test]
fn duplicate_projection_equal_entries_preserve_actual_native_wire() {
    let base = duplicate_projection_base();
    let mut decorated = base.clone();
    decorated["opaque_catalog_annotation"] = json!("synthetic-wire-canary");
    let single = parse(json!([base.clone()])).unwrap().entries.remove(0);
    let duplicate = parse(json!([decorated, base])).unwrap().entries.remove(0);
    let req = crate::domain::ChatRequest {
        model: "public-alias".into(),
        messages: vec![crate::domain::Message::user("hello")],
        temperature: None,
        top_p: None,
        max_tokens: Some(16),
        stop: None,
        stream: false,
        tools: None,
        tool_choice: None,
        thinking: None,
        extra: [("reasoning_effort".into(), json!("low"))]
            .into_iter()
            .collect(),
    };
    let one = super::request::build_native_request(&req, &single, SourceFunction::SoloWorkRemote)
        .unwrap();
    let two =
        super::request::build_native_request(&req, &duplicate, SourceFunction::SoloWorkRemote)
            .unwrap();
    assert_eq!(one.body, two.body);
    assert_eq!(one.constraints.config_name, two.constraints.config_name);
    assert!(!two.body.to_string().contains("synthetic-wire-canary"));
    assert_eq!(duplicate.declared_vision, None);
    assert_eq!(duplicate.declared_tools, None);
    assert_eq!(duplicate.declared_thinking, None);
}

#[test]
fn duplicate_projection_conflicts_invalidity_and_exclusion_still_fail() {
    let base = duplicate_projection_base();
    for fields in [
        json!({"display_config":{"display_name":"Different"}}),
        json!({"prompt_max_tokens":4096}),
        json!({"max_tokens":1024}),
        json!({"reasoning_effort_options":["high"]}),
        json!({"reasoning_effort_options":["high","low"]}),
        json!({"max_tokens":0}),
        json!({"max_tokens":"2048"}),
        json!({"reasoning_effort_options":["auto"]}),
        json!({"display_config":{"display_name":null}}),
        json!({"custom":true}),
        json!({"config_source":2}),
        json!({"model_detail_list":[{"custom":true}]}),
    ] {
        let mut changed = base.clone();
        changed
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        for pair in [
            json!([base.clone(), changed.clone()]),
            json!([changed.clone(), base.clone()]),
        ] {
            // A malformed first record retains its original shape error. A differing
            // duplicate must retain selector_conflict rather than expose its error.
            let expected = if parse(json!([changed.clone()])).is_err() && pair[0] == changed {
                "native_catalog_shape_invalid"
            } else {
                "native_catalog_selector_conflict"
            };
            assert_eq!(parse(pair).unwrap_err(), expected);
        }
    }
    assert_eq!(
        parse(json!([base,{"config_name":"duplicate-A","function":"solo_agent_remote"}]))
            .unwrap_err(),
        "native_catalog_function_mismatch"
    );
}

#[test]
fn duplicate_projection_excluded_identity_order_and_outer_bounds_are_unchanged() {
    let excluded = json!({"config_name":"excluded","custom":true});
    let result = parse(json!([excluded.clone(), excluded.clone()])).unwrap();
    assert!(result.entries.is_empty());
    assert_eq!(result.excluded_count, 1);
    let mut different = excluded.clone();
    different["opaque_annotation"] = json!(true);
    for pair in [
        json!([excluded.clone(), different.clone()]),
        json!([different, excluded]),
    ] {
        assert_eq!(parse(pair).unwrap_err(), "native_catalog_selector_conflict");
    }
    let result = parse(
        json!([{"config_name":"A"},{"config_name":"A","opaque_annotation":1},
        {"config_name":"a"},{"config_name":"a","opaque_annotation":2}]),
    )
    .unwrap();
    assert_eq!(
        result
            .entries
            .iter()
            .map(|v| v.config_name.as_str())
            .collect::<Vec<_>>(),
        vec!["A", "a"]
    );
    assert_ne!(result.entries[0].selector_id, result.entries[1].selector_id);
    assert_eq!(
        parse(Value::Array(vec![json!({"config_name":"A"}); 513])).unwrap_err(),
        "native_catalog_entry_limit"
    );
}
