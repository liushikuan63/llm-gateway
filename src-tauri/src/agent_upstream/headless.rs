//! Text-only CLI adapters. No arbitrary argv or permission overrides are accepted.
//! Sources: docs.qoder.cn/cli/cli-reference, code.claude.com/docs/en/cli-reference,
//! opencode.ai/docs/cli, opencode.ai/docs/permissions and official run.ts JSON events.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{AgentAdapter, AgentReply, AgentRequest};

/// Qoder CN documents `--mcp-config` as a file path. This non-secret, fixed
/// configuration belongs to one invocation and is removed even if it is cancelled.
struct EmptyMcpConfig(std::path::PathBuf);

impl EmptyMcpConfig {
    fn create() -> Result<Self, String> {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "llmgw-empty-mcp-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        if path.to_str().is_none() {
            return Err("Qoder CN 临时 MCP 文件路径不是有效 UTF-8".into());
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "创建 Qoder CN 临时 MCP 文件失败".to_string())?;
        let guard = Self(path);
        let written = file.write_all(b"{\"mcpServers\":{}}\n");
        // Close the file before error propagation so Windows can always remove it.
        drop(file);
        written.map_err(|_| "写入 Qoder CN 临时 MCP 文件失败".to_string())?;
        Ok(guard)
    }

    fn argument(&self) -> &str {
        // create() validates this before creating the owned file.
        self.0.to_str().expect("validated MCP file path")
    }
}

impl Drop for EmptyMcpConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    QoderCn,
    ClaudeCode,
    OpenCode,
}

impl ClientKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::QoderCn => "qoder-cn",
            Self::ClaudeCode => "claude-code",
            Self::OpenCode => "opencode",
        }
    }
    pub fn program(self) -> &'static str {
        match self {
            Self::QoderCn => "qodercn",
            Self::ClaudeCode => "claude",
            Self::OpenCode => "opencode",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CliInvocation {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<String>,
}

pub fn invocation(kind: ClientKind, model: &str, prompt: &str, agent_name: &str) -> CliInvocation {
    let mut env = Vec::new();
    let mut args: Vec<String> = match kind {
        ClientKind::QoderCn | ClientKind::ClaudeCode => vec![
            "--print",
            "--output-format",
            "json",
            "--tools=",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--setting-sources=",
            "--settings",
            "{\"disableAllHooks\":true}",
            "--no-session-persistence",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        ClientKind::OpenCode => {
            // A unique agent receives a final catch-all deny rule. Using the default build
            // agent or only a global '*' rule would inherit more-specific user allow rules.
            let config = json!({
                "permission": "deny", "share": "disabled", "autoupdate": false,
                "agent": { (agent_name): {
                    "description": "Gateway text-only model request", "mode": "primary",
                    "permission": "deny"
                }}
            });
            env.push(("OPENCODE_CONFIG_CONTENT".into(), config.to_string()));
            env.push(("OPENCODE_PERMISSION".into(), "{\"*\":\"deny\"}".into()));
            for key in [
                "OPENCODE_DISABLE_AUTOUPDATE",
                "OPENCODE_DISABLE_DEFAULT_PLUGINS",
                "OPENCODE_DISABLE_CLAUDE_CODE",
            ] {
                env.push((key.into(), "true".into()));
            }
            // Explicitly cancel inherited auto-share. No --share/--auto/--attach is used.
            env.push(("OPENCODE_AUTO_SHARE".into(), "false".into()));
            vec![
                "run".into(),
                "--format".into(),
                "json".into(),
                "--agent".into(),
                agent_name.into(),
            ]
        }
    };
    if kind == ClientKind::ClaudeCode {
        // Safe mode retains ordinary authentication while skipping user customizations.
        // Unsupported older versions fail before a model request rather than dropping flags.
        args.extend([
            "--safe-mode".into(),
            "--disallowedTools".into(),
            "mcp__*".into(),
        ]);
    }
    if !model.trim().is_empty() {
        args.extend(["--model".into(), model.into()]);
    }
    let stdin = if kind == ClientKind::OpenCode {
        Some(prompt.to_string())
    } else {
        args.extend(["--".into(), prompt.to_string()]);
        None
    };
    CliInvocation { args, env, stdin }
}

pub fn parse_reply(kind: ClientKind, stdout: &str) -> Result<String, String> {
    let label = kind.program();
    if kind != ClientKind::OpenCode {
        let value: Value = serde_json::from_str(stdout.trim()).map_err(|_| {
            format!("{label} 未返回有效的最终 JSON 回复；请核对 CLI 版本和登录状态")
        })?;
        if value.get("is_error").and_then(Value::as_bool) != Some(false)
            || value.get("type").and_then(Value::as_str) != Some("result")
            || value.get("subtype").and_then(Value::as_str) != Some("success")
        {
            return Err(format!(
                "{label} 未完成请求；请核对登录、模型和配额（CLI 返回错误或非成功结果）"
            ));
        }
        return value
            .get("result")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("{label} 最终回复没有文本"));
    }
    let mut text = String::new();
    let mut finished = false;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)
            .map_err(|_| "opencode JSON 事件损坏，已拒绝返回部分回复".to_string())?;
        match value.get("type").and_then(Value::as_str) {
            Some("error") => return Err("opencode 请求失败；请核对登录、模型和配额".into()),
            Some("tool_use") => {
                return Err("opencode 请求包含工具调用，文本透传模式已拒绝该结果".into())
            }
            Some("text") => {
                let part = value
                    .get("part")
                    .and_then(|part| part.get("text"))
                    .and_then(Value::as_str)
                    .ok_or("opencode 文本事件缺少 part.text")?;
                text.push_str(part);
                finished = false;
            }
            Some("step_finish") => {
                finished = value
                    .get("part")
                    .and_then(|part| part.get("reason"))
                    .and_then(Value::as_str)
                    == Some("stop");
            }
            _ => {}
        }
    }
    if !finished || text.trim().is_empty() {
        return Err("opencode 未返回完整的文本回复（缺少最终 stop 事件）".into());
    }
    Ok(text)
}

/// Inspect the CLI's resolved configuration before sending any model prompt.
/// `opencode debug config` is the official redacted, read-only configuration command.
/// Missing agents fall back to an ordinary default in `run`, so argv alone is insufficient.
pub fn verify_opencode_config(stdout: &str, agent_name: &str) -> Result<(), String> {
    let config: Value = serde_json::from_str(stdout)
        .map_err(|_| "opencode 配置预检未返回有效 JSON；请升级官方 CLI".to_string())?;
    let agent = config
        .get("agent")
        .and_then(|agents| agents.get(agent_name))
        .ok_or("opencode 未加载网关的文本 Agent；已阻止回落到默认工具 Agent")?;
    let permission = agent
        .get("permission")
        .ok_or("opencode 文本 Agent 缺少权限配置")?;
    let deny = permission.as_str() == Some("deny")
        || permission.as_object().is_some_and(|rules| {
            rules.get("*").and_then(Value::as_str) == Some("deny")
                && rules.values().all(|value| value.as_str() == Some("deny"))
        });
    if !deny
        || agent.get("mode").and_then(Value::as_str) != Some("primary")
        || agent.get("disable").and_then(Value::as_bool) == Some(true)
    {
        return Err("opencode 文本 Agent 的全禁工具配置未生效；已停止调用".into());
    }
    if config.get("share").and_then(Value::as_str) != Some("disabled") {
        return Err("opencode 禁止会话共享的配置未生效；已停止调用".into());
    }
    for key in ["plugin", "plugin_origins"] {
        if config
            .get(key)
            .is_some_and(|value| value.as_array().map_or(true, |plugins| !plugins.is_empty()))
        {
            return Err(
                "opencode 配置含用户插件；文本透传入口不加载这些扩展，请使用独立的无插件配置"
                    .into(),
            );
        }
    }
    if config.get("mcp").is_some_and(|value| {
        value.as_object().map_or(true, |servers| {
            servers
                .values()
                .any(|server| server.get("enabled").and_then(Value::as_bool) != Some(false))
        })
    }) {
        return Err(
            "opencode 配置含已启用的 MCP；文本透传入口已停止，请先在 CLI 配置中关闭 MCP".into(),
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct HeadlessAdapter {
    pub kind: ClientKind,
    pub executable: Option<String>,
}

impl HeadlessAdapter {
    pub fn new(kind: ClientKind, executable: Option<String>) -> Self {
        Self { kind, executable }
    }
}

#[async_trait]
impl AgentAdapter for HeadlessAdapter {
    fn id(&self) -> &'static str {
        self.kind.id()
    }
    fn label(&self) -> &'static str {
        match self.kind {
            ClientKind::QoderCn => "Qoder CN（文本透传）",
            ClientKind::ClaudeCode => "Claude Code（文本透传）",
            ClientKind::OpenCode => "OpenCode（禁止工具调用）",
        }
    }
    async fn send(&self, request: AgentRequest) -> Result<AgentReply, String> {
        let began = std::time::Instant::now();
        let name = format!("llmgw-text-{}", uuid::Uuid::new_v4().simple());
        let mut invocation = invocation(self.kind, &request.model, &request.prompt, &name);
        let empty_mcp = if self.kind == ClientKind::QoderCn {
            let file = EmptyMcpConfig::create()?;
            let value = invocation
                .args
                .windows(2)
                .position(|args| args[0] == "--mcp-config")
                .ok_or("Qoder CN 缺少受管 MCP 参数")?
                + 1;
            invocation.args[value] = file.argument().to_string();
            Some(file)
        } else {
            None
        };
        let program = self.executable.as_deref().unwrap_or(self.kind.program());
        if self.kind == ClientKind::OpenCode {
            let stdout = super::run_json_cli_with_input(
                program,
                &["debug".into(), "config".into()],
                request.timeout_ms.clamp(1, 10_000),
                "opencode-config",
                &invocation.env,
                None,
            )
            .await?;
            verify_opencode_config(&stdout, &name)?;
        }
        let remaining = request
            .timeout_ms
            .saturating_sub(began.elapsed().as_millis().min(u64::MAX as u128) as u64);
        if remaining == 0 {
            return Err("CLI 配置预检超时，尚未发送模型请求".into());
        }
        let stdout = super::run_json_cli_with_input(
            program,
            &invocation.args,
            remaining,
            self.kind.id(),
            &invocation.env,
            invocation.stdin.as_deref(),
        )
        .await?;
        drop(empty_mcp);
        Ok(AgentReply {
            text: parse_reply(self.kind, &stdout)?,
            transport: "L3".into(),
        })
    }
}
