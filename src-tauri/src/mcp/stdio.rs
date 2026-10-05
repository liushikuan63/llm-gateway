//! C1 stdio 传输：本地拉起 MCP server 进程。
//!
//! ## stdout 是协议通道，不是日志通道
//!
//! 拉起第三方 MCP server 时，**子进程的 stdout 就是 JSON-RPC 协议通道**。
//! 任何往 stdout 打的日志都会污染它。实测踩过（事实源 E1-5）：
//! 参考项目里那个基于 Spring 的 server 默认会往 stdout 打 banner 与
//! 彩色日志，必须显式关掉三处：
//!
//! ```text
//! logging.pattern.console=      # 清空控制台 pattern
//! spring.main.web-application-type=none
//! spring.main.banner-mode=off
//! ```
//!
//! 那三条是**被拉起方**的配置，网关管不着别人的启动参数。
//! 网关这边能做的、也必须做的是：
//!
//! 1. **绝不把子进程的 stderr 并进 stdout** —— 合并了就等于自己制造污染。
//!    stderr 单独读出来转成网关日志。
//! 2. **握手时容忍非 JSON 行**：跳过并计数，而不是直接失败。
//!    一个只会打一行 banner 的 server 不该被判死。
//! 3. 跳过的行数**要能看见**（[`StdioSession::skipped_lines`]）——
//!    「能握手」不等于「server 干净」，长期飘日志的 server 得有人管。
//!
//! ## 进程清理
//!
//! 卡片铁律要求「超时杀进程树」。Windows 上 `Child::kill` 只杀直接子进程，
//! 孙进程会留下来 —— 所以用 `taskkill /T /F` 走进程树。
//! 非 Windows 上没有现成的树杀工具，退化成 `kill`，并把这个边界写在注释里。

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

/// 一次 stdio 会话。
pub struct StdioSession {
    child: Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    /// 握手期间跳过的非 JSON 行数。
    ///
    /// 这个计数是给运维看的：0 表示 server 干净；
    /// 一直涨说明它往协议通道里打日志，该去关那三处配置。
    skipped_lines: usize,
    /// 跳过的行的样本（最多留几条），便于直接看出是什么在污染。
    skipped_samples: Vec<String>,
}

/// 跳过样本的保留条数。
const MAX_SKIPPED_SAMPLES: usize = 3;

/// 单次读写的默认超时。
///
/// 不拍脑袋：MCP server 的握手通常是毫秒级，但本地拉起一个 JVM 要几秒。
/// 30 秒覆盖「冷启动 + 首次握手」，超时即判失败并杀进程树。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

impl StdioSession {
    /// 拉起一个 server。`command` 是要执行的程序，`args` 是它的参数。
    ///
    /// **工作目录隔离**：卡片铁律要求子进程 cwd 隔离 ——
    /// 不隔离的话 server 会以网关的工作目录为基准解析相对路径，
    /// 于是它能读到网关目录下的文件，而这是谁都没打算给它的权限。
    /// 这里把 cwd 设成一个独立的临时子目录。
    pub async fn spawn(
        command: &str,
        args: &[String],
        workdir: &std::path::Path,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(workdir)?;
        let mut child = Command::new(command)
            .args(args)
            .current_dir(workdir)
            // stdin / stdout 是协议通道
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr **单独管道**，绝不并进 stdout
            .stderr(Stdio::piped())
            // 不给它继承网关的 stdin：否则它会跟网关抢同一个终端输入
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child.stdin.take().expect("stdin 已 pipe");
        let stdout = BufReader::new(child.stdout.take().expect("stdout 已 pipe"));

        // stderr 单独排空到一个日志任务里。
        // **必须排空**：管道缓冲区满了之后子进程会写阻塞，
        // 表现为「server 卡死」，而根因在一个跟协议无关的地方。
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // 落到网关日志，带上 server 前缀便于分辨
                    tracing::debug!(target: "mcp::stdio", "[server stderr] {line}");
                }
            });
        }

        Ok(Self {
            child,
            stdin,
            stdout,
            skipped_lines: 0,
            skipped_samples: Vec::new(),
        })
    }

    pub fn skipped_lines(&self) -> usize {
        self.skipped_lines
    }

    pub fn skipped_samples(&self) -> &[String] {
        &self.skipped_samples
    }

    /// 发一条 JSON-RPC 请求并等一条**能解析成 JSON** 的响应。
    ///
    /// 中间那些不能解析成 JSON 的行会被跳过并计数 ——
    /// 这就是「故意打日志的 server 也要能握手」的实现。
    pub async fn request(&mut self, payload: &Value) -> Result<Value, String> {
        self.request_with_timeout(payload, DEFAULT_TIMEOUT).await
    }

    pub async fn request_with_timeout(
        &mut self,
        payload: &Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let mut body = serde_json::to_string(payload).map_err(|e| e.to_string())?;
        body.push('\n');
        self.stdin
            .write_all(body.as_bytes())
            .await
            .map_err(|e| format!("写 stdin 失败：{e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("flush stdin 失败：{e}"))?;

        let mut line = String::new();
        let read = async {
            loop {
                line.clear();
                let n = self
                    .stdout
                    .read_line(&mut line)
                    .await
                    .map_err(|e| format!("读 stdout 失败：{e}"))?;
                if n == 0 {
                    return Err("server 关闭了 stdout（进程可能已退出）".to_string());
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => return Ok(value),
                    Err(_) => {
                        // 非 JSON：跳过、计数、留样本。**不判死** ——
                        // 一个只会打一行 banner 的 server 不该被判失败。
                        self.skipped_lines += 1;
                        if self.skipped_samples.len() < MAX_SKIPPED_SAMPLES {
                            self.skipped_samples
                                .push(trimmed.chars().take(120).collect());
                        }
                    }
                }
            }
        };

        match tokio::time::timeout(timeout, read).await {
            Ok(result) => result,
            Err(_) => {
                // 超时：按铁律杀进程树，不留孤儿
                self.kill_tree().await;
                Err(format!(
                    "等待响应超时（{}s），已终止进程树",
                    timeout.as_secs()
                ))
            }
        }
    }

    /// 杀进程**树**。
    ///
    /// Windows 上 `Child::kill` 只杀直接子进程，孙进程会留下来占着端口、
    /// 继续写文件。`taskkill /T /F` 才走整棵树。
    pub async fn kill_tree(&mut self) {
        #[cfg(windows)]
        {
            if let Some(pid) = self.child.id() {
                // 失败就退回单进程 kill —— 尽力而为，但不假装成功
                let killed = Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &pid.to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .is_ok_and(|s| s.success());
                if !killed {
                    tracing::warn!("taskkill 失败，退回单进程 kill（pid={pid}）");
                    let _ = self.child.kill().await;
                }
                return;
            }
        }
        // 非 Windows：没有现成的树杀工具，退化成杀直接子进程。
        // **边界写在这里**：孙子进程会留下来，需要它的运行环境自己处理。
        let _ = self.child.kill().await;
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        // `kill_on_drop(true)` 已经保证直接子进程会走，
        // 这里只是把「树杀」的意图也覆盖到 drop 路径上（同步尽力而为）。
        #[cfg(windows)]
        if let Some(pid) = self.child.id() {
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
}

/// 生成一个隔离子目录，供某个 server 当工作目录。
///
/// 放在系统临时目录下按 server id 分开：同一个 server 的两次会话
/// 共用一个目录是可以的（它会把自己的状态放这儿），
/// 不同 server 之间必须分开。
pub fn isolated_workdir(server_id: &str) -> std::path::PathBuf {
    // 只保留 id 里的安全字符：id 来自配置，但它会被拼进路径，
    // 带上 `..` 就能跳出临时目录。
    let safe: String = server_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::env::temp_dir().join("llm-gateway-mcp").join(safe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 工作目录按_server_隔离() {
        let a = isolated_workdir("alpha");
        let b = isolated_workdir("beta");
        assert_ne!(a, b, "不同 server 必须用不同工作目录");
        assert!(a.ends_with("alpha"));
    }

    #[test]
    fn 工作目录名里的路径穿越被消掉() {
        // id 来自配置，会被拼进路径 —— 带上 `..` 就能跳出临时目录
        let dir = isolated_workdir("../../etc/passwd");
        let s = dir.to_string_lossy().to_string();
        assert!(!s.contains(".."), "路径穿越没被消掉：{s}");
        assert!(s.contains("llm-gateway-mcp"), "应当仍在隔离根目录下：{s}");
        // 斜杠也被替换掉了，所以不会多出一层目录
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains('/') && !name.contains('\\'), "实际：{name}");
    }

    #[test]
    fn 中文与特殊字符的_server_id_也安全() {
        let dir = isolated_workdir("服务器:v1");
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "实际：{name}"
        );
    }
}
