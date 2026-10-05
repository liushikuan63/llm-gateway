//! C1 MCP 网关：**统一接入、工具目录聚合、按 Key 授权、调用审计**。
//!
//! ## 边界（卡片先钉死的）
//!
//! 本模块**不做**：
//! - ReAct 编排循环（agent 循环属于调用方 —— 塞进网关会让网关变成
//!   有隐式状态的应用服务器）
//! - 具体工具实现
//! - 知识库运营
//!
//! ## 按需连接
//!
//! 目录聚合**不拉起任何 server**。拉起一个 server 是启动一个进程，
//! 代价真实存在 —— 所以只把清单读进来，真正连接发生在第一次调用该
//! server 的工具时。这条有计数型断言盯着（`未选中的_server_不被启动`）。
//!
//! ## 授权失败必须是 403，不是静默过滤
//!
//! 静默过滤会让用户以为「工具本身坏了」，而不是「我没这个权限」。
//! 这两种情况的排查方向完全相反。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub mod runtime;
pub mod stdio;

/// 一次 MCP server 连接的传输方式。
///
/// 用 derive 而不是手写 `impl Default`：`Http` 正好是第一个变体，
/// `#[default]` 与手写完全等价。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// 通过 HTTP 连（含 SSE 流）。
    #[default]
    Http,
    /// 本地拉起进程，走 stdin/stdout。
    Stdio,
}

/// `mcp-servers.json` 里的一条 server 声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub transport: Transport,
    /// `http` 传输：base url。`stdio` 传输：可执行文件路径。
    #[serde(default)]
    pub url: String,
    /// `stdio` 传输的命令行参数。`http` 传输忽略。
    #[serde(default)]
    pub args: Vec<String>,
    /// 是否启用。停用的 server 不进目录，也不会被拉起。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// `mcp-servers.json` 的整体形态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpManifest {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
    /// MCP 接入的总开关。**默认关闭** ——
    /// 关着时目录为空、不启动任何进程、`dispatch` 逐位等价。
    #[serde(default)]
    pub enabled: bool,
}

impl McpManifest {
    /// 解析清单。**解析失败返回空清单而不是报错**，理由与
    /// `budget::parse_allowed_models` 一致：清单坏了的时候，
    /// 「一个工具都不给」比「整个网关起不来」安全。
    ///
    /// 但要**留下痕迹** —— 调用方拿 `McpManifest::load` 的返回值判断
    /// 是否回落了，界面用得上。
    pub fn parse(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or_default()
    }

    /// 启用的 server。
    pub fn active_servers(&self) -> Vec<&McpServerConfig> {
        if !self.enabled {
            return Vec::new();
        }
        self.servers.iter().filter(|s| s.enabled).collect()
    }

    /// 清单里是否存在重复 id。
    ///
    /// 重复 id 会让「按 id 找 server」的结果取决于遍历顺序，
    /// 而工具目录里会出现两个同名来源 —— 排查时极难看出。
    pub fn duplicate_ids(&self) -> Vec<String> {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for server in &self.servers {
            *seen.entry(server.id.as_str()).or_insert(0) += 1;
        }
        seen.into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(id, _)| id.to_string())
            .collect()
    }
}

/// 目录里的一个工具。
///
/// `server_id` 是必填的：同一个工具名可能来自两个 server，
/// 不记来源的话用户点「搜索」根本不知道会连到哪家。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub server_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 入参 JSON Schema，原样透传。
    pub input_schema: Value,
    /// 该 server 当前是否可用。**server 崩了只标记这一条，不是整个网关挂掉。**
    pub available: bool,
}

/// 工具目录：把多个 server 的工具汇成一张表。
#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: Vec<ToolDescriptor>,
}

impl ToolCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// 收一个 server 的工具。`available` 一并带上 ——
    /// 崩溃的 server 的工具仍要出现在目录里（标成不可用），
    /// 否则用户会以为「这个工具从来不存在」。
    pub fn add_server_tools(
        &mut self,
        server_id: &str,
        tools: impl IntoIterator<Item = (String, String, Value)>,
        available: bool,
    ) {
        for (name, description, input_schema) in tools {
            self.tools.push(ToolDescriptor {
                server_id: server_id.to_string(),
                name,
                description,
                input_schema,
                available,
            });
        }
    }

    /// 把一个 server 全部标记为不可用（它崩了）。
    pub fn mark_unavailable(&mut self, server_id: &str) {
        for tool in &mut self.tools {
            if tool.server_id == server_id {
                tool.available = false;
            }
        }
    }

    /// 把一个 server 全部标记为可用（它恢复了）。
    pub fn mark_available(&mut self, server_id: &str) {
        for tool in &mut self.tools {
            if tool.server_id == server_id {
                tool.available = true;
            }
        }
    }

    pub fn all(&self) -> &[ToolDescriptor] {
        &self.tools
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 按 (server_id, name) 精确查找。
    ///
    /// **必须两个都匹配**：只按 name 查会在同名工具来自多个 server 时
    /// 命中任意一个，而调用方以为自己调的是另一家。
    pub fn find(&self, server_id: &str, name: &str) -> Option<&ToolDescriptor> {
        self.tools
            .iter()
            .find(|t| t.server_id == server_id && t.name == name)
    }

    /// 目录里出现过的 server id（去重、有序）。
    pub fn server_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .tools
            .iter()
            .map(|t| t.server_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        ids.sort();
        ids
    }
}

/// MCP 授权判定。
///
/// 与 B2 的模型白名单**同口径**：
/// - 空列表 = 不限
/// - 支持 `*` 通配
/// - 不带通配时精确匹配
///
/// 复用同一套语义是刻意的：用户在两个地方看到「留空 = 不限」，
/// 就不会去猜 MCP 这边是不是反过来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpDecision {
    Allow,
    /// 这个 Key 没有被授权使用该 server 的工具。
    Denied,
}

/// 判定某个 Key 能不能用某个 server 的工具。
///
/// `allowed_servers` 是**该 Key 自己**的 server 白名单。
/// 空 = 不限（与 `allowed_models` 一致）。
pub fn authorize(allowed_servers: &[String], server_id: &str) -> McpDecision {
    if allowed_servers.is_empty() {
        return McpDecision::Allow;
    }
    let ok = allowed_servers.iter().any(|pattern| {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        if let Some(prefix) = pattern.strip_suffix('*') {
            // 前缀通配。`*` 单独出现时 prefix 为空，此时匹配一切 ——
            // 与「空 = 不限」同义，不额外特判。
            server_id.starts_with(prefix)
        } else {
            pattern == server_id
        }
    });
    if ok {
        McpDecision::Allow
    } else {
        McpDecision::Denied
    }
}

/// 一次工具调用的审计记录。
///
/// **刻意没有 `arguments` 字段。** 工具入参可能含凭据与用户数据
/// （卡片点名），所以类型层面就不给存全文的位置 ——
/// 「不小心把入参写进审计」在编译期就不可能。
///
/// 需要留一点可追溯性时用 [`ToolCallAudit::argument_digest`]：
/// 参数个数 + 每个参数**脱敏后**的前若干字符，够定位、不够泄密。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallAudit {
    /// 谁发的。与 `requests.client` 同口径（`remote-key:<id>` 或 `local-unified-key`）。
    pub client: Option<String>,
    /// B4 的 traceId —— 与同一次请求的 LLM 调用串得起来。
    pub trace_id: String,
    pub server_id: String,
    pub tool: String,
    pub ok: bool,
    pub latency_ms: u64,
    /// 失败原因（已截断）。成功时是 `None`。
    pub error: Option<String>,
    /// 参数的**非明文**摘要。见 [`ToolCallAudit::argument_digest`]。
    pub argument_digest: Option<String>,
}

/// 错误原文入库前的截断长度。
///
/// 上游返回的错误可能很长（整段 HTML 错误页），
/// 原样入库会把审计行撑爆，而且那些内容对排查没有额外帮助。
pub const MAX_ERROR_CHARS: usize = 500;

impl ToolCallAudit {
    /// 生成参数的**非明文**摘要。
    ///
    /// 形态：`n=3 sha=ab12cd34ef56 keys=query,top_k` ——
    /// 参数个数、全部参数序列化后的短哈希、参数名清单。
    ///
    /// 为什么是这三个而不是脱敏后的值：脱敏函数（B3 的 `redact_prompt`）
    /// 挡的是**已知形态**的密钥；工具入参的形态是任意的，
    /// 一个没被规则覆盖的字段就会原样落盘。参数名与哈希不泄内容，
    /// 但足以回答「两次调用的入参是不是同一份」这个最常见的排查问题。
    pub fn argument_digest(arguments: &Value) -> Option<String> {
        let obj = arguments.as_object()?;
        let canonical = serde_json::to_string(arguments).unwrap_or_default();
        let hash = short_hash(&canonical);
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        Some(format!(
            "n={} sha={} keys={}",
            obj.len(),
            hash,
            keys.join(",")
        ))
    }

    /// 把错误原文截断到 [`MAX_ERROR_CHARS`]。
    pub fn truncate_error(raw: &str) -> String {
        let mut out: String = raw.chars().take(MAX_ERROR_CHARS).collect();
        if raw.chars().count() > MAX_ERROR_CHARS {
            out.push('…');
        }
        out
    }
}

/// 短哈希。用 `sha2`（已在依赖里），取前 12 位十六进制 ——
/// 够区分不同的入参，又不至于长到占满审计表。
fn short_hash(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex.chars().take(12).collect()
}

/// MCP 相关错误。**每个都映射到明确的状态码**，不做静默降级。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpError {
    /// 总开关关着。
    Disabled,
    /// 工具不在目录里。
    ToolNotFound { server_id: String, tool: String },
    /// 该 Key 未被授权使用这个 server。
    Forbidden { server_id: String },
    /// server 标记为不可用（崩了）。
    ServerUnavailable { server_id: String },
    /// 传输层失败。
    Transport { server_id: String, detail: String },
}

impl McpError {
    pub fn http_status(&self) -> u16 {
        match self {
            McpError::Disabled => 503,
            McpError::ToolNotFound { .. } => 404,
            // 403 而不是「过滤掉」：静默过滤会让用户以为工具本身坏了。
            McpError::Forbidden { .. } => 403,
            McpError::ServerUnavailable { .. } => 503,
            McpError::Transport { .. } => 502,
        }
    }

    /// 给客户端看的错误码。
    pub fn code(&self) -> &'static str {
        match self {
            McpError::Disabled => "mcp_disabled",
            McpError::ToolNotFound { .. } => "tool_not_found",
            McpError::Forbidden { .. } => "mcp_forbidden",
            McpError::ServerUnavailable { .. } => "server_unavailable",
            McpError::Transport { .. } => "upstream_error",
        }
    }

    /// 给客户端看的消息。**不含任何入参内容。**
    pub fn message(&self) -> String {
        match self {
            McpError::Disabled => "MCP 接入未启用".into(),
            McpError::ToolNotFound { server_id, tool } => {
                format!("工具 {server_id}/{tool} 不在目录里")
            }
            McpError::Forbidden { server_id } => {
                format!("当前访问 Key 未被授权使用 server {server_id}")
            }
            McpError::ServerUnavailable { server_id } => {
                format!("server {server_id} 当前不可用")
            }
            McpError::Transport { server_id, detail } => {
                format!("server {server_id} 传输失败：{detail}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest_json(servers: &str, enabled: bool) -> String {
        format!(r#"{{"enabled": {enabled}, "servers": [{servers}]}}"#)
    }

    #[test]
    fn 清单解析出两种传输() {
        let raw = manifest_json(
            r#"{"id":"a","transport":"http","url":"http://127.0.0.1:9/sse"},
               {"id":"b","transport":"stdio","url":"node","args":["server.js"]}"#,
            true,
        );
        let m = McpManifest::parse(&raw);
        assert_eq!(m.servers.len(), 2);
        assert_eq!(m.servers[0].transport, Transport::Http);
        assert_eq!(m.servers[1].transport, Transport::Stdio);
        assert_eq!(m.servers[1].args, vec!["server.js"]);
    }

    #[test]
    fn 清单解析失败回落成空清单而不是报错() {
        // 清单坏了的时候，「一个工具都不给」比「整个网关起不来」安全
        let m = McpManifest::parse("{ 这不是 JSON");
        assert!(m.servers.is_empty());
        assert!(!m.enabled);
        assert!(McpManifest::parse("").servers.is_empty());
    }

    #[test]
    fn 总开关关着时不返回任何启用_server() {
        let raw = manifest_json(r#"{"id":"a","url":"http://x"}"#, false);
        let m = McpManifest::parse(&raw);
        assert_eq!(m.servers.len(), 1, "清单本身读得出来");
        assert!(m.active_servers().is_empty(), "但关着时一个都不算启用");
    }

    #[test]
    fn 单项停用的_server_不进启用列表() {
        let raw = manifest_json(
            r#"{"id":"a","url":"http://x"},
               {"id":"b","url":"http://y","enabled":false}"#,
            true,
        );
        let m = McpManifest::parse(&raw);
        let active = m.active_servers();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "a");
    }

    #[test]
    fn 重复_id_被检出() {
        // 重复 id 会让「按 id 找 server」取决于遍历顺序，
        // 而目录里会出现两个同名来源 —— 排查时极难看出
        let raw = manifest_json(
            r#"{"id":"dup","url":"http://x"},{"id":"dup","url":"http://y"},{"id":"ok","url":"http://z"}"#,
            true,
        );
        let m = McpManifest::parse(&raw);
        assert_eq!(m.duplicate_ids(), vec!["dup".to_string()]);
    }

    #[test]
    fn 目录聚合多个_server_的工具() {
        let mut catalog = ToolCatalog::new();
        catalog.add_server_tools(
            "a",
            vec![
                ("search".into(), "搜索".into(), json!({"type": "object"})),
                ("fetch".into(), "抓取".into(), json!({})),
            ],
            true,
        );
        catalog.add_server_tools("b", vec![("calc".into(), "计算".into(), json!({}))], true);
        assert_eq!(catalog.len(), 3);
        assert_eq!(catalog.server_ids(), vec!["a".to_string(), "b".to_string()]);
        // 每个工具都要带来源
        assert!(catalog.all().iter().all(|t| !t.server_id.is_empty()));
    }

    #[test]
    fn 同名工具按_server_加名字两个都找得到() {
        let mut catalog = ToolCatalog::new();
        catalog.add_server_tools(
            "a",
            vec![("search".into(), "A 的搜索".into(), json!({}))],
            true,
        );
        catalog.add_server_tools(
            "b",
            vec![("search".into(), "B 的搜索".into(), json!({}))],
            true,
        );
        assert_eq!(catalog.len(), 2, "同名不同源，两条都要在目录里");
        assert_eq!(catalog.find("a", "search").unwrap().description, "A 的搜索");
        assert_eq!(catalog.find("b", "search").unwrap().description, "B 的搜索");
        // 只按名字查必须查不到 —— 不给「猜一个」的机会
        assert!(catalog.find("c", "search").is_none());
    }

    #[test]
    fn server_崩溃只标记该_server_的工具不可用() {
        let mut catalog = ToolCatalog::new();
        catalog.add_server_tools("a", vec![("x".into(), String::new(), json!({}))], true);
        catalog.add_server_tools("b", vec![("y".into(), String::new(), json!({}))], true);
        catalog.mark_unavailable("a");
        // 工具仍在目录里（不能消失，否则用户以为它从来不存在）
        assert_eq!(catalog.len(), 2);
        assert!(!catalog.find("a", "x").unwrap().available);
        assert!(catalog.find("b", "y").unwrap().available, "b 不受影响");
        // 恢复
        catalog.mark_available("a");
        assert!(catalog.find("a", "x").unwrap().available);
    }

    // ---------- 授权 ----------

    #[test]
    fn 空白名单表示不限() {
        assert_eq!(authorize(&[], "anything"), McpDecision::Allow);
        // 空白项不算授权
        assert_eq!(
            authorize(&["".into(), "  ".into()], "a"),
            McpDecision::Denied
        );
    }

    #[test]
    fn 精确匹配与星号通配() {
        assert_eq!(authorize(&["a".into()], "a"), McpDecision::Allow);
        assert_eq!(authorize(&["a".into()], "ab"), McpDecision::Denied);
        assert_eq!(authorize(&["a*".into()], "ab"), McpDecision::Allow);
        assert_eq!(authorize(&["*".into()], "任何东西"), McpDecision::Allow);
        // 含空格的项要去空白再比
        assert_eq!(authorize(&[" a ".into()], "a"), McpDecision::Allow);
    }

    #[test]
    fn 未授权返回_403_而不是被过滤掉() {
        let err = McpError::Forbidden {
            server_id: "a".into(),
        };
        assert_eq!(err.http_status(), 403);
        assert_eq!(err.code(), "mcp_forbidden");
        // 消息要说清是哪个 server，而不是笼统的「无权限」
        assert!(err.message().contains('a'));
    }

    #[test]
    fn 各类错误的状态码() {
        assert_eq!(McpError::Disabled.http_status(), 503);
        assert_eq!(
            McpError::ToolNotFound {
                server_id: "a".into(),
                tool: "t".into()
            }
            .http_status(),
            404
        );
        assert_eq!(
            McpError::ServerUnavailable {
                server_id: "a".into()
            }
            .http_status(),
            503
        );
        assert_eq!(
            McpError::Transport {
                server_id: "a".into(),
                detail: "conn refused".into()
            }
            .http_status(),
            502
        );
    }

    #[test]
    fn 错误消息里不含入参内容() {
        // 错误消息会回给客户端，也被审计记下来 —— 不能带参数值
        let err = McpError::Transport {
            server_id: "a".into(),
            detail: "connection refused".into(),
        };
        assert!(!err.message().contains("sk-"));
        assert!(err.message().contains("a"));
    }

    // ---------- 审计 ----------

    #[test]
    fn 审计结构里没有入参全文的位置() {
        // 类型层面就保证存不下全文
        let json = serde_json::to_string(&ToolCallAudit {
            client: Some("remote-key:k1".into()),
            trace_id: "t".into(),
            server_id: "a".into(),
            tool: "search".into(),
            ok: true,
            latency_ms: 12,
            error: None,
            argument_digest: None,
        })
        .unwrap();
        for forbidden in ["arguments", "params", "input", "payload"] {
            assert!(
                !json.contains(forbidden),
                "审计结构里不该有 {forbidden}：{json}"
            );
        }
    }

    #[test]
    fn 入参摘要不含明文只含个数哈希与键名() {
        let args = json!({
            "api_key": "sk-abcdefghijklmnopqrstuvwxyz",
            "query": "北京天气",
            "top_k": 5
        });
        let digest = ToolCallAudit::argument_digest(&args).expect("应有摘要");
        assert!(
            !digest.contains("sk-abcdefghijklmnopqrstuvwxyz"),
            "{digest}"
        );
        assert!(!digest.contains("北京天气"), "{digest}");
        assert!(digest.contains("n=3"), "{digest}");
        assert!(digest.contains("sha="), "{digest}");
        // 键名要留下 —— 排查时「这次带了哪些参数」比「参数是什么」常用得多
        assert!(digest.contains("api_key"), "{digest}");
        assert!(digest.contains("query"), "{digest}");
        assert!(digest.contains("top_k"), "{digest}");
    }

    #[test]
    fn 相同入参的摘要相同_不同入参不同() {
        let a = ToolCallAudit::argument_digest(&json!({"x": 1, "y": 2})).unwrap();
        let b = ToolCallAudit::argument_digest(&json!({"y": 2, "x": 1})).unwrap();
        // 键序不影响（serde_json::Value 的 Map 是 BTreeMap，序列化有序）
        assert_eq!(a, b, "键序不该改变摘要：{a} vs {b}");
        let c = ToolCallAudit::argument_digest(&json!({"x": 1, "y": 3})).unwrap();
        assert_ne!(a, c, "值变了摘要必须变");
    }

    #[test]
    fn 非对象入参没有摘要() {
        assert!(ToolCallAudit::argument_digest(&json!(null)).is_none());
        assert!(ToolCallAudit::argument_digest(&json!([1, 2, 3])).is_none());
    }

    #[test]
    fn 过长的错误被截断并留省略号() {
        let long = "e".repeat(MAX_ERROR_CHARS + 50);
        let got = ToolCallAudit::truncate_error(&long);
        assert_eq!(got.chars().count(), MAX_ERROR_CHARS + 1);
        assert!(got.ends_with('…'));
        // 短的原样保留
        assert_eq!(ToolCallAudit::truncate_error("短错误"), "短错误");
    }

    #[test]
    fn 错误按字符而不是字节截断() {
        // 中文按字节截断会切出半个字，产生替换字符
        let long = "错".repeat(MAX_ERROR_CHARS + 10);
        let got = ToolCallAudit::truncate_error(&long);
        assert!(!got.contains('\u{fffd}'), "截断切坏了多字节字符：{got}");
        assert_eq!(got.chars().count(), MAX_ERROR_CHARS + 1);
    }
}
