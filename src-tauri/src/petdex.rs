//! Petdex 桌宠：读取本机宠物包、监控 AI 工具进程、调用 Petdex CLI。
//!
//! 宠物包目录为 `~/.petdex/pets/<slug>/`，包含 `pet.json` 与
//! `spritesheet.webp`（或 `.png`）。精灵图约定为 8 列 × 9 行网格、
//! 单元格 192×208，每行一个动作；行号与逐帧延迟取自内置默认动画集。
//!
//! 本模块只读宠物文件；安装新宠物通过官方 `petdex` CLI 执行，
//! 命令与参数全部来自内置常量与经校验的 slug，不接受任意输入。

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use serde::Serialize;

/// 精灵图网格约定（与 Qoder 读取的宠物包保持一致）。
pub const SPRITE_COLUMNS: u32 = 8;
pub const CELL_WIDTH: u32 = 192;
pub const CELL_HEIGHT: u32 = 208;

/// 默认动画集：动作名、行号、逐帧延迟（毫秒）。
pub struct PetAnimation {
    pub name: &'static str,
    pub row: u32,
    pub delays_ms: &'static [u32],
}

pub const DEFAULT_ANIMATIONS: &[PetAnimation] = &[
    PetAnimation {
        name: "idle",
        row: 0,
        delays_ms: &[280, 110, 110, 140, 140, 320],
    },
    PetAnimation {
        name: "runningRight",
        row: 1,
        delays_ms: &[120, 120, 120, 120, 120, 120, 120, 220],
    },
    PetAnimation {
        name: "runningLeft",
        row: 2,
        delays_ms: &[120, 120, 120, 120, 120, 120, 120, 220],
    },
    PetAnimation {
        name: "waving",
        row: 3,
        delays_ms: &[140, 140, 140, 280],
    },
    PetAnimation {
        name: "jumping",
        row: 4,
        delays_ms: &[140, 140, 140, 140, 280],
    },
    PetAnimation {
        name: "failed",
        row: 5,
        delays_ms: &[140, 140, 140, 140, 140, 140, 140, 240],
    },
    PetAnimation {
        name: "waiting",
        row: 6,
        delays_ms: &[150, 150, 150, 150, 150, 260],
    },
    PetAnimation {
        name: "running",
        row: 7,
        delays_ms: &[120, 120, 120, 120, 120, 220],
    },
    PetAnimation {
        name: "review",
        row: 8,
        delays_ms: &[150, 150, 150, 150, 150, 280],
    },
];

const MAX_PACKAGE_JSON_BYTES: u64 = 64 * 1024;
const MAX_SPRITESHEET_BYTES: u64 = 16 * 1024 * 1024;
const LIST_TIMEOUT: Duration = Duration::from_secs(120);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_CHARS: usize = 4_000;

/// slug 只允许小写字母、数字、点、下划线与连字符，且必须以字母或数字开头。
pub fn is_valid_slug(slug: &str) -> bool {
    let mut chars = slug.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    slug.len() <= 128
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

pub fn pets_root() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".petdex").join("pets"))
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledPet {
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    /// 实际使用的精灵图文件名（spritesheet.webp 或 spritesheet.png）。
    pub spritesheet_file: String,
    pub directory: String,
}

/// 扫描已安装宠物。单个宠物包损坏只跳过该条目，不影响其它宠物。
pub fn list_installed() -> Vec<InstalledPet> {
    let Some(root) = pets_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut pets = Vec::new();
    for entry in entries.flatten() {
        let directory = entry.path();
        if !directory.is_dir() {
            continue;
        }
        let Some(slug) = directory.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_valid_slug(slug) {
            continue;
        }
        let Ok(meta) = read_pet_meta(&directory) else {
            continue;
        };
        let Some((spritesheet_file, _)) = spritesheet_path(&directory) else {
            continue;
        };
        pets.push(InstalledPet {
            slug: slug.to_owned(),
            display_name: meta
                .get("displayName")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(slug)
                .to_owned(),
            description: meta
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            version: meta
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            spritesheet_file,
            directory: directory.display().to_string(),
        });
    }
    pets.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    pets
}

fn read_pet_meta(directory: &std::path::Path) -> Result<serde_json::Value, String> {
    let path = directory.join("pet.json");
    let metadata =
        std::fs::metadata(&path).map_err(|error| format!("读取 pet.json 失败：{error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_PACKAGE_JSON_BYTES {
        return Err("pet.json 不是有效文件".to_string());
    }
    let raw =
        std::fs::read_to_string(&path).map_err(|error| format!("读取 pet.json 失败：{error}"))?;
    serde_json::from_str(&raw).map_err(|_| "pet.json 不是合法 JSON".to_string())
}

/// 精灵图只接受 `spritesheet.webp` 或 `spritesheet.png`，两者同时存在视为歧义。
fn spritesheet_path(directory: &std::path::Path) -> Option<(String, PathBuf)> {
    let webp = directory.join("spritesheet.webp");
    let png = directory.join("spritesheet.png");
    match (webp.is_file(), png.is_file()) {
        (true, false) => Some(("spritesheet.webp".to_string(), webp)),
        (false, true) => Some(("spritesheet.png".to_string(), png)),
        _ => None,
    }
}

/// 桌宠窗口渲染所需的宠物资源。精灵图以 data URL 返回，避免向前端暴露任意文件读取。
#[derive(Debug, Clone, Serialize)]
pub struct PetAsset {
    pub slug: String,
    pub display_name: String,
    pub columns: u32,
    pub rows: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    /// 动画集：动作名 → { 行号, 逐帧延迟 }
    pub animations: serde_json::Value,
    pub spritesheet_data_url: String,
}

pub fn load_asset(slug: &str) -> Result<PetAsset, String> {
    if !is_valid_slug(slug) {
        return Err("宠物标识不合法".to_string());
    }
    let root = pets_root().ok_or_else(|| "找不到用户目录".to_string())?;
    let directory = root.join(slug);
    if !directory.is_dir() {
        return Err(format!("宠物 {slug} 未安装"));
    }
    let meta = read_pet_meta(&directory)?;
    let (spritesheet_file, spritesheet_path) = spritesheet_path(&directory)
        .ok_or_else(|| "宠物包缺少 spritesheet.webp 或 spritesheet.png".to_string())?;
    let metadata = std::fs::metadata(&spritesheet_path)
        .map_err(|error| format!("读取 {spritesheet_file} 失败：{error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_SPRITESHEET_BYTES {
        return Err(format!("{spritesheet_file} 不是有效文件或超过大小上限"));
    }
    let bytes = std::fs::read(&spritesheet_path)
        .map_err(|error| format!("读取 {spritesheet_file} 失败：{error}"))?;
    let mime = if spritesheet_file.ends_with(".png") {
        "image/png"
    } else {
        "image/webp"
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let animations = DEFAULT_ANIMATIONS
        .iter()
        .map(|animation| {
            (
                animation.name.to_string(),
                serde_json::json!({ "row": animation.row, "delays_ms": animation.delays_ms }),
            )
        })
        .collect::<serde_json::Map<_, _>>();

    Ok(PetAsset {
        slug: slug.to_owned(),
        display_name: meta
            .get("displayName")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(slug)
            .to_owned(),
        columns: SPRITE_COLUMNS,
        rows: DEFAULT_ANIMATIONS.len() as u32,
        cell_width: CELL_WIDTH,
        cell_height: CELL_HEIGHT,
        animations: serde_json::Value::Object(animations),
        spritesheet_data_url: format!("data:{mime};base64,{encoded}"),
    })
}

/// 由网关最近活动推导桌宠状态与原因（纯函数，便于断言）。
///
/// - 最近一条请求失败且在观察窗口内 → `error`（失败后还没有新的成功请求）
/// - 窗口内有请求 → `working`
/// - 其余 → `idle`
pub fn derive_activity_status(
    total_in_window: i64,
    last_request: Option<(i64, Option<i64>, Option<String>)>,
    since: i64,
    window_secs: i64,
) -> (String, String) {
    let (mut status, mut reason) = if total_in_window > 0 {
        (
            "working".to_string(),
            format!("最近 {window_secs} 秒内处理了 {total_in_window} 个请求"),
        )
    } else {
        ("idle".to_string(), "网关当前空闲".to_string())
    };
    if let Some((ts, status_code, error)) = last_request {
        let is_failure = status_code.map_or(true, |code| code >= 400);
        if ts >= since && is_failure {
            status = "error".to_string();
            reason = format!(
                "最近一次请求失败：{}",
                error.unwrap_or_else(|| "未知错误".to_string())
            );
        }
    }
    (status, reason)
}

/// 只用于监控的 AI 桌面应用（不属于 CLI 安装清单，因此单独维护）。
/// 进程名不含 `.exe`，大小写不敏感；匹配不到只是不显示，不会误报。
pub struct AiDesktopApp {
    pub id: &'static str,
    pub label: &'static str,
    pub processes: &'static [&'static str],
}

pub const AI_DESKTOP_APPS: &[AiDesktopApp] = &[
    AiDesktopApp {
        id: "qoder_ide",
        label: "Qoder IDE",
        // 含国内版主进程与随附的 computer-use 组件（本机实测进程名）。
        processes: &["Qoder", "Qoder CN", "QoderComputerUse"],
    },
    AiDesktopApp {
        id: "cursor_ide",
        label: "Cursor",
        processes: &["Cursor"],
    },
    AiDesktopApp {
        id: "trae_ide",
        label: "TRAE",
        processes: &["Trae", "Trae CN"],
    },
    AiDesktopApp {
        id: "windsurf",
        label: "Windsurf",
        processes: &["Windsurf"],
    },
    AiDesktopApp {
        id: "claude_desktop",
        label: "Claude Desktop",
        processes: &["Claude"],
    },
    AiDesktopApp {
        id: "yuanbao",
        label: "腾讯元宝",
        processes: &["yuanbao"],
    },
];

/// 一个被检测到的 AI 任务（来自工具自身的会话日志）。
#[derive(Debug, Clone, Serialize)]
pub struct DetectedTask {
    /// 数据来源标识，例如 "qoder_cli"。
    pub source: String,
    pub source_label: String,
    /// 会话所属项目（由日志目录还原为可读路径）。
    pub project: String,
    pub session_id: String,
    /// "running" | "error" | "done"
    pub status: String,
    /// 最近事件的说明（供界面直接展示）。
    pub detail: String,
    /// 最后事件时间（unix 秒）。
    pub updated_at: i64,
}

/// 只关注最近 15 分钟内仍有写入的任务日志。
const TASK_ACTIVE_WINDOW_SECS: i64 = 15 * 60;
/// 每个日志只读末尾这一段，避免大文件拖慢轮询。
const TASK_TAIL_BYTES: u64 = 16 * 1024;

fn qoder_sessions_root() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".qoder").join("logs").join("sessions"))
}

/// 把 `C--Users-a1740` 还原为 `C:\Users\a1740`（日志目录的编码约定：
/// 盘符后紧跟的 `:` 与首个 `\` 各编码为一个 `-`，其余 `\` 同样编码为 `-`）。
fn decode_project_dir(name: &str) -> String {
    if name.len() >= 2 && name.as_bytes()[1] == b'-' {
        let drive = &name[..1];
        let rest = name[2..].replace('-', "\\");
        format!("{drive}:{rest}")
    } else {
        name.to_string()
    }
}

/// 扫描 Qoder CLI 的会话日志，得到最近的任务状态。
pub fn list_qoder_tasks() -> Vec<DetectedTask> {
    let now = chrono::Utc::now().timestamp();
    match qoder_sessions_root() {
        Some(root) => list_tasks_in(&root, now, TASK_ACTIVE_WINDOW_SECS),
        None => Vec::new(),
    }
}

/// 在给定根目录下扫描任务日志（根目录与时间窗口可注入，便于测试）。
/// 数据源：`<root>/<project>/<session>/segments/*.jsonl`，
/// 每行一个事件（phase.started / phase.finished / level=error 等）。
pub fn list_tasks_in(root: &std::path::Path, now: i64, window_secs: i64) -> Vec<DetectedTask> {
    let Ok(projects) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut tasks = Vec::new();

    for project in projects.flatten() {
        let project_dir = project.path();
        if !project_dir.is_dir() {
            continue;
        }
        let project_name = project_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let Ok(sessions) = std::fs::read_dir(&project_dir) else {
            continue;
        };
        for session in sessions.flatten() {
            let segments = session.path().join("segments");
            let Ok(files) = std::fs::read_dir(&segments) else {
                continue;
            };
            // 每个会话只看最新写入的一个 segment 文件。
            let newest = files
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
                .filter_map(|entry| {
                    let modified = entry.metadata().ok()?.modified().ok()?;
                    let secs = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()?
                        .as_secs() as i64;
                    Some((secs, entry.path()))
                })
                .max_by_key(|(secs, _)| *secs);
            let Some((mtime, path)) = newest else {
                continue;
            };
            if now - mtime > window_secs {
                continue;
            }
            let Some((status, detail, updated_at)) = read_last_task_event(&path) else {
                continue;
            };
            tasks.push(DetectedTask {
                source: "qoder_cli".to_string(),
                source_label: "Qoder CLI".to_string(),
                project: decode_project_dir(&project_name),
                session_id: session.file_name().to_string_lossy().into_owned(),
                status,
                detail,
                updated_at: updated_at.unwrap_or(mtime),
            });
        }
    }

    tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_at));
    tasks
}

/// 读取日志末尾的事件，推导任务状态。返回 (状态, 说明, 事件时间)。
fn read_last_task_event(path: &std::path::Path) -> Option<(String, String, Option<i64>)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(TASK_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buffer = String::new();
    file.read_to_string(&mut buffer).ok()?;
    // 首个不完整行直接丢弃，避免解析半行 JSON；从后往前找最后一个可解析事件。
    let mut lines = buffer.lines();
    if start > 0 {
        lines.next();
    }
    lines.filter_map(parse_task_event).next_back()
}

/// 解析一行事件为 (状态, 说明, 时间)。无法识别的事件返回 None。
fn parse_task_event(line: &str) -> Option<(String, String, Option<i64>)> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let event_type = value.get("type").and_then(serde_json::Value::as_str)?;
    let level = value
        .get("level")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("info");
    let data = value
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let phase = data
        .get("phase")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let ts = value
        .get("ts")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.timestamp());
    let finished_ok = data
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let (status, detail) = match event_type {
        _ if level == "error" => (
            "error".to_string(),
            format!("出错：{}", summarize_event(event_type, &data)),
        ),
        "session.phase.finished" if !finished_ok => {
            ("error".to_string(), format!("阶段失败：{phase}"))
        }
        "session.phase.started" => ("running".to_string(), format!("进行中：{phase}")),
        "session.phase.finished" => ("done".to_string(), format!("已完成：{phase}")),
        _ => ("running".to_string(), summarize_event(event_type, &data)),
    };
    Some((status, detail, ts))
}

/// 把事件压缩成一句可读说明（只取最常见的几个字段）。
fn summarize_event(event_type: &str, data: &serde_json::Value) -> String {
    for key in ["message", "error", "reason", "phase"] {
        if let Some(value) = data.get(key).and_then(serde_json::Value::as_str) {
            if !value.is_empty() {
                return format!("{event_type}：{}", truncate(value, 120));
            }
        }
    }
    event_type.to_string()
}

/// 综合网关活动与检测到的任务，得到桌宠状态与原因。
/// 优先级：错误（网关或任务）> 工作中（网关请求或任务运行）> 空闲。
pub fn combine_pet_status(
    gateway_status: &str,
    gateway_reason: &str,
    tasks: &[DetectedTask],
) -> (String, String) {
    if gateway_status == "error" {
        return ("error".to_string(), gateway_reason.to_string());
    }
    if let Some(task) = tasks.iter().find(|task| task.status == "error") {
        return (
            "error".to_string(),
            format!("{} 任务出错：{}", task.source_label, task.detail),
        );
    }
    if gateway_status == "working" {
        return ("working".to_string(), gateway_reason.to_string());
    }
    if let Some(task) = tasks.iter().find(|task| task.status == "running") {
        return (
            "working".to_string(),
            format!("{} 任务{}", task.source_label, task.detail),
        );
    }
    ("idle".to_string(), gateway_reason.to_string())
}

/// 一个正在运行的、属于受支持 AI 工具的进程。
#[derive(Debug, Clone, Serialize)]
pub struct AiProcess {
    pub tool_id: String,
    pub tool_label: String,
    /// "cli"（编码 CLI）或 "app"（桌面应用）。
    pub kind: String,
    pub process_name: String,
    pub pid: u32,
    pub memory_kb: Option<u64>,
}

/// 在 CLI 工具清单与 AI 桌面应用表中查找进程名对应的条目。
fn match_process(stem: &str) -> Option<(String, String, &'static str)> {
    if let Some(spec) = crate::cli_tools::TOOLS.iter().find(|spec| {
        spec.binaries
            .iter()
            .any(|binary| binary.eq_ignore_ascii_case(stem))
    }) {
        return Some((spec.id.to_owned(), spec.label.to_owned(), "cli"));
    }
    if let Some(app) = AI_DESKTOP_APPS.iter().find(|app| {
        app.processes
            .iter()
            .any(|process| process.eq_ignore_ascii_case(stem))
    }) {
        return Some((app.id.to_owned(), app.label.to_owned(), "app"));
    }
    None
}

/// 枚举本机进程并匹配受支持的 AI 工具。Windows 用 `tasklist`，其它平台返回空。
pub async fn list_ai_processes() -> Vec<AiProcess> {
    #[cfg(not(windows))]
    {
        Vec::new()
    }
    #[cfg(windows)]
    {
        let Ok((stdout, _)) =
            crate::cli_tools::run("tasklist", &["/FO", "CSV", "/NH"], PROCESS_TIMEOUT).await
        else {
            return Vec::new();
        };
        let mut processes = Vec::new();
        for line in stdout.lines() {
            let fields = parse_csv_line(line);
            if fields.len() < 5 {
                continue;
            }
            let process_name = fields[0].trim();
            let Ok(pid) = fields[1].trim().parse::<u32>() else {
                continue;
            };
            let stem = process_name
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(process_name);
            let Some((tool_id, tool_label, kind)) = match_process(stem) else {
                continue;
            };
            let memory_kb = fields[4]
                .trim()
                .trim_end_matches([' ', 'K', 'k'])
                .replace(',', "")
                .parse::<u64>()
                .ok();
            processes.push(AiProcess {
                tool_id,
                tool_label,
                kind: kind.to_owned(),
                process_name: process_name.to_owned(),
                pid,
                memory_kb,
            });
        }
        // 同名多进程（如 QoderComputerUse）按工具聚合排序，便于阅读。
        processes.sort_by(|a, b| {
            a.tool_label
                .cmp(&b.tool_label)
                .then_with(|| a.process_name.cmp(&b.process_name))
                .then_with(|| a.pid.cmp(&b.pid))
        });
        processes
    }
}

/// 解析 tasklist 的 CSV 行（字段用双引号包裹，内存列含千位逗号）。
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in line.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    fields.push(current);
    fields
}

/// 结束某个工具/应用当前检测到的全部进程。
/// 只结束本次枚举中匹配到该 tool_id 的进程，且排除本应用自身；
/// 返回逐项结果，失败项如实列出而不是整体报告成功。
pub async fn stop_tool_processes(tool_id: &str) -> Result<String, String> {
    let processes = list_ai_processes().await;
    let targets: Vec<&AiProcess> = processes
        .iter()
        .filter(|process| process.tool_id == tool_id)
        .collect();
    let Some(first) = targets.first() else {
        return Err("该软件当前没有检测到运行中的进程；请先刷新监控".to_string());
    };
    let label = first.tool_label.clone();
    let own_pid = std::process::id();

    let mut killed = Vec::new();
    let mut failures = Vec::new();
    for target in targets {
        if target.pid == own_pid {
            failures.push(format!(
                "{} (PID {})：拒绝对本应用自身执行",
                target.process_name, target.pid
            ));
            continue;
        }
        let pid_text = target.pid.to_string();
        match crate::cli_tools::run(
            "taskkill",
            &["/PID", &pid_text, "/T", "/F"],
            PROCESS_TIMEOUT,
        )
        .await
        {
            Ok(_) => killed.push(format!("{} (PID {})", target.process_name, target.pid)),
            Err(error) => failures.push(format!(
                "{} (PID {}): {error}",
                target.process_name, target.pid
            )),
        }
    }

    let mut summary = format!(
        "已结束 {label} 的 {} 个进程：{}",
        killed.len(),
        killed.join("、")
    );
    if !failures.is_empty() {
        summary.push_str(&format!(
            "；{} 个失败：{}",
            failures.len(),
            failures.join("；")
        ));
    }
    Ok(summary)
}

/// 查询 Petdex 商店列表。返回 CLI 的原始输出，由界面呈现给用户。
pub async fn petdex_list() -> Result<String, String> {
    let npx = resolve_npx()?;
    let (stdout, stderr) =
        crate::cli_tools::run_path(&npx, &["--yes", "petdex@latest", "list"], LIST_TIMEOUT).await?;
    let combined = format!("{stdout}\n{stderr}");
    Ok(truncate(combined.trim(), MAX_OUTPUT_CHARS))
}

/// 通过官方 Petdex CLI 安装一个宠物。slug 经白名单校验后作为独立参数传递，
/// 不拼接任何 shell 语法。
pub async fn petdex_install(slug: &str) -> Result<String, String> {
    if !is_valid_slug(slug) {
        return Err("宠物标识不合法：只允许小写字母、数字、点、下划线与连字符".to_string());
    }
    let npx = resolve_npx()?;
    let (stdout, stderr) = crate::cli_tools::run_path(
        &npx,
        &["--yes", "petdex@latest", "install", slug],
        INSTALL_TIMEOUT,
    )
    .await?;
    let combined = format!("{stdout}\n{stderr}");
    // 安装后必须能在本机目录中看到该宠物，否则如实报告失败。
    let installed = list_installed().iter().any(|pet| pet.slug == slug);
    if !installed {
        return Err(format!(
            "安装命令已执行，但 ~/.petdex/pets/{slug} 仍不可用。输出：{}",
            truncate(combined.trim(), 600)
        ));
    }
    Ok(truncate(combined.trim(), MAX_OUTPUT_CHARS))
}

fn resolve_npx() -> Result<PathBuf, String> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    crate::cli_tools::resolve_binary(&["npx"], &path_env, &crate::cli_tools::well_known_dirs())
        .ok_or_else(|| {
            "未找到 npx。请先安装 Node.js，或按 Petdex 官方文档手动安装宠物。".to_string()
        })
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation_rejects_path_traversal_and_uppercase() {
        assert!(is_valid_slug("snow-plum-lillia"));
        assert!(is_valid_slug("pet.v2_test"));
        assert!(!is_valid_slug("../escape"));
        assert!(!is_valid_slug("Pets"));
        assert!(!is_valid_slug("-leading"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("a/b"));
    }

    #[test]
    fn activity_status_covers_idle_working_and_error_branches() {
        // 无活动 → 空闲
        let (status, _) = derive_activity_status(0, None, 1_000, 60);
        assert_eq!(status, "idle");

        // 窗口内有成功请求 → 工作中
        let (status, reason) = derive_activity_status(3, Some((1_050, Some(200), None)), 1_000, 60);
        assert_eq!(status, "working");
        assert!(reason.contains("3 个请求"), "{reason}");

        // 最近一条失败且在窗口内 → 出错（即使窗口内还有其它成功请求）
        let (status, reason) = derive_activity_status(
            3,
            Some((1_050, Some(502), Some("all_failed".into()))),
            1_000,
            60,
        );
        assert_eq!(status, "error");
        assert!(reason.contains("all_failed"), "{reason}");

        // 失败发生在窗口之外 → 不再误报出错
        let (status, _) = derive_activity_status(0, Some((900, Some(502), None)), 1_000, 60);
        assert_eq!(status, "idle");

        // 状态缺失（进程内异常路径）也按失败处理，不伪装成成功
        let (status, _) = derive_activity_status(1, Some((1_050, None, None)), 1_000, 60);
        assert_eq!(status, "error");
    }

    #[test]
    fn task_event_parsing_maps_phase_lifecycle_to_status() {
        // phase.started → running（带阶段名）
        let (status, detail, ts) = parse_task_event(
            r#"{"ts":"2026-09-10T20:52:26.459+08:00","seq":11,"level":"info","type":"session.phase.started","data":{"phase":"mcp_context.refresh"}}"#,
        )
        .unwrap();
        assert_eq!(status, "running");
        assert!(detail.contains("mcp_context.refresh"), "{detail}");
        assert!(ts.is_some(), "事件时间必须被解析");

        // phase.finished success=true → done
        let (status, _, _) = parse_task_event(
            r#"{"level":"info","type":"session.phase.finished","data":{"phase":"x","success":true}}"#,
        )
        .unwrap();
        assert_eq!(status, "done");

        // phase.finished success=false → error（不能当作已完成）
        let (status, detail, _) = parse_task_event(
            r#"{"level":"info","type":"session.phase.finished","data":{"phase":"edit","success":false}}"#,
        )
        .unwrap();
        assert_eq!(status, "error");
        assert!(detail.contains("edit"), "{detail}");

        // level=error → error（带消息）
        let (status, detail, _) = parse_task_event(
            r#"{"level":"error","type":"session.failed","data":{"message":"boom"}}"#,
        )
        .unwrap();
        assert_eq!(status, "error");
        assert!(detail.contains("boom"), "{detail}");

        // 无法识别的行必须被跳过，而不是猜一个状态
        assert!(parse_task_event("not json").is_none());
        assert!(parse_task_event(r#"{"level":"info"}"#).is_none());
    }

    #[test]
    fn combine_pet_status_prioritizes_errors_then_running() {
        let running = DetectedTask {
            source: "qoder_cli".into(),
            source_label: "Qoder CLI".into(),
            project: "C:\\work".into(),
            session_id: "s1".into(),
            status: "running".into(),
            detail: "进行中：edit".into(),
            updated_at: 0,
        };
        let failed = DetectedTask {
            status: "error".into(),
            detail: "阶段失败：edit".into(),
            ..running.clone()
        };

        // 网关空闲 + 任务运行中 → working（任务驱动宠物动作）
        let (status, reason) =
            combine_pet_status("idle", "网关当前空闲", std::slice::from_ref(&running));
        assert_eq!(status, "working");
        assert!(reason.contains("Qoder CLI"), "{reason}");

        // 任务出错压过网关的工作中
        let (status, _) = combine_pet_status(
            "working",
            "最近 60 秒内处理了 1 个请求",
            &[running.clone(), failed.clone()],
        );
        assert_eq!(status, "error");

        // 网关自身出错优先于任务
        let (status, reason) =
            combine_pet_status("error", "网关错误", std::slice::from_ref(&running));
        assert_eq!(status, "error");
        assert_eq!(reason, "网关错误");

        // 已完成的任务不驱动状态
        let done = DetectedTask {
            status: "done".into(),
            ..running.clone()
        };
        let (status, _) = combine_pet_status("idle", "网关当前空闲", &[done]);
        assert_eq!(status, "idle");
    }

    #[test]
    fn project_dir_decoding_restores_drive_letter() {
        assert_eq!(decode_project_dir("C--Users-a1740"), "C:\\Users\\a1740");
        // 目录名编码有损（连字符与反斜杠同形），这里只保证盘符与分隔形态正确。
        assert_eq!(
            decode_project_dir("D--Java-GitHub-llm-auto"),
            "D:\\Java\\GitHub\\llm\\auto"
        );
        assert_eq!(decode_project_dir("plain-name"), "plain-name");
    }

    #[test]
    fn task_scan_reads_latest_segment_and_filters_stale_logs() {
        let root = std::env::temp_dir().join(format!("qoder-tasks-{}", uuid::Uuid::new_v4()));
        let segments = root
            .join("C--Users-fixture")
            .join("sess-abc")
            .join("segments");
        std::fs::create_dir_all(&segments).unwrap();
        std::fs::write(
            segments.join("2026-01-01T00-00-00-000+08-00-aaa-p1.jsonl"),
            concat!(
                "{\"ts\":\"2026-01-01T00:00:01+08:00\",\"level\":\"info\",\"type\":\"session.config.loaded\",\"data\":{}}\n",
                "{\"ts\":\"2026-01-01T00:00:02+08:00\",\"level\":\"info\",\"type\":\"session.phase.started\",\"data\":{\"phase\":\"edit\"}}\n",
            ),
        )
        .unwrap();

        let now = chrono::Utc::now().timestamp();
        // 窗口内：命中 1 个进行中的任务，字段完整。
        let tasks = list_tasks_in(&root, now, 60);
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        assert_eq!(tasks[0].status, "running");
        assert!(tasks[0].detail.contains("edit"), "{:?}", tasks[0]);
        assert_eq!(tasks[0].project, "C:\\Users\\fixture");
        assert_eq!(tasks[0].session_id, "sess-abc");

        // 窗口外：同一份日志不得再被当作"进行中"（避免过期任务驱动宠物动作）。
        let stale = list_tasks_in(&root, now + 3_600, 60);
        assert!(stale.is_empty(), "{stale:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn process_matching_covers_cli_and_desktop_apps() {
        // CLI 名来自工具清单（单一来源），大小写不敏感。
        let (id, label, kind) = match_process("codex").unwrap();
        assert_eq!(id, "codex");
        assert_eq!(kind, "cli");
        assert!(label.contains("Codex"), "{label}");
        assert_eq!(
            match_process("CODEX").map(|m| m.0),
            Some("codex".to_string())
        );

        // 桌面应用走独立表；Qoder 国内版进程名来自本机实测。
        let (id, _, kind) = match_process("Qoder CN").unwrap();
        assert_eq!(id, "qoder_ide");
        assert_eq!(kind, "app");
        assert_eq!(
            match_process("QoderComputerUse").map(|m| m.0),
            Some("qoder_ide".to_string())
        );
        assert_eq!(
            match_process("Cursor").map(|m| m.0),
            Some("cursor_ide".to_string())
        );
        assert_eq!(
            match_process("Trae CN").map(|m| m.0),
            Some("trae_ide".to_string())
        );
        assert_eq!(
            match_process("yuanbao").map(|m| m.0),
            Some("yuanbao".to_string())
        );

        // 无关进程不得被匹配：误报会让「结束进程」指向错误的进程。
        assert!(match_process("explorer").is_none());
        assert!(match_process("chrome").is_none());
        assert!(match_process("QoderExtra").is_none());
    }

    #[test]
    fn csv_parsing_handles_quoted_memory_values() {
        let fields = parse_csv_line("\"codex.exe\",\"12345\",\"Console\",\"1\",\"56,380 K\"");
        assert_eq!(fields[0], "codex.exe");
        assert_eq!(fields[1], "12345");
        assert_eq!(fields[4], "56,380 K");
        assert_eq!(
            fields[4]
                .trim()
                .trim_end_matches([' ', 'K', 'k'])
                .replace(',', ""),
            "56380"
        );
    }

    #[test]
    fn default_animation_set_covers_all_nine_rows_in_order() {
        assert_eq!(DEFAULT_ANIMATIONS.len(), 9);
        for (index, animation) in DEFAULT_ANIMATIONS.iter().enumerate() {
            assert_eq!(animation.row as usize, index, "行号必须与顺序一致");
            assert!(!animation.delays_ms.is_empty());
            assert!(animation.delays_ms.len() <= SPRITE_COLUMNS as usize);
        }
        // 网格必须能容纳所有动作行：8 列 × 9 行（与宠物包约定一致）。
        assert_eq!(DEFAULT_ANIMATIONS.len() as u32, 9);
    }

    #[test]
    fn load_asset_rejects_invalid_slug_before_touching_disk() {
        let error = load_asset("../etc").unwrap_err();
        assert!(error.contains("不合法"), "{error}");
    }
}
