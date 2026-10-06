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
pub mod fake;

pub use adapter::{AgentAdapter, AgentReply, AgentRequest};
pub use fake::FakeAdapter;

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
