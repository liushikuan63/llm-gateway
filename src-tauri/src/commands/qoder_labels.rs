//! Saved account-model labels are presentation metadata, never API aliases or wire IDs.
use super::*;
use crate::domain::AgentRuntime;
use std::collections::BTreeMap;

pub(super) fn labels(provider: &Provider, runtime: &AgentRuntime) -> BTreeMap<String, String> {
    if runtime.kind == "trae-model" {
        return trae_labels(provider, runtime);
    }
    if runtime.kind != "qoder-model"
        || provider.runtime_id.as_deref() != Some(runtime.id.as_str())
        || !provider.api_key_enc.is_empty()
    {
        return BTreeMap::new();
    }
    let Ok((region, _, aliases)) =
        crate::agent_upstream::runtime::qoder_options(runtime.options.as_ref())
    else {
        return BTreeMap::new();
    };
    if provider.base_url != region.model_origin() {
        return BTreeMap::new();
    }
    let names = crate::agent_upstream::runtime::model_display_names(runtime.options.as_ref());
    // A verified local metadata fallback for already-registered accounts. A freshly
    // discovered display_name always wins; never infer families from opaque IDs.
    let fallback: serde_json::Value =
        serde_json::from_str(include_str!("qoder-labels-20261018.json")).unwrap_or_default();
    let region_key = if region == crate::qoder_auth::Region::Cn {
        "cn"
    } else {
        "global"
    };
    provider
        .models
        .iter()
        .filter_map(|model| {
            let wire_id = aliases.get(&model.upstream).unwrap_or(&model.upstream);
            names
                .get(wire_id)
                .map(String::as_str)
                .or_else(|| fallback[region_key][wire_id].as_str())
                .map(|name| (model.alias.clone(), name.to_owned()))
        })
        .collect()
}

fn trae_labels(provider: &Provider, runtime: &AgentRuntime) -> BTreeMap<String, String> {
    if provider.runtime_id.as_deref() != Some(runtime.id.as_str())
        || !provider.api_key_enc.is_empty()
        || provider.dialect != Dialect::OpenAI
    {
        return BTreeMap::new();
    }
    let Ok(options) = crate::agent_upstream::runtime::trae_options(runtime.options.as_ref()) else {
        return BTreeMap::new();
    };
    if provider.base_url != options.binding().profile_id.origin() {
        return BTreeMap::new();
    }
    // Read the already saved selector/config binding; never guess a model family
    // from an opaque alias or turn a display label into deployment/free evidence.
    provider
        .models
        .iter()
        .filter_map(|model| {
            let selected = options.model_selections.get(&model.alias)?;
            (model.upstream == selected.config_name
                && !selected.display_name.trim().is_empty()
                && selected.display_name.encode_utf16().count() <= 256)
                .then(|| (model.alias.clone(), selected.display_name.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_trae_fixture(
        profile: crate::proxy::trae::profile::TraeProfileId,
    ) -> (Provider, AgentRuntime) {
        use crate::agent_upstream::runtime::{
            TraeModelSelection, TraeProtocolCarriers, TraeRuntimeOptions,
        };
        use crate::proxy::trae::profile::{RawModelIdentity, SourceFunction, TraeRegion};

        let alias = "owned-trae-alias";
        let region = match profile.region() {
            TraeRegion::Cn => "cn",
            TraeRegion::Global => "global",
        };
        let raw = RawModelIdentity {
            model_name: "owned-physical__dev".into(),
            config_source: 1,
        };
        let selected = TraeModelSelection {
            selector_id: crate::proxy::trae::catalog::web::selector(
                profile,
                SourceFunction::ChatV3,
                "owned-config",
                &raw,
            ),
            raw_model: Some(raw),
            source_function: "chat_v3".into(),
            config_name: "owned-config".into(),
            display_name: "Owned catalog name".into(),
            prompt_max_tokens: Some(4096),
            max_tokens: Some(128),
            declared_vision: None,
            declared_tools: None,
            declared_thinking: None,
            reasoning_effort_options: Vec::new(),
            protocol_carriers: TraeProtocolCarriers {
                text: true,
                tools: true,
                images: true,
                audio: false,
                video: false,
            },
            account_model_evidence: "unverified".into(),
            catalog_revision: uuid::Uuid::new_v4().to_string(),
            fetched_at_ms: 1_790_000_000_000,
            allow_undeclared_images: false,
        };
        let options = TraeRuntimeOptions {
            region: region.into(),
            account_id: "owned-account".into(),
            principal_hash: "a".repeat(64),
            profile_id: profile.as_str().into(),
            model_selections: BTreeMap::from([(alias.into(), selected)]),
        };
        let mut runtime = AgentRuntime::new(
            format!("owned-trae-{region}-runtime"),
            "trae-model",
            "owned",
        );
        runtime.options = Some(serde_json::to_value(options).unwrap());
        // The red must reach the missing presentation consumer, not an invalid fixture.
        crate::agent_upstream::runtime::trae_options(runtime.options.as_ref())
            .expect("owned saved metadata must satisfy the production runtime contract");
        let provider = Provider {
            id: format!("owned-trae-{region}"),
            name: format!("Owned Trae {region}"),
            dialect: Dialect::OpenAI,
            base_url: profile.origin().into(),
            api_key_enc: String::new(),
            enabled: true,
            priority: 1,
            rpm_limit: 0,
            intelligence: 50,
            note: None,
            runtime_id: Some(runtime.id.clone()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            models: vec![serde_json::from_value(serde_json::json!({
                "alias":alias,"upstream":"owned-config","context_window":4096,
                "supports_tools":false,"supports_vision":false,"supports_stream":true
            }))
            .unwrap()],
        };
        (provider, runtime)
    }

    #[test]
    fn trae_saved_names_reach_existing_provider_view_without_rewriting_targets() {
        use crate::proxy::trae::profile::TraeProfileId;
        for profile in [TraeProfileId::CnWebRawV1, TraeProfileId::GlobalWebRawSgV1] {
            let (provider, runtime) = saved_trae_fixture(profile);
            let before_models = serde_json::to_value(&provider.models).unwrap();
            let before_options = runtime.options.clone();
            assert_eq!(
                labels(&provider, &runtime)
                    .get("owned-trae-alias")
                    .map(String::as_str),
                Some("Owned catalog name")
            );
            assert_eq!(
                serde_json::to_value(&provider.models).unwrap(),
                before_models
            );
            assert_eq!(runtime.options, before_options);
            // Export only artificial metadata from the actual production getter
            // for the finite browser contract, never real account/catalog data.
            let mut view = ProviderView::from(provider.clone(), None, None);
            view.model_display_names = labels(&provider, &runtime);
            println!(
                "TRAE_LABEL_PROVIDER_VIEW:{}",
                serde_json::to_string(&view).unwrap()
            );
        }
    }

    #[test]
    fn trae_display_names_require_exact_saved_binding_and_valid_metadata() {
        use crate::proxy::trae::profile::TraeProfileId;
        for case in 0..14 {
            let (mut provider, mut runtime) = saved_trae_fixture(TraeProfileId::CnWebRawV1);
            match case {
                0 => provider.runtime_id = Some("other-owned-runtime".into()),
                1 => provider.base_url = "https://example.invalid".into(),
                2 => provider.api_key_enc = "owned-unrelated-key".into(),
                3 => provider.dialect = Dialect::Anthropic,
                4 => provider.models[0].upstream = "different-config".into(),
                5 => provider.models[0].alias = "different-alias".into(),
                6 => runtime.kind = "codex".into(),
                case => {
                    let options = runtime.options.as_mut().unwrap();
                    match case {
                        7 => options["region"] = serde_json::json!("global"),
                        8 => options["principal_hash"] = serde_json::json!("not-a-principal"),
                        9 => {
                            options["model_selections"]["owned-trae-alias"]["display_name"] =
                                serde_json::json!("unsafe\nlabel");
                        }
                        10 => {
                            options["model_selections"]["owned-trae-alias"]["raw_model"]
                                ["model_name"] = serde_json::json!("different-physical__dev");
                        }
                        11 => {
                            options["model_selections"]["owned-trae-alias"]["source_function"] =
                                serde_json::json!("different-source");
                        }
                        12 => {
                            options["model_selections"]["owned-trae-alias"]["display_name"] =
                                serde_json::json!("   ");
                        }
                        13 => {
                            options["model_selections"]["owned-trae-alias"]["display_name"] =
                                serde_json::json!("🦀".repeat(129));
                        }
                        _ => unreachable!(),
                    }
                    if matches!(case, 12 | 13) {
                        crate::agent_upstream::runtime::trae_options(Some(options)).unwrap();
                    }
                }
            }
            assert!(labels(&provider, &runtime).is_empty(), "case {case}");
        }
    }

    #[test]
    fn official_names_are_region_bound_and_do_not_change_model_ids() {
        for (region, origin, expected) in [
            ("global", "https://api2.qoder.sh", "MiniMax-M3"),
            ("cn", "https://gateway.qoder.com.cn", "MiniMax-M2.7"),
        ] {
            let mut runtime = AgentRuntime::new("qoder-label-fixture", "qoder-model", "fixture");
            runtime.options = Some(serde_json::json!({"region":region,"account_id":"fixture"}));
            let mut provider=Provider {id:"fixture".into(),name:"fixture".into(),dialect:Dialect::OpenAI,base_url:origin.into(),api_key_enc:String::new(),enabled:true,priority:1,rpm_limit:0,intelligence:50,note:None,runtime_id:Some(runtime.id.clone()),created_at:chrono::Utc::now(),updated_at:chrono::Utc::now(),models:vec![serde_json::from_value(serde_json::json!({"alias":"mmodel","upstream":"mmodel","context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true})).unwrap()]};
            let before = serde_json::to_value(&provider.models).unwrap();
            assert_eq!(
                labels(&provider, &runtime)
                    .get("mmodel")
                    .map(String::as_str),
                Some(expected)
            );
            assert_eq!(serde_json::to_value(&provider.models).unwrap(), before);
            provider.base_url = "https://example.invalid".into();
            assert!(labels(&provider, &runtime).is_empty());
        }
    }
    #[test]
    fn refreshed_names_and_model_aliases_win_without_guessing_unknown_models() {
        let runtime = AgentRuntime {
            options: Some(
                serde_json::json!({"region":"cn","account_id":"fixture","model_aliases":{"source":"dfmodel"},"model_display_names":{"dfmodel":"Fresh official label","mmodel":"forged\nlabel"}}),
            ),
            ..AgentRuntime::new("runtime", "qoder-model", "fixture")
        };
        let provider=Provider {id:"fixture".into(),name:"fixture".into(),dialect:Dialect::OpenAI,base_url:"https://gateway.qoder.com.cn".into(),api_key_enc:String::new(),enabled:true,priority:1,rpm_limit:0,intelligence:50,note:None,runtime_id:Some(runtime.id.clone()),created_at:chrono::Utc::now(),updated_at:chrono::Utc::now(),models:[("custom","source"),("mmodel","mmodel"),("unknown","unknown")].into_iter().map(|(alias,upstream)|serde_json::from_value(serde_json::json!({"alias":alias,"upstream":upstream,"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_stream":true})).unwrap()).collect()};
        let names = labels(&provider, &runtime);
        assert_eq!(names.get("custom").unwrap(), "Fresh official label");
        assert_eq!(names.get("mmodel").unwrap(), "MiniMax-M2.7");
        assert!(!names.contains_key("unknown"));
    }
}
