//! Desktop bridge code is compiled into the application; Node is an offline bundle resource.
use crate::managed_runtime::{self, OwnedProcess};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeOptions {
    pub port: u16,
    pub cdp_port: u16,
    pub client_path: String,
}
pub fn defaults(kind: &str) -> Result<BridgeOptions, String> {
    let (port, cdp_port) = match kind {
        "qoder" => (8790, 0),
        "workbuddy" => (8791, 9224),
        "trae" => (8792, 9226),
        _ => return Err("未知客户端".into()),
    };
    Ok(BridgeOptions {
        port,
        cdp_port,
        client_path: String::new(),
    })
}
pub fn validate(kind: &str, options: &BridgeOptions) -> Result<(), String> {
    defaults(kind)?;
    if options.port == 0
        || kind != "qoder" && (options.cdp_port == 0 || options.port == options.cdp_port)
    {
        return Err("桥接端口与调试端口须为不同的非零端口".into());
    }
    if !options.client_path.is_empty() {
        let path = std::path::Path::new(&options.client_path);
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let accepted = match kind {
            "workbuddy" => ["workbuddyai.exe", "workbuddy.exe"].contains(&name.as_str()),
            "trae" => ["trae cn.exe", "trae.exe"].contains(&name.as_str()),
            _ => false,
        };
        if !path.is_absolute() || !path.is_file() || !accepted {
            return Err("请选择对应客户端的已安装 EXE 文件".into());
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct BridgeStatus {
    pub kind: String,
    pub options: BridgeOptions,
    pub reachable: bool,
    pub ready: bool,
    pub managed: bool,
    pub pid: Option<u32>,
    pub models: Vec<String>,
    pub runtime_ready: bool,
    pub cli_path: Option<String>,
    pub error: Option<String>,
    pub model_call_verified: bool,
}

pub struct BridgeManager {
    root: PathBuf,
    resources: PathBuf,
    options: parking_lot::Mutex<HashMap<String, BridgeOptions>>,
    children: parking_lot::Mutex<HashMap<String, OwnedProcess>>,
    gate: tokio::sync::Mutex<()>,
    closing: AtomicBool,
}
impl BridgeManager {
    pub fn new(root: PathBuf, resources: PathBuf) -> Self {
        let mut options = HashMap::new();
        for kind in ["qoder", "workbuddy", "trae"] {
            let saved = fs::read(root.join(format!("{kind}.json")))
                .ok()
                .and_then(|v| serde_json::from_slice::<BridgeOptions>(&v).ok())
                .filter(|o| validate(kind, o).is_ok());
            options.insert(
                kind.into(),
                saved.unwrap_or_else(|| defaults(kind).expect("fixed kind")),
            );
        }
        Self {
            root,
            resources,
            options: parking_lot::Mutex::new(options),
            children: parking_lot::Mutex::new(HashMap::new()),
            gate: tokio::sync::Mutex::new(()),
            closing: AtomicBool::new(false),
        }
    }
    pub fn options(&self, kind: &str) -> Result<BridgeOptions, String> {
        self.options
            .lock()
            .get(kind)
            .cloned()
            .ok_or_else(|| "未知客户端".into())
    }
    fn owned_pid(&self, kind: &str) -> Option<u32> {
        let mut children = self.children.lock();
        if children.get_mut(kind).is_some_and(|c| c.running()) {
            children.get(kind).map(OwnedProcess::pid)
        } else {
            children.remove(kind);
            None
        }
    }
    pub async fn save_options(&self, kind: &str, options: BridgeOptions) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        validate(kind, &options)?;
        if self.owned_pid(kind).is_some() {
            return Err("请先停止软件管理的桥接，再修改设置".into());
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err("软件正在退出".into());
        }
        fs::create_dir_all(&self.root).map_err(|_| "无法创建桥接设置目录".to_string())?;
        crate::vpn_install::atomic_write(
            &self.root.join(format!("{kind}.json")),
            &serde_json::to_vec(&options).map_err(|_| "设置无效".to_string())?,
        )?;
        self.options.lock().insert(kind.into(), options);
        Ok(())
    }
    pub async fn status(&self, kind: &str) -> Result<BridgeStatus, String> {
        let options = self.options(kind)?;
        let pid = self.owned_pid(kind);
        let reachable = !managed_runtime::port_free(options.port);
        let client = managed_runtime::local_http()?;
        let health = managed_runtime::read_json(&client, options.port, "/health")
            .await
            .ok();
        let catalog = managed_runtime::read_json(&client, options.port, "/v1/models")
            .await
            .ok();
        let models = catalog
            .as_ref()
            .and_then(|v| v.get("data"))
            .and_then(Value::as_array)
            .map(|data| {
                data.iter()
                    .filter_map(|v| v.get("id").and_then(Value::as_str))
                    .filter(|id| {
                        !id.is_empty() && id.len() <= 200 && !id.chars().any(char::is_control)
                    })
                    .take(100)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let ready = health.as_ref().is_some_and(|v| {
            v["ok"] == true
                && if kind == "qoder" {
                    v["accounts_ready"].as_u64().unwrap_or(0) > 0
                } else {
                    v[kind] == true && v["cdpPort"].as_u64() == Some(options.cdp_port as u64)
                }
        }) && !models.is_empty();
        let error = if ready {
            None
        } else if reachable {
            Some("服务端口已占用但未就绪；请核对客户端登录、调试端口和桥接设置".into())
        } else {
            Some(
                if kind == "qoder" {
                    "HTTP 桥接未启动；也可直接使用软件内的 Qoder CLI 上游"
                } else {
                    "桥接未启动；先登录客户端并以调试端口启动，再启动桥接"
                }
                .into(),
            )
        };
        let cli_path = if kind == "qoder" {
            crate::cli_tools::resolve_binary(
                &["qodercn", "qoder", "qoderclicn", "qodercli"],
                &std::env::var("PATH").unwrap_or_default(),
                &crate::cli_tools::well_known_dirs(),
            )
            .map(|p| p.display().to_string())
        } else {
            None
        };
        Ok(BridgeStatus {
            kind: kind.into(),
            options,
            reachable,
            ready,
            managed: pid.is_some(),
            pid,
            models,
            runtime_ready: self.resources.join("node/node.exe").is_file(),
            cli_path,
            error,
            model_call_verified: false,
        })
    }
    pub async fn list(&self) -> Result<Vec<BridgeStatus>, String> {
        let (q, w, t) = tokio::join!(
            self.status("qoder"),
            self.status("workbuddy"),
            self.status("trae")
        );
        Ok(vec![q?, w?, t?])
    }
    pub async fn start(&self, kind: &str) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        if kind == "qoder" {
            return Err("Qoder 已有原生 CLI 上游；HTTP 网关由外部账号工具管理".into());
        }
        let options = self.options(kind)?;
        if self.owned_pid(kind).is_some() {
            return Ok(());
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err("软件正在退出".into());
        }
        if !managed_runtime::port_free(options.port) {
            return Err("端口被外部进程占用，已保留原进程；请选择空闲桥接端口".into());
        }
        let node = managed_runtime::verified_binary(&self.resources, "node")?;
        let directory = self.root.join("runtime");
        fs::create_dir_all(&directory).map_err(|_| "无法创建内置桥接目录".to_string())?;
        // Identical fixed source only. Settings, credentials and arbitrary scripts are never embedded.
        for (file, code) in [
            ("package.json", "{\"private\":true,\"type\":\"commonjs\"}"),
            (
                "bridge-runtime.js",
                include_str!("../../scripts/local-client-bridges/bridge-runtime.js"),
            ),
            (
                "workbuddy-bridge.js",
                include_str!("../../scripts/local-client-bridges/workbuddy-bridge.js"),
            ),
            (
                "trae-bridge.js",
                include_str!("../../scripts/local-client-bridges/trae-bridge.js"),
            ),
        ] {
            crate::vpn_install::atomic_write(&directory.join(file), code.as_bytes())?;
        }
        let mut command = Command::new(node);
        command
            .arg(directory.join(format!("{kind}-bridge.js")))
            .args([
                "--port",
                &options.port.to_string(),
                "--cdp",
                &options.cdp_port.to_string(),
            ])
            .current_dir(&directory);
        {
            let mut children = self.children.lock();
            if self.closing.load(Ordering::SeqCst) {
                return Err("软件正在退出".into());
            }
            children.insert(kind.into(), managed_runtime::spawn(command)?);
        }
        let client = managed_runtime::local_http()?;
        let mut startup = BridgeStartup {
            manager: self,
            kind,
            committed: false,
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if self.owned_pid(kind).is_none() {
                return Err("内置桥接提前退出，请检查运行环境或端口冲突".into());
            }
            // Listening/catalogue readiness is separate from desktop account readiness.
            if managed_runtime::read_json(&client, options.port, "/v1/models")
                .await
                .is_ok()
            {
                startup.committed = true;
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        self.children.lock().remove(kind);
        Err("桥接启动超时，自有进程已清理".into())
    }
    pub async fn stop(&self, kind: &str) -> Result<(), String> {
        self.options(kind)?;
        let _gate = self.gate.lock().await;
        let mut children = self.children.lock();
        if let Some(child) = children.get_mut(kind) {
            child.terminate()?;
        }
        children.remove(kind);
        Ok(())
    }
    pub async fn launch_client(&self, kind: &str) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        let options = self.options(kind)?;
        validate(kind, &options)?;
        if kind == "qoder" || options.client_path.is_empty() {
            return Err("请先选择 WorkBuddy 或 Trae 客户端 EXE 并保存设置".into());
        }
        let client = managed_runtime::local_http()?;
        if managed_runtime::read_json(&client, options.cdp_port, "/json/version")
            .await
            .is_ok()
        {
            return Ok(());
        }
        if !managed_runtime::port_free(options.cdp_port) {
            return Err("调试端口被占用，请核对设置".into());
        }
        let mut command = Command::new(&options.client_path);
        command
            .arg(format!("--remote-debugging-port={}", options.cdp_port))
            .arg("--remote-debugging-address=127.0.0.1");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command.spawn().map_err(|_| "无法启动客户端".to_string())?;
        // Client belongs to the user and remains running when gateway exits.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            if managed_runtime::read_json(&client, options.cdp_port, "/json/version")
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err("客户端未开放调试端口。若已普通启动，请自行退出客户端，再用本按钮启动；未强制关闭你的会话".into())
    }
    pub fn shutdown_blocking(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.children.lock().clear();
    }
}
impl Drop for BridgeManager {
    fn drop(&mut self) {
        self.shutdown_blocking();
    }
}
struct BridgeStartup<'a> {
    manager: &'a BridgeManager,
    kind: &'a str,
    committed: bool,
}
impl Drop for BridgeStartup<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.manager.children.lock().remove(self.kind);
        }
    }
}
