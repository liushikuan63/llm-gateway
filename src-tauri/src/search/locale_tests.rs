//! Owned loopback exercises the production resolver, resource and source governors.
use super::*;
use crate::config::{FreeSearchLocale as Locale, FreeSearchScope};
use crate::search::resources::{SearchResources, SearchReuse};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
    routing::any,
    Router,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

#[derive(Clone, Debug)]
struct Wire {
    method: Method,
    path: String,
    params: std::collections::HashMap<String, String>,
}
#[derive(Clone, Default)]
struct Record {
    calls: Arc<Mutex<Vec<Wire>>>,
    hold: Arc<AtomicBool>,
    wait: Arc<AtomicBool>,
    entered: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Notify>,
}
struct Closed(Arc<AtomicUsize>);
impl Drop for Closed {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
struct Owned {
    origin: String,
    record: Record,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Owned {
    async fn new() -> Self {
        let record = Record::default();
        let app=Router::new().fallback(any(|State(s):State<Record>,method:Method,uri:Uri,headers:HeaderMap,body:Bytes|async move {
   assert!(!headers.contains_key("authorization")); assert!(!headers.contains_key("x-subscription-token"));
   let raw=if method==Method::POST {std::str::from_utf8(&body).unwrap()} else {uri.query().unwrap_or("")};
   s.calls.lock().unwrap().push(Wire{method,path:uri.path().into(),params:reqwest::Url::parse(&format!("http://owned.invalid/?{raw}")).unwrap().query_pairs().into_owned().collect()});
   if s.wait.load(Ordering::SeqCst) { return (StatusCode::TOO_MANY_REQUESTS,[("retry-after","60")],"owned wait").into_response(); }
   let content=match uri.path() {
    "/search"=>"<rss><channel><item><title>Owned RSS</title><link>https://owned.invalid/rss</link></item></channel></rss>",
    "/lite/"=>"<a class=\"result-link\" href=\"https://owned.invalid/ddg\">Owned DDG</a>",
    "/-/v1/search"=>r#"{"objects":[{"package":{"name":"owned","version":"1.0.0","description":"Owned package","links":{"npm":"https://www.npmjs.com/package/owned"}}}]}"#,
    _=>panic!("unexpected owned path {uri}"),
   };
   let body=async_stream::stream! {
    let _closed=Closed(s.closed); let notified=s.release.notified(); tokio::pin!(notified); notified.as_mut().enable();
    s.entered.fetch_add(1,Ordering::SeqCst);
    if s.hold.load(Ordering::SeqCst) { yield Ok::<_,std::convert::Infallible>(Bytes::new()); notified.await; }
    yield Ok::<_,std::convert::Infallible>(Bytes::from_static(content.as_bytes()));
   };
   axum::http::Response::builder().status(200).body(axum::body::Body::from_stream(body)).unwrap()
  })).with_state(record.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            origin,
            record,
            task,
        }
    }
    fn count(&self) -> usize {
        self.record.calls.lock().unwrap().len()
    }
}
fn cfg(locale: Locale) -> SearchConfig {
    SearchConfig {
        backend: SearchBackendKind::FreeAggregate,
        free_search_locale: locale,
        timeout_ms: 1500,
        ..Default::default()
    }
}
async fn run(r: &SearchResources, c: &SearchConfig, origin: &str, text: &str) -> SearchOutcome {
    let Some(SelectedBackend::Aggregate(mut b)) = backend_of(c) else {
        panic!("production factory required")
    };
    b.bing_base_url = origin.into();
    b.ddg_base_url = origin.into();
    b.npm_base_url = origin.into();
    execute_selected_with_resources(
        shared_client(),
        c,
        Some("owned-never-forwarded"),
        &SearchQuery {
            text: text.into(),
            max_results: 5,
        },
        Some(SelectedBackend::Aggregate(b)),
        r,
    )
    .await
}
async fn until(mut f: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !f() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned barrier");
}
#[tokio::test]
async fn locale_wire_presets_default_and_technical_scope() {
    let o = Owned::new().await;
    for (locale, suffix, kl) in [
        (Locale::SourceDefault, "", None),
        (Locale::CnZh, "", Some("cn-zh")),
        (Locale::UsEn, " language:en loc:US", Some("us-en")),
        (Locale::GbEn, " language:en loc:GB", Some("uk-en")),
    ] {
        let c = cfg(locale);
        let r = SearchResources::new(c.clone());
        let out = run(&r, &c, &o.origin, "Rust & 中文").await;
        assert!(out.error.is_none(), "{:?}", out.error);
        assert_eq!(out.results.len(), 2);
        assert_eq!(
            out.requested_locale,
            if locale == Locale::SourceDefault {
                None
            } else {
                Some(locale)
            }
        );
        let calls = o.record.calls.lock().unwrap();
        let pair = &calls[calls.len() - 2..];
        let b = pair.iter().find(|x| x.path == "/search").unwrap();
        assert_eq!(b.method, Method::GET);
        assert_eq!(b.params.len(), 3);
        assert_eq!(b.params["q"], format!("Rust & 中文{suffix}"));
        assert_eq!(b.params["format"], "rss");
        assert_eq!(b.params["mkt"], "zh-CN");
        let d = pair.iter().find(|x| x.path == "/lite/").unwrap();
        assert_eq!(d.method, Method::POST);
        assert_eq!(d.params["q"], "Rust & 中文");
        assert_eq!(d.params.get("kl").map(String::as_str), kl);
        assert_eq!(d.params.len(), if kl.is_some() { 2 } else { 1 });
    }
    assert_eq!(o.count(), 8);
    let mut c = cfg(Locale::UsEn);
    c.free_search_scope = FreeSearchScope::NpmPackages;
    let r = SearchResources::new(c.clone());
    let out = run(&r, &c, &o.origin, "npm package owned").await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.requested_locale, None);
    assert_eq!(o.count(), 9);
    let x = o.record.calls.lock().unwrap().last().unwrap().clone();
    assert_eq!(x.path, "/-/v1/search");
    assert_eq!(x.params.len(), 2);
    assert_eq!(x.params["text"], "npm package owned");
}
#[tokio::test]
async fn locale_expanded_bound_rejects_both_sources_before_admission() {
    let o = Owned::new().await;
    let c = cfg(Locale::UsEn);
    let r = SearchResources::new(c.clone());
    let out = run(&r, &c, &o.origin, &"中".repeat(256)).await;
    assert!(out.error.unwrap().contains("locale-derived query"));
    assert_eq!(o.count(), 0);
    let n = 256 - " language:en loc:US".chars().count();
    let out = run(&r, &c, &o.origin, &"中".repeat(n)).await;
    assert!(out.error.is_none());
    assert_eq!(o.count(), 2);
    let c = cfg(Locale::SourceDefault);
    let r = SearchResources::new(c.clone());
    assert!(run(&r, &c, &o.origin, &"中".repeat(256))
        .await
        .error
        .is_none());
    assert_eq!(o.count(), 4);
}
#[tokio::test]
async fn locale_cache_coalesce_and_captured_generation() {
    let o = Owned::new().await;
    let mut c = cfg(Locale::UsEn);
    c.query_cache_ttl_secs = 60;
    c.coalesce_queries = true;
    let r = SearchResources::new(c.clone());
    let first = run(&r, &c, &o.origin, "owned first").await;
    let cached = run(&r, &c, &o.origin, "owned first").await;
    assert_eq!(cached.reuse, Some(SearchReuse::Cached));
    assert_eq!(first.retrieved_at_utc, cached.retrieved_at_utc);
    assert_eq!(cached.requested_locale, Some(Locale::UsEn));
    assert_eq!(o.count(), 2);
    o.record.hold.store(true, Ordering::SeqCst);
    let a = run(&r, &c, &o.origin, "owned held");
    let b = run(&r, &c, &o.origin, "owned held");
    tokio::pin!(a, b);
    tokio::select! {_=&mut a=>panic!("must hold"),_=&mut b=>panic!("must hold"),_ = until(||o.count()==4 && r.stats().coalesced>=1)=>{}}
    let mut gb = c.clone();
    gb.free_search_locale = Locale::GbEn;
    r.reconfigure(&gb);
    o.record.hold.store(false, Ordering::SeqCst);
    o.record.release.notify_waiters();
    let (a, b) = tokio::join!(a, b);
    assert!(a.error.is_none() && b.error.is_none());
    assert_eq!(a.requested_locale, Some(Locale::UsEn));
    assert_eq!(b.requested_locale, Some(Locale::UsEn));
    assert_eq!(a.retrieved_at_utc, b.retrieved_at_utc);
    assert_eq!(
        r.stats().entries,
        0,
        "old captured generation cannot fill current cache"
    );
    let new = run(&r, &gb, &o.origin, "owned held").await;
    assert_eq!(new.requested_locale, Some(Locale::GbEn));
    assert_eq!(o.count(), 6);
    assert_eq!(
        run(&r, &gb, &o.origin, "owned held").await.reuse,
        Some(SearchReuse::Cached)
    );
    assert_eq!(o.count(), 6);
}
#[tokio::test]
async fn locale_switch_does_not_wash_source_retry_after() {
    let o = Owned::new().await;
    o.record.wait.store(true, Ordering::SeqCst);
    let c = cfg(Locale::UsEn);
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let r = SearchResources::with_database(c.clone(), db.clone());
    assert!(run(&r, &c, &o.origin, "owned wait").await.error.is_some());
    assert_eq!(o.count(), 2);
    let mut other = c.clone();
    other.free_search_locale = Locale::GbEn;
    r.reconfigure(&other);
    assert!(run(&r, &other, &o.origin, "owned wait")
        .await
        .error
        .is_some());
    assert_eq!(
        o.count(),
        2,
        "locale cannot create a new physical-source cooldown identity"
    );
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM meta WHERE key LIKE 'search.free_cooldown.v1.%'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(n, 2);
}
#[tokio::test]
async fn locale_cancel_drops_both_owned_bodies_without_clearing_reservation() {
    let o = Owned::new().await;
    o.record.hold.store(true, Ordering::SeqCst);
    let mut c = cfg(Locale::UsEn);
    c.source_min_interval_ms = 60_000;
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let r = SearchResources::with_database(c.clone(), db);
    {
        let work = run(&r, &c, &o.origin, "owned cancel");
        tokio::pin!(work);
        tokio::select! {_=&mut work=>panic!("must hold"),_=until(||o.record.entered.load(Ordering::SeqCst)==2)=>{}}
    }
    until(|| o.record.closed.load(Ordering::SeqCst) == 2).await;
    let mut next = c.clone();
    next.free_search_locale = Locale::GbEn;
    r.reconfigure(&next);
    assert!(run(&r, &next, &o.origin, "owned cancel")
        .await
        .error
        .is_some());
    assert_eq!(o.count(), 2);
}
