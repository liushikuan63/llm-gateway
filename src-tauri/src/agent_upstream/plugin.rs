//! B8：外部适配器协议 —— **描述文件的解析与校验**。
//!
//! ## 为什么校验是这一格的主体
//!
//! 「不改网关代码也能接一个新工具」的代价是：**描述文件是外部输入**，
//! 而它会变成子进程的命令行 —— 命令注入面就在这里。
//! 卡片把两条负向实验写成判据，正是因为「我们不这么用」不构成防护。
//!
//! ## 只允许这四项（卡片原文）
//!
//! 描述文件是**固定字段的 JSON**：`id` / `kind` / `exe` / `args` / `protocol`。
//! 不做可执行脚本、不做动态表达式 —— 一旦允许，这个文件就变成了代码，
//! 而它来自一个用户可以随手改的目录。
//!
//! ## 网关**不得**对描述文件所在目录做解释执行
//!
//! 「白名单目录」在卡片里有第二层含义：网关只**读**那里的描述文件，
//! 不去 import、不去 eval、不去执行该目录下的任何东西。
//! 本模块只做 `fs::read_to_string` 与 JSON 解析，正是这条的落实。

use std::path::{Path, PathBuf};

/// 支持的外部适配器协议版本。
///
/// **白名单比较而不是前缀匹配**：`"jsonl-v1evil"` 用 `starts_with` 会漏过去，
/// 而漏过去的表现是「一个我们不认识的协议被当成认识的」，等到真发请求才炸。
pub const SUPPORTED_PROTOCOL: &str = "jsonl-v1";

/// 一个外部适配器的描述文件。
///
/// 字段名与卡片逐字对应，`#[serde(deny_unknown_fields)]` **刻意不加**：
/// 加了这个，将来协议加字段会让**所有旧描述文件**直接失效，
/// 而那是一次静默的兼容性断裂（用户只知道「昨天还能用」）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginManifest {
    /// 适配器标识，对应 `AdapterRegistry` 里的 key。
    pub id: String,
    /// 运行时类型（用户可以按它给供应商分组）。
    pub kind: String,
    /// 可执行文件。可以是绝对路径，也可以是 PATH 上的名字。
    pub exe: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// 协议版本，必须等于 [`SUPPORTED_PROTOCOL`]。
    pub protocol: String,
}

/// 会被 shell 解释的字符。
///
/// `args` 是**逐项**交给 `Command::arg` 的，本身不经过 shell —— 但
/// `exe` 若是一个 `.cmd` / `.bat`，Windows 会用 `cmd.exe` 去解析它，
/// 那一刻这些字符就真的会被解释。两处都拦，因为「哪一处会经过 shell」
/// 取决于 exe 的扩展名，而那是用户可以改的。
const SHELL_METACHARACTERS: [&str; 7] = [";", "&&", "|", "`", "$(", "\n", "\r"];

/// 解析描述文件。
///
/// **只做 JSON 与字段自身的校验**，不碰路径 —— 路径白名单需要额外的上下文
/// （描述文件在哪、允许哪些目录），那是 [`validate_manifest`] 的事。
/// 拆成两步是为了让「文件内容合法」与「这个位置允许加载」两条失败
/// 能各自被断言，而不是混成一个「加载失败」。
pub fn parse_manifest(raw: &str) -> Result<PluginManifest, String> {
    let manifest: PluginManifest =
        serde_json::from_str(raw).map_err(|e| format!("描述文件不是合法的 JSON：{e}"))?;
    if manifest.id.trim().is_empty() {
        return Err("描述文件缺少 id".into());
    }
    if manifest.exe.trim().is_empty() {
        return Err(format!("描述文件 {} 缺少 exe", manifest.id));
    }
    if manifest.protocol.trim() != SUPPORTED_PROTOCOL {
        return Err(format!(
            "不支持的协议版本 {:?}（只认得 {SUPPORTED_PROTOCOL}）",
            manifest.protocol
        ));
    }
    Ok(manifest)
}

/// 校验描述文件**所在的位置**与它将要启动的命令行。
///
/// `manifest_path` 必须已经存在（会做 `canonicalize`）——
/// 用符号链接把白名单目录指到外面是最常见的绕过方式，规范化能挡住它。
///
/// 报错里**必须带路径**（卡片对负向实验的要求）：只说「不在白名单里」，
/// 用户拿不到任何线索判断是自己填错了目录还是白名单没配。
pub fn validate_manifest(
    manifest: &PluginManifest,
    manifest_path: &Path,
    allowed_dirs: &[PathBuf],
) -> Result<(), String> {
    let canonical = manifest_path
        .canonicalize()
        .map_err(|e| format!("描述文件路径无法解析：{}（{e}）", manifest_path.display()))?;
    // 每个白名单目录**各自**规范化：目录不存在时它不该让整批校验失败，
    // 只是这一条不算数 —— 配置写错一个目录不该连带把能用的也废掉。
    let allowed = allowed_dirs.iter().any(|dir| {
        dir.canonicalize()
            .map(|dir| canonical.starts_with(&dir))
            .unwrap_or(false)
    });
    if !allowed {
        let list = allowed_dirs
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join("、");
        return Err(format!(
            "描述文件不在允许的目录内：{}（允许的目录：{}）",
            canonical.display(),
            if list.is_empty() {
                "（没有配置任何目录）".to_string()
            } else {
                list
            }
        ));
    }

    // 注入面：`exe` 与每一个 `args` 都要过。
    let mut targets: Vec<(String, &str)> = vec![("exe".to_string(), manifest.exe.as_str())];
    for (index, arg) in manifest.args.iter().enumerate() {
        targets.push((format!("args[{index}]"), arg.as_str()));
    }
    for (field, value) in targets {
        if let Some(hit) = SHELL_METACHARACTERS.iter().find(|m| value.contains(**m)) {
            return Err(format!(
                "{field} 里含会被 shell 解释的字符 {hit:?}：{value:?}"
            ));
        }
    }
    Ok(())
}
