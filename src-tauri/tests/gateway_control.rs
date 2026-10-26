//! Isolated owned-listener tests; no providers, client processes or OS settings.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use llm_gateway_lib::config::AppConfig;
use llm_gateway_lib::db::Db;
use llm_gateway_lib::proxy::server::{serve, GatewayState};
use llm_gateway_lib::proxy_control::gateway::GatewayService;
use tokio::net::TcpListener;

async fn fixture(port: u16) -> (Arc<GatewayState>, Arc<GatewayService>) {
    let cfg = AppConfig {
        bind: "127.0.0.1".into(),
        port,
        unified_key: "isolated-gateway-control-key".into(),
        ..Default::default()
    };
    let state = Arc::new(GatewayState::new(
        Db::connect_in_memory().await.unwrap(),
        cfg,
    ));
    let service = Arc::new(GatewayService::new(state.clone()));
    (state, service)
}

async fn health(addr: &str) -> reqwest::StatusCode {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn default_stopped_start_is_bound_and_stop_releases_port() {
    let (state, service) = fixture(0).await;
    assert!(!service.status().await.running);
    assert!(state.listener_snapshot().is_none());

    let started = service.set_enabled(true).await.unwrap();
    assert!(started.running, "enable must start an owned listener");
    let addr = started.bound_addr.as_deref().unwrap();
    assert_eq!(health(addr).await, reqwest::StatusCode::OK);
    assert_eq!(
        state.listener_snapshot().unwrap().bound_addr.to_string(),
        addr
    );
    assert_eq!(service.set_enabled(true).await.unwrap(), started);

    let stopped = service.set_enabled(false).await.unwrap();
    assert!(!stopped.running);
    assert!(stopped.bound_addr.is_none());
    assert!(state.listener_snapshot().is_none());
    let replacement = TcpListener::bind(addr).await.unwrap();
    assert_eq!(replacement.local_addr().unwrap().to_string(), addr);
    assert_eq!(service.set_enabled(false).await.unwrap(), stopped);
}

#[tokio::test]
async fn conflict_is_an_error_and_never_stops_or_adopts_external_healthz() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let external_addr = listener.local_addr().unwrap();
    let external = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route("/healthz", axum::routing::get(|| async { "external" })),
        )
        .await
        .unwrap();
    });
    let (state, service) = fixture(external_addr.port()).await;
    let error = service.set_enabled(true).await.unwrap_err();
    let status = service.status().await;
    assert!(!status.running);
    assert!(status.bound_addr.is_none());
    assert_eq!(status.last_error.as_deref(), Some(error.as_str()));
    assert!(state.listener_snapshot().is_none());
    assert_eq!(
        health(&external_addr.to_string()).await,
        reqwest::StatusCode::OK
    );
    service.set_enabled(false).await.unwrap();
    assert_eq!(
        health(&external_addr.to_string()).await,
        reqwest::StatusCode::OK
    );
    external.abort();
    assert!(external.await.unwrap_err().is_cancelled());

    let restarted = service.set_enabled(true).await.unwrap();
    assert!(restarted.running);
    assert!(restarted.last_error.is_none());
    service.set_enabled(false).await.unwrap();
}

#[tokio::test]
async fn repeated_restart_waits_for_old_snapshot_cleanup() {
    let (state, service) = fixture(0).await;
    let first = service.set_enabled(true).await.unwrap();
    let addr: SocketAddr = first.bound_addr.unwrap().parse().unwrap();
    service.set_enabled(false).await.unwrap();
    // Keep the same configured port to expose premature listener release/rebind.
    let mut cfg = state.cfg_snapshot();
    cfg.port = addr.port();
    state.update_cfg(cfg);
    for _ in 0..8 {
        let running = service.set_enabled(true).await.unwrap();
        assert_eq!(
            running.bound_addr.as_deref(),
            Some(addr.to_string().as_str())
        );
        tokio::task::yield_now().await;
        assert_eq!(state.listener_snapshot().unwrap().bound_addr, addr);
        assert!(service.status().await.running);
        assert_eq!(health(&addr.to_string()).await, reqwest::StatusCode::OK);
        service.set_enabled(false).await.unwrap();
        assert!(state.listener_snapshot().is_none());
        drop(TcpListener::bind(addr).await.unwrap());
    }
}

#[tokio::test]
async fn concurrent_commands_share_one_owned_listener() {
    let (state, service) = fixture(0).await;
    let (one, two, three) = tokio::join!(
        service.set_enabled(true),
        service.set_enabled(true),
        service.set_enabled(true)
    );
    let one = one.unwrap();
    assert!(one.running);
    assert_eq!(one, two.unwrap());
    assert_eq!(one, three.unwrap());
    let (off, on) = tokio::join!(service.set_enabled(false), service.set_enabled(true));
    assert!(!off.unwrap().running);
    assert!(on.unwrap().running);
    assert!(state.listener_snapshot().is_some());
    service.set_enabled(false).await.unwrap();
}

#[tokio::test]
async fn cancelled_start_and_stop_calls_keep_task_ownership() {
    let (state, service) = fixture(0).await;
    let mut start = Box::pin(service.set_enabled(true));
    assert!(futures_util::poll!(start.as_mut()).is_pending());
    drop(start);
    let status = service.status().await;
    assert!(status.running);
    let addr = status.bound_addr.unwrap();

    let mut stop = Box::pin(service.set_enabled(false));
    let _ = futures_util::poll!(stop.as_mut());
    drop(stop);
    let stopped = service.status().await;
    assert!(!stopped.running);
    assert!(state.listener_snapshot().is_none());
    drop(TcpListener::bind(&addr).await.unwrap());
    assert!(service.set_enabled(true).await.unwrap().running);
    service.set_enabled(false).await.unwrap();
}

#[tokio::test]
async fn invalid_remote_mode_reports_failure_before_binding() {
    let (state, service) = fixture(0).await;
    let mut cfg = state.cfg_snapshot();
    cfg.remote_mode.enabled = true;
    cfg.remote_mode.public_url = None;
    state.update_cfg(cfg);
    assert!(service.set_enabled(true).await.is_err());
    assert!(!service.status().await.running);
    assert!(state.listener_snapshot().is_none());
}

#[tokio::test]
async fn public_serve_still_explicitly_starts_the_listener() {
    let (state, service) = fixture(0).await;
    let task = tokio::spawn(serve(state.clone()));
    let addr = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(snapshot) = state.listener_snapshot() {
                break snapshot.bound_addr;
            }
            assert!(!task.is_finished(), "legacy serve exited before binding");
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(health(&addr.to_string()).await, reqwest::StatusCode::OK);
    // The controller does not own a caller's direct serve() task.
    assert!(!service.status().await.running);
    service.set_enabled(false).await.unwrap();
    assert_eq!(health(&addr.to_string()).await, reqwest::StatusCode::OK);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(state.listener_snapshot().is_none());
}

#[tokio::test]
async fn old_direct_serve_cannot_clear_a_new_owned_listener_snapshot() {
    let (state, service) = fixture(0).await;
    let old = tokio::spawn(serve(state.clone()));
    let old_addr = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(snapshot) = state.listener_snapshot() {
                break snapshot.bound_addr;
            }
            assert!(!old.is_finished());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let started = service.set_enabled(true).await.unwrap();
    let new_addr: SocketAddr = started.bound_addr.unwrap().parse().unwrap();
    assert_ne!(old_addr, new_addr);
    old.abort();
    assert!(old.await.unwrap_err().is_cancelled());
    assert_eq!(state.listener_snapshot().unwrap().bound_addr, new_addr);
    assert_eq!(health(&new_addr.to_string()).await, reqwest::StatusCode::OK);
    service.set_enabled(false).await.unwrap();
    assert!(state.listener_snapshot().is_none());
}

#[tokio::test]
async fn dropping_the_last_controller_owner_does_not_detach_listener() {
    let (state, service) = fixture(0).await;
    let running = service.set_enabled(true).await.unwrap();
    let addr = running.bound_addr.unwrap();
    drop(service);
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.listener_snapshot().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(TcpListener::bind(&addr).await.unwrap());
}

#[tokio::test]
async fn stopping_gateway_closes_an_existing_keepalive_client_connection() {
    let (_state, service) = fixture(0).await;
    let addr = service.set_enabled(true).await.unwrap().bound_addr.unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .retry(reqwest::retry::never())
        .pool_max_idle_per_host(1)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let url = format!("http://{addr}/healthz");
    // Consume the body so the same client retains an idle pooled connection.
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        "ok"
    );
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        "ok"
    );
    assert!(!service.set_enabled(false).await.unwrap().running);
    drop(TcpListener::bind(&addr).await.unwrap());
    let after_stop = client.get(&url).send().await;
    assert!(
        after_stop.is_err(),
        "an existing keepalive connection still accepted healthz after stop: {after_stop:?}"
    );
}

#[tokio::test]
async fn dropping_last_owner_closes_an_existing_keepalive_client_connection() {
    let (state, service) = fixture(0).await;
    let addr = service.set_enabled(true).await.unwrap().bound_addr.unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let url = format!("http://{addr}/healthz");
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        "ok"
    );
    drop(service);
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.listener_snapshot().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(client.get(&url).send().await.is_err());
    drop(TcpListener::bind(&addr).await.unwrap());
}
