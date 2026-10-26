//! Session-only personal CLI testing. This is not an entitlement issuer.
//! Codex metadata is an observed login binding, never a credential lease or
//! entitlement. Real CLI probing remains separately authorized from source tests.
use crate::{config::AppConfig, db::repo, domain::Provider, proxy::server::GatewayState};
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::Instant,
};

pub(crate) const HEADER: &str = "x-llmgw-personal-ticket";
#[derive(Clone, Default, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub target: Option<String>,
    pub trusted_source_ready: bool,
}
#[derive(Clone, PartialEq)]
struct SourceSnapshot {
    // Legacy private names: metadata hashes represent contextual login and local
    // observation epoch, not a universal human ID or actual credential version.
    principal: [u8; 32],
    generation: [u8; 32],
}
#[async_trait::async_trait]
trait TrustedSource: Send + Sync {
    async fn capture(&self, target: &str) -> Result<SourceSnapshot, String>;
    async fn recheck(&self, proof: &SourceSnapshot) -> Result<(), String>;
    fn ready(&self, _proof: &SourceSnapshot) -> bool {
        true
    }
    fn retire(&self) {}
    fn prepare_command(
        &self,
        _command: &mut tokio::process::Command,
        _model: &str,
        _prompt: &str,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Synthetic fixtures can hold their source lock. Metadata only checks the
    /// latest observation; it does not lock the external CLI credential store.
    fn with_current(
        &self,
        proof: &SourceSnapshot,
        spawn: &mut dyn FnMut() -> Result<tokio::process::Child, String>,
    ) -> Result<tokio::process::Child, String>;
}
#[path = "personal_test_metadata.rs"]
mod metadata_source;
async fn local_metadata_source(
    target: &str,
    context: crate::agent_upstream::codex_metadata::LaunchContext,
) -> Result<Arc<dyn TrustedSource>, String> {
    metadata_source::connect(target, context).await
}
/// There is no trusted terminal contract in this slice. Reply text and exit0
/// never manufacture VerifiedTerminal or any entitlement/settlement evidence.
#[derive(Default)]
pub(crate) struct Progress(AtomicU8);
impl Progress {
    pub(crate) fn outcome(&self) -> &'static str {
        match self.0.load(Ordering::SeqCst) {
            0 => "not_dispatched",
            1 => "dispatched_unknown",
            _ => "reply_unverified",
        }
    }
}
#[derive(Clone, PartialEq)]
struct Plan {
    target: String,
    provider: String,
    runtime: String,
    physical: String,
    program: String,
    provider_fingerprint: [u8; 32],
    runtime_fingerprint: [u8; 32],
}
fn fingerprint(value: &impl Serialize) -> Result<[u8; 32], String> {
    Ok(Sha256::digest(serde_json::to_vec(value).map_err(|_| "个人测试目标无法绑定")?).into())
}
fn explicit(model: &str) -> bool {
    !model.is_empty()
        && !matches!(
            model,
            "auto"
                | "smart"
                | "free"
                | "fastest"
                | "smartest"
                | "balanced"
                | "reliable"
                | "cascade"
        )
}
impl Plan {
    async fn capture(state: &GatewayState, target: &str) -> Result<Self, String> {
        let (id, alias) = target
            .split_once(':')
            .ok_or("个人测试必须明确选择供应商和模型")?;
        if !explicit(alias) {
            return Err("个人测试禁止自动或免费虚拟模型".into());
        }
        let provider = state
            .providers
            .read()
            .iter()
            .find(|p| p.id == id && p.enabled)
            .cloned()
            .ok_or("个人测试供应商已停用或不存在")?;
        let selected = provider
            .models
            .iter()
            .find(|m| {
                m.alias == alias && m.enabled && m.model_type == crate::domain::ModelType::Chat
            })
            .ok_or("个人测试模型已变化")?;
        if selected.overrides.as_ref().is_some_and(|o| !o.is_empty()) {
            return Err("个人测试 CLI 尚不支持此模型参数覆盖，请明确移除覆盖后再选择".into());
        }
        let runtime_id = provider
            .runtime_id
            .as_deref()
            .ok_or("个人测试仅支持已配置的 Codex CLI 账号运行时")?;
        let runtime = repo::get_agent_runtime(state.db.pool(), runtime_id)
            .await
            .map_err(|_| "个人测试运行时读取失败")?
            .ok_or("请选择现有 Codex CLI 账号运行时")?;
        runtime.validate()?;
        if !runtime.enabled || runtime.kind != "codex" {
            return Err("个人测试仅允许一个已启用 Codex CLI 运行时".into());
        }
        let (executable, aliases) =
            crate::agent_upstream::runtime::parse_options(runtime.options.as_ref())?;
        let physical = aliases
            .get(&selected.upstream)
            .cloned()
            .unwrap_or_else(|| selected.upstream.clone());
        if !explicit(&physical) {
            return Err("个人测试实际模型必须明确，不能是自动/免费选择器".into());
        }
        let program = crate::agent_upstream::CodexAdapter { executable }
            .program()
            .to_owned();
        Ok(Self {
            target: target.into(),
            provider: provider.id.clone(),
            runtime: runtime.id.clone(),
            physical,
            program,
            provider_fingerprint: fingerprint(&provider)?,
            runtime_fingerprint: fingerprint(&runtime)?,
        })
    }
}
#[derive(Clone)]
struct Session {
    revision: u64,
    plan: Plan,
    source: Option<Arc<dyn TrustedSource>>,
    proof: Option<SourceSnapshot>,
}
#[derive(Default)]
struct State {
    revision: u64,
    pending: HashMap<String, u64>,
    pending_sources: HashMap<String, Arc<dyn TrustedSource>>,
    sessions: HashMap<String, Session>,
    tickets: HashMap<String, Arc<Dispatch>>,
}
/// Cancellation of an enable future also retires its intent. No persistent
/// tombstone is needed: every later operation has a distinct process revision.
struct ConfigureIntent<'a> {
    controller: &'a Controller,
    owner: String,
    revision: u64,
}
impl Drop for ConfigureIntent<'_> {
    fn drop(&mut self) {
        let mut state = self.controller.state.lock();
        if state.pending.get(&self.owner) == Some(&self.revision) {
            state.pending.remove(&self.owner);
            if let Some(source) = state.pending_sources.remove(&self.owner) {
                source.retire();
            }
        }
    }
}
#[derive(Default)]
pub struct Controller {
    state: Mutex<State>,
    #[cfg(test)]
    source: parking_lot::RwLock<Option<Arc<dyn TrustedSource>>>,
}
impl Controller {
    async fn source(&self, _plan: &Plan) -> Result<Option<Arc<dyn TrustedSource>>, String> {
        #[cfg(test)]
        {
            Ok(self.source.read().clone())
        }
        #[cfg(not(test))]
        {
            metadata_source::for_plan(_plan).await.map(Some)
        }
    }
    pub fn status(&self, owner: &str) -> Status {
        self.state
            .lock()
            .sessions
            .get(owner)
            .map(|s| Status {
                enabled: true,
                target: Some(s.plan.target.clone()),
                trusted_source_ready: s
                    .source
                    .as_ref()
                    .zip(s.proof.as_ref())
                    .is_some_and(|(source, proof)| source.ready(proof)),
            })
            .unwrap_or_default()
    }
    pub async fn configure(
        &self,
        owner: &str,
        target: Option<&str>,
        gateway: &GatewayState,
    ) -> Result<Status, String> {
        if owner.is_empty() || owner.len() > 128 {
            return Err("个人测试窗口归属无效".into());
        }
        let (revision, retired) = {
            let mut state = self.state.lock();
            if target.is_some()
                && !state.sessions.contains_key(owner)
                && !state.pending.contains_key(owner)
                && state
                    .sessions
                    .keys()
                    .chain(state.pending.keys())
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    >= 8
            {
                return Err("个人测试窗口过多".into());
            }
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or("个人测试操作代次耗尽，请重启应用")?;
            let revision = state.revision;
            let mut retired = Vec::new();
            if let Some(old) = state.sessions.remove(owner) {
                if let Some(source) = old.source {
                    retired.push(source);
                }
            }
            if let Some(source) = state.pending_sources.remove(owner) {
                retired.push(source);
            }
            state.tickets.retain(|_, t| t.owner != owner);
            if target.is_some() {
                state.pending.insert(owner.into(), revision);
            } else {
                state.pending.remove(owner);
            }
            (revision, retired)
        };
        for source in retired {
            source.retire();
        }
        let _intent = ConfigureIntent {
            controller: self,
            owner: owner.into(),
            revision,
        };
        if let Some(target) = target {
            let plan = Plan::capture(gateway, target).await?;
            let source = self.source(&plan).await?;
            {
                let mut state = self.state.lock();
                if state.pending.get(owner) != Some(&revision) {
                    if let Some(source) = &source {
                        source.retire();
                    }
                    drop(state);
                    return Ok(self.status(owner));
                }
                if let Some(source) = &source {
                    state.pending_sources.insert(owner.into(), source.clone());
                }
            }
            let proof = if let Some(source) = &source {
                Some(source.capture(target).await?)
            } else {
                None
            };
            let mut state = self.state.lock();
            if state.pending.get(owner) != Some(&revision) {
                drop(state);
                return Ok(self.status(owner));
            }
            state.sessions.insert(
                owner.into(),
                Session {
                    revision,
                    plan,
                    source,
                    proof,
                },
            );
            state.pending_sources.remove(owner);
        }
        Ok(self.status(owner))
    }
    pub(crate) async fn issue(
        self: &Arc<Self>,
        owner: &str,
        input: &crate::desktop_chat::ChatInput,
        trace: uuid::Uuid,
        gateway: &GatewayState,
        progress: Arc<Progress>,
    ) -> Result<Ticket, String> {
        let session = self
            .state
            .lock()
            .sessions
            .get(owner)
            .cloned()
            .ok_or("个人测试默认关闭，请先明确启用并选择一个目标")?;
        if session.plan.target != input.model {
            return Err("个人测试目标与会话选择不一致".into());
        }
        if gateway.cfg.read().free_routing.enabled {
            return Err("严格免费模式没有可信权益证明，个人测试不能绕过".into());
        }
        let source = session
            .source
            .clone()
            .ok_or("Codex CLI 可信主体/凭据代次接口未就绪；未派发。个人测试开关不是免费证明")?;
        let proof = session.proof.clone().ok_or("个人测试缺少可信来源")?;
        if proof.principal == [0; 32] || proof.generation == [0; 32] {
            return Err("个人测试可信来源无效".into());
        }
        if Plan::capture(gateway, &input.model).await? != session.plan {
            return Err("个人测试目标来源已变化，请重新选择".into());
        }
        let body:crate::domain::ChatRequest=serde_json::from_value(serde_json::json!({"model":input.model,"messages":input.messages,"max_tokens":input.max_tokens,"stream":false})).map_err(|_|"个人测试请求无法绑定")?;
        let ctx = Arc::new(Dispatch {
            controller: self.clone(),
            owner: owner.into(),
            session,
            source,
            proof,
            body_fingerprint: fingerprint(&body)?,
            trace: trace.simple().to_string(),
            session_id: input.session_id.clone(),
            issued: Instant::now(),
            cfg: gateway.cfg.clone(),
            attempted: AtomicBool::new(false),
            progress,
        });
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        {
            let mut state = self.state.lock();
            state
                .tickets
                .retain(|_, t| t.issued.elapsed().as_secs() < 30);
            if state.tickets.len() >= 32 {
                return Err("个人测试请求过多".into());
            }
            state.tickets.insert(token.clone(), ctx.clone());
        }
        Ok(Ticket {
            controller: self.clone(),
            token,
            ctx,
        })
    }
    pub(crate) fn take(
        &self,
        token: &str,
        body: &crate::domain::ChatRequest,
        trace: &str,
        session: &str,
        client: Option<&str>,
    ) -> Result<Arc<Dispatch>, String> {
        let ctx = self
            .state
            .lock()
            .tickets
            .remove(token)
            .ok_or("个人测试内部归属无效或已消费")?;
        if client != Some("local-unified-key")
            || ctx.session.plan.target != body.model
            || ctx.body_fingerprint != fingerprint(body)?
            || ctx.trace != trace
            || ctx.session_id != session
            || ctx.issued.elapsed().as_secs() >= 30
        {
            return Err("个人测试请求归属不匹配".into());
        }
        Ok(ctx)
    }
}
pub(crate) struct Ticket {
    controller: Arc<Controller>,
    pub token: String,
    pub ctx: Arc<Dispatch>,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.controller.state.lock().tickets.remove(&self.token);
    }
}
pub(crate) struct Dispatch {
    controller: Arc<Controller>,
    owner: String,
    session: Session,
    source: Arc<dyn TrustedSource>,
    proof: SourceSnapshot,
    body_fingerprint: [u8; 32],
    trace: String,
    session_id: String,
    issued: Instant,
    cfg: Arc<parking_lot::RwLock<AppConfig>>,
    attempted: AtomicBool,
    progress: Arc<Progress>,
}
tokio::task_local! {static CURRENT:Arc<Dispatch>;}
pub(crate) fn current() -> Option<Arc<Dispatch>> {
    CURRENT.try_with(Clone::clone).ok()
}
pub(crate) async fn scope<F: Future>(ctx: Arc<Dispatch>, future: F) -> F::Output {
    CURRENT.scope(ctx, future).await
}
impl Dispatch {
    pub(crate) async fn before_runtime(
        &self,
        pool: &sqlx::SqlitePool,
        runtime: &crate::agent_upstream::ResolvedRuntime,
    ) -> Result<(), String> {
        if runtime.id != self.session.plan.runtime
            || runtime.kind != "codex"
            || runtime.adapter.id() != "codex"
        {
            return Err("个人测试最终运行时不匹配".into());
        }
        let current = repo::get_agent_runtime(pool, &runtime.id)
            .await
            .map_err(|_| "个人测试运行时复核失败")?
            .ok_or("个人测试运行时已删除")?;
        if fingerprint(&current)? != self.session.plan.runtime_fingerprint {
            return Err("个人测试完整运行时来源已变化".into());
        }
        self.source.recheck(&self.proof).await?;
        // Metadata I/O can wait too: don't use the earlier database snapshot.
        let current = repo::get_agent_runtime(pool, &runtime.id)
            .await
            .map_err(|_| "个人测试最终运行时复核失败")?
            .ok_or("个人测试运行时已删除")?;
        if fingerprint(&current)? != self.session.plan.runtime_fingerprint {
            return Err("个人测试运行时在元数据等待期间已变化".into());
        }
        Ok(())
    }
    pub(crate) async fn before_agent(
        &self,
        gateway: &GatewayState,
        provider: &Provider,
        runtime: &str,
    ) -> Result<(), String> {
        if provider.id != self.session.plan.provider
            || runtime != self.session.plan.runtime
            || fingerprint(provider)? != self.session.plan.provider_fingerprint
        {
            return Err("个人测试最终目标已变化".into());
        }
        if Plan::capture(gateway, &self.session.plan.target).await? != self.session.plan {
            return Err("个人测试运行时/别名/参数来源已变化".into());
        }
        Ok(())
    }
    pub(crate) fn received(&self) {
        self.progress.0.store(2, Ordering::SeqCst);
    }
    pub(crate) fn outcome(&self) -> &'static str {
        self.progress.outcome()
    }
}
/// The ordinary helper keeps exactly its former spawn operation when no private
/// context exists. A CLI label or caller header cannot construct this context.
pub(crate) fn spawn(
    command: &mut tokio::process::Command,
    program: &str,
    args: &[String],
    label: &str,
) -> Result<tokio::process::Child, String> {
    let Some(ctx) = current() else {
        return command.spawn().map_err(|e| e.to_string());
    };
    let state = ctx.controller.state.lock();
    let session = state.sessions.get(&ctx.owner).ok_or("个人测试已关闭")?;
    if session.revision != ctx.session.revision {
        return Err("个人测试选择代次已变化".into());
    }
    let cfg = ctx.cfg.read();
    if cfg.free_routing.enabled {
        return Err("严格免费模式禁止未验证个人测试".into());
    }
    if label != "codex"
        || program != ctx.session.plan.program
        || args
            != crate::agent_upstream::codex::exec_args(
                &ctx.session.plan.physical,
                args.last().ok_or("个人测试参数缺失")?,
            )
    {
        return Err("个人测试最终物理模型/CLI参数不匹配".into());
    }
    ctx.source.prepare_command(
        command,
        &ctx.session.plan.physical,
        args.last().ok_or("个人测试参数缺失")?,
    )?;
    ctx.source.with_current(&ctx.proof, &mut || {
        if ctx.attempted.swap(true, Ordering::SeqCst) {
            return Err("个人测试只允许一次自有CLI启动，不重试或回退".into());
        }
        let child = command.spawn().map_err(|e| e.to_string())?;
        ctx.progress.0.store(1, Ordering::SeqCst);
        Ok(child)
    })
}

#[cfg(test)]
#[path = "personal_test_tests.rs"]
mod tests;
