use super::{InputRow, TaskError, TaskRef};
use crate::proxy::subject_receipt::{Profile as AccountProfile, Receipt};
use crate::proxy::trae::profile::{TraeProfileId, TraeRegion};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Profile {
    Native(TraeProfileId),
    Account(AccountProfile),
}
impl Profile {
    fn as_str(self) -> &'static str {
        match self {
            Self::Native(p) => p.as_str(),
            Self::Account(p) => p.as_str(),
        }
    }
    fn region(self) -> &'static str {
        match self {
            Self::Native(p) => match p.region() {
                TraeRegion::Cn => "cn",
                TraeRegion::Global => "global",
            },
            Self::Account(p) => p.region(),
        }
    }
}

// Deliberately not Debug/Serialize: slot and principal never cross the read DTO.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct Scope {
    session: String,
    key: Option<String>,
    profile: Profile,
    account: String,
    principal: String,
}
#[derive(Deserialize)]
struct Attempt {
    provider_id: String,
    model: String,
    trace_id: String,
    attempt: u32,
    native_exact: Marker,
}
#[derive(Deserialize)]
struct Marker {
    version: u8,
    profile_id: TraeProfileId,
    account_id: String,
    principal_hash: String,
    source_function: String,
    attempt: u32,
    upstream_post_started: bool,
}
pub(super) fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control) && !s.contains("://")
}
pub(super) fn hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(super) fn extract(row: &InputRow) -> std::result::Result<Scope, &'static str> {
    if row.id <= 0 || row.id > super::MAX_SAFE || row.ts < 0 || row.ts > super::MAX_SAFE {
        return Err("invalid_record");
    }
    let session = row
        .session
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or("missing_session")?;
    if !identifier(session) {
        return Err("invalid_session");
    }
    if crate::budget::access_key_id_of(row.client.as_deref()) != row.access_key.as_deref()
        || row.access_key.as_deref().is_some_and(|k| !identifier(k))
    {
        return Err("caller_scope_unknown");
    }
    if let Some(scope) = extract_account(row, session)? {
        return Ok(scope);
    }
    if row.intent.as_deref() != Some("native_exact") {
        return Err("isolation_unrecorded");
    }
    let raw = row.attempts.as_deref().ok_or("isolation_unrecorded")?;
    if raw.len() > 256 * 1024 {
        return Err("isolation_invalid");
    }
    // Known scope fields use typed serde decoding: duplicate known fields, unknown
    // profiles and wrong types fail closed. Unused usage/body fields are ignored,
    // never copied, measured or treated as evidence about price or completion.
    let attempts: Vec<Attempt> = serde_json::from_str(raw).map_err(|_| "isolation_invalid")?;
    let [a] = attempts.as_slice() else {
        return Err("isolation_invalid");
    };
    let m = &a.native_exact;
    if m.version != 1
        || m.attempt != 1
        || a.attempt != 1
        || !identifier(&m.account_id)
        || !hex(&m.principal_hash, 64)
        || m.source_function != m.profile_id.function().as_str()
        || row.trace.as_deref() != Some(a.trace_id.as_str())
        || row.provider.as_deref() != Some(a.provider_id.as_str())
        || row.model.as_deref() != Some(a.model.as_str())
    {
        return Err("isolation_invalid");
    }
    let _started = m.upstream_post_started; // Both pre-post cancellation and POST are request facts.
    let crate::audit::route_decision::Projection::Recorded { snapshot } =
        crate::audit::route_decision::project(row.decision.as_deref())
    else {
        return Err("isolation_unrecorded");
    };
    if !snapshot.matches_exact_target(&a.provider_id, &a.model) {
        return Err("isolation_invalid");
    }
    Ok(Scope {
        session: session.into(),
        key: row.access_key.clone(),
        profile: Profile::Native(m.profile_id),
        account: m.account_id.clone(),
        principal: m.principal_hash.clone(),
    })
}
impl Scope {
    pub(super) fn task_ref(&self) -> TaskRef {
        let bytes = serde_json::to_vec(&(
            &self.session,
            &self.key,
            self.profile.as_str(),
            &self.account,
            &self.principal,
        ))
        .expect("owned string scope serialization");
        TaskRef {
            session_id: self.session.clone(),
            group_id: format!("{:x}", Sha256::digest(bytes)),
        }
    }
    pub(super) fn profile(&self) -> String {
        self.profile.as_str().into()
    }
    pub(super) fn region(&self) -> String {
        self.profile.region().into()
    }
    pub(super) fn caller(&self) -> String {
        if self.key.is_some() {
            "remote_key"
        } else {
            "local_key"
        }
        .into()
    }
}
pub(super) fn validate_task(task: &TaskRef) -> std::result::Result<(), TaskError> {
    if !identifier(&task.session_id) || !hex(&task.group_id, 64) {
        Err(TaskError::TaskInvalidInput)
    } else {
        Ok(())
    }
}

#[derive(Deserialize)]
struct AccountAttempt {
    provider_id: Option<String>,
    model: Option<String>,
    trace_id: Option<String>,
    attempt: Option<usize>,
    subject_receipt: Option<Receipt>,
    native_exact: Option<serde::de::IgnoredAny>,
}
fn extract_account(
    row: &InputRow,
    session: &str,
) -> std::result::Result<Option<Scope>, &'static str> {
    let Some(raw) = row.attempts.as_deref() else {
        return Ok(None);
    };
    if raw.len() > 256 * 1024 {
        return Err("isolation_invalid");
    }
    let attempts: Vec<AccountAttempt> = match serde_json::from_str(raw) {
        Ok(value) => value,
        // Legacy non-native malformed traces were never identity evidence.
        Err(_) => {
            return if row.intent.as_deref() == Some("native_exact") {
                Err("isolation_invalid")
            } else {
                Ok(None)
            }
        }
    };
    if attempts.iter().all(|a| a.subject_receipt.is_none()) {
        return Ok(None);
    }
    let [a] = attempts.as_slice() else {
        return Err("isolation_invalid");
    };
    let r = a.subject_receipt.as_ref().ok_or("isolation_unrecorded")?;
    if a.native_exact.is_some()
        || row.intent.as_deref() == Some("native_exact")
        || a.attempt != Some(0)
        || a.trace_id != row.trace
        || a.provider_id != row.provider
        || a.model != row.model
    {
        return Err("isolation_invalid");
    }
    let (Some(trace), Some(provider), Some(model)) = (
        row.trace.as_deref(),
        row.provider.as_deref(),
        row.model.as_deref(),
    ) else {
        return Err("isolation_invalid");
    };
    if !r.validate(session, trace, provider, model, 0) {
        return Err("isolation_invalid");
    }
    Ok(Some(Scope {
        session: session.into(),
        key: row.access_key.clone(),
        profile: Profile::Account(r.profile()),
        account: String::new(),
        principal: r.scope().into(),
    }))
}
