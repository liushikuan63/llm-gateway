//! 任务卡二 A6：Codex 适配器。
//!
//! ## 本机实测的底数（2026-10-07）
//!
//! `codex --help` 证实卡片说的两条路都存在：
//! `exec`（非交互，L3 用）与 `exec-server`（`[EXPERIMENTAL]`，L1 用）。
//! 本文件先落 **L3**（`codex exec` 一次性跑一轮），因为：
//!
//! - L1 要起一个常驻服务、管它的生命周期与端口，是另一个量级的工作；
//! - L3 已经能满足「跑一轮拿回复」这个最小闭环，且**失败模式简单**。
//!
//! ## 【硬约束】不许用 `--dangerously-bypass-approvals-and-sandbox`
//!
//! 这个参数真实存在（`codex exec --help` 里能看到），名字已经说明它做什么。
//! 本项目的铁律要求**子进程 cwd 隔离**，绕沙箱是反方向。
//! 参数表里没有它，用例里也断言它**不会**出现在构造出的参数里。
//!
//! ## 【实测硬事实】它会静默挂住
//!
//! `codex exec --json --skip-git-repo-check --ephemeral "say hi"` 在临时目录里
//! 跑了 **90 秒零输出且不结束**。所以超时是这条路径上必然触发的分支，
//! 不是防御性代码 —— 见 [`EXEC_TIMEOUT_MS`]。

use async_trait::async_trait;

use super::adapter::{AgentAdapter, AgentReply, AgentRequest};

/// `codex exec` 的默认超时。
///
/// 【不是拍脑袋】实测它会静默挂住（90 秒零输出且不结束）。
/// 这个值要比「正常的 codex 跑一轮」宽裕得多，同时不能让用户干等 ——
/// 取 5 分钟：正常的一轮（含工具调用）通常在 1 分钟以内，
/// 而挂住的情况下用户在 5 分钟内一定能拿到明确报错而不是无限等待。
pub const EXEC_TIMEOUT_MS: u64 = 300_000;

/// 构造 `codex exec` 的参数。
///
/// **纯函数**，所以「不许用哪个参数」这件事可以直接断言。
///
/// 参数逐个有理由：
/// - `exec`：非交互模式（交互模式会挂住等输入）
/// - `--json`：事件流，L3 靠它取最终消息
/// - `--skip-git-repo-check`：cwd 是隔离出来的临时目录，**不是 git 仓库**，
///   不加这个参数 codex 会直接拒绝运行
/// - `--ephemeral`：不落会话记录 —— 网关的会话由网关自己管，
///   让 CLI 再存一份会让「哪份是权威」说不清，也会在用户机器上堆垃圾
pub fn exec_args(model: &str, prompt: &str) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        "--json".to_string(),
        "--skip-git-repo-check".to_string(),
        "--ephemeral".to_string(),
    ];
    if !model.trim().is_empty() {
        args.push("-m".to_string());
        args.push(model.to_string());
    }
    args.push(prompt.to_string());
    args
}

/// 从 `--json` 的事件流里取最终回复。
///
/// ## 【为什么写得这么宽容】形状还没抓到
///
/// 真实的 `--json` 输出形状**没拿到** —— 实测那次它挂住了。
/// 项目踩过「mock 形状与真实不一致」的坑（`CLAUDE.md` 纪律 9），
/// 所以这里**不按猜出来的字段名硬解析**，而是：
///
/// 1. 逐行当 JSON 解析，收集所有能解析的对象；
/// 2. 按一组**候选路径**找文本（`item.text` / `msg.content` / `text` …），
///    取**最后一个**非空的 —— 事件流里最后一条助手消息就是结果；
/// 3. 一个都找不到时，**把见过的原始行放进错误里**。
///
/// 第 3 条是关键：第一次真跑的时候，错误信息本身就把形状告诉我们了，
/// 不必再去猜或让用户帮忙抓包。这比「先猜一版，跑不通再改」少一轮往返。
pub fn extract_final_message(stdout: &str) -> Result<String, String> {
    let mut last_text: Option<String> = None;
    let mut parsed_any = false;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        parsed_any = true;
        if let Some(text) = find_text(&value) {
            if !text.trim().is_empty() {
                last_text = Some(text);
            }
        }
    }
    if let Some(text) = last_text {
        return Ok(text);
    }
    // 诊断信息里带上**见过的原始行**，让第一次真跑的报错直接说明形状
    let sample: Vec<&str> = stdout
        .lines()
        .filter(|l| l.trim().starts_with('{'))
        .take(3)
        .collect();
    if !parsed_any {
        return Err(format!(
            "codex 没有输出任何 JSON 事件（stdout 前几行：{:?}）—— \
             检查是否已登录（`codex login`）",
            stdout.lines().take(3).collect::<Vec<_>>()
        ));
    }
    Err(format!(
        "codex 输出了 JSON 事件，但没找到可识别的回复文本。原始事件（前 3 条）：{sample:?}"
    ))
}

/// 在一棵 JSON 里找「助手说的话」。
///
/// 候选路径按**从具体到宽泛**排：先看已知的事件类型字段，
/// 最后才退回「任何叫 text/content 的字符串」。
/// 顺序反过来的话，一个 `{"type":"tool_call","name":"text"}` 之类
/// 会先被当成回复。
fn find_text(value: &serde_json::Value) -> Option<String> {
    // 明确是「消息」类的才有资格
    for path in [
        &["item", "text"][..],
        &["msg", "content"][..],
        &["message", "content"][..],
        &["text"][..],
        &["content"][..],
        &["item", "content"][..],
    ] {
        let mut cursor = value;
        let mut ok = true;
        for key in path {
            match cursor.get(*key) {
                Some(next) => cursor = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if let Some(s) = cursor.as_str() {
                return Some(s.to_string());
            }
            // OpenAI 风格的内容数组
            if let Some(arr) = cursor.as_array() {
                let joined: String = arr
                    .iter()
                    .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("");
                if !joined.is_empty() {
                    return Some(joined);
                }
            }
        }
    }
    None
}

/// L3 适配器：`codex exec --json` 一次性跑一轮。
#[derive(Debug, Clone, Default)]
pub struct CodexAdapter {
    /// 可执行文件。默认 `codex`（走 PATH）。
    /// 允许覆盖是为了测试能塞一个 mock 进去 —— 不覆盖就没法在没登录的
    /// 机器上测这条路径。
    pub executable: Option<String>,
}

impl CodexAdapter {
    pub fn with_executable(exe: impl Into<String>) -> Self {
        Self {
            executable: Some(exe.into()),
        }
    }

    fn program(&self) -> &str {
        self.executable.as_deref().unwrap_or("codex")
    }
}

#[async_trait]
impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn label(&self) -> &'static str {
        "Codex（账号型）"
    }

    async fn send(&self, request: AgentRequest) -> Result<AgentReply, String> {
        let args = exec_args(&request.model, &request.prompt);
        let stdout =
            super::run_json_cli(self.program(), &args, request.timeout_ms.max(1), "codex").await?;
        let text = extract_final_message(&stdout)?;
        Ok(AgentReply {
            text,
            // 卡片 A6 判据 1 要求审计行记录**实际**走的传输。
            // 本适配器目前只实现了 L3，所以如实写 L3。
            transport: "L3".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 参数里绝不出现绕过沙箱的开关() {
        // 这是本文件的硬约束，用例直接钉住它 ——
        // 名字已经说明 `--dangerously-bypass-approvals-and-sandbox` 做什么，
        // 而本项目铁律要求子进程 cwd 隔离。
        let args = exec_args("gpt-5", "你好");
        assert!(
            !args.iter().any(|a| a.contains("dangerously")),
            "绝不许绕过沙箱：{args:?}"
        );
        assert!(!args.iter().any(|a| a.contains("bypass")), "同上：{args:?}");
    }

    #[test]
    fn 参数包含非交互与隔离所需的四项() {
        let args = exec_args("gpt-5", "你好");
        assert_eq!(args[0], "exec", "必须是非交互模式，否则会挂住等输入");
        for required in ["--json", "--skip-git-repo-check", "--ephemeral"] {
            assert!(
                args.contains(&required.to_string()),
                "缺 {required}：{args:?}"
            );
        }
        // 模型与提示词都真的传进去了
        assert!(args.contains(&"gpt-5".to_string()));
        assert_eq!(args.last().unwrap(), "你好");
    }

    #[test]
    fn 空模型名时不传_m_参数() {
        // 空模型名交给 codex 用它的默认值，而不是传一个空串过去
        // —— 后者会让 CLI 报「model 为空」这种没意义的错。
        let args = exec_args("", "你好");
        assert!(!args.contains(&"-m".to_string()), "实际：{args:?}");
        assert_eq!(args.last().unwrap(), "你好");
    }

    #[test]
    fn 解析取最后一条助手消息() {
        // 事件流形态按候选路径的关键字段构造 —— 形状未确认，
        // 但**取最后一条**这个语义是确定的（前面的都是过程）。
        let stdout = r#"{"type":"start","item":{"text":"开场"}}
{"type":"agent_message","item":{"text":"中间"}}
{"type":"agent_message","item":{"text":"最终回复"}}"#;
        assert_eq!(extract_final_message(stdout).unwrap(), "最终回复");
    }

    #[test]
    fn 解析容忍非_json_的杂音行() {
        // 真实 CLI 常在事件流里混入日志/banner。混进去不该让整条解析失败。
        let stdout = "Reading config...\n{\"item\":{\"text\":\"结果\"}}\nDone.\n";
        assert_eq!(extract_final_message(stdout).unwrap(), "结果");
    }

    #[test]
    fn 解析支持_openai_风格的内容数组() {
        let stdout = r#"{"message":{"content":[{"text":"你"},{"text":"好"}]}}"#;
        assert_eq!(extract_final_message(stdout).unwrap(), "你好");
    }

    #[test]
    fn 零输出时给出可诊断的错误而不是空回复() {
        // 这正是实测那次挂住之后（超时杀掉）会看到的东西。
        // 错误里必须提示「检查是否已登录」—— 那是这个现象最常见的原因。
        let err = extract_final_message("").unwrap_err();
        assert!(err.contains("没有输出任何 JSON"), "实际：{err}");
        assert!(err.contains("codex login"), "要点出最可能的原因：{err}");
    }

    #[test]
    fn 有事件但认不出文本时把原始事件带进错误() {
        // 【这条是给第一次真跑用的】形状没抓到，所以报错要把见过的
        // 原始事件带出来 —— 那样第一轮真跑的报错本身就说明了形状，
        // 不必再去猜或让用户抓包。
        let stdout = r#"{"type":"unknown_event","payload":{"foo":1}}"#;
        let err = extract_final_message(stdout).unwrap_err();
        assert!(err.contains("unknown_event"), "错误里要带原始事件：{err}");
        assert!(err.contains("payload"), "要带上足够分辨形状的内容：{err}");
    }

    #[test]
    fn 只有空白文本的事件不算回复() {
        let stdout = r#"{"item":{"text":"   "}}"#;
        assert!(
            extract_final_message(stdout).is_err(),
            "空白不该被当成有效回复 —— 那会让上层以为跑成功了"
        );
    }
}

#[cfg(test)]
mod run_json_cli_tests {
    use super::super::run_json_cli;

    /// **超时必现**：实测 `codex exec --json` 会静默挂住
    /// （90 秒零输出且不结束）。这条用一个必然挂住的程序模拟它，
    /// 断言 `run_json_cli` 会**在超时后返回**，而不是永远等下去。
    ///
    /// 判据不只是「返回了错误」，还有**耗时**：必须在超时附近返回。
    /// 只断言错误文本的话，一个「等了 10 分钟才超时」的实现也能通过。
    #[tokio::test]
    async fn 挂住的程序会在超时后被终止并返回() {
        #[cfg(windows)]
        let (program, args) = (
            "cmd".to_string(),
            vec!["/C".to_string(), "ping -n 60 127.0.0.1 > NUL".to_string()],
        );
        #[cfg(not(windows))]
        let (program, args) = ("sleep".to_string(), vec!["60".to_string()]);

        let started = std::time::Instant::now();
        let err = run_json_cli(&program, &args, 1_000, "mock-hang")
            .await
            .expect_err("挂住的程序必须超时报错，而不是等到它自己结束");
        let elapsed = started.elapsed();

        assert!(
            err.contains("没有结束") && err.contains("已终止"),
            "错误要说清是超时终止：{err}"
        );
        assert!(err.contains("登录"), "要点出最可能的原因（未登录）：{err}");
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "必须在超时附近返回，实际用了 {elapsed:?} —— \
             说明它没有真的超时，而是在等子进程自己结束"
        );
    }

    /// 正常结束的程序：拿到 stdout，不报错。
    #[tokio::test]
    async fn 正常结束的程序返回_stdout() {
        // 用**不含引号**的输出：这条用例验的是「stdout 管道通不通」，
        // 而 `cmd /C echo {"a":"b"}` 的引号会被 cmd 自己吃掉
        // （第一版就是这么写的，被用例抓到）。JSON 解析另有用例覆盖。
        #[cfg(windows)]
        let (program, args) = (
            "cmd".to_string(),
            vec!["/C".to_string(), "echo hello-from-mock".to_string()],
        );
        #[cfg(not(windows))]
        let (program, args) = (
            "sh".to_string(),
            vec!["-c".to_string(), "echo hello-from-mock".to_string()],
        );

        let stdout = run_json_cli(&program, &args, 30_000, "mock-echo")
            .await
            .expect("正常退出的程序不该报错");
        assert!(
            stdout.contains("hello-from-mock"),
            "stdout 必须原样回来，实际：{stdout:?}"
        );
    }

    /// 非零退出：错误里要带 stderr 的**前几行** —— 那里面通常就是原因。
    #[tokio::test]
    async fn 非零退出时把_stderr_带进错误() {
        #[cfg(windows)]
        let (program, args) = (
            "cmd".to_string(),
            vec!["/C".to_string(), "echo 未登录 >&2 & exit /b 3".to_string()],
        );
        #[cfg(not(windows))]
        let (program, args) = (
            "sh".to_string(),
            vec!["-c".to_string(), "echo 未登录 >&2; exit 3".to_string()],
        );

        let err = run_json_cli(&program, &args, 30_000, "mock-fail")
            .await
            .expect_err("非零退出必须报错");
        assert!(err.contains("未登录"), "stderr 内容要出现在错误里：{err}");
        assert!(err.contains("退出码"), "要带上退出码：{err}");
    }

    /// 程序不存在：给一句能照做的提示，而不是裸的 OS 错误。
    #[tokio::test]
    async fn 程序不存在时提示去检查安装() {
        let err = run_json_cli("绝对不存在的程序-xyz", &[], 5_000, "mock-missing")
            .await
            .expect_err("不存在的程序必须报错");
        assert!(err.contains("启动"), "实际：{err}");
        assert!(
            err.contains("PATH"),
            "要告诉用户下一步做什么（装它 / 加 PATH）：{err}"
        );
    }
}
