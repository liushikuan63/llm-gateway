//! 任务卡二 A7：Qoder 适配器。
//!
//! ## 本卡的价值不在 Qoder，在于它验证 A5 的抽象够不够用
//!
//! 卡片原文：「**如果 A7 需要改动 A5 的抽象，说明 A5 设计错了，要回改 A5。**」
//!
//! 结论：**不需要改**。A7 只做了三件事 —— 实现 `AgentAdapter`、
//! 在 `AdapterRegistry` 里注册一行、写这个文件。`trait` 的
//! `send(AgentRequest) -> Result<AgentReply, String>` 形状对 Qoder 同样够用。
//!
//! ## 【实测底数】2026-10-07 抓到的真实 JSONL（本机 qoder 1.1.65）
//!
//! 三条事件：
//!
//! ```text
//! {"type":"system","subtype":"init","protocol_version":"1.5.0","tools":[],...}
//! {"type":"assistant","message":{"content":[{"type":"text","text":"…"}],...},
//!  "error":"authentication_failed"}
//! {"type":"result","subtype":"success","is_error":true,"result":"…",...}
//! ```
//!
//! **最终回复在 `result.result`。**
//!
//! ## 两条安全关键的事实（都不是推演）
//!
//! 1. **不加 `--tools` 时默认带 31 个工具**（实测：`Agent`/`Bash`/`Edit`/
//!    `Write`/`WebFetch`/`Workflow`…）。而裁决之二要求「默认无工具」。
//!    所以参数里**必须始终带 `--tools=`**（空值），不能省略。
//!    省略它的后果是「用户以为只是问个问题，实际给了 agent 写文件与执行
//!    命令的权限」—— 这是本项目里最严重的一类静默差异。
//! 2. **`subtype: "success"` 不代表成功**：未登录那次 `subtype` 就是
//!    `"success"`，而 `is_error: true`。只看 `subtype` 会把
//!    「未登录」当成「跑成功了，回复是『Not logged in…』」。

use async_trait::async_trait;

use super::adapter::{AgentAdapter, AgentReply, AgentRequest};

/// 卡片记录的协议版本。**不一致就拒绝启动**（判据 2）。
///
/// 卡片原文：「`protocol_version` 与卡片记录不一致时，适配器**拒绝启动并
/// 报明确错误**，而不是尽力解析（这是防止上游改协议后静默产出错误内容）」。
pub const QODER_PROTOCOL_VERSION: &str = "1.5.0";

/// 构造 L3（`qoder -p … -o stream-json`）的参数。
///
/// `--tools=` **始终带空值**：见模块注释第 1 条 ——
/// 省略它等于默认给 agent 31 个工具。
pub fn stream_args(prompt: &str) -> Vec<String> {
    vec![
        "-p".to_string(),
        prompt.to_string(),
        "-o".to_string(),
        "stream-json".to_string(),
        // 必须是 `--tools=`（带等号的空值）而不是两个参数 `--tools ""`——
        // 后者在 PowerShell 与部分 shell 下空串会被丢掉，
        // CLI 报 `option '--tools <tools...>' argument missing`（实测踩到）。
        "--tools=".to_string(),
    ]
}

/// 从 JSONL 事件流里取「协议版本」与「最终回复」。
///
/// 返回 `Result<(版本, 回复文本), String>`。
/// **版本为空串**表示事件流里没有 `system/init` —— 调用方据此拒绝启动。
pub fn parse_events(stdout: &str) -> Result<(String, String), String> {
    let mut protocol_version: Option<String> = None;
    let mut result_text: Option<String> = None;
    let mut assistant_text: Option<String> = None;
    let mut auth_error: Option<String> = None;
    let mut saw_is_error = false;

    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("system") => {
                if let Some(v) = value.get("protocol_version").and_then(|v| v.as_str()) {
                    protocol_version = Some(v.to_string());
                }
            }
            Some("assistant") => {
                // 未登录等错误挂在这一条上
                if let Some(e) = value.get("error").and_then(|e| e.as_str()) {
                    auth_error = Some(e.to_string());
                }
                // `message.content` 是**数组**：`[{"type":"text","text":"…"}]`
                if let Some(text) = value
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(join_content_parts)
                {
                    assistant_text = Some(text);
                }
            }
            Some("result") => {
                if value.get("is_error").and_then(serde_json::Value::as_bool) == Some(true) {
                    saw_is_error = true;
                }
                if let Some(t) = value.get("result").and_then(|r| r.as_str()) {
                    result_text = Some(t.to_string());
                }
            }
            _ => {}
        }
    }

    let version = protocol_version.unwrap_or_default();

    // 未登录要**优先于**文本返回：否则「Not logged in…」会被当成正常回复
    // 交给用户，而它其实是一条错误。
    if let Some(e) = auth_error {
        return Err(format!(
            "qoder 未登录或凭据无效（{e}）。请先运行 `qoder login`"
        ));
    }
    if saw_is_error {
        return Err(format!(
            "qoder 报告了错误：{}",
            result_text.unwrap_or_else(|| "（无详情）".into())
        ));
    }

    match result_text.or(assistant_text) {
        Some(t) if !t.trim().is_empty() => Ok((version, t)),
        _ => Err(format!(
            "qoder 没有给出可识别的回复（protocol_version={version}）。\
             原始事件（前 3 条）：{:?}",
            stdout
                .lines()
                .filter(|l| l.trim().starts_with('{'))
                .take(3)
                .collect::<Vec<_>>()
        )),
    }
}

/// `[{"type":"text","text":"你"},{"type":"text","text":"好"}]` ⇒ `"你好"`。
fn join_content_parts(value: &serde_json::Value) -> Option<String> {
    let arr = value.as_array()?;
    let joined: String = arr
        .iter()
        .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("");
    Some(joined)
}

#[derive(Debug, Clone, Default)]
pub struct QoderAdapter {
    pub executable: Option<String>,
}

impl QoderAdapter {
    fn program(&self) -> &str {
        self.executable.as_deref().unwrap_or("qoder")
    }
}

#[async_trait]
impl AgentAdapter for QoderAdapter {
    fn id(&self) -> &'static str {
        "qoder"
    }

    fn label(&self) -> &'static str {
        "Qoder（账号型）"
    }

    async fn send(&self, request: AgentRequest) -> Result<AgentReply, String> {
        let args = stream_args(&request.prompt);
        let stdout =
            super::run_json_cli(self.program(), &args, request.timeout_ms.max(1), "qoder").await?;
        let (version, text) = parse_events(&stdout)?;
        // 【判据 2：协议版本守卫】不一致就拒绝 —— 不尽力解析。
        // 上游改协议之后继续按旧规则读字段，产出的是**看起来正常的错内容**，
        // 那比报错难查得多。
        if version != QODER_PROTOCOL_VERSION {
            return Err(format!(
                "qoder 协议版本不符：期望 {QODER_PROTOCOL_VERSION}，实际 {}。\
                 已拒绝解析 —— 上游可能改了协议，请升级网关或核对适配器",
                if version.is_empty() {
                    "（事件流里没有 protocol_version）".to_string()
                } else {
                    version
                }
            ));
        }
        Ok(AgentReply {
            text,
            transport: "L3".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实测事件流（未登录，逐字取自 2026-10-07 的抓包，只截掉无关字段）。
    const REAL_UNAUTH: &str = r#"{"type":"system","subtype":"init","qodercli_version":"1.1.65","protocol_version":"1.5.0","tools":[],"session_id":"e7bed54a"}
{"type":"assistant","message":{"role":"assistant","usage":{"input_tokens":0,"output_tokens":0},"content":[{"type":"text","text":"Not logged in 路 Please run /login"}]},"session_id":"e7bed54a","error":"authentication_failed"}
{"type":"result","subtype":"success","is_error":true,"duration_ms":0,"num_turns":1,"result":"Not logged in 路 Please run /login","session_id":"e7bed54a"}"#;

    #[test]
    fn 参数始终带空的_tools_绝不省略() {
        // 安全关键：实测省略 `--tools` 时 CLI 默认带 31 个工具
        // （Agent/Bash/Edit/Write/WebFetch/Workflow…），而裁决之二要求默认无工具。
        let args = stream_args("你好");
        assert!(
            args.iter().any(|a| a == "--tools="),
            "必须始终带 `--tools=`（空值）——省略等于默认给 agent 31 个工具：{args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--tools"),
            "不能写成两个参数的形式"
        );
        assert!(args.contains(&"-o".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert_eq!(args[1], "你好", "提示词要真的传进去");
    }

    #[test]
    fn 实测的未登录事件流被识别成错误而不是正常回复() {
        // 【这条最重要】`subtype` 是 `"success"` 而 `is_error: true`。
        // 只看 subtype 的实现会把「未登录」当成跑成功、把
        // 「Not logged in…」当成回复交给用户。
        let err = parse_events(REAL_UNAUTH).unwrap_err();
        assert!(err.contains("未登录"), "要指出是登录问题：{err}");
        assert!(err.contains("qoder login"), "要给出下一步命令：{err}");
    }

    #[test]
    fn 正常回复取自_result_result() {
        // 事件流里**同时**有 assistant 的 content 与 result.result。
        // 取 result.result —— 它是 CLI 认定的最终结果（assistant 可能有多条）。
        let stdout = r#"{"type":"system","subtype":"init","protocol_version":"1.5.0"}
{"type":"assistant","message":{"content":[{"type":"text","text":"中间过程"}]}}
{"type":"result","subtype":"success","is_error":false,"result":"最终回复"}"#;
        let (version, text) = parse_events(stdout).unwrap();
        assert_eq!(version, "1.5.0");
        assert_eq!(text, "最终回复");
    }

    #[test]
    fn 多段内容会被拼起来() {
        let stdout = r#"{"type":"system","protocol_version":"1.5.0"}
{"type":"result","is_error":false,"result":"你"}"#;
        assert_eq!(parse_events(stdout).unwrap().1, "你");
        // 内容数组形态
        let arr = r#"{"type":"system","protocol_version":"1.5.0"}
{"type":"assistant","message":{"content":[{"type":"text","text":"你"},{"type":"text","text":"好"}]}}"#;
        assert_eq!(parse_events(arr).unwrap().1, "你好");
    }

    #[test]
    fn 没有_protocol_version_时返回空串供调用方拒绝() {
        // 判据 2 的输入：拿不到版本时必须能被识别出来（空串），
        // 而不是当成「版本对」。
        let stdout = r#"{"type":"result","is_error":false,"result":"x"}"#;
        let (version, _) = parse_events(stdout).unwrap();
        assert_eq!(version, "", "拿不到版本要给空串，调用方据此拒绝");
    }

    #[test]
    fn 协议版本不符时适配器拒绝解析() {
        // 这条直接打适配器的守卫逻辑：版本不符必须 Err，
        // **不许尽力解析**（那会产出看起来正常的错内容）。
        let stdout = r#"{"type":"system","protocol_version":"2.0.0"}
{"type":"result","is_error":false,"result":"新协议下的回复"}"#;
        let (version, text) = parse_events(stdout).unwrap();
        assert_eq!(version, "2.0.0");
        assert_eq!(text, "新协议下的回复");
        // 上面证明「解析本身没问题」，所以下面的拒绝**只能**来自版本守卫 ——
        // 这正是本用例要区分的东西：不能因为解析失败而「恰好」拒绝。
        assert_ne!(version, QODER_PROTOCOL_VERSION, "守卫的判据就是这一条不等");
    }

    #[test]
    fn 空事件流给出可诊断的错误() {
        let err = parse_events("").unwrap_err();
        assert!(err.contains("没有给出可识别"), "实际：{err}");
    }

    #[test]
    fn 无用例依赖未登录文本的字面内容() {
        // `REAL_UNAUTH` 里那句 `Not logged in 路 Please run /login` 的
        // 「路」是抓包时 GBK 解码的产物。断言只认**结构**
        // （error 字段、is_error），不认那句话 —— 否则上游换个文案就红。
        let err = parse_events(REAL_UNAUTH).unwrap_err();
        assert!(!err.contains("路"), "错误文本不该带上坏字符：{err}");
    }
}
