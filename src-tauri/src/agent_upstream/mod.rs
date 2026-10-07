//! 任务卡二 A5：让 Provider 有**第二种上游形态** —— 账号型 Agent。
//!
//! ## 为什么不另起一套 Provider
//!
//! 账号型上游（Codex / Qoder / Claude Code…）与普通 API 上游的差别只在
//! **「怎么把一轮对话发出去、怎么把结果拿回来」**。选谁、怎么排、
//! 花多少钱、怎么审计 —— 那些逻辑对两者完全一样。
//!
//! 所以做法是给 `Provider` 加一个 `runtime_id` 列：
//! **为空 ⇒ 走现有的 HTTP 直连路径（逐字节不变）；
//! 有值 ⇒ 把「发出去」这一步委托给一个 [`AgentAdapter`]。**
//!
//! ## 模式隔离（卡片判据 1、2）
//!
//! `runtime_id` 为 `NULL` 时，分派点必须**完全不改变**既有行为。
//! 这一条由既有测试（`router.rs` / `server_e2e.rs` / `db.rs`）退出码 0
//! 加一条负向对照守着 —— 见 `tests/agent_upstream.rs`。

pub mod adapter;
pub mod codex;
pub mod fake;
pub mod plugin;
pub mod plugin_process;
pub mod qoder;
pub mod quota;
pub mod run;
pub mod workspace;

pub use adapter::{AgentAdapter, AgentReply, AgentRequest};
pub use codex::CodexAdapter;
pub use fake::FakeAdapter;
pub use qoder::QoderAdapter;
pub use quota::{AgentQuota, ConcurrencyGate, ConcurrencyRejection, QuotaBook, QuotaRejection};
pub use run::{run_agent, run_agent_request, AgentRunOutcome};
pub use workspace::WorkspaceRoot;

use std::collections::BTreeMap;
use std::sync::Arc;

/// 适配器注册表：`runtime_id` → 适配器。
///
/// **用注册表而不是 `match`**：A6/A7 会陆续加真适配器，
/// 而 `match` 每加一个都要改分派点本身 —— 那正是「唯一分派点」
/// 想要避免的事（分派点改得越少，模式隔离越安全）。
#[derive(Default)]
pub struct AdapterRegistry {
    // `BTreeMap` 而不是 `HashMap`：`ids()` 要给出稳定顺序，
    // 否则设置页的适配器列表每次刷新都在跳。
    adapters: BTreeMap<String, Arc<dyn AgentAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 只装假适配器。**生产环境也装它** ——
    /// 它是 A5 判据里「把上层管线在没有真实账号时也测起来」的那一件，
    /// 而它固定返回一段文本、不碰任何外部进程或凭据，
    /// 留在注册表里的风险是零。
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(Arc::new(FakeAdapter::default()));
        // A6：Codex（L3 = `codex exec --json`）。它**不需要登录也能注册** ——
        // 未登录会在 `send` 时给出可读错误，而不是在注册时失败。
        // 这样「配了但没登录」的用户拿到的是明确提示，而不是「找不到适配器」。
        registry.register(Arc::new(CodexAdapter::default()));
        // A7：Qoder。**它的存在就是 A5 抽象的验收** —— 加它只做了三件事：
        // 实现 trait、注册一行、写 qoder.rs。**没有改动 A5 的任何抽象。**
        registry.register(Arc::new(QoderAdapter::default()));
        registry
    }

    pub fn register(&mut self, adapter: Arc<dyn AgentAdapter>) {
        self.adapters.insert(adapter.id().to_string(), adapter);
    }

    pub fn get(&self, runtime_id: &str) -> Option<Arc<dyn AgentAdapter>> {
        self.adapters.get(runtime_id).cloned()
    }

    /// 已注册的 `runtime_id`，按字典序。供设置页列出可选项。
    pub fn ids(&self) -> Vec<&str> {
        self.adapters.keys().map(String::as_str).collect()
    }

    /// 解析 `runtime_id`。
    ///
    /// ## 为什么要单独一个函数（卡片判据 3）
    ///
    /// 「指向不存在的适配器」必须给出**可读错误**，
    /// 不是 500、不是 panic。做成 `Result` 而不是 `Option` 是为了
    /// 让调用方**必须**处理 —— `Option` 很容易被 `.unwrap()` 或被
    /// `if let` 静默跳过，而静默跳过的后果是**回落到 HTTP 直连**：
    /// 一个配了 `runtime_id = "codexx"`（拼错）的 Provider
    /// 会悄悄按普通 API 发出去，带着一个空 Key 去连真实上游。
    ///
    /// `runtime_id` 为 `None`（列是 NULL）返回 `Ok(None)` ——
    /// 那是**正常情况**，不是错误。
    pub fn resolve(
        &self,
        runtime_id: Option<&str>,
    ) -> Result<Option<Arc<dyn AgentAdapter>>, String> {
        match runtime_id {
            None => Ok(None),
            // 空串与 NULL 同等对待：配置文件与前端都可能写出 `""`，
            // 而把它当成「有个叫空字符串的适配器」显然不对。
            Some(raw) if raw.trim().is_empty() => Ok(None),
            Some(raw) => match self.get(raw) {
                Some(adapter) => Ok(Some(adapter)),
                None => Err(format!("未知账号运行时：{raw}")),
            },
        }
    }
}

// ==================== A5：唯一分派点用的两个入口 ====================

/// `Role` 的协议写法。领域枚举没有提供这个 —— 它不该关心协议拼写，
/// 而**这里需要**（提示词是给账号型 CLI 看的纯文本）。
fn role_label(role: &crate::domain::Role) -> &'static str {
    use crate::domain::Role;
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// 把消息历史拼成一段提示词。
///
/// 账号型上游收的是**一轮对话**（`codex exec "…"` 那类），不是消息数组 ——
/// 各家的消息格式都不一样，逐家实现一遍不如在分派点做一次归一。
///
/// **不做任何「智能压缩」**：拼接必须是无损的、可预测的。
/// 智能裁剪属于上下文管理，那是网关侧的会话职责（见 `context` 模块），
/// 在这里再做一次会让「发出去的到底是什么」变得说不清。
pub fn flatten_messages(messages: &[crate::domain::Message]) -> String {
    messages
        .iter()
        .map(|m| format!("{}: {}", role_label(&m.role), m.content_text()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 走账号型上游发一轮。**这是「唯一分派点」调用的那个函数。**
///
/// ## 三种失败各有各的状态码（卡片判据 3）
///
/// - `runtime_id` 解析不出来 ⇒ `ModelNotFound` ⇒ **404**
///   （错误文本是「未知账号运行时：xxx」）
/// - 适配器自己报错（未登录、命令不存在…）⇒ `CapabilityUnavailable` ⇒ **400**
///   —— 用户能改的东西，给 4xx；让他知道「这不是服务器的锅」
/// - 超时 ⇒ `Timeout` ⇒ 504
///
/// **一律不是 500** —— 500 意味着「服务端有 bug」，而这三件事
/// 全是配置或环境问题，给 500 会把用户引向错误的排查方向。
pub async fn call_agent(
    registry: &AdapterRegistry,
    runtime_id: &str,
    model: &str,
    messages: &[crate::domain::Message],
    timeout_ms: u64,
) -> Result<crate::domain::ChatResponse, crate::error::GatewayError> {
    let adapter = registry
        .resolve(Some(runtime_id))
        .map_err(crate::error::GatewayError::ModelNotFound)?;
    // `resolve(Some(非空))` 成功时一定是 `Some`；`None` 只在空串时出现，
    // 而空串在上面已经被 `resolve` 挡成 `Ok(None)` 了 —— 这里再挡一次
    // 是为了不写 `unwrap`（那会在将来有人改 `resolve` 时变成一个 panic）。
    let Some(adapter) = adapter else {
        return Err(crate::error::GatewayError::ModelNotFound(format!(
            "未知账号运行时：{runtime_id}"
        )));
    };
    let prompt = flatten_messages(messages);
    let reply = adapter
        .send(AgentRequest {
            model: model.to_string(),
            prompt,
            timeout_ms,
        })
        .await
        .map_err(|e| {
            // 「超时」要单独归类：它是**唯一**一个「重试可能有用」的失败，
            // 而其余（未登录、命令不存在）重试一万次也一样。
            if e.contains("超时") {
                crate::error::GatewayError::Timeout(e)
            } else {
                // 用 `Upstream` 而不是 `CapabilityUnavailable`：
                // 后者的消息是「请为相应模型勾选对应能力后重试」，
                // 用在「适配器起不来」上会把用户引向完全无关的地方。
                // `Upstream { status: 400 }` 走 `from_u16` ⇒ 400 ⇒ 4xx。
                crate::error::GatewayError::Upstream {
                    provider: runtime_id.to_string(),
                    model: model.to_string(),
                    status: 400,
                    body: e,
                }
            }
        })?;
    // 【临时落点，不是终态】卡片 A6 判据 1 要求审计行记录
    // `runtime_kind` 与**实际**走的 `transport`（L1/L3），而 `requests`
    // 表还没有这两列 —— 那笔账要动 11 处 `RequestLog` 字面量，独立一笔。
    //
    // 在那之前，这里先用日志把它记下来：`AgentReply.transport` 是
    // **适配器如实回报**的值（假适配器报 "fake"、本适配器报 "L3"），
    // 有它在日志里就**不是「只写不读」**（铁律 9），需要时也能从
    // 运行日志追出「这一次到底走了哪条路」。
    //
    // 还账后这一行应当**删掉**（审计列才是权威），别让它变成两份真相。
    tracing::info!(
        runtime_kind = runtime_id,
        agent_transport = %reply.transport,
        model = model,
        "账号型上游返回"
    );
    Ok(reply.into_chat_response(model))
}

/// 跑一个「输出 JSON 行」的外部 CLI，带**超时 + 杀进程树 + cwd 隔离**。
///
/// ## 三件事都不是防御性代码
///
/// 1. **超时**：实测 `codex exec --json` 会静默挂住（90 秒零输出且不结束）。
///    没有超时的话这个请求会永远挂住，而用户看到的是「一直在转」。
/// 2. **杀进程树**：`codex` 在本机是包装器脚本（`.cmd` + `.ps1`），
///    真正的 node 进程是**孙进程**。只杀直接子进程会留下它继续占资源 ——
///    见 `crate::proc_util`（那份实现有真起孙进程的用例守着）。
/// 3. **cwd 隔离**：账号型 CLI 会在 cwd 里读写文件（甚至改 git 仓库）。
///    让它们跑在网关进程的 cwd 里等于把用户的项目目录交给一个
///    「不受我们控制的工具」去操作。每个 runtime kind 一个隔离目录。
///
/// `label` 只用于错误文本（如 `codex`），让用户知道是**哪个**工具出的问题。
pub async fn run_json_cli(
    program: &str,
    args: &[String],
    timeout_ms: u64,
    label: &str,
) -> Result<String, String> {
    use std::process::Stdio;

    let workdir = crate::mcp::stdio::isolated_workdir(&format!("agent-{label}"));
    // 目录建不出来就报错而不是退回当前目录 —— 「悄悄跑在用户的项目目录里」
    // 比「起不来」危险得多。
    std::fs::create_dir_all(&workdir).map_err(|e| {
        format!(
            "创建 {label} 的隔离工作目录失败（{}）：{e}",
            workdir.display()
        )
    })?;

    let mut child = tokio::process::Command::new(program)
        .args(args)
        .current_dir(&workdir)
        .stdin(Stdio::null()) // 交互式 CLI 读到 stdin 会等输入 —— 直接关掉
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            // 「命令不存在」是最常见的失败，给一句能照做的提示
            format!("启动 {label} 失败（{program}）：{e}。请确认它已安装并在 PATH 里")
        })?;

    // 【为什么不用 `wait_with_output`】它**取走** `child`（`self` 按值），
    // 于是超时分支里再也没有 `child` 可杀 —— 而 `kill_on_drop(true)` 会先
    // 杀掉直接子进程，之后 `taskkill /T` **就找不到孙进程了**
    // （树是从父进程往上走的）。所以必须：先取走两根管道、并发读，
    // 让 `child` 一直活到超时分支里。
    use tokio::io::AsyncReadExt;
    let mut out_pipe = child.stdout.take().ok_or("拿不到 stdout 管道")?;
    let mut err_pipe = child.stderr.take().ok_or("拿不到 stderr 管道")?;
    // 并发读，避免「输出塞满管道缓冲 → 子进程阻塞 → 永远等不到超时结束」
    let out_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = out_pipe.read_to_string(&mut s).await;
        s
    });
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s).await;
        s
    });

    let status = match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait(),
    )
    .await
    {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            crate::proc_util::kill_tree(&mut child).await;
            return Err(format!("{label} 进程异常：{e}"));
        }
        Err(_) => {
            // 超时：**杀树**，不是杀进程。此刻 `child` 还活着，
            // 树是完整的，`taskkill /T` 才走得通。
            crate::proc_util::kill_tree(&mut child).await;
            let secs = timeout_ms / 1000;
            return Err(format!(
                "{label} 在 {secs} 秒内没有结束，已终止。\
                 常见原因：未登录（试 `{label} login`）、需要交互输入、或网络不通"
            ));
        }
    };
    let stdout = out_task.await.unwrap_or_default();
    let stderr = err_task.await.unwrap_or_default();

    if !status.success() {
        let stderr = stderr.as_str();
        // 失败时把 stderr 的**前几行**带上 —— 那里面通常就是原因
        // （未登录、配置错、模型名不对），比一个退出码有用得多。
        let hint: String = stderr.lines().take(3).collect::<Vec<_>>().join(" / ");
        return Err(format!(
            "{label} 退出码 {}：{}",
            status.code().unwrap_or(-1),
            if hint.trim().is_empty() {
                "（stderr 为空）".to_string()
            } else {
                hint
            }
        ));
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> AdapterRegistry {
        AdapterRegistry::with_builtins()
    }

    #[test]
    fn 空_runtime_id_走原有路径而不是报错() {
        // 这是模式隔离的入口：绝大多数 Provider 没有 runtime_id，
        // 它们必须原样走 HTTP 直连。
        let r = registry();
        assert!(matches!(r.resolve(None), Ok(None)));
        assert!(matches!(r.resolve(Some("")), Ok(None)));
        assert!(matches!(r.resolve(Some("   ")), Ok(None)));
    }

    #[test]
    fn 未知_runtime_id_给出可读错误而不是_panic() {
        // 卡片判据 3 原文：「返回**可读错误**（`未知账号运行时：xxx`），
        // 不是 500、不是 panic」。
        let r = registry();
        // 不能用 `expect_err`：它要求 Ok 类型实现 `Debug`，
        // 而 `Arc<dyn AgentAdapter>` 没有（trait object 无法 derive Debug）。
        // 这也正是把 trait 设计成**不给适配器加 Debug 约束**的代价 ——
        // 那个约束会传染给每个实现者，而适配器里没有什么值得打印的东西。
        let err = match r.resolve(Some("codexx")) {
            Err(e) => e,
            Ok(_) => panic!("拼错的 id 必须报错，不能静默回落到 HTTP 直连"),
        };
        assert_eq!(err, "未知账号运行时：codexx");
        // 错误里要带**原样的 id** —— 用户靠它在配置里搜出那个拼错的地方
        assert!(err.contains("codexx"));
    }

    #[test]
    fn 已注册的_id_能解析出适配器() {
        let r = registry();
        let adapter = r
            .resolve(Some("fake"))
            .expect("已注册的 id 不该报错")
            .expect("应当解析出适配器");
        assert_eq!(adapter.id(), "fake");
    }

    #[test]
    fn ids_按字典序且稳定() {
        let mut r = registry();
        r.register(Arc::new(FakeAdapter::with_id("zzz")));
        r.register(Arc::new(FakeAdapter::with_id("aaa")));
        let ids = r.ids();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "`ids()` 必须给出稳定顺序，否则设置页会跳");
        assert!(ids.contains(&"fake"));
        assert!(ids.contains(&"aaa"));
        assert!(ids.contains(&"zzz"));
    }

    #[test]
    fn 重复注册同一_id_是覆盖而不是堆积() {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(FakeAdapter::with_id("dup")));
        r.register(Arc::new(FakeAdapter::with_id("dup")));
        assert_eq!(r.ids(), vec!["dup"], "注册表不该因为重复注册而变长");
    }
}
