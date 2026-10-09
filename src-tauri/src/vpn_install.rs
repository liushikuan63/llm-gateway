//! Pinned Mihomo installation. The IPC caller cannot supply a URL, digest or executable.
//! Packages are verified in a private sibling staging directory before publication.

use futures_util::StreamExt;
use reqwest::{redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
};

pub const VERSION: &str = "v1.19.32";
pub const ASSET_NAME: &str = "mihomo-windows-amd64-compatible-v1.19.32.zip";
pub const SOURCE_URL: &str = "https://github.com/MetaCubeX/mihomo/releases/tag/v1.19.32";
pub const LICENSE_URL: &str = "https://github.com/MetaCubeX/mihomo/blob/v1.19.32/LICENSE";
pub const ASSET_URL: &str = "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.32/mihomo-windows-amd64-compatible-v1.19.32.zip";
pub const ZIP_SHA256: &str = "974a4d7ad69aed27aa2e8f91d61113573c14dadb14562c63e58effabf59816f0";
pub const EXE_SHA256: &str = "04f8d7fc2b314771e1ecbf15951d59f0e1cb7914503c88cfb6972ff6a824e314";
pub const ENTRY_NAME: &str = "mihomo-windows-amd64-compatible.exe";
pub const ZIP_LIMIT: u64 = 32 * 1024 * 1024;
pub const EXE_LIMIT: u64 = 96 * 1024 * 1024;
type OwnedPid = Arc<parking_lot::Mutex<Option<u32>>>;

pub fn same_kernel_path(first: &Path, second: &Path) -> bool {
    let first = fs::canonicalize(first).unwrap_or_else(|_| first.to_path_buf());
    let second = fs::canonicalize(second).unwrap_or_else(|_| second.to_path_buf());
    #[cfg(windows)]
    {
        first
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&second.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        first == second
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct VpnKernelInfo {
    pub supported: bool,
    pub version: String,
    pub installed: bool,
    pub managed: bool,
    pub can_rollback: bool,
    pub license_url: String,
    pub source_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledKernel {
    pub directory: String,
    pub version: String,
    pub sha256: String,
}

impl InstalledKernel {
    pub fn path(&self, root: &Path) -> Option<PathBuf> {
        if self.directory.is_empty()
            || self.directory.len() > 128
            || !self
                .directory
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
            || self.directory == "."
            || self.directory == ".."
        {
            return None;
        }
        Some(
            root.join("kernels")
                .join(&self.directory)
                .join("mihomo.exe"),
        )
    }

    pub fn verified(&self, root: &Path) -> bool {
        self.path(root)
            .is_some_and(|path| verify_hash(&path, &self.sha256, EXE_LIMIT).is_ok())
    }

    pub fn is_target(&self, root: &Path) -> bool {
        self.version == VERSION && self.sha256 == EXE_SHA256 && self.verified(root)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelFallback {
    pub path: String,
    #[serde(default)]
    pub managed: Option<InstalledKernel>,
}

impl KernelFallback {
    pub fn available(&self, root: &Path) -> bool {
        if let Some(managed) = &self.managed {
            managed
                .path(root)
                .is_some_and(|path| same_kernel_path(&path, Path::new(&self.path)))
                && managed.verified(root)
        } else {
            !self.path.is_empty() && Path::new(&self.path).is_file()
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KernelInstallState {
    pub current: Option<InstalledKernel>,
    pub previous: Option<KernelFallback>,
    pub pending: bool,
}

impl KernelInstallState {
    pub fn selected_current(&self, root: &Path, selected: &str) -> bool {
        self.current
            .as_ref()
            .and_then(|kernel| kernel.path(root))
            .is_some_and(|path| same_kernel_path(&path, Path::new(selected)))
    }

    pub fn selected_managed(&self, root: &Path, selected: &str) -> bool {
        self.selected_current(root, selected)
            || self
                .previous
                .as_ref()
                .and_then(|previous| previous.managed.as_ref())
                .and_then(|kernel| kernel.path(root))
                .is_some_and(|path| same_kernel_path(&path, Path::new(selected)))
    }

    pub fn fallback(&self, root: &Path, selected: &str) -> Option<KernelFallback> {
        if selected.is_empty() || !Path::new(selected).is_file() {
            return None;
        }
        let managed = self
            .current
            .as_ref()
            .filter(|kernel| {
                kernel
                    .path(root)
                    .is_some_and(|path| same_kernel_path(&path, Path::new(selected)))
            })
            .cloned()
            .or_else(|| {
                self.previous
                    .as_ref()
                    .filter(|previous| {
                        same_kernel_path(Path::new(&previous.path), Path::new(selected))
                    })
                    .and_then(|previous| previous.managed.clone())
            });
        Some(KernelFallback {
            path: selected.into(),
            managed,
        })
    }

    pub fn info(&self, root: &Path, selected: &str) -> VpnKernelInfo {
        VpnKernelInfo {
            supported: supported(),
            version: VERSION.into(),
            installed: self
                .current
                .as_ref()
                .is_some_and(|kernel| kernel.is_target(root)),
            managed: self.selected_managed(root, selected),
            can_rollback: self.selected_current(root, selected)
                && self
                    .previous
                    .as_ref()
                    .is_some_and(|previous| previous.available(root)),
            license_url: LICENSE_URL.into(),
            source_url: SOURCE_URL.into(),
        }
    }
}

pub fn supported() -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
}

pub fn official_asset_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.fragment().is_none()
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

pub fn verify_hash(path: &Path, expected: &str, maximum: u64) -> Result<(), String> {
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("内核校验摘要无效".into());
    }
    let mut file = fs::File::open(path).map_err(|_| "无法读取待校验内核文件".to_string())?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let size = file
            .read(&mut chunk)
            .map_err(|_| "内核文件读取失败".to_string())?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if total > maximum {
            return Err("内核文件超过大小上限".into());
        }
        hasher.update(&chunk[..size]);
    }
    if total == 0 || format!("{:x}", hasher.finalize()) != expected.to_ascii_lowercase() {
        return Err("内核文件 SHA256 校验失败".into());
    }
    Ok(())
}

pub fn verify_windows_amd64(path: &Path) -> Result<(), String> {
    let file = fs::File::open(path).map_err(|_| "无法读取内核架构".to_string())?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取内核架构".to_string())?;
    let valid = (|| {
        if bytes.get(..2)? != b"MZ" {
            return None;
        }
        let offset = u32::from_le_bytes(bytes.get(0x3c..0x40)?.try_into().ok()?) as usize;
        let header = bytes.get(offset..offset.checked_add(26)?)?;
        Some(
            &header[..4] == b"PE\0\0"
                && header[4..6] == [0x64, 0x86]
                && header[24..26] == [0x0b, 0x02],
        )
    })();
    if valid != Some(true) {
        return Err("内核不是 Windows amd64 可执行文件".into());
    }
    Ok(())
}

/// Separate transfer primitive for offline HTTP fixtures; production supplies the fixed official URL/policy.
pub async fn download_to_file(
    source: &str,
    destination: &Path,
    allowed: fn(&Url) -> bool,
) -> Result<(), String> {
    let mut url = Url::parse(source).map_err(|_| "内核下载地址无效".to_string())?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|_| "无法初始化内核下载客户端".to_string())?;
    for _ in 0..=4 {
        if !allowed(&url) {
            return Err("内核下载目标不在允许范围内".into());
        }
        let response = client
            .get(url.clone())
            .header("User-Agent", "llm-gateway-kernel-installer")
            .send()
            .await
            .map_err(|_| "内核下载失败，请检查网络后重试".to_string())?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "内核下载重定向无效".to_string())?;
            url = url
                .join(location)
                .map_err(|_| "内核下载重定向无效".to_string())?;
            continue;
        }
        if !response.status().is_success() {
            return Err("官方内核下载服务器拒绝请求".into());
        }
        if response
            .content_length()
            .is_some_and(|size| size > ZIP_LIMIT)
        {
            return Err("内核压缩包超过大小上限".into());
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)
            .await
            .map_err(|_| "无法创建内核下载文件".to_string())?;
        let mut downloaded = DownloadFile {
            path: destination.to_path_buf(),
            keep: false,
        };
        use tokio::io::AsyncWriteExt;
        let mut total = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "内核下载中断".to_string())?;
            total += chunk.len() as u64;
            if total > ZIP_LIMIT {
                return Err("内核压缩包超过大小上限".into());
            }
            file.write_all(&chunk)
                .await
                .map_err(|_| "无法保存内核下载文件".to_string())?;
        }
        if total == 0 {
            return Err("内核下载内容为空".into());
        }
        file.sync_all()
            .await
            .map_err(|_| "无法保存内核下载文件".to_string())?;
        downloaded.keep = true;
        return Ok(());
    }
    Err("内核下载重定向次数过多".into())
}

struct DownloadFile {
    path: PathBuf,
    keep: bool,
}
impl Drop for DownloadFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct StageDirectory {
    path: PathBuf,
    keep: bool,
}
impl Drop for StageDirectory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub struct PreparedKernel {
    pub kernel: InstalledKernel,
    stage: StageDirectory,
}
impl PreparedKernel {
    pub fn commit(mut self) -> InstalledKernel {
        self.stage.keep = true;
        self.kernel.clone()
    }
}

struct InstallerChild {
    child: Child,
    registry: OwnedPid,
    pid: Option<u32>,
}
impl Drop for InstallerChild {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            if let Some(pid) = self.child.id() {
                crate::proc_util::kill_tree_blocking(pid);
            }
            let _ = self.child.start_kill();
        }
        let mut registered = self.registry.lock();
        if *registered == self.pid {
            *registered = None;
        }
    }
}

fn spawn(
    mut command: Command,
    closing: &AtomicBool,
    registry: OwnedPid,
) -> Result<InstallerChild, String> {
    let mut registered = registry.lock();
    if closing.load(Ordering::Acquire) {
        return Err("VPN 管理器正在退出".into());
    }
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for (name, _) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("CLASH_") || upper == "SAFE_PATHS" || upper == "LLMGW_MASTER_KEY" {
            command.env_remove(name);
        }
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let child = command
        .spawn()
        .map_err(|_| "无法运行内核安装校验".to_string())?;
    let pid = child.id();
    *registered = pid;
    drop(registered);
    Ok(InstallerChild {
        child,
        registry,
        pid,
    })
}

#[cfg(windows)]
const EXTRACT_SCRIPT: &str = r#"param([string]$ArchivePath, [string]$OutputPath, [string]$ExpectedEntry, [long]$MaxBytes)
$ErrorActionPreference = 'Stop'
try {
  Add-Type -AssemblyName System.IO.Compression.FileSystem
  $archive = [IO.Compression.ZipFile]::OpenRead($ArchivePath)
  try {
    if ($archive.Entries.Count -ne 1) { throw 'invalid entry count' }
    $entry = $archive.Entries[0]
    if (-not [String]::Equals($entry.FullName, $ExpectedEntry, [StringComparison]::Ordinal)) { throw 'unsafe entry path' }
    if ($entry.Length -le 0 -or $entry.Length -gt $MaxBytes) { throw 'invalid entry size' }
    if (($entry.ExternalAttributes -band 1024) -ne 0 -or (($entry.ExternalAttributes -shr 16) -band 61440) -eq 40960) { throw 'link entry' }
    $inputStream = $entry.Open()
    $outputStream = [IO.File]::Open($OutputPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
    try { $inputStream.CopyTo($outputStream); $outputStream.Flush($true) }
    finally { $outputStream.Dispose(); $inputStream.Dispose() }
  } finally { $archive.Dispose() }
  exit 0
} catch { exit 1 }
"#;

/// Fixed, ASCII script; file paths are separate process arguments, never interpolated code.
pub async fn extract_verified_zip(
    archive: &Path,
    output: &Path,
    script: &Path,
    closing: &AtomicBool,
    registry: OwnedPid,
) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = (archive, output, script, closing, registry);
        return Err("当前平台不支持内核安装".into());
    }
    #[cfg(windows)]
    {
        fs::write(script, EXTRACT_SCRIPT).map_err(|_| "无法创建内核解压校验脚本".to_string())?;
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        let mut command =
            Command::new(system_root.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script)
            .arg("-ArchivePath")
            .arg(archive)
            .arg("-OutputPath")
            .arg(output)
            .arg("-ExpectedEntry")
            .arg(ENTRY_NAME)
            .arg("-MaxBytes")
            .arg(EXE_LIMIT.to_string())
            .stdout(Stdio::null());
        let mut child = spawn(command, closing, registry)?;
        let result = tokio::time::timeout(Duration::from_secs(30), child.child.wait()).await;
        if !matches!(result, Ok(Ok(status)) if status.success()) {
            return Err("内核压缩包结构无效或解压失败".into());
        }
        Ok(())
    }
}

async fn verify_version(
    binary: &Path,
    closing: &AtomicBool,
    registry: OwnedPid,
) -> Result<(), String> {
    let mut command = Command::new(binary);
    command.arg("-v").stdout(Stdio::piped());
    let mut child = spawn(command, closing, registry)?;
    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| "无法读取内核版本".to_string())?;
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        let read = async move {
            let mut bytes = Vec::new();
            stdout
                .take(4097)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        };
        tokio::join!(read, child.child.wait())
    })
    .await;
    match result {
        Ok((Ok(bytes), Ok(status))) if status.success() && bytes.len() <= 4096 => {
            let text = String::from_utf8(bytes).map_err(|_| "内核版本响应无效".to_string())?;
            if text.split_whitespace().take(8).any(|word| word == VERSION) {
                Ok(())
            } else {
                Err("内核版本与固定发行版本不符".into())
            }
        }
        _ => Err("内核版本校验失败或超时".into()),
    }
}

pub async fn prepare(
    root: &Path,
    archive: Option<&Path>,
    closing: &AtomicBool,
    registry: OwnedPid,
) -> Result<PreparedKernel, String> {
    if !supported() {
        return Err("内核安装仅支持 Windows amd64".into());
    }
    let install_root = root.join("kernels");
    fs::create_dir_all(&install_root).map_err(|_| "无法创建内核安装目录".to_string())?;
    let mut stage = StageDirectory {
        path: install_root.join(format!("staging-{}", uuid::Uuid::new_v4())),
        keep: false,
    };
    fs::create_dir(&stage.path).map_err(|_| "无法创建内核暂存目录".to_string())?;
    let zip = stage.path.join("asset.zip");
    if let Some(archive) = archive {
        verify_hash(archive, ZIP_SHA256, ZIP_LIMIT)?;
        fs::copy(archive, &zip).map_err(|_| "无法读取内核压缩包".to_string())?;
    } else {
        tokio::time::timeout(
            Duration::from_secs(120),
            download_to_file(ASSET_URL, &zip, official_asset_url),
        )
        .await
        .map_err(|_| "内核下载超时".to_string())??;
    }
    verify_hash(&zip, ZIP_SHA256, ZIP_LIMIT)?;
    let binary = stage.path.join("mihomo.exe");
    extract_verified_zip(
        &zip,
        &binary,
        &stage.path.join("extract.ps1"),
        closing,
        registry.clone(),
    )
    .await?;
    verify_hash(&binary, EXE_SHA256, EXE_LIMIT)?;
    verify_windows_amd64(&binary)?;
    verify_version(&binary, closing, registry).await?;
    if closing.load(Ordering::Acquire) {
        return Err("VPN 管理器正在退出".into());
    }
    fs::remove_file(&zip).map_err(|_| "无法清理内核下载暂存文件".to_string())?;
    fs::remove_file(stage.path.join("extract.ps1"))
        .map_err(|_| "无法清理内核安装暂存脚本".to_string())?;
    let kernel = InstalledKernel {
        directory: format!("{}-{}", VERSION, uuid::Uuid::new_v4()),
        version: VERSION.into(),
        sha256: EXE_SHA256.into(),
    };
    let published = kernel
        .path(root)
        .expect("generated safe directory")
        .parent()
        .expect("binary parent")
        .to_path_buf();
    fs::rename(&stage.path, &published).map_err(|_| "无法发布已校验内核".to_string())?;
    stage.path = published;
    Ok(PreparedKernel { kernel, stage })
}

/// Replace a small settings/journal document atomically on the same filesystem.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "VPN 设置路径无效".to_string())?;
    fs::create_dir_all(parent).map_err(|_| "无法创建 VPN 设置目录".to_string())?;
    let temporary = parent.join(format!(".settings-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| "无法写入 VPN 设置".to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "无法保存 VPN 设置".to_string())?;
        drop(file);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{
                MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
            };
            let from: Vec<_> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err("无法原子发布 VPN 设置".into());
            }
        }
        #[cfg(not(windows))]
        fs::rename(&temporary, path).map_err(|_| "无法原子发布 VPN 设置".to_string())?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}
