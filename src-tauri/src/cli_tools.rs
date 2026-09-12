//! 本机 CLI 工具检测：是否存在、装在哪、什么版本、能不能升级。
//!
//! 只做三件事，且都不修改用户环境：
//!  1) 在 PATH 与常见安装目录里定位可执行文件（Windows 需要处理 .cmd/.exe 等）；
//!  2) 运行 `<tool> --version` 读取版本（带超时，避免卡住界面）；
//!  3) 查询 npm registry 的 latest 版本，供界面提示「可更新」。
//!
//! 更新动作本身由界面显式触发，并展示确切命令后才执行。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

const VERSION_TIMEOUT: Duration = Duration::from_secs(15);
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(10);
const UPDATE_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_OUTPUT_CHARS: usize = 4_000;

/// 一个受支持的 CLI 工具。
pub struct ToolSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// npm 包名；也是更新命令里使用的名称
    pub npm_package: &'static str,
    /// 可能出现在 PATH 中的可执行文件名（不含扩展名）
    pub binaries: &'static [&'static str],
}

pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        id: "claude_code",
        label: "Claude Code",
        npm_package: "@anthropic-ai/claude-code",
        binaries: &["claude"],
    },
    ToolSpec {
        id: "codex",
        label: "Codex CLI",
        npm_package: "@openai/codex",
        binaries: &["codex"],
    },
    ToolSpec {
        id: "gemini_cli",
        label: "Gemini CLI",
        npm_package: "@google/gemini-cli",
        binaries: &["gemini"],
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct CliToolStatus {
    pub id: String,
    pub label: String,
    pub npm_package: String,
    pub installed: bool,
    /// 解析到的可执行文件路径
    pub path: Option<String>,
    pub version: Option<String>,
    /// 更新时使用的确切命令（供界面原样展示）
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
    let output = run_command(path, &["--version"], VERSION_TIMEOUT).await?;
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

async fn run_command(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<(String, String), String> {
    let mut command = command_for(program);
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
        .map_err(|e| format!("启动 {} 失败：{e}", program.display()))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("{} 执行超时（{} 秒）", program.display(), timeout.as_secs()))?
        .map_err(|e| format!("{} 执行失败：{e}", program.display()))?;
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
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
        npm_package: spec.npm_package.to_owned(),
        installed: path.is_some(),
        path: path.map(|path| path.display().to_string()),
        version,
        install_command: format!("npm install -g {}@latest", spec.npm_package),
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

/// 执行一次更新。命令与参数全部来自内置常量，不接受任何用户输入。
/// 返回合并后的输出，供界面展示结果。
pub async fn update(spec: &ToolSpec) -> Result<String, String> {
    let npm = resolve_npm().ok_or_else(|| {
        "未找到 npm。请先安装 Node.js，或改用官方安装方式升级该 CLI。".to_string()
    })?;
    let args = ["install", "-g", &format!("{}@latest", spec.npm_package)];
    let (stdout, stderr) = run_command(&npm, &args, UPDATE_TIMEOUT).await?;
    let combined = format!("{stdout}\n{stderr}");
    Ok(truncate(combined.trim(), MAX_OUTPUT_CHARS))
}

fn resolve_npm() -> Option<PathBuf> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    resolve_binary(&["npm"], &path_env, &well_known_dirs())
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
}
