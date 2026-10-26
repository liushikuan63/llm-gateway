//! 搜索执行与上下文注入。
//! 选择、期限和部分成功约束见 [搜索维护](../../../docs/wiki/search.md)。
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
use crate::search::free_aggregate::FreeAggregateBackend;

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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub result_sources: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed_sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reuse: Option<crate::search::resources::SearchReuse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieved_at_utc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_text: Option<super::page_text::PageTextReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_locale: Option<crate::config::FreeSearchLocale>,
    /// Opaque reference; hits counts structured results only, never MDN documents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unstructured_reference: Option<super::mdn_reference::MdnReference>,
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
            result_sources: Vec::new(),
            failed_sources: Vec::new(),
            reuse: None,
            retrieved_at_utc: None,
            page_text: None,
            requested_locale: None,
            unstructured_reference: None,
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

enum SelectedBackend {
    Single(Box<dyn SearchBackend>),
    Aggregate(FreeAggregateBackend),
}

/// Free aggregation uses a dedicated direct client: no ambient proxy, credential
/// headers, redirects or automatic HTTP retry; existing five backends are unchanged.
pub(super) fn free_client() -> Result<&'static reqwest::Client, &'static str> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(3))
                .build()
                .map_err(|_| "免费聚合连接初始化失败".to_owned())
        })
        .as_ref()
        .map_err(|_| "免费聚合连接初始化失败")
}

/// The same resolver drives request prefetch and the LocalModels test action.
fn backend_of(cfg: &SearchConfig) -> Option<SelectedBackend> {
    let single: Box<dyn SearchBackend> = match cfg.backend {
        SearchBackendKind::FreeAggregate => {
            return Some(SelectedBackend::Aggregate(FreeAggregateBackend::default()))
        }
        SearchBackendKind::MdnMcp => return None,
        SearchBackendKind::Tavily => Box::new(TavilyBackend::default()),
        SearchBackendKind::Brave => Box::new(BraveBackend::default()),
        SearchBackendKind::SearXng => {
            let base = cfg.searxng_url.clone()?.trim().to_owned();
            if base.is_empty() {
                return None;
            }
            Box::new(SearXngBackend { base_url: base })
        }
        SearchBackendKind::BingCn => Box::new(BingCnBackend::default()),
        SearchBackendKind::DuckDuckGo => Box::new(DuckDuckGoBackend::default()),
    };
    Some(SelectedBackend::Single(single))
}

/// 执行一次搜索。**永不返回 `Err`**——调用方要能区分「没有搜索」和「搜索失败」，
/// 但两者都不该阻断请求。
pub async fn execute(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    query: &SearchQuery,
) -> SearchOutcome {
    if cfg.backend == SearchBackendKind::MdnMcp {
        return SearchOutcome::failed(cfg.backend, "mdn_storage_required");
    }
    if cfg.backend == SearchBackendKind::FreeAggregate
        && cfg.free_search_scope == crate::config::FreeSearchScope::StackOverflow
    {
        return SearchOutcome::failed(cfg.backend, "stack_overflow_protected_path_required");
    }
    let owned = if cfg.backend == SearchBackendKind::FreeAggregate {
        match super::egress::ClientPool::default().client(cfg.free_proxy_url.as_deref()) {
            Ok(client) => Some(client),
            Err(error) => return SearchOutcome::failed(cfg.backend, error),
        }
    } else {
        None
    };
    execute_selected(
        owned.as_ref().unwrap_or(http),
        cfg,
        key,
        query,
        backend_of(cfg),
    )
    .await
}

pub async fn execute_with_resources(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    query: &SearchQuery,
    resources: &crate::search::resources::SearchResources,
) -> SearchOutcome {
    execute_selected_with_resources(http, cfg, key, query, backend_of(cfg), resources).await
}

pub(super) fn resource_fingerprint(
    cfg: &SearchConfig,
    query: &SearchQuery,
    backend: &FreeAggregateBackend,
) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let data = serde_json::to_vec(&(
        cfg,
        &query.text,
        query.max_results,
        &backend.bing_base_url,
        &backend.ddg_base_url,
        &backend.npm_base_url,
        &backend.hn_base_url,
    ))
    .expect("nonsecret search settings serialize");
    Sha256::digest(data).into()
}

async fn execute_selected_with_resources(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    query: &SearchQuery,
    selected: Option<SelectedBackend>,
    resources: &crate::search::resources::SearchResources,
) -> SearchOutcome {
    if cfg.backend == SearchBackendKind::MdnMcp {
        return super::mdn_transport::execute(cfg, query, resources).await;
    }
    if let Some(SelectedBackend::Aggregate(backend)) = selected {
        if cfg.free_search_scope == crate::config::FreeSearchScope::StackOverflow {
            if !resources.captures_current(cfg) {
                return SearchOutcome::failed(cfg.backend, "stack_overflow_configuration_changed");
            }
            if resources.stack_exchange().is_none() {
                return SearchOutcome::failed(cfg.backend, "stack_overflow_storage_required");
            }
            let fingerprint = resource_fingerprint(cfg, query, &backend);
            let owned_cfg = cfg.clone();
            let query = query.clone();
            let current = resources.clone();
            return resources
                .run(fingerprint, cfg, move || async move {
                    execute_stack(&current, &owned_cfg, &query).await
                })
                .await;
        }
        let http = match resources.free_client(cfg) {
            Ok(client) => client,
            Err(e) => return SearchOutcome::failed(cfg.backend, e),
        };
        let fingerprint = resource_fingerprint(cfg, query, &backend);
        let owned_cfg = cfg.clone();
        let query = query.clone();
        let cooldowns = resources.cooldowns();
        return resources
            .run(fingerprint, cfg, move || async move {
                execute_selected_guarded(
                    &http,
                    &owned_cfg,
                    None,
                    &query,
                    Some(SelectedBackend::Aggregate(backend)),
                    cooldowns.as_ref(),
                )
                .await
            })
            .await;
    }
    execute_selected(http, cfg, key, query, selected).await
}

/// Shared protected consumer: both automatic prefetch and fresh manual probes.
/// It neither uses a model client nor changes query-reuse policy itself.
async fn execute_stack(
    resources: &crate::search::resources::SearchResources,
    cfg: &SearchConfig,
    query: &SearchQuery,
) -> SearchOutcome {
    if !resources.captures_current(cfg) {
        return SearchOutcome::failed(cfg.backend, "stack_overflow_configuration_changed");
    }
    let Some(store) = resources.stack_exchange() else {
        return SearchOutcome::failed(cfg.backend, "stack_overflow_storage_required");
    };
    let sender = match store
        .sender(cfg.free_proxy_url.as_deref(), cfg.source_min_interval_ms)
        .await
    {
        Ok(sender) => sender,
        Err(failure) => return stack_failure(cfg.backend, failure),
    };
    // Construction/retirement may have waited. Reject already-known stale
    // configuration before admission. A later change is not claimed to cancel
    // an accepted remote request or shorten its persistent restrictions.
    if !resources.captures_current(cfg) {
        return SearchOutcome::failed(cfg.backend, "stack_overflow_configuration_changed");
    }
    match sender
        .search(
            query,
            Duration::from_millis(cfg.timeout_ms.clamp(500, 60_000)),
        )
        .await
    {
        Ok(report) => {
            let unresolved = report.completion
                == super::stack_exchange_admission::Completion::ControlsUnresolved;
            SearchOutcome {
                backend: cfg.backend,
                hits: report.results.len(),
                error: unresolved.then(|| "stack_overflow_controls_unresolved".to_owned()),
                result_sources: vec!["stack_overflow".to_owned(); report.results.len()],
                failed_sources: if unresolved {
                    vec!["stack_overflow".to_owned()]
                } else {
                    vec![]
                },
                results: report.results,
                reuse: None,
                retrieved_at_utc: Some(
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ),
                page_text: None,
                requested_locale: None,
                unstructured_reference: None,
            }
        }
        Err(failure) => stack_failure(cfg.backend, failure),
    }
}

/// Fixed safe classes; no raw URL/query/proxy/upstream body enters the error.
fn stack_failure(
    backend: SearchBackendKind,
    failure: super::stack_exchange_sender::Failed,
) -> SearchOutcome {
    use super::{stack_exchange_admission::Block as B, stack_exchange_sender::FailureKind as F};
    let code = match failure.kind {
        F::InvalidRequest => "stack_overflow_invalid_request",
        F::Client => "stack_overflow_client_unavailable",
        F::Admission(B::Wait(_)) => "stack_overflow_wait",
        F::Admission(B::Exhausted) => "stack_overflow_quota_exhausted",
        F::Admission(B::Unresolved) => "stack_overflow_outcome_unknown",
        F::Admission(B::Storage) => "stack_overflow_storage_unavailable",
        F::Admission(B::InvalidState) => "stack_overflow_state_invalid",
        F::Admission(B::InvalidPolicy) => "stack_overflow_policy_invalid",
        F::Admission(B::Clock) => "stack_overflow_clock_invalid",
        F::Admission(B::Retired | B::ForeignAttempt) => "stack_overflow_configuration_changed",
        F::Admission(B::Deadline) | F::Timeout => "stack_overflow_timeout",
        F::Transport => "stack_overflow_transport_outcome_unknown",
        F::Http(_) => "stack_overflow_http_failure",
        F::BodyTooLarge => "stack_overflow_response_too_large",
        F::Encoding | F::Decode(_) => "stack_overflow_response_invalid",
    };
    let mut result = SearchOutcome::failed(backend, code);
    result.failed_sources = vec!["stack_overflow".to_owned()];
    result
}

async fn execute_selected(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    query: &SearchQuery,
    selected: Option<SelectedBackend>,
) -> SearchOutcome {
    execute_selected_guarded(http, cfg, key, query, selected, None).await
}
async fn execute_selected_guarded(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    query: &SearchQuery,
    selected: Option<SelectedBackend>,
    cooldowns: Option<&std::sync::Arc<super::cooldown::Cooldowns>>,
) -> SearchOutcome {
    if cfg.free_search_scope == crate::config::FreeSearchScope::StackOverflow
        && matches!(&selected, Some(SelectedBackend::Aggregate(_)))
    {
        return SearchOutcome::failed(cfg.backend, "stack_overflow_protected_path_required");
    }
    #[cfg(test)]
    assert!(
        !super::page_text::owned_transport_active(),
        "owned page tests must hit their explicitly seeded query cache, never external search"
    );
    if crate::budget::holds::active()
        && matches!(
            cfg.backend,
            SearchBackendKind::Tavily | SearchBackendKind::Brave | SearchBackendKind::SearXng
        )
    {
        return SearchOutcome::failed(cfg.backend, "budget_search_reference_contract_unverified");
    }
    let Some(selected) = selected else {
        return SearchOutcome::failed(cfg.backend, "当前后端缺少必要配置");
    };
    let backend = match selected {
        SelectedBackend::Aggregate(mut backend) => {
            // One authoritative aggregate deadline preserves a finished source;
            // do not put the legacy outer timeout in a race with that result.
            backend.timeout =
                Duration::from_millis(cfg.timeout_ms.clamp(500, 60_000).saturating_sub(25))
                    .min(Duration::from_secs(10));
            let mut outcome = match backend
                .search_with_preferences(
                    http,
                    query,
                    cooldowns,
                    cfg.source_min_interval_ms,
                    cfg.free_search_scope,
                    cfg.free_search_locale,
                )
                .await
            {
                Ok(report) => SearchOutcome {
                    backend: SearchBackendKind::FreeAggregate,
                    hits: report.results.len(),
                    error: None,
                    results: report.results,
                    result_sources: report
                        .result_sources
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    failed_sources: report
                        .failed_sources
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    reuse: Some(super::resources::SearchReuse::Fresh),
                    retrieved_at_utc: Some(
                        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    ),
                    page_text: None,
                    requested_locale: None,
                    unstructured_reference: None,
                },
                Err(error) => SearchOutcome::failed(cfg.backend, error.to_string()),
            };
            if cfg.free_search_scope == crate::config::FreeSearchScope::Web
                && cfg.free_search_locale != crate::config::FreeSearchLocale::SourceDefault
            {
                outcome.requested_locale = Some(cfg.free_search_locale);
            }
            return outcome;
        }
        SelectedBackend::Single(backend) => backend,
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
            result_sources: Vec::new(),
            failed_sources: Vec::new(),
            reuse: Some(super::resources::SearchReuse::Fresh),
            retrieved_at_utc: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs,true)),
            page_text: None,
            requested_locale: None,
            unstructured_reference: None,
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
    prefetch_inner(http, cfg, key, messages, user_text, None).await
}

pub async fn prefetch_with_resources(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    messages: &mut Vec<Message>,
    user_text: &str,
    resources: &crate::search::resources::SearchResources,
) -> SearchOutcome {
    prefetch_inner(http, cfg, key, messages, user_text, Some(resources)).await
}

async fn prefetch_inner(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    messages: &mut Vec<Message>,
    user_text: &str,
    resources: Option<&crate::search::resources::SearchResources>,
) -> SearchOutcome {
    let search_deadline = tokio::time::Instant::now()
        + Duration::from_millis(cfg.timeout_ms.clamp(500, 60_000)).min(Duration::from_secs(10));
    let query = crate::search::query_from(user_text, query_limit(cfg));
    // 按**字符**判长度：`query.len()` 是字节数，一个中文字就有 3 字节，
    // 用它判断会让「中」也触发一次网络往返。
    if query.chars().count() < 2 {
        // 没有可用的检索词就别浪费一次网络往返。
        return SearchOutcome {
            backend: cfg.backend,
            hits: 0,
            error: None,
            results: Vec::new(),
            result_sources: Vec::new(),
            failed_sources: Vec::new(),
            reuse: None,
            retrieved_at_utc: None,
            page_text: None,
            requested_locale: None,
            unstructured_reference: None,
        };
    }
    let input = SearchQuery {
        text: query.clone(),
        max_results: cfg.normalized_max_results(),
    };
    let mut outcome = if let Some(resources) = resources {
        execute_with_resources(http, cfg, key, &input, resources).await
    } else {
        execute(http, cfg, key, &input).await
    };

    if outcome.results.is_empty() {
        inject_unstructured_reference(&outcome, messages);
        return outcome;
    }

    let mut rendered = crate::search::parse::render(
        &query,
        &crate::search::backend_code(outcome.backend),
        &outcome.results,
    );
    if outcome.reuse == Some(crate::search::resources::SearchReuse::Cached) {
        rendered = format!(
            "【本机检索缓存；检索完成时间 {}，不是网页发布时间或实时可用性保证】\n{rendered}",
            outcome.retrieved_at_utc.as_deref().unwrap_or("未知")
        );
    }
    // Query reuse ends above. Fetch excerpts after it, never filling the query
    // cache or audit/session data with page text. The existing model-window
    // admission still runs after this prefetch in the gateway consumer.
    if cfg.page_text_enabled
        && !(cfg.backend == SearchBackendKind::FreeAggregate
            && cfg.free_search_scope == crate::config::FreeSearchScope::StackOverflow)
    {
        let read =
            super::page_text::enrich(cfg, &outcome.results, resources, search_deadline).await;
        super::page_text::append(&mut rendered, &read);
        outcome.page_text = Some(read.report);
    }
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
    inject_unstructured_reference(&outcome, messages);
    outcome
}

// Opaque reference data has its own low-authority message. A failed outcome
// cannot inject a stale reference; existing structured partial behavior is unchanged.
pub(crate) fn inject_unstructured_reference(outcome: &SearchOutcome, messages: &mut Vec<Message>) {
    if outcome.error.is_none() {
        if let Some(reference) = &outcome.unstructured_reference {
            reference.inject_into(messages);
        }
    }
}

/// 给界面「测试后端」用：只跑一次，不注入。
pub async fn test_backend(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    text: &str,
) -> SearchOutcome {
    let query = crate::search::query_from(text, query_limit(cfg));
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

/// A fresh diagnostic still respects committed source restrictions. It bypasses
/// the query cache/singleflight, not server-requested source protection.
pub async fn test_backend_with_resources(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    text: &str,
    resources: &crate::search::resources::SearchResources,
) -> SearchOutcome {
    test_backend_selected(http, cfg, key, text, resources, backend_of(cfg)).await
}
async fn test_backend_selected(
    http: &reqwest::Client,
    cfg: &SearchConfig,
    key: Option<&str>,
    text: &str,
    resources: &crate::search::resources::SearchResources,
    selected: Option<SelectedBackend>,
) -> SearchOutcome {
    let search_deadline = tokio::time::Instant::now()
        + Duration::from_millis(cfg.timeout_ms.clamp(500, 60_000)).min(Duration::from_secs(10));
    let query = SearchQuery {
        text: crate::search::query_from(text, query_limit(cfg)),
        max_results: cfg.normalized_max_results(),
    };
    if cfg.backend == SearchBackendKind::MdnMcp {
        return super::mdn_transport::execute(cfg, &query, resources).await;
    }
    if cfg.backend == SearchBackendKind::FreeAggregate
        && cfg.free_search_scope == crate::config::FreeSearchScope::StackOverflow
    {
        return execute_stack(resources, cfg, &query).await;
    }
    let owned = if cfg.backend == SearchBackendKind::FreeAggregate {
        match resources.free_client(cfg) {
            Ok(c) => Some(c),
            Err(e) => return SearchOutcome::failed(cfg.backend, e),
        }
    } else {
        None
    };
    let cooldowns = resources.cooldowns();
    let mut outcome = execute_selected_guarded(
        owned.as_ref().unwrap_or(http),
        cfg,
        key,
        &query,
        selected,
        cooldowns.as_ref(),
    )
    .await;
    if cfg.page_text_enabled && !outcome.results.is_empty() {
        let read =
            super::page_text::enrich(cfg, &outcome.results, Some(resources), search_deadline).await;
        outcome.page_text = Some(read.report);
    }
    outcome
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

fn query_limit(cfg: &SearchConfig) -> usize {
    if cfg.backend == SearchBackendKind::MdnMcp {
        return super::mdn_mcp_contract::MAX_QUERY_CHARS;
    }
    if cfg.backend == SearchBackendKind::FreeAggregate {
        crate::search::free_aggregate::MAX_QUERY_CHARS
    } else {
        400
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode, Uri},
        response::IntoResponse,
        routing::any,
        Router,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Fixture {
        mode: &'static str,
        calls: Arc<Mutex<Vec<String>>>,
    }

    struct HeldSources {
        origin: String,
        calls: Arc<Mutex<Vec<(String, String)>>>,
        entered: Arc<std::sync::atomic::AtomicUsize>,
        closed: Arc<std::sync::atomic::AtomicUsize>,
        release: Arc<tokio::sync::Notify>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for HeldSources {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    struct BodyClosed(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for BodyClosed {
        fn drop(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl HeldSources {
        async fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            let calls = Arc::new(Mutex::new(Vec::new()));
            let entered = Arc::new(AtomicUsize::new(0));
            let closed = Arc::new(AtomicUsize::new(0));
            let release = Arc::new(tokio::sync::Notify::new());
            let state = (
                calls.clone(),
                entered.clone(),
                closed.clone(),
                release.clone(),
            );
            let app=Router::new().fallback(any(move |uri:Uri,headers:HeaderMap,method:axum::http::Method|{let state=state.clone();async move {
                assert!(!headers.contains_key("authorization"));assert!(!headers.contains_key("x-subscription-token"));state.0.lock().unwrap().push((method.to_string(),uri.path().to_owned()));
                let bing=uri.path()=="/search";assert!(bing||uri.path()=="/lite/");assert_eq!(method,if bing{axum::http::Method::GET}else{axum::http::Method::POST});
                let stream=async_stream::stream!{
                    let _closed=BodyClosed(state.2);let wait=state.3.notified();tokio::pin!(wait);wait.as_mut().enable();state.1.fetch_add(1,Ordering::SeqCst);
                    yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from_static(if bing{b"<rss><channel>"}else{b"<html><body>"}));
                    wait.await;
                    yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from_static(if bing{b"<item><title>Held RSS</title><link>https://example.test/held-rss</link></item></channel></rss>"}else{b"<a class=\"result-link\" href=\"https://example.test/held-ddg\">Held DDG</a></body></html>"}));
                };
                axum::http::Response::builder().status(200).body(axum::body::Body::from_stream(stream)).unwrap()
            }}));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                origin,
                calls,
                entered,
                closed,
                release,
                task,
            }
        }
    }
    async fn wait_resource(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned HTTP resource barrier did not complete");
    }
    async fn held_query(
        resources: &crate::search::resources::SearchResources,
        cfg: &SearchConfig,
        origin: &str,
    ) -> SearchOutcome {
        let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(cfg) else {
            panic!("normal production factory must choose aggregate")
        };
        backend.bing_base_url = origin.into();
        backend.ddg_base_url = origin.into();
        execute_selected_with_resources(
            free_client().unwrap(),
            cfg,
            Some("must-not-leak"),
            &SearchQuery {
                text: "owned same pending query".into(),
                max_results: 5,
            },
            Some(SelectedBackend::Aggregate(backend)),
            resources,
        )
        .await
    }

    #[tokio::test]
    async fn shared_real_http_survives_one_cancel_and_last_cancel_closes_both_sources() {
        use std::sync::atomic::Ordering;
        let cfg = SearchConfig {
            backend: SearchBackendKind::FreeAggregate,
            timeout_ms: 3000,
            query_cache_ttl_secs: 10,
            coalesce_queries: true,
            ..Default::default()
        };
        let sources = HeldSources::new().await;
        let resources = Arc::new(crate::search::resources::SearchResources::new(cfg.clone()));
        let first = {
            let r = resources.clone();
            let c = cfg.clone();
            let origin = sources.origin.clone();
            tokio::spawn(async move { held_query(&r, &c, &origin).await })
        };
        wait_resource(|| sources.entered.load(Ordering::SeqCst) == 2).await;
        let second = {
            let r = resources.clone();
            let c = cfg.clone();
            let origin = sources.origin.clone();
            tokio::spawn(async move { held_query(&r, &c, &origin).await })
        };
        wait_resource(|| resources.stats().coalesced == 1).await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(sources.closed.load(Ordering::SeqCst), 0);
        sources.release.notify_waiters();
        let result = second.await.unwrap();
        assert!(result.error.is_none());
        assert!(result.failed_sources.is_empty());
        assert_eq!(
            result.reuse,
            Some(crate::search::resources::SearchReuse::Shared)
        );
        assert_eq!(resources.stats().entries, 1);
        assert_eq!(sources.calls.lock().unwrap().len(), 2);
        assert_eq!(resources.stats().queries_started, 1);
        assert_eq!(resources.stats().inflight, 0);
        let hit = held_query(&resources, &cfg, &sources.origin).await;
        assert_eq!(
            hit.reuse,
            Some(crate::search::resources::SearchReuse::Cached)
        );
        assert_eq!(sources.calls.lock().unwrap().len(), 2);

        let cancelled = HeldSources::new().await;
        let r = Arc::new(crate::search::resources::SearchResources::new(cfg.clone()));
        let a = {
            let r = r.clone();
            let c = cfg.clone();
            let origin = cancelled.origin.clone();
            tokio::spawn(async move { held_query(&r, &c, &origin).await })
        };
        wait_resource(|| cancelled.entered.load(Ordering::SeqCst) == 2).await;
        let b = {
            let r = r.clone();
            let c = cfg.clone();
            let origin = cancelled.origin.clone();
            tokio::spawn(async move { held_query(&r, &c, &origin).await })
        };
        wait_resource(|| r.stats().coalesced == 1).await;
        a.abort();
        b.abort();
        assert!(a.await.unwrap_err().is_cancelled());
        assert!(b.await.unwrap_err().is_cancelled());
        wait_resource(|| cancelled.closed.load(Ordering::SeqCst) == 2).await;
        assert_eq!(r.stats().inflight, 0);
        assert_eq!(r.stats().entries, 0);
        assert_eq!(cancelled.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn completed_search_metadata_has_observed_time_without_resource_reuse() {
        use futures_util::FutureExt;
        let (origin, calls, task) = server("success").await;
        let result = std::panic::AssertUnwindSafe(async {
            let cfg = SearchConfig { enabled: true, backend: SearchBackendKind::FreeAggregate, timeout_ms: 500, ..Default::default() };
            assert_eq!(cfg.query_cache_ttl_secs, 0);
            assert!(!cfg.coalesce_queries);
            let resources = super::super::resources::SearchResources::with_database(cfg.clone(), crate::db::Db::connect_in_memory().await.unwrap());
            let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&cfg) else { panic!("normal factory must select aggregate"); };
            backend.bing_base_url = origin.clone(); backend.ddg_base_url = origin.clone();
            let start = chrono::Utc::now().timestamp();
            let value = test_backend_selected(free_client().unwrap(), &cfg, None, "owned metadata observation", &resources, Some(SelectedBackend::Aggregate(backend))).await;
            assert!(value.error.is_none());
            assert_eq!(value.hits, 2);
            assert_eq!(calls.lock().unwrap().len(), 2, "two owned source requests, no replay");
            let observed = value.retrieved_at_utc.as_deref().expect("successful metadata result needs an actual observed completion time even with reuse disabled");
            let at = chrono::DateTime::parse_from_rfc3339(observed).unwrap().timestamp();
            assert!(at >= start && at <= chrono::Utc::now().timestamp());
            assert_eq!(value.reuse, Some(super::super::resources::SearchReuse::Fresh));
            // A delayed wrapper must retain the time produced by actual HTTP,
            // and a later cache hit must not manufacture another observation.
            let expected = value.retrieved_at_utc.clone();
            let mut cached_cfg = cfg.clone(); cached_cfg.query_cache_ttl_secs = 30;
            let cached = super::super::resources::SearchResources::new(cached_cfg.clone());
            let first = cached.run([93;32], &cached_cfg, move || async move {
                tokio::time::sleep(Duration::from_millis(1100)).await; value
            }).await;
            assert_eq!(first.retrieved_at_utc, expected);
            let second = cached.run([93;32], &cached_cfg, || async { panic!("existing metadata should be cached"); }).await;
            assert_eq!(second.retrieved_at_utc, expected);
            assert_eq!(second.reuse, Some(super::super::resources::SearchReuse::Cached));
            assert_eq!(calls.lock().unwrap().len(), 2);
        }).catch_unwind().await;
        task.abort();
        let _ = task.await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
        for (mode, hits) in [("legacy-facts", 1), ("legacy-empty-facts", 0)] {
            let (origin, calls, task) = server(mode).await;
            let checked = std::panic::AssertUnwindSafe(async {
                let cfg = SearchConfig {
                    enabled: true,
                    backend: SearchBackendKind::SearXng,
                    searxng_url: Some(origin),
                    timeout_ms: 500,
                    ..Default::default()
                };
                let resources = super::super::resources::SearchResources::with_database(
                    cfg.clone(),
                    crate::db::Db::connect_in_memory().await.unwrap(),
                );
                let before = chrono::Utc::now().timestamp();
                let value = test_backend_selected(
                    free_client().unwrap(),
                    &cfg,
                    None,
                    "owned legacy metadata",
                    &resources,
                    backend_of(&cfg),
                )
                .await;
                assert!(value.error.is_none());
                assert_eq!(value.hits, hits);
                assert_eq!(calls.lock().unwrap().len(), 1);
                let at = chrono::DateTime::parse_from_rfc3339(
                    value
                        .retrieved_at_utc
                        .as_deref()
                        .expect("completed legacy/empty response still has observed time"),
                )
                .unwrap()
                .timestamp();
                assert!(at >= before && at <= chrono::Utc::now().timestamp());
                assert_eq!(
                    value.reuse,
                    Some(super::super::resources::SearchReuse::Fresh)
                );
            })
            .catch_unwind()
            .await;
            task.abort();
            let _ = task.await;
            if let Err(p) = checked {
                std::panic::resume_unwind(p);
            }
        }
    }

    async fn server(
        mode: &'static str,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let app=Router::new().fallback(any(|State(state):State<Fixture>,uri:Uri,headers:HeaderMap,method:axum::http::Method|async move {
            state.calls.lock().unwrap().push(uri.path().to_owned());
            if state.mode=="legacy-facts" || state.mode=="legacy-empty-facts" {
                assert_eq!(method,axum::http::Method::GET);assert_eq!(uri.path(),"/search");
                return axum::Json(serde_json::json!({"results":if state.mode=="legacy-facts" {vec![serde_json::json!({"title":"owned legacy","url":"https://owned-index.invalid/result","content":"owned metadata"})]}else{vec![]}})).into_response();
            }
            if state.mode=="paid" {return (StatusCode::UNAUTHORIZED,"owned credential rejection").into_response();}
            assert!(!headers.contains_key("authorization"));assert!(!headers.contains_key("x-subscription-token"));
            if state.mode.starts_with("npm") && (uri.path()=="/-/v1/search" || state.mode!="npm-mixed") {
                assert_eq!(method,axum::http::Method::GET);assert_eq!(uri.path(),"/-/v1/search","npm scope cannot fallback or probe another path");
                let params:std::collections::HashMap<_,_>=reqwest::Url::parse(&format!("http://owned.test{uri}")).unwrap().query_pairs().into_owned().collect();assert_eq!(params.get("text").map(String::as_str),Some("npm package test"));assert_eq!(params.get("size").map(String::as_str),Some("5"));assert_eq!(params.len(),2);
                match state.mode {
                    "npm401"=>return (StatusCode::UNAUTHORIZED,"owned no retry").into_response(),
                    "npm429"=>return (StatusCode::TOO_MANY_REQUESTS,[("retry-after","60")],"owned wait").into_response(),
                    "npm-bad"=>return "<html>verify you are human</html>".into_response(),
                    "npm-huge"=>return "x".repeat(super::super::free_aggregate::MAX_BODY_BYTES+1).into_response(),
                    "npm-timeout"=>{tokio::time::sleep(Duration::from_secs(2)).await;},
                    _=>{}
                }
                return axum::Json(serde_json::json!({"objects":[{"package":{"name":"owned-package","description":"owned metadata","links":{"npm":"javascript:owned"}},"score":{"final":1.0}}]})).into_response();
            }
            if uri.path()=="/search" {
                if state.mode=="retry" {return (StatusCode::TOO_MANY_REQUESTS,[("retry-after","60")],"owned rate limit").into_response();}
                if state.mode=="retry503" {return (StatusCode::SERVICE_UNAVAILABLE,[("retry-after",(chrono::Utc::now()+chrono::Duration::seconds(60)).format("%a, %d %b %Y %H:%M:%S GMT").to_string())],"owned temporary failure").into_response();}
                if state.mode=="redirect" {return (StatusCode::FOUND,[("location","/trap")],"").into_response();}
                if state.mode=="all-fail" {return StatusCode::BAD_GATEWAY.into_response();}
                return "<rss><channel><item><title>Resolver</title><link>https://example.test/one</link></item></channel></rss>".into_response();
            }
            assert_eq!(uri.path(),"/lite/","redirect target or third source must not be called");
            if state.mode=="deadline" {tokio::time::sleep(Duration::from_secs(2)).await;}
            if state.mode=="redirect" || state.mode=="success" || state.mode=="retry" || state.mode=="retry503" || state.mode=="npm-mixed" {return "<a class=\"result-link\" href=\"https://example.test/ddg\">DDG</a>".into_response();}
            StatusCode::FORBIDDEN.into_response()
        })).with_state(Fixture{mode,calls:calls.clone()});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        (
            origin,
            calls,
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
        )
    }

    #[tokio::test]
    async fn configured_resources_reuse_real_complete_search_and_default_stays_fresh() {
        let (origin, calls, task) = server("success").await;
        let result = std::panic::AssertUnwindSafe(async {
            let mut cfg = SearchConfig {
                backend: SearchBackendKind::FreeAggregate,
                timeout_ms: 500,
                ..Default::default()
            };
            let resources = crate::search::resources::SearchResources::new(cfg.clone());
            let q = SearchQuery {
                text: "owned resource query".into(),
                max_results: 5,
            };
            for _ in 0..2 {
                let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&cfg) else {
                    panic!("production factory failed")
                };
                backend.bing_base_url = origin.clone();
                backend.ddg_base_url = origin.clone();
                let result = execute_selected_with_resources(
                    free_client().unwrap(),
                    &cfg,
                    Some("must-not-leak"),
                    &q,
                    Some(SelectedBackend::Aggregate(backend)),
                    &resources,
                )
                .await;
                assert!(result.error.is_none());
                assert!(result.failed_sources.is_empty());
            }
            assert_eq!(
                calls.lock().unwrap().len(),
                4,
                "default off must make two source calls per query"
            );
            cfg.query_cache_ttl_secs = 10;
            cfg.coalesce_queries = true;
            resources.reconfigure(&cfg);
            for _ in 0..2 {
                let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&cfg) else {
                    panic!("production factory failed")
                };
                backend.bing_base_url = origin.clone();
                backend.ddg_base_url = origin.clone();
                let result = execute_selected_with_resources(
                    free_client().unwrap(),
                    &cfg,
                    None,
                    &q,
                    Some(SelectedBackend::Aggregate(backend)),
                    &resources,
                )
                .await;
                assert!(result.error.is_none());
            }
            assert_eq!(
                calls.lock().unwrap().len(),
                6,
                "second enabled query must reuse the real source result"
            );
            assert_eq!(resources.stats().cache_hits, 1);
            let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&cfg) else {
                panic!("normal factory must resolve aggregate")
            };
            backend.bing_base_url = origin.clone();
            backend.ddg_base_url = origin.clone();
            let fresh = test_backend_selected(
                free_client().unwrap(),
                &cfg,
                None,
                &q.text,
                &resources,
                Some(SelectedBackend::Aggregate(backend)),
            )
            .await;
            assert!(fresh.error.is_none());
            assert_eq!(
                calls.lock().unwrap().len(),
                8,
                "fresh diagnostic must not use the warm query cache"
            );
        });
        use futures_util::FutureExt;
        let result = result.catch_unwind().await;
        task.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[tokio::test]
    async fn known_retry_after_survives_owned_database_reopen_and_gateway_recreation() {
        for mode in ["retry", "retry503"] {
            let (origin, calls, task) = server(mode).await;
            let path = std::env::temp_dir()
                .join(format!(
                    "llmgw-owned-search-cooldown-{}",
                    uuid::Uuid::new_v4()
                ))
                .join("owned.db");
            eprintln!("OWNED_SEARCH_COOLDOWN_DB={}", path.display());
            let result = std::panic::AssertUnwindSafe(async {
                let app = crate::config::AppConfig {
                    search: SearchConfig {
                        backend: SearchBackendKind::FreeAggregate,
                        timeout_ms: 500,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let db = crate::db::Db::connect_path(&path).await.unwrap();
                let first = crate::proxy::server::GatewayState::new(db.clone(), app.clone());
                let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&app.search) else {
                    panic!("normal factory must resolve free aggregate")
                };
                backend.bing_base_url = origin.clone();
                backend.ddg_base_url = origin.clone();
                let query = SearchQuery {
                    text: "owned retry contract".into(),
                    max_results: 5,
                };
                let first_result = execute_selected_with_resources(
                    free_client().unwrap(),
                    &app.search,
                    None,
                    &query,
                    Some(SelectedBackend::Aggregate(backend)),
                    &first.search_resources,
                )
                .await;
                assert!(first_result.error.is_none());
                assert_eq!(first_result.failed_sources, vec!["bing_rss"]);
                assert_eq!(calls.lock().unwrap().len(), 2);
                drop(first);
                db.pool().close().await;
                let reopened = crate::db::Db::connect_path(&path).await.unwrap();
                let second = crate::proxy::server::GatewayState::new(reopened.clone(), app.clone());
                let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&app.search) else {
                    panic!("normal factory must resolve free aggregate")
                };
                backend.bing_base_url = origin.clone();
                backend.ddg_base_url = origin.clone();
                let next = execute_selected_with_resources(
                    free_client().unwrap(),
                    &app.search,
                    None,
                    &query,
                    Some(SelectedBackend::Aggregate(backend)),
                    &second.search_resources,
                )
                .await;
                assert!(next.error.is_none());
                assert_eq!(next.failed_sources, vec!["bing_rss"]);
                assert_eq!(calls.lock().unwrap().len(),3,"reopened source state must block Bing while allowing the other explicitly selected source");
                let stored: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM meta WHERE key LIKE 'search.free_cooldown.v1.%'",
                )
                .fetch_one(reopened.pool())
                .await
                .unwrap();
                assert_eq!(
                    stored, 1,
                    "memory-only/global-static protection is not persistent proof"
                );
                let mut changed = app.clone();
                changed.search.backend = SearchBackendKind::DuckDuckGo;
                second.update_cfg(changed.clone());
                changed.search.backend = SearchBackendKind::FreeAggregate;
                second.update_cfg(changed);
                let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&app.search) else {
                    panic!("normal factory must resolve free aggregate")
                };
                backend.bing_base_url = origin.clone();
                backend.ddg_base_url = origin.clone();
                let after_settings = execute_selected_with_resources(
                    free_client().unwrap(),
                    &app.search,
                    None,
                    &query,
                    Some(SelectedBackend::Aggregate(backend)),
                    &second.search_resources,
                )
                .await;
                assert!(after_settings.error.is_none());
                assert_eq!(
                    calls.lock().unwrap().len(),
                    4,
                    "backend/config edits must not wash committed source protection"
                );
                let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&app.search) else {
                    panic!("normal factory must resolve aggregate")
                };
                backend.bing_base_url = origin.clone();
                backend.ddg_base_url = origin.clone();
                let diagnostic = test_backend_selected(
                    free_client().unwrap(),
                    &app.search,
                    None,
                    &query.text,
                    &second.search_resources,
                    Some(SelectedBackend::Aggregate(backend)),
                )
                .await;
                assert!(diagnostic.error.is_none());
                assert_eq!(
                    calls.lock().unwrap().len(),
                    5,
                    "fresh diagnostic bypasses cache but must not bypass source protection"
                );
            });
            use futures_util::FutureExt;
            let result = result.catch_unwind().await;
            task.abort();
            if let Err(e) = result {
                std::panic::resume_unwind(e);
            }
        }
    }

    struct OwnedHn {
        origin: String,
        calls: Arc<Mutex<Vec<String>>>,
        closed: Arc<std::sync::atomic::AtomicUsize>,
        started: Arc<std::sync::atomic::AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for OwnedHn {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl OwnedHn {
        async fn new(mode: &'static str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            let calls = Arc::new(Mutex::new(Vec::new()));
            let closed = Arc::new(AtomicUsize::new(0));
            let started = Arc::new(AtomicUsize::new(0));
            let state = (calls.clone(), closed.clone(), started.clone());
            let app=Router::new().fallback(any(move |uri:Uri,method:axum::http::Method,headers:HeaderMap|{let s=state.clone();async move{
                assert!(!headers.contains_key("authorization"));assert!(!headers.contains_key("proxy-authorization"));assert!(!headers.contains_key("x-subscription-token"));s.0.lock().unwrap().push(uri.path().to_owned());
                if uri.path()=="/search" {return "<rss><channel><item><title>Web</title><link>https://example.test/web</link></item></channel></rss>".into_response();}
                if uri.path()=="/lite/" {return "<a class=\"result-link\" href=\"https://example.test/ddg\">Web DDG</a>".into_response();}
                assert_eq!(method,axum::http::Method::GET);assert_eq!(uri.path(),"/api/v1/search");let query:std::collections::HashMap<_,_>=reqwest::Url::parse(&format!("http://owned.test{}",uri.path_and_query().unwrap().as_str())).unwrap().query_pairs().into_owned().collect();assert_eq!(query.get("query").map(String::as_str),Some("Rust programming"));assert_eq!(query.get("tags").map(String::as_str),Some("story"));assert_eq!(query.get("hitsPerPage").map(String::as_str),Some("5"));assert_eq!(query.len(),3);
                match mode {"401"=>return StatusCode::UNAUTHORIZED.into_response(),"429"=>return (StatusCode::TOO_MANY_REQUESTS,[("retry-after","60")],"owned wait").into_response(),"503"=>return (StatusCode::SERVICE_UNAVAILABLE,[("retry-after","60")],"owned wait").into_response(),"bad"=>return "<html>captcha</html>".into_response(),"big"=>return "x".repeat(super::super::free_aggregate::MAX_BODY_BYTES+1).into_response(),"timeout"=>{tokio::time::sleep(Duration::from_secs(2)).await;},"proxy"=>assert_eq!(uri.scheme_str(),Some("http")),"hold"=>{let body=async_stream::stream!{let _closed=BodyClosed(s.1);s.2.fetch_add(1,Ordering::SeqCst);yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from_static(b"{\"hits\":["));std::future::pending::<()>().await;};return axum::http::Response::builder().status(200).body(axum::body::Body::from_stream(body)).unwrap();},_=>{}}
                axum::Json(serde_json::json!({"hits":[{"objectID":"123","title":"Owned Rust programming","points":0,"num_comments":null,"_tags":["story"],"url":"javascript:owned"}]})).into_response()
            }}));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                origin,
                calls,
                closed,
                started,
                task,
            }
        }
    }
    async fn hn_owned_query(
        r: &crate::search::resources::SearchResources,
        cfg: &SearchConfig,
        origin: &str,
        text: &str,
        diagnostic: bool,
    ) -> SearchOutcome {
        let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(cfg) else {
            panic!("normal factory required")
        };
        backend.bing_base_url = origin.into();
        backend.ddg_base_url = origin.into();
        backend.npm_base_url = origin.into();
        backend.hn_base_url = origin.into();
        if diagnostic {
            test_backend_selected(
                free_client().unwrap(),
                cfg,
                Some("owned-not-forwarded"),
                text,
                r,
                Some(SelectedBackend::Aggregate(backend)),
            )
            .await
        } else {
            execute_selected_with_resources(
                free_client().unwrap(),
                cfg,
                Some("owned-not-forwarded"),
                &SearchQuery {
                    text: text.into(),
                    max_results: 5,
                },
                Some(SelectedBackend::Aggregate(backend)),
                r,
            )
            .await
        }
    }
    #[tokio::test]
    async fn hn_owned_success_cache_known_wait_and_failures_use_real_consumers() {
        let source = OwnedHn::new("success").await;
        let cfg = SearchConfig {
            backend: SearchBackendKind::FreeAggregate,
            free_search_scope: crate::config::FreeSearchScope::HackerNews,
            timeout_ms: 1000,
            ..Default::default()
        };
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let state = crate::proxy::server::GatewayState::new(
            db.clone(),
            crate::config::AppConfig {
                search: cfg.clone(),
                ..Default::default()
            },
        );
        for q in ["Hello World", "Apple stock news", "今天电影新闻"] {
            assert!(
                hn_owned_query(&state.search_resources, &cfg, &source.origin, q, false)
                    .await
                    .error
                    .is_some()
            );
        }
        assert!(source.calls.lock().unwrap().is_empty());
        let ok = hn_owned_query(
            &state.search_resources,
            &cfg,
            &source.origin,
            "Rust programming",
            false,
        )
        .await;
        assert!(ok.error.is_none());
        assert_eq!(ok.result_sources, vec!["hacker_news"]);
        assert_eq!(
            ok.results[0].url,
            "https://news.ycombinator.com/item?id=123"
        );
        assert_eq!(source.calls.lock().unwrap().as_slice(), ["/api/v1/search"]);
        let raw: String = sqlx::query_scalar(
            "SELECT value FROM meta WHERE key LIKE 'search.free_cooldown.v1.hacker_news.%'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        let raw: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(raw["reason"], "service_policy");
        assert_eq!(
            raw["not_before_ms"].as_i64().unwrap() - raw["observed_ms"].as_i64().unwrap(),
            360
        );
        let proxy = OwnedHn::new("proxy").await;
        let proxy_cfg = SearchConfig {
            free_proxy_url: Some(proxy.origin.clone()),
            ..cfg.clone()
        };
        let via = crate::proxy::server::GatewayState::new(
            crate::db::Db::connect_in_memory().await.unwrap(),
            crate::config::AppConfig {
                search: proxy_cfg.clone(),
                ..Default::default()
            },
        );
        assert!(hn_owned_query(
            &via.search_resources,
            &proxy_cfg,
            &source.origin,
            "Rust programming",
            true
        )
        .await
        .error
        .is_none());
        assert_eq!(proxy.calls.lock().unwrap().len(), 1);
        assert_eq!(
            source.calls.lock().unwrap().len(),
            1,
            "explicit egress must not contact direct origin"
        );
        let cached = OwnedHn::new("success").await;
        let mut c = SearchConfig {
            query_cache_ttl_secs: 10,
            coalesce_queries: true,
            source_min_interval_ms: 60000,
            ..cfg.clone()
        };
        let gateway = crate::proxy::server::GatewayState::new(
            crate::db::Db::connect_in_memory().await.unwrap(),
            crate::config::AppConfig {
                search: c.clone(),
                ..Default::default()
            },
        );
        let r = &gateway.search_resources;
        assert!(
            hn_owned_query(r, &c, &cached.origin, "Rust programming", false)
                .await
                .error
                .is_none()
        );
        assert_eq!(
            hn_owned_query(r, &c, &cached.origin, "Rust programming", false)
                .await
                .reuse,
            Some(crate::search::resources::SearchReuse::Cached)
        );
        assert_eq!(cached.calls.lock().unwrap().len(), 1);
        c.free_search_scope = crate::config::FreeSearchScope::Web;
        r.reconfigure(&c);
        let web = hn_owned_query(r, &c, &cached.origin, "Rust programming", false).await;
        assert_eq!(web.result_sources, vec!["bing_rss", "duckduckgo"]);
        assert_eq!(
            cached.calls.lock().unwrap().len(),
            3,
            "web scope does not silently add HN even for technical text"
        );
        c.free_search_scope = crate::config::FreeSearchScope::HackerNews;
        r.reconfigure(&c);
        assert!(
            hn_owned_query(r, &c, &cached.origin, "Rust programming", true)
                .await
                .error
                .is_some()
        );
        assert_eq!(
            cached.calls.lock().unwrap().len(),
            3,
            "scope switching cannot wash admitted source time or borrow web cache"
        );
        for mode in ["401", "429", "503", "bad", "big", "timeout"] {
            let failed = OwnedHn::new(mode).await;
            let path = std::env::temp_dir()
                .join(format!("llmgw-owned-hn-{}", uuid::Uuid::new_v4()))
                .join("owned.db");
            eprintln!("OWNED_HN_DB={}", path.display());
            let db = crate::db::Db::connect_path(&path).await.unwrap();
            let f = SearchConfig {
                query_cache_ttl_secs: 10,
                coalesce_queries: true,
                timeout_ms: 500,
                ..cfg.clone()
            };
            let g = crate::proxy::server::GatewayState::new(
                db.clone(),
                crate::config::AppConfig {
                    search: f.clone(),
                    ..Default::default()
                },
            );
            assert!(
                hn_owned_query(
                    &g.search_resources,
                    &f,
                    &failed.origin,
                    "Rust programming",
                    false
                )
                .await
                .error
                .is_some(),
                "{mode}"
            );
            assert_eq!(failed.calls.lock().unwrap().as_slice(), ["/api/v1/search"]);
            assert_eq!(g.search_resources.stats().entries, 0);
            if matches!(mode, "429" | "503") {
                drop(g);
                db.pool().close().await;
                let reopened = crate::db::Db::connect_path(&path).await.unwrap();
                let p = OwnedSearchProxy::new("refuse").await;
                let changed = SearchConfig {
                    free_proxy_url: Some(p.url.clone()),
                    ..f
                };
                let reset = crate::proxy::server::GatewayState::new(
                    reopened,
                    crate::config::AppConfig {
                        search: changed.clone(),
                        ..Default::default()
                    },
                );
                assert!(hn_owned_query(
                    &reset.search_resources,
                    &changed,
                    &failed.origin,
                    "Rust programming",
                    true
                )
                .await
                .error
                .is_some());
                assert_eq!(failed.calls.lock().unwrap().len(), 1);
                assert!(
                    p.calls.lock().unwrap().is_empty(),
                    "owned DB reopen and egress changes cannot erase HN Retry-After"
                );
            }
        }
    }
    #[tokio::test]
    async fn hn_cancel_closes_owned_body_without_cache_or_time_refund() {
        use std::sync::atomic::Ordering;
        let held = OwnedHn::new("hold").await;
        let cfg = SearchConfig {
            backend: SearchBackendKind::FreeAggregate,
            free_search_scope: crate::config::FreeSearchScope::HackerNews,
            source_min_interval_ms: 60000,
            query_cache_ttl_secs: 10,
            coalesce_queries: true,
            timeout_ms: 3000,
            ..Default::default()
        };
        let gateway = Arc::new(crate::proxy::server::GatewayState::new(
            crate::db::Db::connect_in_memory().await.unwrap(),
            crate::config::AppConfig {
                search: cfg.clone(),
                ..Default::default()
            },
        ));
        let caller = {
            let g = gateway.clone();
            let origin = held.origin.clone();
            let c = cfg.clone();
            tokio::spawn(async move {
                hn_owned_query(&g.search_resources, &c, &origin, "Rust programming", false).await
            })
        };
        wait_resource(|| held.started.load(Ordering::SeqCst) == 1).await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        wait_resource(|| held.closed.load(Ordering::SeqCst) == 1).await;
        assert_eq!(gateway.search_resources.stats().entries, 0);
        assert_eq!(gateway.search_resources.stats().inflight, 0);
        assert_eq!(held.calls.lock().unwrap().len(), 1);
        assert!(hn_owned_query(
            &gateway.search_resources,
            &cfg,
            &held.origin,
            "Rust programming",
            true
        )
        .await
        .error
        .is_some());
        assert_eq!(held.calls.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn admission_expired_cas_blocks_real_hn_http_until_an_explicit_new_call() {
        let source = OwnedHn::new("success").await;
        let cfg = SearchConfig {
            backend: SearchBackendKind::FreeAggregate,
            free_search_scope: crate::config::FreeSearchScope::HackerNews,
            timeout_ms: 3000,
            ..Default::default()
        };
        let db = crate::db::Db::connect_in_memory().await.unwrap();
        let gateway = crate::proxy::server::GatewayState::new(
            db.clone(),
            crate::config::AppConfig {
                search: cfg.clone(),
                ..Default::default()
            },
        );
        let held = db.pool().acquire().await.unwrap();
        let mut request = Box::pin(hn_owned_query(
            &gateway.search_resources,
            &cfg,
            &source.origin,
            "Rust programming",
            false,
        ));
        assert!(futures_util::poll!(&mut request).is_pending());
        let mut barrier = Box::pin(db.pool().acquire());
        assert!(futures_util::poll!(&mut barrier).is_pending());
        drop(held);
        let held_cas = tokio::select! {v=&mut request=>panic!("fixture expected source CAS pool wait, got {v:?}"),b=&mut barrier=>b.unwrap()};
        assert!(futures_util::poll!(&mut request).is_pending());
        tokio::time::sleep(Duration::from_millis(400)).await;
        drop(held_cas);
        let rejected = request.await;
        assert_eq!(
            source.calls.lock().unwrap().len(),
            0,
            "an expired CAS acknowledgement must not dispatch actual source HTTP"
        );
        assert!(
            rejected
                .error
                .as_deref()
                .is_some_and(|e| e.contains("admission")),
            "must be an admission rejection, not a fixture timeout: {:?}",
            rejected.error
        );
        let accepted = hn_owned_query(
            &gateway.search_resources,
            &cfg,
            &source.origin,
            "Rust programming",
            false,
        )
        .await;
        assert!(accepted.error.is_none());
        assert_eq!(accepted.result_sources, vec!["hacker_news"]);
        assert_eq!(
            source.calls.lock().unwrap().as_slice(),
            ["/api/v1/search"],
            "only the explicit fresh invocation may dispatch, not an internal replay"
        );
    }
    #[tokio::test]
    async fn explicit_hn_scope_accepts_the_production_command_contract() {
        let input = serde_json::json!({"enabled":true,"backend":"free_aggregate","free_search_scope":"hacker_news","searxng_url":null,"max_results":5,"timeout_ms":1000,"inject_as":"system"});
        let parsed = serde_json::from_value::<crate::search::SearchSettingsInput>(input.clone());
        assert!(parsed.is_ok(),"the explicit HN choice must be accepted by the actual production settings decoder, not hidden behind a test factory: {:?}",parsed.err());
        let cfg: SearchConfig = serde_json::from_value(input).unwrap();
        assert_eq!(
            serde_json::to_value(&cfg).unwrap()["free_search_scope"],
            "hacker_news"
        );
    }
    #[tokio::test]
    async fn configured_search_egress_reaches_only_the_owned_proxy() {
        let (origin, origin_calls, origin_task) = server("success").await;
        let proxy_calls = Arc::new(Mutex::new(Vec::new()));
        let captured = proxy_calls.clone();
        let proxy=Router::new().fallback(any(move |uri:Uri,method:axum::http::Method,headers:HeaderMap|{let calls=captured.clone();async move {
            assert!(!headers.contains_key("authorization"));assert!(!headers.contains_key("proxy-authorization"));assert!(!headers.contains_key("x-subscription-token"));
            assert_eq!(uri.scheme_str(),Some("http"));assert!(uri.authority().unwrap().as_str().starts_with("127.0.0.1:"));
            calls.lock().unwrap().push((method.to_string(),uri.path().to_owned()));
            if uri.path()=="/search" {"<rss><channel><item><title>Proxy RSS</title><link>https://example.test/proxy-rss</link></item></channel></rss>"}else{assert_eq!(uri.path(),"/lite/");"<a class=\"result-link\" href=\"https://example.test/proxy-ddg\">Proxy DDG</a>"}
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", listener.local_addr().unwrap());
        let proxy_task = tokio::spawn(async move {
            axum::serve(listener, proxy).await.unwrap();
        });
        let cfg:SearchConfig=serde_json::from_value(serde_json::json!({"backend":"free_aggregate","timeout_ms":1000,"free_proxy_url":proxy_url})).unwrap();
        let result = std::panic::AssertUnwindSafe(async {
            let state = crate::proxy::server::GatewayState::new(
                crate::db::Db::connect_in_memory().await.unwrap(),
                crate::config::AppConfig {
                    search: cfg.clone(),
                    ..Default::default()
                },
            );
            let Some(SelectedBackend::Aggregate(mut b)) = backend_of(&cfg) else {
                panic!("normal production factory required")
            };
            b.bing_base_url = origin.clone();
            b.ddg_base_url = origin;
            let value = test_backend_selected(
                free_client().unwrap(),
                &cfg,
                Some("owned-no-forward"),
                "owned proxy query",
                &state.search_resources,
                Some(SelectedBackend::Aggregate(b)),
            )
            .await;
            assert_eq!(
                proxy_calls.lock().unwrap().len(),
                2,
                "an explicit search proxy must carry both source requests"
            );
            assert!(value.error.is_none());
            assert_eq!(
                origin_calls.lock().unwrap().len(),
                0,
                "proxy configuration cannot silently fall through to direct source HTTP"
            );
        });
        use futures_util::FutureExt;
        let result = result.catch_unwind().await;
        proxy_task.abort();
        origin_task.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
    struct OwnedSearchProxy {
        url: String,
        calls: Arc<Mutex<Vec<(String, String)>>>,
        started: Arc<std::sync::atomic::AtomicUsize>,
        closed: Arc<std::sync::atomic::AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for OwnedSearchProxy {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl OwnedSearchProxy {
        async fn new(mode: &'static str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            let calls = Arc::new(Mutex::new(Vec::new()));
            let started = Arc::new(AtomicUsize::new(0));
            let closed = Arc::new(AtomicUsize::new(0));
            let state = (calls.clone(), started.clone(), closed.clone());
            let app=Router::new().fallback(any(move |uri:Uri,method:axum::http::Method,headers:HeaderMap|{let state=state.clone();async move {
                assert!(!headers.contains_key("authorization"));assert!(!headers.contains_key("proxy-authorization"));assert!(!headers.contains_key("x-subscription-token"));
                state.0.lock().unwrap().push((method.to_string(),uri.to_string()));
                if mode=="connect-refuse" {assert_eq!(method,axum::http::Method::CONNECT);return StatusCode::PROXY_AUTHENTICATION_REQUIRED.into_response();}
                assert_eq!(method,axum::http::Method::GET);assert_eq!(uri.scheme_str(),Some("http"));assert_eq!(uri.path(),"/-/v1/search");
                if mode=="refuse" {return StatusCode::PROXY_AUTHENTICATION_REQUIRED.into_response();}
                if mode=="limit" {return (StatusCode::TOO_MANY_REQUESTS,[("retry-after","60")],"owned wait").into_response();}
                if mode=="wait" {
                    let stream=async_stream::stream!{let _closed=BodyClosed(state.2);state.1.fetch_add(1,Ordering::SeqCst);yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from_static(b"{\"objects\":["));std::future::pending::<()>().await;};
                    return axum::http::Response::builder().status(200).body(axum::body::Body::from_stream(stream)).unwrap();
                }
                axum::Json(serde_json::json!({"objects":[{"package":{"name":format!("owned-{mode}")}}]})).into_response()
            }}));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                url,
                calls,
                started,
                closed,
                task,
            }
        }
    }
    async fn egress_owned_query(
        resources: &crate::search::resources::SearchResources,
        cfg: &SearchConfig,
        origin: &str,
        diagnostic: bool,
    ) -> SearchOutcome {
        let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(cfg) else {
            panic!("normal factory required")
        };
        backend.bing_base_url = origin.into();
        backend.ddg_base_url = origin.into();
        backend.npm_base_url = origin.into();
        if diagnostic {
            test_backend_selected(
                free_client().unwrap(),
                cfg,
                Some("owned-no-forward"),
                "npm package test",
                resources,
                Some(SelectedBackend::Aggregate(backend)),
            )
            .await
        } else {
            execute_selected_with_resources(
                free_client().unwrap(),
                cfg,
                Some("owned-no-forward"),
                &SearchQuery {
                    text: "npm package test".into(),
                    max_results: 5,
                },
                Some(SelectedBackend::Aggregate(backend)),
                resources,
            )
            .await
        }
    }
    #[tokio::test]
    async fn egress_switches_isolate_cache_and_failures_never_fallback_or_clear_source_wait() {
        let (origin, origin_calls, origin_task) = server("npm-mixed").await;
        let a = OwnedSearchProxy::new("a").await;
        let b = OwnedSearchProxy::new("b").await;
        let refuse = OwnedSearchProxy::new("refuse").await;
        let limit = OwnedSearchProxy::new("limit").await;
        let result = std::panic::AssertUnwindSafe(async {
            let direct = SearchConfig {
                backend: SearchBackendKind::FreeAggregate,
                free_search_scope: crate::config::FreeSearchScope::NpmPackages,
                query_cache_ttl_secs: 10,
                coalesce_queries: true,
                timeout_ms: 1000,
                ..Default::default()
            };
            let mut cfg = direct.clone();
            let gateway = crate::proxy::server::GatewayState::new(
                crate::db::Db::connect_in_memory().await.unwrap(),
                crate::config::AppConfig {
                    search: cfg.clone(),
                    ..Default::default()
                },
            );
            let r = &gateway.search_resources;
            assert_eq!(
                egress_owned_query(r, &cfg, &origin, false).await.results[0].title,
                "owned-package"
            );
            assert_eq!(origin_calls.lock().unwrap().len(), 1);
            cfg.free_proxy_url = Some(a.url.clone());
            r.reconfigure(&cfg);
            let old_a = cfg.clone();
            assert_eq!(
                egress_owned_query(r, &cfg, &origin, false).await.results[0].title,
                "owned-a"
            );
            assert_eq!(
                egress_owned_query(r, &cfg, &origin, false).await.reuse,
                Some(crate::search::resources::SearchReuse::Cached)
            );
            assert_eq!(a.calls.lock().unwrap().len(), 1);
            assert_eq!(origin_calls.lock().unwrap().len(), 1);
            assert_eq!(
                egress_owned_query(r, &direct, &origin, false).await.results[0].title,
                "owned-package"
            );
            assert_eq!(origin_calls.lock().unwrap().len(),2,"an old captured direct request keeps its own route, without borrowing the proxy cache");
            cfg.free_proxy_url = Some(b.url.clone());
            r.reconfigure(&cfg);
            assert_eq!(
                egress_owned_query(r, &cfg, &origin, false).await.results[0].title,
                "owned-b"
            );
            assert_eq!(b.calls.lock().unwrap().len(), 1);
            assert_eq!(
                egress_owned_query(r, &old_a, &origin, false).await.results[0].title,
                "owned-a"
            );
            assert_eq!(
                a.calls.lock().unwrap().len(),
                2,
                "old captured proxy must not be silently routed through the new one"
            );
            cfg.free_proxy_url = Some(refuse.url.clone());
            r.reconfigure(&cfg);
            assert!(egress_owned_query(r, &cfg, &origin, true)
                .await
                .error
                .is_some());
            assert_eq!(refuse.calls.lock().unwrap().len(), 1);
            assert_eq!(r.stats().entries, 0);
            assert_eq!(
                origin_calls.lock().unwrap().len(),
                2,
                "proxy rejection is not direct fallback"
            );
            cfg.free_proxy_url = Some(limit.url.clone());
            r.reconfigure(&cfg);
            assert!(egress_owned_query(r, &cfg, &origin, true)
                .await
                .error
                .is_some());
            assert_eq!(limit.calls.lock().unwrap().len(), 1);
            cfg.free_proxy_url = Some(b.url.clone());
            r.reconfigure(&cfg);
            assert!(egress_owned_query(r, &cfg, &origin, true)
                .await
                .error
                .is_some());
            assert_eq!(
                b.calls.lock().unwrap().len(),
                1,
                "changing egress must not wash a physical source Retry-After"
            );
            let target = super::super::cooldown::Target::new(
                super::super::cooldown::Source::NpmRegistry,
                &origin,
            )
            .unwrap();
            assert_eq!(r.cooldowns().unwrap().view(&target).await.state, "cooldown");
        });
        use futures_util::FutureExt;
        let result = result.catch_unwind().await;
        origin_task.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
    #[tokio::test]
    async fn egress_owned_socks5h_transfers_target_name_and_http_connect_refuses_without_fallback()
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("socks5h://{}", listener.local_addr().unwrap());
        let mut socks = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = [0; 2];
            stream.read_exact(&mut head).await.unwrap();
            assert_eq!(head[0], 5);
            assert!((1..=8).contains(&head[1]));
            let mut methods = vec![0; head[1] as usize];
            stream.read_exact(&mut methods).await.unwrap();
            assert_eq!(methods, vec![0]);
            stream.write_all(&[5, 0]).await.unwrap();
            let mut request = [0; 4];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(
                request,
                [5, 1, 0, 3],
                "destination hostname must be carried to the owned SOCKS proxy"
            );
            let size = stream.read_u8().await.unwrap() as usize;
            let mut hostname = vec![0; size];
            stream.read_exact(&mut hostname).await.unwrap();
            assert_eq!(hostname, b"owned-npm.invalid");
            assert_eq!(stream.read_u16().await.unwrap(), 80);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                assert!(bytes.len() < 8192);
                bytes.push(stream.read_u8().await.unwrap());
            }
            let text = String::from_utf8(bytes).unwrap();
            assert!(text.starts_with("GET /-/v1/search?"));
            let lower = text.to_ascii_lowercase();
            assert!(!lower.contains("\r\nauthorization:"));
            assert!(!lower.contains("\r\nproxy-authorization:"));
            let body = r#"{"objects":[{"package":{"name":"owned-socks"}}]}"#;
            let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let result = std::panic::AssertUnwindSafe(async {
            let cfg = SearchConfig {
                backend: SearchBackendKind::FreeAggregate,
                free_search_scope: crate::config::FreeSearchScope::NpmPackages,
                free_proxy_url: Some(proxy_url),
                timeout_ms: 1000,
                ..Default::default()
            };
            let gateway = crate::proxy::server::GatewayState::new(
                crate::db::Db::connect_in_memory().await.unwrap(),
                crate::config::AppConfig {
                    search: cfg.clone(),
                    ..Default::default()
                },
            );
            let value = egress_owned_query(
                &gateway.search_resources,
                &cfg,
                "http://owned-npm.invalid",
                true,
            )
            .await;
            assert!(value.error.is_none(), "{:?}", value.error);
            assert_eq!(value.results[0].title, "owned-socks");
            tokio::time::timeout(Duration::from_secs(3), &mut socks)
                .await
                .unwrap()
                .unwrap();
        });
        use futures_util::FutureExt;
        let result = result.catch_unwind().await;
        socks.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
        let proxy = OwnedSearchProxy::new("connect-refuse").await;
        let (origin, origin_calls, task) = server("npm-mixed").await;
        let result = std::panic::AssertUnwindSafe(async {
            let cfg = SearchConfig {
                backend: SearchBackendKind::FreeAggregate,
                free_search_scope: crate::config::FreeSearchScope::NpmPackages,
                free_proxy_url: Some(proxy.url.clone()),
                timeout_ms: 1000,
                ..Default::default()
            };
            let gateway = crate::proxy::server::GatewayState::new(
                crate::db::Db::connect_in_memory().await.unwrap(),
                crate::config::AppConfig {
                    search: cfg.clone(),
                    ..Default::default()
                },
            );
            assert!(egress_owned_query(
                &gateway.search_resources,
                &cfg,
                &origin.replacen("http:", "https:", 1),
                true
            )
            .await
            .error
            .is_some());
            let calls = proxy.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, "CONNECT");
            assert_eq!(origin_calls.lock().unwrap().len(), 0);
        });
        let result = result.catch_unwind().await;
        task.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
    #[tokio::test]
    async fn egress_cancel_closes_the_owned_proxy_body_and_keeps_admission() {
        use std::sync::atomic::Ordering;
        let proxy = OwnedSearchProxy::new("wait").await;
        let (origin, origin_calls, task) = server("npm-mixed").await;
        let result = std::panic::AssertUnwindSafe(async {
            let cfg = SearchConfig {
                backend: SearchBackendKind::FreeAggregate,
                free_search_scope: crate::config::FreeSearchScope::NpmPackages,
                free_proxy_url: Some(proxy.url.clone()),
                source_min_interval_ms: 60000,
                query_cache_ttl_secs: 10,
                coalesce_queries: true,
                timeout_ms: 3000,
                ..Default::default()
            };
            let gateway = Arc::new(crate::proxy::server::GatewayState::new(
                crate::db::Db::connect_in_memory().await.unwrap(),
                crate::config::AppConfig {
                    search: cfg.clone(),
                    ..Default::default()
                },
            ));
            let caller = {
                let gateway = gateway.clone();
                let cfg = cfg.clone();
                let origin = origin.clone();
                tokio::spawn(async move {
                    egress_owned_query(&gateway.search_resources, &cfg, &origin, false).await
                })
            };
            wait_resource(|| proxy.started.load(Ordering::SeqCst) == 1).await;
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            wait_resource(|| proxy.closed.load(Ordering::SeqCst) == 1).await;
            assert_eq!(proxy.calls.lock().unwrap().len(), 1);
            assert!(origin_calls.lock().unwrap().is_empty());
            assert_eq!(gateway.search_resources.stats().entries, 0);
            assert_eq!(gateway.search_resources.stats().inflight, 0);
            let target = super::super::cooldown::Target::new(
                super::super::cooldown::Source::NpmRegistry,
                &origin,
            )
            .unwrap();
            assert!(!gateway
                .search_resources
                .cooldowns()
                .unwrap()
                .reserve(&target, 0)
                .await
                .unwrap());
        });
        use futures_util::FutureExt;
        let result = result.catch_unwind().await;
        task.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
    #[tokio::test]
    async fn explicit_npm_scope_uses_only_the_selected_technical_source() {
        let (origin, calls, task) = server("npm").await;
        let cfg:SearchConfig=serde_json::from_value(serde_json::json!({"backend":"free_aggregate","timeout_ms":1000,"free_search_scope":"npm_packages"})).unwrap();
        let result = std::panic::AssertUnwindSafe(async {
            let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&cfg) else {
                panic!("normal factory required")
            };
            backend.bing_base_url = origin.clone();
            backend.ddg_base_url = origin.clone();
            backend.npm_base_url = origin;
            let state = crate::proxy::server::GatewayState::new(
                crate::db::Db::connect_in_memory().await.unwrap(),
                crate::config::AppConfig {
                    search: cfg.clone(),
                    ..Default::default()
                },
            );
            let query = SearchQuery {
                text: "npm package test".into(),
                max_results: 5,
            };
            let outcome = execute_selected_with_resources(
                free_client().unwrap(),
                &cfg,
                Some("owned-must-not-send"),
                &query,
                Some(SelectedBackend::Aggregate(backend)),
                &state.search_resources,
            )
            .await;
            assert_eq!(
                outcome.result_sources,
                vec!["npm_registry"],
                "an explicitly selected npm scope must not silently perform general web search"
            );
            assert!(outcome.error.is_none());
            assert_eq!(calls.lock().unwrap().len(), 1);
        });
        use futures_util::FutureExt;
        let result = result.catch_unwind().await;
        task.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
    #[tokio::test]
    async fn npm_scope_cache_guards_and_failures_use_the_same_production_consumer() {
        async fn run(
            resources: &crate::search::resources::SearchResources,
            cfg: &SearchConfig,
            origin: &str,
            text: &str,
        ) -> SearchOutcome {
            let Some(SelectedBackend::Aggregate(mut b)) = backend_of(cfg) else {
                panic!("normal factory required")
            };
            b.bing_base_url = origin.into();
            b.ddg_base_url = origin.into();
            b.npm_base_url = origin.into();
            execute_selected_with_resources(
                free_client().unwrap(),
                cfg,
                Some("owned-never-forward"),
                &SearchQuery {
                    text: text.into(),
                    max_results: 5,
                },
                Some(SelectedBackend::Aggregate(b)),
                resources,
            )
            .await
        }
        let (origin, calls, task) = server("npm-mixed").await;
        let result = std::panic::AssertUnwindSafe(async {
            let mut cfg = SearchConfig {
                backend: SearchBackendKind::FreeAggregate,
                free_search_scope: crate::config::FreeSearchScope::NpmPackages,
                query_cache_ttl_secs: 10,
                coalesce_queries: true,
                timeout_ms: 1000,
                ..Default::default()
            };
            let db = crate::db::Db::connect_in_memory().await.unwrap();
            let state = crate::proxy::server::GatewayState::new(
                db,
                crate::config::AppConfig {
                    search: cfg.clone(),
                    ..Default::default()
                },
            );
            let r = &state.search_resources;
            for text in ["Hello world", "今天 npm 公司的新闻", "Rust教程"] {
                let value = run(r, &cfg, &origin, text).await;
                assert!(value.error.is_some());
            }
            assert!(
                calls.lock().unwrap().is_empty(),
                "unrelated Latin/Chinese queries must not dispatch even after explicit npm opt-in"
            );
            let first = run(r, &cfg, &origin, "npm package test").await;
            assert!(first.error.is_none());
            assert_eq!(
                first.results[0].url,
                "https://www.npmjs.com/package/owned-package"
            );
            assert_eq!(first.results[0].score, 0.0);
            let second = run(r, &cfg, &origin, "npm package test").await;
            assert_eq!(
                second.reuse,
                Some(crate::search::resources::SearchReuse::Cached)
            );
            assert_eq!(calls.lock().unwrap().len(), 1);
            cfg.free_search_scope = crate::config::FreeSearchScope::Web;
            r.reconfigure(&cfg);
            let web = run(r, &cfg, &origin, "npm package test").await;
            assert!(web.error.is_none());
            assert_eq!(web.result_sources, vec!["bing_rss", "duckduckgo"]);
            assert_eq!(calls.lock().unwrap().len(), 3);
            cfg.free_search_scope = crate::config::FreeSearchScope::NpmPackages;
            r.reconfigure(&cfg);
            assert_eq!(
                run(r, &cfg, &origin, "npm package test")
                    .await
                    .result_sources,
                vec!["npm_registry"]
            );
            assert_eq!(
                calls.lock().unwrap().len(),
                4,
                "web cache cannot become an npm result"
            );
        });
        use futures_util::FutureExt;
        let result = result.catch_unwind().await;
        task.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
        for mode in ["npm401", "npm429", "npm-bad", "npm-huge", "npm-timeout"] {
            let (origin, calls, task) = server(mode).await;
            let result = std::panic::AssertUnwindSafe(async {
                let cfg = SearchConfig {
                    backend: SearchBackendKind::FreeAggregate,
                    free_search_scope: crate::config::FreeSearchScope::NpmPackages,
                    query_cache_ttl_secs: 10,
                    coalesce_queries: true,
                    timeout_ms: 500,
                    ..Default::default()
                };
                let db = crate::db::Db::connect_in_memory().await.unwrap();
                let state = crate::proxy::server::GatewayState::new(
                    db.clone(),
                    crate::config::AppConfig {
                        search: cfg.clone(),
                        ..Default::default()
                    },
                );
                let outcome = run(&state.search_resources, &cfg, &origin, "npm package test").await;
                assert!(outcome.error.is_some(), "{mode}");
                assert_eq!(
                    calls.lock().unwrap().as_slice(),
                    ["/-/v1/search"],
                    "no web fallback/auth retry"
                );
                assert_eq!(state.search_resources.stats().entries, 0);
                if mode == "npm429" {
                    let fresh = crate::proxy::server::GatewayState::new(
                        db,
                        crate::config::AppConfig {
                            search: cfg.clone(),
                            ..Default::default()
                        },
                    );
                    assert!(
                        run(&fresh.search_resources, &cfg, &origin, "npm package test")
                            .await
                            .error
                            .is_some()
                    );
                    assert_eq!(
                        calls.lock().unwrap().len(),
                        1,
                        "new owner must retain the acknowledged npm source wait"
                    );
                }
            });
            let result = result.catch_unwind().await;
            task.abort();
            if let Err(e) = result {
                std::panic::resume_unwind(e);
            }
        }
    }
    #[tokio::test]
    async fn npm_cancel_closes_only_the_owned_body_and_keeps_the_admitted_slot() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let state = (calls.clone(), closed.clone());
        let app=Router::new().fallback(any(move |uri:Uri,method:axum::http::Method,headers:HeaderMap|{let state=state.clone();async move {
            assert_eq!(uri.path(),"/-/v1/search");assert_eq!(method,axum::http::Method::GET);assert!(!headers.contains_key("authorization"));
            let stream=async_stream::stream!{let _closed=BodyClosed(state.1);state.0.fetch_add(1,Ordering::SeqCst);yield Ok::<_,std::convert::Infallible>(axum::body::Bytes::from_static(b"{\"objects\":["));std::future::pending::<()>().await;};
            axum::http::Response::builder().status(200).body(axum::body::Body::from_stream(stream)).unwrap()
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let result = std::panic::AssertUnwindSafe(async {
            let cfg = SearchConfig {
                backend: SearchBackendKind::FreeAggregate,
                free_search_scope: crate::config::FreeSearchScope::NpmPackages,
                source_min_interval_ms: 60000,
                query_cache_ttl_secs: 10,
                coalesce_queries: true,
                timeout_ms: 3000,
                ..Default::default()
            };
            let gateway = Arc::new(crate::proxy::server::GatewayState::new(
                crate::db::Db::connect_in_memory().await.unwrap(),
                crate::config::AppConfig {
                    search: cfg.clone(),
                    ..Default::default()
                },
            ));
            let caller = {
                let cfg = cfg.clone();
                let gateway = gateway.clone();
                let origin = origin.clone();
                tokio::spawn(async move {
                    let Some(SelectedBackend::Aggregate(mut b)) = backend_of(&cfg) else {
                        panic!("normal factory")
                    };
                    b.bing_base_url = origin.clone();
                    b.ddg_base_url = origin.clone();
                    b.npm_base_url = origin;
                    execute_selected_with_resources(
                        free_client().unwrap(),
                        &cfg,
                        None,
                        &SearchQuery {
                            text: "npm package test".into(),
                            max_results: 5,
                        },
                        Some(SelectedBackend::Aggregate(b)),
                        &gateway.search_resources,
                    )
                    .await
                })
            };
            wait_resource(|| calls.load(Ordering::SeqCst) == 1).await;
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            wait_resource(|| closed.load(Ordering::SeqCst) == 1).await;
            assert_eq!(gateway.search_resources.stats().entries, 0);
            assert_eq!(gateway.search_resources.stats().inflight, 0);
            let target = super::super::cooldown::Target::new(
                super::super::cooldown::Source::NpmRegistry,
                &origin,
            )
            .unwrap();
            assert!(!gateway
                .search_resources
                .cooldowns()
                .unwrap()
                .reserve(&target, 0)
                .await
                .unwrap());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
        use futures_util::FutureExt;
        let result = result.catch_unwind().await;
        server.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
    #[tokio::test]
    async fn explicit_source_spacing_prevents_repeat_dispatch_and_survives_reopen() {
        let (origin, calls, task) = server("success").await;
        let path = std::env::temp_dir()
            .join(format!(
                "llmgw-owned-source-spacing-{}",
                uuid::Uuid::new_v4()
            ))
            .join("owned.db");
        eprintln!("OWNED_SOURCE_SPACING_DB={}", path.display());
        let result = std::panic::AssertUnwindSafe(async {
            let cfg:SearchConfig=serde_json::from_value(serde_json::json!({"backend":"free_aggregate","timeout_ms":1000,"source_min_interval_ms":60000})).unwrap();
            let db = crate::db::Db::connect_path(&path).await.unwrap();
            let app = crate::config::AppConfig {
                search: cfg.clone(),
                ..Default::default()
            };
            let state = crate::proxy::server::GatewayState::new(db.clone(), app);
            let q = SearchQuery {
                text: "owned explicit interval".into(),
                max_results: 5,
            };
            for n in 0..2 {
                let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&cfg) else {
                    panic!("normal factory required")
                };
                backend.bing_base_url = origin.clone();
                backend.ddg_base_url = origin.clone();
                let outcome = execute_selected_with_resources(
                    free_client().unwrap(),
                    &cfg,
                    None,
                    &q,
                    Some(SelectedBackend::Aggregate(backend)),
                    &state.search_resources,
                )
                .await;
                if n == 0 {
                    assert!(outcome.error.is_none());
                    assert_eq!(calls.lock().unwrap().len(), 2);
                } else {
                    assert_eq!(calls.lock().unwrap().len(),2,"an explicitly configured interval must reject before either repeated source request");
                    assert!(outcome.error.is_some());
                }
            }
            drop(state);
            db.pool().close().await;
            let reopened = crate::db::Db::connect_path(&path).await.unwrap();
            let old_cfg = SearchConfig {
                backend: SearchBackendKind::FreeAggregate,
                timeout_ms: 1000,
                ..Default::default()
            };
            let reset = crate::proxy::server::GatewayState::new(
                reopened.clone(),
                crate::config::AppConfig {
                    search: old_cfg.clone(),
                    ..Default::default()
                },
            );
            let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&old_cfg) else {
                panic!("normal factory required")
            };
            backend.bing_base_url = origin.clone();
            backend.ddg_base_url = origin.clone();
            let outcome = execute_selected_with_resources(
                free_client().unwrap(),
                &old_cfg,
                None,
                &q,
                Some(SelectedBackend::Aggregate(backend)),
                &reset.search_resources,
            )
            .await;
            assert!(outcome.error.is_some());
            assert_eq!(
                calls.lock().unwrap().len(),
                2,
                "setting0/reopen cannot erase committed source wait"
            );
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM meta WHERE key LIKE 'search.free_cooldown.v1.%'",
            )
            .fetch_one(reopened.pool())
            .await
            .unwrap();
            assert_eq!(count, 2);
            let default_db = crate::db::Db::connect_in_memory().await.unwrap();
            let defaults = crate::proxy::server::GatewayState::new(
                default_db.clone(),
                crate::config::AppConfig {
                    search: old_cfg.clone(),
                    ..Default::default()
                },
            );
            for _ in 0..2 {
                let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&old_cfg) else {
                    panic!("normal factory required")
                };
                backend.bing_base_url = origin.clone();
                backend.ddg_base_url = origin.clone();
                let outcome = execute_selected_with_resources(
                    free_client().unwrap(),
                    &old_cfg,
                    None,
                    &q,
                    Some(SelectedBackend::Aggregate(backend)),
                    &defaults.search_resources,
                )
                .await;
                assert!(outcome.error.is_none());
            }
            assert_eq!(
                calls.lock().unwrap().len(),
                6,
                "default0 must preserve both fresh dual-source calls"
            );
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM meta WHERE key LIKE 'search.free_cooldown.v1.%'",
            )
            .fetch_one(default_db.pool())
            .await
            .unwrap();
            assert_eq!(count, 0);
            let failed_db = crate::db::Db::connect_in_memory().await.unwrap();
            sqlx::raw_sql("CREATE TRIGGER owned_interval_failure BEFORE INSERT ON meta BEGIN SELECT RAISE(ABORT,'owned interval failure'); END;").execute(failed_db.pool()).await.unwrap();
            let failed = crate::proxy::server::GatewayState::new(
                failed_db,
                crate::config::AppConfig {
                    search: cfg.clone(),
                    ..Default::default()
                },
            );
            let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&cfg) else {
                panic!("normal factory required")
            };
            backend.bing_base_url = origin.clone();
            backend.ddg_base_url = origin.clone();
            let outcome = execute_selected_with_resources(
                free_client().unwrap(),
                &cfg,
                None,
                &q,
                Some(SelectedBackend::Aggregate(backend)),
                &failed.search_resources,
            )
            .await;
            assert!(outcome.error.is_some());
            assert_eq!(
                calls.lock().unwrap().len(),
                6,
                "failed persistence must not start either physical source HTTP request"
            );
        });
        use futures_util::FutureExt;
        let result = result.catch_unwind().await;
        task.abort();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[tokio::test]
    async fn actual_resolver_preserves_partial_deadline_sources_and_free_client_boundaries() {
        let cfg = SearchConfig {
            backend: SearchBackendKind::FreeAggregate,
            timeout_ms: 500,
            ..Default::default()
        };
        assert_eq!(query_limit(&cfg), 256);
        assert_eq!(query_limit(&SearchConfig::default()), 400);
        for mode in ["partial", "deadline", "redirect", "all-fail"] {
            let (origin, calls, task) = server(mode).await;
            let Some(SelectedBackend::Aggregate(mut backend)) = backend_of(&cfg) else {
                panic!("explicit free config did not resolve to aggregate")
            };
            assert_eq!(backend.bing_base_url, "https://cn.bing.com");
            assert_eq!(backend.ddg_base_url, "https://lite.duckduckgo.com");
            backend.bing_base_url = origin.clone();
            backend.ddg_base_url = origin;
            let outcome = tokio::time::timeout(
                Duration::from_millis(850),
                execute_selected(
                    free_client().unwrap(),
                    &cfg,
                    Some("fixture-unused-legacy-key"),
                    &SearchQuery {
                        text: "fixed offline query".into(),
                        max_results: 5,
                    },
                    Some(SelectedBackend::Aggregate(backend)),
                ),
            )
            .await;
            task.abort();
            let outcome =
                outcome.expect("finished source must survive authoritative aggregate deadline");
            assert_eq!(outcome.backend, SearchBackendKind::FreeAggregate);
            assert_eq!(calls.lock().unwrap().len(), 2);
            if mode == "all-fail" {
                assert!(outcome.error.is_some());
                assert_eq!(outcome.hits, 0);
            } else {
                assert!(outcome.error.is_none());
                assert_eq!(outcome.hits, 1);
                assert_eq!(outcome.result_sources.len(), 1);
                assert_eq!(outcome.failed_sources.len(), 1);
                assert_eq!(
                    outcome.result_sources[0],
                    if mode == "redirect" {
                        "duckduckgo"
                    } else {
                        "bing_rss"
                    }
                );
                assert_eq!(
                    outcome.failed_sources[0],
                    if mode == "redirect" {
                        "bing_rss"
                    } else {
                        "duckduckgo"
                    }
                );
            }
        }
    }

    #[tokio::test]
    async fn actual_resolver_keeps_paid_401_terminal_without_free_fallback() {
        for kind in [SearchBackendKind::Tavily, SearchBackendKind::Brave] {
            let cfg = SearchConfig {
                backend: kind,
                ..Default::default()
            };
            let Some(SelectedBackend::Single(resolved)) = backend_of(&cfg) else {
                panic!("legacy config changed resolver class")
            };
            assert_eq!(resolved.kind(), kind);
            let (origin, calls, task) = server("paid").await;
            let backend: Box<dyn SearchBackend> = if kind == SearchBackendKind::Tavily {
                Box::new(TavilyBackend { base_url: origin })
            } else {
                Box::new(BraveBackend { base_url: origin })
            };
            let outcome = execute_selected(
                free_client().unwrap(),
                &cfg,
                Some("fixture-paid-key"),
                &SearchQuery {
                    text: "fixed".into(),
                    max_results: 3,
                },
                Some(SelectedBackend::Single(backend)),
            )
            .await;
            task.abort();
            assert_eq!(outcome.backend, kind);
            assert_eq!(outcome.hits, 0);
            assert!(outcome.error.unwrap().contains("不会"));
            assert!(outcome.result_sources.is_empty());
            assert_eq!(
                calls.lock().unwrap().len(),
                1,
                "401 must not add a retry or free search request"
            );
        }
    }
}

#[cfg(test)]
#[path = "locale_tests.rs"]
mod locale_tests;

#[cfg(test)]
#[path = "stack_exchange_routing_tests.rs"]
mod stack_exchange_routing_tests;
