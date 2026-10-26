//! Windows-owned TLS origins: per-process memory keys, explicit test root, no
//! machine trust change. The production client/config factory remains in use.
use super::{OWNED_NETWORK_TEST_RESOLVE, OWNED_NETWORK_TEST_ROOT};
use crate::{
    config::{AppConfig, RoutingStrategy},
    db::{repo, Db},
    domain::Provider,
    proxy::server::GatewayState,
    proxy_control::gateway::GatewayService,
};
use base64::Engine;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
};

struct OwnedTls {
    child: Child,
    ready: Value,
}
impl Drop for OwnedTls {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl OwnedTls {
    fn new(body: bool) -> Self {
        let support = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support");
        let mut generator = Command::new("pwsh")
            .args(["-NoLogo", "-NoProfile", "-File"])
            .arg(support.join("n4-owned-tls.ps1"))
            .creation_flags(0x08000000)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = Self::read_ready(&mut generator);
        assert!(
            generator.wait().unwrap().success(),
            "owned in-memory certificate generator must exit0"
        );
        input["mode"] = json!(if body { "body" } else { "headers" });
        let mut child = Command::new("node")
            .arg(support.join("n4-owned-tls.cjs"))
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            let mut pipe = child.stdin.take().unwrap();
            serde_json::to_writer(&mut pipe, &input).unwrap();
            pipe.write_all(b"\n").unwrap();
        }
        // Only the public CA is echoed in readiness; never log the fixture PFX
        // or write it to a file. The private transfer above is an anonymous pipe.
        let ready = Self::read_ready(&mut child);
        Self { child, ready }
    }
    fn read_ready(child: &mut Child) -> Value {
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let read = BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send((read, line));
        });
        let line = match rx.recv_timeout(Duration::from_secs(10)) {
            Ok((Ok(n), line)) if n > 0 => line,
            other => {
                let _ = child.kill();
                let _ = child.wait();
                let mut error = String::new();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                panic!("owned TLS fixture readiness {other:?}: {error}");
            }
        };
        serde_json::from_str(&line).expect("owned fixture readiness JSON")
    }
    fn port(&self, label: &str) -> u16 {
        u16::try_from(self.ready[label].as_u64().unwrap()).unwrap()
    }
    fn root(&self) -> reqwest::Certificate {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(self.ready["root_der"].as_str().unwrap())
            .unwrap();
        reqwest::Certificate::from_der(&bytes).unwrap()
    }
    async fn state(&self) -> Value {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://127.0.0.1:{}/state", self.port("control")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn release(&self) {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://127.0.0.1:{}/release", self.port("control")))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    async fn stop(&mut self) {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://127.0.0.1:{}/stop", self.port("control")))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(exit) = self.child.try_wait().unwrap() {
                assert!(exit.success());
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
struct OwnedProxy {
    url: String,
    calls: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for OwnedProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl OwnedProxy {
    async fn new(origin: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn({
            let calls = calls.clone();
            async move {
                loop {
                    let (mut wire, _) = listener.accept().await.unwrap();
                    let calls = calls.clone();
                    tokio::spawn(async move {
                        let mut header = Vec::new();
                        while !header.windows(4).any(|w| w == b"\r\n\r\n") {
                            let mut c = [0; 1];
                            assert_eq!(wire.read(&mut c).await.unwrap(), 1);
                            header.push(c[0]);
                            assert!(header.len() < 8192);
                        }
                        assert!(String::from_utf8_lossy(&header)
                            .starts_with("CONNECT owned-upstream.invalid:443 HTTP/1.1\r\n"));
                        calls.fetch_add(1, Ordering::SeqCst);
                        let mut target =
                            TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, origin))
                                .await
                                .unwrap();
                        wire.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .await
                            .unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut wire, &mut target).await;
                    });
                }
            }
        });
        Self { url, calls, task }
    }
}
async fn request(
    base: String,
    question: &str,
    streaming: bool,
    seen: Arc<Notify>,
) -> (u16, String) {
    let mut response=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(4)).build().unwrap().post(format!("{base}/v1/chat/completions")).bearer_auth("owned-proxy-matrix-key").header("x-session-id",format!("owned-{question}"))
  .json(&json!({"model":"owned-model","messages":[{"role":"user","content":question}],"stream":streaming,"max_tokens":16})).send().await.unwrap();
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
        if bytes.windows(5).any(|s| s == b"via-A") {
            seen.notify_one();
        }
    }
    (status, String::from_utf8(bytes).unwrap())
}

#[tokio::test]
async fn n4_tls_gateway_config_reload_keeps_inflight_owner_and_new_requests_use_new_proxy() {
    for (streaming, body) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut tls = OwnedTls::new(body);
        let root = tls.root();
        OWNED_NETWORK_TEST_ROOT.scope(root,async {
   let a=OwnedProxy::new(tls.port("a")).await;let b=OwnedProxy::new(tls.port("b")).await;
   let db=Db::connect_in_memory().await.unwrap();
   let p:Provider=serde_json::from_value(json!({"id":"owned-provider","name":"owned-provider","base_url":"https://owned-upstream.invalid/v1","api_key_enc":"","dialect":"openai","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"owned-model","upstream":"wire-model","enabled":true,"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","price":null}]})).unwrap();
   repo::upsert_provider(db.pool(),&p).await.unwrap();
   let mut cfg=AppConfig {port:0,unified_key:"owned-proxy-matrix-key".into(),routing_strategy:RoutingStrategy::Priority,http_proxy:Some(a.url.clone()),..Default::default()};cfg.cache.enabled=false;cfg.search.enabled=false;cfg.smart_routing.enabled=false;cfg.failover_enabled=false;cfg.dedup.mode=crate::proxy::dedup::DedupMode::Off;
   let state=Arc::new(GatewayState::new(db.clone(),cfg));state.reload_providers().await.unwrap();let service=GatewayService::new(state.clone());let started=service.set_enabled(true).await.unwrap();let base=format!("http://{}",started.bound_addr.unwrap());
   let seen=Arc::new(Notify::new());let old=tokio::spawn({let base=base.clone();let seen=seen.clone();async move {request(base,"held",streaming,seen).await}});
   let arrived=tokio::time::timeout(Duration::from_secs(3),async {
    loop {let status=tls.state().await;assert_eq!(status["error"],"");if status["a"]==1 && (!body || streaming || status["offered"]==1) {break;}tokio::time::sleep(Duration::from_millis(5)).await;}
    if body&&streaming {seen.notified().await;}
   }).await;
   if arrived.is_err() {let status=tls.state().await;if old.is_finished(){let result=old.await;panic!("owned TLS arrival: streaming={streaming} body={body} actual={result:?} TLS={status}");}old.abort();let _=old.await;panic!("owned TLS arrival timeout: streaming={streaming} body={body} TLS={status}");}
   assert!(!old.is_finished());let mut cfg=state.cfg_snapshot();cfg.http_proxy=Some(b.url.clone());state.update_cfg(cfg);
   let new=request(base,"new",streaming,Arc::new(Notify::new())).await;assert_eq!(new.0,200,"{new:?}");assert!(new.1.contains("via-B")&&!new.1.contains("via-A"));assert!(!old.is_finished());
   tls.release().await;let old=tokio::time::timeout(Duration::from_secs(3),old).await.unwrap().unwrap();assert_eq!(old.0,200);assert!(old.1.contains("via-A")&&!old.1.contains("via-B"));
   let actual=tls.state().await;assert_eq!(actual["a"],1);assert_eq!(actual["b"],1);assert_eq!(actual["error"],"");assert_eq!(a.calls.load(Ordering::SeqCst),1);assert_eq!(b.calls.load(Ordering::SeqCst),1);
   let deadline=tokio::time::Instant::now()+Duration::from_secs(3);
   loop {let rows:Vec<(i64,i64,i64,Option<String>)>=sqlx::query_as("SELECT status,prompt_tokens,completion_tokens,error FROM requests ORDER BY id").fetch_all(db.pool()).await.unwrap();if rows.len()==2 {assert_eq!(rows,vec![(200,3,2,None),(200,3,2,None)]);break;}assert!(tokio::time::Instant::now()<deadline);tokio::time::sleep(Duration::from_millis(5)).await;}
   let mut stopped=service.set_enabled(false).await.unwrap();while stopped.running {assert!(tokio::time::Instant::now()<deadline);tokio::time::sleep(Duration::from_millis(5)).await;stopped=service.status().await;}
   assert!(stopped.last_error.is_none());assert!(state.listener_snapshot().is_none());
   eprintln!("N4 owned TLS Gateway: stream={streaming}, body_hold={body}, CONNECT_A=1, CONNECT_B=1, model_POSTs=2, audits=2, stop=true; extra root only cfg(test)");
  }).await;
        tls.stop().await;
    }
}

#[tokio::test]
async fn n4_socks_tls_gateway_preserves_inflight_owner_wire_target_and_audit() {
    use crate::outbound::socks_tests::{Mode, OwnedSocks};
    for (scheme, streaming) in [
        ("socks5", false),
        ("socks5", true),
        ("socks5h", false),
        ("socks5h", true),
    ] {
        let mut tls = OwnedTls::new(true);
        let root = tls.root();
        let target_port = tls.port("a");
        let resolve = (
            "owned-upstream.invalid".to_owned(),
            std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, target_port)),
        );
        OWNED_NETWORK_TEST_ROOT.scope(root,OWNED_NETWORK_TEST_RESOLVE.scope(resolve,async {
            let mut a=OwnedSocks::new(scheme,std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST,tls.port("a"))),target_port,Mode::Forward).await;
            let mut b=OwnedSocks::new(scheme,std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST,tls.port("b"))),target_port,Mode::Forward).await;
            let db=Db::connect_in_memory().await.unwrap();
            let p:Provider=serde_json::from_value(json!({"id":"owned-socks-provider","name":"owned-socks-provider","base_url":format!("https://owned-upstream.invalid:{target_port}/v1"),"api_key_enc":"","dialect":"openai","enabled":true,"priority":0,"rpm_limit":0,"intelligence":50,"runtime_id":null,"created_at":"2026-10-20T00:00:00Z","updated_at":"2026-10-20T00:00:00Z","models":[{"alias":"owned-model","upstream":"wire-model","enabled":true,"context_window":32768,"supports_tools":false,"supports_vision":false,"supports_audio":false,"supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat","price":null}]})).unwrap();
            repo::upsert_provider(db.pool(),&p).await.unwrap();
            let mut cfg=AppConfig {port:0,unified_key:"owned-proxy-matrix-key".into(),routing_strategy:RoutingStrategy::Priority,http_proxy:Some(a.url.clone()),..Default::default()};cfg.cache.enabled=false;cfg.search.enabled=false;cfg.smart_routing.enabled=false;cfg.failover_enabled=false;cfg.dedup.mode=crate::proxy::dedup::DedupMode::Off;
            let state=Arc::new(GatewayState::new(db.clone(),cfg));state.reload_providers().await.unwrap();let service=GatewayService::new(state.clone());let started=service.set_enabled(true).await.unwrap();let base=format!("http://{}",started.bound_addr.unwrap());
            let seen=Arc::new(Notify::new());let old=tokio::spawn({let base=base.clone();let seen=seen.clone();async move{request(base,"held",streaming,seen).await}});
            let arrived=tokio::time::timeout(Duration::from_secs(3),async {
                loop {let actual=tls.state().await;assert_eq!(actual["error"],"");if actual["a"]==1 && actual["offered"]==1 {break;}tokio::time::sleep(Duration::from_millis(5)).await;}
                if streaming {seen.notified().await;}
            }).await;
            if arrived.is_err(){old.abort();let _=old.await;panic!("owned SOCKS arrival failed: scheme={scheme},stream={streaming},A={:?},TLS={}",a.stats(),tls.state().await);}
            assert!(!old.is_finished());let mut cfg=state.cfg_snapshot();cfg.http_proxy=Some(b.url.clone());state.update_cfg(cfg);
            let new=request(base,"new",streaming,Arc::new(Notify::new())).await;assert_eq!(new.0,200);assert!(new.1.contains("via-B")&&!new.1.contains("via-A"));assert!(!old.is_finished());
            tls.release().await;let old=tokio::time::timeout(Duration::from_secs(3),old).await.unwrap().unwrap();assert_eq!(old.0,200);assert!(old.1.contains("via-A")&&!old.1.contains("via-B"));
            let expected=if scheme=="socks5h"{(3,"owned-upstream.invalid".to_owned(),target_port)}else{(1,"127.0.0.1".to_owned(),target_port)};
            for proxy in [&a,&b]{let stats=proxy.stats();assert_eq!(stats.accepted,1);assert_eq!(stats.targets,vec![expected.clone()]);assert!(stats.errors.is_empty(),"{:?}",stats.errors);}
            let actual=tls.state().await;assert_eq!(actual["a"],1);assert_eq!(actual["b"],1);assert_eq!(actual["error"],"");
            let deadline=tokio::time::Instant::now()+Duration::from_secs(3);
            loop {let rows:Vec<(i64,i64,i64,Option<String>)>=sqlx::query_as("SELECT status,prompt_tokens,completion_tokens,error FROM requests ORDER BY id").fetch_all(db.pool()).await.unwrap();if rows.len()==2{assert_eq!(rows,vec![(200,3,2,None),(200,3,2,None)]);break;}assert!(tokio::time::Instant::now()<deadline);tokio::time::sleep(Duration::from_millis(5)).await;}
            let stopped=tokio::time::timeout(Duration::from_secs(3),service.set_enabled(false)).await.unwrap().unwrap();assert!(!stopped.running);assert!(stopped.last_error.is_none());assert!(state.listener_snapshot().is_none());a.stop().await;b.stop().await;
            eprintln!("N4 owned SOCKS Gateway: scheme={scheme},stream={streaming},SOCKS_A=1,SOCKS_B=1,POST2,audits2,stop=true; test-only DNS/root, no system changes");
        })).await;
        tls.stop().await;
    }
}
