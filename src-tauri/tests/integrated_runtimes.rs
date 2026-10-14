use llm_gateway_lib::{
    client_bridges::{self, BridgeManager, BridgeOptions},
    managed_runtime,
    vpn_xray::{self, XrayManager, XraySettings},
};
use serde_json::json;
use std::{fs, path::PathBuf};

struct PrivateDir(PathBuf);
struct MasterKeyGuard(Option<std::ffi::OsString>);
impl MasterKeyGuard {
    fn fixture() -> Self {
        use base64::Engine;
        let previous = std::env::var_os("LLMGW_MASTER_KEY");
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            base64::engine::general_purpose::STANDARD.encode([0x11; 32]),
        );
        Self(previous)
    }
}
impl Drop for MasterKeyGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.0 {
            std::env::set_var("LLMGW_MASTER_KEY", value);
        } else {
            std::env::remove_var("LLMGW_MASTER_KEY");
        }
    }
}
impl PrivateDir {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("llmgw-integrated-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn resources() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources")
}
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn two_ports() -> (u16, u16) {
    let first = free_port();
    loop {
        let second = free_port();
        if second != first {
            return (first, second);
        }
    }
}

#[test]
fn invalid_ports_and_bridge_kinds_are_rejected() {
    assert!(vpn_xray::validate_settings(&XraySettings {
        http_port: 0,
        socks_port: 2
    })
    .is_err());
    assert!(vpn_xray::validate_settings(&XraySettings {
        http_port: 2,
        socks_port: 2
    })
    .is_err());
    assert!(vpn_xray::validate_settings(&XraySettings {
        http_port: 2,
        socks_port: 3
    })
    .is_ok());
    assert!(client_bridges::defaults("arbitrary").is_err());
    assert!(client_bridges::validate(
        "trae",
        &BridgeOptions {
            port: 1,
            cdp_port: 1,
            client_path: "".into()
        }
    )
    .is_err());
    assert!(client_bridges::validate(
        "workbuddy",
        &BridgeOptions {
            port: 1,
            cdp_port: 2,
            client_path: "C:/Windows/System32/cmd.exe".into()
        }
    )
    .is_err());
}

#[test]
fn xray_import_selects_proxy_and_discards_foreign_listeners_and_routes() {
    let profile = json!({"log":{"access":"sensitive.log"},"inbounds":[{"listen":"0.0.0.0","protocol":"tun","port":9000}],"api":{"services":["HandlerService"]},"routing":{"rules":[{"outboundTag":"direct"}]},"outbounds":[{"protocol":"freedom"},{"tag":"source","protocol":"socks","settings":{"servers":[{"address":"127.0.0.1","port":30001}]}}]});
    let outbound = vpn_xray::imported_outbound(&profile).unwrap();
    assert_eq!(outbound["protocol"], "socks");
    assert_eq!(outbound["tag"], "VPN");
    let config = vpn_xray::managed_config(&XraySettings::default(), outbound).unwrap();
    assert_eq!(config["inbounds"].as_array().unwrap().len(), 2);
    for inbound in config["inbounds"].as_array().unwrap() {
        assert_eq!(inbound["listen"], "127.0.0.1");
        assert_ne!(inbound["protocol"], "tun");
    }
    assert!(config.get("routing").is_none());
    assert!(config.get("api").is_none());
    assert_eq!(config["log"], json!({"loglevel":"none"}));
}

#[test]
fn xray_unsupported_and_chained_profiles_are_explicit_errors() {
    for profile in [
        json!({}),
        json!({"outbounds":[{"protocol":"freedom"}]}),
        json!({"outbounds":[{"protocol":"socks","proxySettings":{"tag":"upstream"}}]}),
    ] {
        assert!(vpn_xray::imported_outbound(&profile).is_err());
    }
}

#[tokio::test]
async fn external_bridge_is_read_only_and_never_stopped_or_adopted() {
    let dir = PrivateDir::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cdp = free_port();
    let server = tokio::spawn(
        axum::serve(
            listener,
            axum::Router::new()
                .route(
                    "/health",
                    axum::routing::get(move || async move {
                        axum::Json(json!({"ok":true,"workbuddy":true,"cdpPort":cdp}))
                    }),
                )
                .route(
                    "/v1/models",
                    axum::routing::get(|| async {
                        axum::Json(json!({"data":[{"id":"fixture-text"}]}))
                    }),
                ),
        )
        .into_future(),
    );
    let manager = BridgeManager::new(dir.0.clone(), resources());
    manager
        .save_options(
            "workbuddy",
            BridgeOptions {
                port,
                cdp_port: cdp,
                client_path: "".into(),
            },
        )
        .await
        .unwrap();
    let status = manager.status("workbuddy").await.unwrap();
    assert!(status.ready && status.reachable);
    assert!(!status.managed && !status.model_call_verified);
    assert!(status.pid.is_none());
    assert!(manager
        .start("workbuddy")
        .await
        .unwrap_err()
        .contains("外部进程"));
    manager.stop("workbuddy").await.unwrap();
    assert!(
        manager.status("workbuddy").await.unwrap().ready,
        "stop cannot touch external service"
    );
    drop(manager);
    assert!(
        managed_runtime::read_json(&managed_runtime::local_http().unwrap(), port, "/health")
            .await
            .is_ok()
    );
    server.abort();
}

#[cfg(windows)]
#[tokio::test]
async fn bundled_bridge_launch_stop_shutdown_and_persistence_are_real() {
    let dir = PrivateDir::new();
    let manager = BridgeManager::new(dir.0.clone(), resources());
    let (port, cdp_port) = two_ports();
    let options = BridgeOptions {
        port,
        cdp_port,
        client_path: "".into(),
    };
    manager
        .save_options("workbuddy", options.clone())
        .await
        .unwrap();
    assert!(!manager.status("workbuddy").await.unwrap().reachable);
    manager.start("workbuddy").await.unwrap();
    let status = manager.status("workbuddy").await.unwrap();
    assert!(status.managed && status.reachable);
    assert!(
        !status.ready,
        "no desktop API fixture: must not report account ready"
    );
    assert_eq!(status.models.len(), 11);
    let pid = status.pid;
    manager.start("workbuddy").await.unwrap();
    assert_eq!(
        manager.status("workbuddy").await.unwrap().pid,
        pid,
        "idempotent launch keeps the process"
    );
    assert!(manager
        .save_options("workbuddy", options.clone())
        .await
        .is_err());
    manager.stop("workbuddy").await.unwrap();
    assert!(managed_runtime::port_free(port));
    assert!(!manager.status("workbuddy").await.unwrap().managed);
    manager.start("workbuddy").await.unwrap();
    manager.shutdown_blocking();
    assert!(managed_runtime::port_free(port));
    assert!(manager.start("workbuddy").await.is_err());
    let restored = BridgeManager::new(dir.0.clone(), resources());
    assert_eq!(restored.options("workbuddy").unwrap().port, port);
    assert!(restored.start("qoder").await.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn xray_actual_import_launch_http_proxy_stop_and_rollback_of_invalid_import() {
    let _key = MasterKeyGuard::fixture();
    let dir = PrivateDir::new();
    let manager = XrayManager::new(dir.0.clone(), resources());
    let (http_port, socks_port) = two_ports();
    let options = XraySettings {
        http_port,
        socks_port,
    };
    manager.save_settings(options.clone()).await.unwrap();
    assert!(manager.start().await.is_err(), "no profile must fail");
    let profile = dir.0.join("input.json");
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = upstream.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(socket.read_u8().await.unwrap());
            assert!(header.len() < 8192);
        }
        assert!(
            header.starts_with(b"CONNECT example.invalid:80 "),
            "HTTP outbound uses CONNECT, not a plain GET fixture"
        );
        socket
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        header.clear();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(socket.read_u8().await.unwrap());
            assert!(header.len() < 8192);
        }
        assert!(header.starts_with(b"GET /probe "));
        let body = "actual-xray-loopback-proxy";
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    fs::write(&profile, serde_json::to_vec(&json!({"outbounds":[{"protocol":"http","settings":{"servers":[{"address":"127.0.0.1","port":upstream_port,"users":[{"user":"fixture-user","pass":"fixture-xray-password"}]}]}}]})).unwrap()).unwrap();
    manager.import_profile(&profile).await.unwrap();
    let good = fs::read(dir.0.join("outbound.enc")).unwrap();
    assert!(!String::from_utf8_lossy(&good).contains("fixture-xray-password"));
    assert!(!dir.0.join("outbound.json").exists());
    fs::write(
        &profile,
        b"{\"outbounds\":[{\"protocol\":\"socks\",\"settings\":{\"servers\":[]}}]}",
    )
    .unwrap();
    assert!(
        manager.import_profile(&profile).await.is_err(),
        "real kernel rejects invalid proxy config"
    );
    assert_eq!(fs::read(dir.0.join("outbound.enc")).unwrap(), good);
    assert!(fs::read_dir(&dir.0).unwrap().all(|v| !v
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("validate-")));
    let occupied = std::net::TcpListener::bind(("127.0.0.1", http_port)).unwrap();
    assert!(manager.start().await.unwrap_err().contains("端口被占用"));
    assert!(!manager.status().running);
    drop(occupied);
    manager.start().await.unwrap();
    let status = manager.status();
    assert!(status.running && status.profile_ready && status.kernel_ready);
    assert_eq!(status.protocol.as_deref(), Some("http"));
    let http_client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::http(&status.proxy_url).unwrap())
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let response = http_client
        .get("http://example.invalid/probe")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.text().await.unwrap(),
        "actual-xray-loopback-proxy",
        "request traverses actual bundled Xray and a loopback HTTP proxy fixture"
    );
    assert!(manager.save_settings(options).await.is_err());
    assert!(manager.import_profile(&profile).await.is_err());
    let mut connection = tokio::net::TcpStream::connect(("127.0.0.1", socks_port))
        .await
        .unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    connection.write_all(&[5, 1, 0]).await.unwrap();
    let mut reply = [0; 2];
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        connection.read_exact(&mut reply),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reply, [5, 0], "actual Xray SOCKS handshake");
    let pid = status.pid;
    manager.start().await.unwrap();
    assert_eq!(manager.status().pid, pid);
    drop(connection);
    manager.stop().await.unwrap();
    assert!(!manager.status().running);
    assert!(!dir.0.join("managed.json").exists());
    assert!(managed_runtime::port_free(http_port) && managed_runtime::port_free(socks_port));
    manager.start().await.unwrap();
    manager.shutdown_blocking();
    assert!(!dir.0.join("managed.json").exists());
    assert!(managed_runtime::port_free(http_port) && managed_runtime::port_free(socks_port));
    assert!(manager.start().await.is_err());
    let restored = XrayManager::new(dir.0.clone(), resources());
    assert_eq!(restored.status().settings.http_port, http_port);
    assert!(restored.status().profile_ready && !restored.status().running);
    upstream_task.await.unwrap();
}

#[test]
fn runtime_hash_checks_reject_modified_and_unknown_binaries() {
    let dir = PrivateDir::new();
    fs::create_dir_all(dir.0.join("node")).unwrap();
    fs::write(dir.0.join("node/node.exe"), b"modified").unwrap();
    assert!(managed_runtime::verified_binary(&dir.0, "node").is_err());
    assert!(managed_runtime::verified_binary(&resources(), "arbitrary").is_err());
}

use std::future::IntoFuture;

#[tokio::test]
async fn qoder_atomic_registration_retries_and_preserves_user_disabled_provider() {
    use llm_gateway_lib::db::{repo, Db};
    let db = Db::connect_in_memory().await.unwrap();
    let first = repo::register_desktop_qoder(
        db.pool(),
        "C:/fixture/qodercn.cmd",
        "http://127.0.0.1:8790/v1",
    )
    .await
    .unwrap();
    let provider = repo::list_providers(db.pool()).await.unwrap().remove(0);
    assert!(provider.enabled);
    assert_eq!(provider.runtime_id.as_deref(), Some("desktop-qoder"));
    assert_eq!(provider.models.len(), 1);
    assert_eq!(provider.models[0].alias, "qoder-cli-auto");
    assert!(!provider.models[0].supports_tools);
    let runtime = repo::get_agent_runtime(db.pool(), "desktop-qoder")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runtime.kind, "qoder-cn");
    sqlx::query("UPDATE providers SET enabled=0,name='user-edited',priority=5 WHERE id=?")
        .bind(&first)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        repo::register_desktop_qoder(
            db.pool(),
            "C:/fixture/qodercn.cmd",
            "http://127.0.0.1:9999/v1"
        )
        .await
        .unwrap(),
        first
    );
    let saved = repo::list_providers(db.pool()).await.unwrap().remove(0);
    assert!(!saved.enabled);
    assert_eq!(saved.name, "user-edited");
    assert_eq!(saved.priority, 5);
    assert_eq!(saved.base_url, "http://127.0.0.1:8790/v1");
    assert!(
        repo::register_desktop_qoder(
            db.pool(),
            "C:/fixture/qodercli.cmd",
            "http://127.0.0.1:8790/v1"
        )
        .await
        .is_err(),
        "must not silently swap editions"
    );
}

#[tokio::test]
async fn qoder_model_insert_failure_rolls_back_runtime_and_provider_then_retry_completes() {
    use llm_gateway_lib::db::{repo, Db};
    let db = Db::connect_in_memory().await.unwrap();
    sqlx::query("CREATE TRIGGER fixture_fail_model BEFORE INSERT ON models BEGIN SELECT RAISE(ABORT,'fixture'); END").execute(db.pool()).await.unwrap();
    assert!(repo::register_desktop_qoder(
        db.pool(),
        "C:/fixture/qodercli.cmd",
        "http://127.0.0.1:8790/v1"
    )
    .await
    .is_err());
    assert!(repo::list_providers(db.pool()).await.unwrap().is_empty());
    assert!(repo::list_agent_runtimes(db.pool())
        .await
        .unwrap()
        .is_empty());
    sqlx::query("DROP TRIGGER fixture_fail_model")
        .execute(db.pool())
        .await
        .unwrap();
    repo::register_desktop_qoder(
        db.pool(),
        "C:/fixture/qodercli.cmd",
        "http://127.0.0.1:8790/v1",
    )
    .await
    .unwrap();
    assert_eq!(
        repo::get_agent_runtime(db.pool(), "desktop-qoder")
            .await
            .unwrap()
            .unwrap()
            .kind,
        "qoder",
        "international qodercli must not be detected as qoder-cn"
    );
    assert_eq!(repo::list_providers(db.pool()).await.unwrap().len(), 1);
}
