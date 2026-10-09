//! Offline contracts. The fixture core only binds loopback sockets; it cannot proxy traffic.

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use llm_gateway_lib::vpn::{
    managed_config, subscription_url, validate_settings, VpnController, VpnManager,
    VpnProfileInput, VpnSettings,
};
use serde_json::{json, Value};
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    sync::{Arc, Once},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("llmgw-vpn-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn test_key() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::env::set_var(
            "LLMGW_MASTER_KEY",
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
        )
    });
}

fn settings(kernel: String) -> VpnSettings {
    let first = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let second = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    VpnSettings {
        kernel_path: kernel,
        mixed_port: first.local_addr().unwrap().port(),
        controller_port: second.local_addr().unwrap().port(),
    }
}

#[test]
fn managed_main_configuration_uses_only_safe_fixed_fields() {
    let settings = VpnSettings::default();
    let config = managed_config(&settings, "offline-controller-secret").unwrap();
    assert_eq!(config["mixed-port"], 17890);
    assert_eq!(config["bind-address"], "127.0.0.1");
    assert_eq!(config["external-controller"], "127.0.0.1:17909");
    assert_eq!(config["allow-lan"], false);
    assert_eq!(config["tun"]["enable"], false);
    assert_eq!(config["dns"]["enable"], false);
    assert_eq!(config["proxy-providers"]["imported"]["type"], "file");
    assert_eq!(
        config["proxy-providers"]["imported"]["path"],
        "./nodes.yaml"
    );
    assert_eq!(config["proxy-groups"][0]["use"], json!(["imported"]));
    assert_eq!(config["proxy-groups"][0]["empty-fallback"], "REJECT");
    assert_eq!(config["rules"], json!(["MATCH,VPN"]));
    for forbidden in [
        "script",
        "listeners",
        "external-ui",
        "post-up",
        "post-down",
        "proxies",
    ] {
        assert!(config.get(forbidden).is_none(), "must not copy {forbidden}");
    }
    assert!(managed_config(&settings, "").is_err());
    let mut invalid = settings;
    invalid.mixed_port = 0;
    assert!(validate_settings(&invalid).is_err());
    invalid.mixed_port = invalid.controller_port;
    assert!(validate_settings(&invalid).is_err());
}

#[test]
fn subscriptions_require_https_without_credentials_or_fragment() {
    assert!(subscription_url("https://subscription.example.invalid/nodes?token=offline").is_ok());
    for invalid in [
        "http://127.0.0.1/nodes",
        "file:///secret",
        "https://user:pass@example.invalid/nodes",
        "https://example.invalid/nodes#fragment",
        "invalid",
    ] {
        let error = subscription_url(invalid).unwrap_err();
        assert!(!error.contains(invalid));
        assert!(!error.contains("pass"));
    }
}

#[tokio::test]
async fn profile_storage_is_encrypted_and_failed_import_keeps_previous_profile() {
    test_key();
    let directory = Directory::new();
    let manager = VpnManager::new(directory.0.join("owned"));
    let source = directory.0.join("profile.yaml");
    let profile = "proxies:\n  - { name: offline, type: socks5, server: 127.0.0.1, port: 9 }\ntun: { enable: true }\nexternal-controller: 0.0.0.0:1234\n";
    fs::write(&source, profile).unwrap();
    let status = manager
        .import_profile(VpnProfileInput {
            path: Some(source.to_string_lossy().into()),
            url: None,
        })
        .await
        .unwrap();
    assert!(status.profile_ready);
    assert!(!status.running);
    let encrypted = fs::read_to_string(directory.0.join("owned/profile.enc")).unwrap();
    assert!(!encrypted.contains("socks5"));
    assert_eq!(
        llm_gateway_lib::crypto::decrypt(&encrypted).unwrap(),
        profile
    );
    assert!(!directory.0.join("owned/runtime/nodes.yaml").exists());
    assert!(!serde_json::to_string(&status).unwrap().contains("proxies:"));

    for invalid in [vec![], vec![0xff], vec![b'a'; 2 * 1024 * 1024 + 1]] {
        fs::write(&source, invalid).unwrap();
        assert!(manager
            .import_profile(VpnProfileInput {
                path: Some(source.to_string_lossy().into()),
                url: None
            })
            .await
            .is_err());
        assert_eq!(
            fs::read_to_string(directory.0.join("owned/profile.enc")).unwrap(),
            encrypted
        );
    }
    assert!(manager
        .import_profile(VpnProfileInput {
            path: Some(source.to_string_lossy().into()),
            url: Some("https://example.invalid/offline".into())
        })
        .await
        .is_err());
    assert!(manager
        .import_profile(VpnProfileInput {
            path: None,
            url: Some("http://127.0.0.1/offline".into())
        })
        .await
        .is_err());
    manager.stop().await.unwrap();
    assert!(directory.0.join("owned/profile.enc").exists());
}

#[derive(Default)]
struct MockState {
    calls: parking_lot::Mutex<Vec<(String, String, Value)>>,
    reject: bool,
    imported: Option<Value>,
    proxies: Option<Value>,
}

async fn controller_mock(State(state): State<Arc<MockState>>, request: Request<Body>) -> Response {
    if request
        .headers()
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        != Some("Bearer offline-controller-secret")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if state.reject {
        return (StatusCode::UNAUTHORIZED, "sensitive subscription https://private.invalid/?token=secret offline-controller-secret").into_response();
    }
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let bytes = to_bytes(request.into_body(), 4096).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    state
        .calls
        .lock()
        .push((method.clone(), path.clone(), body));
    if method == "GET" && path == "/version" {
        return Json(json!({ "meta": true, "version": "mock-v1" })).into_response();
    }
    if method == "GET" && path == "/proxies" {
        return Json(state.proxies.clone().unwrap_or_else(|| json!({ "proxies": {
            "GLOBAL": { "type": "Selector", "now": "DIRECT", "all": ["DIRECT", "VPN"] },
            "VPN": { "type": "Selector", "now": "offline", "all": ["offline"] },
            "VPN /?#中文": { "name": "VPN /?#中文", "type": "Selector", "now": "offline", "all": ["offline", "second"] },
            "auto": { "type": "URLTest", "all": ["offline"] },
            "offline": { "type": "Socks5", "all": ["must-not-be-a-group"] }
        } }))).into_response();
    }
    if method == "GET" && path == "/providers/proxies/imported" {
        return Json(state.imported.clone().unwrap_or_else(|| json!({ "name": "imported", "type": "Proxy", "vehicleType": "File", "proxies": [{ "name": "offline", "type": "Socks5" }] }))).into_response();
    }
    if (method == "PATCH" && path == "/configs")
        || (method == "PUT" && path.starts_with("/proxies/"))
    {
        return StatusCode::NO_CONTENT.into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn mock_controller(state: Arc<MockState>) -> (VpnController, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .fallback(any(controller_mock))
        .with_state(state);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        VpnController::new(port, "offline-controller-secret".into()).unwrap(),
        task,
    )
}

#[tokio::test]
async fn controller_contract_encodes_group_names_and_rejects_invalid_mutations() {
    let state = Arc::new(MockState::default());
    let (controller, task) = mock_controller(state.clone()).await;
    assert_eq!(controller.version().await.unwrap(), "mock-v1");
    let proxies = controller.proxies().await.unwrap();
    assert!(proxies
        .iter()
        .find(|proxy| proxy.name == "offline")
        .unwrap()
        .members
        .is_empty());
    controller
        .select_proxy("VPN /?#中文", "second")
        .await
        .unwrap();
    controller.set_mode("global").await.unwrap();
    let calls = state.calls.lock().clone();
    let put = calls.iter().find(|call| call.0 == "PUT").unwrap();
    assert_eq!(put.1, "/proxies/VPN%20%2F%3F%23%E4%B8%AD%E6%96%87");
    assert_eq!(put.2, json!({ "name": "second" }));
    assert!(calls.iter().any(|call| call.0 == "PATCH"
        && call.1 == "/configs"
        && call.2 == json!({ "mode": "global" })));
    assert!(calls.iter().any(|call| call.0 == "PUT"
        && call.1 == "/proxies/GLOBAL"
        && call.2 == json!({ "name": "VPN" })));
    assert!(controller.select_proxy("auto", "offline").await.is_err());
    assert!(controller
        .select_proxy("VPN /?#中文", "missing")
        .await
        .is_err());
    assert!(controller.select_proxy("missing", "offline").await.is_err());
    assert!(controller.set_mode("invalid").await.is_err());
    let final_calls = state.calls.lock();
    assert_eq!(final_calls.iter().filter(|call| call.0 == "PUT").count(), 2);
    assert_eq!(
        final_calls.iter().filter(|call| call.0 == "PATCH").count(),
        1
    );
    drop(final_calls);
    task.abort();
}

#[tokio::test]
async fn controller_errors_do_not_echo_response_or_secret() {
    let (controller, task) = mock_controller(Arc::new(MockState {
        reject: true,
        ..Default::default()
    }))
    .await;
    let error = controller.version().await.unwrap_err();
    assert!(error.contains("401"));
    assert!(!error.contains("private.invalid"));
    assert!(!error.contains("offline-controller-secret"));
    task.abort();
}

#[tokio::test]
async fn actual_provider_members_are_required_and_global_never_silently_defaults_to_direct() {
    let (controller, task) = mock_controller(Arc::new(MockState::default())).await;
    controller.verify_imported_nodes().await.unwrap();
    task.abort();
    for state in [
        MockState {
            imported: Some(json!({ "proxies": [] })),
            ..Default::default()
        },
        MockState {
            imported: Some(json!({ "proxies": [{ "name": "COMPATIBLE", "type": "Direct" }] })),
            ..Default::default()
        },
        MockState {
            proxies: Some(
                json!({ "proxies": { "VPN": { "type": "Selector", "all": ["REJECT"] } } }),
            ),
            ..Default::default()
        },
    ] {
        let (controller, task) = mock_controller(Arc::new(state)).await;
        assert!(controller
            .verify_imported_nodes()
            .await
            .unwrap_err()
            .contains("节点文件无效"));
        task.abort();
    }
    let state = Arc::new(MockState {
        proxies: Some(json!({ "proxies": { "VPN": { "type": "Selector", "all": ["offline"] } } })),
        ..Default::default()
    });
    let (controller, task) = mock_controller(state.clone()).await;
    assert!(controller.set_mode("global").await.is_err());
    assert!(state.calls.lock().iter().all(|call| call.0 != "PATCH"));
    controller.set_mode("direct").await.unwrap();
    assert!(state.calls.lock().iter().any(|call| call.0 == "PATCH"));
    task.abort();
}

#[tokio::test]
async fn occupied_ports_are_never_adopted_or_stopped() {
    test_key();
    let directory = Directory::new();
    let manager = VpnManager::new(directory.0.join("owned"));
    let occupied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut configured = settings(std::env::current_exe().unwrap().to_string_lossy().into());
    configured.controller_port = occupied.local_addr().unwrap().port();
    manager.save_settings(configured.clone()).await.unwrap();
    let source = directory.0.join("nodes.yaml");
    fs::write(&source, "proxies: []").unwrap();
    manager
        .import_profile(VpnProfileInput {
            path: Some(source.to_string_lossy().into()),
            url: None,
        })
        .await
        .unwrap();
    assert!(manager
        .start()
        .await
        .unwrap_err()
        .contains("控制端口已被占用"));
    assert!(!manager.status().await.unwrap().running);
    manager.stop().await.unwrap();
    assert!(TcpListener::bind(("127.0.0.1", configured.controller_port)).is_err());
    assert!(!directory.0.join("owned/runtime/config.json").exists());
}

const MOCK_CORE: &str = r###"
use std::{fs, io::{Read,Write}, net::TcpListener, path::PathBuf};
fn field<'a>(config: &'a str, name: &str) -> &'a str {
    let marker = format!("\"{}\": \"", name);
    config.split(&marker).nth(1).unwrap().split('"').next().unwrap()
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let root = PathBuf::from(&args[args.iter().position(|s| s == "-d").unwrap()+1]);
    let config = fs::read_to_string(root.join("config.json")).unwrap();
    let profile = fs::read_to_string(root.join("nodes.yaml")).unwrap();
    if std::env::vars().any(|(name,_)| name.starts_with("CLASH_")) { std::process::exit(4); }
    if args.iter().any(|s| s == "-t") {
        if profile.contains("HANG_VALIDATION") {
            fs::write(root.join("validation.pid"), std::process::id().to_string()).unwrap();
            loop { std::thread::sleep(std::time::Duration::from_secs(1)); }
        }
        if profile.contains("FAIL_VALIDATION") { std::process::exit(2); }
        fs::write(root.join("validated"), "yes").unwrap();
        return;
    }
    if !root.join("validated").exists() { std::process::exit(3); }
    if profile.contains("EXIT_AFTER_VALIDATION") { std::process::exit(5); }
    let secret = field(&config, "secret");
    let listener = TcpListener::bind(field(&config, "external-controller")).unwrap();
    let mixed_port: u16 = config.split("\"mixed-port\": ").nth(1).unwrap().split(',').next().unwrap().trim().parse().unwrap();
    let _mixed = TcpListener::bind(("127.0.0.1", mixed_port)).unwrap();
    let mut selected_global = false;
    for connection in listener.incoming() {
        let mut socket = connection.unwrap();
        let mut buffer = [0u8;8192];
        let length = socket.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..length]);
        if !request.to_ascii_lowercase().contains(&format!("authorization: bearer {}", secret).to_ascii_lowercase()) {
            socket.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap(); continue;
        }
        if request.starts_with("PUT /proxies/GLOBAL ") { selected_global = true; }
        let body = if request.starts_with("GET /version ") { r#"{"meta":true,"version":"offline-mock-core"}"# }
            else if request.starts_with("GET /providers/proxies/imported ") {
                if profile.contains("EMPTY_NODES") { r#"{"name":"imported","type":"Proxy","vehicleType":"File","proxies":[]}"# }
                else { r#"{"name":"imported","type":"Proxy","vehicleType":"File","proxies":[{"name":"offline","type":"Socks5"}]}"# }
            }
            else if request.starts_with("GET /proxies ") {
                if profile.contains("EMPTY_NODES") { r#"{"proxies":{"VPN":{"type":"Selector","now":"REJECT","all":["REJECT"]},"REJECT":{"type":"Reject"}}}"# }
                else { r#"{"proxies":{"GLOBAL":{"type":"Selector","now":"__GLOBAL__","all":["DIRECT","VPN"]},"VPN":{"type":"Selector","now":"offline","all":["offline"]},"offline":{"type":"Socks5"},"DIRECT":{"type":"Direct"}}}"# }
            }
            else { "" };
        let body = body.replace("__GLOBAL__", if selected_global { "VPN" } else { "DIRECT" });
        let status = if body.is_empty() { "204 No Content" } else { "200 OK" };
        write!(socket,"HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",status,body.len(),body).unwrap();
    }
}
"###;

#[tokio::test]
async fn mock_core_lifecycle_validates_starts_serially_and_cleans_plaintext() {
    test_key();
    let directory = Directory::new();
    let source = directory.0.join("mock_core.rs");
    fs::write(&source, MOCK_CORE).unwrap();
    let binary = directory.0.join(if cfg!(windows) {
        "mock_core.exe"
    } else {
        "mock_core"
    });
    let compiler = std::process::Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compiler.status.success(),
        "mock core fixture must compile: {}",
        String::from_utf8_lossy(&compiler.stderr)
    );
    let root = directory.0.join("owned");
    let manager = Arc::new(VpnManager::new(root.clone()));
    let configured = settings(binary.to_string_lossy().into());
    manager.save_settings(configured.clone()).await.unwrap();
    let profile = directory.0.join("nodes.yaml");
    fs::write(&profile, "HANG_VALIDATION").unwrap();
    manager
        .import_profile(VpnProfileInput {
            path: Some(profile.to_string_lossy().into()),
            url: None,
        })
        .await
        .unwrap();
    let pending_manager = manager.clone();
    let pending = tokio::spawn(async move { pending_manager.start().await });
    let pid_file = root.join("runtime/validation.pid");
    for _ in 0..50 {
        if pid_file.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        manager.diagnostics_snapshot().is_none(),
        "read-only diagnostics must not wait for or mutate a busy lifecycle"
    );
    assert!(root.join("runtime/config.json").exists());
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert!(
        pid_file.exists(),
        "mock validation must actually have started"
    );
    let canceled_pid: u32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    for _ in 0..20 {
        if !pid_exists(canceled_pid) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        !pid_exists(canceled_pid),
        "canceling startup must kill the owned validator"
    );
    assert!(!root.join("runtime/nodes.yaml").exists());
    assert!(!root.join("runtime/config.json").exists());
    for (marker, expected) in [
        ("FAIL_VALIDATION", "校验失败"),
        ("EXIT_AFTER_VALIDATION", "启动时退出"),
        ("EMPTY_NODES", "节点文件无效"),
    ] {
        fs::write(&profile, marker).unwrap();
        manager
            .import_profile(VpnProfileInput {
                path: Some(profile.to_string_lossy().into()),
                url: None,
            })
            .await
            .unwrap();
        assert!(manager.start().await.unwrap_err().contains(expected));
        assert!(!manager.status().await.unwrap().running);
        assert!(!root.join("runtime/nodes.yaml").exists());
        assert!(!root.join("runtime/config.json").exists());
    }
    fs::write(&profile, "proxies:\n  - { name: offline, type: socks5, server: 127.0.0.1, port: 9 }\ntun: { enable: true }\n").unwrap();
    manager
        .import_profile(VpnProfileInput {
            path: Some(profile.to_string_lossy().into()),
            url: None,
        })
        .await
        .unwrap();
    let previous_post_up = std::env::var_os("CLASH_POST_UP");
    std::env::set_var("CLASH_POST_UP", "OFFLINE_TEST_MUST_NOT_RUN");
    let start = manager.start().await;
    match previous_post_up {
        Some(value) => std::env::set_var("CLASH_POST_UP", value),
        None => std::env::remove_var("CLASH_POST_UP"),
    }
    let started = start.unwrap();
    assert!(started.running);
    assert!(started.pid.is_some());
    assert_eq!(started.version.as_deref(), Some("offline-mock-core"));
    let config: Value =
        serde_json::from_slice(&fs::read(root.join("runtime/config.json")).unwrap()).unwrap();
    assert_eq!(config["tun"]["enable"], false);
    assert!(!serde_json::to_string(&started)
        .unwrap()
        .contains(config["secret"].as_str().unwrap()));
    assert_eq!(manager.start().await.unwrap().pid, started.pid);
    assert!(manager.diagnostics_snapshot().unwrap().running);
    assert!(manager
        .install_kernel()
        .await
        .unwrap_err()
        .contains("先停止"));
    assert!(manager
        .rollback_kernel()
        .await
        .unwrap_err()
        .contains("先停止"));
    assert!(manager
        .save_settings(configured.clone())
        .await
        .unwrap_err()
        .contains("先停止"));
    assert!(manager
        .import_profile(VpnProfileInput {
            path: Some(profile.to_string_lossy().into()),
            url: None
        })
        .await
        .unwrap_err()
        .contains("先停止"));
    assert_eq!(manager.proxies().await.unwrap().len(), 4);
    manager.select_proxy("VPN", "offline").await.unwrap();
    manager.set_mode("global").await.unwrap();
    assert_eq!(manager.status().await.unwrap().mode, "global");
    assert_eq!(
        manager
            .proxies()
            .await
            .unwrap()
            .iter()
            .find(|proxy| proxy.name == "GLOBAL")
            .unwrap()
            .now
            .as_deref(),
        Some("VPN")
    );
    let stopped = manager.stop().await.unwrap();
    assert!(!stopped.running);
    assert!(root.join("profile.enc").exists());
    assert!(!root.join("runtime/nodes.yaml").exists());
    assert!(!root.join("runtime/config.json").exists());
    assert!(TcpListener::bind(("127.0.0.1", configured.mixed_port)).is_ok());
    assert!(TcpListener::bind(("127.0.0.1", configured.controller_port)).is_ok());
    let (first, second) = tokio::join!(manager.start(), manager.start());
    assert_eq!(first.unwrap().pid, second.unwrap().pid);
    manager.shutdown_blocking();
    assert!(!manager.status().await.unwrap().running);
    assert!(!root.join("runtime/config.json").exists());
    assert!(manager.start().await.is_err());
    let restored = VpnManager::new(root);
    let status = restored.status().await.unwrap();
    assert_eq!(status.settings, configured);
    assert!(status.profile_ready);
    assert!(!status.running);
}

fn pid_exists(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap();
        assert!(output.status.success(), "process check must work");
        String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
    }
    #[cfg(not(windows))]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}

/// Explicit offline acceptance: uses a supplied, separately verified official binary.
#[tokio::test]
#[ignore = "requires LLMGW_TEST_MIHOMO_PATH; no download, upstream traffic, TUN or system proxy"]
async fn real_mihomo_file_provider_offline_acceptance() {
    test_key();
    let kernel = std::env::var("LLMGW_TEST_MIHOMO_PATH").expect("set verified Mihomo binary path");
    let directory = Directory::new();
    let root = directory.0.join("owned");
    let manager = VpnManager::new(root.clone());
    let configured = settings(kernel);
    manager.save_settings(configured.clone()).await.unwrap();
    let profile = directory.0.join("nodes.yaml");
    fs::write(&profile, "proxies:\n  - { name: offline-localhost-only, type: socks5, server: 127.0.0.1, port: 9 }\ntun: { enable: true }\nexternal-controller: 0.0.0.0:1234\n").unwrap();
    manager
        .import_profile(VpnProfileInput {
            path: Some(profile.to_string_lossy().into()),
            url: None,
        })
        .await
        .unwrap();
    let started = manager.start().await.unwrap();
    println!(
        "verified official core version: {}",
        started.version.as_deref().unwrap()
    );
    assert!(started.running);
    let runtime_config: Value =
        serde_json::from_slice(&fs::read(root.join("runtime/config.json")).unwrap()).unwrap();
    let config: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!(
            "http://127.0.0.1:{}/configs",
            configured.controller_port
        ))
        .bearer_auth(runtime_config["secret"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(config["allow-lan"], false);
    assert_eq!(config["tun"]["enable"], false);
    manager
        .select_proxy("VPN", "offline-localhost-only")
        .await
        .unwrap();
    manager.set_mode("global").await.unwrap();
    assert_eq!(
        manager
            .proxies()
            .await
            .unwrap()
            .iter()
            .find(|proxy| proxy.name == "GLOBAL")
            .unwrap()
            .now
            .as_deref(),
        Some("VPN")
    );
    manager.stop().await.unwrap();
    for invalid in ["proxies: []\n", "proxies: not-a-list\n"] {
        fs::write(&profile, invalid).unwrap();
        manager
            .import_profile(VpnProfileInput {
                path: Some(profile.to_string_lossy().into()),
                url: None,
            })
            .await
            .unwrap();
        let error = manager.start().await.unwrap_err();
        assert!(
            error.contains("节点文件无效"),
            "controlled invalid-node error: {error}"
        );
        assert!(!manager.status().await.unwrap().running);
        assert!(!root.join("runtime/nodes.yaml").exists());
        assert!(!root.join("runtime/config.json").exists());
        assert!(TcpListener::bind(("127.0.0.1", configured.controller_port)).is_ok());
    }
}
