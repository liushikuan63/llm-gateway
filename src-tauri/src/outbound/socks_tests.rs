//! Inert NOAUTH SOCKS5 fixture: forwards only to one captured owned loopback.
use super::apply_proxy;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};

#[derive(Clone, Copy)]
pub(crate) enum Mode {
    Forward,
    RejectMethod,
    RejectConnect,
    Eof,
    Stall,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Stats {
    pub accepted: usize,
    pub targets: Vec<(u8, String, u16)>,
    pub errors: Vec<String>,
    pub stopped: bool,
}
pub(crate) struct OwnedSocks {
    pub url: String,
    addr: SocketAddr,
    stats: Arc<Mutex<Stats>>,
    task: Option<JoinHandle<()>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for OwnedSocks {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl OwnedSocks {
    pub async fn new(scheme: &str, origin: SocketAddr, expected_port: u16, mode: Mode) -> Self {
        assert!(matches!(scheme, "socks5" | "socks5h"));
        assert!(origin.ip().is_loopback());
        assert_ne!(expected_port, 0);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stats = Arc::new(Mutex::new(Stats::default()));
        let (shutdown, mut stopping) = tokio::sync::oneshot::channel();
        let task = tokio::spawn({
            let stats = stats.clone();
            async move {
                let mut children = JoinSet::new();
                loop {
                    tokio::select! {
                        _=&mut stopping=>break,
                        incoming=listener.accept()=>{
                            let (wire,_)=incoming.unwrap();stats.lock().unwrap().accepted+=1;let stats=stats.clone();
                            children.spawn(async move {if let Err(error)=serve(wire,origin,expected_port,mode,stats.clone()).await {stats.lock().unwrap().errors.push(error);}});
                        },
                        result=children.join_next(),if !children.is_empty()=>{if let Some(Err(error))=result {stats.lock().unwrap().errors.push(error.to_string());}}
                    }
                }
                drop(listener);
                children.abort_all();
                while children.join_next().await.is_some() {}
                stats.lock().unwrap().stopped = true;
            }
        });
        Self {
            url: format!("{scheme}://{addr}"),
            addr,
            stats,
            task: Some(task),
            shutdown: Some(shutdown),
        }
    }
    pub fn stats(&self) -> Stats {
        self.stats.lock().unwrap().clone()
    }
    pub async fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
        }
        assert!(
            self.stats().stopped,
            "owned listener/task shutdown was not acknowledged"
        );
        eprintln!(
            "owned SOCKS listener {} dropped and child tasks joined",
            self.addr
        );
    }
}
async fn serve(
    mut wire: TcpStream,
    origin: SocketAddr,
    expected_port: u16,
    mode: Mode,
    stats: Arc<Mutex<Stats>>,
) -> Result<(), String> {
    if matches!(mode, Mode::Eof) {
        return Ok(());
    }
    if matches!(mode, Mode::Stall) {
        std::future::pending::<()>().await;
    }
    let mut hello = [0; 2];
    wire.read_exact(&mut hello)
        .await
        .map_err(|e| e.to_string())?;
    if hello[0] != 5 || hello[1] == 0 {
        return Err("unexpected owned SOCKS greeting".into());
    }
    let mut methods = vec![0; hello[1] as usize];
    wire.read_exact(&mut methods)
        .await
        .map_err(|e| e.to_string())?;
    if !methods.contains(&0) {
        return Err("NOAUTH not offered".into());
    }
    if matches!(mode, Mode::RejectMethod) {
        wire.write_all(&[5, 255]).await.map_err(|e| e.to_string())?;
        return Ok(());
    }
    wire.write_all(&[5, 0]).await.map_err(|e| e.to_string())?;
    let mut header = [0; 4];
    wire.read_exact(&mut header)
        .await
        .map_err(|e| e.to_string())?;
    if header[..3] != [5, 1, 0] {
        return Err("fixture accepts only SOCKS5 CONNECT".into());
    }
    let host = match header[3] {
        1 => {
            let mut b = [0; 4];
            wire.read_exact(&mut b).await.map_err(|e| e.to_string())?;
            std::net::Ipv4Addr::from(b).to_string()
        }
        3 => {
            let n = wire.read_u8().await.map_err(|e| e.to_string())?;
            let mut b = vec![0; n as usize];
            wire.read_exact(&mut b).await.map_err(|e| e.to_string())?;
            String::from_utf8(b).map_err(|e| e.to_string())?
        }
        4 => {
            let mut b = [0; 16];
            wire.read_exact(&mut b).await.map_err(|e| e.to_string())?;
            std::net::Ipv6Addr::from(b).to_string()
        }
        _ => return Err("unknown SOCKS address type".into()),
    };
    let port = wire.read_u16().await.map_err(|e| e.to_string())?;
    if !["owned-upstream.invalid", "owned-socks.invalid", "127.0.0.1"].contains(&host.as_str())
        || port != expected_port
    {
        return Err(format!("unexpected owned destination {host}:{port}"));
    }
    stats.lock().unwrap().targets.push((header[3], host, port));
    if matches!(mode, Mode::RejectConnect) {
        wire.write_all(&[5, 5, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let mut target = TcpStream::connect(origin)
        .await
        .map_err(|e| e.to_string())?;
    wire.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
        .await
        .map_err(|e| e.to_string())?;
    // TLS/HTTP peers may finish by FIN or reset; protocol errors above remain
    // observable. No host supplied by the wire is ever resolved or connected.
    let _ = tokio::io::copy_bidirectional(&mut wire, &mut target).await;
    Ok(())
}

#[tokio::test]
async fn n4_socks_shared_client_wire_dns_and_local_bypass() {
    use axum::{routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = origin.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let hits = hits.clone();
        async move {
            axum::serve(
                origin,
                Router::new().route(
                    "/",
                    get(move || {
                        let hits = hits.clone();
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            "owned-origin"
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        }
    });
    for (scheme, kind, host) in [
        ("socks5", 1, "127.0.0.1"),
        ("socks5h", 3, "owned-socks.invalid"),
    ] {
        let mut proxy = OwnedSocks::new(scheme, addr, addr.port(), Mode::Forward).await;
        let client = apply_proxy(
            reqwest::Client::builder().resolve("owned-socks.invalid", addr),
            Some(&proxy.url),
        )
        .unwrap()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
        assert_eq!(
            client
                .get(format!("http://owned-socks.invalid:{}/", addr.port()))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "owned-origin"
        );
        assert_eq!(
            proxy.stats().targets,
            vec![(kind, host.into(), addr.port())]
        );
        assert_eq!(
            client
                .get(format!("http://{addr}/"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "owned-origin"
        );
        assert_eq!(proxy.stats().accepted, 1);
        assert!(proxy.stats().errors.is_empty());
        proxy.stop().await;
    }
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    task.abort();
    let _ = task.await;
    eprintln!("N4 owned SOCKS shared client: socks5 ATYP1 / socks5h ATYP3; proxy CONNECT2, owned HTTP4 including direct controls; listeners stopped");
}

#[tokio::test]
async fn n4_socks_handshake_failures_never_fall_back_to_reachable_owned_direct() {
    use axum::{routing::get, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = origin.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let hits = hits.clone();
        async move {
            axum::serve(
                origin,
                Router::new().route(
                    "/",
                    get(move || {
                        let hits = hits.clone();
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            "owned-direct"
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        }
    });
    let url = format!("http://owned-socks.invalid:{}/", addr.port());
    let control = reqwest::Client::builder()
        .no_proxy()
        .resolve("owned-socks.invalid", addr)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    assert_eq!(
        control
            .get(&url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "owned-direct"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    for mode in [
        Mode::RejectMethod,
        Mode::RejectConnect,
        Mode::Eof,
        Mode::Stall,
    ] {
        let mut proxy = OwnedSocks::new("socks5h", addr, addr.port(), mode).await;
        let client = apply_proxy(
            reqwest::Client::builder().resolve("owned-socks.invalid", addr),
            Some(&proxy.url),
        )
        .unwrap()
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
        let error = client.get(&url).send().await.unwrap_err();
        if matches!(mode, Mode::Stall) {
            assert!(error.is_timeout());
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "SOCKS failure fell back to a reachable direct origin"
        );
        assert_eq!(proxy.stats().accepted, 1);
        assert!(proxy.stats().errors.is_empty());
        proxy.stop().await;
    }
    assert_eq!(
        control
            .get(&url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "owned-direct"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    task.abort();
    let _ = task.await;
    eprintln!("N4 owned SOCKS failures: method/connect/EOF/stall4, proxied origin HTTP0, reachable direct controls2; no fallback or external DNS");
}
