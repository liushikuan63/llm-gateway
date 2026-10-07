//! B8 判据 4：插件进程的**孤儿清理**。
//!
//! ## 为什么需要它
//!
//! 插件是长驻子进程。网关崩溃 / 被强杀时，它没有机会去杀自己拉起来的进程 ——
//! 于是那些进程留在系统里，占端口、占内存，而且**下一次启动根本看不到它们**。
//! 卡片要的正是「网关进程崩溃后重启，插件进程必须被清理干净」。
//!
//! ## 为什么不用 Job Object
//!
//! Windows 的 Job Object 是这个问题最干净的解（父进程一死，OS 连带杀掉整个
//! job）。但它要 `windows` / `winapi` crate —— 而本仓有「不许加依赖」的硬约束。
//! 所以这里用**台账 + 启动清扫**：网关每次起插件都记一条（pid + exe），
//! 下次启动先按台账清理上一轮的残留。
//!
//! ## 误杀是怎么防的
//!
//! 只按 pid 杀是危险的：**pid 会被系统复用**，网关重启后那个号可能已经属于
//! 一个毫不相干的进程。所以动手前用 `tasklist` 核对**进程名**，
//! 只有名字与台账里记的一致才杀。
//!
//! ## 三种情况都从台账移除
//!
//! - 查不到进程 ⇒ 它已经死了，无需处理；
//! - 名字匹配 ⇒ 杀掉；
//! - 名字不匹配 ⇒ **pid 被复用了，那个进程不是我们的** —— 更要移除，
//!   否则下一次启动还会来核对一遍，而每次都得出同一个结论。
//!
//! 三种都移除，所以 `sweep` 之后台账必然为空。留着一个永远清不掉的条目，
//! 只会让「台账里有一条」这件事失去意义。

use std::path::{Path, PathBuf};

/// 台账里的一条：一个我们拉起过的插件进程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginProcess {
    pub pid: u32,
    /// 可执行文件名（**只取 basename**）。核对进程名时用它 ——
    /// 完整路径在 `tasklist` 的默认输出里拿不到，而名字已经足够区分
    /// 「是我们的插件」与「是一个复用了同一个 pid 的别的进程」。
    pub exe: String,
}

/// 台账文件的默认位置。与 `config.toml` / `gateway.db` 同处。
pub fn ledger_path() -> PathBuf {
    crate::config::app_data_dir().join("plugin-processes.txt")
}

/// **某个插件**的台账路径。
///
/// 一个插件一个文件，不共享一份：共享台账要读-改-写，而多个插件可能同时起 ——
/// 那点锁的复杂度不值得，而且共享文件被写坏时**所有**插件的清理一起失效。
pub fn ledger_path_for(plugin_id: &str) -> PathBuf {
    crate::config::app_data_dir().join(format!("plugin-processes-{}.txt", sanitize_id(plugin_id)))
}

/// 把插件 id 消毒成安全的文件名片段。
///
/// id 来自**用户可以随手改的**描述文件。直接拿它拼路径会开出目录穿越
/// （`../../evil` 能写到应用数据目录之外），而那种写入是**静默的**。
/// 白名单：只留 ASCII 字母数字与 `-` `_`，其余一律换成 `_`，并截断到 64 字符。
pub fn sanitize_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "unnamed".to_string()
    } else {
        cleaned
    }
}

/// 扫掉**所有**插件的台账。网关启动时调一次。
///
/// 前缀匹配同时覆盖旧的单文件台账（`plugin-processes.txt`）——
/// 升级上来的机器上可能还留着它。
pub fn sweep_all() -> Result<usize, String> {
    let dir = crate::config::app_data_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // 目录还不存在（全新环境）不是错误。
        Err(_) => return Ok(0),
    };
    let mut killed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("plugin-processes") || !name.ends_with(".txt") {
            continue;
        }
        killed += sweep(&entry.path())?;
    }
    Ok(killed)
}

/// 把 exe 归一成 basename：`C:\tools\ref.exe` → `ref.exe`。
///
/// 同时吃掉 Windows 与 Unix 两种分隔符 —— 描述文件里写哪种都可能，
/// 而这里只是要比对名字，不该因为分隔符不同就判成「不是我们的进程」。
pub fn exe_basename(exe: &str) -> String {
    exe.rsplit(['/', '\\']).next().unwrap_or(exe).to_string()
}

/// 编码台账：每行 `pid<TAB>exe`。
///
/// 用制表符而不是逗号/空格：Windows 路径里空格很常见，
/// 而制表符在路径里是非法的。这样解码不需要转义规则。
pub fn encode(entries: &[PluginProcess]) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!("{}\t{}\n", entry.pid, entry.exe));
    }
    out
}

/// 解析台账。**坏行跳过**而不是整体失败。
///
/// 台账是崩溃现场留下来的文件，它本身可能只写了一半。为了一个坏行
/// 放弃整份台账，等于放弃清理 —— 而清理正是这个文件存在的唯一目的。
pub fn parse(raw: &str) -> Vec<PluginProcess> {
    raw.lines()
        .filter_map(|line| {
            let (pid, exe) = line.trim().split_once('\t')?;
            let pid = pid.trim().parse::<u32>().ok()?;
            let exe = exe.trim();
            if exe.is_empty() {
                return None;
            }
            Some(PluginProcess {
                pid,
                exe: exe.to_string(),
            })
        })
        .collect()
}

/// 查一个 pid 现在叫什么名字。查不到（进程不存在）返回 `None`。
///
/// 用 `tasklist` 而不是 winapi：它是系统自带命令，不需要新依赖。
/// 输出是 CSV，一行为 `"cmd.exe","1234","Console","1","3,456 K"`；
/// 没有匹配时输出一行 `INFO: 没有运行的任务匹配指定标准。`（中文本机）。
fn process_name(pid: u32) -> Option<String> {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next()?.trim();
    if first.is_empty() || first.starts_with("INFO:") || !first.starts_with('"') {
        return None;
    }
    let name = first
        .split(',')
        .next()?
        .trim()
        .trim_matches('"')
        .to_string();
    (!name.is_empty()).then_some(name)
}

/// 启动清扫：按台账把上一轮残留的插件进程杀掉，返回清掉了几个。
///
/// **同步阻塞**：`tasklist` 是外部命令。它只在启动路径上跑一次
/// （台账为空时几毫秒返回），不值得为它引入异步。
///
/// 台账不存在时返回 0（第一次启动的正常情形），不报错。
pub fn sweep(path: &Path) -> Result<usize, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return Ok(0),
    };
    let entries = parse(&raw);
    let mut killed = 0usize;
    for entry in &entries {
        let Some(name) = process_name(entry.pid) else {
            continue; // 已经死了
        };
        if !name.eq_ignore_ascii_case(&entry.exe) {
            // pid 被复用了 —— 那个进程不是我们的，不动它。
            continue;
        }
        crate::proc_util::kill_tree_blocking(entry.pid);
        killed += 1;
    }
    // 无论杀没杀掉，台账都要清空：三种情况都已经有结论（见模块文档）。
    let _ = std::fs::remove_file(path);
    Ok(killed)
}

/// 把当前活着的插件进程写进台账（起进程时调）。
///
/// **整体覆盖**而不是追加：台账描述的是「现在活着的那些」，
/// 追加会让已经退出的进程永远留在里面。
pub fn write_ledger(path: &Path, entries: &[PluginProcess]) -> Result<(), String> {
    if entries.is_empty() {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, encode(entries)).map_err(|e| format!("写插件进程台账失败：{e}"))
}

// ============================ 真进程 transport ============================

/// 起真子进程、按 JSONL 与它对话的传输层。
///
/// ## 进程是**长驻**的
///
/// 协议里 `request_id` 配对的意义就在于**同一个进程服务多次请求** ——
/// 每次请求都重启一个插件，等于把「进程启动开销」乘上请求数，
/// 而那种开销在 agent 类插件上通常不小（要加载模型 / 建连接）。
/// 所以这里惰性启动一次、之后复用。
///
/// ## 超时之后**必须杀进程**
///
/// 一个「活着但不说话」的插件会把请求挂到天荒地老（A6 实测到的
/// `codex exec` 正是这个行为）。超时后不杀的话，下一个请求会接着往一个
/// 已经乱掉的 stdin 里写，错误会以「协议解析失败」的形式出现在很远的地方。
pub struct ProcessTransport {
    /// 台账按它分文件 —— 见 [`ledger_path_for`]。
    plugin_id: String,
    exe: String,
    args: Vec<String>,
    inner: tokio::sync::Mutex<Option<Running>>,
}

struct Running {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
}

impl ProcessTransport {
    /// `exe` 与 `args` 应当**已经过 `validate_manifest` 的注入校验** ——
    /// 这里不重复校验（重复会掩盖「谁该负责」），也不会替调用方补。
    ///
    /// `plugin_id` 只用于**台账分文件**（网关崩溃后靠它找回自己拉起的进程），
    /// 所以它会被 [`sanitize_id`] 消毒后才拼进路径。
    pub fn new(plugin_id: impl Into<String>, exe: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            exe: exe.into(),
            args,
            inner: tokio::sync::Mutex::new(None),
        }
    }

    /// 记下当前这个进程（网关崩溃后靠它来清理）。
    ///
    /// **失败不阻断**：台账是兜底手段，写不进去也只是「下次启动清不掉」，
    /// 不该因此让插件起不来。
    fn note_ledger(&self, pid: u32) {
        let entry = PluginProcess {
            pid,
            exe: exe_basename(&self.exe),
        };
        if let Err(error) = write_ledger(&ledger_path_for(&self.plugin_id), &[entry]) {
            tracing::warn!("插件进程台账写入失败（不影响本次运行）：{error}");
        }
    }

    fn clear_ledger(&self) {
        let _ = std::fs::remove_file(ledger_path_for(&self.plugin_id));
    }

    fn spawn(&self) -> Result<Running, String> {
        let mut child = tokio::process::Command::new(&self.exe)
            .args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr 丢掉：插件的诊断信息与我们无关，混进 stdout 才是灾难。
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("起插件进程失败（{}）：{e}", self.exe))?;
        let stdin = child.stdin.take().ok_or("拿不到插件的 stdin")?;
        let stdout = child.stdout.take().ok_or("拿不到插件的 stdout")?;
        if let Some(pid) = child.id() {
            self.note_ledger(pid);
        }
        Ok(Running {
            child,
            stdin,
            stdout: tokio::io::BufReader::new(stdout),
        })
    }

    /// 结束当前进程（杀树）并清空槽位与台账。
    async fn kill(&self, slot: &mut Option<Running>) {
        if let Some(mut running) = slot.take() {
            crate::proc_util::kill_tree(&mut running.child).await;
            let _ = running.child.wait().await;
        }
        self.clear_ledger();
    }
}

impl Drop for ProcessTransport {
    fn drop(&mut self) {
        // Drop 里不能 await，所以用同步的树杀。
        // `kill_on_drop(true)` 已经兜了一层，但它只杀**直接子进程** ——
        // 插件若是包装脚本（`.cmd` / `.ps1`），真正干活的是孙进程。
        if let Ok(mut slot) = self.inner.try_lock() {
            if let Some(running) = slot.take() {
                if let Some(pid) = running.child.id() {
                    crate::proc_util::kill_tree_blocking(pid);
                }
            }
            // 自己收的尾，台账要跟着清 —— 否则下次启动会去清理一个
            // 早就不存在的 pid（无害，但会让「台账里有东西」这件事失去意义）。
            drop(slot);
            self.clear_ledger();
        }
    }
}

#[async_trait::async_trait]
impl crate::agent_upstream::plugin::PluginTransport for ProcessTransport {
    async fn exchange(&self, line: &str, timeout_ms: u64) -> Result<Vec<String>, String> {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

        let request_id = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| {
                value
                    .get("request_id")
                    .and_then(|id| id.as_str())
                    .map(str::to_owned)
            })
            .ok_or("请求里没有可解析的 request_id，无法配对响应")?;
        let budget = std::time::Duration::from_millis(timeout_ms.max(1));

        let mut slot = self.inner.lock().await;
        if slot.is_none() {
            *slot = Some(self.spawn()?);
        }
        let running = slot.as_mut().ok_or("进程槽位刚刚被填进去却又空了")?;

        // 1) 写一行。
        let write = async {
            running.stdin.write_all(line.as_bytes()).await?;
            running.stdin.write_all(b"\n").await?;
            running.stdin.flush().await
        };
        match tokio::time::timeout(budget, write).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                self.kill(&mut slot).await;
                return Err(format!("写插件 stdin 失败：{e}"));
            }
            Err(_) => {
                self.kill(&mut slot).await;
                return Err(format!("把请求写进插件超时（{timeout_ms}ms）"));
            }
        }

        // 2) 读到**属于这次请求**的那一行。日志行与别的请求的响应都跳过 ——
        //    与 `pick_response` 同一个口径，只是发生在读的那一刻。
        let read = async {
            loop {
                let mut buf = String::new();
                let n = running
                    .stdout
                    .read_line(&mut buf)
                    .await
                    .map_err(|e| format!("读插件 stdout 失败：{e}"))?;
                if n == 0 {
                    return Err("插件把 stdout 关了（进程可能已经退出）".to_string());
                }
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if value.get("request_id").and_then(|id| id.as_str())
                        == Some(request_id.as_str())
                    {
                        return Ok(vec![trimmed.to_string()]);
                    }
                }
                // 不是给我们的那一行：继续读，直到超时。
            }
        };
        match tokio::time::timeout(budget, read).await {
            Ok(Ok(lines)) => Ok(lines),
            Ok(Err(error)) => {
                self.kill(&mut slot).await;
                Err(error)
            }
            Err(_) => {
                self.kill(&mut slot).await;
                Err(format!("插件在 {timeout_ms}ms 内没有回应，已终止它"))
            }
        }
    }
}
