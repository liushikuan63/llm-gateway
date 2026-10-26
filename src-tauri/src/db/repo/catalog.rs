//! Durable catalog coordination. A lease is claimed before network I/O and every
//! terminal write rechecks its fencing token and captured editable revision.

mod apply;
mod eligibility;
mod inventory;
mod policy;
pub use apply::{apply_observation, apply_observation_checked, ApplySummary};
pub use eligibility::{free_eligibility, free_eligibility_checked, FreeEligibility};
pub use inventory::{list_inventory, InventoryEntry, InventoryPage};
pub(crate) use policy::reconcile_model_policies_on;

use crate::domain::{ModelRef, Provider};
use crate::model_catalog::observation::SourceContract;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};

pub const DAY_MS: i64 = 86_400_000;
pub const LEASE_MS: i64 = 120_000;
const RETRY_MS: i64 = 300_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanTrigger {
    Startup,
    Periodic,
    Manual,
}
impl ScanTrigger {
    fn code(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Periodic => "periodic",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanLease {
    pub provider_id: String,
    pub run_id: String,
    pub owner: String,
    pub fence: i64,
    pub captured_revision: i64,
    pub target_fingerprint: String,
    pub source_contract: SourceContract,
    pub started_at: i64,
    pub expires_at: i64,
    pub clock_epoch_reset: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", content = "lease", rename_all = "snake_case")]
pub enum ClaimOutcome {
    Acquired(ScanLease),
    NotDue,
    Busy,
    Conflict,
    Disabled,
    ClockAnomaly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanStatus {
    pub provider_id: String,
    pub status: String,
    pub last_attempt_at: Option<i64>,
    pub last_complete_success_at: Option<i64>,
    pub next_due_at: Option<i64>,
    pub next_retry_at: Option<i64>,
    pub failure_kind: Option<String>,
    pub warning_codes: Vec<String>,
    pub counters: ApplySummary,
    pub run_id: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub fence: i64,
    pub clock_anomaly: bool,
    pub runtime_pending: bool,
}

pub fn target_fingerprint(provider: &Provider) -> String {
    fingerprint(
        &provider.id,
        &serde_json::to_value(provider.dialect).expect("fixed dialect enum"),
        &provider.base_url,
        &provider.api_key_enc,
        provider.enabled,
        provider.runtime_id.as_deref(),
    )
}

fn fingerprint(
    id: &str,
    dialect: &serde_json::Value,
    base: &str,
    key_enc: &str,
    enabled: bool,
    runtime: Option<&str>,
) -> String {
    // Only the digest leaves this function; no key material appears in receipts.
    let bytes=serde_json::to_vec(&serde_json::json!({"contract_version":1,"id":id,"dialect":dialect,"base_url":base,"key_enc":key_enc,"enabled":enabled,"runtime_id":runtime})).expect("fixed JSON values");
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) async fn current_target(
    connection: &mut SqliteConnection,
    id: &str,
) -> Result<Option<String>> {
    let row = sqlx::query(
        "SELECT id,dialect,base_url,api_key_enc,enabled,runtime_id FROM providers WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut *connection)
    .await?;
    Ok(row.map(|r| {
        fingerprint(
            &r.get::<String, _>("id"),
            &serde_json::Value::String(r.get("dialect")),
            &r.get::<String, _>("base_url"),
            &r.get::<String, _>("api_key_enc"),
            r.get::<i64, _>("enabled") == 1,
            r.get::<Option<String>, _>("runtime_id").as_deref(),
        )
    }))
}

async fn revision(connection: &mut SqliteConnection, id: &str) -> Result<i64> {
    sqlx::query_scalar("SELECT revision FROM catalog_provider_revision WHERE provider_id=?")
        .bind(id)
        .fetch_optional(&mut *connection)
        .await?
        .ok_or_else(|| anyhow!("catalog revision is missing"))
}

pub async fn claim_scan(
    pool: &SqlitePool,
    provider: &Provider,
    owner: &str,
    now: i64,
    trigger: ScanTrigger,
) -> Result<ClaimOutcome> {
    claim_scan_checked(pool, provider, owner, || now, trigger).await
}

/// Refresh the clock after acquiring SQLite's write lock, rather than dating a
/// new lease from a timestamp captured before an arbitrarily long await.
pub async fn claim_scan_checked(
    pool: &SqlitePool,
    provider: &Provider,
    owner: &str,
    clock: impl Fn() -> i64,
    trigger: ScanTrigger,
) -> Result<ClaimOutcome> {
    let now = clock();
    if now < 0 || owner.is_empty() || owner.len() > 128 {
        return Err(anyhow!("invalid catalog clock or owner"));
    }
    if !provider.enabled {
        return Ok(ClaimOutcome::Disabled);
    }
    let expected = target_fingerprint(provider);
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    if current_target(&mut tx, &provider.id).await?.as_deref() != Some(&expected) {
        return Ok(ClaimOutcome::Conflict);
    }
    let captured_revision = revision(&mut tx, &provider.id).await?;
    sqlx::query("INSERT OR IGNORE INTO catalog_scan_state(provider_id) VALUES(?)")
        .bind(&provider.id)
        .execute(&mut *tx)
        .await?;
    let state = sqlx::query("SELECT * FROM catalog_scan_state WHERE provider_id=?")
        .bind(&provider.id)
        .fetch_one(&mut *tx)
        .await?;
    let now = clock();
    if now < 0 {
        return Err(anyhow!("invalid catalog clock"));
    }
    let previous_owner: Option<String> = state.get("lease_owner");
    let expiry: Option<i64> = state.get("lease_expires_at");
    if previous_owner.is_some() && expiry.is_none() {
        return Err(anyhow!("catalog lease expiry is missing"));
    }
    let same_target = state.get::<String, _>("target_fingerprint") == expected;
    let recovered = state.get::<String, _>("status") == "clock_recovered";
    let last_attempt: Option<i64> = state.get("last_attempt_at");
    if same_target && !recovered && last_attempt.is_some_and(|previous| previous > now) {
        sqlx::query("UPDATE catalog_scan_state SET clock_anomaly=1 WHERE provider_id=?")
            .bind(&provider.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(ClaimOutcome::ClockAnomaly);
    }
    if previous_owner.is_some() && expiry.is_some_and(|expires| expires > now) {
        return Ok(ClaimOutcome::Busy);
    }
    if same_target {
        let retry: Option<i64> = state.get("next_retry_at");
        if retry.is_some_and(|retry| retry > now) {
            return Ok(ClaimOutcome::NotDue);
        }
        let due: Option<i64> = state.get("next_due_at");
        if trigger != ScanTrigger::Manual && due.is_some_and(|due| due > now) {
            return Ok(ClaimOutcome::NotDue);
        }
    }
    if let Some(old_run) = state
        .get::<Option<String>, _>("run_id")
        .filter(|_| previous_owner.is_some())
    {
        sqlx::query("UPDATE catalog_scan_runs SET status='lease_expired',finished_at=?,failure_kind='lease_expired' WHERE run_id=? AND finished_at IS NULL")
            .bind(now).bind(old_run).execute(&mut *tx).await?;
    }
    let fence = state
        .get::<i64, _>("fence")
        .checked_add(1)
        .ok_or_else(|| anyhow!("catalog fence exhausted"))?;
    let expires_at = now
        .checked_add(LEASE_MS)
        .ok_or_else(|| anyhow!("catalog clock overflow"))?;
    let lease = ScanLease {
        provider_id: provider.id.clone(),
        run_id: uuid::Uuid::new_v4().to_string(),
        owner: owner.into(),
        fence,
        captured_revision,
        target_fingerprint: expected,
        source_contract: SourceContract::for_provider(provider),
        started_at: now,
        expires_at,
        clock_epoch_reset: recovered,
    };
    sqlx::query("UPDATE catalog_scan_state SET target_fingerprint=?,run_id=?,fence=?,lease_owner=?,lease_started_at=?,lease_expires_at=?,last_attempt_at=?,status='running',failure_kind=NULL,warning_codes_json='[]',next_retry_at=NULL,next_due_at=CASE WHEN ? THEN next_due_at ELSE NULL END,last_complete_success_at=CASE WHEN ? THEN last_complete_success_at ELSE NULL END,failure_count=CASE WHEN ? THEN failure_count ELSE 0 END WHERE provider_id=?")
        .bind(&lease.target_fingerprint).bind(&lease.run_id).bind(fence).bind(owner).bind(now).bind(expires_at).bind(now).bind(same_target).bind(same_target).bind(same_target).bind(&lease.provider_id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO catalog_scan_runs(run_id,provider_id,target_fingerprint,captured_revision,fence,lease_owner,trigger_kind,started_at,status) VALUES(?,?,?,?,?,?,?,?,'running')")
        .bind(&lease.run_id).bind(&lease.provider_id).bind(&lease.target_fingerprint).bind(captured_revision).bind(fence).bind(owner).bind(trigger.code()).bind(now).execute(&mut *tx).await?;
    let final_now = clock();
    if final_now < now || final_now >= expires_at {
        return Ok(ClaimOutcome::ClockAnomaly);
    }
    tx.commit().await?;
    Ok(ClaimOutcome::Acquired(lease))
}

async fn owns(connection: &mut SqliteConnection, lease: &ScanLease, now: i64) -> Result<bool> {
    Ok(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM catalog_scan_state WHERE provider_id=? AND run_id=? AND fence=? AND lease_owner=? AND target_fingerprint=? AND lease_expires_at>? AND status='running'")
        .bind(&lease.provider_id).bind(&lease.run_id).bind(lease.fence).bind(&lease.owner).bind(&lease.target_fingerprint).bind(now).fetch_one(&mut *connection).await?==1)
}

pub async fn fail_scan(
    pool: &SqlitePool,
    lease: &ScanLease,
    kind: &str,
    retry_after_at: Option<i64>,
    now: i64,
) -> Result<bool> {
    fail_scan_checked(pool, lease, kind, retry_after_at, || now).await
}

pub async fn fail_scan_checked(
    pool: &SqlitePool,
    lease: &ScanLease,
    kind: &str,
    retry_after_at: Option<i64>,
    clock: impl Fn() -> i64,
) -> Result<bool> {
    // Fixed classifications only; provider errors, bodies and keys never enter SQL.
    const KINDS: &[&str] = &[
        "auth_rejected",
        "rate_limited",
        "temporary_http",
        "http_rejected",
        "timeout",
        "unavailable",
        "readonly_adapter_required",
        "saved_key_unavailable",
        "invalid_target",
        "invalid_key",
        "client_unavailable",
        "invalid_or_oversized_body",
        "invalid_catalog_shape",
        "catalog_row_limit",
        "compatible_read_failed",
        "cancelled",
        "clock_changed",
        "persistence_failed",
    ];
    let kind = if KINDS.contains(&kind) {
        kind
    } else {
        "observation_failed"
    };
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let now = clock();
    if !owns(&mut tx, lease, now).await? {
        return Ok(false);
    }
    let failures: i64 =
        sqlx::query_scalar("SELECT failure_count FROM catalog_scan_state WHERE provider_id=?")
            .bind(&lease.provider_id)
            .fetch_one(&mut *tx)
            .await?;
    let backoff = RETRY_MS
        .saturating_mul(1_i64 << u32::try_from(failures.min(8)).unwrap_or(8))
        .min(DAY_MS);
    let mut retry = now.saturating_add(backoff);
    if matches!(kind, "auth_rejected" | "readonly_adapter_required") {
        retry = i64::MAX;
    }
    if let Some(server) = retry_after_at {
        retry = retry.max(server);
    }
    sqlx::query("UPDATE catalog_scan_state SET lease_owner=NULL,lease_expires_at=NULL,status=?,failure_kind=?,next_retry_at=?,failure_count=failure_count+1,clock_anomaly=CASE WHEN ?='clock_changed' THEN 1 ELSE clock_anomaly END WHERE provider_id=? AND run_id=? AND fence=?")
        .bind(kind).bind(kind).bind(retry).bind(kind).bind(&lease.provider_id).bind(&lease.run_id).bind(lease.fence).execute(&mut *tx).await?;
    sqlx::query("UPDATE catalog_scan_runs SET status=?,failure_kind=?,finished_at=? WHERE run_id=? AND finished_at IS NULL")
        .bind(kind).bind(kind).bind(now).bind(&lease.run_id).execute(&mut *tx).await?;
    let final_now = clock();
    if final_now < now || final_now >= lease.expires_at {
        return Ok(false);
    }
    tx.commit().await?;
    Ok(true)
}

/// Only the manager may call this after a real monotonic quiet interval. The
/// source/revision history stays intact; old owners are fenced and pending
/// missing observations are reset by the next accepted observation.
pub async fn recover_clock_after_quiet_interval(
    pool: &SqlitePool,
    provider_id: &str,
    expected_fence: i64,
    now: i64,
    quiet_ms: i64,
) -> Result<bool> {
    if quiet_ms < LEASE_MS {
        return Ok(false);
    }
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("UPDATE catalog_scan_runs SET status='clock_superseded',failure_kind='clock_changed',finished_at=? WHERE run_id=(SELECT run_id FROM catalog_scan_state WHERE provider_id=? AND fence=? AND clock_anomaly=1) AND finished_at IS NULL")
        .bind(now).bind(provider_id).bind(expected_fence).execute(&mut *tx).await?;
    let changed=sqlx::query("UPDATE catalog_scan_state SET fence=fence+1,lease_owner=NULL,lease_expires_at=NULL,status='clock_recovered',clock_anomaly=1,next_due_at=?,next_retry_at=NULL WHERE provider_id=? AND fence=? AND clock_anomaly=1")
        .bind(now).bind(provider_id).bind(expected_fence).execute(&mut *tx).await?.rows_affected()==1;
    tx.commit().await?;
    Ok(changed)
}

pub async fn list_status(pool: &SqlitePool) -> Result<Vec<ScanStatus>> {
    let rows = sqlx::query("SELECT * FROM catalog_scan_state ORDER BY provider_id")
        .fetch_all(pool)
        .await?;
    rows.into_iter()
        .map(|r| {
            Ok(ScanStatus {
                provider_id: r.get("provider_id"),
                status: r.get("status"),
                last_attempt_at: r.get("last_attempt_at"),
                last_complete_success_at: r.get("last_complete_success_at"),
                next_due_at: r.get("next_due_at"),
                next_retry_at: r.get("next_retry_at"),
                failure_kind: r.get("failure_kind"),
                warning_codes: serde_json::from_str(&r.get::<String, _>("warning_codes_json"))?,
                counters: serde_json::from_str(&r.get::<String, _>("counters_json"))?,
                run_id: r.get("run_id"),
                lease_expires_at: r.get("lease_expires_at"),
                fence: r.get("fence"),
                clock_anomaly: r.get::<i64, _>("clock_anomaly") == 1,
                runtime_pending: r.get::<i64, _>("runtime_pending") == 1,
            })
        })
        .collect()
}

/// A reload acknowledges only the captured generation. A later observation
/// remains pending even when it commits while a runtime reload is in flight.
pub async fn acknowledge_runtime(
    pool: &SqlitePool,
    provider_id: &str,
    run_id: &str,
    fence: i64,
) -> Result<bool> {
    Ok(sqlx::query("UPDATE catalog_scan_state SET runtime_pending=0 WHERE provider_id=? AND run_id=? AND fence=? AND runtime_pending=1")
        .bind(provider_id).bind(run_id).bind(fence).execute(pool).await?.rows_affected()==1)
}

/// Automatic restrictions are separate from the legacy manual-mode policy.
pub async fn filter_routable_models(
    pool: &SqlitePool,
    provider_id: &str,
    expected_target: Option<&str>,
    models: Vec<ModelRef>,
    now: i64,
) -> Result<Vec<ModelRef>> {
    let mut connection = pool.acquire().await?;
    let actual_target = current_target(&mut connection, provider_id).await?;
    let rows=sqlx::query("SELECT p.alias,p.upstream_id,p.managed_by,p.user_override,i.target_fingerprint,i.source_contract,i.fee_status,i.observed_at,i.fee_expires_at,i.lifecycle,i.model_json FROM catalog_model_policy p LEFT JOIN catalog_inventory i ON i.provider_id=p.provider_id AND i.upstream_id=p.upstream_id WHERE p.provider_id=?")
        .bind(provider_id).fetch_all(&mut *connection).await?;
    let policies: std::collections::HashMap<String, _> =
        rows.into_iter().map(|r| (r.get("alias"), r)).collect();
    Ok(models
        .into_iter()
        .filter(|model| {
            if !model.enabled {
                return false;
            }
            let Some(policy) = policies.get(&model.alias) else {
                return true;
            };
            if policy.get::<String, _>("managed_by") != "catalog"
                || policy.get::<String, _>("upstream_id") != model.upstream
            {
                return true;
            }
            if matches!(
                policy.get::<String, _>("user_override").as_str(),
                "disabled" | "removed"
            ) {
                return false;
            }
            let fingerprint: Option<String> = policy.get("target_fingerprint");
            let observed: Option<i64> = policy.get("observed_at");
            let expires: Option<i64> = policy.get("fee_expires_at");
            let lifecycle: Option<String> = policy.get("lifecycle");
            let metadata: Option<String> = policy.get("model_json");
            let qualified_metadata = metadata
                .and_then(|raw| {
                    serde_json::from_str::<crate::model_catalog::DiscoveredModel>(&raw).ok()
                })
                .is_some_and(|m| {
                    m.context_source == crate::model_catalog::ContextSource::Provider
                        && m.context_window > 0
                        && m.model_type.is_some()
                });
            actual_target.is_some()
                && fingerprint == actual_target
                && expected_target
                    .map_or(true, |expected| actual_target.as_deref() == Some(expected))
                && policy
                    .get::<Option<String>, _>("source_contract")
                    .as_deref()
                    == Some("openrouter_full_v1")
                && policy.get::<Option<String>, _>("fee_status").as_deref() == Some("free")
                && observed.is_some_and(|observed| observed <= now)
                && expires.is_some_and(|expires| expires > now)
                && matches!(lifecycle.as_deref(), Some("active" | "missing_pending"))
                && qualified_metadata
        })
        .collect())
}
