//! Dedicated v2 SQL boundary. Only the final CASE-selected attempts payload is
//! exposed to SQLx; SQLite may still scan/materialize data internally. Neither
//! these limits nor row streaming claim a disk, SQLite-cache or RSS hard bound.
use crate::audit::native_usage::UsageProjection;
use crate::audit::v2::{
    self, AttemptsBody, AuditPageV2, BodyReason, BodyState, CoverageBuilder, JournalCountsV2,
    ModelUsageV2, ProviderUsageV2, UsageStatsV2, MAX_PAYLOAD_BYTES, MAX_RECORD_BYTES,
};
use crate::audit::{AuditRow, Bind, RequestFilter};
use crate::error::{GatewayError, Result};
use futures_util::TryStreamExt;
use sqlx::{Row, SqlitePool};
use std::collections::BTreeMap;

pub const RAW_EXPORT_UNAVAILABLE: &str = "审计原始导出包含超限或不可读取正文，请缩小筛选范围后重试";

macro_rules! bind_filter {
    ($q:expr, $binds:expr) => {{
        let mut q = $q;
        for b in $binds {
            q = match b {
                Bind::I64(v) => q.bind(*v),
                Bind::F64(v) => q.bind(*v),
                Bind::Text(v) => q.bind(v.clone()),
            };
        }
        q
    }};
}

fn bounded_sql(where_sql: &str, paged: bool) -> String {
    let source = if paged {
        super::AUDIT_COLUMNS
    } else {
        "id, ts, status, fallback_attempts, routed_provider, routed_model, attempts_json"
    };
    // The safe NULL is only a metadata-mapping placeholder. It must never feed
    // the evidence classifier: v2_body_type/v2_raw carry that separate decision.
    let metadata = if paged {
        super::AUDIT_COLUMNS.replace("attempts_json", "NULL AS attempts_json")
    } else {
        "id, ts, status, fallback_attempts, routed_provider, routed_model".to_owned()
    };
    let page = if paged { " LIMIT ? OFFSET ?" } else { "" };
    format!(
        "WITH v2_window AS (
        SELECT {source}, typeof(attempts_json) AS v2_body_type,
            length(CAST(attempts_json AS BLOB)) AS v2_body_bytes
        FROM requests{where_sql} ORDER BY ts DESC, id DESC{page}
    ), v2_budget AS (
        SELECT *, SUM(CASE WHEN v2_body_type='text' AND v2_body_bytes<={MAX_RECORD_BYTES}
            THEN v2_body_bytes ELSE 0 END) OVER (
            ORDER BY ts DESC, id DESC ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
        ) AS v2_running_bytes FROM v2_window
    ) SELECT {metadata}, v2_body_type, v2_body_bytes, v2_running_bytes,
        CASE WHEN v2_body_type='text' AND v2_body_bytes<={MAX_RECORD_BYTES}
            AND v2_running_bytes<={MAX_PAYLOAD_BYTES} THEN attempts_json ELSE NULL END AS v2_raw
        FROM v2_budget ORDER BY ts DESC, id DESC"
    )
}

fn unavailable_body(row: &sqlx::sqlite::SqliteRow) -> Result<Option<AttemptsBody>> {
    let kind: String = row.try_get("v2_body_type")?;
    let bytes = row
        .try_get::<Option<i64>, _>("v2_body_bytes")?
        .and_then(|bytes| u64::try_from(bytes).ok());
    let (state, reason) = if kind == "null" {
        (BodyState::Absent, None)
    } else if kind != "text" {
        (BodyState::Omitted, Some(BodyReason::UnsupportedType))
    } else if bytes.map_or(true, |bytes| bytes > MAX_RECORD_BYTES as u64) {
        (BodyState::Omitted, Some(BodyReason::RecordLimit))
    } else if row.try_get::<i64, _>("v2_running_bytes")? > MAX_PAYLOAD_BYTES as i64 {
        (BodyState::Omitted, Some(BodyReason::PayloadBudget))
    } else {
        return Ok(None);
    };
    Ok(Some(AttemptsBody {
        state,
        reason,
        byte_length: bytes,
    }))
}

fn read_raw(row: &sqlx::sqlite::SqliteRow) -> std::result::Result<String, AttemptsBody> {
    row.try_get::<Option<String>, _>("v2_raw")
        .ok()
        .flatten()
        .ok_or_else(|| AttemptsBody {
            state: BodyState::Omitted,
            reason: Some(BodyReason::DecodeError),
            byte_length: row
                .try_get::<Option<i64>, _>("v2_body_bytes")
                .ok()
                .flatten()
                .and_then(|bytes| u64::try_from(bytes).ok()),
        })
}

pub async fn query_page(pool: &SqlitePool, filter: &RequestFilter) -> Result<AuditPageV2> {
    let (where_sql, binds) = crate::audit::build_where(filter);
    let mut tx = pool.begin().await?;
    let count_sql = format!("SELECT COUNT(*) FROM requests{where_sql}");
    let total = bind_filter!(sqlx::query_scalar::<_, i64>(&count_sql), &binds)
        .fetch_one(&mut *tx)
        .await? as u64;
    let sql = bounded_sql(&where_sql, true);
    let raw_rows = bind_filter!(sqlx::query(&sql), &binds)
        .bind(i64::from(filter.page_size()))
        .bind(i64::from(filter.page_offset()))
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    let mut coverage = CoverageBuilder::default();
    let mut rows = Vec::with_capacity(raw_rows.len());
    for row in raw_rows {
        let metadata = super::audit_row_from(&row);
        let (mut projected, projection) = if let Some(body) = unavailable_body(&row)? {
            (v2::AuditRowV2::unavailable(metadata, body), None)
        } else {
            match read_raw(&row) {
                Ok(raw) => v2::project(metadata, &raw, raw.len() as u64),
                Err(body) => (v2::AuditRowV2::unavailable(metadata, body), None),
            }
        };
        super::request_timing::normalize_generation_reports(&mut projected.metadata.attempts);
        coverage.add(&projected.attempts_body, projection.as_ref());
        rows.push(projected);
    }
    Ok(AuditPageV2 {
        schema_version: 2,
        truncated: u64::from(filter.page_offset()) + (rows.len() as u64) < total,
        rows,
        total,
        limits: Default::default(),
        usage_coverage: coverage.finish(),
    })
}

/// Full filter scope, ignoring pagination. Only attempts payload within the
/// common 8MiB budget enters SQLx. Remaining matches still count as unknown.
pub async fn usage_stats(pool: &SqlitePool, filter: &RequestFilter) -> Result<UsageStatsV2> {
    let (where_sql, binds) = crate::audit::build_where(filter);
    let mut tx = pool.begin().await?;
    let sql = bounded_sql(&where_sql, false);
    let mut coverage = CoverageBuilder::default();
    let mut providers: BTreeMap<Option<String>, CoverageBuilder> = BTreeMap::new();
    let mut models: BTreeMap<(Option<String>, Option<String>), CoverageBuilder> = BTreeMap::new();
    let mut request_count = 0_u64;
    let mut successful_requests = 0_u64;
    let mut fallback_requests = 0_u64;
    {
        let mut rows = bind_filter!(sqlx::query(&sql), &binds).fetch(&mut *tx);
        while let Some(row) = rows.try_next().await? {
            request_count = request_count
                .checked_add(1)
                .ok_or_else(|| GatewayError::Protocol("审计统计计数超出范围".into()))?;
            if row
                .try_get::<Option<i64>, _>("status")?
                .map_or(true, |status| status < 400)
            {
                successful_requests += 1;
            }
            if row.try_get::<i64, _>("fallback_attempts")? > 0 {
                fallback_requests += 1;
            }
            let (body, projection): (AttemptsBody, Option<UsageProjection>) =
                if let Some(body) = unavailable_body(&row)? {
                    (body, None)
                } else {
                    match read_raw(&row) {
                        Ok(raw) => {
                            let (body, projection, _) =
                                v2::read_evidence(&raw, raw.len() as u64, row.try_get("ts")?);
                            (body, projection)
                        }
                        Err(body) => (body, None),
                    }
                };
            coverage.add(&body, projection.as_ref());
            let provider: Option<String> = row.try_get("routed_provider")?;
            let model: Option<String> = row.try_get("routed_model")?;
            providers
                .entry(provider.clone())
                .or_default()
                .add(&body, projection.as_ref());
            models
                .entry((provider, model))
                .or_default()
                .add(&body, projection.as_ref());
        }
    }
    // Match the gate evidence used for review, regardless of review status.
    // The persisted flag alone does not establish that an attempt was unsent.
    let journal = sqlx::query("WITH scoped AS (
        SELECT j.audit_request_id, j.reviewed_at,
            CASE WHEN j.kind='agent_intent' AND j.known_not_sent=1
                AND j.agent_gate_reason IN ('quota','concurrency')
                AND j.audit_request_id IS NULL AND j.background_scope_id IS NULL
                AND v.present=1 THEN 1 ELSE 0 END AS confirmed_unsent,
            j.audit_request_id IS NOT NULL AND (r.id IS NULL OR r.attempts_json IS NULL) AS missing_link_body
        FROM usage_attempt_journal_v2 j
        LEFT JOIN usage_attempt_versions_v2 v ON v.attempt_id=j.attempt_id
        LEFT JOIN requests r ON r.id=j.audit_request_id
        WHERE (? IS NULL OR j.created_at>=?) AND (? IS NULL OR j.created_at<=?)
    ) SELECT COUNT(*) AS total_entries,
        COALESCE(SUM(audit_request_id IS NULL),0) AS unassociated_entries,
        COALESCE(SUM(audit_request_id IS NULL AND confirmed_unsent=0),0) AS unknown_unassociated_entries,
        COALESCE(SUM(confirmed_unsent),0) AS known_not_sent_entries,
        COALESCE(SUM(confirmed_unsent=1 AND reviewed_at IS NULL),0) AS unreviewed_known_not_sent_entries,
        COALESCE(SUM(missing_link_body),0) AS linked_body_missing_entries
        FROM scoped")
        .bind(filter.from_ts).bind(filter.from_ts).bind(filter.to_ts).bind(filter.to_ts)
        .fetch_one(&mut *tx).await?;
    let count = |name| journal.try_get::<i64, _>(name).map(|count| count as u64);
    let journal_counts = JournalCountsV2 {
        scope: "intent_created_at",
        from_ts: filter.from_ts,
        to_ts: filter.to_ts,
        total_entries: count("total_entries")?,
        unassociated_entries: count("unassociated_entries")?,
        unknown_unassociated_entries: count("unknown_unassociated_entries")?,
        known_not_sent_entries: count("known_not_sent_entries")?,
        unreviewed_known_not_sent_entries: count("unreviewed_known_not_sent_entries")?,
        linked_body_missing_entries: count("linked_body_missing_entries")?,
    };
    tx.commit().await?;
    Ok(UsageStatsV2 {
        schema_version: 2,
        request_count,
        successful_requests,
        fallback_requests,
        usage_coverage: coverage.finish(),
        by_provider: providers
            .into_iter()
            .map(|(provider_id, coverage)| ProviderUsageV2 {
                provider_id,
                usage_coverage: coverage.finish(),
            })
            .collect(),
        by_model: models
            .into_iter()
            .map(|((provider_id, model), coverage)| ModelUsageV2 {
                provider_id,
                model,
                usage_coverage: coverage.finish(),
            })
            .collect(),
        journal_counts,
        limits: Default::default(),
    })
}

/// Compatibility export rows, not a v2 DTO export. Precheck and payload read
/// share one usual isolated-SQLite read snapshot. No files or directories are
/// created here, so the command must call this before main/CSV-sidecar writes.
pub async fn query_requests_for_export(
    pool: &SqlitePool,
    filter: &RequestFilter,
) -> Result<Vec<AuditRow>> {
    export_snapshot(pool, filter, || async { Ok(()) }).await
}

// The private checkpoint lets owned WAL tests place a real writer precisely
// between preflight and read. Production supplies a no-op, in the same tx.
async fn export_snapshot<F, Fut>(
    pool: &SqlitePool,
    filter: &RequestFilter,
    after_preflight: F,
) -> Result<Vec<AuditRow>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let (where_sql, binds) = crate::audit::build_where(filter);
    let mut tx = pool.begin().await?;
    let check_sql = format!(
        "SELECT COUNT(*) AS total,
        COALESCE(SUM(CASE WHEN typeof(attempts_json)='text'
            THEN length(CAST(attempts_json AS BLOB)) ELSE 0 END),0) AS bytes,
        COALESCE(MAX(CASE WHEN typeof(attempts_json)='text'
            THEN length(CAST(attempts_json AS BLOB)) ELSE 0 END),0) AS max_bytes,
        COALESCE(SUM(typeof(attempts_json) NOT IN ('null','text')),0) AS invalid_types
        FROM requests{where_sql}"
    );
    let check = bind_filter!(sqlx::query(&check_sql), &binds)
        .fetch_one(&mut *tx)
        .await?;
    crate::audit::check_export_size(check.try_get::<i64, _>("total")? as u64)
        .map_err(GatewayError::Protocol)?;
    if check.try_get::<i64, _>("max_bytes")? > MAX_RECORD_BYTES as i64
        || check.try_get::<i64, _>("bytes")? > MAX_PAYLOAD_BYTES as i64
        || check.try_get::<i64, _>("invalid_types")? != 0
    {
        return Err(raw_export_unavailable());
    }
    after_preflight().await?;
    // Retain a CASE even after precheck as an explicit SQLx payload boundary.
    let columns = super::AUDIT_COLUMNS.replace("attempts_json", &format!(
        "CASE WHEN typeof(attempts_json)='text' AND length(CAST(attempts_json AS BLOB))<={MAX_RECORD_BYTES} \
         THEN attempts_json ELSE NULL END AS attempts_json"));
    let sql = format!("SELECT {columns} FROM requests{where_sql} ORDER BY ts DESC, id DESC");
    let rows = bind_filter!(sqlx::query(&sql), &binds)
        .fetch_all(&mut *tx)
        .await?;
    // The legacy writer can substitute an empty attempt array on decode failure.
    // Verify all selected text in this snapshot before handing it to that writer.
    for row in &rows {
        if let Some(raw) = row
            .try_get::<Option<&str>, _>("attempts_json")
            .map_err(|_| raw_export_unavailable())?
        {
            let (body, _, _) = v2::read_evidence(raw, raw.len() as u64, row.try_get("ts")?);
            if body.state != BodyState::Available {
                return Err(raw_export_unavailable());
            }
        }
    }
    tx.commit().await?;
    Ok(rows.iter().map(super::audit_row_from).collect())
}

fn raw_export_unavailable() -> GatewayError {
    GatewayError::Other(anyhow::anyhow!(RAW_EXPORT_UNAVAILABLE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn v2_export_preflight_and_read_keep_one_snapshot_during_a_wal_write() {
        let owned =
            std::env::temp_dir().join(format!("llmgw-v2-wal-snapshot-{}", uuid::Uuid::new_v4()));
        assert!(!owned.exists());
        std::fs::create_dir(&owned).unwrap();
        let path = owned.join("owned.sqlite3");
        let db = crate::db::Db::connect_path(&path).await.unwrap();
        let id = sqlx::query(
            "INSERT INTO requests(ts,requested_model,attempts_json) VALUES(1,'auto','[]')",
        )
        .execute(db.pool())
        .await
        .unwrap()
        .last_insert_rowid();
        let oversize = "x".repeat(MAX_RECORD_BYTES + 1);
        let rows = export_snapshot(db.pool(), &RequestFilter::default(), || async {
            sqlx::query("UPDATE requests SET attempts_json=? WHERE id=?")
                .bind(&oversize)
                .bind(id)
                .execute(db.pool())
                .await?;
            sqlx::query(
                "INSERT INTO requests(ts,requested_model,attempts_json) VALUES(2,'new','[]')",
            )
            .execute(db.pool())
            .await?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "newly inserted request must not enter the export snapshot"
        );
        assert_eq!(rows[0].id, id);
        assert!(
            rows[0].attempts.is_empty(),
            "the bounded preflight snapshot body is still []"
        );
        let current_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            current_count, 2,
            "the competing writer committed before the export read"
        );
        assert_eq!(
            query_requests_for_export(db.pool(), &RequestFilter::default())
                .await
                .unwrap_err()
                .to_string(),
            RAW_EXPORT_UNAVAILABLE
        );
        db.pool().close().await;
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(owned).unwrap();
    }

    #[tokio::test]
    async fn v2_sql_boundary_never_selects_oversized_or_overbudget_payload() {
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        for index in 0..34_i64 {
            sqlx::query("INSERT INTO requests(ts,requested_model,prompt_tokens,completion_tokens,fallback_attempts,trace_id,attempts_json) VALUES(?, 'auto',0,0,0,'synthetic',?)")
                .bind(index).bind(" ".repeat(MAX_RECORD_BYTES)).execute(db.pool()).await.unwrap();
        }
        sqlx::query("INSERT INTO requests(ts,requested_model,prompt_tokens,completion_tokens,fallback_attempts,trace_id,attempts_json) VALUES(100, 'auto',0,0,0,'synthetic',?)")
            .bind(" ".repeat(MAX_RECORD_BYTES + 1)).execute(db.pool()).await.unwrap();
        let sql = bounded_sql("", true);
        let rows = sqlx::query(&sql)
            .bind(500_i64)
            .bind(0_i64)
            .fetch_all(db.pool())
            .await
            .unwrap();
        let mut observed = 0;
        let mut omitted = 0;
        for row in rows {
            let raw: Option<String> = row.try_get("v2_raw").unwrap();
            if let Some(raw) = raw {
                observed += raw.len();
                assert!(raw.len() <= MAX_RECORD_BYTES);
            } else {
                omitted += 1;
            }
            assert!(row
                .try_get::<Option<String>, _>("attempts_json")
                .unwrap()
                .is_none());
        }
        assert_eq!(observed, MAX_PAYLOAD_BYTES);
        assert_eq!(omitted, 3);
    }
}
