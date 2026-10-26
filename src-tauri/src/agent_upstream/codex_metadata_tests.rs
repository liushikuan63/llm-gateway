use super::*;

fn metadata() -> Value {
    json!({"account":{"type":"chatgpt","email":"owned@example.test","planType":"unknown"},"requiresOpenaiAuth":true,"workspaceRouting":{"accountRoutingOverride":"NO_CONSTRAINT","backendOrigin":"https://owned.invalid","chatgptAccountId":"owned-workspace"}})
}
#[test]
fn observed_chatgpt_workspace_is_parseable_without_tokens_or_a_free_proof() {
    let account =
        parse_account_result(&metadata()).expect("supported observed metadata must be consumed");
    assert_ne!(account.fingerprint, [0; 32]);
    assert!(!format!("{account:?}").contains("owned@example.test"));
}
#[test]
fn missing_identity_unsupported_auth_and_invalid_metadata_never_become_a_binding() {
    let mut variants = vec![
        json!({"account":null,"requiresOpenaiAuth":true}),
        json!({"account":null,"requiresOpenaiAuth":false}),
        json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":true}),
        json!({"account":{"type":"amazonBedrock"},"requiresOpenaiAuth":false}),
    ];
    for path in ["email", "planType"] {
        let mut v = metadata();
        v["account"][path] = Value::Null;
        variants.push(v);
    }
    for path in [
        "backendOrigin",
        "chatgptAccountId",
        "accountRoutingOverride",
    ] {
        let mut v = metadata();
        v["workspaceRouting"][path] = json!("");
        variants.push(v);
    }
    let mut v = metadata();
    v["workspaceRouting"] = Value::Null;
    variants.push(v);
    let mut v = metadata();
    v["accessToken"] = json!("synthetic-sensitive-not-to-be-logged");
    variants.push(v);
    let mut v = metadata();
    v["account"]["email"] = json!("owned\n@example.test");
    variants.push(v);
    for v in variants {
        let error = parse_account_result(&v).unwrap_err();
        assert!(!error.to_string().contains("sensitive"));
    }
    assert_eq!(
        parse_account_result(&json!({"account":null,"requiresOpenaiAuth":true})),
        Err(MetadataError::LoginRequired)
    );
}
#[test]
fn observation_epochs_expiry_and_events_are_not_credential_generations() {
    let account = parse_account_result(&metadata()).unwrap();
    let mut state = ObservationState::new([1; 32]);
    let first = state.accept(account.clone()).unwrap();
    assert_eq!(first, state.accept(account).unwrap());
    assert!(state.check(&first, Instant::now()));
    assert!(!state.check(&first, Instant::now() + MAX_AGE));
    for method in ["account/updated", "account/login/completed"] {
        let params = if method == "account/updated" {
            json!({"authMode":"chatgpt","planType":"unknown"})
        } else {
            json!({"success":false,"error":"synthetic-sensitive-error","loginId":null})
        };
        state
            .notification(&json!({"method":method,"params":params}))
            .unwrap();
        assert!(!state.check(&first, Instant::now()));
        let next = state
            .accept(parse_account_result(&metadata()).unwrap())
            .unwrap();
        assert_ne!(first, next);
    }
    let another = ObservationState::new([1; 32]);
    assert!(!another.check(&first, Instant::now()));
    state.closed = true;
    state.invalidate();
    assert!(!state.check(&first, Instant::now()));
}

#[cfg(windows)]
pub(crate) struct OwnedFixture {
    pub(crate) root: PathBuf,
}
#[cfg(windows)]
impl OwnedFixture {
    pub(crate) fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "llmgw-owned-codex-metadata-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("home")).unwrap();
        std::fs::write(root.join("account.json"), metadata().to_string()).unwrap();
        std::fs::write(
            root.join("owned.cmd"),
            "@echo off\r\nnode \"%~dp0owned.cjs\" %*\r\n",
        )
        .unwrap();
        std::fs::write(root.join("owned.cjs"),r#"const fs=require('node:fs'),path=require('node:path'),readline=require('node:readline');const root=__dirname;
const add=(f,x)=>fs.appendFileSync(path.join(root,f),x+'\n');
add('args.jsonl',JSON.stringify(process.argv.slice(2)));
if(process.argv[2]==='exec'){add('model-starts','1');console.log(JSON.stringify({type:'item.completed',item:{type:'agent_message',text:'owned metadata-bound reply'}}));process.exit(0);}
if(process.argv.slice(2).join(' ')!=='app-server --listen stdio://')process.exit(7);
add('helper-starts','1');fs.writeFileSync(path.join(root,'helper-node-pid'),String(process.pid));
const rl=readline.createInterface({input:process.stdin});let timer;
rl.on('line',line=>{const r=JSON.parse(line);add('methods.jsonl',JSON.stringify({method:r.method,params:r.params}));
if(r.method==='initialize'){console.log(JSON.stringify({id:r.id,result:{userAgent:'owned-only'}}));return;}
if(r.method==='initialized')return;
if(r.method!=='account/read'||r.params.refreshToken!==false)process.exit(8);
const mode=fs.existsSync(path.join(root,'mode'))?fs.readFileSync(path.join(root,'mode'),'utf8'):'';
const reply=()=>console.log(JSON.stringify({id:r.id,result:JSON.parse(fs.readFileSync(path.join(root,'account.json'),'utf8'))}));
if(mode==='hold'){fs.writeFileSync(path.join(root,'held'),'1');timer=setInterval(()=>{if(fs.existsSync(path.join(root,'release'))){clearInterval(timer);reply();}},10);return;}
if(mode==='event')console.log(JSON.stringify({method:'account/updated',params:{authMode:'chatgpt',planType:'unknown'}}));
if(mode==='server-request'){console.log(JSON.stringify({id:99,method:'account/chatgptAuthTokens/refresh',params:{reason:'unauthorized'}}));return;}
if(mode==='wrong-id'){console.log(JSON.stringify({id:999,result:{}}));return;}
if(mode==='oversize'){console.log('x'.repeat(65537));return;}
if(mode==='error'){console.log(JSON.stringify({id:r.id,error:{code:-1,message:'synthetic-sensitive-error'}}));return;}
if(mode==='disconnect'){process.exit(0);return;}
reply();});rl.on('close',()=>{if(timer)clearInterval(timer);process.exit(0);});
"#).unwrap();
        Self { root }
    }
    pub(crate) fn context(&self) -> LaunchContext {
        LaunchContext::new(
            &self.root.join("owned.cmd"),
            &self.root,
            &self.root.join("home"),
        )
        .unwrap()
    }
    pub(crate) fn mode(&self, mode: &str) {
        std::fs::write(self.root.join("mode"), mode).unwrap();
    }
    pub(crate) fn count(&self, file: &str) -> usize {
        std::fs::read_to_string(self.root.join(file))
            .unwrap_or_default()
            .lines()
            .count()
    }
    fn assert_methods(&self) {
        for line in std::fs::read_to_string(self.root.join("methods.jsonl"))
            .unwrap_or_default()
            .lines()
        {
            let value: Value = serde_json::from_str(line).unwrap();
            let method = value["method"].as_str().unwrap();
            assert!(matches!(
                method,
                "initialize" | "initialized" | "account/read"
            ));
            if method == "account/read" {
                assert_eq!(value["params"], json!({"refreshToken":false}));
            }
        }
    }
}
#[cfg(windows)]
impl Drop for OwnedFixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("OWNED_METADATA_FAILURE_FIXTURE={}", self.root.display());
            return;
        }
        let parent = std::env::temp_dir().canonicalize().unwrap();
        if let Ok(resolved) = self.root.canonicalize() {
            assert!(resolved.starts_with(&parent));
            assert!(resolved
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("llmgw-owned-codex-metadata-"));
            let _ = std::fs::remove_dir_all(resolved);
        }
    }
}

#[cfg(windows)]
#[tokio::test]
async fn selected_context_fake_helper_positive_recheck_and_one_owned_request() {
    let f = OwnedFixture::new();
    let ctx = f.context();
    let same = f.context();
    assert_eq!(
        ctx.fingerprint, same.fingerprint,
        "runtime labels do not create separate account identities"
    );
    let mut session = AccountSession::connect(ctx.clone(), Duration::from_secs(3))
        .await
        .unwrap();
    let binding = session.read_account().await.unwrap();
    session.recheck(&binding).await.unwrap();
    assert!(session.is_current(&binding));
    let mut command = session
        .launch_context()
        .request_command("owned-physical", "owned prompt")
        .unwrap();
    let output = command
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
        .wait_with_output()
        .await
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        super::super::codex::extract_final_message(std::str::from_utf8(&output.stdout).unwrap())
            .unwrap(),
        "owned metadata-bound reply"
    );
    session.shutdown().await.unwrap();
    assert_eq!(f.count("helper-starts"), 1);
    assert_eq!(f.count("model-starts"), 1);
    f.assert_methods();
    assert_eq!(f.count("methods.jsonl"), 4);
    eprintln!("OWNED_METADATA_POSITIVE helpers=1 account_reads=2 request_starts=1 usage=unknown terminal=unverified");
}
#[cfg(windows)]
#[tokio::test]
async fn changed_metadata_or_protocol_failure_retires_old_binding_with_zero_request_starts() {
    for mode in [
        "email",
        "workspace",
        "origin",
        "plan",
        "routing",
        "event",
        "server-request",
        "wrong-id",
        "oversize",
        "error",
        "disconnect",
    ] {
        let f = OwnedFixture::new();
        let mut s = AccountSession::connect(f.context(), Duration::from_secs(3))
            .await
            .unwrap();
        let b = s.read_account().await.unwrap();
        let mut value = metadata();
        match mode {
            "email" => value["account"]["email"] = json!("other@example.test"),
            "workspace" => value["workspaceRouting"]["chatgptAccountId"] = json!("other-workspace"),
            "origin" => value["workspaceRouting"]["backendOrigin"] = json!("https://other.invalid"),
            "plan" => value["account"]["planType"] = json!("free"),
            "routing" => value["workspaceRouting"]["accountRoutingOverride"] = json!("us"),
            _ => f.mode(mode),
        };
        std::fs::write(f.root.join("account.json"), value.to_string()).unwrap();
        let result = s.recheck(&b).await;
        if result.is_ok() {
            let mut c = s
                .launch_context()
                .request_command("owned-physical", "unexpected owned launch")
                .unwrap();
            let _ = c.spawn().unwrap().wait().await;
        }
        assert!(result.is_err(), "{mode}");
        assert!(!s.is_current(&b), "{mode}");
        assert_eq!(f.count("model-starts"), 0, "{mode}");
        assert!(!result
            .unwrap_err()
            .to_string()
            .contains("synthetic-sensitive"));
        f.assert_methods();
    }
}
#[cfg(windows)]
#[tokio::test]
async fn dropped_pending_read_invalidates_and_closes_only_owned_helper() {
    let f = OwnedFixture::new();
    let mut s = AccountSession::connect(f.context(), Duration::from_secs(3))
        .await
        .unwrap();
    let b = s.read_account().await.unwrap();
    f.mode("hold");
    let mut pending = Box::pin(s.read_account());
    let held = async {
        while !f.root.join("held").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::select! {r=&mut pending=>panic!("read must be pending: {r:?}"),r=tokio::time::timeout(Duration::from_secs(3),held)=>r.unwrap()}
    drop(pending);
    assert!(!s.is_current(&b));
    tokio::time::timeout(Duration::from_secs(3), s.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.count("model-starts"), 0);
    f.assert_methods();
}

#[cfg(windows)]
#[tokio::test]
async fn expired_observation_requires_a_fresh_read_but_does_not_end_session_opt_in() {
    let f = OwnedFixture::new();
    let mut s = AccountSession::connect(f.context(), Duration::from_secs(3))
        .await
        .unwrap();
    let binding = s.read_account().await.unwrap();
    s.state.current.as_mut().unwrap().1 = Instant::now() - MAX_AGE - Duration::from_secs(1);
    assert!(!s.is_current(&binding));
    let result = s.recheck(&binding).await;
    let shutdown = s.shutdown().await;
    result.expect(
        "cache expiry must trigger a fresh same-account observation instead of ending the opt-in",
    );
    shutdown.unwrap();
    assert_eq!(f.count("helper-starts"), 1);
    assert_eq!(f.count("model-starts"), 0);
    assert_eq!(f.count("methods.jsonl"), 4);
    f.assert_methods();
}
