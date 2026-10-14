//! Fixed bundled runtimes and app-owned processes. Never adopts a PID from a port.
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

pub struct OwnedProcess(pub Child);
impl OwnedProcess {
    /// Polls the held process handle without waiting/reaping or changing Child state.
    pub fn readonly_running(&self) -> Option<bool> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{
                Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
                System::Threading::WaitForSingleObject,
            };
            match unsafe { WaitForSingleObject(self.0.as_raw_handle() as _, 0) } {
                WAIT_TIMEOUT => Some(true),
                WAIT_OBJECT_0 => Some(false),
                _ => None,
            }
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
    pub fn running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
    pub fn pid(&self) -> u32 {
        self.0.id()
    }
    pub fn terminate(&mut self) -> Result<(), String> {
        self.terminate_with(crate::proc_util::kill_tree_blocking)
    }
    fn terminate_with(&mut self, tree_kill: impl FnOnce(u32)) -> Result<(), String> {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return Ok(());
        }
        tree_kill(self.0.id());
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return Ok(());
        }
        // std::Child has no kill_on_drop. Use its owned process handle if tree kill failed.
        self.0
            .kill()
            .map_err(|_| "无法停止自有进程，请检查安全软件或权限".to_string())?;
        self.0
            .wait()
            .map_err(|_| "无法确认自有进程已退出".to_string())?;
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn handle_fallback_stops_real_owned_node_when_tree_kill_fails() {
        let resources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        let binary = verified_binary(&resources, "node").unwrap();
        let ready = std::env::temp_dir().join(format!("llmgw-node-ready-{}", uuid::Uuid::new_v4()));
        let mut command = Command::new(binary);
        command
            .args([
                "-e",
                "require('fs').writeFileSync(process.argv[1],'ready');setInterval(()=>{},1000)",
            ])
            .arg(&ready);
        let mut child = spawn(command).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let acknowledged = ready.is_file();
        let _ = std::fs::remove_file(&ready);
        assert!(
            acknowledged && child.running(),
            "real child must acknowledge startup and remain alive"
        );
        child
            .terminate_with(|_| { /* failed tree-kill boundary; no process is killed here */ })
            .unwrap();
        assert_eq!(child.readonly_running(), Some(false));
        assert!(
            child.0.try_wait().unwrap().is_some(),
            "owned-handle fallback must actually stop it"
        );
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if self.terminate().is_err() {
            tracing::warn!("自有运行进程清理失败");
        }
    }
}

pub fn spawn(mut command: Command) -> Result<OwnedProcess, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
        .spawn()
        .map(OwnedProcess)
        .map_err(|_| "无法启动内置运行进程，请检查权限和安全软件".into())
}

pub fn verified_binary(resources: &Path, name: &str) -> Result<PathBuf, String> {
    let (entry, hash) = match name {
        "node" => (
            "node.exe",
            "3602f2bb1a10f2cbab4c36886218a33c1ab3db87290e73b033c46c77147d0237",
        ),
        "xray" => (
            "xray.exe",
            "15c2d007954ac53ba69b80ec91242786b3c0b71d52649165b4ca1d5cc96ef8f1",
        ),
        _ => return Err("未知内置运行环境".into()),
    };
    let binary = resources.join(name).join(entry);
    crate::vpn_install::verify_hash(&binary, hash, 128 * 1024 * 1024)
        .map_err(|_| format!("内置 {name} 文件缺失或校验失败，请重新安装完整软件包"))?;
    crate::vpn_install::verify_windows_amd64(&binary)?;
    Ok(binary)
}

pub fn port_free(port: u16) -> bool {
    port != 0 && std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok()
}

pub fn local_http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|_| "无法初始化本机连接".into())
}

/// Bounded local responses; arbitrary local servers must not fill memory or follow redirects.
pub async fn read_json(
    client: &reqwest::Client,
    port: u16,
    path: &str,
) -> Result<serde_json::Value, String> {
    use futures_util::StreamExt;
    let response = client
        .get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .await
        .map_err(|_| "本机服务未响应".to_string())?;
    if !response.status().is_success() {
        return Err("本机服务尚未就绪".into());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "本机服务响应中断".to_string())?;
        if bytes.len() + chunk.len() > 128 * 1024 {
            return Err("本机服务响应过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "本机服务响应格式无效".into())
}
