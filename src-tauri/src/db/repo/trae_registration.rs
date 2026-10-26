//! Native registration deliberately does not use the generic suffix/replace merge.
use super::*;
use crate::agent_upstream::runtime::{trae_options, TraeRuntimeOptions};
use crate::proxy::trae::profile::TraeModelBinding;
use sha2::{Digest, Sha256};

fn invalid(message: &'static str) -> crate::error::GatewayError {
    crate::error::GatewayError::Protocol(message.into())
}
fn saved_options(row: &sqlx::sqlite::SqliteRow) -> Result<serde_json::Value> {
    let raw = row
        .try_get::<Option<String>, _>("options_json")
        .map_err(|_| invalid("native_runtime_options_invalid"))?
        .ok_or_else(|| invalid("native_runtime_options_invalid"))?;
    serde_json::from_str(&raw).map_err(|_| invalid("native_runtime_options_invalid"))
}
pub(crate) fn native_ids(binding: &TraeModelBinding) -> (String, String) {
    let mut hash = Sha256::new();
    for part in [
        match binding.region {
            crate::proxy::trae::profile::TraeRegion::Cn => "cn",
            crate::proxy::trae::profile::TraeRegion::Global => "global",
        },
        &binding.account_id,
        &binding.principal_hash,
        binding.profile_id.as_str(),
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let identity = format!("{:x}", hash.finalize());
    (format!("p-trae-{identity}"), format!("trae-{identity}"))
}

/// Caller owns the private auth mutation gate throughout this local transaction.
/// The final synchronous check cannot initiate HTTP or return an unpolled future.
pub(crate) async fn register_native(
    pool: &SqlitePool,
    requested: &TraeRuntimeOptions,
    before_commit: impl FnOnce() -> std::result::Result<(), String>,
) -> Result<(String, String, Vec<String>)> {
    let requested_json =
        serde_json::to_value(requested).map_err(|_| invalid("native_registration_invalid"))?;
    trae_options(Some(&requested_json)).map_err(crate::error::GatewayError::Protocol)?;
    let origin = requested.binding().profile_id.origin();
    let (provider_id, runtime_id) = native_ids(&requested.binding());
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let mut merged = requested.clone();
    merged.model_selections.clear();
    let mut warnings = Vec::new();
    let existing_runtime =
        sqlx::query("SELECT kind,options_json,enabled FROM agent_runtimes WHERE id=?")
            .bind(&runtime_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(row) = existing_runtime.as_ref() {
        if row.get::<String, _>("kind") != "trae-model" {
            return Err(invalid("native_runtime_identity_conflict"));
        }
        let raw = saved_options(row)?;
        merged =
            trae_options(Some(&raw)).map_err(|_| invalid("native_runtime_identity_conflict"))?;
        if merged.binding() != requested.binding() {
            return Err(invalid("native_runtime_identity_conflict"));
        }
        if row.get::<i64, _>("enabled") != 1 {
            warnings.push("native_existing_runtime_disabled".into());
        }
    }
    let existing_provider = sqlx::query(
        "SELECT runtime_id,dialect,base_url,api_key_enc,enabled FROM providers WHERE id=?",
    )
    .bind(&provider_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = existing_provider.as_ref() {
        if existing_runtime.is_none()
            || row.get::<Option<String>, _>("runtime_id").as_deref() != Some(&runtime_id)
            || row.get::<String, _>("dialect") != "openai"
            || row.get::<String, _>("base_url") != origin
            || !row.get::<String, _>("api_key_enc").is_empty()
        {
            return Err(invalid("native_provider_identity_conflict"));
        }
        if row.get::<i64, _>("enabled") != 1 {
            warnings.push("native_existing_provider_disabled".into());
        }
    }
    if existing_runtime.is_some() {
        let foreign: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM providers WHERE runtime_id=? AND id<>?)",
        )
        .bind(&runtime_id)
        .bind(&provider_id)
        .fetch_one(&mut *tx)
        .await?;
        if foreign {
            return Err(invalid("native_runtime_foreign_provider_reference"));
        }
        let present = sqlx::query("SELECT id,alias,upstream FROM models WHERE provider_id=?")
            .bind(&provider_id)
            .fetch_all(&mut *tx)
            .await?;
        for row in present {
            let alias: String = row.get("alias");
            let selected = merged
                .model_selections
                .get(&alias)
                .ok_or_else(|| invalid("native_saved_selection_model_mismatch"))?;
            if row.get::<String, _>("id") != format!("{provider_id}:{alias}")
                || row.get::<String, _>("upstream") != selected.config_name
            {
                return Err(invalid("native_saved_selection_model_mismatch"));
            }
        }
    }
    // Include disabled rows and legacy upstream names: NativeExact's resolver also
    // considers them, so an alias must not be accepted only to become ambiguous.
    for (alias, selection) in &requested.model_selections {
        let model_id = format!("{provider_id}:{alias}");
        let conflicts = sqlx::query("SELECT id,provider_id,alias,upstream,enabled FROM models WHERE alias=? OR upstream=? OR id=?")
            .bind(alias).bind(alias).bind(&model_id).fetch_all(&mut *tx).await?;
        let prior = merged.model_selections.get(alias);
        if prior.is_some_and(|old| {
            old.selector_id != selection.selector_id
                || old.source_function != selection.source_function
                || old.config_name != selection.config_name
        }) {
            return Err(invalid("native_alias_selection_conflict"));
        }
        let mut found = false;
        for row in conflicts {
            if row.get::<String, _>("id") != model_id
                || row.get::<String, _>("provider_id") != provider_id
                || row.get::<String, _>("alias") != *alias
                || row.get::<String, _>("upstream") != selection.config_name
                || prior.is_none()
            {
                return Err(invalid("native_global_alias_or_model_id_conflict"));
            }
            found = true;
            if row.get::<i64, _>("enabled") != 1 {
                warnings.push("native_existing_model_disabled".into());
            }
        }
        // Explicit fresh-ticket registration can restore missing rows for the same
        // authenticated binding. Keep other selections so subset recovery can resume.
        // Present rows and all foreign references were validated above; collisions
        // still fail, and no deleted pricing/policy is invented.
        if found {
            warnings.push("native_existing_model_settings_preserved".into());
        }
        merged
            .model_selections
            .insert(alias.clone(), selection.clone());
    }
    let merged_json =
        serde_json::to_value(&merged).map_err(|_| invalid("native_registration_invalid"))?;
    trae_options(Some(&merged_json)).map_err(crate::error::GatewayError::Protocol)?;
    let now = Utc::now().to_rfc3339();
    if existing_runtime.is_some() {
        sqlx::query("UPDATE agent_runtimes SET options_json=?,updated_at=? WHERE id=?")
            .bind(merged_json.to_string())
            .bind(&now)
            .bind(&runtime_id)
            .execute(&mut *tx)
            .await?;
    } else {
        let label = format!(
            "Trae {} {}",
            requested.region,
            if requested.binding().profile_id.is_web() {
                "Web Raw"
            } else {
                "Native"
            }
        );
        sqlx::query("INSERT INTO agent_runtimes(id,kind,label,options_json,enabled,created_at,updated_at) VALUES(?,'trae-model',?,?,1,?,?)")
            .bind(&runtime_id).bind(label).bind(merged_json.to_string()).bind(&now).bind(&now).execute(&mut *tx).await?;
    }
    if existing_provider.is_none() {
        let label = format!(
            "Trae {} {}",
            requested.region,
            if requested.binding().profile_id.is_web() {
                "Web Raw"
            } else {
                "Native"
            }
        );
        sqlx::query("INSERT INTO providers(id,name,dialect,base_url,api_key_enc,enabled,priority,rpm_limit,intelligence,note,runtime_id,created_at,updated_at) VALUES(?,?,'openai',?,'',1,30,0,50,'NativeExact; account model evidence unverified',?,?,?)")
            .bind(&provider_id).bind(label).bind(origin).bind(&runtime_id).bind(&now).bind(&now).execute(&mut *tx).await?;
    }
    for (alias, selected) in &requested.model_selections {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM models WHERE provider_id=? AND alias=?)",
        )
        .bind(&provider_id)
        .bind(alias)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            // No total-context or model capability proof was supplied. The input
            // limit and protocol carriers remain in typed runtime provenance.
            sqlx::query("INSERT INTO models(id,provider_id,alias,upstream,context_window,supports_tools,supports_vision,supports_thinking,supports_stream) VALUES(?,?,?,?,0,0,0,0,1)")
                .bind(format!("{provider_id}:{alias}")).bind(&provider_id).bind(alias).bind(&selected.config_name).execute(&mut *tx).await?;
        }
    }
    before_commit().map_err(crate::error::GatewayError::Protocol)?;
    tx.commit().await?;
    warnings.sort();
    warnings.dedup();
    Ok((provider_id, runtime_id, warnings))
}

/// Validate generic editor saves in the same transaction as their writes.
pub(super) async fn guard_native_provider_on(
    connection: &mut SqliteConnection,
    p: &Provider,
    preserve_runtime: bool,
) -> Result<bool> {
    let saved = sqlx::query("SELECT p.runtime_id,r.kind,r.options_json FROM providers p LEFT JOIN agent_runtimes r ON r.id=p.runtime_id WHERE p.id=?")
        .bind(&p.id).fetch_optional(&mut *connection).await?;
    let Some(row) = saved else {
        return Ok(false);
    };
    if row.get::<Option<String>, _>("kind").as_deref() != Some("trae-model") {
        return Ok(false);
    }
    let runtime_id = row.get::<Option<String>, _>("runtime_id");
    let raw = saved_options(&row)?;
    let options = trae_options(Some(&raw)).map_err(crate::error::GatewayError::Protocol)?;
    if (!preserve_runtime && p.runtime_id != runtime_id)
        || p.dialect != Dialect::OpenAI
        || p.base_url != options.binding().profile_id.origin()
        || !p.api_key_enc.is_empty()
    {
        return Err(invalid("native_provider_fixed_binding_required"));
    }
    let previous = list_models_on(connection, &p.id).await?;
    if previous.len() != p.models.len() {
        return Err(invalid("native_model_identity_edit_denied"));
    }
    let mut aliases = std::collections::HashSet::new();
    for model in &p.models {
        let old = previous
            .iter()
            .find(|m| m.alias == model.alias)
            .ok_or_else(|| invalid("native_model_identity_edit_denied"))?;
        let selected = options
            .model_selections
            .get(&model.alias)
            .ok_or_else(|| invalid("native_saved_selection_model_mismatch"))?;
        if !aliases.insert(&model.alias)
            || model.upstream != old.upstream
            || model.upstream != selected.config_name
            || model.upstream_path != old.upstream_path
            || model.model_type != old.model_type
            || model.context_window != old.context_window
            || model.supports_tools != old.supports_tools
            || model.supports_vision != old.supports_vision
            || model.supports_audio != old.supports_audio
            || model.supports_video != old.supports_video
            || model.supports_thinking != old.supports_thinking
            || model.supports_stream != old.supports_stream
            || model.local != old.local
            || model.capabilities != old.capabilities
        {
            return Err(invalid("native_model_identity_edit_denied"));
        }
        if let Some(overrides) = &model.overrides {
            if overrides.temperature.is_some()
                || overrides.extra_body.is_some()
                || overrides
                    .extra_headers
                    .as_ref()
                    .is_some_and(|h| !h.is_empty())
                || overrides
                    .max_tokens
                    .is_some_and(|v| v == 0 || selected.max_tokens.is_some_and(|max| v > max))
            {
                return Err(invalid("native_model_unsupported_defaults"));
            }
        }
    }
    Ok(true)
}

/// Generic runtime edits may only change display label and enabled state. Keep
/// the latest saved selections instead of writing an editor's stale options.
pub(crate) async fn save_native_runtime_settings(
    pool: &SqlitePool,
    proposed: &AgentRuntime,
) -> Result<()> {
    proposed
        .validate()
        .map_err(crate::error::GatewayError::Protocol)?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query("SELECT kind,options_json FROM agent_runtimes WHERE id=?")
        .bind(&proposed.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| invalid("native_runtime_requires_explicit_registration"))?;
    let raw = saved_options(&row)?;
    trae_options(Some(&raw)).map_err(crate::error::GatewayError::Protocol)?;
    if row.get::<String, _>("kind") != "trae-model"
        || proposed.kind != "trae-model"
        || proposed.options.as_ref() != Some(&raw)
    {
        return Err(invalid("native_runtime_identity_edit_denied"));
    }
    sqlx::query("UPDATE agent_runtimes SET label=?,enabled=?,updated_at=? WHERE id=?")
        .bind(&proposed.label)
        .bind(proposed.enabled)
        .bind(Utc::now().to_rfc3339())
        .bind(&proposed.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
