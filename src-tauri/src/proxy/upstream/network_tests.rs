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
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
            .timeout(Duration::from_secs(3))
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
            .timeout(Duration::from_secs(3))
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
            .timeout(Duration::from_secs(3))
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
    reject: Arc<AtomicBool>,
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
        let reject = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn({
            let calls = calls.clone();
            let reject = reject.clone();
            async move {
                loop {
                    let (mut wire, _) = listener.accept().await.unwrap();
                    let calls = calls.clone();
                    let reject = reject.clone();
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
                        if reject.load(Ordering::SeqCst) {
                            wire.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                .await
                                .unwrap();
                            return;
                        }
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
        Self {
            url,
            calls,
            reject,
            task,
        }
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

#[tokio::test]
async fn n4_socks_proxy_switch_refusal_preserves_inflight_owner_and_never_goes_direct() {
    use crate::outbound::socks_tests::{Mode, OwnedSocks};

    for scheme in ["socks5", "socks5h"] {
        let mut tls = OwnedTls::new(true);
        let root = tls.root();
        let target_port = tls.port("a");
        let direct = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, target_port));
        OWNED_NETWORK_TEST_ROOT
            .scope(
                root,
                OWNED_NETWORK_TEST_RESOLVE.scope(("owned-upstream.invalid".to_owned(), direct), async {
                    let mut a = OwnedSocks::new(
                        scheme,
                        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, tls.port("a"))),
                        target_port,
                        Mode::Forward,
                    )
                    .await;
                    let mut b = OwnedSocks::new(
                        scheme,
                        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, tls.port("b"))),
                        target_port,
                        Mode::RejectConnect,
                    )
                    .await;
                    let db = Db::connect_in_memory().await.unwrap();
                    let provider: Provider = serde_json::from_value(json!({
                        "id": "owned-socks-switch-provider",
                        "name": "owned-socks-switch-provider",
                        "base_url": format!("https://owned-upstream.invalid:{target_port}/v1"),
                        "api_key_enc": "",
                        "dialect": "openai",
                        "enabled": true,
                        "priority": 0,
                        "rpm_limit": 0,
                        "intelligence": 50,
                        "created_at": "2026-10-20T00:00:00Z",
                        "updated_at": "2026-10-20T00:00:00Z",
                        "models": [{
                            "alias": "owned-model",
                            "upstream": "wire-model",
                            "enabled": true,
                            "context_window": 32768,
                            "supports_tools": false,
                            "supports_vision": false,
                            "supports_audio": false,
                            "supports_video": false,
                            "supports_thinking": false,
                            "supports_stream": true,
                            "model_type": "chat",
                            "price": null
                        }]
                    }))
                    .unwrap();
                    repo::upsert_provider(db.pool(), &provider).await.unwrap();
                    let mut cfg = AppConfig {
                        port: 0,
                        unified_key: "owned-proxy-matrix-key".into(),
                        routing_strategy: RoutingStrategy::Priority,
                        http_proxy: Some(a.url.clone()),
                        failover_enabled: false,
                        ..Default::default()
                    };
                    cfg.cache.enabled = false;
                    cfg.search.enabled = false;
                    cfg.smart_routing.enabled = false;
                    cfg.dedup.mode = crate::proxy::dedup::DedupMode::Off;
                    let state = Arc::new(GatewayState::new(db.clone(), cfg));
                    state.reload_providers().await.unwrap();
                    let service = GatewayService::new(state.clone());
                    let started = service.set_enabled(true).await.unwrap();
                    let base = format!("http://{}", started.bound_addr.unwrap());
                    let seen = Arc::new(Notify::new());
                    let old = tokio::spawn({
                        let base = base.clone();
                        let seen = seen.clone();
                        async move { request(base, "held", true, seen).await }
                    });
                    let arrived = tokio::time::timeout(Duration::from_secs(3), async {
                        loop {
                            let actual = tls.state().await;
                            assert_eq!(actual["error"], "");
                            if actual["a"] == 1 && actual["offered"] == 1 {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        seen.notified().await;
                    })
                    .await;
                    if arrived.is_err() {
                        if !old.is_finished() {
                            old.abort();
                            let _ = old.await;
                        }
                        panic!(
                            "owned SOCKS refusal arrival failed: scheme={scheme}, A={:?}, TLS={}",
                            a.stats(),
                            tls.state().await
                        );
                    }
                    assert!(!old.is_finished());

                    let mut switched = state.cfg_snapshot();
                    switched.http_proxy = Some(b.url.clone());
                    state.update_cfg(switched);
                    let rejected = request(base, "new", true, Arc::new(Notify::new())).await;
                    assert_eq!(rejected.0, 503, "{rejected:?}");
                    assert!(rejected.1.contains("upstream_error"), "{rejected:?}");
                    assert!(!old.is_finished());
                    assert_eq!(tls.state().await["a"], 1);
                    assert_eq!(tls.state().await["b"], 0);

                    let expected = if scheme == "socks5h" {
                        (3, "owned-upstream.invalid".to_owned(), target_port)
                    } else {
                        (1, "127.0.0.1".to_owned(), target_port)
                    };
                    let a_stats = a.stats();
                    let b_stats = b.stats();
                    assert_eq!(a_stats.accepted, 1);
                    assert_eq!(a_stats.targets, vec![expected.clone()]);
                    assert!(a_stats.errors.is_empty(), "{a_stats:?}");
                    assert_eq!(b_stats.accepted, 1);
                    assert_eq!(b_stats.targets, vec![expected]);
                    assert!(b_stats.errors.is_empty(), "{b_stats:?}");

                    tls.release().await;
                    let completed = tokio::time::timeout(Duration::from_secs(3), old)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(completed.0, 200, "{completed:?}");
                    assert!(completed.1.contains("via-A"));
                    assert!(!completed.1.contains("via-B"));
                    let final_state = tls.state().await;
                    assert_eq!(final_state["a"], 1);
                    assert_eq!(final_state["b"], 0);
                    assert_eq!(final_state["error"], "");
                    let stopped = tokio::time::timeout(
                        Duration::from_secs(3),
                        service.set_enabled(false),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert!(!stopped.running);
                    assert!(stopped.last_error.is_none());
                    assert!(state.listener_snapshot().is_none());
                    a.stop().await;
                    b.stop().await;
                    eprintln!(
                        "N4 owned SOCKS refusal: scheme={scheme}, old=A, new=B-reject, A=1, B=0, direct=0, stop=true"
                    );
                },),
            )
            .await;
        tls.stop().await;
    }
}

#[tokio::test]
async fn n4_http_proxy_switch_preserves_open_stream_and_refusal_never_goes_direct() {
    let mut tls = OwnedTls::new(true);
    let root = tls.root();
    let host = "owned-upstream.invalid";
    // Keep an ordinary HTTPS authority for CONNECT. The test DNS override
    // maps direct client traffic for that same authority to the live A origin.
    let direct_address = std::net::SocketAddr::from(([127, 0, 0, 1], tls.port("a")));
    let direct = reqwest::Client::builder()
        .no_proxy()
        .resolve(host, direct_address)
        .add_root_certificate(root.clone())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let direct_control = |question: &'static str| {
        direct
            .post(format!("https://{host}/v1/chat/completions"))
            .json(&json!({"model": "wire-model", "stream": true,
                "messages": [{"role": "user", "content": question}]}))
    };
    let assert_done = |body: &str, expected: &str, unexpected: &str| {
        assert!(body.contains(expected), "missing stream source: {body}");
        assert!(!body.contains(unexpected), "mixed stream sources: {body}");
        assert!(
            !body.contains("\"error\""),
            "unexpected stream error: {body}"
        );
        assert_eq!(body.matches("data: [DONE]").count(), 1, "{body}");
    };

    OWNED_NETWORK_TEST_ROOT
        .scope(
            root,
            OWNED_NETWORK_TEST_RESOLVE.scope((host.to_owned(), direct_address), async {
                let a = OwnedProxy::new(tls.port("a")).await;
                let b = OwnedProxy::new(tls.port("b")).await;
                let provider: Provider = serde_json::from_value(json!({
                    "id": "owned-switch-provider", "name": "owned-switch-provider",
                    "base_url": format!("https://{host}/v1"), "dialect": "openai",
                    "api_key_enc": "", "enabled": true, "priority": 0, "rpm_limit": 0,
                    "intelligence": 50, "created_at": "2026-10-20T00:00:00Z",
                    "updated_at": "2026-10-20T00:00:00Z",
                    "models": [{"alias": "owned-model", "upstream": "wire-model", "enabled": true,
                        "context_window": 32768, "supports_tools": false, "supports_vision": false,
                        "supports_thinking": false, "supports_stream": true, "model_type": "chat"}]
                }))
                .unwrap();
                let db = Db::connect_in_memory().await.unwrap();
                repo::upsert_provider(db.pool(), &provider).await.unwrap();
                let mut cfg = AppConfig {
                    port: 0,
                    unified_key: "owned-proxy-matrix-key".into(),
                    routing_strategy: RoutingStrategy::Priority,
                    http_proxy: Some(a.url.clone()),
                    failover_enabled: false,
                    ..Default::default()
                };
                cfg.cache.enabled = false;
                cfg.search.enabled = false;
                cfg.smart_routing.enabled = false;
                cfg.dedup.mode = crate::proxy::dedup::DedupMode::Off;
                let state = Arc::new(GatewayState::new(db, cfg));
                state.reload_providers().await.unwrap();
                let service = GatewayService::new(state.clone());
                let started = service.set_enabled(true).await.unwrap();
                let chat_url =
                    format!("http://{}/v1/chat/completions", started.bound_addr.unwrap());
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap();
                let send = |question: &'static str| {
                    client
                        .post(&chat_url)
                        .bearer_auth("owned-proxy-matrix-key")
                        .header("x-session-id", format!("owned-{question}"))
                        .json(
                            &json!({"model": "owned-model", "stream": true, "max_tokens": 16,
                            "messages": [{"role": "user", "content": question}]}),
                        )
                };

                let before = direct_control("direct-before")
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .text()
                    .await
                    .unwrap();
                assert_done(&before, "via-A", "via-B");
                assert_eq!(
                    (
                        a.calls.load(Ordering::SeqCst),
                        b.calls.load(Ordering::SeqCst)
                    ),
                    (0, 0)
                );

                let mut old = send("held").send().await.unwrap();
                assert_eq!(old.status(), reqwest::StatusCode::OK);
                let mut prefix = Vec::new();
                while !prefix.windows(5).any(|w| w == b"via-A") {
                    let chunk = old.chunk().await.unwrap().expect("first A event");
                    prefix.extend_from_slice(&chunk);
                    assert!(prefix.len() <= 16_384, "bounded first event");
                }
                let before_switch = tls.state().await;
                assert_eq!(
                    before_switch,
                    json!({"a": 2, "b": 0, "offered": 1, "error": ""})
                );
                assert_eq!(a.calls.load(Ordering::SeqCst), 1);
                assert!(!String::from_utf8_lossy(&prefix).contains("data: [DONE]"));

                // Existing production runtime publication entrypoint; test config stays in memory.
                let mut next = state.cfg_snapshot();
                next.http_proxy = Some(b.url.clone());
                state.update_cfg(next);
                assert_eq!(
                    state.cfg_snapshot().http_proxy.as_deref(),
                    Some(b.url.as_str())
                );
                let fresh = send("new")
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .text()
                    .await
                    .unwrap();
                assert_done(&fresh, "via-B", "via-A");
                assert_eq!(
                    tls.state().await,
                    json!({"a": 2, "b": 1, "offered": 1, "error": ""})
                );

                tls.release().await;
                let suffix = old.text().await.unwrap();
                assert!(
                    suffix.contains("data: [DONE]"),
                    "A must finish after release: {suffix}"
                );
                prefix.extend_from_slice(suffix.as_bytes());
                assert_done(&String::from_utf8(prefix).unwrap(), "via-A", "via-B");
                assert_eq!(
                    (
                        a.calls.load(Ordering::SeqCst),
                        b.calls.load(Ordering::SeqCst)
                    ),
                    (1, 1)
                );

                // The origin closes the successful B response's connection,
                // so this request exercises a new tunnel attempt through B.
                b.reject.store(true, Ordering::SeqCst);
                let rejected = send("refused").send().await.unwrap();
                assert_eq!(rejected.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(
                    rejected.json::<Value>().await.unwrap(),
                    json!({"error": {
                        "message": "上游服务 owned-switch-provider/wire-model 返回 HTTP 503",
                        "type": "upstream_error", "code": null
                    }})
                );
                assert_eq!(
                    (
                        a.calls.load(Ordering::SeqCst),
                        b.calls.load(Ordering::SeqCst)
                    ),
                    (1, 2)
                );
                assert_eq!(
                    tls.state().await,
                    json!({"a": 2, "b": 1, "offered": 1, "error": ""})
                );

                let after = direct_control("direct-after")
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .text()
                    .await
                    .unwrap();
                assert_done(&after, "via-A", "via-B");
                let stopped = service.set_enabled(false).await.unwrap();
                assert!(!stopped.running);
                assert!(stopped.last_error.is_none(), "{stopped:?}");
                assert!(state.listener_snapshot().is_none());
                assert_eq!(
                    tls.state().await,
                    json!({"a": 3, "b": 1, "offered": 1, "error": ""})
                );
            }),
        )
        .await;
    tls.stop().await;
}

#[cfg(feature = "ipc-tests")]
#[tokio::test]
async fn n4_proxy_config_production_ipc_persists_to_fresh_gateway_socks_forward_and_refusal() {
    use crate::outbound::socks_tests::{Mode, OwnedSocks};

    let host = "owned-upstream.invalid";
    for scheme in ["socks5", "socks5h"] {
        let mut tls = OwnedTls::new(false);
        let target_port = tls.port("a");
        let direct = std::net::SocketAddr::from(([127, 0, 0, 1], target_port));
        let forwarded = std::net::SocketAddr::from(([127, 0, 0, 1], tls.port("b")));
        // The same named target is directly reachable at A. The owned proxy
        // forwards only to B, giving direct and proxied traffic distinct receipts.
        let mut forward = OwnedSocks::new(scheme, forwarded, target_port, Mode::Forward).await;
        let mut reject = OwnedSocks::new(scheme, forwarded, target_port, Mode::RejectConnect).await;
        let expected_wire_target = if scheme == "socks5h" {
            (3, host.to_owned(), target_port)
        } else {
            (1, direct.ip().to_string(), target_port)
        };
        let direct_request = || {
            reqwest::Client::builder()
                .no_proxy()
                .resolve(host, direct)
                .add_root_certificate(tls.root())
                .timeout(Duration::from_secs(4))
                .build()
                .unwrap()
                .post(format!("https://{host}:{target_port}/v1/chat/completions"))
                .json(&json!({
                    "model": "wire-model",
                    "messages": [{"role": "user", "content": "direct-control"}],
                    "stream": false,
                    "max_tokens": 16
                }))
        };
        let before: Value = direct_request()
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(before["choices"][0]["message"]["content"], "via-A");
        assert_eq!(
            tls.state().await,
            json!({"a": 1, "b": 0, "offered": 0, "error": ""})
        );

        let provider: Provider = serde_json::from_value(json!({
            "id": "owned-socks-ipc-provider", "name": "owned-socks-ipc-provider",
            "base_url": format!("https://{host}:{target_port}/v1"), "dialect": "openai",
            "api_key_enc": "", "enabled": true, "priority": 0, "rpm_limit": 0,
            "intelligence": 50, "created_at": "2026-10-20T00:00:00Z",
            "updated_at": "2026-10-20T00:00:00Z",
            "models": [{"alias": "owned-model", "upstream": "wire-model", "enabled": true,
                "context_window": 32768, "supports_tools": false, "supports_vision": false,
                "supports_thinking": false, "supports_stream": true, "model_type": "chat"}]
        }))
        .unwrap();
        for (proxy, label, expected_status) in [
            (&forward, "socks-forward", 200),
            (&reject, "socks-refused", 503),
        ] {
            let mut cfg = AppConfig {
                port: 0,
                unified_key: "owned-proxy-matrix-key".into(),
                routing_strategy: RoutingStrategy::Priority,
                failover_enabled: false,
                ..Default::default()
            };
            cfg.cache.enabled = false;
            cfg.search.enabled = false;
            cfg.smart_routing.enabled = false;
            cfg.dedup.mode = crate::proxy::dedup::DedupMode::Off;
            let initial = forward.url.clone();
            let target = proxy.url.clone();
            tokio::task::spawn_blocking(move || {
                crate::commands::replace_owned_proxy_via_ipc(cfg, &initial, &target);
            })
            .await
            .unwrap();

            // The save runtime has gone away. No in-memory client or config
            // from that runtime is passed to the gateway receiving the request.
            let (on_disk, warning) = AppConfig::load_or_init_with_warning().unwrap();
            assert!(warning.is_none(), "{warning:?}");
            assert_eq!(on_disk.http_proxy.as_deref(), Some(proxy.url.as_str()));
            assert_eq!(on_disk.port, 0);
            assert_eq!(on_disk.bind, "127.0.0.1");
            let db = Db::connect_in_memory().await.unwrap();
            repo::upsert_provider(db.pool(), &provider).await.unwrap();
            let state = Arc::new(OWNED_NETWORK_TEST_ROOT.sync_scope(tls.root(), || {
                OWNED_NETWORK_TEST_RESOLVE.sync_scope((host.to_owned(), direct), || {
                    GatewayState::new(db.clone(), on_disk)
                })
            }));
            state.reload_providers().await.unwrap();
            let service = GatewayService::new(state.clone());
            let started = service.set_enabled(true).await.unwrap();
            let bound: std::net::SocketAddr = started.bound_addr.unwrap().parse().unwrap();
            assert!(bound.ip().is_loopback() && bound.port() != 0);
            let (status, body) = request(
                format!("http://{bound}"),
                label,
                false,
                Arc::new(Notify::new()),
            )
            .await;
            let stopped = tokio::time::timeout(Duration::from_secs(5), service.set_enabled(false))
                .await
                .unwrap()
                .unwrap();
            assert!(!stopped.running);
            assert!(stopped.last_error.is_none(), "{stopped:?}");
            assert!(state.listener_snapshot().is_none());

            assert_eq!(status, expected_status, "{scheme} {label}: {body}");
            let body: Value = serde_json::from_str(&body).unwrap();
            let stats = proxy.stats();
            assert_eq!(stats.accepted, 1, "{scheme} {label}: {stats:?}");
            assert_eq!(
                stats.targets.as_slice(),
                std::slice::from_ref(&expected_wire_target)
            );
            assert!(stats.errors.is_empty(), "{stats:?}");
            assert_eq!(
                tls.state().await,
                json!({"a": 1, "b": 1, "offered": 0, "error": ""})
            );

            let rows = sqlx::query_as::<_, (
                i64,
                Option<String>,
                Option<String>,
                i64,
                i64,
                Option<String>,
                i64,
            )>(
                "SELECT status,routed_provider,routed_model,prompt_tokens,completion_tokens,error,fallback_attempts FROM requests",
            )
            .fetch_all(db.pool())
            .await
            .unwrap();
            assert_eq!(rows.len(), 1, "{scheme} {label}: {rows:?}");
            if expected_status == 200 {
                assert_eq!(body["choices"][0]["message"]["content"], "via-B");
                assert_eq!(body["choices"][0]["finish_reason"], "stop");
                assert_eq!(body["usage"]["prompt_tokens"], 3);
                assert_eq!(body["usage"]["completion_tokens"], 2);
                assert_eq!(reject.stats().accepted, 0);
                assert_eq!(
                    rows[0],
                    (
                        200,
                        Some(provider.id.clone()),
                        Some("wire-model".into()),
                        3,
                        2,
                        None,
                        0
                    )
                );
            } else {
                assert_eq!(body["error"]["type"], "upstream_error");
                assert_eq!(forward.stats().accepted, 1);
                assert_eq!((&rows[0].0, &rows[0].1, &rows[0].2), (&503, &None, &None));
                assert_eq!((rows[0].3, rows[0].4, rows[0].6), (0, 0, 0));
                assert!(rows[0].5.is_some(), "{rows:?}");
            }
        }

        // Use a newly built direct client so the post-refusal control must
        // establish its own connection to the original destination.
        let after: Value = direct_request()
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(after["choices"][0]["message"]["content"], "via-A");
        assert_eq!(
            tls.state().await,
            json!({"a": 2, "b": 1, "offered": 0, "error": ""})
        );
        forward.stop().await;
        reject.stop().await;
        tls.stop().await;
        eprintln!(
            "N4 owned IPC SOCKS cold load: scheme={scheme}, forward=200, refusal=503 upstream_error, forwarded POST=1 then 0, direct controls=2, audits=2, closed"
        );
    }
}

#[cfg(feature = "ipc-tests")]
#[tokio::test]
async fn n4_proxy_config_production_ipc_persists_to_fresh_gateway_connect() {
    let mut tls = OwnedTls::new(false);
    let host = "owned-upstream.invalid";
    let a = OwnedProxy::new(tls.port("a")).await;
    let b = OwnedProxy::new(tls.port("b")).await;
    let mut original = AppConfig {
        port: 0,
        unified_key: "owned-proxy-matrix-key".into(),
        routing_strategy: RoutingStrategy::Priority,
        failover_enabled: false,
        ..Default::default()
    };
    original.cache.enabled = false;
    original.search.enabled = false;
    original.smart_routing.enabled = false;
    original.dedup.mode = crate::proxy::dedup::DedupMode::Off;
    let (a_url, b_url) = (a.url.clone(), b.url.clone());
    tokio::task::spawn_blocking(move || {
        crate::commands::replace_owned_proxy_via_ipc(original, &a_url, &b_url);
    })
    .await
    .unwrap();

    // The MockRuntime that issued the save is gone. The next gateway gets only
    // the file read by the production startup loader, not a saved IPC DTO or
    // its previous state/client pool.
    let (on_disk, warning) = AppConfig::load_or_init_with_warning().unwrap();
    assert!(warning.is_none(), "{warning:?}");
    assert_eq!(
        on_disk.http_proxy.as_deref(),
        Some(format!("{}/", b.url).as_str())
    );
    assert_eq!(on_disk.port, 0);
    assert_eq!(on_disk.bind, "127.0.0.1");
    assert_eq!(
        tls.state().await,
        json!({"a": 0, "b": 0, "offered": 0, "error": ""})
    );

    let db = Db::connect_in_memory().await.unwrap();
    let provider: Provider = serde_json::from_value(json!({
        "id": "owned-ipc-proxy", "name": "owned-ipc-proxy",
        "base_url": format!("https://{host}/v1"), "dialect": "openai",
        "api_key_enc": "", "enabled": true, "priority": 0, "rpm_limit": 0,
        "intelligence": 50, "created_at": "2026-10-20T00:00:00Z",
        "updated_at": "2026-10-20T00:00:00Z",
        "models": [{"alias": "owned-model", "upstream": "wire-model", "enabled": true,
            "context_window": 32768, "supports_tools": false, "supports_vision": false,
            "supports_thinking": false, "supports_stream": true, "model_type": "chat"}]
    }))
    .unwrap();
    repo::upsert_provider(db.pool(), &provider).await.unwrap();

    // A direct request would go to the owned A origin. Keep hostname and TLS
    // verification; the scoped inputs only affect construction of this client.
    let direct = std::net::SocketAddr::from(([127, 0, 0, 1], tls.port("a")));
    let state = Arc::new(OWNED_NETWORK_TEST_ROOT.sync_scope(tls.root(), || {
        OWNED_NETWORK_TEST_RESOLVE.sync_scope((host.to_owned(), direct), || {
            GatewayState::new(db.clone(), on_disk)
        })
    }));
    state.reload_providers().await.unwrap();
    let service = GatewayService::new(state.clone());
    let started = service.set_enabled(true).await.unwrap();
    let bound: std::net::SocketAddr = started.bound_addr.unwrap().parse().unwrap();
    assert!(bound.ip().is_loopback() && bound.port() != 0);
    let (status, body) = request(
        format!("http://{bound}"),
        "ipc-fresh-gateway",
        false,
        Arc::new(Notify::new()),
    )
    .await;
    let stopped = tokio::time::timeout(Duration::from_secs(5), service.set_enabled(false))
        .await
        .unwrap()
        .unwrap();
    assert!(!stopped.running);
    assert!(stopped.last_error.is_none(), "{stopped:?}");
    assert!(state.listener_snapshot().is_none());

    assert_eq!(status, 200, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "via-B");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["usage"]["prompt_tokens"], 3);
    assert_eq!(body["usage"]["completion_tokens"], 2);
    assert_eq!(a.calls.load(Ordering::SeqCst), 0);
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        tls.state().await,
        json!({"a": 0, "b": 1, "offered": 0, "error": ""})
    );
    let audits: Vec<(i64, String, String, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT status,routed_provider,routed_model,prompt_tokens,completion_tokens,error FROM requests",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        audits,
        vec![(
            200,
            "owned-ipc-proxy".into(),
            "wire-model".into(),
            3,
            2,
            None
        )]
    );
    tls.stop().await;
    eprintln!("N4 owned IPC proxy: A saved; invalid rejected unchanged; B saved and read on fresh gateway; CONNECT A=0 B=1; model POST A=0 B=1; audit=1; closed");
}
