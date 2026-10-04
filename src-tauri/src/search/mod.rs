//! 网关内置联网搜索。
//!
//! 四种后端（用户口径：多后端可配置 + 免 Key 兜底）：
//!
//! | 后端 | 凭据 | 说明 |
//! | --- | --- | --- |
//! | `tavily` | API Key | 为 LLM 设计的检索，直接给摘要 |
//! | `brave` | Subscription Token | 独立索引 |
//! | `searxng` | 无（自建） | 隐私优先 |
//! | `duckduckgo` | **无** | 最后兜底，可用性无保证 |
//!
//! ## 注入时机是「预取」而不是「流中工具回合」
//!
//! 既有约束：首个流式字节输出后不得换上游。要在流中做工具回合就必须缓冲整段再重放，
//! 首包延迟从百毫秒级涨到「两轮上游耗时之和」。因此分类判定需要联网时，网关在
//! **第一次上游调用之前**完成搜索并把结果注入上下文；客户端（Claude Code / Codex /
//! Cursor）不需要知道网关背后做了什么，也不用改任何工具定义。
//!
//! ## 失败语义
//!
//! - 401/403 ⇒ 凭据失效，**不**回落到免 Key 后端（避免用错的凭据反复打）；
//! - 其余错误 ⇒ 可换后端；
//! - 全部不可用 ⇒ 返回 `Err`，但调用方**不阻断请求**，只回 `X-Route-Search: failed`。

pub mod backend;
pub mod executor;
pub mod parse;

pub use backend::{
    BraveBackend, DuckDuckGoBackend, SearchBackend, SearchError, SearchQuery, SearchResult,
    SearXngBackend, TavilyBackend,
};
pub use executor::{as_message, execute, prefetch, test_backend, SearchOutcome};
pub use parse::{decode, duckduckgo_lite, render};

use serde::{Deserialize, Serialize};

use crate::config::{SearchBackendKind, SearchConfig};

/// 解析一个查询串。长度受限是为了避免把整个会话塞进搜索框——
/// 搜索后端对超长 query 只会返回更差的结果。
pub fn query_from(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let cleaned: String = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let chars: Vec<char> = cleaned.chars().collect();
    if chars.len() <= max_chars {
        return cleaned;
    }
    // 保尾：用户的问题通常在长提示词的末尾。
    chars[chars.len() - max_chars..].iter().collect()
}

/// 配置值（serde 真值）的**程序化取值**。
///
/// 这是「配置侧字符串」的唯一事实源：从 serde 序列化结果反推，
/// 而不是再手写一遍 `snake_case`。手写第二遍就是两套值漂移的开始。
///
/// 用途：测试用它与 `backend_code()` 对照，把「展示值和配置值被误合并」
/// 这个风险变成可失败断言（见 `tests/search.rs` 的 `后端码_响应头用的必须是展示拼法`）。
/// 配置本身不需要它 —— serde 会自己处理。
pub fn backend_serde_value(kind: &SearchBackendKind) -> String {
    // SearchBackendKind 的序列化就是那个 snake_case 字符串（枚举无自定义 Serialize）。
    serde_json::to_value(kind.clone())
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// 搜索后端的稳定取值。响应头、审计与注入文本共用同一份，
/// 避免出现 `DuckDuckGo` / `duck_duck_go` / `failed` 三种写法混用。
///
/// **注意它不是配置值。** 配置值由 serde 决定（`sear_xng` / `duck_duck_go`），
/// 这里给的是给人看的展示值（`searxng` / `duckduckgo`）。两套字符串用途不同，
/// 写成对方的值就是 bug：配置值写错 → TOML 解析失败 → 整个应用打不开。
/// 完整对照见设计方案 §6.1。
pub fn backend_code(kind: SearchBackendKind) -> String {
    match kind {
        SearchBackendKind::Tavily => "tavily".to_owned(),
        SearchBackendKind::Brave => "brave".to_owned(),
        SearchBackendKind::SearXng => "searxng".to_owned(),
        SearchBackendKind::BingCn => "bing_cn".to_owned(),
        SearchBackendKind::DuckDuckGo => "duckduckgo".to_owned(),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchSettingsInput {
    pub enabled: bool,
    pub backend: SearchBackendKind,
    pub searxng_url: Option<String>,
    pub max_results: u32,
    pub timeout_ms: u64,
    pub inject_as: crate::config::SearchInjectFormat,
    /// 明文 Key；空串表示「不改」，`Some(None)` 语义由 `clear_api_key` 表达。
    #[serde(default)]
    pub api_key: Option<String>,
    /// 显式要求删除已存 Key
    #[serde(default)]
    pub clear_api_key: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchSettingsView {
    pub enabled: bool,
    pub backend: SearchBackendKind,
    pub searxng_url: Option<String>,
    pub max_results: u32,
    pub timeout_ms: u64,
    pub inject_as: crate::config::SearchInjectFormat,
    /// 已存 Key 的掩码，形如 `tvly-••••3f2a`；没有存过则为 None。
    pub api_key_masked: Option<String>,
    /// 当前后端是否需要凭据。免 Key 后端为 false。
    pub backend_needs_key: bool,
}

/// 与 `ProviderView::api_key_masked` 同款处理：只回掩码，绝不回明文。
pub fn mask_key(key: &str) -> Option<String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        return Some("••••".to_owned());
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    Some(format!("{head}••••{tail}"))
}

/// 校验搜索设置。地址类字段只放行 http/https。
pub fn validate(cfg: &SearchConfig) -> anyhow::Result<()> {
    if cfg.max_results == 0 || cfg.max_results > 10 {
        anyhow::bail!("搜索结果条数必须在 1 到 10 之间");
    }
    if let Some(url) = cfg.searxng_url.as_deref() {
        if !url.trim().is_empty() {
            crate::config::validate_http_url(url)?;
        }
    }
    if matches!(cfg.backend, SearchBackendKind::SearXng)
        && cfg
            .searxng_url
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        anyhow::bail!("选择 SearXNG 后端时必须填写实例地址");
    }
    Ok(())
}