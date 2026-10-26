//! Owned transport observations only; no system proxy, real DNS or supplier.
use super::*;
use axum::{routing::get, Router};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn n4_mapped_local_destination_never_enters_explicit_proxy() {
    let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = direct.local_addr().unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    let direct_count = Arc::new(AtomicUsize::new(0));
    let proxy_count = Arc::new(AtomicUsize::new(0));
    let direct_task = tokio::spawn({
        let count = direct_count.clone();
        async move {
            axum::serve(
                direct,
                Router::new().route(
                    "/",
                    get(move || {
                        let count = count.clone();
                        async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            "direct"
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        }
    });
    let proxy_task = tokio::spawn({
        let count = proxy_count.clone();
        async move {
            axum::serve(
                proxy,
                Router::new().fallback(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        "via-proxy"
                    }
                }),
            )
            .await
            .unwrap();
        }
    });
    let ordinary = format!("http://{addr}/");
    let mapped = format!("http://[::ffff:127.0.0.1]:{}/", addr.port());
    let client = apply_proxy(Client::builder(), Some(&format!("http://{proxy_addr}")))
        .unwrap()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    assert_eq!(
        client
            .get(&ordinary)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "direct"
    );
    let result = client.get(&mapped).send().await;
    let mapped_connected = match result {
        Ok(response) => {
            let text = response.text().await.unwrap();
            assert_eq!(
                text,
                "direct",
                "mapped local destination leaked into proxy; proxy_calls={}",
                proxy_count.load(Ordering::SeqCst)
            );
            true
        }
        Err(error) => {
            assert!(
                error.is_connect(),
                "only a local connect limitation is admissible: {error}"
            );
            false
        }
    };
    assert_eq!(proxy_count.load(Ordering::SeqCst),0,"local destination must never be delegated to a proxy, even on a platform that cannot connect it");
    assert_eq!(
        client
            .get(&ordinary)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "direct"
    );
    assert_eq!(
        direct_count.load(Ordering::SeqCst),
        if mapped_connected { 3 } else { 2 }
    );
    for address in [
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.8",
        "::ffff:172.16.0.8",
        "::ffff:192.168.0.8",
        "::ffff:169.254.0.8",
    ] {
        assert!(
            is_local_target(&Url::parse(&format!("http://[{address}]/")).unwrap()),
            "{address}"
        );
    }
    assert!(!is_local_target(
        &Url::parse("http://[::ffff:203.0.113.8]/").unwrap()
    ));
    eprintln!("N4 owned mapped transport: direct_controls=2, mapped_connected={mapped_connected}, proxy_calls=0; unsupported mapped connect is NOT a connectivity pass");
    direct_task.abort();
    proxy_task.abort();
}

#[tokio::test]
async fn n4_proxy_disconnect_and_error_never_fall_back_to_reachable_owned_origin() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let origin_task = tokio::spawn({
        let count = count.clone();
        async move {
            axum::serve(
                origin,
                Router::new().route(
                    "/",
                    get(move || {
                        let count = count.clone();
                        async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            "owned-origin"
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        }
    });
    let url = format!("http://owned-origin.invalid:{}/", origin_addr.port());
    let direct = Client::builder()
        .no_proxy()
        .resolve("owned-origin.invalid", origin_addr)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    assert_eq!(
        direct.get(&url).send().await.unwrap().text().await.unwrap(),
        "owned-origin"
    );
    for mode in ["disconnect", "status503", "timeout"] {
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn({
            let seen = seen.clone();
            async move {
                let (mut wire, _) = proxy.accept().await.unwrap();
                seen.fetch_add(1, Ordering::SeqCst);
                let mut data = Vec::new();
                while !data.windows(4).any(|w| w == b"\r\n\r\n") {
                    let mut chunk = [0u8; 2048];
                    let n = wire.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&chunk[..n]);
                    assert!(data.len() <= 8192);
                }
                assert!(
                    String::from_utf8_lossy(&data).starts_with("GET http://owned-origin.invalid:")
                );
                match mode {
                "status503"=>wire.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap(),
                "timeout"=>{std::future::pending::<()>().await;},
                _=>{wire.shutdown().await.unwrap();}
            }
            }
        });
        let client = apply_proxy(
            Client::builder().resolve("owned-origin.invalid", origin_addr),
            Some(&format!("http://{proxy_addr}")),
        )
        .unwrap()
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap();
        let result = client.get(&url).send().await;
        if mode == "status503" {
            assert_eq!(result.unwrap().status().as_u16(), 503);
        } else {
            let error = result.unwrap_err();
            if mode == "timeout" {
                assert!(error.is_timeout());
            }
        }
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "{mode}: proxy failure must not try the reachable direct origin"
        );
        task.abort();
    }
    assert_eq!(
        direct.get(&url).send().await.unwrap().text().await.unwrap(),
        "owned-origin"
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    origin_task.abort();
}
