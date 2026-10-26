//! Real existing HTTPS fetch factory, isolated loopback and in-memory test CA.
use super::*;
use base64::Engine;
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct OwnedFeed {
    child: Child,
    ready: Value,
}
impl Drop for OwnedFeed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl OwnedFeed {
    fn ready(child: &mut Child) -> Value {
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send((result, line));
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok((Ok(n), line)) if n > 0 => {
                serde_json::from_str(&line).expect("owned feed readiness JSON")
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned feed readiness failed; no private pipe contents logged");
            }
        }
    }
    fn new(mode: &str) -> Self {
        assert_eq!(MAX_FEED_BYTES, 24 * 1024 * 1024);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut generator = Command::new("pwsh")
            .args(["-NoLogo", "-NoProfile", "-File"])
            .arg(root.join("tests/support/n4-owned-tls.ps1"))
            .creation_flags(0x08000000)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut owned = Self::ready(&mut generator);
        assert!(generator.wait().unwrap().success());
        owned["mode"] = json!(mode);
        owned["max_bytes"] = json!(MAX_FEED_BYTES);
        let mut child = Command::new("node")
            .arg(
                root.parent()
                    .unwrap()
                    .join("scripts/price-feed-owned-tls.cjs"),
            )
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            let mut pipe = child.stdin.take().unwrap();
            serde_json::to_writer(&mut pipe, &owned).unwrap();
            pipe.write_all(b"\n").unwrap();
        }
        let ready = Self::ready(&mut child);
        Self { child, ready }
    }
    fn transport(&self) -> (reqwest::Certificate, std::net::SocketAddr) {
        let der = base64::engine::general_purpose::STANDARD
            .decode(self.ready["root_der"].as_str().unwrap())
            .unwrap();
        (
            reqwest::Certificate::from_der(&der).unwrap(),
            std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                self.ready["origin"].as_u64().unwrap() as u16,
            )),
        )
    }
    fn url(&self) -> String {
        format!(
            "https://owned-upstream.invalid:{}/feed",
            self.ready["origin"]
        )
    }
    async fn state(&self) -> Value {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://127.0.0.1:{}/state", self.ready["control"]))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn stop(&mut self) {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://127.0.0.1:{}/stop", self.ready["control"]))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let end = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(exit) = self.child.try_wait().unwrap() {
                assert!(exit.success());
                break;
            }
            assert!(tokio::time::Instant::now() < end);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

#[tokio::test]
async fn n4_price_feed_oversized_unfinished_body_rejects_at_existing_limit() {
    let mut owned = OwnedFeed::new("over_hold");
    let url = owned.url();
    let result = OWNED_FEED_TEST_TRANSPORT
        .scope(
            owned.transport(),
            tokio::time::timeout(Duration::from_secs(3), fetch_feed(&url, None)),
        )
        .await;
    let actual = owned.state().await;
    assert_eq!(actual["requests"], 1);
    assert_eq!(actual["authorization_headers"], 0);
    assert_eq!(actual["error"], "");
    assert_eq!(actual["decoded_fixture_bytes"], MAX_FEED_BYTES + 1);
    assert_eq!(
        actual["flushed"], true,
        "owned oversize bytes were not flushed; not a product body-limit proof"
    );
    assert!(matches!(&result,Ok(Err(message)) if message.contains("响应过大")),"existing24MiB cap must reject before unfinished body EOF/20s timeout: result={result:?},owned={actual}");
    let end = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if owned.state().await["response_closed"] == 1 {
            break;
        }
        assert!(tokio::time::Instant::now() < end);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    owned.stop().await;
    eprintln!("Owned price HTTPS oversize: MAX+1 flushed, early size error, response closed, no auth or external DNS");
}

#[tokio::test]
async fn n4_price_feed_normal_and_exact_decoded_limit_remain_accepted() {
    for mode in ["normal", "exact"] {
        let mut owned = OwnedFeed::new(mode);
        let url = owned.url();
        let feed = OWNED_FEED_TEST_TRANSPORT
            .scope(owned.transport(), fetch_feed(&url, None))
            .await
            .unwrap();
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].id, "owned-price-model");
        assert_eq!(feed[0].price.prompt, 1.0);
        assert_eq!(feed[0].price.completion, 2.0);
        let actual = owned.state().await;
        assert_eq!(actual["requests"], 1);
        assert_eq!(actual["authorization_headers"], 0);
        assert_eq!(actual["error"], "");
        if mode == "exact" {
            assert_eq!(actual["decoded_fixture_bytes"], MAX_FEED_BYTES);
        }
        owned.stop().await;
        // leak-check: allow static per-token unit description; no token value or credential is logged.
        eprintln!("Owned price HTTPS accepted: mode={mode},feed1,original per-token unit conversion retained");
    }
}

#[tokio::test]
async fn n4_price_feed_decompressed_size_and_non_success_header_keep_original_policy() {
    for mode in ["gzip_over", "status_hold"] {
        let mut owned = OwnedFeed::new(mode);
        let url = owned.url();
        let result = OWNED_FEED_TEST_TRANSPORT
            .scope(
                owned.transport(),
                tokio::time::timeout(Duration::from_secs(3), fetch_feed(&url, None)),
            )
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            result.contains(if mode == "gzip_over" {
                "响应过大"
            } else {
                "HTTP 503"
            }),
            "{result}"
        );
        let actual = owned.state().await;
        assert_eq!(actual["requests"], 1);
        assert_eq!(actual["authorization_headers"], 0);
        assert_eq!(actual["error"], "");
        if mode == "gzip_over" {
            assert!(actual["wire_offered"].as_u64().unwrap() < MAX_FEED_BYTES as u64);
            assert_eq!(actual["decoded_fixture_bytes"], MAX_FEED_BYTES + 1);
        }
        owned.stop().await;
        eprintln!(
            "Owned price HTTPS rejected: mode={mode},body/status original policy,child exit0"
        );
    }
}
