//! Expand explicit physical variants. No first/default/suffix selection.
use super::*;
use crate::proxy::trae::profile::RawModelIdentity;

pub(crate) fn selector(
    profile: TraeProfileId,
    function: SourceFunction,
    config: &str,
    raw: &RawModelIdentity,
) -> String {
    let mut hash = Sha256::new();
    for part in [
        profile.as_str(),
        function.as_str(),
        config,
        &raw.model_name,
        &raw.config_source.to_string(),
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
pub(super) fn project(
    binding: &TraeModelBinding,
    function: SourceFunction,
    packet: &Value,
) -> Result<TraeNativeCatalogView, String> {
    if function != binding.profile_id.function() {
        return Err("native_catalog_function_mismatch".into());
    }
    let mut envelope = packet.clone();
    let list = packet
        .get("config_info_list")
        .and_then(Value::as_array)
        .ok_or(INVALID)?;
    envelope["config_info_list"] = serde_json::json!([]);
    let mut out = project_inner(binding, function, &envelope).map_err(|e| e.into_public(packet))?;
    if list.len() > MAX_ENTRIES {
        return Err("native_catalog_entry_limit".into());
    }
    let mut seen = HashMap::new();
    // Closed reason categories only: never publish rejected names, connection
    // fields or raw details. Keep the same exclusion/acceptance predicates.
    let mut excluded_reasons = [false; 4];
    for value in list {
        let object = value.as_object().ok_or(INVALID)?;
        if object.get("config_source").and_then(Value::as_u64) != Some(1) {
            excluded_reasons[0] = true;
            out.excluded_count += 1;
            continue;
        }
        if custom(object) {
            excluded_reasons[1] = true;
            out.excluded_count += 1;
            continue;
        }
        if object
            .get("function")
            .is_some_and(|v| v.as_str() != Some(function.as_str()))
        {
            return Err("native_catalog_function_mismatch".into());
        }
        let details = object
            .get("model_detail_list")
            .and_then(Value::as_array)
            .ok_or(INVALID)?;
        if details.len() > MAX_ENTRIES {
            return Err("native_catalog_entry_limit".into());
        }
        if details.is_empty() {
            excluded_reasons[2] = true;
            out.excluded_count += 1;
        }
        for detail in details {
            let model = detail
                .get("model_name")
                .and_then(Value::as_str)
                .ok_or("native_catalog_physical_model_missing")?;
            let raw = RawModelIdentity {
                model_name: model.into(),
                config_source: 1,
            };
            if !raw.valid() {
                return Err("native_catalog_physical_model_invalid".into());
            }
            let mut entry = value.clone();
            entry["model_detail_list"] = serde_json::json!([detail]);
            let mut view = match project_entry(entry.as_object().ok_or(INVALID)?, binding, function)
                .map_err(|e| e.into_public(packet))?
            {
                Some(v) => v,
                None => {
                    excluded_reasons[3] = true;
                    out.excluded_count += 1;
                    continue;
                }
            };
            view.selector_id = selector(binding.profile_id, function, &view.config_name, &raw);
            view.raw_model = Some(raw);
            if let Some(prior) = seen.get(&view.selector_id) {
                if prior != &view {
                    return Err("native_catalog_selector_conflict".into());
                }
                continue;
            }
            if out.entries.len() >= MAX_ENTRIES {
                return Err("native_catalog_entry_limit".into());
            }
            seen.insert(view.selector_id.clone(), view.clone());
            out.entries.push(view);
        }
    }
    if out.excluded_count > 0 {
        out.warnings
            .push("native_catalog_custom_or_unresolved_physical_model_excluded");
        for (present, code) in excluded_reasons.into_iter().zip([
            "native_catalog_source_unverified_excluded",
            "native_catalog_custom_connection_excluded",
            "native_catalog_empty_physical_models_excluded",
            "native_catalog_detail_or_reasoning_excluded",
        ]) {
            if present {
                out.warnings.push(code);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod exclusion_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn web_exclusions_report_fixed_reasons_without_projecting_excluded_metadata() {
        for profile in [TraeProfileId::CnWebRawV1, TraeProfileId::GlobalWebRawSgV1] {
            let binding = TraeModelBinding {
                region: profile.region(),
                account_id: "owned-exclusion-fixture".into(),
                profile_id: profile,
                principal_hash: "a".repeat(64),
            };
            let good = json!({"config_name":"Exact-owned","config_source":1,
                "model_detail_list":[{"model_name":"Owned-physical__dev"}]});
            let source = json!({"config_source":2,"private_name":"EXCLUDED_NAME"});
            let custom = json!({"config_source":1,"custom":true,"ak":"EXCLUDED_KEY"});
            let empty =
                json!({"config_name":"EXCLUDED_EMPTY","config_source":1,"model_detail_list":[]});
            let detail = json!({"config_name":"EXCLUDED_DETAIL","config_source":1,
                "model_detail_list":[{"model_name":"EXCLUDED_PHYSICAL","custom":true}]});
            let packet = json!({"config_info_list":[good,source,source,custom,empty,detail]});
            let view = project(&binding, SourceFunction::ChatV3, &packet).expect(
                "baseline must reach the missing reason projection, not fail fixture parsing",
            );
            assert_eq!(view.entries.len(), 1);
            assert_eq!(view.entries[0].config_name, "Exact-owned");
            assert_eq!(view.entries[0].account_model_evidence, "unverified");
            assert_eq!(view.excluded_count, 5);
            assert_eq!(
                view.warnings,
                vec![
                    "native_catalog_custom_or_unresolved_physical_model_excluded",
                    "native_catalog_source_unverified_excluded",
                    "native_catalog_custom_connection_excluded",
                    "native_catalog_empty_physical_models_excluded",
                    "native_catalog_detail_or_reasoning_excluded",
                ]
            );
            assert!(!serde_json::to_string(&view).unwrap().contains("EXCLUDED_"));
        }
    }
}
