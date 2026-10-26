//! Retained conversation facts, never a task-quality/fee/entitlement score.
mod scope;
mod types;
use crate::audit::{self, Bind, RequestFilter};
use futures_util::TryStreamExt;
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use types::Result;
pub use types::*;
const MAX_SAFE: i64 = 9_007_199_254_740_991;
const MAX_ROWS: usize = audit::EXPORT_MAX_ROWS as usize;
const COLUMNS:&str="r.id,r.ts,r.session_id,r.client,r.access_key_id,r.route_intent,r.routed_provider,r.routed_model,r.trace_id,r.status,
 CASE WHEN length(CAST(r.attempts_json AS BLOB))<=262144 THEN r.attempts_json END AS attempts_json,
 CASE WHEN length(CAST(r.route_decision_json AS BLOB))<=16384 THEN r.route_decision_json END AS route_decision_json,
 f.confirmed_class,f.outcome,f.revision,f.source";
const JOIN: &str = "FROM requests r LEFT JOIN request_feedback f ON f.request_id=r.id";
struct InputRow {
    id: i64,
    ts: i64,
    session: Option<String>,
    client: Option<String>,
    access_key: Option<String>,
    intent: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    trace: Option<String>,
    status: Option<i64>,
    attempts: Option<String>,
    decision: Option<String>,
    class: Option<String>,
    outcome: Option<String>,
    revision: Option<i64>,
    source: Option<String>,
}
impl InputRow {
    fn read(r: &SqliteRow) -> Result<Self> {
        Ok(Self {
            id: r.try_get("id").map_err(|_| TaskError::TaskStorageFailed)?,
            ts: r.try_get("ts").map_err(|_| TaskError::TaskStorageFailed)?,
            session: r
                .try_get("session_id")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            client: r
                .try_get("client")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            access_key: r
                .try_get("access_key_id")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            intent: r
                .try_get("route_intent")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            provider: r
                .try_get("routed_provider")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            model: r
                .try_get("routed_model")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            trace: r
                .try_get("trace_id")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            status: r
                .try_get("status")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            attempts: r
                .try_get("attempts_json")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            decision: r
                .try_get("route_decision_json")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            class: r
                .try_get("confirmed_class")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            outcome: r
                .try_get("outcome")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            revision: r
                .try_get("revision")
                .map_err(|_| TaskError::TaskStorageFailed)?,
            source: r
                .try_get("source")
                .map_err(|_| TaskError::TaskStorageFailed)?,
        })
    }
    fn item(&self, matches_filter: bool) -> TaskRequest {
        let absent = self.revision.is_none()
            && self.source.is_none()
            && self.class.is_none()
            && self.outcome.is_none();
        let valid = self.source.as_deref() == Some("local_user")
            && self.revision.is_some_and(|r| (1..=MAX_SAFE).contains(&r))
            && self
                .class
                .as_deref()
                .map_or(true, |c| matches!(c, "simple" | "vision" | "reasoning"))
            && self.outcome.as_deref().map_or(true, |s| {
                matches!(s, "met_need" | "did_not_meet_need" | "unsure")
            });
        TaskRequest {
            id: self.id,
            ts: self.ts,
            status: self.status.filter(|s| (100..=599).contains(s)),
            trace_id: self.trace.clone().filter(|t| scope::hex(t, 32)),
            routed_provider: self.provider.clone(),
            routed_model: self.model.clone(),
            confirmed_class: valid.then(|| self.class.clone()).flatten(),
            outcome: valid.then(|| self.outcome.clone()).flatten(),
            feedback_revision: valid.then_some(self.revision).flatten(),
            feedback_unavailable: !valid && !absent,
            matches_filter,
        }
    }
}
impl RequestCounts {
    fn add(&mut self, row: &InputRow) {
        self.requests += 1;
        let item = row.item(false);
        if item.confirmed_class.is_some() {
            self.class_confirmed += 1;
        }
        if item.confirmed_class.is_some() || item.outcome.is_some() {
            self.annotated += 1;
        }
        match item.outcome.as_deref() {
            Some("met_need") => self.met_need += 1,
            Some("did_not_meet_need") => self.did_not_meet_need += 1,
            Some("unsure") => self.unsure += 1,
            _ => self.missing_outcome += 1,
        }
        if item.feedback_unavailable {
            self.feedback_unavailable += 1;
        }
        match item.status {
            Some(200..=299) => self.http_2xx += 1,
            Some(499) => self.cancelled += 1,
            Some(400..=599) => self.errors += 1,
            _ => self.status_other_unknown += 1,
        }
    }
}
fn validate_filter(f: &RequestFilter) -> Result<()> {
    if f.from_ts.zip(f.to_ts).is_some_and(|(a, b)| a > b)
        || f.min_cost.is_some_and(|v| !v.is_finite())
        || f.max_cost.is_some_and(|v| !v.is_finite())
    {
        Err(TaskError::TaskInvalidInput)
    } else {
        Ok(())
    }
}
macro_rules! filtered {
    ($sql:expr,$binds:expr) => {{
        let mut q = sqlx::query($sql);
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

pub async fn summary(
    pool: &SqlitePool,
    filter: &RequestFilter,
    page: Option<TaskPage>,
) -> Result<TaskSummary> {
    validate_filter(filter)?;
    let page = page.unwrap_or_default();
    let limit = page.limit.unwrap_or(50).clamp(1, 100);
    let (where_sql, binds) = audit::build_where(filter);
    let sql = format!(
        "SELECT {COLUMNS} {JOIN}{where_sql} ORDER BY r.id LIMIT {}",
        MAX_ROWS + 1
    );
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| TaskError::TaskStorageFailed)?;
    let mut matched = 0u64;
    let mut selected = HashMap::<scope::Scope, u64>::new();
    let mut reasons = BTreeMap::<String, u64>::new();
    let mut sessions = BTreeSet::new();
    {
        let mut rows = filtered!(&sql, &binds).fetch(&mut *tx);
        while let Some(r) = rows
            .try_next()
            .await
            .map_err(|_| TaskError::TaskStorageFailed)?
        {
            matched += 1;
            if matched > MAX_ROWS as u64 {
                return Err(TaskError::TaskSummaryLimitExceeded);
            }
            let row = InputRow::read(&r)?;
            match scope::extract(&row) {
                Ok(key) => {
                    sessions.insert(row.session.expect("validated scope session"));
                    *selected.entry(key).or_default() += 1;
                }
                Err(reason) => *reasons.entry(reason.into()).or_default() += 1,
            }
        }
    }
    let mut groups = HashMap::<scope::Scope, ConversationTask>::new();
    if !selected.is_empty() {
        // All retained records for selected sessions, not merely the filtered page.
        // Scope matching below prevents another subject/region/caller sharing a label
        // from entering this task. Both scans use the same readonly SQLite snapshot.
        let sessions = serde_json::to_string(&sessions).map_err(|_| TaskError::TaskInvalidInput)?;
        let sql=format!("SELECT {COLUMNS} {JOIN} WHERE r.session_id IN (SELECT value FROM json_each(?)) ORDER BY r.id LIMIT {}",MAX_ROWS+1);
        let mut rows = sqlx::query(&sql).bind(sessions).fetch(&mut *tx);
        let mut seen = 0;
        while let Some(r) = rows
            .try_next()
            .await
            .map_err(|_| TaskError::TaskStorageFailed)?
        {
            seen += 1;
            if seen > MAX_ROWS {
                return Err(TaskError::TaskSummaryLimitExceeded);
            }
            let row = InputRow::read(&r)?;
            let Ok(key) = scope::extract(&row) else {
                continue;
            };
            let Some(&matching) = selected.get(&key) else {
                continue;
            };
            let entry = groups
                .entry(key.clone())
                .or_insert_with(|| ConversationTask {
                    task: key.task_ref(),
                    profile_id: key.profile(),
                    region: key.region(),
                    caller_scope: key.caller(),
                    matching_requests: matching,
                    counts: Default::default(),
                    first_ts: row.ts,
                    last_ts: row.ts,
                    last_request_id: row.id,
                });
            entry.counts.add(&row);
            entry.first_ts = entry.first_ts.min(row.ts);
            entry.last_ts = entry.last_ts.max(row.ts);
            entry.last_request_id = entry.last_request_id.max(row.id);
        }
    }
    if groups.len() != selected.len() {
        return Err(TaskError::TaskStorageFailed);
    }
    tx.commit()
        .await
        .map_err(|_| TaskError::TaskStorageFailed)?;
    let grouped_matching_requests = selected.values().sum();
    let ungrouped_matching_requests = reasons.values().sum();
    let mut tasks = groups.into_values().collect::<Vec<_>>();
    let total_tasks = tasks.len() as u64;
    let total_stored_requests_in_tasks = tasks.iter().map(|g| g.counts.requests).sum();
    tasks.sort_by(|a, b| {
        b.last_ts
            .cmp(&a.last_ts)
            .then_with(|| b.last_request_id.cmp(&a.last_request_id))
            .then_with(|| a.task.group_id.cmp(&b.task.group_id))
    });
    let offset = page.offset as usize;
    let has_more = offset.saturating_add(limit as usize) < tasks.len();
    let tasks = tasks
        .into_iter()
        .skip(offset)
        .take(limit as usize)
        .collect();
    Ok(TaskSummary {
        observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        matched_requests: matched,
        grouped_matching_requests,
        ungrouped_matching_requests,
        ungrouped_reasons: reasons,
        total_tasks,
        total_stored_requests_in_tasks,
        tasks,
        offset: page.offset,
        limit,
        has_more,
    })
}

pub async fn requests(
    pool: &SqlitePool,
    task: &TaskRef,
    filter: &RequestFilter,
    after_id: Option<i64>,
    limit: Option<u32>,
) -> Result<TaskRequests> {
    scope::validate_task(task)?;
    validate_filter(filter)?;
    if after_id.is_some_and(|id| !(1..=MAX_SAFE).contains(&id)) {
        return Err(TaskError::TaskInvalidInput);
    }
    let limit = limit.unwrap_or(100).clamp(1, 500) as usize;
    let (where_sql, binds) = audit::build_where(filter);
    let sql=format!("WITH matching AS (SELECT id FROM requests{where_sql}) SELECT {COLUMNS},CASE WHEN m.id IS NULL THEN 0 ELSE 1 END AS matches_filter {JOIN} LEFT JOIN matching m ON m.id=r.id WHERE r.session_id=? ORDER BY r.id LIMIT {}",MAX_ROWS+1);
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| TaskError::TaskStorageFailed)?;
    let mut result = Vec::new();
    let mut total = 0u64;
    let mut found_cursor = after_id.is_none();
    {
        let mut rows = filtered!(&sql, &binds)
            .bind(&task.session_id)
            .fetch(&mut *tx);
        let mut seen = 0;
        while let Some(r) = rows
            .try_next()
            .await
            .map_err(|_| TaskError::TaskStorageFailed)?
        {
            seen += 1;
            if seen > MAX_ROWS {
                return Err(TaskError::TaskSummaryLimitExceeded);
            }
            let row = InputRow::read(&r)?;
            let Ok(key) = scope::extract(&row) else {
                continue;
            };
            if key.task_ref() != *task {
                continue;
            }
            total += 1;
            if after_id == Some(row.id) {
                found_cursor = true;
            }
            if row.id > after_id.unwrap_or(0) && result.len() <= limit {
                result.push(
                    row.item(
                        r.try_get::<i64, _>("matches_filter")
                            .map_err(|_| TaskError::TaskStorageFailed)?
                            != 0,
                    ),
                );
            }
        }
    }
    if total == 0 {
        return Err(TaskError::TaskUnavailable);
    }
    if !found_cursor {
        return Err(TaskError::TaskCursorInvalid);
    }
    tx.commit()
        .await
        .map_err(|_| TaskError::TaskStorageFailed)?;
    let has_more = result.len() > limit;
    result.truncate(limit);
    let next_cursor = if has_more {
        result.last().map(|r| r.id)
    } else {
        None
    };
    Ok(TaskRequests {
        task: task.clone(),
        total_requests: total,
        rows: result,
        has_more,
        next_cursor,
    })
}

#[cfg(test)]
mod subject_receipt_tests;
