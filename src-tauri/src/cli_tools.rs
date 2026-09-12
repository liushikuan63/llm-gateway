//! 本机 CLI 工具检测与安装：是否存在、装在哪、什么版本、能不能升级。
//!
//! 只做四件事，且都不修改用户环境以外的任何数据：
//!  1) 在 PATH 与常见安装目录里定位可执行文件（Windows 需要处理 .cmd/.exe 等）；
//!  2) 运行 `<tool> --version` 读取版本（带超时，避免卡住界面）；
//!  3) 查询 npm registry 的 latest 版本，供界面提示「可更新」；
//!  4) 在界面显式确认后执行安装 / 更新（命令全部来自内置常量，不接受用户输入）。
//!
//! 安装来源分两类：
//!  - [`InstallSource::Npm`]：npm 全局安装，可查询 latest 版本并提示更新；
//!  - [`InstallSource::PowerShellScript`]：官方安装脚本（仅 Windows 可用）。
//!    脚本本身始终安装最新版，因此这类工具不做版本比对，界面只提供「安装 / 重新安装」。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

const VERSION_TIMEOUT: Duration = Duration::from_secs(15);
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(10);
const UPDATE_TIMEOUT: Duration = Duration::from_secs(600);
/// 安装脚本要从网络下载运行时，给它比 npm 命令更宽的窗口。
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(900);
const MAX_OUTPUT_CHARS: usize = 4_000;

/// 一个工具的安装来源。两类都来自内置常量，界面原样展示后才执行。
pub enum InstallSource {
    /// npm 全局安装；`package` 同时用于查询 latest 版本。
    Npm { package: &'static str },
    /// 官方安装脚本的 PowerShell 命令原文（仅 Windows 可用）。
    PowerShellScript { command: &'static str },
}

impl InstallSource {
    pub fn kind(&self) -> &'static str {
        match self {
            InstallSource::Npm { .. } => "npm",
            InstallSource::PowerShellScript { .. } => "script",
        }
    }

    /// 界面展示与执行使用的确切命令。
    pub fn command(&self) -> String {
        match self {
            InstallSource::Npm { package } => format!("npm install -g {package}@latest"),
            InstallSource::PowerShellScript { command } => (*command).to_owned(),
        }
    }

    /// 供界面在未安装（没有路径可显示）时展示的安装目标。
    pub fn target(&self) -> &'static str {
        match self {
            InstallSource::Npm { package } => package,
            InstallSource::PowerShellScript { .. } => "官方安装脚本",
        }
    }

    pub fn package(&self) -> Option<&'static str> {
        match self {
            InstallSource::Npm { package } => Some(package),
            InstallSource::PowerShellScript { .. } => None,
        }
    }
}

/// 一个受支持的 CLI 工具。
pub struct ToolSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub source: InstallSource,
    /// 可能出现在 PATH 中的可执行文件名（不含扩展名）
    pub binaries: &'static [&'static str],
    /// 官方安装说明地址，供用户核对来源。
    pub docs_url: &'static str,
}

pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        id: "claude_code",
        label: "Claude Code",
        source: InstallSource::Npm {
            package: "@anthropic-ai/claude-code",
        },
        binaries: &["claude"],
        docs_url: "https://docs.claude.com/en/docs/claude-code/setup",
    },
    ToolSpec {
        id: "codex",
        label: "Codex CLI",
        source: InstallSource::Npm {
            package: "@openai/codex",
        },
        binaries: &["codex"],
        docs_url: "https://github.com/openai/codex",
    },
    ToolSpec {
        id: "gemini_cli",
        label: "Gemini CLI",
        source: InstallSource::Npm {
            package: "@google/gemini-cli",
        },
        binaries: &["gemini"],
        docs_url: "https://github.com/google-gemini/gemini-cli",
    },
    ToolSpec {
        id: "qoder",
        label: "Qoder CLI",
        source: InstallSource::Npm {
            package: "@qoder-ai/qodercli",
        },
        binaries: &["qoder", "qodercli"],
        docs_url: "https://docs.qoder.com/zh/cli/installation",
    },
    ToolSpec {
        id: "opencode",
        label: "OpenCode",
        source: InstallSource::Npm {
            package: "opencode-ai",
        },
        binaries: &["opencode"],
        docs_url: "https://opencode.ai/docs/",
    },
    ToolSpec {
        id: "openclaw",
        label: "OpenClaw",
        source: InstallSource::Npm {
            package: "openclaw",
        },
        binaries: &["openclaw"],
        docs_url: "https://docs.openclaw.ai/",
    },
    ToolSpec {
        id: "pi",
        label: "Pi Coding Agent",
        source: InstallSource::Npm {
            package: "@earendil-works/pi-coding-agent",
        },
        binaries: &["pi"],
        docs_url: "https://pi.dev",
    },
    ToolSpec {
        id: "deepseek_harness",
        label: "DeepSeek Harness",
        source: InstallSource::Npm {
            package: "@deepseek-ai/dsh",
        },
        binaries: &["dsh"],
        docs_url: "https://github.com/deepseek-ai/deepseek-harness",
    },
    ToolSpec {
        id: "workbuddy",
        label: "WorkBuddy",
        source: InstallSource::Npm {
            package: "@tencent-ai/codebuddy-code",
        },
        binaries: &["codebuddy", "cbc"],
        docs_url: "https://www.workbuddy.ai/",
    },
    ToolSpec {
        id: "qoder_cn",
        label: "Qoder CLI 国内版",
        source: InstallSource::Npm {
            package: "@qodercn-ai/qoderclicn",
        },
        binaries: &["qodercn", "qoderclicn"],
        docs_url: "https://qoder.com/cli",
    },
    ToolSpec {
        id: "cline",
        label: "Cline",
        source: InstallSource::Npm { package: "cline" },
        binaries: &["cline"],
        docs_url: "https://cline.bot",
    },
    ToolSpec {
        id: "amp",
        label: "Amp",
        source: InstallSource::Npm {
            package: "@ampcode/cli",
        },
        binaries: &["amp"],
        docs_url: "https://ampcode.com/",
    },
    ToolSpec {
        id: "auggie",
        label: "Auggie",
        source: InstallSource::Npm {
            package: "@augmentcode/auggie",
        },
        binaries: &["auggie"],
        docs_url: "https://augmentcode.com",
    },
    ToolSpec {
        id: "continue_cli",
        label: "Continue CLI",
        source: InstallSource::Npm {
            package: "@continuedev/cli",
        },
        binaries: &["cn"],
        docs_url: "https://continue.dev",
    },
    ToolSpec {
        id: "crush",
        label: "Crush",
        source: InstallSource::Npm {
            package: "@charmland/crush",
        },
        binaries: &["crush"],
        docs_url: "https://charm.sh/crush",
    },
    ToolSpec {
        id: "droid",
        label: "Factory Droid",
        source: InstallSource::Npm { package: "droid" },
        binaries: &["droid"],
        docs_url: "https://github.com/Factory-AI/factory",
    },
    ToolSpec {
        id: "iflow_cli",
        label: "iFlow CLI",
        source: InstallSource::Npm {
            package: "@iflow-ai/iflow-cli",
        },
        binaries: &["iflow"],
        docs_url: "https://github.com/iflow-ai/iflow-cli",
    },
    ToolSpec {
        id: "grok_build",
        label: "Grok Build",
        source: InstallSource::PowerShellScript {
            command: "irm https://x.ai/cli/install.ps1 | iex",
        },
        binaries: &["grok"],
        docs_url: "https://github.com/xai-org/grok-build",
    },
    ToolSpec {
        id: "cursor_cli",
        label: "Cursor CLI",
        source: InstallSource::PowerShellScript {
            command: "irm 'https://cursor.com/install?win32=true' | iex",
        },
        binaries: &["cursor-agent"],
        docs_url: "https://cursor.com/docs/cli/installation",
    },
    ToolSpec {
        id: "trae_cli",
        label: "TRAE CLI",
        source: InstallSource::PowerShellScript {
            command: "irm https://trae.cn/trae-cli/install.ps1 | iex",
        },
        binaries: &["traecli"],
        docs_url: "https://docs.trae.cn/cli_get-started-with-trae-cli",
    },
    ToolSpec {
        id: "hermes",
        label: "Hermes Agent",
        source: InstallSource::PowerShellScript {
            command: "iex (irm https://hermes-agent.nousresearch.com/install.ps1)",
        },
        binaries: &["hermes"],
        docs_url: "https://hermes-agent.nousresearch.com",
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct CliToolStatus {
    pub id: String,
    pub label: String,
    pub installed: bool,
    /// 解析到的可执行文件路径
    pub path: Option<String>,
    pub version: Option<String>,
    /// 安装来源类型："npm" 或 "script"。
    pub source: String,
    /// 未安装时供界面展示的安装目标：npm 包名或「官方安装脚本」。
    pub install_target: String,
    /// 官方安装说明地址。
    pub docs_url: String,
    /// 当前平台是否具备执行该安装方式的条件（npm 可用 / Windows 有 PowerShell）。
    pub can_install: bool,
    /// 安装或更新时使用的确切命令（供界面原样展示）
    pub install_command: String,
}

/// 在给定 PATH 与常见安装目录中定位可执行文件。扩展名按 Windows 规则展开；
/// 其他平台退化为原样尝试。测试注入 PATH，不依赖真实环境。
pub fn resolve_binary(
    binaries: &[&str],
    path_env: &str,
    extra_dirs: &[PathBuf],
) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path_env).collect();
    dirs.extend(extra_dirs.iter().cloned());
    for binary in binaries {
        for dir in &dirs {
            if dir.as_os_str().is_empty() {
                continue;
            }
            for candidate in executable_names(binary) {
                let path = dir.join(&candidate);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn executable_names(binary: &str) -> Vec<String> {
    // PATHEXT 的顺序即解析优先级；.ps1 不是可直接执行的进程，因此不包含。
    ["com", "exe", "bat", "cmd"]
        .iter()
        .map(|ext| format!("{binary}.{ext}"))
        .chain(std::iter::once(binary.to_owned()))
        .collect()
}

#[cfg(not(windows))]
fn executable_names(binary: &str) -> Vec<String> {
    vec![binary.to_owned()]
}

/// 常见安装目录：npm 全局目录、原生安装器目录。仅作为 PATH 之外的补充。
pub fn well_known_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(appdata) = dirs::data_dir() {
        dirs.push(appdata.join("npm"));
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join("bin"));
        // 官方安装脚本（Grok Build / Hermes）默认写入的用户级目录。
        dirs.push(home.join(".grok").join("bin"));
        dirs.push(home.join(".hermes").join("bin"));
    }
    dirs
}

/// 从 `--version` 输出里提取版本号。工具们的输出格式不一（有的带前后缀、
/// 有的多行），所以只认第一个形如 `1.2.3` 的 token。
pub fn parse_version(stdout: &str) -> Option<String> {
    for token in stdout.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let cleaned = token
            .trim_start_matches('v')
            .trim_matches(|c: char| c == '(' || c == ')');
        if is_semver_like(cleaned) {
            return Some(cleaned.to_owned());
        }
    }
    None
}

fn is_semver_like(token: &str) -> bool {
    let core = token.split(['-', '+']).next().unwrap_or("");
    let mut parts = core.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    [major, minor, patch]
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// 运行 `<exe> --version` 并解析版本。失败时返回错误文本，供界面提示。
pub async fn read_version(path: &Path) -> Result<String, String> {
    let output = run_path(path, &["--version"], VERSION_TIMEOUT).await?;
    let combined = format!("{}\n{}", output.0, output.1);
    parse_version(&combined).ok_or_else(|| {
        format!(
            "无法从 --version 输出中识别版本：{}",
            truncate(combined.trim(), 120)
        )
    })
}

/// Windows 上 .cmd/.bat 需要经过 cmd.exe 启动；直接 spawn 会失败。
fn command_for(path: &Path) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if ext == "cmd" || ext == "bat" {
            let mut command = tokio::process::Command::new("cmd");
            command.arg("/C").arg(path);
            return command;
        }
    }
    tokio::process::Command::new(path)
}

async fn run_spawn(
    mut command: tokio::process::Command,
    display: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(String, String), String> {
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        // 后台运行不弹出控制台窗口。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("启动 {display} 失败：{e}"))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("{display} 执行超时（{} 秒）", timeout.as_secs()))?
        .map_err(|e| format!("{display} 执行失败：{e}"))?;
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// 按已知路径运行程序（自动处理 Windows 上的 .cmd/.bat 包装）。
pub(crate) async fn run_path(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<(String, String), String> {
    run_spawn(
        command_for(program),
        &program.display().to_string(),
        args,
        timeout,
    )
    .await
}

/// 按 PATH 中的名字运行系统命令（如 tasklist / taskkill）。
pub(crate) async fn run(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(String, String), String> {
    run_spawn(
        tokio::process::Command::new(program),
        program,
        args,
        timeout,
    )
    .await
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…")
}

/// 检测单个工具。PATH 与补充目录由调用方注入，便于在测试里构造隔离环境。
pub async fn detect(spec: &ToolSpec, path_env: &str, extra_dirs: &[PathBuf]) -> CliToolStatus {
    let path = resolve_binary(spec.binaries, path_env, extra_dirs);
    let version = match &path {
        Some(path) => read_version(path).await.ok(),
        None => None,
    };
    CliToolStatus {
        id: spec.id.to_owned(),
        label: spec.label.to_owned(),
        installed: path.is_some(),
        path: path.map(|path| path.display().to_string()),
        version,
        source: spec.source.kind().to_owned(),
        install_target: spec.source.target().to_owned(),
        docs_url: spec.docs_url.to_owned(),
        can_install: install_tool_available(&spec.source),
        install_command: spec.source.command(),
    }
}

/// 当前平台是否具备执行该安装方式的条件。npm 来源需要 npm，脚本来源需要
/// Windows 与 PowerShell；两者都不满足时界面禁用按钮并说明原因。
pub fn install_tool_available(source: &InstallSource) -> bool {
    match source {
        InstallSource::Npm { .. } => resolve_npm().is_some(),
        InstallSource::PowerShellScript { .. } => resolve_powershell().is_some(),
    }
}

/// 查询 npm registry 的 latest 版本。匿名可访问，只读。
pub async fn latest_version(package: &str, proxy: Option<&str>) -> Result<String, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(REGISTRY_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none());
    if let Some(proxy) = proxy.filter(|proxy| !proxy.trim().is_empty()) {
        builder = builder
            .proxy(reqwest::Proxy::all(proxy.trim()).map_err(|_| "HTTP 代理地址无效".to_string())?);
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    let url = format!("https://registry.npmjs.org/{package}/latest");
    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("查询 {package} 最新版本失败：{e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "查询 {package} 最新版本失败：HTTP {}",
            response.status().as_u16()
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| format!("{package} 的版本信息不是合法 JSON"))?;
    body.get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{package} 的版本信息缺少 version 字段"))
}

/// 执行一次安装或更新（两者是同一个动作：安装到最新版）。命令与参数全部
/// 来自内置常量，不接受任何用户输入。返回合并后的输出，供界面展示结果。
pub async fn install(spec: &ToolSpec) -> Result<String, String> {
    match &spec.source {
        InstallSource::Npm { package } => {
            let npm = resolve_npm().ok_or_else(|| {
                "未找到 npm。请先安装 Node.js，或改用官方安装方式安装该 CLI。".to_string()
            })?;
            let args = ["install", "-g", &format!("{package}@latest")];
            let (stdout, stderr) = run_path(&npm, &args, UPDATE_TIMEOUT).await?;
            let combined = format!("{stdout}\n{stderr}");
            Ok(truncate(combined.trim(), MAX_OUTPUT_CHARS))
        }
        InstallSource::PowerShellScript { command } => {
            let powershell = resolve_powershell().ok_or_else(|| {
                format!(
                    "未找到 PowerShell，无法执行官方安装脚本。请按官方文档手动安装：{}",
                    spec.docs_url
                )
            })?;
            let (stdout, stderr) = run_path(
                &powershell,
                &[
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    command,
                ],
                SCRIPT_TIMEOUT,
            )
            .await?;
            let combined = format!("{stdout}\n{stderr}");
            Ok(truncate(combined.trim(), MAX_OUTPUT_CHARS))
        }
    }
}

fn resolve_npm() -> Option<PathBuf> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    resolve_binary(&["npm"], &path_env, &well_known_dirs())
}

fn resolve_powershell() -> Option<PathBuf> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    resolve_binary(&["powershell", "pwsh"], &path_env, &well_known_dirs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing_handles_real_world_output_shapes() {
        assert_eq!(parse_version("1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(
            parse_version("claude 2.1.97 (Claude Code)\n").as_deref(),
            Some("2.1.97")
        );
        // 预发布后缀必须保留：显示成 0.44.0 会让用户以为装的是正式版。
        assert_eq!(
            parse_version("v0.44.0-beta.1").as_deref(),
            Some("0.44.0-beta.1")
        );
        assert_eq!(
            parse_version("codex-cli 0.2.0-alpha.3").as_deref(),
            Some("0.2.0-alpha.3")
        );
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(parse_version("1.2"), None, "两段式版本不算合法");
    }

    #[test]
    fn binary_resolution_prefers_path_order_and_expands_windows_extensions() {
        let dir_a = std::env::temp_dir().join(format!("cli-a-{}", uuid::Uuid::new_v4()));
        let dir_b = std::env::temp_dir().join(format!("cli-b-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        let name = if cfg!(windows) {
            "claude.cmd"
        } else {
            "claude"
        };
        std::fs::write(dir_a.join(name), b"@echo off\n").unwrap();
        std::fs::write(dir_b.join(name), b"@echo off\n").unwrap();

        let path_env = std::env::join_paths([&dir_b, &dir_a]).unwrap();
        let resolved = resolve_binary(&["claude"], path_env.to_str().unwrap(), &[]).unwrap();
        assert_eq!(
            resolved.parent(),
            Some(dir_b.as_path()),
            "应命中 PATH 中靠前的目录"
        );

        // PATH 里没有时，补充目录仍然可命中（对应 npm 全局目录的场景）。
        let empty_path = std::env::join_paths([std::env::temp_dir()]).unwrap();
        let resolved = resolve_binary(
            &["claude"],
            empty_path.to_str().unwrap(),
            std::slice::from_ref(&dir_a),
        );
        assert_eq!(
            resolved.map(|path| path.parent().map(Path::to_path_buf)),
            Some(Some(dir_a.clone()))
        );

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[tokio::test]
    async fn detection_reports_installed_version_from_an_isolated_directory() {
        let dir = std::env::temp_dir().join(format!("cli-detect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        if cfg!(windows) {
            std::fs::write(
                dir.join("claude.cmd"),
                b"@echo off\r\necho 2.1.97 (Claude Code)\r\n",
            )
            .unwrap();
        } else {
            let script = dir.join("claude");
            std::fs::write(&script, b"#!/bin/sh\necho 2.1.97 (Claude Code)\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&script).unwrap().permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&script, permissions).unwrap();
            }
        }

        let spec = &TOOLS[0];
        let empty_path = std::env::join_paths([std::env::temp_dir()]).unwrap();
        let status = detect(
            spec,
            empty_path.to_str().unwrap(),
            std::slice::from_ref(&dir),
        )
        .await;
        assert!(status.installed);
        assert_eq!(status.version.as_deref(), Some("2.1.97"));
        assert_eq!(
            status.install_command,
            "npm install -g @anthropic-ai/claude-code@latest"
        );

        // 目录里没有该工具时必须是「未安装」而不是猜测。
        let missing = detect(spec, empty_path.to_str().unwrap(), &[]).await;
        assert!(!missing.installed);
        assert!(missing.version.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_catalog_is_unique_and_carries_an_executable_install_command() {
        let mut ids: Vec<&str> = TOOLS.iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "工具 id 必须唯一");
        // 阈值贴着清单规模设置：误删一半工具时这里必须变红，而不是仍然通过。
        assert!(count >= 20, "工具清单不应缩水到 {count} 个");

        for spec in TOOLS {
            assert!(!spec.label.is_empty(), "{} 缺少显示名", spec.id);
            assert!(!spec.binaries.is_empty(), "{} 缺少可执行文件名", spec.id);
            assert!(
                spec.docs_url.starts_with("https://"),
                "{} 缺少官方说明地址",
                spec.id
            );
            let command = spec.source.command();
            assert!(!command.trim().is_empty(), "{} 缺少安装命令", spec.id);
            match &spec.source {
                InstallSource::Npm { package } => {
                    assert!(
                        command.contains(&format!("{package}@latest")),
                        "{} 的 npm 命令与包名不一致：{command}",
                        spec.id
                    );
                }
                InstallSource::PowerShellScript { .. } => {
                    // 脚本类必须来自官方 https 地址，且命令里不出现用户输入占位。
                    assert!(command.contains("https://"), "{} 脚本缺少官方地址", spec.id);
                    assert!(
                        command.contains("iex"),
                        "{} 脚本必须显式执行下载内容",
                        spec.id
                    );
                }
            }
        }

        // 两类来源都要有实际覆盖：工具清单里同时存在 npm 与官方脚本两类。
        assert!(TOOLS.iter().any(|spec| spec.source.package().is_some()));
        assert!(TOOLS.iter().any(|spec| spec.source.package().is_none()));
    }

    #[tokio::test]
    async fn script_tools_report_their_own_source_and_never_invent_versions() {
        let spec = TOOLS
            .iter()
            .find(|spec| spec.id == "grok_build")
            .expect("Grok Build 必须在清单里");
        let empty_path = std::env::join_paths([std::env::temp_dir()]).unwrap();
        let status = detect(spec, empty_path.to_str().unwrap(), &[]).await;

        assert_eq!(status.source, "script");
        assert_eq!(status.install_target, "官方安装脚本");
        assert_eq!(status.version, None);
        assert_eq!(
            status.install_command,
            "irm https://x.ai/cli/install.ps1 | iex"
        );
        // 脚本类工具没有 npm 包，不能拿去查 registry。
        assert!(spec.source.package().is_none());

        // 未安装时也能给出类别正确的状态，供界面展示「安装」按钮。
        assert!(!status.installed);
        assert!(status.docs_url.starts_with("https://"));
    }

    #[tokio::test]
    async fn npm_tools_expose_package_name_and_exact_command() {
        let spec = TOOLS
            .iter()
            .find(|spec| spec.id == "qoder")
            .expect("Qoder CLI 必须在清单里");
        let empty_path = std::env::join_paths([std::env::temp_dir()]).unwrap();
        let status = detect(spec, empty_path.to_str().unwrap(), &[]).await;

        assert_eq!(status.source, "npm");
        assert_eq!(status.install_target, "@qoder-ai/qodercli");
        assert_eq!(
            status.install_command,
            "npm install -g @qoder-ai/qodercli@latest"
        );
        assert_eq!(spec.source.package(), Some("@qoder-ai/qodercli"));
    }
}
