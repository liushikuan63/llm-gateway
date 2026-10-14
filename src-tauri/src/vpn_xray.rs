//! Private, bundled Xray. Only a selected outbound is imported, never foreign listeners/TUN/API.
use crate::managed_runtime::{self, OwnedProcess};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct XraySettings {
    pub http_port: u16,
    pub socks_port: u16,
}
impl Default for XraySettings {
    fn default() -> Self {
        Self {
            http_port: 17891,
            socks_port: 17892,
        }
    }
}
pub fn validate_settings(settings: &XraySettings) -> Result<(), String> {
    if settings.http_port == 0
        || settings.socks_port == 0
        || settings.http_port == settings.socks_port
    {
        return Err("HTTP 与 SOCKS 端口须为不同的非零端口".into());
    }
    Ok(())
}

pub fn imported_outbound(profile: &Value) -> Result<Value, String> {
    let outbounds = profile
        .get("outbounds")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "请导入含 outbounds 的 Xray JSON 配置；Clash YAML 请使用 Mihomo".to_string()
        })?;
    let mut selected = outbounds
        .iter()
        .find(|v| {
            matches!(
                v["protocol"].as_str(),
                Some("vless" | "vmess" | "trojan" | "shadowsocks" | "socks" | "http" | "wireguard")
            )
        })
        .cloned()
        .ok_or_else(|| {
            "配置没有支持的代理出站（VLESS/VMess/Trojan/Shadowsocks/SOCKS/HTTP/WireGuard）"
                .to_string()
        })?;
    if selected.get("proxySettings").is_some() {
        return Err("当前导入支持单一代理出站；请先导出不依赖其他出站的节点配置".into());
    }
    selected
        .as_object_mut()
        .ok_or_else(|| "代理出站无效".to_string())?
        .insert("tag".into(), json!("VPN"));
    Ok(selected)
}

pub fn managed_config(settings: &XraySettings, outbound: Value) -> Result<Value, String> {
    validate_settings(settings)?;
    Ok(json!({
        "log": { "loglevel": "none" },
        "inbounds": [
            { "listen": "127.0.0.1", "port": settings.http_port, "protocol": "http", "settings": { "allowTransparent": false } },
            { "listen": "127.0.0.1", "port": settings.socks_port, "protocol": "socks", "settings": { "auth": "noauth", "udp": false } }
        ],
        "outbounds": [outbound]
    }))
}

#[derive(Clone, Debug, Serialize)]
pub struct XrayStatus {
    pub settings: XraySettings,
    pub kernel_ready: bool,
    pub profile_ready: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub version: String,
    pub protocol: Option<String>,
    pub proxy_url: String,
}
pub struct XrayManager {
    root: PathBuf,
    resources: PathBuf,
    settings: parking_lot::Mutex<XraySettings>,
    child: parking_lot::Mutex<Option<OwnedProcess>>,
    runtime_file: parking_lot::Mutex<Option<TemporaryConfig>>,
    gate: tokio::sync::Mutex<()>,
    closing: AtomicBool,
}
struct TemporaryConfig(PathBuf);
impl Drop for TemporaryConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
struct Startup<'a> {
    manager: &'a XrayManager,
    committed: bool,
}
impl Drop for Startup<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.manager.child.lock().take();
            self.manager.runtime_file.lock().take();
        }
    }
}
impl XrayManager {
    pub fn diagnostics_snapshot(&self) -> Option<crate::diagnostics::XrayDiagnostics> {
        let _gate = self.gate.try_lock().ok()?;
        let settings = self.settings.try_lock()?;
        let child = self.child.try_lock()?;
        let running = match child.as_ref() {
            Some(child) => child.readonly_running()?,
            None => false,
        };
        Some(crate::diagnostics::XrayDiagnostics {
            running,
            http_port: settings.http_port,
            socks_port: settings.socks_port,
        })
    }
    pub fn new(root: PathBuf, resources: PathBuf) -> Self {
        let settings = fs::read(root.join("settings.json"))
            .ok()
            .and_then(|v| serde_json::from_slice::<XraySettings>(&v).ok())
            .filter(|s| validate_settings(s).is_ok())
            .unwrap_or_default();
        Self {
            root,
            resources,
            settings: parking_lot::Mutex::new(settings),
            child: parking_lot::Mutex::new(None),
            runtime_file: parking_lot::Mutex::new(None),
            gate: tokio::sync::Mutex::new(()),
            closing: AtomicBool::new(false),
        }
    }
    fn outbound(&self) -> Option<Value> {
        let mut encrypted = String::new();
        fs::File::open(self.root.join("outbound.enc"))
            .ok()?
            .take(3 * 1024 * 1024 + 1)
            .read_to_string(&mut encrypted)
            .ok()?;
        if encrypted.len() > 3 * 1024 * 1024 {
            return None;
        }
        let plain = crate::crypto::decrypt(&encrypted).ok()?;
        serde_json::from_str(&plain).ok()
    }
    pub fn status(&self) -> XrayStatus {
        let settings = self.settings.lock().clone();
        let mut child = self.child.lock();
        if child.as_mut().is_some_and(|c| !c.running()) {
            child.take();
            self.runtime_file.lock().take();
        }
        let pid = child.as_ref().map(OwnedProcess::pid);
        let protocol = self
            .outbound()
            .and_then(|v| v["protocol"].as_str().map(str::to_owned));
        XrayStatus {
            proxy_url: format!("http://127.0.0.1:{}", settings.http_port),
            settings,
            kernel_ready: self.resources.join("xray/xray.exe").is_file(),
            profile_ready: protocol.is_some(),
            running: pid.is_some(),
            pid,
            version: "v26.3.27".into(),
            protocol,
        }
    }
    pub async fn save_settings(&self, settings: XraySettings) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        validate_settings(&settings)?;
        if self.status().running {
            return Err("请先停止 Xray 再修改端口".into());
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err("软件正在退出".into());
        }
        fs::create_dir_all(&self.root).map_err(|_| "无法创建 Xray 设置目录".to_string())?;
        crate::vpn_install::atomic_write(
            &self.root.join("settings.json"),
            &serde_json::to_vec(&settings).map_err(|_| "Xray 设置无效".to_string())?,
        )?;
        *self.settings.lock() = settings;
        Ok(())
    }
    pub async fn import_profile(&self, path: &Path) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        if self.status().running {
            return Err("请先停止 Xray 再导入配置".into());
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err("软件正在退出".into());
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(|_| "无法读取所选配置文件".to_string())?
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "无法读取配置".to_string())?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("配置超过 2 MiB".into());
        }
        let profile: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "文件不是有效的 Xray JSON 配置".to_string())?;
        let outbound = imported_outbound(&profile)?;
        let settings = self.settings.lock().clone();
        let config = managed_config(&settings, outbound.clone())?;
        // Test with the actual fixed kernel before publishing the private profile.
        let binary = managed_runtime::verified_binary(&self.resources, "xray")?;
        fs::create_dir_all(&self.root).map_err(|_| "无法创建 Xray 配置目录".to_string())?;
        let stage = self
            .root
            .join(format!("validate-{}.json", uuid::Uuid::new_v4()));
        let stage_cleanup = TemporaryConfig(stage.clone());
        crate::vpn_install::atomic_write(
            &stage,
            &serde_json::to_vec(&config).map_err(|_| "配置无效".to_string())?,
        )?;
        let mut command = Command::new(binary);
        command
            .args(["run", "-test", "-c"])
            .arg(&stage)
            .current_dir(&self.root);
        let mut process = managed_runtime::spawn(command)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let validated = loop {
            if self.closing.load(Ordering::SeqCst) {
                break false;
            }
            match process.0.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                _ => break false,
            }
        };
        drop(process);
        drop(stage_cleanup);
        if !validated {
            return Err("Xray 配置验证未通过或超时；已有配置已保留".into());
        }
        let text = serde_json::to_string(&outbound).map_err(|_| "配置无效".to_string())?;
        let encrypted = crate::crypto::encrypt(&text)
            .map_err(|_| "Xray 节点加密失败，已有配置保留".to_string())?;
        if self.closing.load(Ordering::SeqCst) {
            return Err("软件正在退出".into());
        }
        crate::vpn_install::atomic_write(&self.root.join("outbound.enc"), encrypted.as_bytes())
    }
    pub async fn start(&self) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        if self.status().running {
            return Ok(());
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err("软件正在退出".into());
        }
        let settings = self.settings.lock().clone();
        if !managed_runtime::port_free(settings.http_port)
            || !managed_runtime::port_free(settings.socks_port)
        {
            return Err("Xray 端口被占用，未修改外部进程".into());
        }
        let binary = managed_runtime::verified_binary(&self.resources, "xray")?;
        let outbound = self
            .outbound()
            .ok_or_else(|| "请先导入 Xray JSON 节点配置".to_string())?;
        let config = managed_config(&settings, outbound)?;
        let path = self.root.join("managed.json");
        let runtime_file = TemporaryConfig(path.clone());
        crate::vpn_install::atomic_write(
            &path,
            &serde_json::to_vec(&config).map_err(|_| "配置无效".to_string())?,
        )?;
        let mut command = Command::new(binary);
        command
            .args(["run", "-c"])
            .arg(path)
            .current_dir(&self.root);
        {
            let mut child = self.child.lock();
            if self.closing.load(Ordering::SeqCst) {
                return Err("软件正在退出".into());
            }
            *child = Some(managed_runtime::spawn(command)?);
            *self.runtime_file.lock() = Some(runtime_file);
        }
        let mut startup = Startup {
            manager: self,
            committed: false,
        };
        for _ in 0..50 {
            if !self.status().running {
                return Err("Xray 提前退出，请重新导入有效配置".into());
            }
            let (http, socks) = tokio::join!(
                tokio::net::TcpStream::connect(("127.0.0.1", settings.http_port)),
                tokio::net::TcpStream::connect(("127.0.0.1", settings.socks_port))
            );
            if http.is_ok() && socks.is_ok() {
                startup.committed = true;
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.child.lock().take();
        Err("Xray 启动超时，自有进程已清理".into())
    }
    pub async fn stop(&self) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        let mut child = self.child.lock();
        if let Some(child) = child.as_mut() {
            child.terminate()?;
        }
        child.take();
        self.runtime_file.lock().take();
        Ok(())
    }
    pub fn shutdown_blocking(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.child.lock().take();
        self.runtime_file.lock().take();
    }
}
impl Drop for XrayManager {
    fn drop(&mut self) {
        self.shutdown_blocking();
    }
}
