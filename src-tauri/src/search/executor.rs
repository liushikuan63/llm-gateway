//! 搜索执行与上下文注入。
//!
//! 顺序固定为：**预取 → 注入 → 才发上游**。这样流式与非流式的行为完全一致，
//! 客户端不需要知道网关背后做了什么（见 `crate::search` 的模块注释）。

use std::time::Duration;

use serde::Serialize;

use crate::config::{SearchBackendKind, SearchConfig};
use crate::domain::{Content, Message, Role};
use crate::search::backend::{
    BingCnBackend, BraveBackend, DuckDuckGoBackend, SearXngBackend, SearchBackend, SearchError,
    SearchQuery, SearchResult, TavilyBackend,
};

/// 一次搜索的结果与它的可观测字段。
#[derive(Debug, Clone, Serialize)]
pub struct SearchOutcome {
    /// 实际生效的后端；凭据失效时是 `failed`。
    pub backend: SearchBackendKind,
    pub hits: usize,
    /// 失败原因。成功时为 None。
    pub error: Option<String>,
    /// 空结果与「后端挂了」是两回事，界面要分开显示。
    pub results: Vec<SearchResult>,
}

impl SearchOutcome {
    /// 构造失败结果。`attempted` 是**用户配置的那个后端**，不是随便一个默认值——
    /// 界面要显示「你选的 Tavily 失败了」，而不是显示 DuckDuckGo 失败。
    pub fn failed(attempted: SearchBackendKind, error: impl Into<String>) -> Self {
        Self {
            backend: attempted,
            hits: 0,
            error: Some(error.into()),
            results: Vec::new(),
        }
    }
}

/// 共享的 HTTP 客户端。
///
/// 搜索是每请求都可能走的热路径，每次新建 client 会重跑 TLS 配置与连接池初始化。
/// 全进程共用一个；超时由每次调用自己带，不会互相影响。
pub fn shared_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// 按配置挑一个后端实例。
fn backend_of(cfg: &SearchConfig) -> Option<Box<dyn SearchBackend>> {
    match cfg.backend {
        SearchBackendKind::Tavily => Some(Box::new(TavilyBackend::default())),
        SearchBackendKind::Brave => Some(Box::new(BraveBackend::default())),
        SearchBackendKind::SearXng => {
            let base = cfg.searxng_url.clone()?.trim().to_owned();
            if base.is_empty() {
                return None;
            }
            Some(Box::new(SearXngBackend { base_url: base }))
        }
        SearchBackendKind::BingCn => Some(Box::new(BingCnBackend::default())),
        SearchBackendKind::DuckDuckGo => Some(Box::new(DuckDuckGoBackend::default())),
    }
}

/// 执行一次搜索。**永不返回 `Err`**——调用方要能区分「没有搜索」和「搜索失败」，
/// 但两者都不该阻断请求。
pub async fn execute(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    query: &SearchQuery,
) -> SearchOutcome {
    let Some(backend) = backend_of(cfg) else {
        return SearchOutcome::failed(cfg.backend, "当前后端缺少必要配置");
    };
    let timeout = Duration::from_millis(cfg.timeout_ms.clamp(500, 60_000));
    let future = backend.search(http, query, key);
    match tokio::time::timeout(timeout, future).await {
        Err(_) => SearchOutcome::failed(
            cfg.backend,
            format!("搜索超时（{}ms）", cfg.timeout_ms),
        ),
        Ok(Err(SearchError::CredentialRejected)) => SearchOutcome::failed(
            cfg.backend,
            "搜索后端凭据被拒（401/403）。已停止本次搜索，**不会**自动回落到免 Key 后端——请先修正 API Key。",
        ),
        Ok(Err(error)) => SearchOutcome::failed(cfg.backend, error.to_string()),
        Ok(Ok(results)) => SearchOutcome {
            backend: backend.kind(),
            hits: results.len(),
            error: None,
            results,
        },
    }
}

/// 执行搜索并返回「是否应当把注入消息塞进对话」。
///
/// 注入位置在**最后一条 user 消息之前**，而不是 system prompt 开头：
/// 检索结果往往很长，放开头会把真正的指令挤出注意力，模型先读新闻再读需求，
/// 效果反而更差。
pub async fn prefetch(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    messages: &mut Vec<Message>,
    user_text: &str,
) -> SearchOutcome {
    let query = crate::search::query_from(user_text, 400);
    // 按**字符**判长度：`query.len()` 是字节数，一个中文字就有 3 字节，
    // 用它判断会让「中」也触发一次网络往返。
    if query.chars().count() < 2 {
        // 没有可用的检索词就别浪费一次网络往返。
        return SearchOutcome {
            backend: cfg.backend,
            hits: 0,
            error: None,
            results: Vec::new(),
        };
    }
    let outcome = execute(
        http,
        cfg,
        key,
        &SearchQuery {
            text: query.clone(),
            max_results: cfg.normalized_max_results(),
        },
    )
    .await;

    if outcome.results.is_empty() {
        return outcome;
    }

    let rendered = crate::search::parse::render(
        &query,
        &crate::search::backend_code(outcome.backend),
        &outcome.results,
    );
    let injected = match cfg.inject_as {
        crate::config::SearchInjectFormat::System => Message::system(rendered),
        crate::config::SearchInjectFormat::User => Message::user(rendered),
    };

    // 插到最后一条 user 之前；没有 user 就追加到末尾。
    let position = messages
        .iter()
        .rposition(|m| m.role == Role::User)
        .unwrap_or(messages.len().saturating_sub(1));
    messages.insert(position, injected);
    outcome
}

/// 给界面「测试后端」用：只跑一次，不注入。
pub async fn test_backend(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    text: &str,
) -> SearchOutcome {
    let query = crate::search::query_from(text, 400);
    execute(
        http,
        cfg,
        key,
        &SearchQuery {
            text: query,
            max_results: cfg.normalized_max_results(),
        },
    )
    .await
}

/// 把注入文本包成一条消息。仅测试用。
pub fn as_message(cfg: &SearchConfig, text: String) -> Message {
    let message = match cfg.inject_as {
        crate::config::SearchInjectFormat::System => Message::system(text),
        crate::config::SearchInjectFormat::User => Message::user(text),
    };
    debug_assert!(matches!(message.content, Content::Text(_)));
    message
}
