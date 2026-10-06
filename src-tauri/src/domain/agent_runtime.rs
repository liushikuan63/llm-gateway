//! 任务卡二 A5：账号型上游的运行时定义。
//!
//! ## 为什么是数据而不是枚举
//!
//! 「有哪些账号型上游」看起来像编译期的事（适配器就是代码），
//! 但**用户的**运行时是数据：同一家 Codex 可能被配两次
//! （两个不同的登录 / 两个不同的模型别名表）。
//! 所以库里存运行时，代码里存适配器，两者用 `kind` 对上。
//!
//! ## `kind` 与 `id` 的分工
//!
//! - `id`：用户起的名字，`provider.runtime_id` 引用它。**唯一**
//! - `kind`：指向哪个适配器（`codex` / `fake`…）。**可重复**
//!
//! 分开是为了让「同一家配两次」和「换一家实现」两件事都能做 ——
//! 合起来的话，改适配器实现就必须改用户配置。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 一个账号型上游的运行时。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRuntime {
    /// 用户可读的标识，被 `provider.runtime_id` 引用。
    pub id: String,
    /// 适配器标识，对应 `AdapterRegistry` 里的 key。
    pub kind: String,
    /// 界面上显示的名字。
    pub label: String,
    /// 附加配置（可执行文件路径、模型别名表…）。JSON 对象，可为空。
    ///
    /// 存 JSON 而不是拆成列：各家的附加项差别很大（Codex 要 exec-server
    /// 的参数、Qoder 要 SDK 的路径），拆成定宽列会让「加一家」变成一次
    /// 表结构迁移。与 `models.local_json` 同构。
    #[serde(default)]
    pub options: Option<serde_json::Value>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentRuntime {
    /// 新建一个运行时。时间戳由这里盖 —— 与 `resolved_from_ledger` 同款理由：
    /// 让每个调用方各盖一次，漏盖的表现是「界面永远显示成很久以前更新的」。
    pub fn new(id: impl Into<String>, kind: impl Into<String>, label: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: id.into(),
            kind: kind.into(),
            label: label.into(),
            options: None,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    /// 校验。**在写库前调用**，把能拦的错拦在写之前。
    ///
    /// `options` 必须是**对象**（不是数组/字符串）：它是「一组具名参数」，
    /// 存成别的形状会让每个读它的适配器都要处理一堆类型分支，
    /// 而那些分支的失败方式是「读不到 → 用默认值 → 静默行为不符预期」。
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("运行时 id 不能为空".into());
        }
        if self.kind.trim().is_empty() {
            return Err("运行时 kind 不能为空（它指向一个适配器）".into());
        }
        if let Some(options) = &self.options {
            if !options.is_object() {
                return Err("运行时 options 必须是 JSON 对象".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 新建时默认启用且盖上时间戳() {
        let r = AgentRuntime::new("codex-work", "codex", "Codex（工作）");
        assert!(r.enabled, "新建的运行时默认启用");
        assert_eq!(r.id, "codex-work");
        assert_eq!(r.kind, "codex");
        assert_eq!(r.label, "Codex（工作）");
        assert_eq!(r.options, None);
        // 时间戳必须真的盖上了 —— 漏盖的表现是界面永远显示很久以前
        assert_eq!(r.created_at, r.updated_at);
    }

    #[test]
    fn id_与_kind_都非空才合法() {
        assert!(AgentRuntime::new("a", "codex", "l").validate().is_ok());
        let err = AgentRuntime::new("", "codex", "l").validate().unwrap_err();
        assert!(err.contains("id"), "错误要指向具体字段：{err}");
        let err = AgentRuntime::new("a", "   ", "l").validate().unwrap_err();
        assert!(err.contains("kind"), "错误要指向具体字段：{err}");
        // 纯空白 id 也算空 —— 手改配置文件很容易写出来
        assert!(AgentRuntime::new("  ", "codex", "l").validate().is_err());
    }

    #[test]
    fn options_必须是对象() {
        let mut r = AgentRuntime::new("a", "codex", "l");
        r.options = Some(serde_json::json!({"exe": "codex.exe"}));
        assert!(r.validate().is_ok());

        for bad in [
            serde_json::json!(["a", "b"]),
            serde_json::json!("codex.exe"),
            serde_json::json!(42),
            serde_json::json!(null),
        ] {
            r.options = Some(bad.clone());
            let err = r.validate().unwrap_err();
            assert!(err.contains("对象"), "{bad} 应当被拒：{err}");
        }
    }

    #[test]
    fn 没有_options_也是合法的() {
        // 大多数运行时不需要附加配置（比如假适配器）。
        let r = AgentRuntime::new("a", "fake", "假");
        assert_eq!(r.options, None);
        assert!(r.validate().is_ok());
    }

    #[test]
    fn 同一_kind_可以配多个_id() {
        // 这正是 id 与 kind 分开的理由：同一家 Codex 可能被配两次
        // （两个不同的登录 / 两个不同的模型别名表）。
        let a = AgentRuntime::new("codex-work", "codex", "工作");
        let b = AgentRuntime::new("codex-home", "codex", "家里");
        assert_ne!(a.id, b.id, "id 必须不同 —— 它是被引用的那个");
        assert_eq!(a.kind, b.kind, "kind 可以相同 —— 它指向同一个适配器");
    }

    #[test]
    fn 序列化往返不丢信息() {
        let mut r = AgentRuntime::new("a", "codex", "l");
        r.options = Some(serde_json::json!({"exe": "codex.exe", "args": ["--json"]}));
        let json = serde_json::to_string(&r).unwrap();
        let back: AgentRuntime = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }
}
