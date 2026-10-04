//! Petdex 桌宠：读取本机宠物包、监控 AI 工具进程、调用 Petdex CLI。
//!
//! 宠物包目录为 `~/.petdex/pets/<slug>/`，包含 `pet.json` 与
//! `spritesheet.webp`（或 `.png`）。精灵图约定为 8 列 × 9 行网格、
//! 单元格 192×208，每行一个动作；行号与逐帧延迟取自内置默认动画集。
//!
//! 本模块只读宠物文件；安装新宠物通过官方 `petdex` CLI 执行，
//! 命令与参数全部来自内置常量与经校验的 slug，不接受任意输入。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
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
    // AI 编码 IDE / 桌面 Agent
    AiDesktopApp {
        id: "codex_desktop",
        label: "Codex Desktop",
        processes: &["ChatGPT"],
    },
    AiDesktopApp {
        id: "qoder_ide",
        label: "Qoder IDE",
        processes: &["Qoder", "Qoder CN", "Qoder IDE", "QoderComputerUse"],
    },
    AiDesktopApp {
        id: "cursor_ide",
        label: "Cursor",
        processes: &["Cursor"],
    },
    AiDesktopApp {
        id: "trae_ide",
        label: "TRAE",
        processes: &[
            "Trae",
            "Trae CN",
            "TRAE SOLO CN",
            "TRAE Work CN",
            "MarsCode",
        ],
    },
    AiDesktopApp {
        id: "windsurf",
        label: "Windsurf",
        processes: &["Windsurf"],
    },
    AiDesktopApp {
        id: "kiro",
        label: "Kiro",
        processes: &["Kiro"],
    },
    AiDesktopApp {
        id: "void_editor",
        label: "Void",
        processes: &["Void"],
    },
    AiDesktopApp {
        id: "zed_editor",
        label: "Zed",
        processes: &["Zed"],
    },
    AiDesktopApp {
        id: "claude_desktop",
        label: "Claude Desktop",
        processes: &["Claude"],
    },
    AiDesktopApp {
        id: "codebuddy",
        label: "CodeBuddy",
        processes: &["CodeBuddy"],
    },
    AiDesktopApp {
        id: "comate",
        label: "Baidu Comate",
        processes: &["Comate"],
    },
    AiDesktopApp {
        id: "lingma",
        label: "通义灵码",
        processes: &["Lingma"],
    },
    AiDesktopApp {
        id: "tabnine",
        label: "Tabnine",
        processes: &["TabNine"],
    },
    AiDesktopApp {
        id: "eigent",
        label: "Eigent",
        processes: &["Eigent"],
    },
    AiDesktopApp {
        id: "hermes_studio",
        label: "Hermes Studio",
        processes: &["Hermes Studio"],
    },
    AiDesktopApp {
        id: "reasonix",
        label: "Reasonix",
        processes: &["Reasonix"],
    },
    AiDesktopApp {
        id: "aingdesk",
        label: "AingDesk",
        processes: &["AingDesk"],
    },
    AiDesktopApp {
        id: "hyperchat",
        label: "HyperChat",
        processes: &["HyperChat"],
    },
    AiDesktopApp {
        id: "chatall",
        label: "ChatALL",
        processes: &["ChatALL"],
    },
    AiDesktopApp {
        id: "five_ire",
        label: "5ire",
        processes: &["5ire"],
    },
    // AI 助手 / 聊天客户端
    AiDesktopApp {
        id: "perplexity",
        label: "Perplexity",
        processes: &["Perplexity"],
    },
    AiDesktopApp {
        id: "doubao",
        label: "豆包",
        processes: &["Doubao"],
    },
    AiDesktopApp {
        id: "kimi",
        label: "Kimi",
        processes: &["Kimi", "Kimi智能助手"],
    },
    AiDesktopApp {
        id: "yuanbao",
        label: "腾讯元宝",
        processes: &["yuanbao"],
    },
    AiDesktopApp {
        id: "qianwen",
        label: "千问",
        processes: &["Qianwen"],
    },
    AiDesktopApp {
        id: "nami_ai",
        label: "纳米AI",
        processes: &["NamiAI"],
    },
    AiDesktopApp {
        id: "chatglm",
        label: "智谱清言",
        processes: &["ChatGLM"],
    },
    AiDesktopApp {
        id: "monica",
        label: "Monica",
        processes: &["Monica"],
    },
    AiDesktopApp {
        id: "chatbox",
        label: "Chatbox",
        processes: &["Chatbox"],
    },
    AiDesktopApp {
        id: "lobehub",
        label: "LobeHub",
        processes: &["LobeHub"],
    },
    AiDesktopApp {
        id: "witsy",
        label: "Witsy",
        processes: &["Witsy"],
    },
    AiDesktopApp {
        id: "chatwise",
        label: "ChatWise",
        processes: &["ChatWise"],
    },
    AiDesktopApp {
        id: "deepchat",
        label: "DeepChat",
        processes: &["DeepChat"],
    },
    // 本地模型 / 多模型工作台
    AiDesktopApp {
        id: "msty",
        label: "Msty",
        processes: &["Msty", "Msty Studio"],
    },
    AiDesktopApp {
        id: "jan",
        label: "Jan",
        processes: &["Jan"],
    },
    AiDesktopApp {
        id: "gpt4all",
        label: "GPT4All",
        processes: &["GPT4All"],
    },
    AiDesktopApp {
        id: "anythingllm",
        label: "AnythingLLM",
        processes: &["AnythingLLM"],
    },
    AiDesktopApp {
        id: "lm_studio",
        label: "LM Studio",
        processes: &["LM Studio"],
    },
    AiDesktopApp {
        id: "ollama",
        label: "Ollama",
        processes: &["Ollama app"],
    },
    AiDesktopApp {
        id: "koboldcpp",
        label: "KoboldCpp",
        processes: &["koboldcpp"],
    },
    AiDesktopApp {
        id: "backyard_ai",
        label: "Backyard AI",
        processes: &["Backyard AI", "BackyardAI"],
    },
    AiDesktopApp {
        id: "cherry_studio",
        label: "Cherry Studio",
        processes: &["Cherry Studio"],
    },
    AiDesktopApp {
        id: "chatless",
        label: "Chatless",
        processes: &["Chatless"],
    },
    AiDesktopApp {
        id: "kelivo",
        label: "Kelivo",
        processes: &["Kelivo"],
    },
    AiDesktopApp {
        id: "nekot",
        label: "Nekot",
        processes: &["Nekot"],
    },
    AiDesktopApp {
        id: "neatchat",
        label: "NeatChat",
        processes: &["NeatChat"],
    },
];

/// 一个被检测到的 AI 任务（来自工具自身的会话日志）。
#[derive(Debug, Clone, Serialize)]
pub struct DetectedTask {
    /// 可执行操作对应的受监控工具标识（例如 "codex" / "qoder"）。
    pub tool_id: String,
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
    /// 从用户消息/会话 Recap 提取的具体任务标题。
    pub title: String,
    /// 最近的助手消息或任务 Recap，供详情面板展示。
    pub last_message: String,
    /// 官方任务级深链；没有可靠协议时为 None，不伪造入口。
    pub deep_link: Option<String>,
    /// 最后事件时间（unix 秒）。
    pub updated_at: i64,
}

/// 只关注最近 15 分钟内仍有写入的任务日志。
const TASK_ACTIVE_WINDOW_SECS: i64 = 15 * 60;
/// 候选文件的 mtime 粗筛范围：Windows 上持续写入的 rollout 有时 mtime 会滞后，
/// 所以先放宽到 6 小时，随后再用文件内的 `timestamp` 精确判断活跃度。
const TASK_SCAN_WINDOW_SECS: i64 = 6 * 60 * 60;
/// 运行中的任务（还没有 task_complete）保留更久，避免等待用户输入的任务消失。
const TASK_RUNNING_WINDOW_SECS: i64 = 6 * 60 * 60;
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

/// Codex rollout 根目录（Codex CLI 与 Codex Desktop 共用）。
fn codex_sessions_root() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codex").join("sessions"))
}

/// Codex 会话索引（`~/.codex/session_index.jsonl`）里的显示名，与桌面端侧边栏一致。
fn codex_session_names() -> std::collections::HashMap<String, String> {
    match dirs::home_dir() {
        Some(home) => codex_session_names_in(&home.join(".codex").join("session_index.jsonl")),
        None => std::collections::HashMap::new(),
    }
}

/// 索引按行追加，同一会话重复出现时以最后一条为准。
fn codex_session_names_in(path: &Path) -> std::collections::HashMap<String, String> {
    let mut names = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return names;
    };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(name) = value.get("thread_name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let name = clean_preview_text(name);
        if name.is_empty() {
            continue;
        }
        names.insert(id.to_string(), truncate(&name, 64));
    }
    names
}

#[derive(Debug)]
struct CodexSessionMeta {
    tool_id: String,
    source_label: String,
    project: String,
    session_id: String,
}

/// 扫描 Codex rollout，得到最近写入的会话状态。
pub fn list_codex_tasks() -> Vec<DetectedTask> {
    let now = chrono::Utc::now().timestamp();
    match codex_sessions_root() {
        Some(root) => list_codex_tasks_in(&root, now, TASK_ACTIVE_WINDOW_SECS),
        None => Vec::new(),
    }
}

fn list_codex_tasks_in(root: &Path, now: i64, window_secs: i64) -> Vec<DetectedTask> {
    list_codex_tasks_with_names(root, now, window_secs, &codex_session_names())
}

/// 任务标题优先复用 Codex 会话索引里的显示名（桌面端侧边栏显示的就是它），
/// 索引缺失时才退回日志正文提取，避免把附件包装文案当成任务标题。
fn list_codex_tasks_with_names(
    root: &Path,
    now: i64,
    window_secs: i64,
    session_names: &std::collections::HashMap<String, String>,
) -> Vec<DetectedTask> {
    let mut files = Vec::new();
    // mtime 粗筛放宽，活跃度以文件内的 timestamp（内容时间）为准。
    collect_recent_jsonl(
        root,
        now,
        window_secs.max(TASK_SCAN_WINDOW_SECS),
        &mut files,
    );
    files.sort_by_key(|item| std::cmp::Reverse(item.0));

    // 同一逻辑会话可能因为分页/续写产生多个 rollout；保留更新时间最新的一份。
    let mut by_session: std::collections::HashMap<String, DetectedTask> =
        std::collections::HashMap::new();
    for (mtime, path) in files.into_iter().take(80) {
        let Some(meta) = read_codex_session_meta(&path) else {
            continue;
        };
        // Windows 上长期持有句柄的 rollout 可能几十分钟不更新 mtime，
        // 用文件内最后一个 timestamp 作为活跃时间，否则会漏掉正在跑的任务。
        let content_at = read_last_content_timestamp(&path);
        let Some((status, detail, event_at)) =
            read_codex_task_state(&path, content_at.unwrap_or(mtime))
        else {
            continue;
        };
        // 活跃时间取三者最大值：生命周期事件时间、文件内容时间、文件 mtime。
        // 任何一个偏旧（例如写入方不刷新 mtime）都不会把仍在跑的任务判成过期。
        let updated_at = [event_at, content_at, Some(mtime)]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(mtime);
        let active_window = if status == "running" {
            TASK_RUNNING_WINDOW_SECS
        } else {
            window_secs
        };
        if now - updated_at > active_window {
            continue;
        }
        let content = read_codex_task_content(&path);
        let title = match session_names.get(&meta.session_id) {
            Some(name) if !name.is_empty() => name.clone(),
            _ if content.title.is_empty() => detail.clone(),
            _ => content.title,
        };
        let last_message = if content.last_message.is_empty() {
            detail.clone()
        } else {
            content.last_message
        };
        let deep_link = codex_deep_link(&meta.tool_id, &meta.session_id);
        let task = DetectedTask {
            tool_id: meta.tool_id,
            source: "codex".to_string(),
            source_label: meta.source_label,
            project: meta.project,
            session_id: meta.session_id,
            status,
            detail,
            title,
            last_message,
            deep_link,
            updated_at,
        };
        match by_session.get(&task.session_id) {
            Some(existing) if existing.updated_at >= task.updated_at => {}
            _ => {
                by_session.insert(task.session_id.clone(), task);
            }
        }
    }

    let mut tasks: Vec<DetectedTask> = by_session.into_values().collect();
    tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_at));
    tasks
}

/// 只收集最近窗口内修改过的 JSONL；目录递归不跟随符号链接。
fn collect_recent_jsonl(dir: &Path, now: i64, window_secs: i64, out: &mut Vec<(i64, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_recent_jsonl(&path, now, window_secs, out);
            continue;
        }
        if !file_type.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("jsonl")
        {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_secs() as i64)
            .unwrap_or(0);
        if now - mtime <= window_secs {
            out.push((mtime, path));
        }
    }
}

fn read_codex_session_meta(path: &Path) -> Option<CodexSessionMeta> {
    use std::io::BufRead;

    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let value: serde_json::Value = serde_json::from_str(first.trim()).ok()?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = value.get("payload")?;
    let session_id = payload
        .get("id")
        .or_else(|| payload.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            path.file_stem()
                .map(|value| value.to_string_lossy().into_owned())
        })?;
    let project = payload
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let originator = payload
        .get("originator")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let (tool_id, source_label) = if originator.to_ascii_lowercase().contains("desktop") {
        ("codex_desktop", "Codex Desktop")
    } else {
        ("codex", "Codex CLI")
    };
    Some(CodexSessionMeta {
        tool_id: tool_id.to_string(),
        source_label: source_label.to_string(),
        project,
        session_id,
    })
}

/// 读取最近的生命周期事件。若长任务把 task_started 推到了尾部窗口之外，
/// 则以“文件仍在写入 + 文件头存在 task_started”保守判定为运行中。
fn read_codex_task_state(path: &Path, fallback_ts: i64) -> Option<(String, String, Option<i64>)> {
    if let Some((status, detail, event_at)) = read_codex_tail_task_event(path) {
        // 运行中的任务以文件最后写入时间展示“刚刚/几分钟前”，而不是整轮开始时间。
        let updated_at = if status == "running" {
            Some(fallback_ts)
        } else {
            event_at
        };
        return Some((status, detail, updated_at));
    }
    let prefix = read_prefix_text(path, 256 * 1024)?;
    if prefix.contains("\"task_started\"") {
        return Some((
            "running".to_string(),
            "任务进行中".to_string(),
            Some(fallback_ts),
        ));
    }
    None
}

/// rollout 末尾最后一个可解析的 `timestamp`（RFC3339）。
/// 写入方长期持有文件句柄时 Windows 的 mtime 可能滞后几十分钟，这里以内容时间为准。
fn read_last_content_timestamp(path: &Path) -> Option<i64> {
    let text = read_tail_text(path, TASK_TAIL_BYTES)?;
    for line in text.lines().rev() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let Some(raw) = value.get("timestamp").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(raw) {
            return Some(parsed.timestamp());
        }
    }
    None
}

fn read_codex_tail_task_event(path: &Path) -> Option<(String, String, Option<i64>)> {
    use std::io::{Read, Seek, SeekFrom};

    const TAIL_BYTES: u64 = 512 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if start > 0 {
        // 从中间截断的首行不一定是完整 JSON。
        lines.next();
    }
    lines.filter_map(parse_codex_task_event).next_back()
}

fn read_prefix_text(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::Read;

    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(max_bytes).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_codex_task_event(line: &str) -> Option<(String, String, Option<i64>)> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = value.get("payload")?;
    let event_type = payload.get("type").and_then(serde_json::Value::as_str)?;
    let timestamp = payload
        .get("completed_at")
        .or_else(|| payload.get("started_at"))
        .and_then(serde_json::Value::as_i64);
    match event_type {
        "task_started" => Some(("running".to_string(), "任务进行中".to_string(), timestamp)),
        "task_complete" => {
            let error = payload
                .get("error")
                .and_then(|value| value.get("message"))
                .and_then(serde_json::Value::as_str);
            if let Some(error) = error.filter(|value| !value.is_empty()) {
                return Some((
                    "error".to_string(),
                    format!("失败：{}", truncate(error, 120)),
                    timestamp,
                ));
            }
            let detail = payload
                .get("last_agent_message")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| format!("已完成：{}", truncate(value, 120)))
                .unwrap_or_else(|| "任务已完成".to_string());
            Some(("done".to_string(), detail, timestamp))
        }
        "turn_aborted" => {
            let reason = payload
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("未知原因");
            Some((
                "error".to_string(),
                format!("已中断：{}", truncate(reason, 120)),
                timestamp,
            ))
        }
        _ => None,
    }
}

/// 合并 Qoder CLI 与 Codex 的任务列表，短时间缓存避免两个窗口同时轮询重复扫描。
pub fn list_tasks() -> Vec<DetectedTask> {
    const CACHE_TTL: Duration = Duration::from_secs(2);

    #[derive(Clone)]
    struct CachedTasks {
        at: std::time::Instant,
        tasks: Vec<DetectedTask>,
    }

    static CACHE: OnceLock<parking_lot::Mutex<Option<CachedTasks>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| parking_lot::Mutex::new(None));
    {
        let guard = cache.lock();
        if let Some(cached) = guard.as_ref() {
            if cached.at.elapsed() < CACHE_TTL {
                return cached.tasks.clone();
            }
        }
    }

    let mut tasks = list_qoder_tasks();
    tasks.extend(list_codex_tasks());
    tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_at));
    let mut seen = std::collections::HashSet::new();
    tasks.retain(|task| seen.insert(format!("{}:{}", task.source, task.session_id)));
    tasks.truncate(30);
    *cache.lock() = Some(CachedTasks {
        at: std::time::Instant::now(),
        tasks: tasks.clone(),
    });
    tasks
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
            let content = read_qoder_task_content(&path);
            let title = if content.title.is_empty() {
                detail.clone()
            } else {
                content.title
            };
            let last_message = if content.last_message.is_empty() {
                detail.clone()
            } else {
                content.last_message
            };
            tasks.push(DetectedTask {
                tool_id: "qoder".to_string(),
                source: "qoder_cli".to_string(),
                source_label: "Qoder CLI".to_string(),
                project: decode_project_dir(&project_name),
                session_id: session.file_name().to_string_lossy().into_owned(),
                status,
                detail,
                title,
                last_message,
                deep_link: None,
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

#[derive(Debug, Default)]
struct TaskContent {
    title: String,
    last_message: String,
}

fn read_tail_text(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn normalize_task_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clean_preview_text(raw: &str) -> String {
    let text = raw.replace(['\r', '\n'], " ").replace(['`', '|', '#'], " ");
    normalize_task_text(&text)
}

/// 桌面端会把附件说明包进用户消息正文；任务标题与缩略只取「## My request:」之后的真实请求。
fn strip_user_wrapper(raw: &str) -> String {
    let text = raw.replace("\r\n", "\n");
    for marker in ["## My request:", "## My request："] {
        if let Some(index) = text.find(marker) {
            return text[index + marker.len()..].to_string();
        }
    }
    text
}

/// 附件包装、图片标记等不是请求正文，不能进入任务标题或缩略。
fn is_context_wrapper_text(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.is_empty()
        || trimmed.starts_with("# Files mentioned by the user")
        || trimmed.starts_with("<image ")
        || trimmed.starts_with("<image>")
        || trimmed == "</image>"
}

/// 取第一句作为标题：中文标点直接断句；ASCII 句号/叹号/问号只在其后是空白或结尾时断句，
/// 避免把「1.将 120×130…」这类编号列表截成半句。
fn first_sentence(text: &str) -> &str {
    let mut index = 0;
    while index < text.len() {
        let Some(character) = text[index..].chars().next() else {
            break;
        };
        let width = character.len_utf8();
        let boundary = match character {
            '。' | '！' | '？' | '；' | ';' => true,
            '.' | '!' | '?' => {
                let rest = &text[index + width..];
                rest.is_empty() || rest.starts_with(char::is_whitespace)
            }
            _ => false,
        };
        if boundary {
            let candidate = text[..index].trim();
            if candidate.chars().count() >= 6 {
                return candidate;
            }
        }
        index += width;
    }
    text.trim()
}

fn task_title(raw: &str) -> String {
    let text = clean_preview_text(raw);
    if text.is_empty() {
        return String::new();
    }
    truncate(first_sentence(&text), 64)
}

fn set_task_content(content: &mut TaskContent, raw: &str, assistant: bool) {
    let raw = if assistant {
        raw.to_string()
    } else {
        strip_user_wrapper(raw)
    };
    let mut text = clean_preview_text(&raw);
    if assistant {
        text = text
            .trim_start_matches("已完成：")
            .trim_start_matches("已完成:")
            .to_string();
    }
    if text.is_empty() || is_context_wrapper_text(&text) {
        return;
    }
    // 标题固定为任务开头的那次请求：后续「继续 / 再改一下」不应该改掉任务名。
    if content.title.is_empty() {
        let title = task_title(&text);
        if !title.is_empty() {
            content.title = title;
        }
    }
    content.last_message = truncate(&text, 90);
}

fn extract_recap_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for key in ["recap", "summary", "message", "text", "content"] {
            if let Some(text) = value.get(key).and_then(serde_json::Value::as_str) {
                return text.to_string();
            }
        }
        if let Some(text) = value.as_str() {
            return text.to_string();
        }
    }
    trimmed.to_string()
}

fn is_internal_qoder_prompt(text: &str) -> bool {
    text.starts_with("你正在为 Qoder 任务监控编写简短的会话 Recap")
        || text.starts_with("You are generating a concise session recap")
        || text.contains("只返回一个 JSON 对象")
}

fn read_qoder_task_content(path: &Path) -> TaskContent {
    let mut content = TaskContent::default();
    let Some(text) = read_tail_text(path, 512 * 1024) else {
        return content;
    };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let event_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let data = value.get("data").unwrap_or(&serde_json::Value::Null);
        match event_type {
            "hook.started" => {
                let hook = data.get("hook_input").unwrap_or(&serde_json::Value::Null);
                let is_stop = hook
                    .get("hook_event_name")
                    .and_then(serde_json::Value::as_str)
                    == Some("Stop");
                let is_plugin =
                    hook.get("source").and_then(serde_json::Value::as_str) == Some("plugins");
                if is_stop && is_plugin {
                    if let Some(raw) = hook
                        .get("last_assistant_message")
                        .and_then(serde_json::Value::as_str)
                    {
                        let recap = extract_recap_text(raw);
                        set_task_content(&mut content, &recap, false);
                    }
                }
            }
            "input.prompt.received" => {
                if let Some(preview) = data.get("text_preview").and_then(serde_json::Value::as_str)
                {
                    if !is_internal_qoder_prompt(preview) {
                        set_task_content(&mut content, preview, false);
                    }
                }
            }
            _ => {}
        }
    }
    content
}

fn is_system_user_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    [
        "# AGENTS.md",
        "<environment_context",
        "<permissions instructions",
        "<app-context>",
        "<in-app-browser-context",
        "<ambient-ui-state",
        "# Tools",
        "<multi_agent_mode",
        "You are /root",
    ]
    .iter()
    .any(|marker| trimmed.starts_with(marker))
}

fn message_text(content: &serde_json::Value, role: &str) -> String {
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    let texts = parts
        .iter()
        .filter_map(|part| {
            let kind = part
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if !(kind.eq_ignore_ascii_case("text") || kind == "input_text" || kind == "output_text")
            {
                return None;
            }
            part.get("text").and_then(serde_json::Value::as_str)
        })
        .collect::<Vec<_>>();
    if role == "user" {
        // 一条用户消息可能被拆成多段：附件说明、图片标记、真实请求。
        // 按出现顺序取第一段有效正文，避免把「</image>」这类标记当成任务内容。
        texts
            .into_iter()
            .map(strip_user_wrapper)
            .find(|text| !is_system_user_text(text) && !is_context_wrapper_text(text))
            .unwrap_or_default()
    } else {
        texts.last().copied().unwrap_or_default().to_string()
    }
}

fn parse_codex_task_content_line(line: &str, content: &mut TaskContent) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return;
    };
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("response_item") => {
            let Some(payload) = value.get("payload") else {
                return;
            };
            if payload.get("type").and_then(serde_json::Value::as_str) != Some("message") {
                return;
            }
            let role = payload
                .get("role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let text = message_text(
                payload.get("content").unwrap_or(&serde_json::Value::Null),
                role,
            );
            if role == "user" {
                set_task_content(content, &text, false);
            } else if role == "assistant" {
                set_task_content(content, &text, true);
            }
        }
        Some("event_msg") => {
            let Some(payload) = value.get("payload") else {
                return;
            };
            if payload.get("type").and_then(serde_json::Value::as_str) != Some("item_completed") {
                return;
            }
            let Some(item) = payload.get("item") else {
                return;
            };
            let item_type = item
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let text = message_text(
                item.get("content").unwrap_or(&serde_json::Value::Null),
                if item_type == "UserMessage" {
                    "user"
                } else {
                    "assistant"
                },
            );
            if item_type == "UserMessage" {
                set_task_content(content, &text, false);
            } else if item_type == "AgentMessage" {
                set_task_content(content, &text, true);
            }
        }
        _ => {}
    }
}

fn read_codex_task_content(path: &Path) -> TaskContent {
    let mut content = TaskContent::default();
    for text in [
        read_prefix_text(path, 256 * 1024),
        read_tail_text(path, 256 * 1024),
    ]
    .into_iter()
    .flatten()
    {
        for line in text.lines() {
            parse_codex_task_content_line(line, &mut content);
        }
    }
    content
}

fn codex_deep_link(tool_id: &str, session_id: &str) -> Option<String> {
    if !matches!(tool_id, "codex" | "codex_desktop") {
        return None;
    }
    if session_id.is_empty()
        || !session_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return None;
    }
    Some(format!("codex://threads/{session_id}"))
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
            format!("[{}] 任务出错：{}", task.source_label, task.detail),
        );
    }
    if gateway_status == "working" {
        return ("working".to_string(), gateway_reason.to_string());
    }
    if let Some(task) = tasks.iter().find(|task| task.status == "running") {
        return (
            "working".to_string(),
            format!("[{}] {}", task.source_label, task.detail),
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

/// 测试用：只按进程名匹配，不读取可执行路径。
#[cfg(test)]
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

/// 解析进程时同时考虑可执行文件路径，解决 Claude/Qoder 这类 CLI 与桌面应用同名问题。
fn resolve_process_match(
    stem: &str,
    executable_path: Option<&str>,
) -> Option<(String, String, &'static str)> {
    let cli = crate::cli_tools::TOOLS.iter().find(|spec| {
        spec.binaries
            .iter()
            .any(|binary| binary.eq_ignore_ascii_case(stem))
    });
    let desktop = AI_DESKTOP_APPS.iter().find(|app| {
        app.processes
            .iter()
            .any(|process| process.eq_ignore_ascii_case(stem))
    });
    match (desktop, cli) {
        (Some(app), Some(spec)) => {
            if prefer_desktop_process(app.id, executable_path) {
                Some((app.id.to_owned(), app.label.to_owned(), "app"))
            } else {
                Some((spec.id.to_owned(), spec.label.to_owned(), "cli"))
            }
        }
        (Some(app), None) => Some((app.id.to_owned(), app.label.to_owned(), "app")),
        (None, Some(spec)) => Some((spec.id.to_owned(), spec.label.to_owned(), "cli")),
        (None, None) => None,
    }
}

fn prefer_desktop_process(app_id: &str, executable_path: Option<&str>) -> bool {
    // 这些桌面应用与 CLI 可能使用同名可执行文件，必须结合路径判断。
    let ambiguous = matches!(app_id, "claude_desktop" | "qoder_ide" | "codebuddy");
    if !ambiguous {
        return true;
    }
    let Some(path) = executable_path else {
        // 无法读取路径时沿用旧行为，优先 CLI，避免把 CLI 进程当成桌面应用后误杀。
        return false;
    };
    !looks_like_cli_path(path)
}

fn looks_like_cli_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [
        "\\.local\\bin\\",
        "\\node_modules\\",
        "\\npm\\",
        "\\pnpm\\",
        "\\.qoder\\bin\\",
        "\\appdata\\roaming\\npm\\",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(windows)]
fn process_image_path(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) };
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..length as usize]))
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
            let executable_path = process_image_path(pid);
            let Some((tool_id, tool_label, kind)) =
                resolve_process_match(stem, executable_path.as_deref())
            else {
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

/// 将某个受监控 AI 软件的顶层窗口切到前台。
/// 只接受本次进程枚举中存在的工具标识，不对任意 PID 或窗口句柄开放接口。
pub async fn focus_tool_window(tool_id: &str) -> Result<String, String> {
    let processes = list_ai_processes().await;
    let targets: Vec<&AiProcess> = processes
        .iter()
        .filter(|process| process.tool_id == tool_id)
        .collect();
    let Some(first) = targets.first() else {
        return Err("该软件当前没有检测到运行中的进程；请先刷新监控".to_string());
    };
    let label = first.tool_label.clone();
    let mut pids: Vec<u32> = targets.iter().map(|process| process.pid).collect();
    pids.sort_unstable();
    pids.dedup();

    #[cfg(windows)]
    {
        match find_tool_window(&pids) {
            Some((pid, title)) => {
                let title = if title.is_empty() {
                    String::new()
                } else {
                    format!("，窗口：{title}")
                };
                Ok(format!("已跳转到 {label}（PID {pid}{title}）"))
            }
            None => Err(format!(
                "已检测到 {label}，但没有找到可置前的顶层窗口；CLI 可能正在终端或后台运行。"
            )),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pids;
        Err("当前平台暂不支持定位 AI 软件窗口".to_string())
    }
}

#[cfg(windows)]
fn is_overlay_window_title(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    [
        "desktop pet",
        "computer use status",
        "default ime",
        "msctfime",
        "dde server",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(windows)]
fn find_tool_window(pids: &[u32]) -> Option<(u32, String)> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GetWindow, GetWindowRect, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetForegroundWindow,
        SetWindowPos, ShowWindowAsync, GW_OWNER, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SW_RESTORE,
    };

    struct Candidate {
        hwnd: HWND,
        pid: u32,
        title: String,
        area: i64,
        minimized: bool,
    }

    struct EnumContext {
        pids: Vec<u32>,
        candidates: Vec<Candidate>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
        let context = &mut *(lparam as *mut EnumContext);
        if IsWindowVisible(hwnd) == 0 || !GetWindow(hwnd, GW_OWNER).is_null() {
            return 1;
        }
        let mut pid = 0_u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if !context.pids.contains(&pid) {
            return 1;
        }
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return 1;
        }
        let mut buffer = vec![0_u16; length as usize + 1];
        let copied = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        if copied <= 0 {
            return 1;
        }
        let title = String::from_utf16_lossy(&buffer[..copied as usize]);
        if is_overlay_window_title(&title) {
            return 1;
        }
        let minimized = IsIconic(hwnd) != 0;
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return 1;
        }
        let width = (rect.right - rect.left).max(0) as i64;
        let height = (rect.bottom - rect.top).max(0) as i64;
        let area = width * height;
        // 最小化的主窗口在 Windows 上会位于屏幕外且尺寸很小；仍要保留它，
        // 由后面统一 SW_RESTORE。反之，非最小化的小窗口多半是托盘/覆盖层。
        if !minimized && area < 10_000 {
            return 1;
        }
        context.candidates.push(Candidate {
            hwnd,
            pid,
            title,
            area,
            minimized,
        });
        1
    }

    if pids.is_empty() {
        return None;
    }
    let mut context = EnumContext {
        pids: pids.to_vec(),
        candidates: Vec::new(),
    };
    unsafe {
        EnumWindows(Some(enum_proc), &mut context as *mut EnumContext as LPARAM);
    }
    context.candidates.sort_by(|a, b| {
        a.minimized
            .cmp(&b.minimized)
            .then_with(|| b.area.cmp(&a.area))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    let best = context.candidates.into_iter().next()?;
    unsafe {
        let _ = ShowWindowAsync(best.hwnd, SW_RESTORE);
        let _ = BringWindowToTop(best.hwnd);
        let _ = SetForegroundWindow(best.hwnd);
        let _ = SetWindowPos(
            best.hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );
    }
    Some((best.pid, best.title))
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
            tool_id: "qoder".into(),
            source: "qoder_cli".into(),
            source_label: "Qoder CLI".into(),
            project: "C:\\work".into(),
            session_id: "s1".into(),
            status: "running".into(),
            detail: "进行中：edit".into(),
            title: "修复编辑流程".into(),
            last_message: "继续检查 edit 阶段".into(),
            deep_link: None,
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
            match_process("ChatGPT").map(|m| m.0),
            Some("codex_desktop".to_string())
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
        assert_eq!(
            match_process("Windsurf").map(|m| m.0),
            Some("windsurf".to_string())
        );
        assert_eq!(
            match_process("Kimi智能助手").map(|m| m.0),
            Some("kimi".to_string())
        );
        assert_eq!(
            match_process("Cherry Studio").map(|m| m.0),
            Some("cherry_studio".to_string())
        );
        assert_eq!(
            match_process("LM Studio").map(|m| m.0),
            Some("lm_studio".to_string())
        );
        assert_eq!(
            match_process("Doubao").map(|m| m.0),
            Some("doubao".to_string())
        );
        assert_eq!(
            match_process("Zed").map(|m| m.0),
            Some("zed_editor".to_string())
        );
        assert_eq!(
            match_process("Void").map(|m| m.0),
            Some("void_editor".to_string())
        );
        assert_eq!(
            resolve_process_match(
                "CodeBuddy",
                Some(r"C:\Program Files\CodeBuddy\CodeBuddy.exe")
            )
            .map(|m| m.0),
            Some("codebuddy".to_string())
        );
        assert_eq!(
            resolve_process_match(
                "CodeBuddy",
                Some(r"C:\Users\fixture\.local\bin\codebuddy.exe")
            )
            .map(|m| m.0),
            Some("workbuddy".to_string())
        );
        assert_eq!(
            match_process("ChatGLM").map(|m| m.0),
            Some("chatglm".to_string())
        );

        // 同名进程用可执行路径消歧：Claude Desktop 不能误判为 Claude Code CLI。
        let (id, _, kind) = resolve_process_match(
            "Claude",
            Some(r"C:\Users\fixture\AppData\Local\AnthropicClaude\Claude.exe"),
        )
        .unwrap();
        assert_eq!(id, "claude_desktop");
        assert_eq!(kind, "app");
        let (id, _, kind) =
            resolve_process_match("Claude", Some(r"C:\Users\fixture\.local\bin\claude.exe"))
                .unwrap();
        assert_eq!(id, "claude_code");
        assert_eq!(kind, "cli");
        let (id, _, kind) = resolve_process_match(
            "Qoder",
            Some(r"D:\Program Files\Qoder\Qoder CN\Qoder CN.exe"),
        )
        .unwrap();
        assert_eq!(id, "qoder_ide");
        assert_eq!(kind, "app");
        let (id, _, kind) =
            resolve_process_match("Qoder", Some(r"C:\Users\fixture\.qoder\bin\qoder.exe")).unwrap();
        assert_eq!(id, "qoder");
        assert_eq!(kind, "cli");

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

    #[test]
    fn codex_task_event_parser_maps_lifecycle_and_errors() {
        let (status, _, ts) = parse_codex_task_event(
            r#"{"type":"event_msg","payload":{"type":"task_started","started_at":100}}"#,
        )
        .unwrap();
        assert_eq!(status, "running");
        assert_eq!(ts, Some(100));

        let (status, detail, _) = parse_codex_task_event(
            r#"{"type":"event_msg","payload":{"type":"task_complete","completed_at":120,"error":{"message":"boom"}}}"#,
        )
        .unwrap();
        assert_eq!(status, "error");
        assert!(detail.contains("boom"), "{detail}");

        let (status, detail, _) = parse_codex_task_event(
            r#"{"type":"event_msg","payload":{"type":"turn_aborted","completed_at":130,"reason":"interrupted"}}"#,
        )
        .unwrap();
        assert_eq!(status, "error");
        assert!(detail.contains("interrupted"), "{detail}");

        assert!(
            parse_codex_task_event(r#"{"type":"event_msg","payload":{"type":"token_count"}}"#)
                .is_none()
        );
    }

    #[test]
    fn codex_task_scanner_uses_session_meta_and_last_lifecycle_event() {
        let root =
            std::env::temp_dir().join(format!("llm-gateway-petdex-codex-{}", uuid::Uuid::new_v4()));
        let nested = root.join("2026").join("09").join("12");
        std::fs::create_dir_all(&nested).unwrap();
        let path = nested.join("rollout-fixture.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"sess-codex","cwd":"C:\\Users\\fixture","originator":"Codex Desktop"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<app-context>skip</app-context>"}]}}"#,
                "\n",
                r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions"},{"type":"input_text","text":"完善桌宠任务列表"}]}}"##,
                "\n",
                r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<in-app-browser-context source=\"ambient-ui-state\"> This block is internal"}]}}"##,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"task_started","started_at":100}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"已提取任务标题和最近内容"}]}}}"#,
                "\n"
            ),
        )
        .unwrap();

        let now = chrono::Utc::now().timestamp();
        let tasks = list_codex_tasks_in(&root, now, 60);
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        assert_eq!(tasks[0].tool_id, "codex_desktop");
        assert_eq!(tasks[0].source_label, "Codex Desktop");
        assert_eq!(tasks[0].project, r"C:\Users\fixture");
        assert_eq!(tasks[0].session_id, "sess-codex");
        assert_eq!(tasks[0].status, "running");
        assert_eq!(tasks[0].title, "完善桌宠任务列表");
        assert!(
            tasks[0].last_message.contains("已提取任务标题"),
            "{:?}",
            tasks[0]
        );
        assert_eq!(
            tasks[0].deep_link.as_deref(),
            Some("codex://threads/sess-codex")
        );

        // 同一文件追加失败完成事件后，后一次扫描必须覆盖旧状态，不能继续显示运行中。
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"sess-codex","cwd":"C:\\Users\\fixture","originator":"Codex Desktop"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"task_started","started_at":100}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"task_complete","completed_at":120,"error":{"message":"upstream failed"}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let tasks = list_codex_tasks_in(&root, now, 60);
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        assert_eq!(tasks[0].status, "error");
        assert!(
            tasks[0].detail.contains("upstream failed"),
            "{:?}",
            tasks[0]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn task_title_keeps_numbered_list_and_drops_attachment_wrapper() {
        let title =
            task_title("完善桌宠：\n1.将120×130 作为100%大小，\n2.宠物区域窗口不要限制在120×130");
        assert!(
            title.starts_with("完善桌宠： 1.将120×130"),
            "编号列表不能被句号截断：{title}"
        );

        let wrapped = "# Files mentioned by the user:\n\n## shot.png: C:/tmp/shot.png\n\n## My request:\n依旧未实现理想化效果\n";
        assert_eq!(strip_user_wrapper(wrapped).trim(), "依旧未实现理想化效果");
        assert!(is_context_wrapper_text("</image>"));
        assert!(is_context_wrapper_text(
            "<image name=[Image #1] path=\"C:/tmp/a.png\">"
        ));
        assert!(is_context_wrapper_text(
            "# Files mentioned by the user:\n\n## shot.png: C:/tmp/shot.png"
        ));
    }

    #[test]
    fn codex_task_survives_stale_mtime_when_content_is_fresh() {
        // Windows 上持续写入的 rollout 可能几十分钟不更新 mtime：
        // 这类任务必须靠文件内的 timestamp 被检测到，而不是被 mtime 过滤掉。
        let root = std::env::temp_dir().join(format!("petdex-mtime-{}", uuid::Uuid::new_v4()));
        let nested = root.join("2026").join("09").join("13");
        std::fs::create_dir_all(&nested).unwrap();
        let path = nested.join("rollout-stale-mtime.jsonl");
        let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        std::fs::write(
            &path,
            format!(
                concat!(
                    r#"{{"timestamp":"{stamp}","type":"session_meta","payload":{{"id":"sess-stale","cwd":"D:\\Java\\GitHub\\tieshi","originator":"Codex Desktop"}}}}"#,
                    "\n",
                    r#"{{"timestamp":"{stamp}","type":"event_msg","payload":{{"type":"task_started","started_at":100}}}}"#,
                    "\n",
                    r#"{{"timestamp":"{stamp}","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"继续收口剩余问题"}}]}}}}"#,
                    "\n"
                ),
                stamp = stamp
            ),
        )
        .unwrap();
        // 把 mtime 拨回 40 分钟前：mtime 粗筛会把它排到窗口外，但内容时间仍是刚刚。
        let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 60);
        let handle = std::fs::File::options().write(true).open(&path).unwrap();
        handle.set_modified(stale).unwrap();
        drop(handle);

        let now = chrono::Utc::now().timestamp();
        let tasks = list_codex_tasks_in(&root, now, TASK_ACTIVE_WINDOW_SECS);
        assert_eq!(
            tasks.len(),
            1,
            "mtime 滞后但内容仍在写入的任务必须被检测到：{tasks:?}"
        );
        assert_eq!(tasks[0].status, "running");
        assert_eq!(tasks[0].session_id, "sess-stale");
        assert!(
            now - tasks[0].updated_at <= 5,
            "活跃时间必须取内容时间：{}",
            tasks[0].updated_at
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_task_title_uses_session_index_name_and_skips_wrapper_text() {
        let fixture = |root: &Path| {
            let nested = root.join("2026").join("09").join("12");
            std::fs::create_dir_all(&nested).unwrap();
            let path = nested.join("rollout-title-fixture.jsonl");
            std::fs::write(
                &path,
                concat!(
                    r#"{"type":"session_meta","payload":{"id":"sess-title","cwd":"D:\\Java\\GitHub\\llm-auto","originator":"Codex Desktop"}}"#,
                    "\n",
                    r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# Files mentioned by the user:\n\n## codex-clipboard-abc.png: C:/tmp/codex-clipboard-abc.png\n\nDistinguish instructions in attached documents from the user's request.\n\n## My request:\n完善桌宠：\n1.将120×130 作为100%大小，\n2.宠物区域窗口不要限制在120×130"},{"type":"input_text","text":"<image name=[Image #1] path=\"C:/tmp/codex-clipboard-abc.png\">"},{"type":"input_text","text":"</image>"}]}}"##,
                    "\n",
                    r#"{"type":"event_msg","payload":{"type":"task_started","started_at":100}}"#,
                    "\n"
                ),
            )
            .unwrap();
            path
        };

        let root = std::env::temp_dir().join(format!("petdex-title-{}", uuid::Uuid::new_v4()));
        fixture(&root);
        let now = chrono::Utc::now().timestamp();
        let names = std::collections::HashMap::from([(
            "sess-title".to_string(),
            "实现通用网关".to_string(),
        )]);
        let tasks = list_codex_tasks_with_names(&root, now, 60, &names);
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        assert_eq!(tasks[0].title, "实现通用网关");

        // 索引缺失时退回日志正文：标题取「## My request:」里的请求，附件说明与图片标记都不能进标题或缩略。
        let fallback =
            list_codex_tasks_with_names(&root, now, 60, &std::collections::HashMap::new());
        assert_eq!(fallback.len(), 1, "{fallback:?}");
        assert!(
            fallback[0].title.starts_with("完善桌宠： 1.将120×130"),
            "{:?}",
            fallback[0]
        );
        assert!(
            !fallback[0]
                .last_message
                .contains("Files mentioned by the user")
                && !fallback[0].last_message.contains("</image>"),
            "缩略不能是附件包装或图片标记：{:?}",
            fallback[0]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_session_names_use_last_entry_per_session() {
        let root = std::env::temp_dir().join(format!("petdex-index-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("session_index.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"id":"sess-a","thread_name":"旧标题"}"#,
                "\n",
                r#"{"id":"sess-a","thread_name":"实现通用网关"}"#,
                "\n",
                r#"{"id":"sess-b","thread_name":"   "}"#,
                "\n",
                "not json\n"
            ),
        )
        .unwrap();
        let names = codex_session_names_in(&path);
        assert_eq!(
            names.get("sess-a").map(String::as_str),
            Some("实现通用网关")
        );
        assert!(!names.contains_key("sess-b"), "空白标题必须忽略：{names:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn qoder_task_content_prefers_recap_over_internal_prompt() {
        let root = std::env::temp_dir().join(format!("qoder-content-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("segment.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"input.prompt.received","data":{"text_preview":"你正在为 Qoder 任务监控编写简短的会话 Recap。只返回一个 JSON 对象：{\"recap\":\"...\"}"}}"#,
                "\n",
                r#"{"type":"hook.started","data":{"hook_input":{"hook_event_name":"Stop","source":"plugins","last_assistant_message":"{\"recap\":\"完成桌宠任务列表联调，并保留未收尾的窗口定位回归。\"}"}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let content = read_qoder_task_content(&path);
        assert!(
            content.title.contains("完成桌宠任务列表联调"),
            "{content:?}"
        );
        assert!(!content.title.contains("你正在为 Qoder"), "{content:?}");
        assert!(content.last_message.contains("窗口定位回归"), "{content:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn desktop_catalog_has_unique_ids_and_process_names() {
        let mut ids = std::collections::HashSet::new();
        let mut processes = std::collections::HashSet::new();
        for app in AI_DESKTOP_APPS {
            assert!(ids.insert(app.id), "重复的桌面工具 id：{}", app.id);
            for process in app.processes {
                assert!(
                    processes.insert(process.to_ascii_lowercase()),
                    "重复的进程名：{process}"
                );
            }
        }
        assert!(AI_DESKTOP_APPS.len() >= 40, "桌面工具清单过少");
    }

    #[cfg(windows)]
    #[test]
    fn window_focus_rejects_pet_and_status_overlays() {
        assert!(is_overlay_window_title("Qoder Desktop Pet"));
        assert!(is_overlay_window_title("Qoder Computer Use Status"));
        assert!(!is_overlay_window_title("Qoder CN"));
    }
}
