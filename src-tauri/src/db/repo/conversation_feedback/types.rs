use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum TaskError {
    #[error("task_unavailable")]
    TaskUnavailable,
    #[error("task_invalid_input")]
    TaskInvalidInput,
    #[error("task_cursor_invalid")]
    TaskCursorInvalid,
    #[error("task_storage_failed")]
    TaskStorageFailed,
    #[error("task_summary_limit_exceeded")]
    TaskSummaryLimitExceeded,
}
pub(super) type Result<T> = std::result::Result<T, TaskError>;
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPage {
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRef {
    pub session_id: String,
    pub group_id: String,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct RequestCounts {
    pub requests: u64,
    pub annotated: u64,
    pub class_confirmed: u64,
    pub met_need: u64,
    pub did_not_meet_need: u64,
    pub unsure: u64,
    pub missing_outcome: u64,
    pub feedback_unavailable: u64,
    pub http_2xx: u64,
    pub cancelled: u64,
    pub errors: u64,
    pub status_other_unknown: u64,
}
#[derive(Debug, Clone, Serialize)]
pub struct ConversationTask {
    pub task: TaskRef,
    pub profile_id: String,
    pub region: String,
    pub caller_scope: String,
    pub matching_requests: u64,
    pub counts: RequestCounts,
    pub first_ts: i64,
    pub last_ts: i64,
    pub last_request_id: i64,
}
#[derive(Debug, Serialize)]
pub struct TaskSummary {
    pub observed_at_unix_ms: i64,
    pub matched_requests: u64,
    pub grouped_matching_requests: u64,
    pub ungrouped_matching_requests: u64,
    pub ungrouped_reasons: BTreeMap<String, u64>,
    pub total_tasks: u64,
    pub total_stored_requests_in_tasks: u64,
    pub tasks: Vec<ConversationTask>,
    pub offset: u32,
    pub limit: u32,
    pub has_more: bool,
}
#[derive(Debug, Serialize)]
pub struct TaskRequest {
    pub id: i64,
    pub ts: i64,
    pub status: Option<i64>,
    pub trace_id: Option<String>,
    pub routed_provider: Option<String>,
    pub routed_model: Option<String>,
    pub confirmed_class: Option<String>,
    pub outcome: Option<String>,
    pub feedback_revision: Option<i64>,
    pub feedback_unavailable: bool,
    pub matches_filter: bool,
}
#[derive(Debug, Serialize)]
pub struct TaskRequests {
    pub task: TaskRef,
    pub total_requests: u64,
    pub rows: Vec<TaskRequest>,
    pub has_more: bool,
    pub next_cursor: Option<i64>,
}
