//! 任务卡二 A8：Agent 型入口的执行体。
//!
//! ## 与 LLM 透传的分工（裁决之一）
//!
//! 「LLM 透传与 Agent 型入口两个都做且**分开**」。区别在这里体现：
//!
//! | | LLM 透传（A5 分派点） | Agent 型入口（本模块） |
//! | --- | --- | --- |
//! | 干什么 | 发一轮对话，拿一段文本 | 让 agent 在**隔离目录里干活** |
//! | 产物 | 无 | 有，且要列清单 |
//! | 落盘 | 不落 | 落在 `runtimes/<id>/workspace`，**拒绝写出** |
//! | 工具 | 默认无 | 由 agent 自己决定（但仍受落盘边界约束） |
//!
//! 两者共用 [`AgentAdapter`](super::AgentAdapter) 与
//! [`WorkspaceRoot`] —— 复用执行层，但不复用语义。

use std::path::PathBuf;

use super::adapter::{AgentAdapter, AgentRequest};
use super::workspace::WorkspaceRoot;

/// 一次 Agent 型执行的产出。
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRunOutcome {
    /// agent 说的话。
    pub text: String,
    /// 实际走的传输（`L1` / `L3`）。
    pub transport: String,
    /// **产物清单**：这次执行在产物根里新增或改动的文件（相对产物根的路径）。
    ///
    /// 卡片 A8 要求「审计新增 `route_intent=agent` 与**产物清单**字段」。
    ///
    /// 用**前后快照求差**而不是让适配器自己报：适配器可能走 L1/L3 两条路、
    /// 也可能什么都不报；而「目录里到底多了什么」是**可观测的事实**，
    /// 不依赖适配器的诚实度。
    pub artifacts: Vec<String>,
}

/// 在隔离目录里跑一次 Agent。
///
/// ## 为什么产物根是**必填参数**
///
/// 卡片要求「默认写到 `runtimes\<id>\workspace`，**拒绝写出该目录**」。
/// 把根做成参数（而不是在这里自己拼路径）是为了让
/// 「产物写到哪」**只有一个决定点** —— 调用方给什么根，就只能在什么根里写。
/// 在这里再算一遍默认路径的话，两处算法迟早不一致，
/// 而那种不一致的表现是「产物被写到了没被检查的目录里」。
///
/// ## 超时与杀树
///
/// 由 `run_json_cli` 负责（它带超时 + 杀进程树 + cwd 隔离）。
/// 本函数不重复实现 —— 两份实现里必然有一份先腐坏。
pub async fn run_agent(
    adapter: &dyn AgentAdapter,
    model: &str,
    prompt: &str,
    timeout_ms: u64,
    workspace: &WorkspaceRoot,
) -> Result<AgentRunOutcome, String> {
    // ① 执行前快照
    let before = snapshot(workspace);

    // ② 跑
    let reply = adapter
        .send(AgentRequest {
            model: model.to_string(),
            prompt: prompt.to_string(),
            timeout_ms,
        })
        .await?;

    // ③ 执行后快照，求差
    let after = snapshot(workspace);
    let mut artifacts: Vec<String> = after
        .into_iter()
        .filter(|(path, stamp)| before.get(path) != Some(stamp))
        .map(|(path, _)| path)
        .collect();
    // 排序：产物清单会进审计，顺序不稳定的话每次 diff 都在跳
    artifacts.sort();

    // ④ 复核每一件产物都落在根内。
    vet_artifacts(workspace, &artifacts)?;

    Ok(AgentRunOutcome {
        text: reply.text,
        transport: reply.transport,
        artifacts,
    })
}

/// 复核产物清单里的每一项都落在产物根内。
///
/// ## 【诚实标注】当前实现下这一段**不可达**
///
/// `snapshot` 是按根遍历出来的，所以它给的路径**按构造**一定在根内。
/// 我做过注违规自检：把调用去掉，5 条用例**全绿** —— 证明它没有覆盖。
///
/// 那为什么还留着一个不可达的检查？两个理由：
///
/// 1. **它是给将来的 `snapshot` 实现兜底的。** 如果哪天换成
///    跟随软链接的遍历（或改成「适配器自报产物」），越界就真的可能发生，
///    而这个检查会立刻生效 —— 那时它有用例，见下。
/// 2. **它可以直接测。** 抽成独立函数就是为了这个：
///    `产物越界的清单会被拒绝` 用**手工构造的越界项**打它，
///    所以「这段逻辑对不对」是**有证据的**，只是「在当前调用路径上
///    会不会触发」暂时为否。
///
/// ## 仍然存在的真实缺口（记在这里，别以为它被覆盖了）
///
/// **根内的软链接指向根外**：文件的**路径**在根内，而它的**内容**
/// 落在根外。`contains` 判的是路径，所以看不出来。
/// 要堵它需要遍历时 `symlink_metadata` 并拒绝软链接 —— 那是独立一笔。
fn vet_artifacts(workspace: &WorkspaceRoot, artifacts: &[String]) -> Result<(), String> {
    for artifact in artifacts {
        if !workspace.contains(artifact) {
            return Err(format!(
                "产物越界：{artifact} 不在产物根内。已拒绝返回结果 —— \
                 越界写入必须让人看见，不能混在一份看起来正常的清单里"
            ));
        }
    }
    Ok(())
}

/// 一次 Agent 型请求的完整解析链：**配置门禁 → 产物根 → 适配器 → 执行**。
///
/// ## 为什么把这条链单独抽出来
///
/// HTTP 路由只该做「取参数 / 转 JSON」；上面这四步**每一步都可能被拒绝**
/// （开关关着、产物根建不出来、适配器不存在、执行失败），
/// 而它们的错误文本是给用户看的。混在处理器里就只能靠端到端碰运气测。
///
/// ## 【铁律 2：模式隔离】开关关着时**第一步就返回**
///
/// `agent.enabled` 默认 `false`。关着时这个函数**不解析产物根、
/// 不碰文件系统、不调适配器** —— 直接返回错误。
/// 这样「没开这个能力」与「开了但失败」在**副作用上**就区分得开：
/// 前者一定没有创建任何目录。用例守着这一条。
pub async fn run_agent_request(
    config: &crate::config::AgentConfig,
    registry: &super::AdapterRegistry,
    runtime_id: &str,
    model: &str,
    prompt: &str,
) -> Result<AgentRunOutcome, String> {
    // ① 开关。**必须排在最前** —— 关着时不许有任何副作用。
    if !config.enabled {
        return Err("Agent 型入口未启用。它是让外部 CLI 在本地读写文件的能力，\
             默认关闭；确认需要后请在配置里打开 agent.enabled"
            .to_string());
    }

    // ② 适配器。解析不出就报可读错误（与 A5 判据 3 同一条口径）。
    let adapter = registry
        .resolve(Some(runtime_id))?
        .ok_or_else(|| format!("未知账号运行时：{runtime_id}"))?;

    // ③ 产物根。`runtime_id` 的消毒在 `default_workspace_root` 里。
    let base = config.resolve_base()?;
    let workspace = super::workspace::default_workspace_root(&base, runtime_id)?;

    // ④ 执行。超时来自配置（默认 300s，依据是实测 codex 会静默挂住）。
    let timeout_ms = config.exec_timeout_secs.saturating_mul(1000).max(1);
    run_agent(adapter.as_ref(), model, prompt, timeout_ms, &workspace).await
}

/// Database-aware production entry; the disabled global gate runs before DB or disk I/O.
pub async fn run_agent_request_with_runtime(
    pool: &sqlx::SqlitePool,
    config: &crate::config::AgentConfig,
    registry: &super::AdapterRegistry,
    runtime_id: &str,
    model: &str,
    prompt: &str,
) -> Result<AgentRunOutcome, String> {
    if !config.enabled {
        return Err("Agent 型入口未启用；请在配置里打开 agent.enabled".into());
    }
    let runtime = super::resolve_runtime(pool, registry, runtime_id).await?;
    let workspace = super::workspace::default_workspace_root(&config.resolve_base()?, &runtime.id)?;
    let timeout_ms = config.exec_timeout_secs.saturating_mul(1000).max(1);
    run_agent(
        runtime.adapter.as_ref(),
        model,
        prompt,
        timeout_ms,
        &workspace,
    )
    .await
}

/// 列出产物根下的所有文件：**相对路径 → 修改时间戳**。
///
/// 用「修改时间」而不是「文件大小」判断改动：agent 完全可能
/// 把一个大文件改小，而大小相同、内容不同的情况更是常见。
/// 时间戳也有它的局限（同秒内的改动分辨不出），所以对
/// 「新增文件」这个主要场景它是够的 —— 而对「改了已有文件」，
/// 那点局限的后果是**少报一次改动**，不是误报。
fn snapshot(workspace: &WorkspaceRoot) -> std::collections::BTreeMap<String, u128> {
    let mut out = std::collections::BTreeMap::new();
    let root = workspace.as_path();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let stamp = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            // 用 `/` 统一分隔符：这个字符串要进审计与前端，
            // 而 Windows 的 `\` 在 JSON/URL 里都要转义。
            let key = relative.to_string_lossy().replace('\\', "/");
            out.insert(key, stamp);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_upstream::FakeAdapter;

    fn workspace(name: &str) -> WorkspaceRoot {
        let p = std::env::temp_dir().join(format!("llmgw-run-agent-{name}"));
        // 先清空，避免上一轮的残留文件被算成「已有」
        let _ = std::fs::remove_dir_all(&p);
        WorkspaceRoot::new(&p).expect("建产物根")
    }

    #[tokio::test]
    async fn 空跑时产物清单为空() {
        // 假适配器不写任何文件 —— 清单必须是空的，
        // 而不是「把根里已有的文件都算上」。
        let ws = workspace("empty");
        std::fs::write(ws.as_path().join("早就存在的.txt"), b"x").unwrap();

        let adapter = FakeAdapter::default();
        let out = run_agent(&adapter, "m", "你好", 5_000, &ws)
            .await
            .expect("假适配器不该失败");
        assert!(out.text.contains("你好"));
        assert_eq!(out.transport, "fake");
        assert!(
            out.artifacts.is_empty(),
            "**已有文件不算产物** —— 否则每次执行都会报出一堆无关文件：{:?}",
            out.artifacts
        );
    }

    #[tokio::test]
    async fn 新增的文件出现在产物清单里() {
        let ws = workspace("newfile");
        // 用一个会写文件的适配器
        struct Writer(WorkspaceRoot);
        #[async_trait::async_trait]
        impl AgentAdapter for Writer {
            fn id(&self) -> &'static str {
                "writer"
            }
            fn label(&self) -> &'static str {
                "写文件的测试适配器"
            }
            async fn send(&self, _r: AgentRequest) -> Result<super::super::AgentReply, String> {
                std::fs::write(self.0.as_path().join("产物.txt"), b"hello").unwrap();
                std::fs::create_dir_all(self.0.as_path().join("子目录")).unwrap();
                std::fs::write(self.0.as_path().join("子目录").join("深层.txt"), b"x").unwrap();
                Ok(super::super::AgentReply {
                    text: "写好了".into(),
                    transport: "L3".into(),
                })
            }
        }

        let out = run_agent(&Writer(ws.clone()), "m", "写点东西", 5_000, &ws)
            .await
            .expect("应当成功");
        assert_eq!(
            out.artifacts,
            vec!["产物.txt".to_string(), "子目录/深层.txt".to_string()],
            "两件产物都要列出来，且用 `/` 分隔、按字典序"
        );
        assert_eq!(out.transport, "L3", "适配器回报什么就是什么");
    }

    #[tokio::test]
    async fn 改了已有文件也算产物() {
        // 「新增」只是产物的一种。改了已有文件同样是产物 ——
        // 只比对文件名集合的实现会漏掉它。
        let ws = workspace("modified");
        let target = ws.as_path().join("旧文件.txt");
        std::fs::write(&target, b"old").unwrap();
        // 确保 mtime 会变（有些文件系统的分辨率是秒级）
        std::thread::sleep(std::time::Duration::from_millis(20));

        struct Modifier(WorkspaceRoot);
        #[async_trait::async_trait]
        impl AgentAdapter for Modifier {
            fn id(&self) -> &'static str {
                "modifier"
            }
            fn label(&self) -> &'static str {
                "改文件的测试适配器"
            }
            async fn send(&self, _r: AgentRequest) -> Result<super::super::AgentReply, String> {
                std::fs::write(self.0.as_path().join("旧文件.txt"), b"new content").unwrap();
                Ok(super::super::AgentReply {
                    text: "改好了".into(),
                    transport: "L3".into(),
                })
            }
        }

        let out = run_agent(&Modifier(ws.clone()), "m", "改一下", 5_000, &ws)
            .await
            .expect("应当成功");
        assert_eq!(
            out.artifacts,
            vec!["旧文件.txt".to_string()],
            "改了已有文件也必须是产物"
        );
    }

    #[tokio::test]
    async fn 适配器失败时原样冒泡错误() {
        struct Failing;
        #[async_trait::async_trait]
        impl AgentAdapter for Failing {
            fn id(&self) -> &'static str {
                "failing"
            }
            fn label(&self) -> &'static str {
                "总是失败的测试适配器"
            }
            async fn send(&self, _r: AgentRequest) -> Result<super::super::AgentReply, String> {
                Err("未登录：请先运行 login".into())
            }
        }

        let ws = workspace("failing");
        let err = run_agent(&Failing, "m", "x", 5_000, &ws)
            .await
            .expect_err("适配器报错必须冒泡");
        assert!(err.contains("未登录"), "错误要原样保留：{err}");
    }

    /// **直接打 `vet_artifacts`** —— 因为它在 `run_agent` 的调用路径上
    /// 不可达（快照按构造不会给出越界项）。手工构造越界项来验它对不对。
    #[test]
    fn 产物越界的清单会被拒绝() {
        let ws = workspace("vet");
        // 在根内、以及根外各来一项
        assert!(vet_artifacts(&ws, &["根内的.txt".to_string()]).is_ok());
        assert!(vet_artifacts(&ws, &["子目录/也根内.txt".to_string()]).is_ok());
        assert!(vet_artifacts(&ws, &[]).is_ok(), "空清单合法");

        let err =
            vet_artifacts(&ws, &["../跑出去了.txt".to_string()]).expect_err("上跳穿越必须被拒");
        assert!(err.contains("产物越界"), "错误要说清是什么问题：{err}");
        assert!(err.contains("跑出去了.txt"), "要指出是哪一个：{err}");

        // 前缀相同的兄弟目录 —— `WorkspaceRoot` 那套逐组件比较在这里生效
        let sibling = format!("{}-evil/x.txt", ws.as_path().display());
        assert!(
            vet_artifacts(&ws, &[sibling]).is_err(),
            "前缀相同的兄弟目录必须被拒"
        );

        // 混合清单：只要有一项越界，整份就拒（不能只丢掉那一项）
        let mixed = vec!["好的.txt".to_string(), "../坏的.txt".to_string()];
        assert!(
            vet_artifacts(&ws, &mixed).is_err(),
            "混杂清单里有越界项时必须整体拒绝 —— 悄悄丢掉那一项会让\
             用户以为「没有越界」"
        );
    }

    // ---------------- 完整解析链（A8 路由的内核） ----------------

    fn enabled_config(
        name: &str,
        enabled: bool,
    ) -> (crate::config::AgentConfig, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("llmgw-req-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        (
            crate::config::AgentConfig {
                enabled,
                workspace_root: Some(base.clone()),
                exec_timeout_secs: 5,
                // 配额测试另有用例（`quota.rs`），这里不限。
                quota: crate::agent_upstream::AgentQuota::default(),
                max_concurrency: 0,
                // 这批用例不碰外部插件（各自目录都不给）。
                plugin_dirs: Vec::new(),
            },
            base,
        )
    }

    /// **铁律 2 的副作用隔离**：开关关着时，**不许创建任何目录**。
    ///
    /// 只断言「返回了错误」是不够的 —— 一个「先建目录再检查开关」的实现
    /// 也能通过那种断言，而它在**关着的时候**就已经在用户机器上落了盘。
    #[tokio::test]
    async fn 开关关着时连目录都不建() {
        let (cfg, base) = enabled_config("gateoff", false);
        let registry = super::super::AdapterRegistry::with_builtins();

        let err = run_agent_request(&cfg, &registry, "fake", "m", "你好")
            .await
            .expect_err("开关关着必须拒绝");
        assert!(err.contains("未启用"), "错误要说清原因：{err}");
        assert!(err.contains("agent.enabled"), "要告诉用户怎么开：{err}");

        // **副作用判据**：那个目录连父目录都不该存在
        assert!(
            !base.exists(),
            "开关关着时不许碰文件系统，但 {} 被创建了",
            base.display()
        );
    }

    /// 对照组：开关打开时**确实**会建出产物根。
    /// 没有这一条的话，上面那条可能只是因为「实现从来不建目录」而通过。
    #[tokio::test]
    async fn 开关打开时会建出产物根() {
        let (cfg, base) = enabled_config("gateon", true);
        let registry = super::super::AdapterRegistry::with_builtins();

        let out = run_agent_request(&cfg, &registry, "fake", "m", "你好")
            .await
            .expect("开了就该跑起来");
        assert!(out.text.contains("你好"));

        let expected = base.join("runtimes").join("fake").join("workspace");
        assert!(
            expected.is_dir(),
            "产物根应当被建出来：{}",
            expected.display()
        );
    }

    /// 未知运行时：给出可读错误，且**不创建任何目录**
    /// （适配器解析排在产物根之前，正是为了这个）。
    #[tokio::test]
    async fn 未知运行时不建目录也不调适配器() {
        let (cfg, base) = enabled_config("unknown", true);
        let registry = super::super::AdapterRegistry::with_builtins();

        let err = run_agent_request(&cfg, &registry, "codexx", "m", "你好")
            .await
            .expect_err("未知运行时必须报错");
        assert_eq!(err, "未知账号运行时：codexx");
        assert!(
            !base.exists(),
            "适配器解析失败时不该留下产物根：{}",
            base.display()
        );
    }

    /// `runtime_id` 的消毒在这一层也生效（越界 id 不许建目录）。
    #[tokio::test]
    async fn 穿越型_runtime_id_被拒且不建目录() {
        let (cfg, base) = enabled_config("traverse", true);
        let registry = super::super::AdapterRegistry::with_builtins();

        let err = run_agent_request(&cfg, &registry, "..", "m", "你好")
            .await
            .expect_err("穿越 id 必须被拒");
        assert!(!err.is_empty());
        assert!(!base.exists(), "被拒时不该留下任何目录");
    }

    #[test]
    fn 快照用正斜杠分隔并忽略目录本身() {
        let ws = workspace("snapshot");
        std::fs::create_dir_all(ws.as_path().join("a").join("b")).unwrap();
        std::fs::write(ws.as_path().join("a").join("b").join("c.txt"), b"x").unwrap();
        let snap = snapshot(&ws);
        assert!(
            snap.contains_key("a/b/c.txt"),
            "深层文件要用 `/` 分隔，实际：{:?}",
            snap.keys().collect::<Vec<_>>()
        );
        // 目录本身不进清单（它是容器，不是产物）
        assert!(!snap.contains_key("a"), "目录不该进产物清单");
        assert!(!snap.contains_key("a/b"), "目录不该进产物清单");
    }
}
