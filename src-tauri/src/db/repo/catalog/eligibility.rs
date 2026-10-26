use super::*;
use crate::model_catalog::{ContextSource, DiscoveredModel};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreeEligibility {
    pub eligible: bool,
    pub reason: String,
    pub observed_at: Option<i64>,
    pub expires_at: Option<i64>,
}
impl FreeEligibility {
    fn denied(reason: &str) -> Self {
        Self {
            eligible: false,
            reason: reason.into(),
            observed_at: None,
            expires_at: None,
        }
    }
}

/// Shared fee fact consumer for preview, filtering and final physical sends.
/// Read target, alias, user intent and observation in one SQLite snapshot;
/// neither a manual zero price nor a name hint supplies positive evidence.
pub async fn free_eligibility(
    pool: &SqlitePool,
    provider: &Provider,
    model: &ModelRef,
    now: i64,
) -> Result<FreeEligibility> {
    free_eligibility_checked(pool, provider, model, || now).await
}

pub async fn free_eligibility_checked(
    pool: &SqlitePool,
    provider: &Provider,
    model: &ModelRef,
    clock: impl Fn() -> i64,
) -> Result<FreeEligibility> {
    if provider.runtime_id.is_some() {
        return Ok(FreeEligibility::denied(
            "account_hard_stop_and_reservation_unverified",
        ));
    }
    if !matches!(
        provider.dialect,
        crate::domain::Dialect::OpenAI | crate::domain::Dialect::Responses
    ) {
        return Ok(FreeEligibility::denied("fee_source_protocol_unverified"));
    }
    // One SELECT supplies a coherent SQLite snapshot. There is no transaction
    // release or additional database await between this read and the final clock.
    let row=sqlx::query("SELECT p.dialect,p.base_url,p.api_key_enc,p.enabled AS provider_enabled,p.runtime_id,m.enabled AS model_enabled,m.model_type,m.context_window,m.upstream_path,m.overrides_json,m.price_json,m.supports_tools,m.supports_vision,m.supports_audio,m.supports_video,m.supports_thinking,m.supports_stream,cp.user_override,COALESCE(s.clock_anomaly,0) AS clock_anomaly,i.target_fingerprint,i.source_contract,i.fee_status,i.observed_at,i.fee_expires_at,i.lifecycle,i.last_complete_run_id,i.model_json FROM providers p JOIN models m ON m.provider_id=p.id LEFT JOIN catalog_model_policy cp ON cp.provider_id=m.provider_id AND cp.alias=m.alias AND cp.upstream_id=m.upstream LEFT JOIN catalog_scan_state s ON s.provider_id=p.id LEFT JOIN catalog_inventory i ON i.provider_id=m.provider_id AND i.upstream_id=m.upstream WHERE p.id=? AND m.alias=? AND m.upstream=?")
        .bind(&provider.id).bind(&model.alias).bind(&model.upstream).fetch_optional(pool).await?;
    let Some(row) = row else {
        return Ok(FreeEligibility::denied("model_identity_changed_or_missing"));
    };
    let actual = fingerprint(
        &provider.id,
        &serde_json::Value::String(row.get("dialect")),
        &row.get::<String, _>("base_url"),
        &row.get::<String, _>("api_key_enc"),
        row.get::<i64, _>("provider_enabled") == 1,
        row.get::<Option<String>, _>("runtime_id").as_deref(),
    );
    if actual != target_fingerprint(provider) || !provider.enabled {
        return Ok(FreeEligibility::denied(
            "provider_target_changed_or_disabled",
        ));
    }
    if row.get::<i64, _>("model_enabled") != 1 {
        return Ok(FreeEligibility::denied("model_disabled"));
    }
    let current_path: Option<String> = row.get("upstream_path");
    let current_overrides: Option<crate::domain::ModelOverrides> = row
        .get::<Option<String>, _>("overrides_json")
        .map(|raw| serde_json::from_str(&raw))
        .transpose()?;
    if row.get::<String, _>("model_type") != model.model_type.code()
        || row.get::<i32, _>("context_window") != model.context_window
        || (row.get::<i64, _>("supports_tools") == 1) != model.supports_tools
        || (row.get::<i64, _>("supports_vision") == 1) != model.supports_vision
        || (row.get::<i64, _>("supports_audio") == 1) != model.supports_audio
        || (row.get::<i64, _>("supports_video") == 1) != model.supports_video
        || (row.get::<i64, _>("supports_thinking") == 1) != model.supports_thinking
        || (row.get::<i64, _>("supports_stream") == 1) != model.supports_stream
        || current_path != model.upstream_path
        || current_overrides != model.overrides
    {
        return Ok(FreeEligibility::denied("model_send_configuration_changed"));
    }
    // A configured nonzero/invalid estimate is a conservative veto, never a
    // positive proof. Numeric zero still requires all authoritative checks below.
    if let Some(raw) = row.get::<Option<String>, _>("price_json") {
        let Ok(price) = serde_json::from_str::<crate::domain::ModelPrice>(&raw) else {
            return Ok(FreeEligibility::denied("configured_price_unverified"));
        };
        if !price.is_valid()
            || price.prompt != 0.0
            || price.completion != 0.0
            || price.cache_read.is_some_and(|v| v != 0.0)
            || price.cache_creation.is_some_and(|v| v != 0.0)
            || price.tiers.iter().any(|tier| {
                tier.prompt != 0.0
                    || tier.completion != 0.0
                    || tier.cache_read.is_some_and(|v| v != 0.0)
                    || tier.cache_creation.is_some_and(|v| v != 0.0)
            })
        {
            return Ok(FreeEligibility::denied(
                "configured_price_conflicts_with_free",
            ));
        }
    }
    if current_path.is_some()
        || current_overrides
            .as_ref()
            .and_then(|o| o.extra_headers.as_ref())
            .is_some_and(|h| !h.is_empty())
    {
        return Ok(FreeEligibility::denied(
            "custom_endpoint_or_identity_unverified",
        ));
    }
    if let Some(extra) = current_overrides
        .as_ref()
        .and_then(|overrides| overrides.extra_body.as_ref())
    {
        if let Some(reason) = crate::cost_policy::wire_shape_issue(extra, &model.upstream) {
            return Ok(FreeEligibility::denied(reason));
        }
    }
    if matches!(
        row.get::<Option<String>, _>("user_override").as_deref(),
        Some("disabled" | "removed")
    ) {
        return Ok(FreeEligibility::denied("user_opted_out"));
    }
    if row.get::<i64, _>("clock_anomaly") != 0 {
        return Ok(FreeEligibility::denied("clock_anomaly"));
    }
    let Some(fact_target) = row.get::<Option<String>, _>("target_fingerprint") else {
        return Ok(FreeEligibility::denied("fee_unknown"));
    };
    if fact_target != target_fingerprint(provider) {
        return Ok(FreeEligibility::denied("fee_target_changed"));
    }
    if row.get::<String, _>("source_contract") != "openrouter_full_v1"
        || row
            .get::<Option<String>, _>("last_complete_run_id")
            .is_none()
    {
        return Ok(FreeEligibility::denied(
            "fee_source_or_completeness_unverified",
        ));
    }
    if row.get::<String, _>("fee_status") != "free" {
        return Ok(FreeEligibility::denied("fee_paid_or_unknown"));
    }
    let observed: i64 = row.get("observed_at");
    let expires: Option<i64> = row.get("fee_expires_at");
    if row.get::<String, _>("lifecycle") == "soft_offline" {
        return Ok(FreeEligibility::denied("model_soft_offline"));
    }
    let metadata: DiscoveredModel = serde_json::from_str(&row.get::<String, _>("model_json"))?;
    if metadata.id != model.upstream
        || metadata.context_source != ContextSource::Provider
        || metadata.context_window <= 0
        || metadata.model_type != Some(model.model_type)
        || row.get::<i32, _>("context_window") > metadata.context_window
        || row.get::<i32, _>("context_window") <= 0
    {
        return Ok(FreeEligibility::denied("fee_use_or_context_unverified"));
    }
    let now = clock();
    if now < 0 || observed > now || expires.map_or(true, |until| until <= now) {
        return Ok(FreeEligibility::denied("fee_expired_or_future"));
    }
    Ok(FreeEligibility {
        eligible: true,
        reason: "catalog_exact_zero".into(),
        observed_at: Some(observed),
        expires_at: expires,
    })
}
