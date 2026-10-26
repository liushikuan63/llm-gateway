//! Async metadata producer for the existing personal gateway consumer.
//! A final read is observational, not an atomic external credential lease.
use super::*;
use crate::agent_upstream::codex_metadata::{AccountSession, LaunchContext, ObservedBinding};
#[cfg(not(test))]
use std::path::{Path, PathBuf};
use std::{process::Stdio, time::Duration};

struct MetadataSource {
    target: String,
    context: LaunchContext,
    client: tokio::sync::Mutex<AccountSession>,
    cached: Mutex<Option<(SourceSnapshot, ObservedBinding)>>,
    revoked: AtomicBool,
    retired: tokio::sync::Notify,
}
fn snapshot(b: &ObservedBinding) -> Result<SourceSnapshot, String> {
    let (principal, generation) = b.observation_keys().map_err(|e| e.to_string())?;
    Ok(SourceSnapshot {
        principal,
        generation,
    })
}
pub(super) async fn connect(
    target: &str,
    context: LaunchContext,
) -> Result<Arc<dyn TrustedSource>, String> {
    let client = AccountSession::connect(context.clone(), Duration::from_secs(3))
        .await
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(MetadataSource {
        target: target.into(),
        context,
        client: tokio::sync::Mutex::new(client),
        cached: Mutex::new(None),
        revoked: AtomicBool::new(false),
        retired: tokio::sync::Notify::new(),
    }))
}
#[cfg(not(test))]
pub(super) async fn for_plan(plan: &Plan) -> Result<Arc<dyn TrustedSource>, String> {
    // Controller.configure reaches this factory only after explicit per-window
    // product opt-in and target validation. Developer tool execution approvals
    // constrain our probes, not the shipped user's ability to opt in.
    super::local_metadata_source(&plan.target, resolve_context(plan)?).await
}
#[cfg(not(test))]
fn resolve_context(plan: &Plan) -> Result<LaunchContext, String> {
    let program = resolve_program(&plan.program)?;
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".codex")))
        .ok_or("metadata_context_unavailable")?;
    let workdir = crate::mcp::stdio::isolated_workdir("agent-codex");
    std::fs::create_dir_all(&workdir).map_err(|_| "metadata_workdir_unavailable")?;
    let context = LaunchContext::new(&program, &workdir, &home).map_err(|e| e.to_string())?;
    Ok(context)
}
#[cfg(not(test))]
fn resolve_program(program: &str) -> Result<PathBuf, String> {
    let p = Path::new(program);
    if p.is_absolute() {
        return p
            .canonicalize()
            .map_err(|_| "metadata_selected_cli_unavailable".into());
    }
    if program.is_empty() || p.components().count() != 1 {
        return Err("metadata_cli_requires_absolute_or_bare_path".into());
    }
    let paths = std::env::var_os("PATH").ok_or("metadata_selected_cli_unavailable")?;
    let mut names = vec![program.to_owned()];
    #[cfg(windows)]
    if p.extension().is_none() {
        names = vec![
            format!("{program}.exe"),
            format!("{program}.cmd"),
            format!("{program}.bat"),
            program.to_owned(),
        ];
    }
    for folder in std::env::split_paths(&paths) {
        for name in &names {
            let candidate = folder.join(name);
            if candidate.is_file() {
                return candidate
                    .canonicalize()
                    .map_err(|_| "metadata_selected_cli_unavailable".into());
            }
        }
    }
    Err("metadata_selected_cli_unavailable".into())
}
#[async_trait::async_trait]
impl TrustedSource for MetadataSource {
    async fn capture(&self, target: &str) -> Result<SourceSnapshot, String> {
        if target != self.target || self.revoked.load(Ordering::SeqCst) {
            return Err("metadata_selection_retired".into());
        }
        let mut client = self.client.lock().await;
        let retired = self.retired.notified();
        tokio::pin!(retired);
        retired.as_mut().enable();
        if self.revoked.load(Ordering::SeqCst) {
            return Err("metadata_selection_retired".into());
        }
        let binding = tokio::select! {biased;_= &mut retired=>return Err("metadata_selection_retired".into()),r=client.read_account()=>r.map_err(|e|e.to_string())?};
        if self.revoked.load(Ordering::SeqCst) {
            client.retire();
            return Err("metadata_selection_retired".into());
        }
        let proof = snapshot(&binding)?;
        *self.cached.lock() = Some((proof.clone(), binding));
        Ok(proof)
    }
    async fn recheck(&self, proof: &SourceSnapshot) -> Result<(), String> {
        let binding = self
            .cached
            .lock()
            .as_ref()
            .filter(|(p, _)| p == proof)
            .map(|(_, b)| b.clone())
            .ok_or("metadata_observation_changed")?;
        let mut client = self.client.lock().await;
        let retired = self.retired.notified();
        tokio::pin!(retired);
        retired.as_mut().enable();
        if self.revoked.load(Ordering::SeqCst) {
            return Err("metadata_selection_retired".into());
        }
        let result = tokio::select! {biased;_=&mut retired=>Err("metadata_selection_retired".into()),r=client.recheck(&binding)=>r.map_err(|e|e.to_string())};
        if result.is_err() {
            self.cached.lock().take();
        }
        result
    }
    fn ready(&self, proof: &SourceSnapshot) -> bool {
        if self.revoked.load(Ordering::SeqCst) {
            return false;
        }
        let b = self
            .cached
            .lock()
            .as_ref()
            .filter(|(p, _)| p == proof)
            .map(|(_, b)| b.clone());
        b.is_some_and(|b| self.client.try_lock().is_ok_and(|mut c| c.check_live(&b)))
    }
    fn retire(&self) {
        self.revoked.store(true, Ordering::SeqCst);
        self.cached.lock().take();
        self.retired.notify_waiters();
        if let Ok(mut c) = self.client.try_lock() {
            c.retire();
        }
    }
    fn prepare_command(
        &self,
        command: &mut tokio::process::Command,
        model: &str,
        prompt: &str,
    ) -> Result<(), String> {
        *command = self
            .context
            .request_command(model, prompt)
            .map_err(|e| e.to_string())?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(())
    }
    fn with_current(
        &self,
        proof: &SourceSnapshot,
        spawn: &mut dyn FnMut() -> Result<tokio::process::Child, String>,
    ) -> Result<tokio::process::Child, String> {
        if self.revoked.load(Ordering::SeqCst) {
            return Err("metadata_selection_retired".into());
        }
        let b = self
            .cached
            .lock()
            .as_ref()
            .filter(|(p, _)| p == proof)
            .map(|(_, b)| b.clone())
            .ok_or("metadata_observation_changed")?;
        let mut client = self
            .client
            .try_lock()
            .map_err(|_| "metadata_check_in_progress")?;
        if !client.check_live(&b) {
            return Err("metadata_observation_expired_or_disconnected".into());
        }
        spawn()
    }
}
