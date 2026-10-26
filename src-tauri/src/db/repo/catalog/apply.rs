use super::*;
use crate::domain::{ModelRef, PriceSource};
use crate::model_catalog::observation::{CatalogObservation, FeeStatus};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApplySummary {
    pub applied: bool,
    pub conflict: bool,
    pub added: usize,
    pub updated: usize,
    pub revoked: usize,
    pub configured: usize,
    pub missing_pending: usize,
    pub soft_offline: usize,
    pub clock_epoch_reset: bool,
}

fn projected(model: &crate::model_catalog::DiscoveredModel, alias: &str) -> Option<ModelRef> {
    Some(ModelRef {
        alias: alias.into(),
        upstream: model.id.clone(),
        context_window: model.context_window,
        supports_tools: model.supports_tools.unwrap_or(false),
        supports_vision: model.supports_vision.unwrap_or(false),
        supports_audio: model.supports_audio.unwrap_or(false),
        supports_video: model.supports_video.unwrap_or(false),
        supports_thinking: model.supports_thinking.unwrap_or(false),
        supports_stream: model.supports_stream.unwrap_or(false),
        model_type: model.model_type?,
        upstream_path: None,
        price: model.price.clone(),
        overrides: None,
        local: None,
        enabled: true,
        capabilities: None,
    })
}

async fn configure_on(
    connection: &mut SqliteConnection,
    provider_id: &str,
    entry: &crate::model_catalog::observation::ObservedModel,
    previous: Option<&str>,
    now: i64,
) -> Result<bool> {
    let existing = super::super::list_models_on(connection, provider_id).await?;
    let matching: Vec<_> = existing
        .iter()
        .filter(|m| m.upstream == entry.model.id)
        .collect();
    if matching.is_empty() {
        // Preserve explicit removals and alias collisions. A rediscovery is not
        // authorization to resurrect a user-deleted/disabled automatic model.
        let opted_out:i64=sqlx::query_scalar("SELECT COUNT(*) FROM catalog_model_policy WHERE provider_id=? AND upstream_id=? AND user_override IN ('disabled','removed')")
            .bind(provider_id).bind(&entry.model.id).fetch_one(&mut *connection).await?;
        if opted_out != 0 || existing.iter().any(|m| m.alias == entry.model.id) {
            return Ok(false);
        }
        let Some(model) = projected(&entry.model, &entry.model.id) else {
            return Ok(false);
        };
        sqlx::query("INSERT INTO catalog_model_policy(provider_id,alias,upstream_id,managed_by,user_override,created_at,updated_at) VALUES(?,?,?,'catalog','none',?,?) ON CONFLICT(provider_id,alias) DO NOTHING")
            .bind(provider_id).bind(&model.alias).bind(&model.upstream).bind(now).bind(now).execute(&mut *connection).await?;
        let policy:Option<String>=sqlx::query_scalar("SELECT managed_by FROM catalog_model_policy WHERE provider_id=? AND alias=? AND upstream_id=?")
            .bind(provider_id).bind(&model.alias).bind(&model.upstream).fetch_optional(&mut *connection).await?;
        if policy.as_deref() != Some("catalog") {
            return Ok(false);
        }
        super::super::insert_model_on(connection, provider_id, &model, false).await?;
        return Ok(true);
    }
    let Some(previous) = previous else {
        return Ok(false);
    };
    let old: crate::model_catalog::DiscoveredModel = serde_json::from_str(previous)?;
    for current in matching {
        let policy=sqlx::query("SELECT managed_by,user_override,field_overrides_json FROM catalog_model_policy WHERE provider_id=? AND alias=? AND upstream_id=?")
            .bind(provider_id).bind(&current.alias).bind(&current.upstream).fetch_optional(&mut *connection).await?;
        let Some(policy) = policy else { continue };
        if policy.get::<String, _>("managed_by") != "catalog"
            || matches!(
                policy.get::<String, _>("user_override").as_str(),
                "disabled" | "removed"
            )
            || !current.enabled
        {
            continue;
        }
        let Some(old_projection) = projected(&old, &current.alias) else {
            continue;
        };
        let Some(proposed) = projected(&entry.model, &current.alias) else {
            continue;
        };
        let mut overrides: serde_json::Map<String, Value> =
            serde_json::from_str(&policy.get::<String, _>("field_overrides_json"))?;
        let mut merged = current.clone();
        macro_rules! merge {
            ($field:ident) => {
                if current.$field != old_projection.$field {
                    overrides.insert(stringify!($field).into(), Value::Bool(true));
                }
                if overrides.get(stringify!($field)) != Some(&Value::Bool(true)) {
                    merged.$field = proposed.$field;
                }
            };
        }
        merge!(context_window);
        merge!(supports_tools);
        merge!(supports_vision);
        merge!(supports_audio);
        merge!(supports_video);
        merge!(supports_thinking);
        merge!(supports_stream);
        merge!(model_type);
        let update_price = current
            .price
            .as_ref()
            .map_or(true, |p| p.source != PriceSource::Manual)
            && current.price == old_projection.price
            && overrides.get("price") != Some(&Value::Bool(true));
        if current.price != old_projection.price {
            overrides.insert("price".into(), Value::Bool(true));
        }
        let price = proposed
            .price
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        // UPDATE selected fields, never REPLACE: preserve capability ledgers,
        // aliases, paths, user parameters, local metadata and exact manual JSON.
        sqlx::query("UPDATE models SET context_window=?,supports_tools=?,supports_vision=?,supports_audio=?,supports_video=?,supports_thinking=?,supports_stream=?,model_type=?,price_json=CASE WHEN ? THEN ? ELSE price_json END WHERE provider_id=? AND alias=? AND upstream=?")
            .bind(merged.context_window).bind(merged.supports_tools as i64).bind(merged.supports_vision as i64).bind(merged.supports_audio as i64).bind(merged.supports_video as i64).bind(merged.supports_thinking as i64).bind(merged.supports_stream as i64).bind(merged.model_type.code()).bind(update_price).bind(price).bind(provider_id).bind(&current.alias).bind(&current.upstream).execute(&mut *connection).await?;
        sqlx::query("UPDATE catalog_model_policy SET field_overrides_json=?,updated_at=? WHERE provider_id=? AND alias=?")
            .bind(serde_json::to_string(&overrides)?).bind(now).bind(provider_id).bind(&current.alias).execute(&mut *connection).await?;
    }
    Ok(false)
}

async fn conflict_on(connection: &mut SqliteConnection, lease: &ScanLease, now: i64) -> Result<()> {
    sqlx::query("UPDATE catalog_scan_state SET lease_owner=NULL,lease_expires_at=NULL,status='conflict',failure_kind='edit_conflict',next_retry_at=? WHERE provider_id=? AND run_id=? AND fence=?")
        .bind(now.saturating_add(RETRY_MS)).bind(&lease.provider_id).bind(&lease.run_id).bind(lease.fence).execute(&mut *connection).await?;
    sqlx::query("UPDATE catalog_scan_runs SET status='conflict',failure_kind='edit_conflict',finished_at=? WHERE run_id=? AND finished_at IS NULL")
        .bind(now).bind(&lease.run_id).execute(&mut *connection).await?;
    Ok(())
}

pub async fn apply_observation(
    pool: &SqlitePool,
    lease: &ScanLease,
    observation: &CatalogObservation,
    now: i64,
) -> Result<ApplySummary> {
    apply_observation_checked(pool, lease, observation, || Some(now)).await
}

/// Production callers supply a fresh clock/cancellation checkpoint. No database
/// wait may turn an expired owner or a cancelled configuration into an apply.
pub async fn apply_observation_checked(
    pool: &SqlitePool,
    lease: &ScanLease,
    observation: &CatalogObservation,
    checkpoint: impl Fn() -> Option<i64>,
) -> Result<ApplySummary> {
    if observation.models.len() > 4096 || observation.raw_count > 4096 {
        return Err(anyhow!("catalog row limit exceeded"));
    }
    if observation.source_contract != lease.source_contract {
        return Err(anyhow!("catalog source contract mismatch"));
    }
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let Some(now) = checkpoint() else {
        return Ok(ApplySummary::default());
    };
    if now < lease.started_at || now >= lease.expires_at {
        return Ok(ApplySummary::default());
    }
    if !owns(&mut tx, lease, now).await? {
        return Ok(ApplySummary::default());
    }
    if current_target(&mut tx, &lease.provider_id)
        .await?
        .as_deref()
        != Some(&lease.target_fingerprint)
        || revision(&mut tx, &lease.provider_id).await? != lease.captured_revision
    {
        conflict_on(&mut tx, lease, now).await?;
        tx.commit().await?;
        return Ok(ApplySummary {
            conflict: true,
            ..Default::default()
        });
    }
    let complete = observation.complete_success_eligible();
    let mut summary = ApplySummary {
        applied: true,
        clock_epoch_reset: lease.clock_epoch_reset,
        ..Default::default()
    };
    if lease.clock_epoch_reset {
        sqlx::query("UPDATE catalog_inventory SET first_missing_at=NULL,last_missing_observation_at=NULL,missing_confirmations=0,lifecycle=CASE WHEN lifecycle='missing_pending' THEN 'active' ELSE lifecycle END WHERE provider_id=? AND target_fingerprint=?")
            .bind(&lease.provider_id).bind(&lease.target_fingerprint).execute(&mut *tx).await?;
    }
    let mut ids = std::collections::HashSet::new();
    for entry in &observation.models {
        if !ids.insert(&entry.model.id) {
            return Err(anyhow!("duplicate observation identity"));
        }
        let previous=sqlx::query("SELECT model_json,fee_status,target_fingerprint FROM catalog_inventory WHERE provider_id=? AND upstream_id=?")
            .bind(&lease.provider_id).bind(&entry.model.id).fetch_optional(&mut *tx).await?;
        let same_target = previous
            .as_ref()
            .is_some_and(|r| r.get::<String, _>("target_fingerprint") == lease.target_fingerprint);
        let fee = if entry.fee_status == FeeStatus::Free && !complete {
            FeeStatus::Unknown
        } else {
            entry.fee_status
        };
        if previous.is_some() {
            summary.updated += 1;
        } else {
            summary.added += 1;
        }
        if previous
            .as_ref()
            .is_some_and(|p| p.get::<String, _>("fee_status") == "free")
            && fee != FeeStatus::Free
        {
            summary.revoked += 1;
        }
        let mut reasons = entry.fee_reason_codes.clone();
        if fee != entry.fee_status {
            reasons.push("incomplete_observation".into());
        }
        if complete && entry.auto_configurable && fee == FeeStatus::Free {
            let old = previous
                .as_ref()
                .filter(|_| same_target)
                .map(|row| row.get::<String, _>("model_json"));
            if configure_on(&mut tx, &lease.provider_id, entry, old.as_deref(), now).await? {
                summary.configured += 1;
            }
        }
        sqlx::query("INSERT INTO catalog_inventory(provider_id,upstream_id,target_fingerprint,observation_generation,observed_at,source_contract,fee_status,fee_reason_codes_json,fee_evidence_json,fee_expires_at,model_json,lifecycle,last_seen_run_id,last_complete_run_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,'active',?,?) ON CONFLICT(provider_id,upstream_id) DO UPDATE SET target_fingerprint=excluded.target_fingerprint,observation_generation=excluded.observation_generation,observed_at=excluded.observed_at,source_contract=excluded.source_contract,fee_status=excluded.fee_status,fee_reason_codes_json=excluded.fee_reason_codes_json,fee_evidence_json=excluded.fee_evidence_json,fee_expires_at=excluded.fee_expires_at,model_json=excluded.model_json,lifecycle='active',first_missing_at=NULL,last_missing_observation_at=NULL,missing_confirmations=0,last_seen_run_id=excluded.last_seen_run_id,last_complete_run_id=CASE WHEN excluded.last_complete_run_id IS NOT NULL THEN excluded.last_complete_run_id WHEN catalog_inventory.target_fingerprint=excluded.target_fingerprint THEN catalog_inventory.last_complete_run_id ELSE NULL END")
            .bind(&lease.provider_id).bind(&entry.model.id).bind(&lease.target_fingerprint).bind(lease.fence).bind(now).bind(observation.source_contract.code()).bind(fee.code()).bind(serde_json::to_string(&reasons)?).bind(serde_json::to_string(&entry.fee_evidence)?).bind(if fee==FeeStatus::Free{Some(now.saturating_add(DAY_MS))}else{None}).bind(serde_json::to_string(&entry.model)?).bind(&lease.run_id).bind(if complete{Some(lease.run_id.as_str())}else{None}).execute(&mut *tx).await?;
    }
    if complete {
        let missing=sqlx::query("SELECT upstream_id,first_missing_at,lifecycle FROM catalog_inventory WHERE provider_id=? AND target_fingerprint=? AND last_seen_run_id<>?")
            .bind(&lease.provider_id).bind(&lease.target_fingerprint).bind(&lease.run_id).fetch_all(&mut *tx).await?;
        for model in missing {
            if model.get::<String, _>("lifecycle") == "soft_offline" {
                continue;
            }
            let first: Option<i64> = model.get("first_missing_at");
            let first = first.filter(|first| *first <= now);
            let offline = first.is_some_and(|first| now.saturating_sub(first) >= DAY_MS);
            let lifecycle = if offline {
                "soft_offline"
            } else {
                "missing_pending"
            };
            if offline {
                summary.soft_offline += 1;
            } else {
                summary.missing_pending += 1;
            }
            sqlx::query("UPDATE catalog_inventory SET lifecycle=?,first_missing_at=?,last_missing_observation_at=?,missing_confirmations=? WHERE provider_id=? AND upstream_id=?")
                .bind(lifecycle).bind(first.unwrap_or(now)).bind(now).bind(if offline{2_i64}else{1_i64}).bind(&lease.provider_id).bind(model.get::<String,_>("upstream_id")).execute(&mut *tx).await?;
        }
    }
    let status = if complete {
        "complete"
    } else if observation.models.is_empty() {
        "empty"
    } else {
        "incomplete"
    };
    let warnings = serde_json::to_string(&observation.warning_codes)?;
    let counters = serde_json::to_string(&summary)?;
    let retry = if complete {
        None
    } else {
        Some(now.saturating_add(
            if observation.source_contract == SourceContract::CompatibleUnknown {
                DAY_MS
            } else {
                RETRY_MS
            },
        ))
    };
    sqlx::query("UPDATE catalog_scan_state SET lease_owner=NULL,lease_expires_at=NULL,status=?,failure_kind=?,warning_codes_json=?,counters_json=?,last_complete_success_at=CASE WHEN ? THEN ? ELSE last_complete_success_at END,next_due_at=CASE WHEN ? THEN ? ELSE next_due_at END,next_retry_at=?,failure_count=CASE WHEN ? THEN 0 ELSE failure_count+1 END,clock_anomaly=CASE WHEN ? THEN 0 ELSE clock_anomaly END WHERE provider_id=? AND run_id=? AND fence=?")
        .bind(status).bind(if complete{None}else{Some(status)}).bind(&warnings).bind(&counters).bind(complete).bind(now).bind(complete).bind(now.saturating_add(DAY_MS)).bind(retry).bind(complete).bind(complete).bind(&lease.provider_id).bind(&lease.run_id).bind(lease.fence).execute(&mut *tx).await?;
    sqlx::query("UPDATE catalog_scan_runs SET status=?,finished_at=?,completeness=?,source_contract=?,nonempty=?,raw_count=?,valid_count=?,duplicate_count=?,rejected_count=?,warning_codes_json=?,changes_json=? WHERE run_id=? AND finished_at IS NULL")
        .bind(status).bind(now).bind(observation.completeness.code()).bind(observation.source_contract.code()).bind(!observation.models.is_empty()).bind(observation.raw_count as i64).bind(observation.models.len() as i64).bind(observation.duplicate_count as i64).bind(observation.rejected_count as i64).bind(&warnings).bind(counters).bind(&lease.run_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE catalog_scan_state SET runtime_pending=1 WHERE provider_id=? AND run_id=? AND fence=?")
        .bind(&lease.provider_id).bind(&lease.run_id).bind(lease.fence).execute(&mut *tx).await?;
    let Some(final_now) = checkpoint() else {
        return Ok(ApplySummary::default());
    };
    if final_now < now || final_now >= lease.expires_at {
        return Ok(ApplySummary::default());
    }
    tx.commit().await?;
    Ok(summary)
}
