//! Fixed-purpose MDN JSON/stateless transport. No generic MCP tool execution.
//! Only explicit MdnMcp selection reaches this producer; it never receives a
//! caller's client, key, cookies, proxy or destination. Runtime service support
//! remains unverified: unsupported SSE/session/version fails without fallback.
use super::{
    cooldown::{self, Cooldowns, Target},
    executor::SearchOutcome,
    mdn_mcp_contract::{Contract, Error as ContractError, RequestDescription, MAX_REPLY_BYTES},
    mdn_reference::MdnReference,
    resources::{SearchResources, SearchReuse},
    SearchQuery,
};
use crate::config::{SearchBackendKind, SearchConfig};
use reqwest::{header, Client};
use std::{sync::Arc, time::Duration};

// No production endpoint override. Unit tests must explicitly install an owned
// loopback endpoint; an omitted test binding is rejected before any HTTP.
#[cfg(test)]
tokio::task_local! { pub(super) static OWNED_ENDPOINT: String; }

#[derive(Clone, Copy)]
enum Stage {
    Preflight,
    Initialize,
    Initialized,
    Search,
}
impl Stage {
    fn code(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Initialize => "initialize",
            Self::Initialized => "initialized",
            Self::Search => "search",
        }
    }
}
#[derive(Clone, Copy)]
enum Failure {
    Configuration,
    Storage,
    Budget,
    Wait,
    Query,
    Client,
    #[cfg(test)]
    TestTarget,
    Network,
    Timeout,
    Http(u16),
    Headers,
    Body,
    TooLarge,
    Contract(ContractError),
}
impl Failure {
    fn code(self) -> String {
        match self {
            Self::Configuration => "configuration_changed",
            Self::Storage => "storage_unavailable",
            Self::Budget => "budget_contract_unverified",
            Self::Wait => "source_wait",
            Self::Query => "query_invalid",
            Self::Client => "client_unavailable",
            #[cfg(test)]
            Self::TestTarget => "owned_test_target_required",
            Self::Network => "transport_unknown",
            Self::Timeout => "timeout_remote_result_unknown",
            Self::Headers => "headers_unsupported",
            Self::Body => "body_unavailable",
            Self::TooLarge => "body_too_large",
            Self::Http(status) => return format!("http_{status}"),
            Self::Contract(error) => return format!("contract_{error}"),
        }
        .to_owned()
    }
}

fn endpoint() -> Result<String, Failure> {
    #[cfg(test)]
    {
        let value = OWNED_ENDPOINT
            .try_with(Clone::clone)
            .map_err(|_| Failure::TestTarget)?;
        let url = reqwest::Url::parse(&value).map_err(|_| Failure::TestTarget)?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || !url.port().is_some_and(|port| port > 0)
            || url.path() != "/"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Failure::TestTarget);
        }
        Ok(value)
    }
    #[cfg(not(test))]
    {
        Ok(super::mdn_mcp_contract::ENDPOINT.to_owned())
    }
}
fn client() -> Result<Client, Failure> {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .referer(false)
        .https_only(!cfg!(test))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| Failure::Client)
}
fn json_content_type(headers: &header::HeaderMap) -> bool {
    if headers.get_all(header::CONTENT_TYPE).iter().count() != 1 {
        return false;
    }
    let Some(value) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let parts: Vec<_> = value.split(';').map(str::trim).collect();
    parts[0].eq_ignore_ascii_case("application/json")
        && (parts.len() == 1
            || (parts.len() == 2
                && (parts[1].eq_ignore_ascii_case("charset=utf-8")
                    || parts[1].eq_ignore_ascii_case("charset=\"utf-8\""))))
}

struct PostContext<'a> {
    cfg: &'a SearchConfig,
    resources: &'a SearchResources,
    cooldowns: &'a Arc<Cooldowns>,
    target: &'a Target,
    endpoint: &'a str,
    client: &'a Client,
}
impl PostContext<'_> {
    async fn post(
        &self,
        description: RequestDescription,
        stage: Stage,
    ) -> Result<(Vec<u8>, bool), Failure> {
        if !self.resources.captures_current(self.cfg) {
            return Err(Failure::Configuration);
        }
        // Re-read acknowledged service wait before each stage. A different
        // in-flight transaction may have received Retry-After since admission.
        if !self
            .cooldowns
            .allowed(self.target)
            .await
            .map_err(|_| Failure::Storage)?
        {
            return Err(Failure::Wait);
        }
        if !self.resources.captures_current(self.cfg) {
            return Err(Failure::Configuration);
        }
        let mut request = self.client.post(self.endpoint);
        for (name, value) in description.headers() {
            request = request.header(*name, *value);
        }
        let mut response = request.json(description.body()).send().await.map_err(|e| {
            if e.is_timeout() {
                Failure::Timeout
            } else {
                Failure::Network
            }
        })?;
        // Persist a valid acknowledged wait before reading a potentially stalled
        // body. This is source policy metadata, never text/query or entitlement.
        let observed = chrono::Utc::now().timestamp_millis();
        let mut wait = false;
        if let Some(until) = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| cooldown::retry_after(h, observed))
        {
            if until > observed {
                self.cooldowns
                    .remember(self.target.clone(), until, observed)
                    .await
                    .map_err(|_| Failure::Storage)?;
                wait = true;
            }
        }
        let expected = if matches!(stage, Stage::Initialized) {
            202
        } else {
            200
        };
        if response.status().as_u16() != expected {
            return Err(Failure::Http(response.status().as_u16()));
        }
        if response.headers().contains_key("mcp-session-id") {
            return Err(Failure::Headers);
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
        {
            return Err(Failure::Headers);
        }
        if !matches!(stage, Stage::Initialized) && !json_content_type(response.headers()) {
            return Err(Failure::Headers);
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_REPLY_BYTES as u64)
        {
            return Err(Failure::TooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Body)? {
            if chunk.len() > MAX_REPLY_BYTES.saturating_sub(bytes.len()) {
                return Err(Failure::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, wait))
    }
}

pub(crate) async fn execute(
    cfg: &SearchConfig,
    query: &SearchQuery,
    resources: &SearchResources,
) -> SearchOutcome {
    let mut stage = Stage::Preflight;
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(cfg.timeout_ms.clamp(500, 60_000)).min(Duration::from_secs(10));
    let operation = async {
        if cfg.backend != SearchBackendKind::MdnMcp || !resources.captures_current(cfg) {
            return Err(Failure::Configuration);
        }
        if crate::budget::holds::active() {
            return Err(Failure::Budget);
        }
        let mut contract =
            Contract::new(&serde_json::json!({"query":query.text})).map_err(|_| Failure::Query)?;
        let endpoint = endpoint()?;
        let client = client()?;
        let cooldowns = resources.cooldowns().ok_or(Failure::Storage)?;
        // Key the owned test profile to the same fixed logical service identity;
        // production never accepts a URL from configuration or external text.
        let target = Target::mdn_mcp();
        if !cooldowns
            .allowed(&target)
            .await
            .map_err(|_| Failure::Storage)?
        {
            return Err(Failure::Wait);
        }
        let local = Target::mdn_local_transaction();
        if !cooldowns
            .reserve(&local, cfg.source_min_interval_ms)
            .await
            .map_err(|_| Failure::Storage)?
        {
            return Err(Failure::Wait);
        }
        let post = PostContext {
            cfg,
            resources,
            cooldowns: &cooldowns,
            target: &target,
            endpoint: &endpoint,
            client: &client,
        };
        stage = Stage::Initialize;
        let (body, wait) = post
            .post(
                contract.initialize_request().map_err(Failure::Contract)?,
                stage,
            )
            .await?;
        contract
            .accept_initialize_json(&body, None)
            .map_err(Failure::Contract)?;
        if wait {
            return Err(Failure::Wait);
        }
        stage = Stage::Initialized;
        let (body, wait) = post
            .post(
                contract
                    .initialized_notification()
                    .map_err(Failure::Contract)?,
                stage,
            )
            .await?;
        contract
            .accept_initialized_status(202, &body)
            .map_err(Failure::Contract)?;
        if wait {
            return Err(Failure::Wait);
        }
        stage = Stage::Search;
        let (body, _wait) = post
            .post(contract.search_request().map_err(Failure::Contract)?, stage)
            .await?;
        let text = contract
            .accept_search_json(&body, None)
            .map_err(Failure::Contract)?;
        Ok(MdnReference::from(&text))
    };
    let result = tokio::time::timeout_at(deadline, operation)
        .await
        .unwrap_or(Err(Failure::Timeout));
    match result {
        Ok(reference) => {
            let mut out = SearchOutcome::failed(SearchBackendKind::MdnMcp, "");
            out.error = None;
            out.unstructured_reference = Some(reference);
            out.reuse = Some(SearchReuse::Fresh);
            out.retrieved_at_utc =
                Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            out
        }
        Err(failure) => SearchOutcome::failed(
            SearchBackendKind::MdnMcp,
            format!(
                "mdn_{}_{}; 本次已停止，不会自动回落或重试",
                stage.code(),
                failure.code()
            ),
        ),
    }
}

#[cfg(test)]
#[path = "mdn_transport_tests.rs"]
mod tests;
