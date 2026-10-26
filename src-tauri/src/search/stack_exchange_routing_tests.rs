use super::*;
use crate::{
    config::{FreeSearchLocale, FreeSearchScope},
    db::Db,
    search::{
        resources::{SearchResources, SearchReuse},
        stack_exchange_sender::owned_endpoint_scope,
    },
};
use axum::{
    body::Body,
    extract::State,
    http::{Request, Response},
    routing::any,
    Router,
};
use std::sync::{Arc, Mutex};
const GOOD: &str = r#"{"items":[{"question_id":42,"title":"Owned &amp; title","body":"must not leak","owner":{"display_name":"must not leak"},"link":"https://other.invalid/private"}],"quota_remaining":7,"has_more":false}"#;
#[derive(Clone)]
struct Fixture {
    body: &'static str,
    seen: Arc<Mutex<Vec<(String, String, axum::http::HeaderMap)>>>,
}
struct Owned {
    url: String,
    fixture: Fixture,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Owned {
    async fn new(body: &'static str) -> Self {
        let fixture = Fixture {
            body,
            seen: Default::default(),
        };
        let app = Router::new()
            .fallback(any(
                |State(s): State<Fixture>, r: Request<Body>| async move {
                    s.seen.lock().unwrap().push((
                        r.method().to_string(),
                        r.uri().to_string(),
                        r.headers().clone(),
                    ));
                    Response::builder()
                        .status(200)
                        .header("content-type", "application/json")
                        .body(Body::from(s.body))
                        .unwrap()
                },
            ))
            .with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/2.3/search/advanced",
            listener.local_addr().unwrap()
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, fixture, task }
    }
    fn selected(&self) -> Option<SelectedBackend> {
        let mut b = FreeAggregateBackend::default();
        // Even a missing production scope guard can reach loopback only.
        let origin = reqwest::Url::parse(&self.url)
            .unwrap()
            .origin()
            .ascii_serialization();
        b.bing_base_url = origin.clone();
        b.ddg_base_url = origin.clone();
        b.npm_base_url = origin.clone();
        b.hn_base_url = origin;
        Some(SelectedBackend::Aggregate(b))
    }
    fn count(&self) -> usize {
        self.fixture.seen.lock().unwrap().len()
    }
}
fn cfg() -> SearchConfig {
    SearchConfig {
        backend: SearchBackendKind::FreeAggregate,
        free_search_scope: FreeSearchScope::StackOverflow,
        timeout_ms: 1500,
        ..Default::default()
    }
}
fn query() -> SearchQuery {
    SearchQuery {
        text: "Rust owned ? & bounded".into(),
        max_results: 3,
    }
}
async fn route(o: &Owned, c: &SearchConfig, r: &SearchResources, q: &SearchQuery) -> SearchOutcome {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "authorization",
        reqwest::header::HeaderValue::from_static("owned-model-credential"),
    );
    let model = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    execute_selected_with_resources(&model, c, Some("must-not-copy"), q, o.selected(), r).await
}
async fn row(db: &Db) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM meta WHERE key='search.stack_exchange.admission.v1'")
        .fetch_optional(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn stack_route_db_less_and_legacy_direct_fail_before_send_or_fallback() {
    let o = Owned::new(GOOD).await;
    let c = cfg();
    let q = query();
    owned_endpoint_scope(&o.url, async {
        let r = SearchResources::new(c.clone());
        assert!(!r
            .supported_scopes()
            .contains(&FreeSearchScope::StackOverflow));
        let out = route(&o, &c, &r, &q).await;
        assert_eq!(
            out.error.as_deref(),
            Some("stack_overflow_storage_required")
        );
        assert_eq!(o.count(), 0);
        let out = execute_selected(free_client().unwrap(), &c, None, &q, o.selected()).await;
        assert_eq!(
            out.error.as_deref(),
            Some("stack_overflow_protected_path_required")
        );
        assert_eq!(o.count(), 0);
        let Some(SelectedBackend::Aggregate(b)) = o.selected() else {
            unreachable!()
        };
        assert!(b
            .search_with_preferences(
                free_client().unwrap(),
                &q,
                None,
                0,
                FreeSearchScope::StackOverflow,
                FreeSearchLocale::SourceDefault
            )
            .await
            .is_err());
        assert_eq!(o.count(), 0);
    })
    .await;
}
#[tokio::test]
async fn stack_route_real_protected_consumer_cache_and_wait_preserve_metadata_only_results() {
    let o = Owned::new(GOOD).await;
    let mut c = cfg();
    c.source_min_interval_ms = 180_000;
    c.query_cache_ttl_secs = 10;
    c.coalesce_queries = true;
    c.free_search_locale = FreeSearchLocale::CnZh;
    c.page_text_enabled = true;
    let db = Db::connect_in_memory().await.unwrap();
    let r = SearchResources::with_database(c.clone(), db.clone());
    owned_endpoint_scope(&o.url, async {
        let out = route(&o, &c, &r, &query()).await;
        assert_eq!(out.hits, 1);
        assert!(out.error.is_none(), "{:?}", out.error);
        assert_eq!(out.result_sources, vec!["stack_overflow"]);
        assert!(out.failed_sources.is_empty());
        assert_eq!(out.results[0].url, "https://stackoverflow.com/questions/42");
        assert!(!serde_json::to_string(&out)
            .unwrap()
            .contains("must not leak"));
        assert!(out.page_text.is_none() && out.requested_locale.is_none());
        let before = row(&db).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&before).unwrap();
        assert_eq!(v["reported_remaining"], 7);
        assert_eq!(v["phase"], "observed");
        assert!(
            v["local_not_before_ms"].as_i64().unwrap() - v["updated_ms"].as_i64().unwrap()
                >= 179_000
        );
        let cached = route(&o, &c, &r, &query()).await;
        assert_eq!(cached.reuse, Some(SearchReuse::Cached));
        assert_eq!(row(&db).await.unwrap(), before);
        assert_eq!(o.count(), 1);
        let q = SearchQuery {
            text: "another owned question".into(),
            max_results: 3,
        };
        let blocked = route(&o, &c, &r, &q).await;
        assert_eq!(blocked.error.as_deref(), Some("stack_overflow_wait"));
        assert_eq!(o.count(), 1);
        c.source_min_interval_ms = 0;
        r.reconfigure(&c);
        assert_eq!(
            route(&o, &c, &r, &q).await.error.as_deref(),
            Some("stack_overflow_wait")
        );
        assert_eq!(o.count(), 1);
        let seen = o.fixture.seen.lock().unwrap();
        let (method, url, headers) = &seen[0];
        assert_eq!(method, "GET");
        assert!(url.starts_with("/2.3/search/advanced?"));
        let parsed = reqwest::Url::parse(&format!("http://owned{url}")).unwrap();
        let pairs: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
        assert_eq!(pairs.get("site").map(|s| s.as_ref()), Some("stackoverflow"));
        assert_eq!(
            pairs.get("q").map(|s| s.as_ref()),
            Some(query().text.as_str())
        );
        for name in [
            "authorization",
            "cookie",
            "x-api-key",
            "x-subscription-token",
        ] {
            assert!(!headers.contains_key(name));
        }
    })
    .await;
    r.drain_source_writes(Duration::from_secs(2)).await.unwrap();
}
#[tokio::test]
async fn stack_route_unknown_zero_and_invalid_results_preserve_protection_without_cache_or_fallback(
) {
    for (body, first_hits, reason, expected_state) in [
        (
            r#"{"items":[{"question_id":43,"title":"owned"}]}"#,
            1,
            "stack_overflow_outcome_unknown",
            crate::search::stack_exchange_scope::LocalState::OutcomeUnknown,
        ),
        (
            r#"{"items":[{"question_id":43,"title":"owned"}],"quota_remaining":0}"#,
            1,
            "stack_overflow_quota_exhausted",
            crate::search::stack_exchange_scope::LocalState::Exhausted,
        ),
        (
            r#"{"items":"invalid","quota_remaining":7,"backoff":120}"#,
            0,
            "stack_overflow_wait",
            crate::search::stack_exchange_scope::LocalState::Waiting,
        ),
    ] {
        let o = Owned::new(body).await;
        let mut c = cfg();
        c.query_cache_ttl_secs = 10;
        c.coalesce_queries = true;
        let db = Db::connect_in_memory().await.unwrap();
        let r = SearchResources::with_database(c.clone(), db.clone());
        owned_endpoint_scope(&o.url, async {
            let first = route(&o, &c, &r, &query()).await;
            assert_eq!(first.hits, first_hits);
            if expected_state == crate::search::stack_exchange_scope::LocalState::OutcomeUnknown {
                assert_eq!(
                    first.error.as_deref(),
                    Some("stack_overflow_controls_unresolved")
                );
                assert_eq!(r.stats().entries, 0);
            }
            let status = r.stack_exchange().unwrap().state().await;
            assert_eq!(status.state, expected_state);
            let second = route(
                &o,
                &c,
                &r,
                &SearchQuery {
                    text: "different noncached query".into(),
                    max_results: 3,
                },
            )
            .await;
            assert_eq!(second.error.as_deref(), Some(reason));
            assert_eq!(o.count(), 1, "no retry/page/alternate source");
        })
        .await;
        r.drain_source_writes(Duration::from_secs(2)).await.unwrap();
    }
}
#[tokio::test]
async fn stack_route_captured_old_config_and_legacy_web_state_never_dispatch() {
    let o = Owned::new(GOOD).await;
    let c = cfg();
    let db = Db::connect_in_memory().await.unwrap();
    let r = SearchResources::with_database(SearchConfig::default(), db.clone());
    owned_endpoint_scope(&o.url, async {
        assert_eq!(
            route(&o, &c, &r, &query()).await.error.as_deref(),
            Some("stack_overflow_configuration_changed")
        );
        assert_eq!(o.count(), 0);
        assert!(row(&db).await.is_none());
        assert!(
            r.source_protection(FreeSearchScope::StackOverflow)
                .await
                .is_none(),
            "do not display Web status for this scope"
        );
    })
    .await;
}
#[test]
fn stack_route_scope_serde_preserves_old_default_and_rejects_unknown() {
    assert_eq!(
        SearchConfig::default().free_search_scope,
        FreeSearchScope::Web
    );
    assert_eq!(
        serde_json::to_string(&FreeSearchScope::StackOverflow).unwrap(),
        "\"stack_overflow\""
    );
    assert_eq!(
        serde_json::from_str::<FreeSearchScope>("\"stack_overflow\"").unwrap(),
        FreeSearchScope::StackOverflow
    );
    assert!(serde_json::from_str::<FreeSearchScope>("\"all_stack_exchange_sites\"").is_err());
}

#[tokio::test]
async fn stack_route_manual_probe_is_protected_and_fresh_not_a_cache_hit() {
    let o = Owned::new(GOOD).await;
    let mut c = cfg();
    c.query_cache_ttl_secs = 10;
    c.coalesce_queries = true;
    c.page_text_enabled = true;
    let db = Db::connect_in_memory().await.unwrap();
    let r = SearchResources::with_database(c.clone(), db);
    owned_endpoint_scope(&o.url, async {
        let first = test_backend_selected(
            free_client().unwrap(),
            &c,
            Some("must-not-copy"),
            &query().text,
            &r,
            o.selected(),
        )
        .await;
        assert_eq!(
            first.hits, 1,
            "manual source test must use protected path: {:?}",
            first.error
        );
        assert!(first.error.is_none() && first.page_text.is_none());
        assert_eq!(o.count(), 1);
        let second = test_backend_selected(
            free_client().unwrap(),
            &c,
            None,
            &query().text,
            &r,
            o.selected(),
        )
        .await;
        assert_eq!(second.error.as_deref(), Some("stack_overflow_wait"));
        assert_eq!(o.count(), 1);
        assert_eq!(
            r.stats().entries,
            0,
            "manual diagnostic must not populate query cache"
        );
        assert_eq!(
            r.stats().queries_started,
            0,
            "query reuse counters are not wire-send receipts"
        );
    })
    .await;
    r.drain_source_writes(Duration::from_secs(2)).await.unwrap();
}
