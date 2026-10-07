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

/// 杀进程树（同步尽力而为），供 `Drop` 路径用。
///
/// `Drop` 里不能 `.await`，所以单独一个同步版本。
pub fn kill_tree_blocking(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
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

    /// 起一个会活很久的子进程，杀掉它，确认它真的没了。
    #[tokio::test]
    async fn 杀树之后子进程不再存活() {
        let mut child = spawn_sleeper().await;
        let pid = child.id().expect("应当拿得到 pid");
        kill_tree(&mut child).await;

        // 给它一点时间退场，然后确认进程真的不在了
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !pid_alive(pid),
            "kill_tree 之后 pid {pid} 仍然存活 —— 树杀没生效"
        );
    }

    /// 对照组：不杀的话它应该**还活着**。
    /// 没有这一条，上面那条可能只是因为「进程本来就起不来」而通过。
    #[tokio::test]
    async fn 对照组_不杀的时候进程还活着() {
        let mut child = spawn_sleeper().await;
        let pid = child.id().expect("应当拿得到 pid");
        assert!(pid_alive(pid), "前置：子进程应当活着");
        let _ = child.kill().await; // 清理
        let _ = child.wait().await;
    }

    /// 起一个**孙进程**：父进程再起一个 sleeper。
    /// 这是本模块存在的全部理由 —— `Child::kill` 杀不掉它。
    #[tokio::test]
    async fn 杀树能带走孙进程() {
        let mut child = spawn_grandchild_spawner().await;
        let _ = child.id().expect("应当拿得到 pid");
        // 等它把孙进程起来
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let grandchild = read_grandchild_pid().await;
        // 【不许「跳过」】项目规则：`eprintln!` + `return` 会被 cargo 记成 ok，
        // 而「跳过」与「通过」必须可区分。本机 `powershell` 必然可用，
        // 所以拿不到孙进程 pid 就是**失败**，不是环境不支持。
        let grandchild = grandchild.expect(
            "拿不到孙进程 pid —— 拿不到就说明这条用例什么都没验，\
             不能记成通过（本机 powershell 必然可用）",
        );
        assert!(pid_alive(grandchild), "前置：孙进程应当活着");

        kill_tree(&mut child).await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            !pid_alive(grandchild),
            "kill_tree 之后孙进程 {grandchild} 仍然存活 —— 只杀了直接子进程"
        );
    }

    // ---------- 平台相关的小工具 ----------

    async fn spawn_sleeper() -> Child {
        #[cfg(windows)]
        {
            tokio::process::Command::new("cmd")
                .args(["/C", "ping -n 60 127.0.0.1 > NUL"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("起 sleeper")
        }
        #[cfg(not(windows))]
        {
            tokio::process::Command::new("sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("起 sleeper")
        }
    }

    /// 起一个「再起一个 sleeper 并把它的 pid 写进文件」的父进程。
    async fn spawn_grandchild_spawner() -> Child {
        let pid_file = grandchild_pid_file();
        let _ = std::fs::remove_file(&pid_file);
        #[cfg(windows)]
        {
            // `start /b` 起一个脱离的 sleeper，然后写它的 pid 需要额外手段；
            // 这里用 powershell 起子进程并落 pid，形状与真实包装器一致
            // （`codex.cmd` → node 也是同样的一层）。
            let script = format!(
                "$p = Start-Process -FilePath 'cmd' -ArgumentList '/C','ping -n 60 127.0.0.1 > NUL' \
                 -PassThru -WindowStyle Hidden; Set-Content -Path '{}' -Value $p.Id; Start-Sleep -Seconds 60",
                pid_file.display()
            );
            tokio::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("起 grandchild spawner")
        }
        #[cfg(not(windows))]
        {
            let script = format!("sleep 60 & echo $! > {}; sleep 60", pid_file.display());
            tokio::process::Command::new("sh")
                .args(["-c", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("起 grandchild spawner")
        }
    }

    fn grandchild_pid_file() -> std::path::PathBuf {
        std::env::temp_dir().join("llmgw-proc-util-grandchild.pid")
    }

    async fn read_grandchild_pid() -> Option<u32> {
        let path = grandchild_pid_file();
        for _ in 0..10 {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    return Some(pid);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        None
    }

    #[cfg(windows)]
    fn pid_alive(pid: u32) -> bool {
        // `tasklist /FI "PID eq N"` 会输出一行表头 + 命中行；
        // 用输出里是否含该 pid 判定。不做 `kill -0` 那种信号检查 ——
        // Windows 上没有对应的东西。
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()),
            Err(_) => false,
        }
    }

    #[cfg(not(windows))]
    fn pid_alive(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}
