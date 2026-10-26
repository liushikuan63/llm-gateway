use super::*;
use crate::proxy::trae::catalog::TraeNativeCatalogView;
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeSelectionInput {
    pub selector_id: String,
    pub alias: String,
    pub allow_undeclared_images: bool,
}
#[derive(Debug, Serialize)]
pub(crate) struct NativeRegistrationResult {
    pub provider_id: String,
    pub runtime_id: String,
    pub model_count: usize,
    pub warnings: Vec<String>,
}
use crate::agent_upstream::runtime::{TraeModelSelection, TraeRuntimeOptions};
use crate::proxy::trae::profile::{SourceFunction, TraeModelBinding, TraeProfileId, TraeRegion};
use std::collections::BTreeMap;

async fn binding(
    gateway: &GatewayState,
    config: &parking_lot::RwLock<AppConfig>,
    account: &str,
) -> Result<TraeModelBinding, String> {
    binding_for_profile(
        gateway,
        config,
        account,
        TraeProfileId::CnOrdinary343Aicc20260212V1,
    )
    .await
}
async fn binding_for_profile(
    gateway: &GatewayState,
    config: &parking_lot::RwLock<AppConfig>,
    account: &str,
    profile: TraeProfileId,
) -> Result<TraeModelBinding, String> {
    if profile.is_web() {
        let auth = gateway
            .trae_auth
            .as_ref()
            .map_err(|_| "native_auth_unavailable")?;
        auth.set_proxy(config.read().http_proxy.as_deref());
        let region = match profile.region() {
            TraeRegion::Cn => crate::trae_auth::WebRegion::Cn,
            TraeRegion::Global => crate::trae_auth::WebRegion::Global,
        };
        let status = auth.web_status(region, account).await?;
        if status.status != "authenticated" {
            return Err("native_web_login_required".into());
        }
        let bound = TraeModelBinding {
            region: profile.region(),
            account_id: account.into(),
            profile_id: profile,
            principal_hash: status
                .identity
                .and_then(|v| v.identity_hash)
                .ok_or("native_login_identity_unavailable")?,
        };
        auth.prepare_model_authorization(&bound).await?;
        return Ok(bound);
    }
    require_trae_catalog_account(&config.read().benefits, account)?;
    let auth = gateway
        .trae_auth
        .as_ref()
        .map_err(|_| "native_auth_unavailable")?;
    auth.set_proxy(config.read().http_proxy.as_deref());
    let status = auth
        .status(account)
        .await
        .map_err(|_| "native_login_status_unavailable")?;
    if status.status != "authenticated" {
        return Err("native_login_required".into());
    }
    let principal_hash = status
        .identity
        .and_then(|v| v.identity_hash)
        .ok_or("native_login_identity_unavailable")?;
    let binding = TraeModelBinding {
        region: TraeRegion::Cn,
        account_id: account.into(),
        principal_hash,
        profile_id: TraeProfileId::CnOrdinary343Aicc20260212V1,
    };
    auth.prepare_model_authorization(&binding).await?;
    require_trae_catalog_account(&config.read().benefits, account)?;
    Ok(binding)
}
pub(crate) async fn catalog(
    gateway: &GatewayState,
    config: &parking_lot::RwLock<AppConfig>,
    account: &str,
    source: &str,
) -> Result<TraeNativeCatalogView, String> {
    let function: SourceFunction = serde_json::from_value(serde_json::json!(source))
        .map_err(|_| "native_source_function_unsupported")?;
    let binding = binding(gateway, config, account).await?;
    let view = gateway
        .trae
        .as_ref()
        .map_err(|_| "native_client_unavailable")?
        .native_catalog(&binding, function)
        .await?;
    require_trae_catalog_account(&config.read().benefits, account)?;
    Ok(view)
}
pub(crate) async fn web_catalog(
    gateway: &GatewayState,
    config: &parking_lot::RwLock<AppConfig>,
    account: &str,
    region: crate::trae_auth::WebRegion,
) -> Result<TraeNativeCatalogView, String> {
    let profile = match region {
        crate::trae_auth::WebRegion::Cn => TraeProfileId::CnWebRawV1,
        crate::trae_auth::WebRegion::Global => TraeProfileId::GlobalWebRawSgV1,
    };
    let bound = binding_for_profile(gateway, config, account, profile).await?;
    gateway
        .trae
        .as_ref()
        .map_err(|_| "native_client_unavailable")?
        .native_catalog(&bound, profile.function())
        .await
}
pub(crate) async fn register(
    gateway: &GatewayState,
    config: &parking_lot::RwLock<AppConfig>,
    account: &str,
    profile: &str,
    revision: &str,
    selections: Vec<NativeSelectionInput>,
) -> Result<NativeRegistrationResult, String> {
    register_impl(
        gateway,
        config,
        account,
        profile,
        revision,
        selections,
        || {},
    )
    .await
}
async fn register_impl(
    gateway: &GatewayState,
    config: &parking_lot::RwLock<AppConfig>,
    account: &str,
    profile: &str,
    revision: &str,
    selections: Vec<NativeSelectionInput>,
    after_commit: impl FnOnce(),
) -> Result<NativeRegistrationResult, String> {
    let profile_id: TraeProfileId = serde_json::from_value(serde_json::json!(profile))
        .map_err(|_| "native_profile_unsupported")?;
    if selections.is_empty() || selections.len() > 512 || uuid::Uuid::parse_str(revision).is_err() {
        return Err("native_registration_selection_invalid".into());
    }
    let mut aliases = HashSet::new();
    let mut selectors = HashSet::new();
    for selected in &selections {
        if !aliases.insert(&selected.alias) || !selectors.insert(&selected.selector_id) {
            return Err("native_registration_duplicate_selection".into());
        }
    }
    let bound = binding_for_profile(gateway, config, account, profile_id).await?;
    let auth = gateway
        .trae_auth
        .as_ref()
        .map_err(|_| "native_auth_unavailable")?;
    let client = gateway
        .trae
        .as_ref()
        .map_err(|_| "native_client_unavailable")?;
    let mut tickets = Vec::with_capacity(selections.len());
    let mut models = BTreeMap::new();
    for selected in &selections {
        let ticket = client
            .consume_cached_catalog(
                &bound,
                profile_id.function(),
                revision,
                &selected.selector_id,
            )
            .await?;
        if ticket.binding() != &bound {
            return Err("native_catalog_binding_mismatch".into());
        }
        if selected.allow_undeclared_images
            && (ticket.entry().declared_vision.is_some()
                || !ticket.entry().protocol_carriers.images)
        {
            return Err("native_image_candidate_policy_invalid".into());
        }
        if tickets
            .first()
            .is_some_and(|first: &crate::proxy::trae::NativeCatalogTicket| {
                first.generation() != ticket.generation()
            })
        {
            return Err("native_model_session_changed".into());
        }
        models.insert(
            selected.alias.clone(),
            TraeModelSelection::from_ticket(&ticket, selected.allow_undeclared_images),
        );
        tickets.push(ticket);
    }
    let requested = TraeRuntimeOptions {
        region: match bound.region {
            TraeRegion::Cn => "cn",
            TraeRegion::Global => "global",
        }
        .into(),
        account_id: bound.account_id.clone(),
        principal_hash: bound.principal_hash.clone(),
        profile_id: profile.into(),
        model_selections: models,
    };
    let raw = serde_json::to_value(&requested).map_err(|_| "native_registration_invalid")?;
    crate::agent_upstream::runtime::trae_options(Some(&raw))?;
    // This guard is an actual async mutex guard, held until commit completes. No
    // future is returned from a synchronous lock closure and awaited afterwards.
    let gate = auth
        .lock_model_registration(&bound, tickets[0].generation())
        .await?;
    let result = repo::trae_registration::register_native(gateway.db.pool(), &requested, || {
        if !profile_id.is_web() {
            require_trae_catalog_account(&config.read().benefits, account)?;
        }
        gate.current()?;
        for ticket in &tickets {
            client.registration_ticket_current(ticket)?;
        }
        Ok(())
    })
    .await
    .map_err(|error| match error {
        crate::error::GatewayError::Protocol(message) => message,
        _ => "native_registration_storage_failed".into(),
    })?;
    drop(gate);
    after_commit();
    let (provider_id, runtime_id, mut warnings) = result;
    gateway
        .reload_providers()
        .await
        .map_err(|_| "native_registration_saved_reload_failed")?;
    let providers = repo::list_providers(gateway.db.pool())
        .await
        .map_err(|_| "native_registration_saved_readback_failed")?;
    let saved = providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or("native_registration_saved_readback_failed")?;
    let runtime = repo::get_agent_runtime(gateway.db.pool(), &runtime_id)
        .await
        .map_err(|_| "native_registration_saved_readback_failed")?
        .ok_or("native_registration_saved_readback_failed")?;
    let actual = crate::agent_upstream::runtime::trae_options(runtime.options.as_ref())?;
    if saved.runtime_id.as_deref() != Some(&runtime_id)
        || runtime.kind != "trae-model"
        || saved.dialect != Dialect::OpenAI
        || saved.base_url != profile_id.origin()
        || !saved.api_key_enc.is_empty()
        || actual.binding() != bound
        || selections.iter().any(|s| {
            actual
                .model_selections
                .get(&s.alias)
                .map_or(true, |m| m.selector_id != s.selector_id)
                || !saved.models.iter().any(|m| {
                    m.alias == s.alias
                        && m.upstream == actual.model_selections[&s.alias].config_name
                })
        })
    {
        return Err("native_registration_saved_binding_changed".into());
    }
    if (!profile_id.is_web()
        && require_trae_catalog_account(&config.read().benefits, account).is_err())
        || auth.prepare_model_authorization(&bound).await.is_err()
    {
        warnings.push("native_registration_saved_account_currently_unavailable".into());
    }
    if !saved.enabled {
        warnings.push("native_existing_provider_disabled".into());
    }
    if !runtime.enabled {
        warnings.push("native_existing_runtime_disabled".into());
    }
    if selections.iter().any(|s| {
        saved
            .models
            .iter()
            .any(|m| m.alias == s.alias && !m.enabled)
    }) {
        warnings.push("native_existing_model_disabled".into());
    }
    warnings.sort();
    warnings.dedup();
    warnings.push("native_model_service_unverified".into());
    Ok(NativeRegistrationResult {
        provider_id,
        runtime_id,
        model_count: selections.len(),
        warnings,
    })
}
pub(super) fn validate_ipc(body: &tauri::ipc::InvokeBody, allowed: &[&str]) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Json(value) = body else {
        return Err("native_ipc_arguments_invalid".into());
    };
    let args = value.as_object().ok_or("native_ipc_arguments_invalid")?;
    if args.len() != allowed.len() || args.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("native_ipc_arguments_invalid".into());
    }
    Ok(())
}
pub(super) fn saved_discovery(
    input: &model_catalog::DiscoveryInput,
    saved: &Provider,
    runtime: &crate::domain::AgentRuntime,
) -> Result<model_catalog::DiscoveryResponse, String> {
    let options = crate::agent_upstream::runtime::trae_options(runtime.options.as_ref())?;
    let origin = options.binding().profile_id.origin();
    if saved.dialect != Dialect::OpenAI
        || saved.base_url != origin
        || !saved.api_key_enc.is_empty()
        || saved.runtime_id.as_deref() != Some(&runtime.id)
        || runtime.kind != "trae-model"
        || input.dialect != Dialect::OpenAI
        || input.base_url.trim_end_matches('/') != origin
        || !input.api_key.is_empty()
    {
        return Err("native_provider_fixed_binding_required".into());
    }
    let models = saved
        .models
        .iter()
        .map(|m| {
            let selection = options
                .model_selections
                .get(&m.alias)
                .filter(|s| s.config_name == m.upstream)
                .ok_or("native_saved_selection_model_mismatch")?;
            Ok(model_catalog::DiscoveredModel {
                id: m.upstream.clone(),
                name: selection.display_name.clone(),
                context_window: 0,
                context_source: model_catalog::ContextSource::Default,
                supports_tools: None,
                supports_vision: None,
                supports_audio: None,
                supports_video: None,
                supports_stream: Some(true),
                supports_thinking: None,
                model_type: Some(crate::domain::ModelType::Chat),
                is_free: None,
                price: m.price.clone(),
                capabilities: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(model_catalog::DiscoveryResponse {
        base_url: saved.base_url.clone(),
        models,
        warnings: vec![
            "native_local_saved_selections_only; total_context_unknown; model_service_unverified"
                .into(),
        ],
    })
}
#[cfg(test)]
#[path = "trae_registration_tests.rs"]
mod tests;
