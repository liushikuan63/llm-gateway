//! Search backends.
///
/// Four adapters behind one trait. They differ only in endpoint, credential and
/// response shape; everything above them (normalisation, injection, failure
/// handling) is shared.
///
/// **Credentials stay out of the config file.** Tavily and Brave need a key; it
/// lives in SQLite `app_secrets` as AES-256-GCM ciphertext and is passed in per
/// call. `None` means "no credential configured", which for those two backends is
/// a hard error rather than a silent retry.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::SearchBackendKind;

/// One normalised search hit. Backends that return scores (Tavily) map them;
/// the others get a neutral `0.0` so the field means "backend said so", not
/// "we computed it".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub score: f32,
}

/// Query normalisation happens once, in `crate::search::query_from`, so each
/// backend can assume a single-line, length-bounded string.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchQuery {
    pub text: String,
    pub max_results: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("后端返回 401/403，凭据无效")]
    CredentialRejected,
    #[error("网络错误：{0}")]
    Transport(String),
    #[error("后端响应异常：{0}")]
    Malformed(String),
    #[error("缺少该后端必需的凭据")]
    MissingCredential,
    #[error("请求超时")]
    Timeout,
}

#[async_trait]
pub trait SearchBackend: Send + Sync {
    fn kind(&self) -> SearchBackendKind;
    async fn search(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        key: Option<&str>,
    ) -> Result<Vec<SearchResult>, SearchError>;
}

fn require_key(key: Option<&str>) -> Result<&str, SearchError> {
    match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => Ok(k),
        None => Err(SearchError::MissingCredential),
    }
}

/// Map an HTTP status onto the error type. 401/403 are singled out because the
/// caller must **not** silently fall back to a keyless backend after them —
/// that would turn a fixable misconfiguration into silent, repeated bad
/// traffic to someone else's service.
fn status_error(status: reqwest::StatusCode, body: &str) -> SearchError {
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return SearchError::CredentialRejected;
    }
    SearchError::Malformed(format!("HTTP {} {}", status.as_u16(), body.trim()))
}

pub struct TavilyBackend {
    /// 默认官方地址；可改指向企业代理或自建网关。
    pub base_url: String,
}

impl Default for TavilyBackend {
    fn default() -> Self {
        Self {
            base_url: "https://api.tavily.com".into(),
        }
    }
}

#[async_trait]
impl SearchBackend for TavilyBackend {
    fn kind(&self) -> SearchBackendKind {
        SearchBackendKind::Tavily
    }

    async fn search(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        key: Option<&str>,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let key = require_key(key)?;
        let response = http
            .post(join_path(&self.base_url, "/search"))
            .json(&serde_json::json!({
                "api_key": key,
                "query": query.text,
                "max_results": query.max_results,
                "search_depth": "basic",
                "include_answer": false,
            }))
            .send()
            .await
            .map_err(map_transport)?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(status_error(status, &body));
        }
        let json: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| SearchError::Malformed(e.to_string()))?;
        let results = json
            .get("results")
            .and_then(|v| v.as_array())
            .ok_or_else(|| SearchError::Malformed("响应缺少 results 数组".into()))?;
        Ok(results
            .iter()
            .take(query.max_results as usize)
            .map(|item| SearchResult {
                title: string_field(item, "title"),
                url: string_field(item, "url"),
                snippet: string_field(item, "content"),
                score: item.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            })
            .filter(|r| !r.url.is_empty())
            .collect())
    }
}

pub struct BraveBackend {
    pub base_url: String,
}

impl Default for BraveBackend {
    fn default() -> Self {
        Self {
            base_url: "https://api.search.brave.com".into(),
        }
    }
}

#[async_trait]
impl SearchBackend for BraveBackend {
    fn kind(&self) -> SearchBackendKind {
        SearchBackendKind::Brave
    }

    async fn search(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        key: Option<&str>,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let key = require_key(key)?;
        let response = http
            .get(join_path(&self.base_url, "/res/v1/web/search"))
            .header("X-Subscription-Token", key)
            .header("Accept", "application/json")
            .query(&[("q", query.text.as_str()), ("count", &query.max_results.to_string())])
            .send()
            .await
            .map_err(map_transport)?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(status_error(status, &body));
        }
        let json: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| SearchError::Malformed(e.to_string()))?;
        let results = json
            .get("web")
            .and_then(|v| v.get("results"))
            .and_then(|v| v.as_array())
            .ok_or_else(|| SearchError::Malformed("响应缺少 web.results 数组".into()))?;
        Ok(results
            .iter()
            .take(query.max_results as usize)
            .map(|item| SearchResult {
                title: string_field(item, "title"),
                url: string_field(item, "url"),
                snippet: string_field(item, "description"),
                // Brave 不返回相关性分。填 0 而不是编一个——界面显示的
                // 「后端未给分」比一个假分数诚实。
                score: 0.0,
            })
            .filter(|r| !r.url.is_empty())
            .collect())
    }
}

pub struct SearXngBackend {
    pub base_url: String,
}

#[async_trait]
impl SearchBackend for SearXngBackend {
    fn kind(&self) -> SearchBackendKind {
        SearchBackendKind::SearXng
    }

    async fn search(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        _key: Option<&str>,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let root = self.base_url.trim().trim_end_matches('/');
        // 地址由用户填写，保存期已校验过协议；这里再挡一次是因为它会被拼进 URL。
        if !(root.starts_with("http://") || root.starts_with("https://")) {
            return Err(SearchError::Malformed("SearXNG 地址必须是 http/https".into()));
        }
        let response = http
            .get(format!("{root}/search"))
            .query(&[("q", query.text.as_str()), ("format", "json")])
            .send()
            .await
            .map_err(map_transport)?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(status_error(status, &body));
        }
        let json: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| SearchError::Malformed(e.to_string()))?;
        let results = json
            .get("results")
            .and_then(|v| v.as_array())
            .ok_or_else(|| SearchError::Malformed("响应缺少 results 数组".into()))?;
        Ok(results
            .iter()
            .take(query.max_results as usize)
            .map(|item| SearchResult {
                title: string_field(item, "title"),
                url: string_field(item, "url"),
                snippet: string_field(item, "content"),
                score: item
                    .get("score")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32,
            })
            .filter(|r| !r.url.is_empty())
            .collect())
    }
}

/// Keyless fallback. Availability is not guaranteed and is not claimed anywhere
/// in the UI — see `docs/智能路由与本地模型设计方案.md` §6.5.
pub struct DuckDuckGoBackend {
    pub base_url: String,
}

impl Default for DuckDuckGoBackend {
    fn default() -> Self {
        Self {
            base_url: "https://lite.duckduckgo.com".into(),
        }
    }
}

#[async_trait]
impl SearchBackend for DuckDuckGoBackend {
    fn kind(&self) -> SearchBackendKind {
        SearchBackendKind::DuckDuckGo
    }

    async fn search(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        _key: Option<&str>,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let response = http
            .post(join_path(&self.base_url, "/lite/"))
            .form(&[("q", query.text.as_str())])
            .header("Accept", "text/html")
            .send()
            .await
            .map_err(map_transport)?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(status_error(status, &body));
        }
        Ok(crate::search::parse::duckduckgo_lite(&body, query.max_results as usize))
    }
}

/// 必应中国站。**免 Key**，本机实测 248 ms 响应、能解析出真实结果，
/// 是本机唯一真正可用的免 Key 后端（DuckDuckGo 在本机全部超时）。
///
/// 抓的是 SERP 而非官方 API：官方 Bing Search API 已退役且需要凭据。
/// 这是页面抓取，请遵守目标站点的使用条款；默认频率低、单次条数少。
pub struct BingCnBackend {
    pub base_url: String,
}

impl Default for BingCnBackend {
    fn default() -> Self {
        Self {
            base_url: "https://cn.bing.com".into(),
        }
    }
}

#[async_trait]
impl SearchBackend for BingCnBackend {
    fn kind(&self) -> SearchBackendKind {
        SearchBackendKind::BingCn
    }

    async fn search(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        _key: Option<&str>,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let url = format!("{}/search", self.base_url.trim().trim_end_matches('/'));
        let response = http
            .get(&url)
            .query(&[("q", query.text.as_str()), ("setlang", "zh-CN")])
            // 不带 UA 会被当成爬虫直接挡掉（实测返回 1488 字节的空壳）。
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
            )
            .header(reqwest::header::ACCEPT, "text/html")
            .send()
            .await
            .map_err(map_transport)?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(status_error(status, &body));
        }
        let results = crate::search::parse::bing_cn(&body, query.max_results as usize);
        if results.is_empty() {
            // 空结果和成功是两回事：静默返回空会让上层以为「搜过了、确实没有」，
            // 而实际是页面结构变了或被反爬拦了。
            return Err(SearchError::Malformed(
                "必应返回了页面但没解析出任何结果（结构可能已变更，或触发了反爬）".into(),
            ));
        }
        Ok(results)
    }
}

fn string_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned()
}

fn map_transport(error: reqwest::Error) -> SearchError {
    if error.is_timeout() {
        SearchError::Timeout
    } else {
        SearchError::Transport(error.to_string())
    }
}

/// 拼接端点与路径。协议必须在 `http`/`https` 之内——这些根地址里 SearXNG 来自
/// 用户输入，其余来自配置，保存期校验过但拼接点仍然再挡一次。
fn join_path(base_url: &str, path: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    debug_assert!(
        base.starts_with("http://") || base.starts_with("https://"),
        "搜索后端根地址必须是 http/https：{base}"
    );
    format!("{base}{path}")
}