//! Owns a private Mihomo child process and its loopback controller.
//! Imported Clash YAML is only a file proxy-provider, never the main configuration.

pub use crate::vpn_install::VpnKernelInfo;
use crate::vpn_install::{self, KernelInstallState};
use futures_util::StreamExt;
use reqwest::{redirect::Policy, Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::Mutex,
    time::{sleep, Instant},
};

const PROFILE_LIMIT: usize = 2 * 1024 * 1024;
const START_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VpnSettings {
    pub kernel_path: String,
    pub mixed_port: u16,
    pub controller_port: u16,
}

impl Default for VpnSettings {
    fn default() -> Self {
        Self {
            kernel_path: String::new(),
            mixed_port: 17890,
            controller_port: 17909,
        }
    }
}

#[derive(Deserialize)]
pub struct VpnProfileInput {
    pub path: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VpnStatus {
    pub settings: VpnSettings,
    pub running: bool,
    pub kernel_ready: bool,
    pub profile_ready: bool,
    pub pid: Option<u32>,
    pub version: Option<String>,
    pub proxy_url: String,
    pub mode: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VpnReadonlyStatus {
    pub running: bool,
    pub mixed_port: u16,
    pub has_error: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct VpnProxy {
    pub name: String,
    pub kind: String,
    pub now: Option<String>,
    pub members: Vec<String>,
}

pub fn validate_settings(settings: &VpnSettings) -> Result<(), String> {
    if settings.mixed_port == 0
        || settings.controller_port == 0
        || settings.mixed_port == settings.controller_port
    {
        return Err("代理端口和控制端口须为不同的非零端口".into());
    }
    Ok(())
}

/// Build a fixed main configuration. Untrusted profile fields are not copied here.
pub fn managed_config(settings: &VpnSettings, secret: &str) -> Result<Value, String> {
    validate_settings(settings)?;
    if secret.is_empty() {
        return Err("控制器密钥不能为空".into());
    }
    Ok(json!({
        "mixed-port": settings.mixed_port,
        "bind-address": "127.0.0.1",
        "allow-lan": false,
        "external-controller": format!("127.0.0.1:{}", settings.controller_port),
        "secret": secret,
        "mode": "rule",
        "log-level": "silent",
        "ipv6": false,
        "tun": { "enable": false },
        "dns": { "enable": false },
        "geo-auto-update": false,
        "profile": { "store-selected": false, "store-fake-ip": false },
        "proxy-providers": {
            "imported": { "type": "file", "path": "./nodes.yaml", "health-check": { "enable": false } }
        },
        "proxy-groups": [{ "name": "VPN", "type": "select", "use": ["imported"], "empty-fallback": "REJECT" }],
        "rules": ["MATCH,VPN"]
    }))
}

pub fn subscription_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "订阅地址无效".to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("订阅只支持不含用户信息和片段的 HTTPS 地址".into());
    }
    Ok(url)
}

/// Public for contract tests. Always loopback; secrets never implement Debug/Serialize.
pub struct VpnController {
    client: Client,
    base: Url,
    secret: String,
}

impl VpnController {
    pub fn new(port: u16, secret: String) -> Result<Self, String> {
        if port == 0 || secret.is_empty() {
            return Err("控制器参数无效".into());
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| "无法初始化 VPN 控制器客户端".to_string())?;
        Ok(Self {
            client,
            base: Url::parse(&format!("http://127.0.0.1:{port}/")).expect("fixed loopback URL"),
            secret,
        })
    }

    async fn request(
        &self,
        method: Method,
        segments: &[&str],
        body: Option<Value>,
    ) -> Result<Value, String> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| "控制器路径无效".to_string())?
            .clear()
            .extend(segments.iter().copied());
        let mut request = self.client.request(method, url).bearer_auth(&self.secret);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "无法连接 VPN 控制器".to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "VPN 控制器请求失败（HTTP {}）",
                response.status().as_u16()
            ));
        }
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let bytes = limited_body(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| "VPN 控制器返回了无效数据".to_string())
    }

    pub async fn version(&self) -> Result<String, String> {
        let value = self.request(Method::GET, &["version"], None).await?;
        let version = value
            .get("version")
            .and_then(Value::as_str)
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 128
                    && !s.chars().any(char::is_control)
                    && !s.contains(&self.secret)
            })
            .ok_or_else(|| "VPN 控制器缺少版本信息".to_string())?;
        Ok(version.into())
    }

    pub async fn proxies(&self) -> Result<Vec<VpnProxy>, String> {
        let value = self.request(Method::GET, &["proxies"], None).await?;
        let entries = value
            .get("proxies")
            .and_then(Value::as_object)
            .ok_or_else(|| "VPN 控制器缺少代理列表".to_string())?;
        let mut proxies = Vec::new();
        for (name, entry) in entries {
            let kind = entry
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| "VPN 代理类型无效".to_string())?;
            let members = entry
                .get("all")
                .filter(|_| {
                    matches!(
                        kind,
                        "Selector" | "URLTest" | "Fallback" | "LoadBalance" | "Relay"
                    )
                })
                .and_then(Value::as_array)
                .map(|all| {
                    all.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            proxies.push(VpnProxy {
                name: name.clone(),
                kind: kind.into(),
                now: entry.get("now").and_then(Value::as_str).map(str::to_owned),
                members,
            });
        }
        proxies.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(proxies)
    }

    pub async fn select_proxy(&self, group: &str, name: &str) -> Result<(), String> {
        let groups = self.proxies().await?;
        let group_entry = groups
            .iter()
            .find(|proxy| proxy.name == group)
            .ok_or_else(|| "代理组不存在".to_string())?;
        if group_entry.kind != "Selector" {
            return Err("只有手动选择组支持切换节点".into());
        }
        if !group_entry.members.iter().any(|member| member == name) {
            return Err("节点不属于所选代理组".into());
        }
        self.request(
            Method::PUT,
            &["proxies", group],
            Some(json!({ "name": name })),
        )
        .await?;
        Ok(())
    }

    pub async fn verify_imported_nodes(&self) -> Result<(), String> {
        let provider = self
            .request(Method::GET, &["providers", "proxies", "imported"], None)
            .await?;
        let imported = provider
            .get("proxies")
            .and_then(Value::as_array)
            .ok_or_else(|| "节点文件无效：内核未加载导入的代理集合".to_string())?;
        let names: Vec<_> = imported
            .iter()
            .filter_map(|proxy| {
                let name = proxy.get("name")?.as_str()?;
                let kind = proxy.get("type")?.as_str()?.to_ascii_uppercase();
                if name.is_empty()
                    || matches!(name, "DIRECT" | "REJECT" | "COMPATIBLE" | "GLOBAL" | "VPN")
                    || matches!(
                        kind.as_str(),
                        "DIRECT" | "REJECT" | "REJECTDROP" | "COMPATIBLE" | "PASS" | "DNS"
                    )
                {
                    None
                } else {
                    Some(name)
                }
            })
            .collect();
        if names.is_empty() {
            return Err("节点文件无效：导入集合没有有效代理节点".into());
        }
        let proxies = self.proxies().await?;
        let group = proxies
            .iter()
            .find(|proxy| proxy.name == "VPN" && proxy.kind == "Selector")
            .ok_or_else(|| "节点文件无效：缺少 VPN 手动选择组".to_string())?;
        if !group
            .members
            .iter()
            .any(|member| names.contains(&member.as_str()))
        {
            return Err("节点文件无效：VPN 组没有加载导入节点".into());
        }
        Ok(())
    }

    pub async fn set_mode(&self, mode: &str) -> Result<(), String> {
        validate_mode(mode)?;
        // Mihomo's automatic GLOBAL selector otherwise defaults to DIRECT.
        if mode == "global" {
            self.select_proxy("GLOBAL", "VPN").await?;
        }
        self.request(Method::PATCH, &["configs"], Some(json!({ "mode": mode })))
            .await?;
        Ok(())
    }
}

fn validate_mode(mode: &str) -> Result<(), String> {
    if !matches!(mode, "rule" | "global" | "direct") {
        return Err("代理模式无效".into());
    }
    Ok(())
}

async fn limited_body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > PROFILE_LIMIT as u64)
    {
        return Err("VPN 数据超过 2 MiB 上限".into());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "VPN 数据读取失败".to_string())?;
        if bytes.len() + chunk.len() > PROFILE_LIMIT {
            return Err("VPN 数据超过 2 MiB 上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

type OwnedPid = Arc<parking_lot::Mutex<Option<u32>>>;

struct ManagedChild {
    child: Child,
    owned_pid: OwnedPid,
    pid: Option<u32>,
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            if let Some(pid) = self.child.id() {
                crate::proc_util::kill_tree_blocking(pid);
            }
            let _ = self.child.start_kill();
        }
        let mut owned = self.owned_pid.lock();
        if *owned == self.pid {
            *owned = None;
        }
    }
}

struct Runtime {
    settings: VpnSettings,
    installs: KernelInstallState,
    child: Option<ManagedChild>,
    controller: Option<VpnController>,
    version: Option<String>,
    mode: String,
    error: Option<String>,
}

struct Startup<'a> {
    manager: &'a VpnManager,
    state: &'a mut Runtime,
    committed: bool,
}

impl Drop for Startup<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.state.child = None;
            self.state.controller = None;
            self.state.version = None;
            self.state
                .error
                .get_or_insert_with(|| "VPN 启动未完成".into());
            self.manager.cleanup();
            if self.state.installs.pending {
                let _ = self.manager.recover_previous(self.state);
            }
        }
    }
}

pub struct VpnManager {
    root: PathBuf,
    bundled_archive: Option<PathBuf>,
    state: Mutex<Runtime>,
    owned_pid: OwnedPid,
    closing: AtomicBool,
}

impl VpnManager {
    pub fn new(root: PathBuf) -> Self {
        let (settings, installs, error) = match fs::read(root.join("settings.json")) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes).and_then(|value| {
                let settings = serde_json::from_value::<VpnSettings>(value.clone())?;
                let installs = value
                    .get("kernel_install")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default();
                Ok((settings, installs))
            }) {
                Ok((settings, installs)) if validate_settings(&settings).is_ok() => {
                    (settings, installs, None)
                }
                _ => (
                    VpnSettings::default(),
                    KernelInstallState::default(),
                    Some("VPN 设置文件无效，请重新保存设置".into()),
                ),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (VpnSettings::default(), KernelInstallState::default(), None)
            }
            Err(_) => (
                VpnSettings::default(),
                KernelInstallState::default(),
                Some("无法读取 VPN 设置".into()),
            ),
        };
        Self {
            root,
            bundled_archive: None,
            state: Mutex::new(Runtime {
                settings,
                installs,
                child: None,
                controller: None,
                version: None,
                mode: "rule".into(),
                error,
            }),
            owned_pid: Arc::new(parking_lot::Mutex::new(None)),
            closing: AtomicBool::new(false),
        }
    }

    pub fn with_bundled_archive(root: PathBuf, archive: PathBuf) -> Self {
        let mut manager = Self::new(root);
        manager.bundled_archive = Some(archive);
        manager
    }

    fn run_dir(&self) -> PathBuf {
        self.root.join("runtime")
    }

    fn cleanup(&self) {
        for name in ["nodes.yaml", "config.json"] {
            let _ = fs::remove_file(self.run_dir().join(name));
        }
    }

    fn refresh(&self, state: &mut Runtime) -> Result<(), String> {
        let exited = match state.child.as_mut() {
            Some(child) => child
                .child
                .try_wait()
                .map_err(|_| "无法读取 VPN 内核状态".to_string())?
                .is_some(),
            None => false,
        };
        if exited {
            state.child = None;
            state.controller = None;
            state.version = None;
            state.error = Some("VPN 内核已退出".into());
            self.cleanup();
        }
        Ok(())
    }

    fn snapshot(&self, state: &Runtime) -> VpnStatus {
        VpnStatus {
            settings: state.settings.clone(),
            running: state.child.is_some(),
            kernel_ready: Path::new(&state.settings.kernel_path).is_file(),
            profile_ready: self.root.join("profile.enc").is_file(),
            pid: state.child.as_ref().and_then(|child| child.child.id()),
            version: state.version.clone(),
            proxy_url: format!("http://127.0.0.1:{}", state.settings.mixed_port),
            mode: state.mode.clone(),
            error: state.error.clone(),
        }
    }

    pub async fn status(&self) -> Result<VpnStatus, String> {
        let mut state = self.state.lock().await;
        self.refresh(&mut state)?;
        Ok(self.snapshot(&state))
    }

    /// Does not reap Child, refresh state, write settings, or delete runtime files.
    pub fn diagnostics_snapshot(&self) -> Option<VpnReadonlyStatus> {
        let state = self.state.try_lock().ok()?;
        let running = if let Some(child) = &state.child {
            #[cfg(windows)]
            {
                use windows_sys::Win32::{
                    Foundation::STILL_ACTIVE, System::Threading::GetExitCodeProcess,
                };
                if let Some(handle) = child.child.raw_handle() {
                    let mut code = 0u32;
                    if unsafe { GetExitCodeProcess(handle, &mut code) } == 0 {
                        return None;
                    }
                    code == STILL_ACTIVE as u32
                } else {
                    false
                }
            }
            #[cfg(not(windows))]
            {
                let _ = child;
                return None;
            }
        } else {
            false
        };
        Some(VpnReadonlyStatus {
            running,
            mixed_port: state.settings.mixed_port,
            has_error: state.error.is_some(),
        })
    }

    fn persist_settings(
        &self,
        settings: &VpnSettings,
        installs: &KernelInstallState,
    ) -> Result<(), String> {
        let mut value =
            serde_json::to_value(settings).map_err(|_| "无法生成 VPN 设置".to_string())?;
        value["kernel_install"] =
            serde_json::to_value(installs).map_err(|_| "无法生成内核安装记录".to_string())?;
        let bytes =
            serde_json::to_vec_pretty(&value).map_err(|_| "无法生成 VPN 设置".to_string())?;
        vpn_install::atomic_write(&self.root.join("settings.json"), &bytes)
    }

    pub async fn kernel_info(&self) -> Result<VpnKernelInfo, String> {
        let state = self.state.lock().await;
        Ok(state.installs.info(&self.root, &state.settings.kernel_path))
    }

    pub async fn install_kernel(&self) -> Result<VpnStatus, String> {
        self.install_kernel_source(None).await
    }

    /// Offline acceptance uses the exact same pinned ZIP, executable and version checks.
    /// This method is not exposed through IPC and never changes the production URL.
    #[doc(hidden)]
    pub async fn install_kernel_from_verified_archive(
        &self,
        archive: &Path,
    ) -> Result<VpnStatus, String> {
        self.install_kernel_source(Some(archive)).await
    }

    async fn install_kernel_source(&self, archive: Option<&Path>) -> Result<VpnStatus, String> {
        let mut state = self.state.lock().await;
        self.refresh(&mut state)?;
        if state.child.is_some() {
            return Err("请先停止 VPN 内核再安装或升级".into());
        }
        self.ensure_open()?;
        if !vpn_install::supported() {
            return Err("内核安装仅支持 Windows amd64".into());
        }
        let archive = if let Some(archive) = archive {
            Some(archive)
        } else if let Some(bundled) = &self.bundled_archive {
            match fs::metadata(bundled) {
                Ok(metadata) if metadata.is_file() => Some(bundled.as_path()),
                Ok(_) => return Err("预置内核压缩包不是普通文件".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err("无法读取预置内核压缩包".into()),
            }
        } else {
            None
        };
        let existing = state
            .installs
            .current
            .as_ref()
            .filter(|kernel| kernel.is_target(&self.root))
            .cloned();
        let mut prepared = None;
        let kernel = if let Some(existing) = existing {
            existing
        } else {
            let cancellation = async {
                while !self.closing.load(Ordering::Acquire) {
                    sleep(Duration::from_millis(100)).await;
                }
            };
            let install = tokio::select! {
                result = vpn_install::prepare(&self.root, archive, &self.closing, self.owned_pid.clone()) => result?,
                _ = cancellation => return Err("VPN 管理器正在退出，内核安装已取消".into()),
            };
            let kernel = install.kernel.clone();
            prepared = Some(install);
            kernel
        };
        self.ensure_open()?;
        let path = kernel
            .path(&self.root)
            .ok_or_else(|| "受管内核路径无效".to_string())?
            .to_string_lossy()
            .into_owned();
        if vpn_install::same_kernel_path(Path::new(&state.settings.kernel_path), Path::new(&path)) {
            return Ok(self.snapshot(&state));
        }
        let mut installs = state.installs.clone();
        installs.previous = installs.fallback(&self.root, &state.settings.kernel_path);
        installs.current = Some(kernel);
        installs.pending = true;
        let mut settings = state.settings.clone();
        settings.kernel_path = path;
        self.persist_settings(&settings, &installs)?;
        if let Some(install) = prepared {
            install.commit();
        }
        state.settings = settings;
        state.installs = installs;
        state.error = None;
        Ok(self.snapshot(&state))
    }

    pub async fn rollback_kernel(&self) -> Result<VpnStatus, String> {
        let mut state = self.state.lock().await;
        self.refresh(&mut state)?;
        if state.child.is_some() {
            return Err("请先停止 VPN 内核再回滚".into());
        }
        self.ensure_open()?;
        if !state
            .installs
            .selected_current(&self.root, &state.settings.kernel_path)
        {
            return Err("当前选择的内核没有可回滚的安装记录".into());
        }
        let previous = state
            .installs
            .previous
            .as_ref()
            .filter(|previous| previous.available(&self.root))
            .ok_or_else(|| "没有可用的前版内核".to_string())?;
        let mut settings = state.settings.clone();
        settings.kernel_path = previous.path.clone();
        let mut installs = state.installs.clone();
        installs.pending = false;
        self.persist_settings(&settings, &installs)?;
        state.settings = settings;
        state.installs = installs;
        state.error = None;
        Ok(self.snapshot(&state))
    }

    fn recover_previous(&self, state: &mut Runtime) -> Result<bool, String> {
        if !state.installs.pending
            || !state
                .installs
                .selected_current(&self.root, &state.settings.kernel_path)
        {
            return Ok(false);
        }
        let mut settings = state.settings.clone();
        let restored = state
            .installs
            .previous
            .as_ref()
            .filter(|previous| previous.available(&self.root))
            .map(|previous| {
                settings.kernel_path = previous.path.clone();
            })
            .is_some();
        let mut installs = state.installs.clone();
        installs.pending = false;
        self.persist_settings(&settings, &installs)?;
        state.settings = settings;
        state.installs = installs;
        Ok(restored)
    }

    pub async fn save_settings(&self, settings: VpnSettings) -> Result<VpnStatus, String> {
        validate_settings(&settings)?;
        let mut state = self.state.lock().await;
        self.refresh(&mut state)?;
        if state.child.is_some() {
            return Err("请先停止 VPN 内核再修改设置".into());
        }
        self.ensure_open()?;
        let mut installs = state.installs.clone();
        if !vpn_install::same_kernel_path(
            Path::new(&settings.kernel_path),
            Path::new(&state.settings.kernel_path),
        ) {
            installs.pending = false;
        }
        self.persist_settings(&settings, &installs)?;
        state.settings = settings;
        state.installs = installs;
        state.error = None;
        Ok(self.snapshot(&state))
    }

    pub async fn import_profile(&self, input: VpnProfileInput) -> Result<VpnStatus, String> {
        let mut state = self.state.lock().await;
        self.refresh(&mut state)?;
        if state.child.is_some() {
            return Err("请先停止 VPN 内核再导入节点".into());
        }
        self.ensure_open()?;
        let bytes = match (input.path, input.url) {
            (Some(path), None) if !path.trim().is_empty() => {
                use std::io::Read;
                let file = fs::File::open(path).map_err(|_| "无法读取节点文件".to_string())?;
                let mut bytes = Vec::new();
                file.take((PROFILE_LIMIT + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "无法读取节点文件".to_string())?;
                bytes
            }
            (None, Some(raw)) => {
                let url = subscription_url(&raw)?;
                let client = Client::builder()
                    .no_proxy()
                    .redirect(Policy::none())
                    .timeout(START_TIMEOUT)
                    .build()
                    .map_err(|_| "无法初始化订阅客户端".to_string())?;
                let response = client
                    .get(url)
                    .send()
                    .await
                    .map_err(|_| "订阅获取失败，请检查地址和网络".to_string())?;
                if !response.status().is_success() {
                    return Err("订阅服务器拒绝请求或发生重定向".into());
                }
                limited_body(response).await?
            }
            _ => return Err("请选择一个本地节点文件或 HTTPS 订阅地址".into()),
        };
        if bytes.len() > PROFILE_LIMIT {
            return Err("节点文件超过 2 MiB 上限".into());
        }
        let text =
            String::from_utf8(bytes).map_err(|_| "节点文件必须为 UTF-8 Clash YAML".to_string())?;
        if text.trim().is_empty() {
            return Err("节点文件不能为空".into());
        }
        let encrypted =
            crate::crypto::encrypt(&text).map_err(|_| "节点文件加密失败".to_string())?;
        self.ensure_open()?;
        write_private(&self.root.join("profile.enc"), encrypted.as_bytes())?;
        state.error = None;
        Ok(self.snapshot(&state))
    }

    fn ensure_open(&self) -> Result<(), String> {
        if self.closing.load(Ordering::Acquire) {
            return Err("VPN 管理器正在退出".into());
        }
        Ok(())
    }

    fn spawn(&self, kernel: &Path, validate: bool) -> Result<ManagedChild, String> {
        let mut owned_pid = self.owned_pid.lock();
        self.ensure_open()?;
        let mut command = Command::new(kernel);
        command
            .arg("-d")
            .arg(self.run_dir())
            .arg("-f")
            .arg(self.run_dir().join("config.json"));
        if validate {
            command.arg("-t");
        }
        command
            .current_dir(self.run_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // Mihomo supports environment overrides, including post-up/down scripts.
        for (name, _) in std::env::vars_os() {
            let uppercase = name.to_string_lossy().to_ascii_uppercase();
            if uppercase.starts_with("CLASH_")
                || uppercase == "SAFE_PATHS"
                || uppercase == "LLMGW_MASTER_KEY"
            {
                command.env_remove(name);
            }
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let child = command
            .spawn()
            .map_err(|_| "无法启动 VPN 内核，请检查可执行文件".to_string())?;
        let pid = child.id();
        *owned_pid = pid;
        Ok(ManagedChild {
            child,
            owned_pid: self.owned_pid.clone(),
            pid,
        })
    }

    pub async fn start(&self) -> Result<VpnStatus, String> {
        let mut state = self.state.lock().await;
        self.refresh(&mut state)?;
        self.ensure_open()?;
        if state.child.is_some() {
            return Ok(self.snapshot(&state));
        }
        // Cancellation must also kill the owned child and remove temporary plaintext.
        let mut startup = Startup {
            manager: self,
            state: &mut state,
            committed: false,
        };
        let mut result = self.start_locked(startup.state).await;
        if let Err(error) = &result {
            if let Some(mut child) = startup.state.child.take() {
                crate::proc_util::kill_tree(&mut child.child).await;
                let _ = child.child.wait().await;
            }
            startup.state.controller = None;
            startup.state.version = None;
            startup.state.error = Some(error.clone());
            self.cleanup();
            if startup.state.installs.pending {
                let original = error.clone();
                result = Err(match self.recover_previous(startup.state) {
                    Ok(true) => {
                        format!("新内核启动失败，已恢复前版内核选择；VPN 保持停止：{original}")
                    }
                    Ok(false) => format!("新内核启动失败，没有可用前版可恢复：{original}"),
                    Err(_) => format!("新内核启动失败，前版选择恢复未能保存：{original}"),
                });
                startup.state.error = result.as_ref().err().cloned();
            }
        }
        if result.is_ok() {
            if startup.state.installs.pending {
                let mut installs = startup.state.installs.clone();
                installs.pending = false;
                self.persist_settings(&startup.state.settings, &installs)?;
                startup.state.installs = installs;
            }
            startup.committed = true;
        }
        result.map(|()| self.snapshot(startup.state))
    }

    async fn start_locked(&self, state: &mut Runtime) -> Result<(), String> {
        let deadline = Instant::now() + START_TIMEOUT;
        validate_settings(&state.settings)?;
        let kernel = fs::canonicalize(&state.settings.kernel_path)
            .map_err(|_| "请先选择本地 Mihomo 内核可执行文件".to_string())?;
        if !kernel.is_file() {
            return Err("VPN 内核路径不是文件".into());
        }
        use std::io::Read;
        let file = fs::File::open(self.root.join("profile.enc"))
            .map_err(|_| "请先导入 Clash YAML 节点文件".to_string())?;
        let mut encrypted = String::new();
        file.take((3 * PROFILE_LIMIT / 2 + 1) as u64)
            .read_to_string(&mut encrypted)
            .map_err(|_| "节点加密文件无效，请重新导入".to_string())?;
        if encrypted.len() > 3 * PROFILE_LIMIT / 2 {
            return Err("节点加密文件超过上限，请重新导入".into());
        }
        let profile = crate::crypto::decrypt(&encrypted)
            .map_err(|_| "节点文件解密失败，请重新导入".to_string())?;
        if profile.is_empty() || profile.len() > PROFILE_LIMIT {
            return Err("节点文件大小无效".into());
        }
        // Bind both ports before probing any HTTP endpoint: never attach to another controller.
        let mixed_guard = TcpListener::bind(("127.0.0.1", state.settings.mixed_port))
            .map_err(|_| "代理端口已被占用，请修改端口或停止占用程序".to_string())?;
        let control_guard = TcpListener::bind(("127.0.0.1", state.settings.controller_port))
            .map_err(|_| "控制端口已被占用，请修改端口或停止占用程序".to_string())?;
        let secret = crate::crypto::new_unified_key();
        let config = serde_json::to_vec_pretty(&managed_config(&state.settings, &secret)?)
            .map_err(|_| "无法生成 VPN 配置".to_string())?;
        write_private(&self.run_dir().join("nodes.yaml"), profile.as_bytes())?;
        write_private(&self.run_dir().join("config.json"), &config)?;
        let mut validator = self.spawn(&kernel, true)?;
        match tokio::time::timeout_at(deadline, validator.child.wait()).await {
            Ok(Ok(status)) if status.success() => {}
            Ok(_) => return Err("节点配置校验失败，请检查 Clash YAML 的 proxies 节点".into()),
            Err(_) => {
                crate::proc_util::kill_tree(&mut validator.child).await;
                let _ = validator.child.wait().await;
                return Err("VPN 配置校验超时".into());
            }
        }
        drop(validator);
        drop(mixed_guard);
        drop(control_guard);
        self.ensure_open()?;
        state.child = Some(self.spawn(&kernel, false)?);
        let controller = VpnController::new(state.settings.controller_port, secret)?;
        let mut node_error = None;
        let mut readiness_error = None;
        loop {
            self.ensure_open()?;
            if state
                .child
                .as_mut()
                .expect("owned child")
                .child
                .try_wait()
                .map_err(|_| "无法读取 VPN 内核状态".to_string())?
                .is_some()
            {
                return Err("VPN 内核在启动时退出".into());
            }
            let readiness = async {
                let version = controller
                    .version()
                    .await
                    .map_err(|error| format!("控制器版本查询失败：{error}"))?;
                tokio::net::TcpStream::connect(("127.0.0.1", state.settings.mixed_port))
                    .await
                    .map_err(|_| "VPN 代理端口尚未就绪".to_string())?;
                let nodes = controller.verify_imported_nodes().await;
                Ok::<_, String>((version, nodes))
            };
            let readiness_result = tokio::time::timeout_at(deadline, readiness).await;
            if let Ok(Err(error)) = &readiness_result {
                readiness_error = Some(error.clone());
            }
            if let Ok(Ok((version, nodes))) = readiness_result {
                if let Err(error) = nodes {
                    node_error = Some(error);
                    if Instant::now() >= deadline {
                        return Err(node_error.expect("recorded node error"));
                    }
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }
                // Authentication alone is insufficient if our child failed while another bound the port.
                if state
                    .child
                    .as_mut()
                    .expect("owned child")
                    .child
                    .try_wait()
                    .map_err(|_| "无法读取 VPN 内核状态".to_string())?
                    .is_some()
                {
                    return Err("VPN 内核在启动时退出".into());
                }
                state.controller = Some(controller);
                state.version = Some(version);
                state.mode = "rule".into();
                state.error = None;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(node_error.unwrap_or_else(|| match readiness_error {
                    Some(error) => format!("VPN 控制器启动超时：{error}"),
                    None => "VPN 控制器启动超时".into(),
                }));
            }
            sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn stop(&self) -> Result<VpnStatus, String> {
        let mut state = self.state.lock().await;
        if let Some(mut child) = state.child.take() {
            crate::proc_util::kill_tree(&mut child.child).await;
            let _ = child.child.wait().await;
        }
        state.controller = None;
        state.version = None;
        state.error = None;
        self.cleanup();
        Ok(self.snapshot(&state))
    }

    pub async fn proxies(&self) -> Result<Vec<VpnProxy>, String> {
        let mut state = self.state.lock().await;
        self.refresh(&mut state)?;
        state
            .controller
            .as_ref()
            .ok_or_else(|| "请先启动 VPN 内核".to_string())?
            .proxies()
            .await
    }

    pub async fn select_proxy(&self, group: &str, name: &str) -> Result<(), String> {
        let mut state = self.state.lock().await;
        self.refresh(&mut state)?;
        state
            .controller
            .as_ref()
            .ok_or_else(|| "请先启动 VPN 内核".to_string())?
            .select_proxy(group, name)
            .await
    }

    pub async fn set_mode(&self, mode: &str) -> Result<(), String> {
        validate_mode(mode)?;
        let mut state = self.state.lock().await;
        self.refresh(&mut state)?;
        state
            .controller
            .as_ref()
            .ok_or_else(|| "请先启动 VPN 内核".to_string())?
            .set_mode(mode)
            .await?;
        state.mode = mode.into();
        Ok(())
    }

    pub fn shutdown_blocking(&self) {
        self.closing.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.try_lock() {
            state.child = None; // ManagedChild kills only the owned live child/tree.
            state.controller = None;
            state.version = None;
        } else if let Some(pid) = *self.owned_pid.lock() {
            crate::proc_util::kill_tree_blocking(pid);
        }
        self.cleanup();
    }
}

impl Drop for VpnManager {
    fn drop(&mut self) {
        self.shutdown_blocking();
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| "VPN 数据路径无效".to_string())?,
    )
    .map_err(|_| "无法创建 VPN 数据目录".to_string())?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "无法写入 VPN 数据文件".to_string())?;
    file.write_all(bytes)
        .map_err(|_| "无法写入 VPN 数据文件".to_string())?;
    file.sync_all()
        .map_err(|_| "无法保存 VPN 数据文件".to_string())
}
