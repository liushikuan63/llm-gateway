//! Bounded projection only; raw upstream JSON never becomes a registration ticket.
use super::profile::{SourceFunction, TraeModelBinding, TraeProfileId, TraeRegion};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(super) const BODY_LIMIT: usize = 1024 * 1024;
const INVALID: &str = "native_catalog_shape_invalid";
const MAX_ENTRIES: usize = 512;
const MAX_TOKENS: u64 = 16_777_216;

mod diagnostic;
pub(crate) mod web;
use diagnostic::{Class, Error, Location, SelectorConflictReason, SelectorField};

#[cfg(test)]
mod diagnostic_tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ProtocolCarriers {
    pub text: bool,
    pub tools: bool,
    pub images: bool,
    pub audio: bool,
    pub video: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct TraeNativeModelView {
    pub selector_id: String,
    pub config_name: String,
    pub display_name: String,
    pub prompt_max_tokens: Option<u32>,
    pub max_tokens: Option<u32>,
    pub declared_vision: Option<bool>,
    pub declared_tools: Option<bool>,
    pub declared_thinking: Option<bool>,
    pub reasoning_effort_options: Vec<String>,
    pub protocol_carriers: ProtocolCarriers,
    pub account_model_evidence: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_model: Option<super::profile::RawModelIdentity>,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct TraeNativeCatalogView {
    pub account_id: String,
    pub region: TraeRegion,
    pub profile_id: TraeProfileId,
    pub source_function: SourceFunction,
    pub fetched_at_ms: i64,
    pub catalog_revision: String,
    pub entries: Vec<TraeNativeModelView>,
    pub excluded_count: usize,
    pub warnings: Vec<&'static str>,
}
pub(crate) fn selector_id(
    profile: TraeProfileId,
    function: SourceFunction,
    config_name: &str,
) -> String {
    // Length-framing avoids delimiter ambiguities and retains exact Unicode/case.
    let mut hash = Sha256::new();
    for part in [profile.as_str(), function.as_str(), config_name] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
pub(super) fn nesting_valid(bytes: &[u8]) -> bool {
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 32 {
                        return false;
                    }
                }
                b'}' | b']' => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                _ => (),
            }
        }
    }
    depth == 0 && !quoted
}
fn name(raw: Option<&Value>, location: Location) -> Result<&str, Error> {
    let value = raw
        .and_then(Value::as_str)
        .ok_or_else(|| Error::wrong_type(location, raw))?;
    let class = if value.is_empty() {
        Some(Class::Empty)
    } else if value.chars().count() > 256 {
        Some(Class::Oversize)
    } else if value.chars().any(char::is_control) {
        Some(Class::Control)
    } else if value.contains(['<', '>']) {
        Some(Class::Unsafe)
    } else {
        None
    };
    match class {
        Some(class) => Err(Error::shape(location, raw, class)),
        None => Ok(value),
    }
}
fn custom(raw: &Map<String, Value>) -> bool {
    ["ak", "sk", "base_url", "auth_type"].iter().any(|key| {
        raw.get(*key)
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
    }) || raw
        .get("client_connect")
        .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
        || raw
            .get("config_source")
            .is_some_and(|v| v.as_i64() != Some(1))
        || ["custom", "is_custom", "is_custom_model", "custom_model"]
            .iter()
            .any(|key| {
                raw.get(*key)
                    .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
            })
}
fn limit(raw: &Map<String, Value>, key: &str, location: Location) -> Result<Option<u32>, Error> {
    raw.get(key)
        .filter(|v| !v.is_null())
        .map(|value| {
            value
                .as_u64()
                .filter(|v| (1..=MAX_TOKENS).contains(v))
                .map(|v| v as u32)
                .ok_or_else(|| {
                    let class = if value.as_u64() == Some(0) {
                        Class::Zero
                    } else if value.as_u64().is_some() {
                        Class::OverLimit
                    } else if value.as_i64().is_some_and(|v| v < 0) {
                        Class::Negative
                    } else {
                        Class::NotUint
                    };
                    Error::shape(location, Some(value), class)
                })
        })
        .transpose()
}
fn limits(raw: &Map<String, Value>, detail: bool) -> Result<(Option<u32>, Option<u32>), Error> {
    let (prompt, output) = if detail {
        (Location::DetailPromptMaxTokens, Location::DetailMaxTokens)
    } else {
        (Location::EntryPromptMaxTokens, Location::EntryMaxTokens)
    };
    Ok((
        limit(raw, "prompt_max_tokens", prompt)?,
        limit(raw, "max_tokens", output)?,
    ))
}
// Only the fixed reference's direct options field is established. Unknown,
// malformed or conflicting control sets exclude the candidate, never guess.
fn reasoning(raw: &Map<String, Value>) -> Result<Option<Vec<String>>, String> {
    let Some(value) = raw.get("reasoning_effort_options").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let values = value.as_array().filter(|v| v.len() <= 5).ok_or(INVALID)?;
    let mut out = Vec::new();
    for value in values {
        let v = value
            .as_str()
            .filter(|v| ["minimal", "low", "medium", "high", "xhigh"].contains(v))
            .ok_or(INVALID)?;
        if out.iter().any(|old| old == v) {
            return Err(INVALID.into());
        }
        out.push(v.to_string());
    }
    Ok(Some(out))
}
fn project_entry(
    raw: &Map<String, Value>,
    binding: &TraeModelBinding,
    function: SourceFunction,
) -> Result<Option<TraeNativeModelView>, Error> {
    let config = name(raw.get("config_name"), Location::ConfigName)?;
    if custom(raw) {
        return Ok(None);
    }
    let mut projected_limits = limits(raw, false)?;
    let mut projected_reasoning = match reasoning(raw) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if let Some(details) = raw.get("model_detail_list") {
        let details_value = details;
        let details = details
            .as_array()
            .ok_or_else(|| Error::wrong_type(Location::DetailsType, Some(details_value)))?;
        if details.len() > MAX_ENTRIES {
            return Err(Error::shape(
                Location::DetailsCount,
                Some(details_value),
                Class::Oversize,
            ));
        }
        let mut agreed = None;
        for detail in details {
            let detail = detail
                .as_object()
                .ok_or_else(|| Error::wrong_type(Location::DetailType, Some(detail)))?;
            if custom(detail) {
                return Ok(None);
            }
            let detail_reasoning = match reasoning(detail) {
                Ok(v) => v,
                Err(_) => return Ok(None),
            };
            if let Some(current) = detail_reasoning {
                if projected_reasoning
                    .as_ref()
                    .is_some_and(|prior| prior != &current)
                {
                    return Ok(None);
                }
                projected_reasoning = Some(current);
            }
            let current = limits(detail, true)?;
            if agreed.is_some_and(|prior| prior != current) {
                return Ok(None);
            }
            agreed = Some(current);
        }
        if let Some(detail) = agreed {
            if (projected_limits.0.is_some()
                && detail.0.is_some()
                && projected_limits.0 != detail.0)
                || (projected_limits.1.is_some()
                    && detail.1.is_some()
                    && projected_limits.1 != detail.1)
            {
                return Ok(None);
            }
            projected_limits = (
                projected_limits.0.or(detail.0),
                projected_limits.1.or(detail.1),
            );
        }
    }
    let display = match raw.get("display_config").filter(|v| !v.is_null()) {
        Some(value) => {
            let object = value
                .as_object()
                .ok_or_else(|| Error::wrong_type(Location::DisplayType, Some(value)))?;
            match object.get("display_name") {
                Some(Value::String(value)) if value.is_empty() => config,
                Some(value) => name(Some(value), Location::DisplayName)?,
                None => config,
            }
        }
        None => config,
    };
    // No native declaration-field mapping has been established for this profile.
    // Remote names or unknown opaque fields cannot establish native capabilities.
    Ok(Some(TraeNativeModelView {
        selector_id: selector_id(binding.profile_id, function, config),
        config_name: config.into(),
        display_name: display.into(),
        prompt_max_tokens: projected_limits.0,
        max_tokens: projected_limits.1,
        declared_vision: None,
        declared_tools: None,
        declared_thinking: None,
        reasoning_effort_options: projected_reasoning.unwrap_or_default(),
        protocol_carriers: ProtocolCarriers {
            text: true,
            tools: true,
            images: true,
            audio: false,
            video: false,
        },
        account_model_evidence: "unverified",
        raw_model: None,
    }))
}
fn selector_view_differences(
    previous: &TraeNativeModelView,
    current: &TraeNativeModelView,
) -> Vec<SelectorField> {
    let mut fields = Vec::new();
    if previous.display_name != current.display_name {
        fields.push(SelectorField::DisplayName);
    }
    if previous.prompt_max_tokens != current.prompt_max_tokens {
        fields.push(SelectorField::PromptMaxTokens);
    }
    if previous.max_tokens != current.max_tokens {
        fields.push(SelectorField::MaxTokens);
    }
    if previous.declared_vision != current.declared_vision {
        fields.push(SelectorField::DeclaredVision);
    }
    if previous.declared_tools != current.declared_tools {
        fields.push(SelectorField::DeclaredTools);
    }
    if previous.declared_thinking != current.declared_thinking {
        fields.push(SelectorField::DeclaredThinking);
    }
    if previous.reasoning_effort_options != current.reasoning_effort_options {
        fields.push(SelectorField::ReasoningEffortOptions);
    }
    if previous.protocol_carriers != current.protocol_carriers {
        fields.push(SelectorField::ProtocolCarriers);
    }
    if previous.account_model_evidence != current.account_model_evidence {
        fields.push(SelectorField::AccountModelEvidence);
    }
    fields
}
fn project_inner(
    binding: &TraeModelBinding,
    function: SourceFunction,
    packet: &Value,
) -> Result<TraeNativeCatalogView, Error> {
    let object = packet
        .as_object()
        .ok_or_else(|| Error::wrong_type(Location::RootType, Some(packet)))?;
    if object.get("code").is_some_and(|v| v.as_i64() != Some(0)) {
        return Err("native_catalog_service_error".into());
    }
    if let Some(metadata) = object.get("ResponseMetadata").filter(|v| !v.is_null()) {
        if !metadata.is_object() || metadata.get("Error").is_some_and(|v| !v.is_null()) {
            return Err("native_catalog_service_error".into());
        }
    }
    if object
        .get("function")
        .is_some_and(|v| v.as_str() != Some(function.as_str()))
    {
        return Err("native_catalog_function_mismatch".into());
    }
    let list = object
        .get("config_info_list")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::wrong_type(Location::ListType, object.get("config_info_list")))?;
    if list.len() > MAX_ENTRIES {
        return Err("native_catalog_entry_limit".into());
    }
    let mut seen = HashMap::<&str, (&Map<String, Value>, Option<usize>)>::new();
    let mut entries: Vec<TraeNativeModelView> = Vec::new();
    let mut excluded_count = 0;
    for value in list {
        let raw = value
            .as_object()
            .ok_or_else(|| Error::wrong_type(Location::EntryType, Some(value)))?;
        let config = name(raw.get("config_name"), Location::ConfigName)?;
        if raw
            .get("function")
            .is_some_and(|v| v.as_str() != Some(function.as_str()))
        {
            return Err("native_catalog_function_mismatch".into());
        }
        if let Some((previous, projected_index)) = seen.get(config) {
            if *previous == raw {
                continue;
            }
            // Tickets and requests consume only this complete, validated view.
            // Never merge excluded/invalid records or choose between differing views.
            let previous_view = projected_index.and_then(|index| entries.get(index));
            let current = project_entry(raw, binding, function);
            match (previous_view, current) {
                (None, _) => {
                    return Err(Error::selector_conflict(
                        SelectorConflictReason::PreviousNotSelectable,
                        Vec::new(),
                    ));
                }
                (Some(previous), Ok(Some(current))) => {
                    let fields = selector_view_differences(previous, &current);
                    if fields.is_empty() {
                        continue;
                    }
                    return Err(Error::selector_conflict(
                        SelectorConflictReason::ViewMismatch,
                        fields,
                    ));
                }
                (Some(_), Ok(None)) => {
                    return Err(Error::selector_conflict(
                        SelectorConflictReason::CurrentExcluded,
                        Vec::new(),
                    ));
                }
                (Some(_), Err(_)) => {
                    return Err(Error::selector_conflict(
                        SelectorConflictReason::CurrentInvalid,
                        Vec::new(),
                    ));
                }
            }
        }
        let projected_index = match project_entry(raw, binding, function)? {
            Some(entry) => {
                let index = entries.len();
                entries.push(entry);
                Some(index)
            }
            None => {
                excluded_count += 1;
                None
            }
        };
        seen.insert(config, (raw, projected_index));
    }
    Ok(TraeNativeCatalogView {
        account_id: binding.account_id.clone(),
        region: binding.region,
        profile_id: binding.profile_id,
        source_function: function,
        fetched_at_ms: chrono::Utc::now().timestamp_millis(),
        catalog_revision: uuid::Uuid::new_v4().to_string(),
        entries,
        excluded_count,
        warnings: if excluded_count > 0 {
            vec!["native_catalog_custom_source_or_detail_conflict_excluded"]
        } else {
            Vec::new()
        },
    })
}

/// Preserve the public result contract; only typed projection failures log once.
pub(super) fn project(
    binding: &TraeModelBinding,
    function: SourceFunction,
    packet: &Value,
) -> Result<TraeNativeCatalogView, String> {
    if binding.profile_id.is_web() {
        return web::project(binding, function, packet);
    }
    project_inner(binding, function, packet).map_err(|error| error.into_public(packet))
}
