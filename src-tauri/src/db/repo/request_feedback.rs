//! Explicit local-user feedback, never inferred from HTTP success or model text.
//! See the S3 feedback contract in docs/wiki/request-feedback.md.
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackError {
    #[error("feedback_invalid_input")]
    FeedbackInvalidInput,
    #[error("feedback_request_not_found")]
    FeedbackRequestNotFound,
    #[error("feedback_conflict")]
    FeedbackConflict,
    #[error("feedback_storage_failed")]
    FeedbackStorageFailed,
}
type Result<T> = std::result::Result<T, FeedbackError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackInput {
    pub request_id: i64,
    pub expected_revision: i64,
    pub confirmed_class: Option<String>,
    pub outcome: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestFeedback {
    pub request_id: i64,
    pub ts: i64,
    pub requested_model: String,
    pub routed_provider: Option<String>,
    pub routed_model: Option<String>,
    pub status: Option<i64>,
    pub route_intent: Option<String>,
    pub route_classifier: Option<String>,
    pub confirmed_class: Option<String>,
    pub outcome: Option<String>,
    pub revision: i64,
    pub source: Option<String>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
}
fn valid_id(id: i64) -> Result<()> {
    if !(1..=MAX_SAFE_INTEGER).contains(&id) {
        return Err(FeedbackError::FeedbackInvalidInput);
    }
    Ok(())
}
impl FeedbackInput {
    fn validate(&self) -> Result<()> {
        valid_id(self.request_id)?;
        if !(0..MAX_SAFE_INTEGER).contains(&self.expected_revision)
            || self.confirmed_class.as_deref().is_some_and(|value| {
                !matches!(crate::router::score::TaskClass::parse(value), Some(class) if class.code() == value)
            })
            || self
                .outcome
                .as_deref()
                .is_some_and(|value| !matches!(value, "met_need" | "did_not_meet_need" | "unsure"))
        {
            return Err(FeedbackError::FeedbackInvalidInput);
        }
        Ok(())
    }
}
const SELECT: &str = "SELECT r.id AS request_id, r.ts, r.requested_model, r.routed_provider, r.routed_model, r.status,
    r.route_intent, r.route_classifier, f.confirmed_class, f.outcome, COALESCE(f.revision,0) AS revision,
    f.source, f.created_at, f.updated_at FROM requests r LEFT JOIN request_feedback f ON f.request_id=r.id WHERE r.id=?";
fn decode(r: SqliteRow) -> RequestFeedback {
    RequestFeedback {
        request_id: r.get("request_id"),
        ts: r.get("ts"),
        requested_model: r.get("requested_model"),
        routed_provider: r.get("routed_provider"),
        routed_model: r.get("routed_model"),
        status: r.get("status"),
        route_intent: r.get("route_intent"),
        route_classifier: r.get("route_classifier"),
        confirmed_class: r.get("confirmed_class"),
        outcome: r.get("outcome"),
        revision: r.get("revision"),
        source: r.get("source"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}
pub async fn get(pool: &SqlitePool, request_id: i64) -> Result<RequestFeedback> {
    valid_id(request_id)?;
    sqlx::query(SELECT)
        .bind(request_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| FeedbackError::FeedbackStorageFailed)?
        .map(decode)
        .ok_or(FeedbackError::FeedbackRequestNotFound)
}
pub async fn set(pool: &SqlitePool, input: FeedbackInput) -> Result<RequestFeedback> {
    input.validate()?;
    let now = chrono::Utc::now().timestamp();
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| FeedbackError::FeedbackStorageFailed)?;
    // First statement takes the write lock. Avoid read->write upgrades and make
    // create/update CAS atomic across windows. No replay on storage failure.
    let changed=sqlx::query_scalar::<_,i64>("INSERT INTO request_feedback
        (request_id,confirmed_class,outcome,revision,source,created_at,updated_at)
        SELECT r.id,?,?,1,'local_user',?,? FROM requests r WHERE r.id=?
          AND (?=0 OR EXISTS (SELECT 1 FROM request_feedback f WHERE f.request_id=r.id AND f.revision=?))
        ON CONFLICT(request_id) DO UPDATE SET confirmed_class=excluded.confirmed_class,
          outcome=excluded.outcome,revision=request_feedback.revision+1,updated_at=excluded.updated_at
        WHERE request_feedback.revision=? RETURNING request_id")
        .bind(&input.confirmed_class).bind(&input.outcome).bind(now).bind(now)
        .bind(input.request_id).bind(input.expected_revision).bind(input.expected_revision).bind(input.expected_revision)
        .fetch_optional(&mut *tx).await.map_err(|_| FeedbackError::FeedbackStorageFailed)?;
    if changed.is_none() {
        let exists: i64 = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM requests WHERE id=?)")
            .bind(input.request_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| FeedbackError::FeedbackStorageFailed)?;
        return Err(if exists == 0 {
            FeedbackError::FeedbackRequestNotFound
        } else {
            FeedbackError::FeedbackConflict
        });
    }
    // Read back inside the write transaction; don't accidentally return another
    // window's later revision. Clearing keeps the row/revision to prevent ABA.
    let row = sqlx::query(SELECT)
        .bind(input.request_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| FeedbackError::FeedbackStorageFailed)?;
    let saved = decode(row);
    tx.commit()
        .await
        .map_err(|_| FeedbackError::FeedbackStorageFailed)?;
    Ok(saved)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageCounts {
    pub requests: i64,
    pub annotated: i64,
    pub class_confirmed: i64,
    pub met_need: i64,
    pub did_not_meet_need: i64,
    pub unsure: i64,
    pub missing_outcome: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageGroup {
    pub routed_provider: Option<String>,
    pub routed_model: Option<String>,
    pub confirmed_class: Option<String>,
    #[serde(flatten)]
    pub counts: CoverageCounts,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackCoverage {
    #[serde(flatten)]
    pub counts: CoverageCounts,
    pub groups: Vec<CoverageGroup>,
    pub groups_truncated: bool,
}
const COVERAGE_COLUMNS: &str = "COUNT(*) AS requests,
 COALESCE(SUM(f.confirmed_class IS NOT NULL OR f.outcome IS NOT NULL),0) AS annotated,
 COALESCE(SUM(f.confirmed_class IS NOT NULL),0) AS class_confirmed,
 COALESCE(SUM(f.outcome='met_need'),0) AS met_need,
 COALESCE(SUM(f.outcome='did_not_meet_need'),0) AS did_not_meet_need,
 COALESCE(SUM(f.outcome='unsure'),0) AS unsure,
 COALESCE(SUM(f.outcome IS NULL),0) AS missing_outcome";
fn coverage_counts(r: &SqliteRow) -> CoverageCounts {
    CoverageCounts {
        requests: r.get("requests"),
        annotated: r.get("annotated"),
        class_confirmed: r.get("class_confirmed"),
        met_need: r.get("met_need"),
        did_not_meet_need: r.get("did_not_meet_need"),
        unsure: r.get("unsure"),
        missing_outcome: r.get("missing_outcome"),
    }
}
/// Same audit filter, full matching population (pagination excluded), no quality
/// score or implicit sample threshold. Both reads share a SQLite snapshot.
pub async fn coverage(
    pool: &SqlitePool,
    filter: &crate::audit::RequestFilter,
) -> Result<FeedbackCoverage> {
    let (where_sql, binds) = crate::audit::build_where(filter);
    let from =
        format!("FROM requests r LEFT JOIN request_feedback f ON f.request_id=r.id{where_sql}");
    let totals_sql = format!("SELECT {COVERAGE_COLUMNS} {from}");
    let groups_sql = format!(
        "SELECT r.routed_provider,r.routed_model,f.confirmed_class,{COVERAGE_COLUMNS} {from}
        GROUP BY r.routed_provider,r.routed_model,f.confirmed_class
        ORDER BY requests DESC,r.routed_provider,r.routed_model,f.confirmed_class LIMIT 201"
    );
    macro_rules! bound {
        ($sql:expr) => {{
            let mut q = sqlx::query($sql);
            for b in &binds {
                q = match b {
                    crate::audit::Bind::I64(v) => q.bind(*v),
                    crate::audit::Bind::F64(v) => q.bind(*v),
                    crate::audit::Bind::Text(v) => q.bind(v.clone()),
                };
            }
            q
        }};
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| FeedbackError::FeedbackStorageFailed)?;
    let total = bound!(&totals_sql)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| FeedbackError::FeedbackStorageFailed)?;
    let rows = bound!(&groups_sql)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| FeedbackError::FeedbackStorageFailed)?;
    let groups_truncated = rows.len() > 200;
    let groups = rows
        .into_iter()
        .take(200)
        .map(|r| CoverageGroup {
            routed_provider: r.get("routed_provider"),
            routed_model: r.get("routed_model"),
            confirmed_class: r.get("confirmed_class"),
            counts: coverage_counts(&r),
        })
        .collect();
    tx.commit()
        .await
        .map_err(|_| FeedbackError::FeedbackStorageFailed)?;
    Ok(FeedbackCoverage {
        counts: coverage_counts(&total),
        groups,
        groups_truncated,
    })
}
