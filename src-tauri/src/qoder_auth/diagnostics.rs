//! Closed local diagnostics for authentication request failures.
//! Never pass request inputs or raw errors to tracing. This is not a wire or
//! billing receipt, and cannot recover observations missing from old attempts.
use std::time::Duration;

#[derive(Clone, Copy)]
pub(super) enum Operation {
    DevicePoll,
    UserInfo,
    DeviceRefresh,
    JobIssue,
    JobRefresh,
    Unknown,
}
impl Operation {
    pub(super) fn from_path(path: &str) -> Self {
        match path {
            "/api/v1/deviceToken/poll" => Self::DevicePoll,
            "/api/v1/userinfo" => Self::UserInfo,
            "/api/v1/deviceToken/refresh" => Self::DeviceRefresh,
            "/api/v1/me/jobToken" => Self::JobIssue,
            "/api/v1/jobToken/refresh" => Self::JobRefresh,
            _ => Self::Unknown,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::DevicePoll => "device_poll",
            Self::UserInfo => "userinfo",
            Self::DeviceRefresh => "device_refresh",
            Self::JobIssue => "job_issue",
            Self::JobRefresh => "job_refresh",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Clone, Copy)]
pub(super) enum Stage {
    Send,
    ResponseRead,
    ResponseParse,
}
impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::ResponseRead => "response_read",
            Self::ResponseParse => "response_parse",
        }
    }
}
#[derive(Clone, Copy)]
pub(super) enum Kind {
    Timeout,
    ConnectOrTls,
    Request,
    Other,
    BodyLimit,
    InvalidJson,
}
impl Kind {
    pub(super) fn from_reqwest(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else if error.is_connect() {
            Self::ConnectOrTls
        } else if error.is_request() {
            Self::Request
        } else {
            Self::Other
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::ConnectOrTls => "connect_or_tls",
            Self::Request => "request",
            Self::Other => "other",
            Self::BodyLimit => "body_limit",
            Self::InvalidJson => "invalid_json",
        }
    }
}
pub(super) fn record(
    operation: Operation,
    stage: Stage,
    kind: Kind,
    http_status: Option<u16>,
    elapsed: Duration,
) {
    let request_elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    // An observed HTTP response can still leave issuance/persistence unknown.
    // A send error does not prove that the peer received zero requests.
    tracing::warn!(
        target: "llm_gateway::qoder_auth",
        operation = operation.label(),
        stage = stage.label(),
        classification = kind.label(),
        http_status = ?http_status,
        request_elapsed_ms,
        remote_auth_outcome = "unknown",
        "Qoder authentication request failed"
    );
}
