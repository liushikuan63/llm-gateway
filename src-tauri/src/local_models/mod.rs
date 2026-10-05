//! 本地模型：把本机推理运行时当成一等公民。
//!
//! 两种运行时形态：
//!
//! - **Ollama**（`/api/tags`、`/api/pull`、`/api/delete`）：有原生管理面，
//!   而且 `/api/tags` 的 `capabilities` 直接给出 vision / tools / thinking / audio，
//!   这是把本地模型接进智能模式的唯一可靠能力来源。
//! - **OpenAI 兼容**（`/v1/models`）：LM Studio、vLLM、llama.cpp server 都在这里。
//!   它们不暴露能力元数据，所以能力位一律保守置 `false`——宁可少标，不可多标。
//!
//! 登记后的本地模型与云端模型**完全平权**：同样进候选链、同样打分、同样降级、同样审计。
//! 唯一的区别是多一份 [`LocalMeta`] 来源信息。

pub mod catalog;
pub mod manage;
pub mod runtime;

pub use catalog::{
    find_by_upstream, ollama_models_from_tags, openai_models_from_list, to_model_ref,
    LocalModelInfo,
};
pub use manage::{delete_ollama_model, pull_ollama_model, PullProgress};
pub use runtime::{default_client, fetch_models, probe_all, probe_one, ProbeOutcome};

use serde::{Deserialize, Serialize};

use crate::config::{LocalEndpoint, LocalRuntimeKind};

/// 一次「登记为供应商」的请求。endpoint_id 指向 `AppConfig::local_models.endpoints`。
#[derive(Debug, Clone, Deserialize)]
pub struct RegisterLocalInput {
    pub endpoint_id: String,
    /// 上游模型名，例如 `gemma4:12b-it-q4_K_M`
    pub upstream: String,
    /// 可选；留空时等于 upstream
    pub alias: Option<String>,
    /// 供应商显示名；留空时用 `本地 · <endpoint label>`
    pub provider_name: Option<String>,
    /// 登记后是否立即启用参与路由
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterLocalOutcome {
    pub provider_id: String,
    pub alias: String,
    /// 本次调用让模型目录增加了几个条目（0 表示早已登记且被幂等合并）
    pub added_models: usize,
    /// 扫描到的全部本地模型，便于 UI 一次性刷新表格
    pub all_models: Vec<LocalModelInfo>,
}

/// 把一个端点解析成可直接拼 URL 的根地址。已在配置归一化阶段去过尾斜杠，
/// 这里再兜一次，避免手改 TOML 时拼出 `http://host//api/tags`。
pub fn root_of(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_owned()
}

/// 端点方言 → 网关侧 Provider 方言。
///
/// Ollama 用原生方言能拿到 `/api/pull` 之外的能力语义，也和既有的
/// `protocol/ollama.rs` 对齐；OpenAI 兼容面统一走 openai 方言。
pub fn dialect_of(kind: LocalRuntimeKind) -> crate::domain::Dialect {
    match kind {
        LocalRuntimeKind::Ollama => crate::domain::Dialect::Ollama,
        LocalRuntimeKind::OpenAiCompatible => crate::domain::Dialect::OpenAI,
    }
}

/// 在端点列表里按 id 找一项。
pub fn find_endpoint<'a>(endpoints: &'a [LocalEndpoint], id: &str) -> Option<&'a LocalEndpoint> {
    endpoints.iter().find(|e| e.id == id)
}

/// 两个 base_url 是否**指向同一台机器**（host + port 相同，路径不同也算）。
///
/// 用途只有一个：识别「同一个端点地址被改过」留下的僵尸供应商。
/// 只比 host:port 是刻意的 —— `…:11434` 与 `…:11434/v1` 主机相同但拼出的
/// 上游路径不同（`/api/chat` vs `/v1/api/chat`），后者必然 404。
///
/// 不含 url crate，手工拆 authority：`host`、`host:port` 两种写法都覆盖。
/// 解析不出来时返回 false（宁可不清理，也不误删）。
pub fn same_host(a: &str, b: &str) -> bool {
    fn authority_of(raw: &str) -> Option<String> {
        let rest = raw
            .trim()
            .split("://")
            .last()
            .unwrap_or("")
            .trim_start_matches('/');
        // 去掉路径与查询串
        let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
        if host_port.is_empty() {
            return None;
        }
        // IPv6 字面量 `[::1]:11434`
        if let Some(rest) = host_port.strip_prefix('[') {
            return rest.split_once(']').map(|(h, tail)| {
                let port = tail.strip_prefix(':').unwrap_or("");
                format!("[{}]:{port}", h.to_ascii_lowercase())
            });
        }
        let mut parts = host_port.splitn(2, ':');
        let host = parts.next().unwrap_or("").to_ascii_lowercase();
        let port = parts.next().map(str::to_owned).unwrap_or_default();
        if host.is_empty() {
            return None;
        }
        Some(format!("{host}:{port}"))
    }
    match (authority_of(a), authority_of(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}
