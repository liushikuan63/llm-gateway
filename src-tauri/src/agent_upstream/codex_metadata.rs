//! Non-secret Codex metadata: an observed login binding, NOT a credential lease.
//! Explicit personal-test opt-in connects the selected CLI; startup/status reads
//! do not. Developer real-account probes need their own approval. Never open
//! auth/key files in this adapter.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

const MAX_FRAME: usize = 64 * 1024;
const MAX_FRAMES: usize = 32;
const MAX_AGE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataError {
    Unsupported,
    LoginRequired,
    InvalidResponse,
    Changed,
    Disconnected,
    Protocol,
    Timeout,
    Io,
    InvalidContext,
}
impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Unsupported => "metadata_unsupported",
                Self::LoginRequired => "metadata_login_required",
                Self::InvalidResponse => "metadata_invalid_response",
                Self::Changed => "metadata_changed",
                Self::Disconnected => "metadata_disconnected",
                Self::Protocol => "metadata_protocol_rejected",
                Self::Timeout => "metadata_timeout",
                Self::Io => "metadata_io_failed",
                Self::InvalidContext => "metadata_context_invalid",
            }
        )
    }
}
impl std::error::Error for MetadataError {}

/// Hashes only approved non-secret fields. Email/workspace/origin are not kept
/// or serialized, and a plan label never grants eligibility or settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedAccount {
    fingerprint: [u8; 32],
}
fn bounded(v: &Value, max: usize) -> Result<&str, MetadataError> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
        .ok_or(MetadataError::InvalidResponse)
}
fn object<'a>(
    v: &'a Value,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, MetadataError> {
    let o = v.as_object().ok_or(MetadataError::InvalidResponse)?;
    if o.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(MetadataError::InvalidResponse);
    }
    Ok(o)
}
fn field<'a>(o: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a Value, MetadataError> {
    o.get(key).ok_or(MetadataError::InvalidResponse)
}
fn digest(value: &impl serde::Serialize) -> Result<[u8; 32], MetadataError> {
    Ok(
        Sha256::digest(serde_json::to_vec(value).map_err(|_| MetadataError::InvalidContext)?)
            .into(),
    )
}
pub fn parse_account_result(value: &Value) -> Result<ObservedAccount, MetadataError> {
    let o = object(
        value,
        &["account", "requiresOpenaiAuth", "workspaceRouting"],
    )?;
    let required = field(o, "requiresOpenaiAuth")?
        .as_bool()
        .ok_or(MetadataError::InvalidResponse)?;
    let account = o.get("account").unwrap_or(&Value::Null);
    if account.is_null() {
        return Err(if required {
            MetadataError::LoginRequired
        } else {
            MetadataError::Unsupported
        });
    }
    let kind = account
        .get("type")
        .and_then(Value::as_str)
        .ok_or(MetadataError::InvalidResponse)?;
    if kind != "chatgpt" || !required {
        return Err(MetadataError::Unsupported);
    }
    let account = object(account, &["type", "email", "planType"])?;
    let email = account
        .get("email")
        .filter(|v| !v.is_null())
        .ok_or(MetadataError::Unsupported)?;
    let email = bounded(email, 320)?;
    if email.chars().any(char::is_whitespace) || !email.contains('@') {
        return Err(MetadataError::InvalidResponse);
    }
    let plan = bounded(field(account, "planType")?, 64)?;
    if !matches!(
        plan,
        "free"
            | "go"
            | "plus"
            | "pro"
            | "prolite"
            | "promax"
            | "team"
            | "self_serve_business_prolite"
            | "self_serve_business_usage_based"
            | "business"
            | "ent26"
            | "enterprise_cbp_automation"
            | "enterprise_cbp_usage_based"
            | "enterprise"
            | "edu"
            | "edu_plus"
            | "edu_pro"
            | "unknown"
    ) {
        return Err(MetadataError::Unsupported);
    }
    let routing = o
        .get("workspaceRouting")
        .filter(|v| !v.is_null())
        .ok_or(MetadataError::Unsupported)?;
    let routing = object(
        routing,
        &[
            "accountRoutingOverride",
            "backendOrigin",
            "chatgptAccountId",
        ],
    )?;
    let route = bounded(field(routing, "accountRoutingOverride")?, 32)?;
    if !matches!(route, "NO_CONSTRAINT" | "us" | "us_cr") {
        return Err(MetadataError::Unsupported);
    }
    let origin = bounded(field(routing, "backendOrigin")?, 2048)?;
    let workspace = bounded(field(routing, "chatgptAccountId")?, 256)?;
    Ok(ObservedAccount {
        fingerprint: digest(&(
            "observed-chatgpt-login-v1",
            email,
            plan,
            route,
            origin,
            workspace,
        ))?,
    })
}

/// Exact explicit context, shared by metadata and any later owned exec builder.
/// Callers resolve the selected executable first; no PATH/Desktop/home fallback.
#[derive(Clone)]
pub struct LaunchContext {
    program: PathBuf,
    workdir: PathBuf,
    home: PathBuf,
    fingerprint: [u8; 32],
    system_env: Vec<(String, std::ffi::OsString)>,
}
impl LaunchContext {
    pub fn new(program: &Path, workdir: &Path, home: &Path) -> Result<Self, MetadataError> {
        if !program.is_absolute() || !workdir.is_absolute() || !home.is_absolute() {
            return Err(MetadataError::InvalidContext);
        }
        let program = program
            .canonicalize()
            .map_err(|_| MetadataError::InvalidContext)?;
        let workdir = workdir
            .canonicalize()
            .map_err(|_| MetadataError::InvalidContext)?;
        let home = home
            .canonicalize()
            .map_err(|_| MetadataError::InvalidContext)?;
        if !program.is_file() || !workdir.is_dir() || !home.is_dir() {
            return Err(MetadataError::InvalidContext);
        }
        let system_env: Vec<_> = [
            "PATH",
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "TEMP",
            "TMP",
            "PATHEXT",
        ]
        .iter()
        .filter_map(|k| std::env::var_os(k).map(|v| ((*k).to_owned(), v)))
        .collect();
        let paths = (&program, &workdir, &home);
        let env_hash: Vec<_> = system_env
            .iter()
            .map(|(k, v)| (k, v.to_string_lossy().into_owned()))
            .collect();
        let fingerprint = digest(&("explicit-cli-context-v1", paths, env_hash))?;
        Ok(Self {
            program,
            workdir,
            home,
            fingerprint,
            system_env,
        })
    }
    fn command(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.program);
        c.args(args)
            .env_clear()
            .envs(self.system_env.iter().cloned())
            .env("CODEX_HOME", &self.home)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .current_dir(&self.workdir)
            .kill_on_drop(true);
        #[cfg(windows)]
        c.creation_flags(0x08000000);
        c
    }
    /// Command construction only. The existing gateway still owns policy,
    /// audit, cancellation and its one-dispatch gate; this grants none of them.
    pub fn request_command(
        &self,
        physical_model: &str,
        prompt: &str,
    ) -> Result<Command, MetadataError> {
        if physical_model.is_empty()
            || physical_model.len() > 512
            || physical_model.chars().any(char::is_control)
            || matches!(
                physical_model,
                "auto"
                    | "smart"
                    | "free"
                    | "cascade"
                    | "balanced"
                    | "fastest"
                    | "smartest"
                    | "reliable"
            )
            || prompt.len() > 8 * 1024 * 1024
        {
            return Err(MetadataError::InvalidContext);
        }
        Ok(self.command(&super::codex::exec_args(physical_model, prompt)))
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedBinding {
    connection: uuid::Uuid,
    context: [u8; 32],
    account: [u8; 32],
    epoch: u64,
}
impl ObservedBinding {
    pub(crate) fn observation_keys(&self) -> Result<([u8; 32], [u8; 32]), MetadataError> {
        Ok((
            self.account,
            digest(&(self.connection, self.context, self.epoch))?,
        ))
    }
}
struct ObservationState {
    connection: uuid::Uuid,
    context: [u8; 32],
    epoch: u64,
    current: Option<(ObservedAccount, Instant)>,
    closed: bool,
}
impl ObservationState {
    fn new(context: [u8; 32]) -> Self {
        Self {
            connection: uuid::Uuid::new_v4(),
            context,
            epoch: 0,
            current: None,
            closed: false,
        }
    }
    fn invalidate(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
        self.current = None;
    }
    fn accept(&mut self, account: ObservedAccount) -> Result<ObservedBinding, MetadataError> {
        if self.closed || self.epoch == u64::MAX {
            return Err(MetadataError::Disconnected);
        }
        if !matches!(self.current.as_ref(),Some((old,_)) if old==&account) {
            self.invalidate();
        }
        let binding = ObservedBinding {
            connection: self.connection,
            context: self.context,
            account: account.fingerprint,
            epoch: self.epoch,
        };
        self.current = Some((account, Instant::now()));
        Ok(binding)
    }
    fn matches_binding(&self, b: &ObservedBinding) -> bool {
        !self.closed
            && b.connection == self.connection
            && b.context == self.context
            && b.epoch == self.epoch
            && self
                .current
                .as_ref()
                .is_some_and(|(a, _)| a.fingerprint == b.account)
    }
    fn check(&self, b: &ObservedBinding, now: Instant) -> bool {
        self.matches_binding(b)
            && self
                .current
                .as_ref()
                .is_some_and(|(_, at)| now.saturating_duration_since(*at) < MAX_AGE)
    }
    fn notification(&mut self, value: &Value) -> Result<(), MetadataError> {
        match value.get("method").and_then(Value::as_str) {
            Some("account/updated") => {
                self.invalidate();
                let p = object(
                    value.get("params").ok_or(MetadataError::Protocol)?,
                    &["authMode", "planType"],
                )?;
                if p.get("authMode")
                    .is_some_and(|v| !v.is_null() && !v.is_string())
                    || p.get("planType")
                        .is_some_and(|v| !v.is_null() && !v.is_string())
                {
                    return Err(MetadataError::Protocol);
                }
                Ok(())
            }
            Some("account/login/completed") => {
                self.invalidate();
                let p = object(
                    value.get("params").ok_or(MetadataError::Protocol)?,
                    &["loginId", "success", "error"],
                )?;
                if p.get("success").and_then(Value::as_bool).is_none() {
                    return Err(MetadataError::Protocol);
                }
                Ok(())
            }
            Some("account/chatgptAuthTokens/refresh") => {
                self.invalidate();
                Err(MetadataError::Protocol)
            }
            Some(_) => Ok(()),
            None => Err(MetadataError::Protocol),
        }
    }
}

/// Owns a metadata-only helper. No model/login/refresh/catalog/rate-limit method
/// is representable on this API. Dropped reads invalidate and kill only it.
pub struct AccountSession {
    context: LaunchContext,
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    state: ObservationState,
    next_id: u64,
    timeout: Duration,
}
impl Drop for AccountSession {
    fn drop(&mut self) {
        self.close();
    }
}
impl AccountSession {
    pub async fn connect(context: LaunchContext, timeout: Duration) -> Result<Self, MetadataError> {
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(MetadataError::InvalidContext);
        }
        let mut command =
            context.command(&["app-server".into(), "--listen".into(), "stdio://".into()]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| MetadataError::Io)?;
        let input = child.stdin.take().ok_or(MetadataError::Io)?;
        let output = child.stdout.take().ok_or(MetadataError::Io)?;
        let state = ObservationState::new(context.fingerprint);
        let mut session = Self {
            context,
            child,
            input: Some(input),
            output: BufReader::new(output),
            state,
            next_id: 2,
            timeout,
        };
        let result=tokio::time::timeout(timeout,async{
            session.write(&json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"llmgw_local_metadata","version":"0.2.1"},"capabilities":{"experimentalApi":true}}})).await?;
            if !session.response(1).await?.is_object(){return Err(MetadataError::Protocol)}session.write(&json!({"method":"initialized","params":{}})).await
        }).await.map_err(|_|MetadataError::Timeout)?;
        result?;
        Ok(session)
    }
    fn close(&mut self) {
        self.state.closed = true;
        self.state.invalidate();
        self.input.take();
        if let Some(pid) = self.child.id() {
            crate::proc_util::kill_tree_blocking(pid);
        }
    }
    async fn write(&mut self, value: &Value) -> Result<(), MetadataError> {
        let mut line = serde_json::to_vec(value).map_err(|_| MetadataError::Protocol)?;
        line.push(b'\n');
        let input = self.input.as_mut().ok_or(MetadataError::Disconnected)?;
        input
            .write_all(&line)
            .await
            .map_err(|_| MetadataError::Io)?;
        input.flush().await.map_err(|_| MetadataError::Io)
    }
    async fn response(&mut self, id: u64) -> Result<Value, MetadataError> {
        for _ in 0..MAX_FRAMES {
            let mut bytes = Vec::new();
            let mut limited = (&mut self.output).take((MAX_FRAME + 1) as u64);
            let n = limited
                .read_until(b'\n', &mut bytes)
                .await
                .map_err(|_| MetadataError::Io)?;
            if n == 0 {
                return Err(MetadataError::Disconnected);
            }
            if n > MAX_FRAME || bytes.last() != Some(&b'\n') {
                return Err(MetadataError::Protocol);
            }
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| MetadataError::InvalidResponse)?;
            if value.get("method").is_some() {
                if value.get("id").is_some() {
                    return Err(MetadataError::Protocol);
                }
                self.state.notification(&value)?;
                continue;
            }
            if value.get("id").and_then(Value::as_u64) != Some(id) || value.get("error").is_some() {
                return Err(MetadataError::Protocol);
            }
            return value
                .get("result")
                .cloned()
                .ok_or(MetadataError::InvalidResponse);
        }
        Err(MetadataError::Protocol)
    }
    pub async fn read_account(&mut self) -> Result<ObservedBinding, MetadataError> {
        let timeout = self.timeout;
        let mut read = PendingRead {
            session: self,
            committed: false,
        };
        let result = tokio::time::timeout(timeout, async {
            let s = &mut read.session;
            if s.state.closed || s.child.try_wait().map_err(|_| MetadataError::Io)?.is_some() {
                return Err(MetadataError::Disconnected);
            }
            let epoch = s.state.epoch;
            let id = s.next_id;
            s.next_id = s.next_id.checked_add(1).ok_or(MetadataError::Protocol)?;
            s.write(&json!({"id":id,"method":"account/read","params":{"refreshToken":false}}))
                .await?;
            let result = s.response(id).await?;
            if s.state.epoch != epoch {
                return Err(MetadataError::Changed);
            }
            s.state.accept(parse_account_result(&result)?)
        })
        .await
        .map_err(|_| MetadataError::Timeout)
        .and_then(|r| r);
        if result.is_ok() {
            read.committed = true;
        }
        result
    }
    /// Final observation before existing dispatch; does not close the external
    /// auth-store TOCTOU window or lock a future separate CLI's credentials.
    pub async fn recheck(&mut self, expected: &ObservedBinding) -> Result<(), MetadataError> {
        // Cache freshness is not the session opt-in lifetime. A stale snapshot
        // never dispatches, but a fresh read may renew the same observed login.
        if !self.state.matches_binding(expected) {
            self.close();
            return Err(MetadataError::Changed);
        }
        if self.read_account().await? != *expected {
            self.close();
            return Err(MetadataError::Changed);
        }
        Ok(())
    }
    pub fn is_current(&self, binding: &ObservedBinding) -> bool {
        self.state.check(binding, Instant::now())
    }
    pub(crate) fn check_live(&mut self, binding: &ObservedBinding) -> bool {
        if !matches!(self.child.try_wait(), Ok(None)) {
            self.close();
            return false;
        }
        self.is_current(binding)
    }
    pub(crate) fn retire(&mut self) {
        self.close();
    }
    pub fn launch_context(&self) -> &LaunchContext {
        &self.context
    }
    pub async fn shutdown(mut self) -> Result<(), MetadataError> {
        self.state.closed = true;
        self.state.invalidate();
        self.input.take();
        match tokio::time::timeout(self.timeout, self.child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            _ => {
                self.close();
                let _ = self.child.wait().await;
                Err(MetadataError::Disconnected)
            }
        }
    }
}
struct PendingRead<'a> {
    session: &'a mut AccountSession,
    committed: bool,
}
impl Drop for PendingRead<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.session.close();
        }
    }
}

#[cfg(test)]
#[path = "codex_metadata_tests.rs"]
pub(crate) mod tests;
