//! 子进程工具。**A6/A7 与 C1（MCP）共用同一份实现。**
//!
//! ## 为什么不各写一份
//!
//! 「杀进程树」这件事在 Windows 上有明确的坑（见下），而它的正确写法
//! 只有一处 —— 抄第二份的结果是两处各自演化，而**其中一处会忘记孙进程**。
//! 那种缺陷的表现是「测试跑完了，但端口还被占着 / 文件还被锁着」，
//! 极难归因。
//!
//! ## Windows 上为什么必须杀树
//!
//! `Child::kill` 只杀**直接子进程**。而本项目的账号型上游
//! （`codex` / `qoder`）在本机都是**包装器脚本**
//! （`codex.cmd` + `codex.ps1`），真正的 node 进程是**孙进程** ——
//! 杀父进程之后它继续跑、继续占着资源，而调用方以为已经清理干净了。
//!
//! 这不是理论：2026-10-07 实测 `codex exec --json` 会**静默挂住**
//! （90 秒零输出且不结束），所以超时杀树是这条路径上**必然触发**的分支。

use std::process::Stdio;

use tokio::process::Child;

/// 杀进程**树**（异步版本，供已经持有 `Child` 的调用方用）。
///
/// 失败时**退回单进程 kill**，并留一条 warn —— 尽力而为，但不假装成功。
pub async fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        if let Some(pid) = child.id() {
            if !taskkill(pid).await {
                tracing::warn!("taskkill 失败，退回单进程 kill（pid={pid}）");
                let _ = child.kill().await;
            }
            return;
        }
    }
    // 非 Windows：没有现成的树杀工具，退化成杀直接子进程。
    // **边界写在这里**：孙子进程会留下来，需要它的运行环境自己处理。
    let _ = child.kill().await;
}

/// 杀进程树（同步，等 `taskkill` 真的跑完），供 `Drop` 与启动清扫用。
///
/// `Drop` 里不能 `.await`，所以单独一个同步版本。
///
/// 【为什么是 `output()` 而不是 `spawn()`】`spawn()` 只是把 `taskkill`
/// 拉起来就返回，**根本不等它做完** —— 调用方拿到「函数返回了」却不知道
/// 进程到底死没死，紧接着的检查会看到它还活着。
/// 实测踩到：B8 的孤儿清理用例里 `killed` 计数是对的（说明走到了杀的分支），
/// 但进程仍在运行。阻塞几十毫秒换一个确定的结论，值。
pub fn kill_tree_blocking(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
    }
    #[cfg(not(windows))]
    {
        // 非 Windows 上 `Drop` 路径依赖 `kill_on_drop(true)`。
        // 这里显式什么都不做，而不是假装杀了 —— 见函数名的 `blocking` 只
        // 表示「不异步」，不表示「一定成功」。
        let _ = pid;
    }
}

#[cfg(windows)]
async fn taskkill(pid: u32) -> bool {
    tokio::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    const READY_TIMEOUT: Duration = Duration::from_secs(15);
    const POLL_INTERVAL: Duration = Duration::from_millis(100);
    // CI 上 PowerShell 启动可能超过旧测试的 600ms + 10 * 100ms 等待窗口。
    const GRANDCHILD_START_DELAY: Duration = Duration::from_secs(2);

    /// 起一个会活很久的子进程，杀掉它，确认它真的没了。
    #[tokio::test]
    async fn 杀树之后子进程不再存活() {
        let mut child = spawn_sleeper().await;
        let pid = child.child.id().expect("应当拿得到 pid");
        assert!(
            pid_alive(pid).expect("读取自有子进程存活状态"),
            "前置：子进程应当活着"
        );
        kill_tree(&mut child.child).await;

        assert!(
            wait_until_gone(pid).await.expect("读取自有子进程退出状态"),
            "kill_tree 之后 pid {pid} 仍然存活 —— 树杀没生效"
        );
    }

    /// 对照组：不杀的话它应该**还活着**。
    /// 没有这一条，上面那条可能只是因为「进程本来就起不来」而通过。
    #[tokio::test]
    async fn 对照组_不杀的时候进程还活着() {
        let child = spawn_sleeper().await;
        let pid = child.child.id().expect("应当拿得到 pid");
        assert!(
            pid_alive(pid).expect("读取自有子进程存活状态"),
            "前置：子进程应当活着"
        );
        // TestProcess 的 Drop 清理整棵自有树，断言失败也不会留下 ping。
    }

    /// 起一个**孙进程**：父进程再起一个 sleeper。
    /// 这是本模块存在的全部理由 —— `Child::kill` 杀不掉它。
    #[tokio::test]
    async fn 杀树能带走孙进程() {
        let started = tokio::time::Instant::now();
        let mut child = spawn_grandchild_spawner().await;
        let _ = child.child.id().expect("应当拿得到 pid");
        let grandchild = read_grandchild_pid(&mut child).await;
        // 【不许「跳过」】项目规则：`eprintln!` + `return` 会被 cargo 记成 ok，
        // 而「跳过」与「通过」必须可区分。本机 `powershell` 必然可用，
        // 所以拿不到孙进程 pid 就是**失败**，不是环境不支持。
        let grandchild = grandchild.expect("拿不到存活的孙进程 pid，不能记成通过");
        assert!(
            started.elapsed() >= GRANDCHILD_START_DELAY,
            "夹具必须实际经过超过旧 1.6s 等待窗口的启动延迟"
        );
        assert!(
            pid_alive(grandchild).expect("读取自有孙进程存活状态"),
            "前置：孙进程应当活着"
        );

        kill_tree(&mut child.child).await;
        assert!(
            wait_until_gone(grandchild)
                .await
                .expect("读取自有孙进程退出状态"),
            "kill_tree 之后孙进程 {grandchild} 仍然存活 —— 只杀了直接子进程"
        );
    }

    // ---------- 平台相关的小工具 ----------

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("llmgw-proc-util-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).expect("创建本测试专用目录");
            Self(path)
        }

        fn pid_file(&self) -> PathBuf {
            self.0.join("grandchild.pid")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 失败、超时和断言 panic 都清理自己启动的父树及已确认的孙进程。
    struct TestProcess {
        child: Child,
        grandchild: Option<u32>,
        directory: Option<TestDirectory>,
    }

    impl Drop for TestProcess {
        fn drop(&mut self) {
            if !matches!(self.child.try_wait(), Ok(Some(_))) {
                if let Some(pid) = self.child.id() {
                    kill_tree_blocking(pid);
                }
                let _ = self.child.start_kill();
            }
            // 先停止父树，再读私有文件，父进程不能继续写入新的 PID。
            let grandchild = self.grandchild.or_else(|| {
                self.directory.as_ref().and_then(|directory| {
                    std::fs::read_to_string(directory.pid_file())
                        .ok()?
                        .trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|pid| *pid > 0)
                })
            });
            // 检测失败时仍尝试清理已确认自有的 PID，Drop 不再触发二次 panic。
            if let Some(pid) = grandchild.filter(|pid| pid_alive(*pid).unwrap_or(true)) {
                #[cfg(windows)]
                kill_tree_blocking(pid);
                #[cfg(not(windows))]
                {
                    let _ = std::process::Command::new("kill")
                        .args(["-KILL", &pid.to_string()])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .output();
                }
            }
        }
    }

    async fn spawn_sleeper() -> TestProcess {
        #[cfg(windows)]
        let child = {
            tokio::process::Command::new("cmd")
                .args(["/C", "ping -n 60 127.0.0.1 > NUL"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("起 sleeper")
        };
        #[cfg(not(windows))]
        let child = {
            tokio::process::Command::new("sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("起 sleeper")
        };
        TestProcess {
            child,
            grandchild: None,
            directory: None,
        }
    }

    /// 起一个「再起一个 sleeper 并把它的 pid 写进文件」的父进程。
    async fn spawn_grandchild_spawner() -> TestProcess {
        let directory = TestDirectory::new();
        let pid_file = directory.pid_file();
        #[cfg(windows)]
        let child = {
            // `start /b` 起一个脱离的 sleeper，然后写它的 pid 需要额外手段；
            // 这里用 powershell 起子进程并落 pid，形状与真实包装器一致
            // （`codex.cmd` → node 也是同样的一层）。
            let script = format!(
                "$ErrorActionPreference = 'Stop'; Start-Sleep -Milliseconds {}; $p = $null; \
                 try {{ $p = Start-Process -FilePath 'cmd.exe' \
                 -ArgumentList '/C','ping -n 60 127.0.0.1 > NUL' -PassThru -WindowStyle Hidden; \
                 Set-Content -LiteralPath $env:LLMGW_PROC_UTIL_PID_FILE -Value $p.Id -Encoding ASCII; \
                 Start-Sleep -Seconds 60 }} catch {{ \
                 if ($null -ne $p) {{ & taskkill.exe /T /F /PID $p.Id > $null 2>&1 }}; throw }}",
                GRANDCHILD_START_DELAY.as_millis()
            );
            tokio::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .env("LLMGW_PROC_UTIL_PID_FILE", &pid_file)
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("起 grandchild spawner")
        };
        #[cfg(not(windows))]
        let child = {
            let script = format!(
                "sleep {}; sleep 60 & echo $! > \"$LLMGW_PROC_UTIL_PID_FILE\"; sleep 60",
                GRANDCHILD_START_DELAY.as_secs()
            );
            tokio::process::Command::new("sh")
                .args(["-c", &script])
                .env("LLMGW_PROC_UTIL_PID_FILE", &pid_file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("起 grandchild spawner")
        };
        TestProcess {
            child,
            grandchild: None,
            directory: Some(directory),
        }
    }

    async fn read_grandchild_pid(process: &mut TestProcess) -> Result<u32, String> {
        let path = process
            .directory
            .as_ref()
            .expect("孙进程夹具必须有独立目录")
            .pid_file();
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        loop {
            match process.child.try_wait() {
                Ok(Some(status)) => return Err(format!("父进程在就绪前退出：{status}")),
                Err(error) => return Err(format!("无法读取自有父进程状态：{error}")),
                Ok(None) => {}
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    if pid > 0 {
                        process.grandchild = Some(pid);
                        if pid_alive(pid)? {
                            return Ok(pid);
                        }
                        return Err("孙进程在就绪前已退出".into());
                    }
                }
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err("等待自有孙进程 PID 和存活状态超过 15 秒".into());
            }
            tokio::time::sleep(POLL_INTERVAL.min(deadline - now)).await;
        }
    }

    async fn wait_until_gone(pid: u32) -> Result<bool, String> {
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        while pid_alive(pid)? {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(POLL_INTERVAL.min(deadline - now)).await;
        }
        Ok(true)
    }

    #[cfg(windows)]
    fn pid_alive(pid: u32) -> Result<bool, String> {
        // `tasklist /FI "PID eq N"` 会输出一行表头 + 命中行；
        // 用输出里是否含该 pid 判定。不做 `kill -0` 那种信号检查 ——
        // Windows 上没有对应的东西。
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map_err(|error| format!("无法执行 tasklist：{error}"))?;
        if !out.status.success() {
            return Err(format!("tasklist 执行失败：{}", out.status));
        }
        Ok(String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
    }

    #[cfg(not(windows))]
    fn pid_alive(pid: u32) -> Result<bool, String> {
        Ok(std::path::Path::new(&format!("/proc/{pid}")).exists())
    }
}
