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
