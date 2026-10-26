//! Protected, unauthenticated Stack Overflow consumer. No config activation.
use super::{
    backend::{SearchQuery, SearchResult},
    stack_exchange::{Controls, Failure as DecodeFailure},
    stack_exchange_admission::{Admission, Block, Completion, View},
};
use crate::db::Db;
use std::time::Duration;
use tokio::{sync::RwLock, time::Instant};

const ENDPOINT: &str = "https://api.stackexchange.com/2.3/search/advanced";
#[cfg(test)]
tokio::task_local! { static OWNED_ENDPOINT: reqwest::Url; }
/// Compile-test-only owned transport. No production endpoint override exists.
#[cfg(test)]
pub(super) async fn owned_endpoint_scope<F: std::future::Future>(
    raw: &str,
    future: F,
) -> F::Output {
    let url = reqwest::Url::parse(raw).unwrap();
    assert_eq!(url.scheme(), "http");
    assert!(url
        .host_str()
        .and_then(|h| h.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback()));
    assert!(url.username().is_empty() && url.password().is_none());
    assert!(url.query().is_none() && url.fragment().is_none());
    assert_eq!(url.path(), "/2.3/search/advanced");
    OWNED_ENDPOINT.scope(url, future).await
}
fn protected_endpoint() -> Option<reqwest::Url> {
    #[cfg(test)]
    if let Ok(owned) = OWNED_ENDPOINT.try_with(Clone::clone) {
        return Some(owned);
    }
    reqwest::Url::parse(ENDPOINT).ok()
}

/// Local dispatch evidence, not a provider charge or exact wire-send counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    NotStarted,
    MayHaveStarted,
    ResponseReceived,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    InvalidRequest,
    Client,
    Admission(Block),
    Http(u16),
    Transport,
    Timeout,
    BodyTooLarge,
    Encoding,
    Decode(DecodeFailure),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failed {
    pub kind: FailureKind,
    pub dispatch: Dispatch,
}
#[derive(Debug)]
pub struct Report {
    pub results: Vec<SearchResult>,
    pub controls: Controls,
    /// Usable current results do not refund unknown future quota. Callers must
    /// retain ControlsUnresolved rather than interpreting success as availability.
    pub completion: Completion,
}
pub struct ProtectedSender {
    gate: Admission,
    http: reqwest::Client,
    endpoint: reqwest::Url,
    active: RwLock<bool>,
}
impl ProtectedSender {
    /// Uses a dedicated credential-free search client. Neither a model client
    /// nor an arbitrary endpoint can be supplied by production callers.
    pub fn with_min_interval(
        db: Db,
        proxy: Option<&str>,
        interval_ms: u32,
    ) -> Result<Self, Failed> {
        let gate = Admission::with_min_interval(db, interval_ms).map_err(|kind| Failed {
            kind: FailureKind::Admission(kind),
            dispatch: Dispatch::NotStarted,
        })?;
        let failure = || Failed {
            kind: FailureKind::Client,
            dispatch: Dispatch::NotStarted,
        };
        let http = super::egress::ClientPool::default()
            .client(proxy)
            .map_err(|_| failure())?;
        Ok(Self {
            gate,
            http,
            endpoint: protected_endpoint().ok_or_else(failure)?,
            active: RwLock::new(true),
        })
    }
    pub fn new(db: Db, proxy: Option<&str>) -> Result<Self, Failed> {
        Self::with_min_interval(db, proxy, 0)
    }
    pub async fn is_active(&self) -> bool {
        *self.active.read().await
    }
    pub async fn state(&self) -> Result<View, Block> {
        self.gate.view().await
    }
    /// Explicit recovery on the current non-restored ledger only; never an
    /// automatic HTTP retry or a way to clear authorized/legacy unknown/zero.
    pub async fn recover_unsent(&self) -> Result<super::stack_exchange_admission::Recovery, Block> {
        let active = self.active.read().await;
        if !*active {
            return Err(Block::Retired);
        }
        self.gate.recover_unsent().await
    }
    /// Stops new sends and drains these local request futures plus admission DB
    /// tasks. In-flight remote delivery/cost and other DB users remain unknown;
    /// this is not a quota refund, remote cancel or complete DB-restore protocol.
    pub async fn retire(&self) {
        *self.active.write().await = false;
        self.gate.retire().await;
    }
    pub async fn search(&self, query: &SearchQuery, timeout: Duration) -> Result<Report, Failed> {
        let timeout = timeout.min(Duration::from_secs(10));
        if timeout.is_zero()
            || query.text.trim().is_empty()
            || query.text.chars().count() > super::free_aggregate::MAX_QUERY_CHARS
            || query.text.chars().any(char::is_control)
            || !(1..=super::free_aggregate::MAX_RESULTS as u32).contains(&query.max_results)
        {
            return Err(Failed {
                kind: FailureKind::InvalidRequest,
                dispatch: Dispatch::NotStarted,
            });
        }
        let request = self
            .http
            .get(self.endpoint.clone())
            .query(&[
                ("q", query.text.trim().to_owned()),
                ("site", "stackoverflow".to_owned()),
                ("page", "1".to_owned()),
                ("pagesize", query.max_results.to_string()),
                ("sort", "relevance".to_owned()),
                ("order", "desc".to_owned()),
            ])
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ACCEPT_ENCODING, "gzip")
            .header(
                reqwest::header::USER_AGENT,
                "LLM-Gateway/0.2.1 (Stack Overflow search)",
            )
            .build()
            .map_err(|_| Failed {
                kind: FailureKind::InvalidRequest,
                dispatch: Dispatch::NotStarted,
            })?;
        if request.url().as_str().len() > super::free_aggregate::MAX_URL_BYTES {
            return Err(Failed {
                kind: FailureKind::InvalidRequest,
                dispatch: Dispatch::NotStarted,
            });
        }
        let deadline = Instant::now() + timeout;
        let mut dispatch = Dispatch::NotStarted;
        let operation = async {
            let active = self.active.read().await;
            if !*active {
                return Err(FailureKind::Admission(Block::Retired));
            }
            let reservation = self
                .gate
                .reserve(deadline)
                .await
                .map_err(FailureKind::Admission)?;
            let attempt = self
                .gate
                .authorize(reservation)
                .await
                .map_err(FailureKind::Admission)?;
            // From this point a transport failure must not be reported as zero
            // sends. Dropping the caller drops this HTTP future, not its durable
            // unresolved reservation or already-submitted DB metadata work.
            dispatch = Dispatch::MayHaveStarted;
            let response = self
                .http
                .execute(request)
                .await
                .map_err(|_| FailureKind::Transport)?;
            dispatch = Dispatch::ResponseReceived;
            let status = response.status().as_u16();
            let received_ms = chrono::Utc::now().timestamp_millis();
            let retry_after = if matches!(status, 429 | 503) {
                response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| super::cooldown::retry_after(v, received_ms))
                    .filter(|until| *until >= received_ms)
            } else {
                None
            };
            if let Some(until) = retry_after {
                // Save a known wait before a stalled/invalid body can be dropped.
                // This updates no quota and does not resolve the attempt.
                self.gate
                    .note_backoff(&attempt, until, received_ms)
                    .await
                    .map_err(FailureKind::Admission)?;
            }
            let body = read_body(response).await;
            let observed_ms = chrono::Utc::now().timestamp_millis();
            let (mut controls, decoded) = match body {
                Ok(body) => match super::stack_exchange::inspect(
                    &body,
                    query.max_results as usize,
                    observed_ms,
                ) {
                    Ok(inspected) => (
                        inspected.controls,
                        inspected.results.map_err(FailureKind::Decode),
                    ),
                    Err(kind) => (Controls::default(), Err(FailureKind::Decode(kind))),
                },
                Err(kind) => (Controls::default(), Err(kind)),
            };
            if let Some(until) = retry_after {
                controls.backoff_until_ms = Some(
                    controls
                        .backoff_until_ms
                        .map_or(until, |known| known.max(until)),
                );
            }
            // Preserve response facts even for unusable items/API/HTTP failures.
            // If timeout cancels this await, DB supervision still saves facts;
            // no detached HTTP, fallback or retry is created.
            let completion = self
                .gate
                .finish(attempt, &controls, observed_ms)
                .await
                .map_err(FailureKind::Admission)?;
            if completion == Completion::Stale {
                return Err(FailureKind::Timeout);
            }
            if status != 200 {
                return Err(FailureKind::Http(status));
            }
            Ok(Report {
                results: decoded?,
                controls,
                completion,
            })
        };
        let result = match tokio::time::timeout_at(deadline, operation).await {
            Ok(result) => result,
            Err(_) => Err(FailureKind::Timeout),
        };
        result.map_err(|kind| Failed { kind, dispatch })
    }
    #[cfg(test)]
    fn fixture(db: Db, url: String) -> Self {
        Self {
            gate: Admission::new(db),
            http: super::executor::free_client().unwrap().clone(),
            endpoint: reqwest::Url::parse(&url).unwrap(),
            active: RwLock::new(true),
        }
    }
}

async fn read_body(mut response: reqwest::Response) -> Result<String, FailureKind> {
    let max = super::free_aggregate::MAX_BODY_BYTES;
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(FailureKind::BodyTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| FailureKind::Transport)? {
        if chunk.len() > max.saturating_sub(bytes.len()) {
            return Err(FailureKind::BodyTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| FailureKind::Encoding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::{Request, State},
        http::{Response, StatusCode},
        routing::any,
        Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use tokio::sync::Notify;

    const GOOD: &str = r#"{"items":[{"question_id":42,"title":"Rust &amp; SQL","owner":{"display_name":"PRIVATE_OWNER"},"body":"PRIVATE_BODY"}],"quota_remaining":5,"has_more":true}"#;
    #[derive(Clone)]
    struct Server {
        count: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<(String, String, axum::http::HeaderMap)>>>,
        status: u16,
        body: Vec<u8>,
        retry_after: Option<String>,
        location: Option<&'static str>,
        gzip: bool,
        hold: bool,
        headers_hold: bool,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    impl Server {
        fn json(body: &str) -> Self {
            Self {
                count: Arc::new(AtomicUsize::new(0)),
                seen: Arc::new(Mutex::new(vec![])),
                status: 200,
                body: body.as_bytes().to_vec(),
                retry_after: None,
                location: None,
                gzip: false,
                hold: false,
                headers_hold: false,
                entered: Arc::new(Notify::new()),
                release: Arc::new(Notify::new()),
            }
        }
    }
    struct OwnedServer(tokio::task::JoinHandle<()>);
    impl Drop for OwnedServer {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    async fn handler(State(s): State<Server>, r: Request) -> Response<Body> {
        s.count.fetch_add(1, Ordering::SeqCst);
        s.seen.lock().unwrap().push((
            r.method().to_string(),
            r.uri().to_string(),
            r.headers().clone(),
        ));
        let mut response = Response::builder()
            .status(StatusCode::from_u16(s.status).unwrap())
            .header("content-type", "application/json");
        if let Some(v) = s.retry_after {
            response = response.header("retry-after", v);
        }
        if let Some(v) = s.location {
            response = response.header("location", v);
        }
        if s.gzip {
            response = response.header("content-encoding", "gzip");
        }
        let body = if s.hold {
            let release = s.release.clone();
            let data = s.body.clone();
            Body::from_stream(async_stream::stream! {
                release.notified().await;
                yield Ok::<_,std::io::Error>(axum::body::Bytes::from(data));
            })
        } else {
            Body::from(s.body.clone())
        };
        s.entered.notify_one();
        if s.headers_hold {
            s.release.notified().await;
        }
        response.body(body).unwrap()
    }
    async fn owned(s: Server) -> (Arc<ProtectedSender>, OwnedServer, Db) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/2.3/search/advanced",
            listener.local_addr().unwrap()
        );
        let app = Router::new().fallback(any(handler)).with_state(s);
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let db = Db::connect_in_memory().await.unwrap();
        (
            Arc::new(ProtectedSender::fixture(db.clone(), url)),
            OwnedServer(task),
            db,
        )
    }
    fn query() -> SearchQuery {
        SearchQuery {
            text: "Rust & 中文 ? site=other".into(),
            max_results: 3,
        }
    }
    fn duration() -> Duration {
        Duration::from_secs(3)
    }
    async fn ledger(db: &Db) -> Option<String> {
        sqlx::query_scalar("SELECT value FROM meta WHERE key='search.stack_exchange.admission.v1'")
            .fetch_optional(db.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn protected_sender_exact_escaped_single_get_and_native_control_settlement() {
        let s = Server::json(GOOD);
        let (sender, _server, db) = owned(s.clone()).await;
        let got = sender.search(&query(), duration()).await.unwrap();
        assert_eq!(got.results.len(), 1);
        assert_eq!(got.results[0].url, "https://stackoverflow.com/questions/42");
        assert_eq!(
            got.controls.shared_quota,
            super::super::stack_exchange::SharedQuota::ReportedRemaining(5)
        );
        assert_eq!(got.completion, Completion::Current);
        let view = sender.state().await.unwrap();
        assert!(!view.needs_observation);
        assert_eq!(view.shared_remaining, Some(5));
        {
            let seen = s.seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            let (method, uri, headers) = &seen[0];
            assert_eq!(method, "GET");
            let url = reqwest::Url::parse(&format!("http://owned.invalid{uri}")).unwrap();
            assert_eq!(url.path(), "/2.3/search/advanced");
            let fields: std::collections::BTreeMap<_, _> = url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            assert_eq!(fields.len(), 6);
            assert_eq!(fields["q"], query().text);
            assert_eq!(fields["site"], "stackoverflow");
            assert_eq!(fields["page"], "1");
            assert_eq!(fields["pagesize"], "3");
            assert_eq!(fields["sort"], "relevance");
            assert_eq!(fields["order"], "desc");
            for h in ["authorization", "cookie", "x-api-key", "referer"] {
                assert!(!headers.contains_key(h));
            }
        }
        let second = sender.search(&query(), duration()).await.unwrap_err();
        assert!(matches!(
            second.kind,
            FailureKind::Admission(Block::Wait(_))
        ));
        assert_eq!(second.dispatch, Dispatch::NotStarted);
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
        let raw = ledger(&db).await.unwrap();
        for forbidden in [
            "Rust",
            "PRIVATE_OWNER",
            "PRIVATE_BODY",
            "site=other",
            "question_id",
        ] {
            assert!(!raw.contains(forbidden));
        }
    }
    #[tokio::test]
    async fn protected_sender_preflight_invalid_input_timeout_and_retired_are_zero_dispatch() {
        let s = Server::json(GOOD);
        let (sender, _server, db) = owned(s.clone()).await;
        for q in [
            SearchQuery {
                text: "".into(),
                max_results: 1,
            },
            SearchQuery {
                text: "x\ny".into(),
                max_results: 1,
            },
            SearchQuery {
                text: "x".repeat(257),
                max_results: 1,
            },
            SearchQuery {
                text: "x".into(),
                max_results: 0,
            },
        ] {
            let e = sender.search(&q, duration()).await.unwrap_err();
            assert_eq!(
                e,
                Failed {
                    kind: FailureKind::InvalidRequest,
                    dispatch: Dispatch::NotStarted
                }
            );
        }
        assert_eq!(
            sender
                .search(&query(), Duration::ZERO)
                .await
                .unwrap_err()
                .kind,
            FailureKind::InvalidRequest
        );
        sender.retire().await;
        assert_eq!(
            sender.search(&query(), duration()).await.unwrap_err(),
            Failed {
                kind: FailureKind::Admission(Block::Retired),
                dispatch: Dispatch::NotStarted
            }
        );
        assert_eq!(s.count.load(Ordering::SeqCst), 0);
        assert!(ledger(&db).await.is_none());
    }
    #[tokio::test]
    async fn protected_sender_concurrent_calls_have_one_http_attempt() {
        let s = Server::json(GOOD);
        let (sender, _server, _) = owned(s.clone()).await;
        let q = query();
        let (a, b) = tokio::join!(sender.search(&q, duration()), sender.search(&q, duration()));
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
        let err = if let Err(e) = a { e } else { b.unwrap_err() };
        assert!(matches!(
            err.kind,
            FailureKind::Admission(Block::Unresolved | Block::Wait(_))
        ));
        assert_eq!(err.dispatch, Dispatch::NotStarted);
    }
    #[tokio::test]
    async fn protected_sender_api_error_preserves_backoff_and_quota_without_raw_error() {
        let s = Server::json(
            r#"{"error_id":502,"error_message":"PRIVATE_UPSTREAM_ERROR","quota_remaining":4,"backoff":120}"#,
        );
        let (sender, _server, _) = owned(s.clone()).await;
        let e = sender.search(&query(), duration()).await.unwrap_err();
        assert_eq!(e.kind, FailureKind::Decode(DecodeFailure::ApiError));
        assert_eq!(e.dispatch, Dispatch::ResponseReceived);
        assert!(!format!("{e:?}").contains("PRIVATE"));
        let view = sender.state().await.unwrap();
        assert_eq!(view.shared_remaining, Some(4));
        assert!(!view.needs_observation);
        assert!(
            view.server_not_before_ms.unwrap() > chrono::Utc::now().timestamp_millis() + 100_000
        );
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn protected_sender_invalid_items_still_commits_valid_controls() {
        let s = Server::json(r#"{"items":null,"quota_remaining":0,"backoff":60}"#);
        let (sender, _server, _) = owned(s.clone()).await;
        assert_eq!(
            sender.search(&query(), duration()).await.unwrap_err().kind,
            FailureKind::Decode(DecodeFailure::InvalidItems)
        );
        let view = sender.state().await.unwrap();
        assert!(view.exhausted);
        assert!(!view.needs_observation);
        assert_eq!(view.shared_remaining, Some(0));
        assert!(view.server_not_before_ms.is_some());
        assert_eq!(
            sender.search(&query(), duration()).await.unwrap_err().kind,
            FailureKind::Admission(Block::Exhausted)
        );
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn protected_sender_missing_quota_returns_hits_but_next_request_is_blocked() {
        let s = Server::json(r#"{"items":[{"question_id":7,"title":"Valid"}]}"#);
        let (sender, _server, _) = owned(s.clone()).await;
        let got = sender.search(&query(), duration()).await.unwrap();
        assert_eq!(got.results.len(), 1);
        assert_eq!(got.completion, Completion::ControlsUnresolved);
        assert!(sender.state().await.unwrap().needs_observation);
        assert_eq!(
            sender.search(&query(), duration()).await.unwrap_err().kind,
            FailureKind::Admission(Block::Unresolved)
        );
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn protected_sender_redirect_is_not_followed_or_retried() {
        let mut s = Server::json(r#"{"items":[],"quota_remaining":3}"#);
        s.status = 302;
        s.location = Some("http://127.0.0.1:1/private");
        let (sender, _server, _) = owned(s.clone()).await;
        let e = sender.search(&query(), duration()).await.unwrap_err();
        assert_eq!(e.kind, FailureKind::Http(302));
        assert_eq!(e.dispatch, Dispatch::ResponseReceived);
        assert_eq!(sender.state().await.unwrap().shared_remaining, Some(3));
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn protected_sender_overlong_decoded_body_preserves_unknown_not_zero() {
        let mut s = Server::json("{}");
        s.body = vec![b' '; super::super::free_aggregate::MAX_BODY_BYTES + 1];
        let (sender, _server, _) = owned(s.clone()).await;
        let e = sender.search(&query(), duration()).await.unwrap_err();
        assert_eq!(e.kind, FailureKind::BodyTooLarge);
        assert_eq!(e.dispatch, Dispatch::ResponseReceived);
        let v = sender.state().await.unwrap();
        assert!(v.needs_observation);
        assert_eq!(v.shared_remaining, None);
        assert!(!v.exhausted);
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn protected_sender_invalid_utf8_is_fixed_error_and_not_echoed() {
        let mut s = Server::json("{}");
        s.body = vec![0xff, 0xfe];
        let (sender, _server, _) = owned(s.clone()).await;
        let e = sender.search(&query(), duration()).await.unwrap_err();
        assert_eq!(e.kind, FailureKind::Encoding);
        assert_eq!(e.dispatch, Dispatch::ResponseReceived);
        assert!(sender.state().await.unwrap().needs_observation);
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn protected_sender_timeout_keeps_retry_after_received_before_stalled_body() {
        let mut s = Server::json("{}");
        s.status = 503;
        s.retry_after = Some("120".into());
        s.hold = true;
        let (sender, _server, _) = owned(s.clone()).await;
        let e = sender
            .search(&query(), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(e.kind, FailureKind::Timeout);
        assert_eq!(e.dispatch, Dispatch::ResponseReceived);
        let v = sender.state().await.unwrap();
        assert!(v.needs_observation);
        assert!(v.server_not_before_ms.unwrap() > chrono::Utc::now().timestamp_millis() + 100_000);
        assert_eq!(v.shared_remaining, None);
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn protected_sender_caller_abort_keeps_pending_and_retirement_blocks_new_sends() {
        let mut s = Server::json("{}");
        s.hold = true;
        let (sender, _server, _) = owned(s.clone()).await;
        let clone = sender.clone();
        let task = tokio::spawn(async move { clone.search(&query(), duration()).await });
        tokio::time::timeout(Duration::from_secs(2), s.entered.notified())
            .await
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        sender.retire().await;
        assert!(sender.state().await.unwrap().needs_observation);
        assert_eq!(
            sender.search(&query(), duration()).await.unwrap_err().kind,
            FailureKind::Admission(Block::Retired)
        );
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn sender_review_production_endpoint_is_fixed_and_credential_proxy_rejected() {
        let db = Db::connect_in_memory().await.unwrap();
        let sender = ProtectedSender::new(db.clone(), None).unwrap();
        assert_eq!(sender.endpoint.as_str(), ENDPOINT);
        assert_eq!(sender.endpoint.scheme(), "https");
        assert_eq!(sender.endpoint.host_str(), Some("api.stackexchange.com"));
        assert_eq!(sender.endpoint.path(), "/2.3/search/advanced");
        assert!(sender.endpoint.query().is_none());
        assert!(matches!(
            ProtectedSender::new(db, Some("http://private:credential@127.0.0.1:1")),
            Err(Failed {
                kind: FailureKind::Client,
                dispatch: Dispatch::NotStarted
            })
        ));
    }
    #[tokio::test]
    async fn sender_review_gzip_header_decodes_but_stripped_header_is_explicitly_unsupported() {
        for header in [true, false] {
            let mut s = Server::json("{}");
            s.body = vec![
                31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 171, 86, 202, 44, 73, 205, 45, 86, 178, 138,
                174, 86, 42, 44, 77, 45, 46, 201, 204, 207, 139, 207, 76, 81, 178, 50, 49, 210, 81,
                42, 201, 44, 201, 73, 85, 178, 82, 10, 42, 45, 46, 81, 170, 141, 213, 1, 170, 200,
                47, 73, 140, 47, 74, 205, 77, 204, 204, 203, 204, 75, 87, 178, 50, 173, 5, 0, 22,
                187, 116, 204, 65, 0, 0, 0,
            ];
            s.gzip = header;
            let (sender, _server, _) = owned(s.clone()).await;
            let result = sender.search(&query(), duration()).await;
            if header {
                assert_eq!(
                    result.unwrap().results[0].url,
                    "https://stackoverflow.com/questions/42"
                );
                assert_eq!(sender.state().await.unwrap().shared_remaining, Some(5));
            } else {
                assert_eq!(result.unwrap_err().kind, FailureKind::Encoding);
                assert!(sender.state().await.unwrap().needs_observation);
            }
            assert_eq!(s.count.load(Ordering::SeqCst), 1);
        }
    }
    #[tokio::test]
    async fn sender_review_gzip_decoded_body_limit_applies_after_small_wire_body() {
        let mut s = Server::json("{}");
        s.body = vec![
            31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 237, 193, 49, 1, 0, 0, 0, 194, 160, 42, 235, 159,
            210, 223, 12, 64, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            156, 1, 80, 20, 74, 200, 1, 0, 4, 0,
        ];
        s.gzip = true;
        assert!(s.body.len() < 4096);
        let (sender, _server, _) = owned(s.clone()).await;
        assert_eq!(
            sender.search(&query(), duration()).await.unwrap_err().kind,
            FailureKind::BodyTooLarge
        );
        assert!(sender.state().await.unwrap().needs_observation);
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn sender_review_retire_waits_active_http_and_blocks_old_sender_afterwards() {
        use std::future::Future;
        let mut s = Server::json(GOOD);
        s.hold = true;
        let (sender, _server, _) = owned(s.clone()).await;
        let caller = sender.clone();
        let task = tokio::spawn(async move { caller.search(&query(), duration()).await });
        tokio::time::timeout(Duration::from_secs(2), s.entered.notified())
            .await
            .unwrap();
        let mut retire = Box::pin(sender.retire());
        let pending = matches!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(retire.as_mut().poll(cx))).await,
            std::task::Poll::Pending
        );
        s.release.notify_one();
        assert!(pending, "retire must wait an active local HTTP future");
        assert!(task.await.unwrap().is_ok());
        tokio::time::timeout(Duration::from_secs(2), retire)
            .await
            .unwrap();
        assert_eq!(
            sender.search(&query(), duration()).await.unwrap_err().kind,
            FailureKind::Admission(Block::Retired)
        );
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn sender_review_retry_after_429_http_date_and_invalid_value_have_real_ledger_effects() {
        let date = (chrono::Utc::now() + chrono::Duration::seconds(120))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        for (header, wait) in [(date, true), ("not-a-delay".to_owned(), false)] {
            let mut s = Server::json(r#"{"items":[],"quota_remaining":4}"#);
            s.status = 429;
            s.retry_after = Some(header);
            let (sender, _server, _) = owned(s.clone()).await;
            assert_eq!(
                sender.search(&query(), duration()).await.unwrap_err().kind,
                FailureKind::Http(429)
            );
            let v = sender.state().await.unwrap();
            assert_eq!(v.shared_remaining, Some(4));
            assert!(!v.needs_observation);
            if wait {
                assert!(
                    v.server_not_before_ms.unwrap()
                        > chrono::Utc::now().timestamp_millis() + 100_000
                );
            } else {
                assert_eq!(v.server_not_before_ms, None);
            }
            assert_eq!(s.count.load(Ordering::SeqCst), 1);
        }
    }
    #[tokio::test]
    async fn sender_review_retry_after_storage_failure_is_response_observed_not_zero_dispatch() {
        let mut s = Server::json(GOOD);
        s.status = 503;
        s.retry_after = Some("120".into());
        s.headers_hold = true;
        let (sender, _server, db) = owned(s.clone()).await;
        let caller = sender.clone();
        let task = tokio::spawn(async move { caller.search(&query(), duration()).await });
        tokio::time::timeout(Duration::from_secs(2), s.entered.notified())
            .await
            .unwrap();
        db.pool().close().await;
        s.release.notify_one();
        let e = task.await.unwrap().unwrap_err();
        assert_eq!(e.kind, FailureKind::Admission(Block::Storage));
        assert_eq!(e.dispatch, Dispatch::ResponseReceived);
        assert_eq!(s.count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn unsent_recovery_sender_consumes_local_recovery_without_any_http_request() {
        let s = Server::json(GOOD);
        let (sender, _server, _) = owned(s.clone()).await;
        drop(
            sender
                .gate
                .reserve(Instant::now() + duration())
                .await
                .unwrap(),
        );
        assert!(sender.state().await.unwrap().can_recover_unsent);
        assert_eq!(
            sender.recover_unsent().await.unwrap(),
            super::super::stack_exchange_admission::Recovery::RecoveredUnsent
        );
        assert!(!sender.state().await.unwrap().needs_observation);
        assert!(matches!(
            sender.search(&query(), duration()).await.unwrap_err().kind,
            FailureKind::Admission(Block::Wait(_))
        ));
        assert_eq!(s.count.load(Ordering::SeqCst), 0);
        sender.retire().await;
        assert_eq!(sender.recover_unsent().await.unwrap_err(), Block::Retired);
    }
}
