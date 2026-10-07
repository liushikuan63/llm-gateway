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

// ============================ 协议编解码 ============================
//
// 一行一个 JSON 对象（卡片原文）。`request_id` 由**网关**生成、响应原样带回 ——
// 它是这个协议唯一的配对手段，因为子进程的 stdout 里可能混着它自己的日志。

/// 一次请求的三种形态。`type` 字段的取值就是下面的 snake_case 名。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginRequestKind {
    /// 探活：插件能不能干活（不加载模型、不发请求）。
    Probe,
    /// 列出它支持的模型名。
    ListModels,
    /// 跑一轮对话。
    Complete { model: String, prompt: String },
}

/// 请求信封：`request_id` 与三个变体拼在同一层。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginRequest {
    pub request_id: String,
    #[serde(flatten)]
    pub kind: PluginRequestKind,
}

/// 响应信封。
///
/// `result` 刻意是**自由的 JSON** 而不是枚举：三种请求的结果形状不同
/// （`ready` / `models` / `text`），用枚举会逼着插件作者按我们的类型写，
/// 而协议的价值在于**最小**。形状由下面的访问器负责解释与报错。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginResponse {
    pub request_id: String,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
    /// 插件可以带回原始行，供排查用。**不参与判定**。
    #[serde(default)]
    pub raw: Option<String>,
}

impl PluginResponse {
    /// `complete` 的文本。
    pub fn text(&self) -> Result<String, String> {
        self.result
            .as_ref()
            .and_then(|value| value.get("text"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("响应里没有 text 字段：{}", self.describe()))
    }

    /// `list_models` 的模型名。
    pub fn models(&self) -> Result<Vec<String>, String> {
        self.result
            .as_ref()
            .and_then(|value| value.get("models"))
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .ok_or_else(|| format!("响应里没有 models 数组：{}", self.describe()))
    }

    /// `probe` 的就绪位。
    pub fn ready(&self) -> Result<bool, String> {
        self.result
            .as_ref()
            .and_then(|value| value.get("ready"))
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| format!("响应里没有 ready 布尔：{}", self.describe()))
    }

    fn describe(&self) -> String {
        self.raw
            .clone()
            .unwrap_or_else(|| serde_json::to_string(self).unwrap_or_default())
    }
}

/// 把一次请求编成**一行**。
///
/// 必须压成单行：协议是 JSONL，行内出现裸换行会把一条消息劈成两条。
/// `serde_json::to_string` 不产生裸换行（字符串里的换行会转义）✓ ——
/// 这里仍然断言一次，免得将来有人换成 pretty 打印。
pub fn encode_request(request: &PluginRequest) -> Result<String, String> {
    let line = serde_json::to_string(request).map_err(|e| format!("请求无法序列化：{e}"))?;
    if line.contains('\n') || line.contains('\r') {
        return Err("编码后的请求里出现了裸换行，会破坏 JSONL 分帧".into());
    }
    Ok(line)
}

/// 解析一行响应。**只解析，不配对**。
pub fn parse_response(line: &str) -> Result<PluginResponse, String> {
    serde_json::from_str(line).map_err(|e| format!("这一行不是合法的响应 JSON：{e}"))
}

/// 从子进程吐出的若干行里挑出 `request_id` 匹配的那一条。
///
/// ## 为什么不匹配的行直接跳过
///
/// 插件的 stdout 里**混着它自己的日志**是常态（它是个普通进程，
/// 没人能禁止它 `println!`）。把「解析不了的行」一律当协议破坏，
/// 会让一个爱打日志的插件完全不可用 —— 而那种失败看起来像
/// 「网关坏了」，排查方向完全错。
///
/// ## 找不到时的报错必须带上原文
///
/// 「没等到响应」本身没有任何可操作性。把最后几行原样带出来，
/// 用户才能看出是「插件根本没起来」「它在等输入」还是「它说了别的东西」。
pub fn pick_response<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    request_id: &str,
) -> Result<PluginResponse, String> {
    let mut tail: Vec<String> = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 只留最后几行，避免一个话痨插件把错误消息撑成几兆。
        tail.push(trimmed.chars().take(200).collect());
        if tail.len() > 3 {
            tail.remove(0);
        }
        let Ok(response) = parse_response(trimmed) else {
            continue; // 日志行，跳过
        };
        if response.request_id != request_id {
            continue; // 别的请求的响应，跳过
        }
        if let Some(error) = response.error.as_deref() {
            return Err(format!("插件报告失败：{error}"));
        }
        return Ok(response);
    }
    Err(format!(
        "没有等到 request_id = {request_id} 的响应。插件最后几行输出：{}",
        if tail.is_empty() {
            "（一行都没有）".to_string()
        } else {
            tail.join(" ⏎ ")
        }
    ))
}

// ============================ 适配器 ============================

/// 一次往返的传输层。
///
/// 抽成 trait 的目的很具体：**协议逻辑不该依赖真进程**。
/// 真实现（子进程 + stdin/stdout）与测试用的假实现各一份，而
/// 「请求编码对不对、响应怎么配对、插件报错怎么办」这些能在假实现上全测完 ——
/// 真进程那一格就只剩「起进程 / 收流 / 超时杀树 / 孤儿清理」。
///
/// `timeout_ms` 由调用方给而不是 transport 自己定：超时是**请求级**的
/// （`AgentRequest.timeout_ms`），传输层只是执行者。
#[async_trait::async_trait]
pub trait PluginTransport: Send + Sync {
    /// 送一行出去，回它吐出来的行。
    async fn exchange(&self, line: &str, timeout_ms: u64) -> Result<Vec<String>, String>;
}

/// 按 B8 协议与外部插件对话的适配器。
pub struct ExternalAdapter {
    id: &'static str,
    label: &'static str,
    transport: std::sync::Arc<dyn PluginTransport>,
}

impl ExternalAdapter {
    /// 用描述文件与一个 transport 建适配器。
    ///
    /// 【为什么要 `Box::leak`】`AgentAdapter::id()` 的契约是 `&'static str`
    /// —— A5 刻意用它逼 id 在编译期定死，因为改名等于破坏用户配置里的引用。
    /// 而外部插件的 id 来自**运行时的描述文件**，两者天然冲突。
    /// 插件在进程生命周期内不会被卸载，泄漏的是每插件几十字节；
    /// 为一个桥接需求去改整个 trait 契约（进而动到 A5/A6/A7 全部适配器）
    /// 是拿大炮打蚊子。**这条冲突本身记在交接单里**，将来若要动态卸载插件
    /// 就得回来改契约。
    pub fn new(manifest: &PluginManifest, transport: std::sync::Arc<dyn PluginTransport>) -> Self {
        Self {
            id: Box::leak(manifest.id.clone().into_boxed_str()),
            label: Box::leak(manifest.kind.clone().into_boxed_str()),
            transport,
        }
    }

    /// 一次完整的往返：生成 request_id → 编码 → 送出去 → 按 id 配对。
    ///
    /// `request_id` 由**网关**生成（不是让插件回显它收到的）：插件可以不回显，
    /// 而网关必须能区分「这条响应是不是我要的那条」。
    async fn round_trip(
        &self,
        kind: PluginRequestKind,
        timeout_ms: u64,
    ) -> Result<PluginResponse, String> {
        let request_id = uuid::Uuid::new_v4().simple().to_string();
        let line = encode_request(&PluginRequest {
            request_id: request_id.clone(),
            kind,
        })?;
        let lines = self.transport.exchange(&line, timeout_ms).await?;
        pick_response(lines.iter().map(String::as_str), &request_id)
    }

    /// 探活。
    pub async fn probe(&self, timeout_ms: u64) -> Result<bool, String> {
        self.round_trip(PluginRequestKind::Probe, timeout_ms)
            .await?
            .ready()
    }

    /// 列出插件支持的模型。
    pub async fn list_models(&self, timeout_ms: u64) -> Result<Vec<String>, String> {
        self.round_trip(PluginRequestKind::ListModels, timeout_ms)
            .await?
            .models()
    }
}

#[async_trait::async_trait]
impl crate::agent_upstream::AgentAdapter for ExternalAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn label(&self) -> &'static str {
        self.label
    }

    async fn send(
        &self,
        request: crate::agent_upstream::AgentRequest,
    ) -> Result<crate::agent_upstream::AgentReply, String> {
        let response = self
            .round_trip(
                PluginRequestKind::Complete {
                    model: request.model,
                    prompt: request.prompt,
                },
                request.timeout_ms,
            )
            .await?;
        Ok(crate::agent_upstream::AgentReply {
            text: response.text()?,
            // 如实回报走了哪条传输：这是一个**外部插件**，
            // 与内置的 L1/L3/fake 都不是一回事，审计里要能分开。
            transport: "external".into(),
        })
    }
}
