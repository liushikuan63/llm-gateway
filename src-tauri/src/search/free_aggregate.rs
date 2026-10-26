//! Explicitly selected, bounded free search aggregation. No provider fallback.
//! Source/deadline/entity invariants: [search maintenance](../../../docs/wiki/search.md).
use super::backend::{SearchError, SearchQuery, SearchResult};
use super::cooldown::Source;
use crate::config::{FreeSearchLocale, FreeSearchScope};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

pub const MAX_BODY_BYTES: usize = 256 * 1024;
pub const MAX_QUERY_CHARS: usize = 256;
pub const MAX_URL_BYTES: usize = 4096;
pub const MAX_RESULTS: usize = 10;

pub struct FreeAggregateBackend {
    pub bing_base_url: String,
    pub ddg_base_url: String,
    pub npm_base_url: String,
    pub hn_base_url: String,
    pub timeout: Duration,
}

#[derive(Debug)]
pub struct FreeAggregateReport {
    pub results: Vec<SearchResult>,
    pub result_sources: Vec<&'static str>,
    /// Fixed source codes only; no query, response body or credential in errors.
    pub failed_sources: Vec<&'static str>,
}

impl Default for FreeAggregateBackend {
    fn default() -> Self {
        Self {
            bing_base_url: "https://cn.bing.com".into(),
            ddg_base_url: "https://lite.duckduckgo.com".into(),
            npm_base_url: "https://registry.npmjs.org".into(),
            hn_base_url: "https://hn.algolia.com".into(),
            timeout: Duration::from_secs(5),
        }
    }
}

impl FreeAggregateBackend {
    pub async fn search(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
    ) -> Result<FreeAggregateReport, SearchError> {
        self.search_guarded(http, query, None).await
    }
    pub async fn search_guarded(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        cooldowns: Option<&Arc<super::cooldown::Cooldowns>>,
    ) -> Result<FreeAggregateReport, SearchError> {
        self.search_governed(http, query, cooldowns, 0).await
    }
    pub async fn search_governed(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        cooldowns: Option<&Arc<super::cooldown::Cooldowns>>,
        interval_ms: u32,
    ) -> Result<FreeAggregateReport, SearchError> {
        self.search_in_scope(http, query, cooldowns, interval_ms, FreeSearchScope::Web)
            .await
    }
    pub async fn search_in_scope(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        cooldowns: Option<&Arc<super::cooldown::Cooldowns>>,
        interval_ms: u32,
        scope: FreeSearchScope,
    ) -> Result<FreeAggregateReport, SearchError> {
        self.search_with_preferences(
            http,
            query,
            cooldowns,
            interval_ms,
            scope,
            FreeSearchLocale::SourceDefault,
        )
        .await
    }
    pub async fn search_with_preferences(
        &self,
        http: &reqwest::Client,
        query: &SearchQuery,
        cooldowns: Option<&Arc<super::cooldown::Cooldowns>>,
        interval_ms: u32,
        scope: FreeSearchScope,
        locale: FreeSearchLocale,
    ) -> Result<FreeAggregateReport, SearchError> {
        if scope == FreeSearchScope::StackOverflow {
            return Err(malformed("stack_overflow_protected_path_required"));
        }
        // Validate the paired web request before reserving or sending either source.
        if scope == FreeSearchScope::Web
            && bing_query(query, locale).chars().count() > MAX_QUERY_CHARS
        {
            return Err(malformed("locale-derived query exceeds free search limit"));
        }
        if (interval_ms > 0 || scope == FreeSearchScope::HackerNews) && cooldowns.is_none() {
            return Err(malformed("free source reservation storage unavailable"));
        }
        if query.text.trim().is_empty()
            || query.text.chars().count() > MAX_QUERY_CHARS
            || query.text.chars().any(char::is_control)
            || !(1..=MAX_RESULTS as u32).contains(&query.max_results)
        {
            return Err(malformed("invalid free search query or result limit"));
        }
        // One deadline, two futures, no spawned jobs: dropping the caller drops
        // both HTTP operations. Completed results survive the other source timeout.
        let timeout = self.timeout.min(Duration::from_secs(10));
        if timeout.is_zero() {
            return Err(SearchError::Timeout);
        }
        let deadline = tokio::time::Instant::now() + timeout;
        if scope == FreeSearchScope::HackerNews {
            if !super::hacker_news::technical_intent(&query.text) {
                return Err(malformed("HN scope requires an explicit technical query"));
            }
            let results = fetch(
                http,
                &self.hn_base_url,
                (Source::HackerNews, FreeSearchLocale::SourceDefault),
                query,
                deadline,
                cooldowns,
                interval_ms,
            )
            .await?;
            let result_sources = vec!["hacker_news"; results.len()];
            return Ok(FreeAggregateReport {
                results,
                result_sources,
                failed_sources: vec![],
            });
        }
        if scope == FreeSearchScope::NpmPackages {
            if !super::npm_packages::package_intent(&query.text) {
                return Err(malformed("npm scope requires an explicit package query (npm package, npm 包 or package.json)"));
            }
            let results = fetch(
                http,
                &self.npm_base_url,
                (Source::NpmRegistry, FreeSearchLocale::SourceDefault),
                query,
                deadline,
                cooldowns,
                interval_ms,
            )
            .await?;
            let result_sources = vec!["npm_registry"; results.len()];
            return Ok(FreeAggregateReport {
                results,
                result_sources,
                failed_sources: vec![],
            });
        }
        let bing = fetch(
            http,
            &self.bing_base_url,
            (Source::BingRss, locale),
            query,
            deadline,
            cooldowns,
            interval_ms,
        );
        let ddg = fetch(
            http,
            &self.ddg_base_url,
            (Source::Duckduckgo, locale),
            query,
            deadline,
            cooldowns,
            interval_ms,
        );
        let (bing, ddg) = tokio::join!(bing, ddg);
        let mut failed_sources = Vec::new();
        let mut failures = Vec::new();
        let mut results = Vec::new();
        let mut result_sources = Vec::new();
        let mut seen = HashSet::new();
        for (source, outcome) in [("bing_rss", bing), ("duckduckgo", ddg)] {
            match outcome {
                Ok(hits) => {
                    for item in hits {
                        if let Some(canonical) = canonical_url(&item.url) {
                            if seen.insert(canonical) && results.len() < query.max_results as usize
                            {
                                results.push(item);
                                result_sources.push(source);
                            }
                        }
                    }
                }
                Err(error) => {
                    failed_sources.push(source);
                    failures.push(format!("{source}: {error}"));
                }
            }
        }
        if results.is_empty() {
            return Err(SearchError::Malformed(format!(
                "all free search sources unavailable ({})",
                failures.join("; ")
            )));
        }
        Ok(FreeAggregateReport {
            results,
            result_sources,
            failed_sources,
        })
    }
}

fn bing_query(query: &SearchQuery, locale: FreeSearchLocale) -> String {
    match locale {
        FreeSearchLocale::UsEn => format!("{} language:en loc:US", query.text),
        FreeSearchLocale::GbEn => format!("{} language:en loc:GB", query.text),
        _ => query.text.clone(),
    }
}

fn malformed(message: &'static str) -> SearchError {
    SearchError::Malformed(message.into())
}

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

async fn fetch(
    http: &reqwest::Client,
    base: &str,
    (source, locale): (Source, FreeSearchLocale),
    query: &SearchQuery,
    deadline: tokio::time::Instant,
    cooldowns: Option<&Arc<super::cooldown::Cooldowns>>,
    interval_ms: u32,
) -> Result<Vec<SearchResult>, SearchError> {
    let operation = async {
        let target = if let Some(store) = cooldowns {
            let target = super::cooldown::Target::new(source, base)
                .map_err(|_| malformed("free source protection target invalid"))?;
            let admitted = if matches!(source, Source::HackerNews)
                && interval_ms < super::hacker_news::MIN_INTERVAL_MS
            {
                store
                    .reserve_service_policy(&target, super::hacker_news::MIN_INTERVAL_MS)
                    .await
            } else {
                store.reserve(&target, interval_ms).await
            };
            if !admitted.map_err(|_| malformed("free source protection admission unavailable"))? {
                return Err(malformed("free source has a recorded local wait"));
            }
            Some(target)
        } else {
            None
        };
        // Endpoint overrides exist for owned loopback fixtures and root-managed
        // outbound configuration. They are never inferred from a response URL.
        let mut endpoint =
            reqwest::Url::parse(base).map_err(|_| malformed("invalid free search endpoint"))?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(malformed("invalid free search endpoint"));
        }
        endpoint.set_path(match source {
            Source::BingRss => "/search",
            Source::Duckduckgo => "/lite/",
            Source::NpmRegistry => "/-/v1/search",
            Source::HackerNews => "/api/v1/search",
        });
        let bing_text = bing_query(query, locale);
        let mut ddg_form = vec![("q", query.text.as_str())];
        if let Some(kl) = match locale {
            FreeSearchLocale::SourceDefault => None,
            FreeSearchLocale::CnZh => Some("cn-zh"),
            FreeSearchLocale::UsEn => Some("us-en"),
            FreeSearchLocale::GbEn => Some("uk-en"),
        } {
            ddg_form.push(("kl", kl));
        }
        let request = match source {
            Source::BingRss => http
                .get(endpoint)
                .query(&[
                    ("q", bing_text.as_str()),
                    ("format", "rss"),
                    ("mkt", "zh-CN"),
                ])
                .header(
                    reqwest::header::ACCEPT,
                    "application/rss+xml, application/xml;q=0.9",
                ),
            Source::Duckduckgo => http
                .post(endpoint)
                .form(&ddg_form)
                .header(reqwest::header::ACCEPT, "text/html"),
            Source::NpmRegistry => http
                .get(endpoint)
                .query(&[
                    ("text", query.text.clone()),
                    ("size", query.max_results.to_string()),
                ])
                .header(reqwest::header::ACCEPT, "application/json"),
            Source::HackerNews => http
                .get(endpoint)
                .query(&[
                    ("query", query.text.clone()),
                    ("tags", "story".to_owned()),
                    ("hitsPerPage", query.max_results.to_string()),
                ])
                .header(reqwest::header::ACCEPT, "application/json"),
        };
        let mut response = request
            .header(reqwest::header::USER_AGENT, UA)
            .send()
            .await
            .map_err(|_| SearchError::Transport("free search request failed".into()))?;
        if matches!(response.status().as_u16(), 429 | 503) {
            let observed = chrono::Utc::now().timestamp_millis();
            if let Some(until) = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok())
                .and_then(|h| super::cooldown::retry_after(h, observed))
            {
                if let (Some(store), Some(target)) = (cooldowns, target) {
                    store
                        .remember(target, until, observed)
                        .await
                        .map_err(|_| malformed("free source protection state was not saved"))?;
                }
            }
        }
        // Never retry an authentication rejection, and never follow it by
        // selecting a third provider. Both explicitly selected free sources
        // already started concurrently; this is not a credential fallback.
        match response.status().as_u16() {
            401 | 403 => return Err(SearchError::CredentialRejected),
            200..=299 => {}
            _ => return Err(malformed("free search HTTP failure")),
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY_BYTES as u64)
        {
            return Err(malformed("free search response too large"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| malformed("free search body unavailable"))?
        {
            if chunk.len() > MAX_BODY_BYTES.saturating_sub(bytes.len()) {
                return Err(malformed("free search response too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = std::str::from_utf8(&bytes)
            .map_err(|_| malformed("free search response is not UTF-8"))?;
        if matches!(source, Source::BingRss) {
            return parse_bing_rss(body, query.max_results as usize);
        }
        if matches!(source, Source::NpmRegistry) {
            return super::npm_packages::parse(body, query.max_results as usize);
        }
        if matches!(source, Source::HackerNews) {
            return super::hacker_news::parse(body, query.max_results as usize);
        }
        // DDG HTTP 200 challenge pages often contain navigation links. Only
        // existing result-link anchors can supply hits. Reject known challenge
        // controls, not ordinary results discussing CAPTCHA or those controls.
        if ddg_challenge(body) {
            return Err(malformed("free search challenge page"));
        }
        let mut seen = HashSet::new();
        let hits = super::parse::duckduckgo_lite(body, MAX_RESULTS)
            .into_iter()
            .filter_map(|item| bounded_hit(item, &mut seen))
            .take(query.max_results as usize)
            .collect::<Vec<_>>();
        if hits.is_empty() {
            return Err(malformed("free search returned no usable results"));
        }
        Ok(hits)
    };
    tokio::time::timeout_at(deadline, operation)
        .await
        .map_err(|_| SearchError::Timeout)?
}

/// A bounded signature check, not a general HTML parser. Attribute names and
/// control tokens matter; quoted/escaped examples and raw-text elements do not.
fn ddg_challenge(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let mut remaining = lower.as_str();
    while let Some((_, tail)) = remaining.split_once('<') {
        if let Some(comment) = tail.strip_prefix("!--") {
            remaining = comment.split_once("-->").map_or("", |(_, rest)| rest);
            continue;
        }
        let Some((tag, content)) = tail.split_once('>') else {
            break;
        };
        remaining = content;
        let end = tag
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(tag.len());
        let name = &tag[..end];
        if let Some(close) = match name {
            "script" => Some("</script"),
            "style" => Some("</style"),
            _ => None,
        } {
            remaining = content.split_once(close).map_or("", |(_, rest)| rest);
            continue;
        }
        if matches!(name, "title" | "h1" | "h2")
            && content
                .split('<')
                .next()
                .unwrap_or("")
                .split_ascii_whitespace()
                .eq(["verify", "you", "are", "human"])
        {
            return true;
        }
        if !matches!(
            name,
            "form" | "div" | "section" | "main" | "input" | "iframe"
        ) {
            continue;
        }
        for attr in ["id", "class"] {
            if tag_attribute(tag, attr).is_some_and(|value| {
                value.split_ascii_whitespace().any(|token| {
                    matches!(
                        token,
                        "anomaly-modal"
                            | "captcha"
                            | "captcha-container"
                            | "captcha-form"
                            | "challenge-form"
                            | "g-recaptcha"
                            | "h-captcha"
                    ) || token.starts_with("anomaly-modal__")
                })
            }) {
                return true;
            }
        }
        if name == "form"
            && tag_attribute(tag, "action")
                .and_then(|value| {
                    reqwest::Url::parse("https://duckduckgo.com/")
                        .ok()?
                        .join(value)
                        .ok()
                })
                .is_some_and(|url| url.path() == "/anomaly.js")
        {
            return true;
        }
    }
    false
}

fn tag_attribute<'a>(tag: &'a str, wanted: &str) -> Option<&'a str> {
    let mut rest = tag;
    while !rest.is_empty() {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        let end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let (name, tail) = rest.split_at(end);
        rest = tail.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        rest = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let (value, tail) =
            if let Some(quote) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) {
                rest[1..].split_once(quote)?
            } else {
                let end = rest
                    .find(|c: char| c.is_ascii_whitespace())
                    .unwrap_or(rest.len());
                rest.split_at(end)
            };
        rest = tail;
        if name == wanted {
            return Some(value);
        }
    }
    None
}

fn canonical_url(raw: &str) -> Option<String> {
    if raw.len() > MAX_URL_BYTES || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let mut url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    // Only the fragment is excluded. Query order/values and percent escapes
    // can carry application semantics; neither is rewritten for deduplication.
    url.set_fragment(None);
    Some(url.into())
}

fn bounded_hit(mut item: SearchResult, seen: &mut HashSet<String>) -> Option<SearchResult> {
    let canonical = canonical_url(&item.url)?;
    if item.title.trim().is_empty() || !seen.insert(canonical) {
        return None;
    }
    item.title = item
        .title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(256)
        .collect();
    item.snippet = item
        .snippet
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(1200)
        .collect();
    item.score = 0.0; // Source rank is not confidence or a comparable relevance score.
    Some(item)
}

fn plain_html(raw: &str) -> String {
    let decoded = super::parse::decode_entities(raw);
    let mut text = String::new();
    let mut in_tag = false;
    for ch in decoded.chars() {
        match ch {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    text
}

fn xml_text(raw: &str) -> Result<String, SearchError> {
    let mut rest = raw;
    let mut decoded = String::with_capacity(raw.len());
    while let Some(at) = rest.find('&') {
        decoded.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let end = tail
            .find(';')
            .ok_or_else(|| malformed("unclosed XML entity"))?;
        let name = &tail[..end];
        let ch = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "apos" => '\'',
            "quot" => '"',
            _ => {
                let number = name
                    .strip_prefix('#')
                    .ok_or_else(|| malformed("unsupported XML entity"))?;
                if number.len() > 8 {
                    return Err(malformed("XML entity too long"));
                }
                let value = if let Some(hex) = number.strip_prefix('x') {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    number.parse::<u32>().ok()
                };
                let ch = value
                    .and_then(char::from_u32)
                    .ok_or_else(|| malformed("invalid XML entity"))?;
                if ch.is_control() && !matches!(ch, '\t' | '\n' | '\r') {
                    return Err(malformed("XML control entity is unsupported"));
                }
                ch
            }
        };
        decoded.push(ch);
        rest = &tail[end + 1..];
    }
    decoded.push_str(rest);
    Ok(decoded)
}

/// A restricted, size-bounded RSS XML reader. No DTD, external entities or
/// entity expansion; comments/CDATA never become structure. XML nesting is
/// checked, including after the requested output limit has been reached.
pub fn parse_bing_rss(xml: &str, limit: usize) -> Result<Vec<SearchResult>, SearchError> {
    if xml.len() > MAX_BODY_BYTES {
        return Err(malformed("RSS response too large"));
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stack: Vec<&str> = Vec::new();
    let mut cursor = 0;
    let mut channel_seen = false;
    let mut root_seen = false;
    let mut item = SearchResult {
        title: String::new(),
        url: String::new(),
        snippet: String::new(),
        score: 0.0,
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    while cursor < xml.len() {
        let rest = &xml[cursor..];
        if rest.starts_with("<!--") {
            let end = rest
                .find("-->")
                .ok_or_else(|| malformed("unclosed RSS comment"))?;
            cursor += end + 3;
            continue;
        }
        if rest.starts_with("<?") {
            let end = rest
                .find("?>")
                .ok_or_else(|| malformed("unclosed RSS declaration"))?;
            cursor += end + 2;
            continue;
        }
        let (text, consumed, cdata) = if let Some(rest) = rest.strip_prefix("<![CDATA[") {
            let end = rest
                .find("]]>")
                .ok_or_else(|| malformed("unclosed RSS CDATA"))?;
            (&rest[..end], end + 12, true)
        } else if !rest.starts_with('<') {
            let end = rest.find('<').unwrap_or(rest.len());
            (&rest[..end], end, false)
        } else {
            ("", 0, false)
        };
        if consumed > 0 {
            let value = if cdata {
                text.to_owned()
            } else {
                xml_text(text)?
            };
            if stack.len() == 4 && stack[..3] == ["rss", "channel", "item"] {
                match stack[3] {
                    "title" => item.title.push_str(&value),
                    "link" => item.url.push_str(&value),
                    "description" => item.snippet.push_str(&value),
                    _ => {}
                }
            } else if stack.is_empty() && !text.trim_matches('\u{feff}').trim().is_empty() {
                return Err(malformed("text outside RSS document"));
            }
            cursor += consumed;
            continue;
        }
        let end = rest
            .find('>')
            .ok_or_else(|| malformed("unclosed RSS tag"))?;
        let tag = rest[1..end].trim();
        if tag.starts_with('!') {
            return Err(malformed("RSS declarations are unsupported"));
        }
        if let Some(closed) = tag.strip_prefix('/') {
            if stack.pop() != Some(closed.trim()) {
                return Err(malformed("RSS nesting mismatch"));
            }
            if closed == "item" && stack == ["rss", "channel"] {
                item.url = item.url.trim().to_owned();
                item.snippet = plain_html(&item.snippet);
                if let Some(hit) = bounded_hit(item.clone(), &mut seen) {
                    if out.len() < limit.min(MAX_RESULTS) {
                        out.push(hit);
                    }
                }
            }
        } else {
            let self_closing = tag.ends_with('/');
            let name = tag
                .trim_end_matches('/')
                .split_whitespace()
                .next()
                .ok_or_else(|| malformed("empty RSS tag"))?;
            if !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
            {
                return Err(malformed("invalid RSS tag"));
            }
            if stack.is_empty() {
                if root_seen || name != "rss" {
                    return Err(malformed("not an RSS document"));
                }
                root_seen = true;
            }
            if name == "channel" && stack == ["rss"] {
                channel_seen = true;
            }
            if name == "item" && stack == ["rss", "channel"] {
                item = SearchResult {
                    title: String::new(),
                    url: String::new(),
                    snippet: String::new(),
                    score: 0.0,
                };
            }
            if !self_closing {
                stack.push(name);
            }
            if stack.len() > 32 {
                return Err(malformed("RSS nesting too deep"));
            }
        }
        cursor += end + 1;
    }
    if !stack.is_empty() || !channel_seen || out.is_empty() {
        return Err(malformed("RSS returned no usable results or malformed XML"));
    }
    Ok(out)
}
