//! Artificial trusted source and owned CLI fixtures only; no production issuer.
use super::*;
use crate::{
    db::Db,
    desktop_chat::{ChatInput, ChatMessage, RequestControls},
    domain::{AgentRuntime, Provider},
    proxy_control::gateway::GatewayService,
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

struct ArtificialSource(Mutex<SourceSnapshot>);
impl ArtificialSource {
    fn new() -> Self {
        Self(Mutex::new(SourceSnapshot {
            principal: [1; 32],
            generation: [2; 32],
        }))
    }
}
#[async_trait::async_trait]
impl TrustedSource for ArtificialSource {
    async fn capture(&self, _target: &str) -> Result<SourceSnapshot, String> {
        Ok(self.0.lock().clone())
    }
    async fn recheck(&self, proof: &SourceSnapshot) -> Result<(), String> {
        if &*self.0.lock() == proof {
            Ok(())
        } else {
            Err("owned artificial source generation changed".into())
        }
    }
    fn with_current(
        &self,
        proof: &SourceSnapshot,
        spawn: &mut dyn FnMut() -> Result<tokio::process::Child, String>,
    ) -> Result<tokio::process::Child, String> {
        let current = self.0.lock();
        if &*current != proof {
            return Err("owned artificial source generation changed".into());
        }
        spawn()
    }
}
struct Fixture {
    root: PathBuf,
    state: Arc<GatewayState>,
    service: GatewayService,
    source: Arc<ArtificialSource>,
}

#[cfg(windows)]
#[tokio::test]
async fn local_metadata_consumer_uses_gateway_and_keeps_usage_unverified() {
    let metadata = crate::agent_upstream::codex_metadata::tests::OwnedFixture::new();
    let f = Fixture::new().await;
    let mut runtime = repo::get_agent_runtime(f.state.db.pool(), "owned-codex")
        .await
        .unwrap()
        .unwrap();
    runtime.options.as_mut().unwrap()["executable"] =
        json!(metadata.root.join("owned.cmd").to_string_lossy());
    repo::upsert_agent_runtime(f.state.db.pool(), &runtime)
        .await
        .unwrap();
    let source = local_metadata_source("owned-personal:owned-alias", metadata.context())
        .await
        .expect("implemented metadata adapter must feed the existing personal consumer");
    *f.state.personal_tests.source.write() = Some(source);
    f.enable().await;
    let reply = f.call().await;
    f.service.set_enabled(false).await.unwrap();
    let reply = reply.unwrap();
    assert_eq!(reply.content, "owned metadata-bound reply");
    assert_eq!(
        reply.personal_test_outcome.as_deref(),
        Some("reply_unverified")
    );
    assert!(reply.usage.is_none());
    assert_eq!(metadata.count("model-starts"), 1);
    assert_eq!(metadata.count("helper-starts"), 1);
    assert_eq!(reply.fallback_attempts, 0);
}
impl Fixture {
    async fn attach_metadata(
        &self,
        metadata: &crate::agent_upstream::codex_metadata::tests::OwnedFixture,
    ) {
        let mut runtime = repo::get_agent_runtime(self.state.db.pool(), "owned-codex")
            .await
            .unwrap()
            .unwrap();
        runtime.options.as_mut().unwrap()["executable"] =
            json!(metadata.root.join("owned.cmd").to_string_lossy());
        repo::upsert_agent_runtime(self.state.db.pool(), &runtime)
            .await
            .unwrap();
        *self.state.personal_tests.source.write() = Some(
            local_metadata_source("owned-personal:owned-alias", metadata.context())
                .await
                .unwrap(),
        );
    }
    async fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("llmgw-owned-personal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let script = root.join("owned.cmd");
        std::fs::write(&script,"@echo off\r\n@echo launch>>\"%~dp0starts.txt\"\r\n@echo %*>>\"%~dp0args.txt\"\r\n@if exist \"%~dp0fail\" exit /b 1\r\n@if exist \"%~dp0hang\" powershell.exe -NoProfile -Command \"Start-Sleep -Seconds 30\"\r\n@echo {\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"owned personal reply\"}}\r\n").unwrap();
        let db = Db::connect_in_memory().await.unwrap();
        let mut runtime = AgentRuntime::new("owned-codex", "codex", "owned artificial account");
        runtime.options = Some(
            json!({"executable":script.to_string_lossy(),"model_aliases":{"alias-source":"owned-physical"}}),
        );
        repo::upsert_agent_runtime(db.pool(), &runtime)
            .await
            .unwrap();
        let provider:Provider=serde_json::from_value(json!({"id":"owned-personal","name":"owned personal","dialect":"openai","base_url":"http://127.0.0.1:1/v1","api_key_enc":"","runtime_id":runtime.id,"enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"created_at":chrono::Utc::now(),"updated_at":chrono::Utc::now(),"models":[{"alias":"owned-alias","upstream":"alias-source","enabled":true,"model_type":"chat","context_window":32768,"supports_stream":true,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"price":null,"overrides":null}]})).unwrap();
        repo::upsert_provider(db.pool(), &provider).await.unwrap();
        let mut cfg = AppConfig {
            port: 0,
            failover_enabled: true,
            max_fallback_attempts: 3,
            ..Default::default()
        };
        cfg.smart_routing.enabled = false;
        let state = Arc::new(GatewayState::new(db, cfg));
        state.reload_providers().await.unwrap();
        let source = Arc::new(ArtificialSource::new());
        *state.personal_tests.source.write() = Some(source.clone());
        let service = GatewayService::new(state.clone());
        service.set_enabled(true).await.unwrap();
        Self {
            root,
            state,
            service,
            source,
        }
    }
    fn input(&self) -> ChatInput {
        ChatInput {
            personal_test: true,
            model: "owned-personal:owned-alias".into(),
            session_id: uuid::Uuid::new_v4().to_string(),
            max_tokens: 32,
            messages: vec![ChatMessage {
                reasoning_content: None,
                role: "user".into(),
                content: "owned explicit personal test".into(),
            }],
        }
    }
    fn starts(&self) -> usize {
        std::fs::read_to_string(self.root.join("starts.txt"))
            .unwrap_or_default()
            .lines()
            .count()
    }
    async fn enable(&self) {
        self.state
            .personal_tests
            .configure("main", Some("owned-personal:owned-alias"), &self.state)
            .await
            .unwrap();
    }
    async fn call(
        &self,
    ) -> Result<crate::desktop_chat::ChatResult, crate::desktop_chat::PersonalFailure> {
        let controls = Arc::new(RequestControls::default());
        let request = controls
            .register("main", &uuid::Uuid::new_v4().to_string())
            .unwrap();
        crate::desktop_chat::send_controlled_personal(&self.state, "main", request, self.input())
            .await
    }
    async fn add_other(&self) -> ChatInput {
        let script = self.root.join("other.cmd");
        std::fs::write(&script,"@echo off\r\n@echo launch>>\"%~dp0other-starts.txt\"\r\n@echo {\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"other owned reply\"}}\r\n").unwrap();
        let mut runtime = AgentRuntime::new("owned-other", "codex", "other owned account");
        runtime.options = Some(json!({"executable":script.to_string_lossy()}));
        repo::upsert_agent_runtime(self.state.db.pool(), &runtime)
            .await
            .unwrap();
        let mut provider = self.state.providers.read()[0].clone();
        provider.id = "owned-other".into();
        provider.runtime_id = Some(runtime.id);
        provider.priority += 1;
        repo::upsert_provider(self.state.db.pool(), &provider)
            .await
            .unwrap();
        self.state.reload_providers().await.unwrap();
        let mut input = self.input();
        input.personal_test = false;
        input.model = "owned-other:owned-alias".into();
        input
    }
    fn other_starts(&self) -> usize {
        std::fs::read_to_string(self.root.join("other-starts.txt"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}

#[cfg(windows)]
#[tokio::test]
async fn metadata_read_pending_enable_is_retired_by_disable_without_resurrection() {
    let metadata = crate::agent_upstream::codex_metadata::tests::OwnedFixture::new();
    let f = Fixture::new().await;
    f.attach_metadata(&metadata).await;
    metadata.mode("hold");
    let mut enabling = Box::pin(f.state.personal_tests.configure(
        "main",
        Some("owned-personal:owned-alias"),
        &f.state,
    ));
    let held = async {
        while !metadata.root.join("held").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::select! {r=&mut enabling=>panic!("expected pending metadata: {}",r.is_ok()),r=tokio::time::timeout(Duration::from_secs(3),held)=>r.unwrap()}
    assert!(
        !f.state
            .personal_tests
            .configure("main", None, &f.state)
            .await
            .unwrap()
            .enabled
    );
    let result = tokio::time::timeout(Duration::from_millis(300), enabling)
        .await
        .expect("disable must retire metadata wait, not wait for its timeout");
    assert!(result.is_err() || !result.unwrap().enabled);
    assert!(!f.state.personal_tests.status("main").enabled);
    assert!(f
        .state
        .personal_tests
        .state
        .lock()
        .pending_sources
        .is_empty());
    f.service.set_enabled(false).await.unwrap();
    assert_eq!(metadata.count("model-starts"), 0);
}
#[cfg(windows)]
#[tokio::test]
async fn metadata_and_runtime_changes_during_final_recheck_prevent_owned_gateway_dispatch() {
    for change in ["workspace", "runtime"] {
        let metadata = crate::agent_upstream::codex_metadata::tests::OwnedFixture::new();
        let f = Fixture::new().await;
        f.attach_metadata(&metadata).await;
        f.enable().await;
        metadata.mode("hold");
        let mut request = Box::pin(f.call());
        let held = async {
            while !metadata.root.join("held").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::select! {r=&mut request=>panic!("expected pending metadata: {r:?}"),r=tokio::time::timeout(Duration::from_secs(3),held)=>r.unwrap()}
        if change == "workspace" {
            let path = metadata.root.join("account.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            value["workspaceRouting"]["chatgptAccountId"] = json!("replacement-owned-workspace");
            std::fs::write(path, value.to_string()).unwrap();
        } else {
            let mut runtime = repo::get_agent_runtime(f.state.db.pool(), "owned-codex")
                .await
                .unwrap()
                .unwrap();
            runtime.options.as_mut().unwrap()["model_aliases"]["alias-source"] =
                json!("replacement-owned-model");
            repo::upsert_agent_runtime(f.state.db.pool(), &runtime)
                .await
                .unwrap();
        }
        std::fs::write(metadata.root.join("release"), "").unwrap();
        let failure = request.await.unwrap_err();
        assert_eq!(failure.outcome, "not_dispatched");
        assert_eq!(metadata.count("model-starts"), 0);
        f.service.set_enabled(false).await.unwrap();
    }
}
#[cfg(windows)]
#[tokio::test]
async fn cancelled_metadata_recheck_isolates_concurrent_ordinary_gateway_request() {
    let metadata = crate::agent_upstream::codex_metadata::tests::OwnedFixture::new();
    let f = Fixture::new().await;
    let other = f.add_other().await;
    f.attach_metadata(&metadata).await;
    f.enable().await;
    metadata.mode("hold");
    let controls = Arc::new(RequestControls::default());
    let id = uuid::Uuid::new_v4().to_string();
    let request = controls.register("main", &id).unwrap();
    let mut personal = Box::pin(crate::desktop_chat::send_controlled_personal(
        &f.state,
        "main",
        request,
        f.input(),
    ));
    let held = async {
        while !metadata.root.join("held").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::select! {r=&mut personal=>panic!("expected pending metadata: {r:?}"),r=tokio::time::timeout(Duration::from_secs(3),held)=>r.unwrap()}
    assert!(!controls.cancel("other-window", &id).unwrap());
    let cfg = f.state.cfg_snapshot();
    let listener = f.state.listener_snapshot().unwrap().bound_addr;
    let reply = crate::desktop_chat::send(&cfg, listener, other)
        .await
        .unwrap();
    assert_eq!(reply.content, "other owned reply");
    assert!(controls.cancel("main", &id).unwrap());
    let failure = tokio::time::timeout(Duration::from_secs(3), personal)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.message, crate::desktop_chat::CANCELLED);
    assert_eq!(failure.outcome, "not_dispatched");
    assert!(!controls.cancel("main", &id).unwrap());
    f.service.set_enabled(false).await.unwrap();
    assert_eq!(metadata.count("model-starts"), 0);
    assert_eq!(f.other_starts(), 1);
    let rows = repo::recent_requests(f.state.db.pool(), 10).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r["status"] == 499));
    assert!(rows.iter().any(|r| r["status"] == 200));
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(files) = std::fs::read_dir(&self.root) {
            for file in files.flatten() {
                if file.file_type().is_ok_and(|kind| kind.is_file()) {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
        let _ = std::fs::remove_dir(&self.root);
    }
}

#[cfg(windows)]
#[tokio::test]
async fn preparation_db_wait_is_cancellable_before_owned_dispatch() {
    let f = Fixture::new().await;
    f.enable().await;
    let controls = Arc::new(RequestControls::default());
    let id = uuid::Uuid::new_v4().to_string();
    let request = controls.register("main", &id).unwrap();
    let held = f.state.db.pool().acquire().await.unwrap();
    let mut future = Box::pin(crate::desktop_chat::send_controlled_personal(
        &f.state,
        "main",
        request,
        f.input(),
    ));
    assert!(futures_util::poll!(future.as_mut()).is_pending());
    assert!(controls.cancel("main", &id).unwrap());
    let result = tokio::time::timeout(Duration::from_millis(300), future.as_mut()).await;
    drop(future);
    drop(held);
    assert!(
        controls.register("main", &id).is_ok(),
        "controlled registration must be released"
    );
    assert!(f.state.personal_tests.state.lock().tickets.is_empty());
    assert_eq!(f.starts(), 0);
    f.service.set_enabled(false).await.unwrap();
    assert!(
        result.is_ok(),
        "cancel must resolve while isolated DB remains held, not wait for issue"
    );
    let failure = result.unwrap().unwrap_err();
    assert_eq!(failure.message, crate::desktop_chat::CANCELLED);
    assert_eq!(failure.outcome, "not_dispatched");
}

#[cfg(windows)]
#[tokio::test]
async fn artificial_trusted_owned_cli_reply_is_one_launch_bound_alias_and_unknown_usage() {
    let f = Fixture::new().await;
    f.enable().await;
    let reply = f.call().await;
    f.service.set_enabled(false).await.unwrap();
    let reply = reply.unwrap();
    eprintln!(
        "OWNED_PERSONAL_POSITIVE starts={} outcome={:?} usage={:?}",
        f.starts(),
        reply.personal_test_outcome,
        reply.usage
    );
    assert_eq!(f.starts(), 1);
    assert_eq!(reply.content, "owned personal reply");
    assert_eq!(
        reply.personal_test_outcome.as_deref(),
        Some("reply_unverified")
    );
    assert!(std::fs::read_to_string(f.root.join("args.txt"))
        .unwrap()
        .contains("-m owned-physical"));
    assert!(
        reply.usage.is_none(),
        "CLI text and exit0 supply no usage evidence"
    );
    let rows = repo::recent_requests(f.state.db.pool(), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["prompt_tokens"].is_null());
    assert!(rows[0]["completion_tokens"].is_null());
    assert!(rows[0]["cost"].is_null());
    assert_eq!(rows[0]["fallback_attempts"], 0);
}

#[cfg(windows)]
#[tokio::test]
async fn owned_cli_failure_exposes_dispatched_unknown_while_identity_rejection_is_not_dispatched() {
    let failed = Fixture::new().await;
    failed.add_other().await;
    failed.enable().await;
    std::fs::write(failed.root.join("fail"), "").unwrap();
    let error = failed.call().await.unwrap_err();
    failed.service.set_enabled(false).await.unwrap();
    assert_eq!(failed.starts(), 1);
    assert_eq!(error.outcome, "dispatched_unknown");
    assert_eq!(
        failed.other_starts(),
        0,
        "no backup dispatch after owned CLI failure"
    );
    let wire = serde_json::to_value(crate::desktop_chat::CommandError::Personal(error)).unwrap();
    assert_eq!(wire["outcome"], "dispatched_unknown");
    assert!(wire["message"].is_string());
    let denied = Fixture::new().await;
    denied.enable().await;
    denied.source.0.lock().generation = [3; 32];
    let error = denied.call().await.unwrap_err();
    denied.service.set_enabled(false).await.unwrap();
    assert_eq!(denied.starts(), 0);
    assert_eq!(error.outcome, "not_dispatched");
    assert_eq!(
        serde_json::to_value(crate::desktop_chat::CommandError::Ordinary(
            "legacy string".into()
        ))
        .unwrap(),
        json!("legacy string")
    );
}

#[cfg(windows)]
#[tokio::test]
async fn source_generation_changes_during_owned_database_wait_are_checked_before_spawn() {
    let f = Fixture::new().await;
    f.enable().await;
    let controls = Arc::new(RequestControls::default());
    let request = controls
        .register("main", &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let held = f.state.db.pool().acquire().await.unwrap();
    let mut future = Box::pin(crate::desktop_chat::send_controlled_personal(
        &f.state,
        "main",
        request,
        f.input(),
    ));
    assert!(futures_util::poll!(future.as_mut()).is_pending());
    f.source.0.lock().generation = [4; 32];
    drop(held);
    let error = future.await.unwrap_err();
    f.service.set_enabled(false).await.unwrap();
    assert_eq!(f.starts(), 0);
    assert_eq!(error.outcome, "not_dispatched");
}

#[cfg(windows)]
#[tokio::test]
async fn full_runtime_alias_change_and_strict_free_both_refuse_without_owned_start() {
    let f = Fixture::new().await;
    f.enable().await;
    let mut runtime = repo::get_agent_runtime(f.state.db.pool(), "owned-codex")
        .await
        .unwrap()
        .unwrap();
    runtime.options.as_mut().unwrap()["model_aliases"]["alias-source"] =
        json!("replacement-physical");
    repo::upsert_agent_runtime(f.state.db.pool(), &runtime)
        .await
        .unwrap();
    assert_eq!(f.call().await.unwrap_err().outcome, "not_dispatched");
    assert_eq!(f.starts(), 0);
    f.service.set_enabled(false).await.unwrap();
    let free = Fixture::new().await;
    free.enable().await;
    free.state.cfg.write().free_routing.enabled = true;
    assert_eq!(free.call().await.unwrap_err().outcome, "not_dispatched");
    assert!(free.state.cfg.read().free_routing.enabled);
    assert_eq!(free.starts(), 0);
    free.service.set_enabled(false).await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn unavailable_real_trusted_interface_and_new_controller_default_remain_closed() {
    let f = Fixture::new().await;
    *f.state.personal_tests.source.write() = None;
    f.enable().await;
    let status = f.state.personal_tests.status("main");
    assert!(status.enabled && !status.trusted_source_ready);
    assert_eq!(f.call().await.unwrap_err().outcome, "not_dispatched");
    assert_eq!(f.starts(), 0);
    assert!(
        !Controller::default().status("main").enabled,
        "restart cannot restore opt-in"
    );
    assert!(f
        .state
        .personal_tests
        .configure("main", Some("free"), &f.state)
        .await
        .is_err());
    f.service.set_enabled(false).await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn opaque_ticket_cannot_be_forged_or_reused_with_changed_body() {
    let f = Fixture::new().await;
    f.enable().await;
    let input = f.input();
    let trace = uuid::Uuid::new_v4();
    let ticket = f
        .state
        .personal_tests
        .issue(
            "main",
            &input,
            trace,
            &f.state,
            Arc::new(Progress::default()),
        )
        .await
        .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let cfg = f.state.cfg_snapshot();
    let listener = f.state.listener_snapshot().unwrap().bound_addr;
    let body = json!({"model":input.model,"messages":[{"role":"user","content":"tampered owned request"}],"max_tokens":32,"stream":false});
    let send = |token: String| {
        client
            .post(format!("http://{listener}/v1/chat/completions"))
            .bearer_auth(&cfg.unified_key)
            .header(HEADER, token)
            .header("x-trace-id", trace.simple().to_string())
            .header("x-session-id", &input.session_id)
            .json(&body)
            .send()
    };
    assert_eq!(
        send("self-claimed-personal".into())
            .await
            .unwrap()
            .status()
            .as_u16(),
        401
    );
    let tampered = send(ticket.token.clone()).await.unwrap().status().as_u16();
    assert_eq!(
        send(ticket.token.clone()).await.unwrap().status().as_u16(),
        401,
        "consumed invalid-body ticket cannot be replayed"
    );
    f.service.set_enabled(false).await.unwrap();
    assert_eq!(tampered,401,"backend ticket must bind the actual selected request body, not just public trace/model IDs");
    assert_eq!(f.starts(), 0);
    assert!(f.state.personal_tests.state.lock().tickets.is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn disable_wins_over_same_owner_enable_waiting_on_owned_database() {
    let f = Fixture::new().await;
    let held = f.state.db.pool().acquire().await.unwrap();
    let mut enabling = Box::pin(f.state.personal_tests.configure(
        "main",
        Some("owned-personal:owned-alias"),
        &f.state,
    ));
    assert!(futures_util::poll!(enabling.as_mut()).is_pending());
    let disabled = f
        .state
        .personal_tests
        .configure("main", None, &f.state)
        .await
        .unwrap();
    assert!(!disabled.enabled);
    assert!(!f.state.personal_tests.status("main").enabled);
    drop(held);
    let old_enable = enabling.await;
    let actual = f.state.personal_tests.status("main");
    f.service.set_enabled(false).await.unwrap();
    eprintln!(
        "OWNED_ENABLE_DISABLE_RACE stale_enable_ok={} final_enabled={} starts={}",
        old_enable.is_ok(),
        actual.enabled,
        f.starts()
    );
    assert!(
        !actual.enabled,
        "a stale enable may not resurrect a completed disable"
    );
    assert_eq!(f.starts(), 0);
    assert!(f.state.personal_tests.state.lock().pending.is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn owned_cli_cancellation_isolates_concurrent_ordinary_request_and_retains_unknown() {
    let f = Fixture::new().await;
    let other = f.add_other().await;
    f.enable().await;
    std::fs::write(f.root.join("hang"), "").unwrap();
    let controls = Arc::new(RequestControls::default());
    let id = uuid::Uuid::new_v4().to_string();
    let request = controls.register("main", &id).unwrap();
    let mut personal = Box::pin(crate::desktop_chat::send_controlled_personal(
        &f.state,
        "main",
        request,
        f.input(),
    ));
    let launched = async {
        while f.starts() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::select! {result=&mut personal=>panic!("owned hanging CLI completed unexpectedly: {result:?}"),result=tokio::time::timeout(Duration::from_secs(3),launched)=>result.unwrap()}
    assert!(!controls.cancel("other-window", &id).unwrap());
    let cfg = f.state.cfg_snapshot();
    let addr = f.state.listener_snapshot().unwrap().bound_addr;
    let other_reply = crate::desktop_chat::send(&cfg, addr, other).await.unwrap();
    assert_eq!(other_reply.content, "other owned reply");
    assert!(controls.cancel("main", &id).unwrap());
    assert!(!controls.cancel("main", &id).unwrap());
    let error = tokio::time::timeout(Duration::from_secs(3), personal)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.message, crate::desktop_chat::CANCELLED);
    assert_eq!(error.outcome, "dispatched_unknown");
    assert!(!controls.cancel("main", &id).unwrap());
    f.service.set_enabled(false).await.unwrap();
    assert_eq!(f.starts(), 1);
    assert_eq!(f.other_starts(), 1);
    assert!(f.state.personal_tests.state.lock().tickets.is_empty());
    let rows = repo::recent_requests(f.state.db.pool(), 10).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r["status"] == 200));
    let cancelled = rows.iter().find(|r| r["status"] == 499).unwrap();
    assert!(cancelled["cost"].is_null());
    assert!(cancelled["prompt_tokens"].is_null() && cancelled["completion_tokens"].is_null());
    assert_eq!(
        cancelled["attempts"][0]["personal_test"]["outcome"],
        "dispatched_unknown"
    );
}
