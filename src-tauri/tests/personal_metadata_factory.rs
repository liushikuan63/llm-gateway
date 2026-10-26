//! Production library (not cfg(test)): explicit opt-in -> actual metadata factory
//! -> controlled desktop send -> authenticated loopback Gateway -> owned fake CLI.
//! This target has one test: environment changes stay in its isolated OS process.
#![cfg(windows)]
use llm_gateway_lib::{
    config::AppConfig,
    db::{repo, Db},
    desktop_chat::{self, ChatInput, ChatMessage, RequestControls},
    domain::{AgentRuntime, Provider},
    proxy::server::GatewayState,
    proxy_control::gateway::GatewayService,
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

struct Owned {
    root: PathBuf,
    prior_env: Vec<(&'static str, Option<std::ffi::OsString>)>,
}
impl Owned {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "llmgw-owned-production-factory-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        for dir in ["home", "tmp"] {
            std::fs::create_dir(root.join(dir)).unwrap();
        }
        let prior_env = ["CODEX_HOME", "TEMP", "TMP"]
            .into_iter()
            .map(|k| (k, std::env::var_os(k)))
            .collect();
        for (k, dir) in [("CODEX_HOME", "home"), ("TEMP", "tmp"), ("TMP", "tmp")] {
            std::env::set_var(k, root.join(dir));
        }
        std::fs::write(
            root.join("owned.cmd"),
            "@echo off\r\nnode \"%~dp0owned.cjs\" %*\r\n",
        )
        .unwrap();
        std::fs::write(root.join("account.json"),json!({"account":{"type":"chatgpt","email":"owned@example.test","planType":"unknown"},"requiresOpenaiAuth":true,"workspaceRouting":{"accountRoutingOverride":"NO_CONSTRAINT","backendOrigin":"https://owned.invalid","chatgptAccountId":"owned-workspace"}}).to_string()).unwrap();
        std::fs::write(root.join("owned.cjs"),r#"const fs=require('node:fs'),path=require('node:path'),readline=require('node:readline');
const root=__dirname,add=(f,v)=>fs.appendFileSync(path.join(root,f),JSON.stringify(v)+'\n');
const args=process.argv.slice(2);add('launches.jsonl',{args,pid:process.pid,cwd:process.cwd(),home:process.env.CODEX_HOME});
if(args[0]==='exec'){
 add('model-starts',1);
 if(fs.existsSync(path.join(root,'fail')))process.exit(9);
 console.log(JSON.stringify({type:'item.completed',item:{type:'agent_message',text:'owned production-factory reply'}}));process.exit(0);
}
if(args.join(' ')!=='app-server --listen stdio://')process.exit(7);
add('helper-starts',1);const rl=readline.createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);add('methods.jsonl',{method:r.method,params:r.params});
if(r.method==='initialize'){console.log(JSON.stringify({id:r.id,result:{userAgent:'owned-only'}}));return;}
if(r.method==='initialized')return;
if(r.method!=='account/read'||r.params.refreshToken!==false)process.exit(8);
console.log(JSON.stringify({id:r.id,result:JSON.parse(fs.readFileSync(path.join(root,'account.json'),'utf8'))}));
});rl.on('close',()=>process.exit(0));
"#).unwrap();
        Self { root, prior_env }
    }
    fn count(&self, name: &str) -> usize {
        std::fs::read_to_string(self.root.join(name))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        for (k, v) in &self.prior_env {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        // Preserve failed fixtures for review. No user profile or key is copied.
        eprintln!("OWNED_PRODUCTION_FACTORY_FIXTURE={}", self.root.display());
    }
}
async fn call(
    state: &GatewayState,
    input: ChatInput,
) -> Result<desktop_chat::ChatResult, desktop_chat::PersonalFailure> {
    let controls = Arc::new(RequestControls::default());
    let request = controls
        .register("main", &uuid::Uuid::new_v4().to_string())
        .unwrap();
    desktop_chat::send_controlled_personal(state, "main", request, input).await
}
fn input() -> ChatInput {
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
#[tokio::test]
async fn explicit_opt_in_reaches_production_factory_owned_cli_and_gateway_without_test_injection() {
    use futures_util::FutureExt;
    let owned = Owned::new();
    let db = Db::connect_in_memory().await.unwrap();
    let mut runtime = AgentRuntime::new("owned-codex", "codex", "owned fake CLI only");
    runtime.options = Some(
        json!({"executable":owned.root.join("owned.cmd").to_string_lossy(),"model_aliases":{"alias-source":"owned-physical"}}),
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
    let service = GatewayService::new(state.clone());
    service.set_enabled(true).await.unwrap();
    let exercise = async {
        assert!(!state.personal_tests.status("main").enabled);
        assert_eq!(
            call(&state, input()).await.unwrap_err().outcome,
            "not_dispatched"
        );
        assert_eq!(owned.count("helper-starts"), 0);
        assert_eq!(owned.count("model-starts"), 0);
        let status = state
            .personal_tests
            .configure("main", Some("owned-personal:owned-alias"), &state)
            .await
            .expect("explicit product opt-in must reach the actual production metadata factory");
        assert!(status.enabled && status.trusted_source_ready);
        let reply = call(&state, input()).await.unwrap();
        assert_eq!(reply.content, "owned production-factory reply");
        assert!(reply.usage.is_none());
        assert_eq!(
            reply.personal_test_outcome.as_deref(),
            Some("reply_unverified")
        );
        assert_eq!(reply.fallback_attempts, 0);
        assert_eq!(owned.count("helper-starts"), 1);
        assert_eq!(owned.count("model-starts"), 1);
        println!("FACTORY_GROUP=default_off_zero_launch_then_explicit_opt_in_one_owned_model_unknown_usage");
        std::fs::write(owned.root.join("fail"), "").unwrap();
        let error = call(&state, input()).await.unwrap_err();
        assert_eq!(error.outcome, "dispatched_unknown");
        assert_eq!(
            owned.count("model-starts"),
            2,
            "one failed launch, no retry or fallback"
        );
        std::fs::remove_file(owned.root.join("fail")).unwrap();
        state.cfg.write().free_routing.enabled = true;
        assert_eq!(
            call(&state, input()).await.unwrap_err().outcome,
            "not_dispatched"
        );
        assert_eq!(owned.count("model-starts"), 2);
        assert!(state.cfg.read().free_routing.enabled);
        state.cfg.write().free_routing.enabled = false;
        println!("FACTORY_GROUP=failed_owned_launch_no_replay_and_strict_free_no_launch");
        let path = owned.root.join("account.json");
        let mut account: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        account["workspaceRouting"]["chatgptAccountId"] = json!("replacement-owned-workspace");
        std::fs::write(path, account.to_string()).unwrap();
        assert_eq!(
            call(&state, input()).await.unwrap_err().outcome,
            "not_dispatched"
        );
        assert_eq!(owned.count("model-starts"), 2);
        assert!(
            !state
                .personal_tests
                .configure("main", None, &state)
                .await
                .unwrap()
                .enabled
        );
        assert!(
            !llm_gateway_lib::personal_test::Controller::default()
                .status("main")
                .enabled
        );
        println!("FACTORY_GROUP=changed_observed_identity_no_launch_disable_and_restart_off");
        let launches: Vec<serde_json::Value> =
            std::fs::read_to_string(owned.root.join("launches.jsonl"))
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
        assert_eq!(launches.len(), 3); // one metadata helper and two explicit owned model launches
        for launch in &launches {
            assert_eq!(
                PathBuf::from(launch["home"].as_str().unwrap())
                    .canonicalize()
                    .unwrap(),
                owned.root.join("home").canonicalize().unwrap()
            );
            assert_eq!(
                PathBuf::from(launch["cwd"].as_str().unwrap())
                    .canonicalize()
                    .unwrap(),
                owned
                    .root
                    .join("tmp/llm-gateway-mcp/agent-codex")
                    .canonicalize()
                    .unwrap()
            );
        }
        for launch in launches.iter().filter(|l| l["args"][0] == "exec") {
            let args = launch["args"].as_array().unwrap();
            let m = args.iter().position(|v| v == "-m").unwrap();
            assert_eq!(args[m + 1], "owned-physical");
        }
        for line in std::fs::read_to_string(owned.root.join("methods.jsonl"))
            .unwrap()
            .lines()
        {
            let r: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(matches!(
                r["method"].as_str(),
                Some("initialize" | "initialized" | "account/read")
            ));
            if r["method"] == "account/read" {
                assert_eq!(r["params"], json!({"refreshToken":false}));
            }
        }
    };
    let result = std::panic::AssertUnwindSafe(exercise).catch_unwind().await;
    state
        .personal_tests
        .configure("main", None, &state)
        .await
        .unwrap();
    service.set_enabled(false).await.unwrap();
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
