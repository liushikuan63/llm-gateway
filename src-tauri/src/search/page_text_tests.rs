use super::*;
use base64::Engine;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
};

struct OwnedPages {
    child: Child,
    transport: OwnedTransport,
    control: String,
}
impl Drop for OwnedPages {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl OwnedPages {
    fn ready(child: &mut Child) -> Value {
        let output = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let r = BufReader::new(output).read_line(&mut line);
            let _ = tx.send((r, line));
        });
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok((Ok(n), line)) if n > 0 => serde_json::from_str(&line).unwrap(),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned page fixture readiness failed");
            }
        }
    }
    fn new() -> Self {
        let support = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support");
        let mut generator = Command::new("pwsh")
            .args(["-NoLogo", "-NoProfile", "-File"])
            .arg(support.join("n4-owned-tls.ps1"))
            .creation_flags(0x08000000)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let certificate = Self::ready(&mut generator);
        assert!(generator.wait().unwrap().success());
        let mut child = Command::new("node")
            .arg(support.join("m3-owned-pages.cjs"))
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        serde_json::to_writer(&mut pipe, &certificate).unwrap();
        pipe.write_all(b"\n").unwrap();
        drop(pipe);
        let ready = Self::ready(&mut child);
        let der = base64::engine::general_purpose::STANDARD
            .decode(ready["root_der"].as_str().unwrap())
            .unwrap();
        Self {
            child,
            transport: OwnedTransport {
                port: ready["port"].as_u64().unwrap() as u16,
                root: reqwest::Certificate::from_der(&der).unwrap(),
            },
            control: format!("http://127.0.0.1:{}", ready["control"]),
        }
    }
    fn result(&self, path: &str) -> SearchResult {
        SearchResult {
            title: "Owned title".into(),
            url: format!(
                "https://owned-upstream.invalid:{}{path}",
                self.transport.port
            ),
            snippet: "Original snippet".into(),
            score: 0.0,
        }
    }
    async fn calls(&self) -> Vec<Value> {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(&self.control)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["calls"]
            .as_array()
            .unwrap()
            .clone()
    }
}
fn cfg() -> SearchConfig {
    SearchConfig {
        backend: SearchBackendKind::FreeAggregate,
        page_text_enabled: true,
        ..Default::default()
    }
}
async fn resources(c: &SearchConfig) -> SearchResources {
    SearchResources::with_database(c.clone(), crate::db::Db::connect_in_memory().await.unwrap())
}

#[test]
fn public_destination_and_visible_text_are_separate_from_owned_tls_exemption() {
    for ip in [
        "0.0.0.0",
        "10.1.2.3",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.1.1",
        "172.16.1.1",
        "192.0.2.1",
        "192.168.1.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.0.0.1",
        "::1",
        "::ffff:8.8.8.8",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
        "2002::1",
    ] {
        assert!(!public_address(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
        assert!(public_address(ip.parse().unwrap()), "{ip}");
    }
    for url in [
        "http://example.org",
        "https://user:pass@example.org",
        "https://example.org:8443",
        "https://127.0.0.1",
        "https://[::1]",
    ] {
        assert!(checked_url(url).is_err());
    }
    let public = SocketAddr::from(([8, 8, 8, 8], 443));
    let local = SocketAddr::from(([127, 0, 0, 1], 443));
    assert!(public_answers(&[public]));
    assert!(!public_answers(&[public, local]));
    assert!(!public_answers(&[]));
    assert!(!public_answers(&vec![public; 17]));
    assert!(checked_url("https://example.org/a%20b?q=x%26y#fragment")
        .unwrap()
        .as_str()
        .ends_with("a%20b?q=x%26y"));
    assert_eq!(
        extract(
            b"<script>bad < tag</script><form>bad</form><p>good &amp; text</p>",
            true
        )
        .unwrap(),
        "good & text"
    );
    assert_eq!(
        extract(
            b"<p>good</p><template>bad</template><p hidden>bad</p>",
            true
        )
        .unwrap(),
        "good"
    );
    assert_eq!(
        extract(b"<meta charset=gbk><p>ascii</p>", true),
        Err("unsupported_encoding")
    );
    assert!(utf8_type("text/html;charset=UTF-8").unwrap());
    assert!(utf8_type("application/json").is_err());
}

#[tokio::test]
async fn owned_tls_default_zero_enabled_two_pages_and_ephemeral_text() {
    let f = OwnedPages::new();
    let c = cfg();
    let r = resources(&c).await;
    let hits = vec![
        f.result("/page"),
        f.result("/page#duplicate"),
        f.result("/two"),
        f.result("/three"),
    ];
    OWNED_TRANSPORT
        .scope(f.transport.clone(), async {
            let mut disabled = c.clone();
            disabled.page_text_enabled = false;
            assert_eq!(
                enrich(
                    &disabled,
                    &hits,
                    Some(&r),
                    Instant::now() + Duration::from_secs(8)
                )
                .await
                .report
                .state,
                "disabled"
            );
            assert!(f.calls().await.is_empty());
            let value = enrich(&c, &hits, Some(&r), Instant::now() + Duration::from_secs(8)).await;
            assert_eq!(
                value.excerpts.len(),
                2,
                "explicit enable must consume actual owned HTTPS pages"
            );
            assert_eq!(f.calls().await.len(), 2);
            let mut rendered = String::new();
            append(&mut rendered, &value);
            assert!(rendered.contains("Owned & visible 正文 😀"));
            assert!(!rendered.contains("secret-"));
            assert!(!serde_json::to_string(&value.report)
                .unwrap()
                .contains("Owned"));
            let again = enrich(&c, &hits, Some(&r), Instant::now() + Duration::from_secs(8)).await;
            assert_eq!(again.excerpts.len(), 2);
            assert_eq!(f.calls().await.len(), 4, "no excerpt cache");
            for call in f.calls().await {
                assert_eq!(call["method"], json!("GET"));
                for field in ["authorization", "cookie", "referer"] {
                    assert_eq!(call[field], json!(false));
                }
            }
        })
        .await;
}

#[tokio::test]
async fn owned_decoded_size_utf8_and_challenge_fail_without_extra_request() {
    let f = OwnedPages::new();
    let c = cfg();
    let r = resources(&c).await;
    OWNED_TRANSPORT
        .scope(f.transport.clone(), async {
            for (path, state) in [
                ("/oversize", "body_limit"),
                ("/compressed", "body_limit"),
                ("/json", "unsupported_content_type"),
                ("/encoding", "unsupported_encoding"),
                ("/invalid-utf8", "unsupported_encoding"),
                ("/captcha", "challenge_or_paywall"),
            ] {
                let n = f.calls().await.len();
                let before = Instant::now();
                let value = enrich(
                    &c,
                    &[f.result(path)],
                    Some(&r),
                    before + Duration::from_secs(8),
                )
                .await;
                assert_eq!(value.report.pages[0].state, state, "{path}");
                assert!(value.excerpts.is_empty());
                assert_eq!(f.calls().await.len(), n + 1);
                assert!(
                    before.elapsed() < Duration::from_secs(3),
                    "bounds must not wait for the unended body EOF"
                );
            }
            let exact = enrich(
                &c,
                &[f.result("/exact")],
                Some(&r),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(exact.excerpts[0].1.chars().count(), MAX_CHARS);
        })
        .await;
}

#[tokio::test]
async fn owned_redirects_revalidate_each_hop_and_never_retry() {
    let f = OwnedPages::new();
    let c = cfg();
    let r = resources(&c).await;
    OWNED_TRANSPORT
        .scope(f.transport.clone(), async {
            let good = enrich(
                &c,
                &[f.result("/redirect")],
                Some(&r),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(good.excerpts.len(), 1);
            assert_eq!(f.calls().await.len(), 2);
            let denied = enrich(
                &c,
                &[f.result("/unsafe")],
                Some(&r),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(denied.report.pages[0].state, "invalid_target");
            assert_eq!(f.calls().await.len(), 3);
            let loops = enrich(
                &c,
                &[f.result("/loop"), f.result("/loop#same")],
                Some(&r),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(loops.report.pages[0].state, "redirect_limit");
            assert_eq!(
                f.calls().await.len(),
                6,
                "one page maximum three GETs; no automatic retry or duplicate fragment fetch"
            );
        })
        .await;
}

#[tokio::test]
async fn owned_explicit_scope_proxy_missing_storage_and_deadline_skip_without_get() {
    let f = OwnedPages::new();
    let c = cfg();
    let r = resources(&c).await;
    let hits = [f.result("/page")];
    OWNED_TRANSPORT
        .scope(f.transport.clone(), async {
            for scope in [FreeSearchScope::NpmPackages, FreeSearchScope::HackerNews] {
                let mut v = c.clone();
                v.free_search_scope = scope;
                assert_eq!(
                    enrich(&v, &hits, Some(&r), Instant::now() + Duration::from_secs(8))
                        .await
                        .report
                        .state,
                    "scope_unavailable"
                );
            }
            let mut proxy = c.clone();
            proxy.free_proxy_url = Some("http://127.0.0.1:1".into());
            assert_eq!(
                enrich(
                    &proxy,
                    &hits,
                    Some(&r),
                    Instant::now() + Duration::from_secs(8)
                )
                .await
                .report
                .state,
                "destination_verification_unavailable"
            );
            assert_eq!(
                enrich(&c, &hits, None, Instant::now() + Duration::from_secs(8))
                    .await
                    .report
                    .state,
                "source_state_unavailable"
            );
            let absent = SearchResources::new(c.clone());
            assert_eq!(
                enrich(
                    &c,
                    &hits,
                    Some(&absent),
                    Instant::now() + Duration::from_secs(8)
                )
                .await
                .report
                .state,
                "source_state_unavailable"
            );
            assert_eq!(
                enrich(&c, &hits, Some(&r), Instant::now())
                    .await
                    .report
                    .pages[0]
                    .state,
                "deadline"
            );
            assert!(f.calls().await.is_empty());
            let before = Instant::now();
            let short = enrich(
                &c,
                &[f.result("/slow")],
                Some(&r),
                before + Duration::from_millis(75),
            )
            .await;
            assert_eq!(short.report.pages[0].state, "deadline");
            assert!(before.elapsed() < Duration::from_secs(1));
            assert_eq!(f.calls().await.len(), 1);
            assert_eq!(r.page_text_permits.available_permits(), 2);
            let before = Instant::now();
            let bounded = enrich(
                &c,
                &[f.result("/slow")],
                Some(&r),
                before + Duration::from_secs(8),
            )
            .await;
            assert_eq!(bounded.report.pages[0].state, "deadline");
            assert!(
                before.elapsed() >= Duration::from_millis(3900)
                    && before.elapsed() < Duration::from_secs(5)
            );
            assert_eq!(f.calls().await.len(), 2);
        })
        .await;
}

#[tokio::test]
async fn owned_gateway_capacity_cancel_releases_and_does_not_resume_network() {
    let f = OwnedPages::new();
    let c = cfg();
    let r = resources(&c).await;
    let start = |c: SearchConfig, r: SearchResources| {
        let transport = f.transport.clone();
        let hits = vec![f.result("/slow")];
        tokio::spawn(async move {
            OWNED_TRANSPORT
                .scope(transport, async move {
                    enrich(&c, &hits, Some(&r), Instant::now() + Duration::from_secs(8)).await
                })
                .await
        })
    };
    let a = start(c.clone(), r.clone());
    let b = start(c.clone(), r.clone());
    tokio::time::timeout(Duration::from_secs(3), async {
        while f.calls().await.len() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(r.page_text_permits.available_permits(), 0);
    let busy = OWNED_TRANSPORT
        .scope(
            f.transport.clone(),
            enrich(
                &c,
                &[f.result("/page")],
                Some(&r),
                Instant::now() + Duration::from_secs(8),
            ),
        )
        .await;
    assert_eq!(busy.report.state, "busy");
    assert_eq!(f.calls().await.len(), 2);
    a.abort();
    assert!(a.await.unwrap_err().is_cancelled());
    assert_eq!(r.page_text_permits.available_permits(), 1);
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{}/release", f.control))
        .send()
        .await
        .unwrap();
    assert_eq!(b.await.unwrap().excerpts.len(), 1);
    assert_eq!(r.page_text_permits.available_permits(), 2);
    assert_eq!(f.calls().await.len(), 2);
}

#[tokio::test]
async fn owned_source_wait_retry_after_storage_failure_and_origin_only_metadata() {
    let f = OwnedPages::new();
    let mut c = cfg();
    c.source_min_interval_ms = 3000;
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let r = SearchResources::with_database(c.clone(), db.clone());
    OWNED_TRANSPORT
        .scope(f.transport.clone(), async {
            let first = enrich(
                &c,
                &[f.result("/page?private-parameter=not-metadata")],
                Some(&r),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(first.excerpts.len(), 1);
            let next = enrich(
                &c,
                &[f.result("/two")],
                Some(&r),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(next.report.pages[0].state, "source_wait");
            assert_eq!(f.calls().await.len(), 1);
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT key,value FROM meta WHERE key LIKE 'search.free_cooldown.%'",
            )
            .fetch_all(db.pool())
            .await
            .unwrap();
            assert_eq!(rows.len(), 1);
            assert!(!rows[0].0.contains("owned-upstream"));
            assert!(!rows[0].1.contains("private-parameter"));
            assert!(!rows[0].1.contains("Owned"));
            assert!(rows[0]
                .0
                .starts_with("search.free_cooldown.v1.result_page."));
            sqlx::query("UPDATE meta SET value='corrupt' WHERE key=?")
                .bind(&rows[0].0)
                .execute(db.pool())
                .await
                .unwrap();
            let independent = SearchResources::with_database(c.clone(), db.clone());
            let corrupt = enrich(
                &c,
                &[f.result("/page")],
                Some(&independent),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(corrupt.report.pages[0].state, "source_state_unavailable");
            assert_eq!(f.calls().await.len(), 1);
            let db2 = crate::db::Db::connect_in_memory().await.unwrap();
            let retry_r = SearchResources::with_database(cfg(), db2.clone());
            let retry = enrich(
                &cfg(),
                &[f.result("/retry")],
                Some(&retry_r),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(retry.report.pages[0].state, "http_unavailable");
            let after = enrich(
                &cfg(),
                &[f.result("/page")],
                Some(&retry_r),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(after.report.pages[0].state, "source_wait");
            assert_eq!(f.calls().await.len(), 2);
            let stored: String = sqlx::query_scalar(
                "SELECT value FROM meta WHERE key LIKE 'search.free_cooldown.v1.result_page.%'",
            )
            .fetch_one(db2.pool())
            .await
            .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&stored).unwrap()["reason"],
                "retry_after"
            );
            db2.pool().close().await;
            let failed = enrich(
                &cfg(),
                &[f.result("/page")],
                Some(&SearchResources::with_database(cfg(), db2.clone())),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(failed.report.pages[0].state, "source_state_unavailable");
            assert_eq!(f.calls().await.len(), 2);
        })
        .await;
}

#[tokio::test]
async fn cached_query_prefetch_fetches_fresh_ephemeral_excerpts_without_persisting_them() {
    let f = OwnedPages::new();
    let mut c = cfg();
    c.query_cache_ttl_secs = 30;
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let r = SearchResources::with_database(c.clone(), db.clone());
    let user = "owned page current query";
    let query = super::super::backend::SearchQuery {
        text: crate::search::query_from(user, 256),
        max_results: c.normalized_max_results(),
    };
    let fingerprint = super::super::executor::resource_fingerprint(
        &c,
        &query,
        &super::super::free_aggregate::FreeAggregateBackend::default(),
    );
    let hits = vec![f.result("/page"), f.result("/two")];
    let metadata = super::super::executor::SearchOutcome {
        backend: c.backend,
        hits: 2,
        error: None,
        results: hits,
        result_sources: vec!["bing_rss".into()],
        failed_sources: vec![],
        reuse: None,
        retrieved_at_utc: None,
        page_text: None,
        requested_locale: None,
        unstructured_reference: None,
    };
    r.run(fingerprint, &c, move || async move { metadata })
        .await;
    OWNED_TRANSPORT
        .scope(f.transport.clone(), async {
            for count in [2, 4] {
                let mut messages = vec![crate::domain::Message::user(user)];
                let outcome = super::super::executor::prefetch_with_resources(
                    &reqwest::Client::new(),
                    &c,
                    Some("not-forwarded"),
                    &mut messages,
                    user,
                    &r,
                )
                .await;
                assert_eq!(
                    outcome.reuse,
                    Some(super::super::resources::SearchReuse::Cached)
                );
                assert_eq!(
                    outcome.page_text.as_ref().unwrap().pages[0].state,
                    "available"
                );
                let injected = serde_json::to_string(&messages).unwrap();
                assert!(injected.contains("Owned & visible"));
                assert!(injected.contains("不可信引用"));
                assert!(injected.contains("不是网页发布时间"));
                assert!(!serde_json::to_string(&outcome)
                    .unwrap()
                    .contains("Owned & visible"));
                assert_eq!(f.calls().await.len(), count);
            }
            let cached = r
                .run(fingerprint, &c, || async {
                    panic!("query metadata should stay cached")
                })
                .await;
            assert!(cached.page_text.is_none());
            assert!(!serde_json::to_string(&cached)
                .unwrap()
                .contains("Owned & visible"));
            let stored: Vec<String> = sqlx::query_scalar("SELECT value FROM meta")
                .fetch_all(db.pool())
                .await
                .unwrap();
            assert!(stored.iter().all(|v| !v.contains("Owned & visible")));
            let request_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(
                request_count, 0,
                "prefetch does not write audit or excerpts"
            );
        })
        .await;
}

#[tokio::test]
async fn owned_reservation_write_failure_and_origin_capacity_never_get() {
    let f = OwnedPages::new();
    let mut c = cfg();
    c.source_min_interval_ms = 1000;
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    sqlx::raw_sql("CREATE TRIGGER owned_page_commit_fail BEFORE INSERT ON meta BEGIN SELECT RAISE(ABORT,'owned page write failure'); END;").execute(db.pool()).await.unwrap();
    let r = SearchResources::with_database(c.clone(), db.clone());
    OWNED_TRANSPORT
        .scope(f.transport.clone(), async {
            let failed = enrich(
                &c,
                &[f.result("/page")],
                Some(&r),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(failed.report.pages[0].state, "source_state_unavailable");
            assert!(f.calls().await.is_empty());
            sqlx::raw_sql("DROP TRIGGER owned_page_commit_fail")
                .execute(db.pool())
                .await
                .unwrap();
            for n in 0..256 {
                sqlx::query("INSERT INTO meta(key,value,updated_at) VALUES(?,?,CURRENT_TIMESTAMP)")
                    .bind(format!("search.free_cooldown.v1.result_page.owned-{n}"))
                    .bind("unknown-version-do-not-evict")
                    .execute(db.pool())
                    .await
                    .unwrap();
            }
            let capped = SearchResources::with_database(cfg(), db.clone());
            let failed = enrich(
                &cfg(),
                &[f.result("/page")],
                Some(&capped),
                Instant::now() + Duration::from_secs(8),
            )
            .await;
            assert_eq!(failed.report.pages[0].state, "source_state_unavailable");
            assert!(f.calls().await.is_empty());
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM meta WHERE value='unknown-version-do-not-evict'",
            )
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert_eq!(count, 256);
        })
        .await;
}

#[tokio::test]
async fn known_physical_origins_reuse_existing_source_reservations() {
    let db = crate::db::Db::connect_in_memory().await.unwrap();
    let store = Cooldowns::new(db);
    for (source, base) in [
        (
            super::super::cooldown::Source::BingRss,
            "https://cn.bing.com",
        ),
        (
            super::super::cooldown::Source::Duckduckgo,
            "https://lite.duckduckgo.com",
        ),
        (
            super::super::cooldown::Source::NpmRegistry,
            "https://registry.npmjs.org",
        ),
        (
            super::super::cooldown::Source::HackerNews,
            "https://hn.algolia.com",
        ),
    ] {
        let target = Target::new(source, base).unwrap();
        assert!(store.reserve(&target, 1000).await.unwrap());
        let page = Target::result_page(
            &reqwest::Url::parse(&format!("{base}/arbitrary/path?not-a-new-source")).unwrap(),
        )
        .unwrap();
        assert!(!store.reserve_page(&page, 0).await.unwrap());
    }
    let trailing =
        Target::result_page(&reqwest::Url::parse("https://CN.BING.COM.:443/public").unwrap())
            .unwrap();
    assert!(
        !store.reserve_page(&trailing, 0).await.unwrap(),
        "trailing-dot spelling cannot bypass the same physical source wait"
    );
    let independent = Cooldowns::new(crate::db::Db::connect_in_memory().await.unwrap());
    let page = Target::result_page(&reqwest::Url::parse("https://hn.algolia.com/article").unwrap())
        .unwrap();
    assert!(independent.reserve_page(&page, 0).await.unwrap());
    assert!(!independent.reserve_page(&page, 0).await.unwrap());
}
