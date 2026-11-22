//! CLIENT v2 journal operations. Retain evidence without a total quota.
//! A successful intent commit is a prerequisite to a protected dispatch, not
//! evidence of sending, usage, or cost. Review only acknowledges a gate record.
use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, SqliteConnection, Transaction};

use crate::db::{client_usage_journal_schema as schema, Db};

const REVIEW: &str = include_str!("../client_usage_review_v2.sql");

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttemptKind {
    Buffered,
    Stream,
    Auxiliary,
    Probe,
    Passthrough,
    Compaction,
    Summary,
    AgentIntent,
    NativeExact,
}

impl AttemptKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Buffered => "buffered",
            Self::Stream => "stream",
            Self::Auxiliary => "auxiliary",
            Self::Probe => "probe",
            Self::Passthrough => "passthrough",
            Self::Compaction => "compaction",
            Self::Summary => "summary",
            Self::AgentIntent => "agent_intent",
            Self::NativeExact => "native_exact",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentGateReason {
    Quota,
    Concurrency,
}

impl AgentGateReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::Concurrency => "concurrency",
        }
    }
}

fn unavailable(_: sqlx::Error) -> anyhow::Error {
    anyhow::anyhow!("client_usage_journal_storage_unavailable")
}

fn valid_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

async fn contract(connection: &mut SqliteConnection) -> anyhow::Result<schema::ContractMeta> {
    schema::inspect_conn(connection)
        .await?
        .ok_or_else(|| anyhow::anyhow!("client_usage_journal_contract_unavailable"))
}

async fn finish<T>(
    transaction: Transaction<'_, Sqlite>,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    match result {
        Ok(value) => {
            transaction
                .commit()
                .await
                .map_err(|_| anyhow::anyhow!("client_usage_journal_commit_unknown"))?;
            Ok(value)
        }
        Err(error) => {
            transaction
                .rollback()
                .await
                .map_err(|_| anyhow::anyhow!("client_usage_journal_rollback_unknown"))?;
            Err(error)
        }
    }
}

/// Await this commit before dispatch. No count, byte quota or retention cleanup.
pub async fn record_intent(db: &Db, scope_id: &str, kind: AttemptKind) -> anyhow::Result<String> {
    anyhow::ensure!(valid_uuid(scope_id), "client_usage_journal_invalid_scope");
    let id = uuid::Uuid::new_v4().to_string();
    let mut transaction = db
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(unavailable)?;
    let result = async {
        contract(&mut transaction).await?;
        sqlx::query("INSERT INTO usage_attempt_journal_v2(attempt_id,scope_id,kind,created_at) VALUES(?,?,?,?)")
            .bind(&id).bind(scope_id).bind(kind.as_str()).bind(chrono::Utc::now().timestamp())
            .execute(&mut *transaction).await.map_err(unavailable)?;
        Ok(id)
    }.await;
    finish(transaction, result).await
}

/// Only a caller that has not dispatched may persist this classification.
pub async fn mark_agent_gate(
    db: &Db,
    attempt_id: &str,
    reason: AgentGateReason,
) -> anyhow::Result<()> {
    anyhow::ensure!(valid_uuid(attempt_id), "client_usage_journal_invalid_id");
    let mut transaction = db
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(unavailable)?;
    let result = async {
        contract(&mut transaction).await?;
        let changed = sqlx::query("UPDATE usage_attempt_journal_v2 SET known_not_sent=1,agent_gate_reason=? WHERE attempt_id=? AND kind='agent_intent' AND known_not_sent=0 AND reviewed_at IS NULL AND audit_request_id IS NULL AND background_scope_id IS NULL")
            .bind(reason.as_str()).bind(attempt_id).execute(&mut *transaction).await.map_err(unavailable)?;
        anyhow::ensure!(changed.rows_affected() == 1, "client_usage_journal_state_conflict");
        Ok(())
    }.await;
    finish(transaction, result).await
}

/// Join the exact request's intent without committing the caller's audit transaction.
pub(super) async fn link_native_audit_in_conn(
    connection: &mut SqliteConnection,
    scope_id: &str,
    attempt_id: &str,
    request_id: i64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_uuid(scope_id) && valid_uuid(attempt_id) && request_id > 0,
        "client_usage_journal_invalid_link"
    );
    contract(connection).await?;
    let changed = sqlx::query(
        r#"UPDATE usage_attempt_journal_v2 SET audit_request_id=?
           WHERE attempt_id=? AND scope_id=? AND kind='native_exact'
           AND audit_request_id IS NULL AND background_scope_id IS NULL
           AND known_not_sent=0 AND reviewed_at IS NULL
           AND EXISTS(SELECT 1 FROM requests WHERE id=?)"#,
    )
    .bind(request_id)
    .bind(attempt_id)
    .bind(scope_id)
    .bind(request_id)
    .execute(connection)
    .await
    .map_err(unavailable)?;
    anyhow::ensure!(
        changed.rows_affected() == 1,
        "client_usage_journal_state_conflict"
    );
    Ok(())
}

/// Links one intent to an existing audit row without dropping its evidence.
pub async fn link_audit(db: &Db, attempt_id: &str, request_id: i64) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_uuid(attempt_id) && request_id > 0,
        "client_usage_journal_invalid_link"
    );
    let mut transaction = db
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(unavailable)?;
    let result = async {
        contract(&mut transaction).await?;
        let changed = sqlx::query("UPDATE usage_attempt_journal_v2 SET audit_request_id=? WHERE attempt_id=? AND audit_request_id IS NULL AND background_scope_id IS NULL AND known_not_sent=0 AND reviewed_at IS NULL AND EXISTS(SELECT 1 FROM requests WHERE id=?)")
            .bind(request_id).bind(attempt_id).bind(request_id).execute(&mut *transaction).await.map_err(unavailable)?;
        anyhow::ensure!(changed.rows_affected() == 1, "client_usage_journal_state_conflict");
        Ok(())
    }.await;
    finish(transaction, result).await
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReviewItem {
    pub attempt_id: String,
    pub scope_id: String,
    pub created_at: i64,
    pub gate_reason: AgentGateReason,
}

/// A page budget bounds one IPC response, never the retained journal.
#[derive(Debug, Serialize)]
pub struct ReviewPage {
    pub schema_version: u8,
    pub items: Vec<ReviewItem>,
    pub next_after: Option<String>,
}

pub async fn list_reviewable(db: &Db, after: Option<&str>) -> anyhow::Result<ReviewPage> {
    anyhow::ensure!(
        after.map_or(true, valid_uuid),
        "client_usage_journal_invalid_cursor"
    );
    let mut transaction = db.pool().begin().await.map_err(unavailable)?;
    let result = async {
        let meta = contract(&mut transaction).await?;
        let rows = sqlx::query("SELECT j.attempt_id,j.scope_id,j.created_at,j.agent_gate_reason,v.entry_generation,v.revision FROM usage_attempt_journal_v2 j JOIN usage_attempt_versions_v2 v ON v.attempt_id=j.attempt_id WHERE j.kind='agent_intent' AND j.known_not_sent=1 AND j.reviewed_at IS NULL AND j.audit_request_id IS NULL AND j.background_scope_id IS NULL AND j.agent_gate_reason IN ('quota','concurrency') AND v.present=1 AND (? IS NULL OR j.attempt_id>?) ORDER BY j.attempt_id LIMIT 51")
            .bind(after).bind(after).fetch_all(&mut *transaction).await.map_err(unavailable)?;
        let has_more = rows.len() > 50;
        let mut items = Vec::with_capacity(rows.len().min(50));
        for row in rows.into_iter().take(50) {
            let id: String = row.try_get("attempt_id").map_err(unavailable)?;
            let scope: String = row.try_get("scope_id").map_err(unavailable)?;
            let created: i64 = row.try_get("created_at").map_err(unavailable)?;
            let generation: i64 = row.try_get("entry_generation").map_err(unavailable)?;
            let revision: i64 = row.try_get("revision").map_err(unavailable)?;
            anyhow::ensure!(valid_uuid(&id) && valid_uuid(&scope) && created >= 0 && chrono::DateTime::from_timestamp(created, 0).is_some() && generation > 0 && generation <= meta.last_entry_generation && revision > 0, "client_usage_journal_invalid_row");
            let reason: String = row.try_get("agent_gate_reason").map_err(unavailable)?;
            let gate_reason = match reason.as_str() {
                "quota" => AgentGateReason::Quota,
                "concurrency" => AgentGateReason::Concurrency,
                _ => anyhow::bail!("client_usage_journal_invalid_row"),
            };
            items.push(ReviewItem { attempt_id: id, scope_id: scope, created_at: created, gate_reason });
        }
        let next_after = has_more.then(|| items.last().expect("nonempty full page").attempt_id.clone());
        Ok(ReviewPage { schema_version: 2, items, next_after })
    }.await;
    finish(transaction, result).await
}

#[derive(Debug)]
struct Snapshot {
    item: ReviewItem,
    generation: i64,
    revision: i64,
}

/// Opaque, non-Clone, non-Deserialize native confirmation. The caller may
/// display items but cannot supply or change snapshot versions. Confirmation
/// consumes this value even on a conflict or unknown commit; obtain a new read
/// and explicit confirmation instead of replaying a failed token.
#[derive(Debug)]
pub struct PendingReview {
    runtime: Arc<uuid::Uuid>,
    meta: schema::ContractMeta,
    snapshots: Vec<Snapshot>,
}

impl PendingReview {
    pub fn items(&self) -> Vec<ReviewItem> {
        self.snapshots
            .iter()
            .map(|snapshot| snapshot.item.clone())
            .collect()
    }
}

/// Captures only explicitly selected IDs in a single read snapshot. No write
/// lock is kept across the confirmation window; unrelated new intents may grow.
pub async fn prepare_review(db: &Db, attempt_ids: &[String]) -> anyhow::Result<PendingReview> {
    let mut ids = HashSet::new();
    anyhow::ensure!(
        !attempt_ids.is_empty()
            && attempt_ids
                .iter()
                .all(|id| valid_uuid(id) && ids.insert(id)),
        "client_usage_journal_invalid_selection"
    );
    let mut transaction = db.pool().begin().await.map_err(unavailable)?;
    let result = async {
        let meta = contract(&mut transaction).await?;
        let mut snapshots = Vec::with_capacity(attempt_ids.len());
        for id in attempt_ids {
            let row = sqlx::query("SELECT j.attempt_id,j.scope_id,j.created_at,j.agent_gate_reason,v.entry_generation,v.revision FROM usage_attempt_journal_v2 j JOIN usage_attempt_versions_v2 v ON v.attempt_id=j.attempt_id WHERE j.attempt_id=? AND j.kind='agent_intent' AND j.known_not_sent=1 AND j.reviewed_at IS NULL AND j.audit_request_id IS NULL AND j.background_scope_id IS NULL AND j.agent_gate_reason IN ('quota','concurrency') AND v.present=1")
                .bind(id).fetch_optional(&mut *transaction).await.map_err(unavailable)?
                .ok_or_else(|| anyhow::anyhow!("client_usage_journal_state_conflict"))?;
            let scope: String = row.try_get("scope_id").map_err(unavailable)?;
            let created: i64 = row.try_get("created_at").map_err(unavailable)?;
            let generation: i64 = row.try_get("entry_generation").map_err(unavailable)?;
            let revision: i64 = row.try_get("revision").map_err(unavailable)?;
            anyhow::ensure!(valid_uuid(&scope) && created >= 0 && chrono::DateTime::from_timestamp(created, 0).is_some() && generation > 0 && generation <= meta.last_entry_generation && revision > 0, "client_usage_journal_invalid_row");
            let reason: String = row.try_get("agent_gate_reason").map_err(unavailable)?;
            let gate_reason = match reason.as_str() {
                "quota" => AgentGateReason::Quota,
                "concurrency" => AgentGateReason::Concurrency,
                _ => anyhow::bail!("client_usage_journal_invalid_row"),
            };
            snapshots.push(Snapshot {
                item: ReviewItem { attempt_id: id.clone(), scope_id: scope, created_at: created, gate_reason },
                generation, revision,
            });
        }
        Ok(PendingReview { runtime: Arc::clone(&db.client_usage_binding), meta, snapshots })
    }.await;
    finish(transaction, result).await
}

/// Atomically acknowledges all displayed gate records. This does not classify
/// an unknown send as not-sent, erase evidence, or assert zero usage/cost.
pub async fn confirm_review(db: &Db, pending: PendingReview) -> anyhow::Result<usize> {
    anyhow::ensure!(
        Arc::ptr_eq(&db.client_usage_binding, &pending.runtime),
        "client_usage_journal_runtime_changed"
    );
    let mut transaction = db
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(unavailable)?;
    let result = async {
        let meta = contract(&mut transaction).await?;
        anyhow::ensure!(meta.db_generation == pending.meta.db_generation && meta.activated_at == pending.meta.activated_at, "client_usage_journal_database_changed");
        let stamp = chrono::Utc::now().timestamp();
        for snapshot in &pending.snapshots {
            let item = &snapshot.item;
            let changed = sqlx::query(REVIEW).bind(stamp).bind(&item.attempt_id).bind(&item.scope_id).bind(item.created_at)
                .bind(item.gate_reason.as_str()).bind(snapshot.generation).bind(snapshot.revision).bind(&pending.meta.db_generation)
                .execute(&mut *transaction).await.map_err(unavailable)?;
            anyhow::ensure!(changed.rows_affected() == 1, "client_usage_journal_state_conflict");
            // RETURNING runs before AFTER triggers. SELECT observes their actual
            // revision increment; a missing increment aborts the entire batch.
            let version: (i64,i64,i64) = sqlx::query_as("SELECT entry_generation,revision,present FROM usage_attempt_versions_v2 WHERE attempt_id=?")
                .bind(&item.attempt_id).fetch_one(&mut *transaction).await.map_err(unavailable)?;
            anyhow::ensure!(version == (snapshot.generation, snapshot.revision.checked_add(1).ok_or_else(|| anyhow::anyhow!("client_usage_journal_revision_overflow"))?, 1), "client_usage_journal_version_write_failed");
        }
        Ok(pending.snapshots.len())
    }.await;
    finish(transaction, result).await
}
