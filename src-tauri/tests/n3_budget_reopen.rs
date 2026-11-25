//! Owned loopback and WAL evidence for N3 reference accounting across audit retention.
//! Uses an invented access key and prices. It makes no supplier billing claim.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::response::Response;
use axum::Router;
use chrono::{DateTime, Utc};
use llm_gateway_lib::budget::{month_start_secs, observation};
use llm_gateway_lib::config::{AppConfig, RemoteModeConfig};
use llm_gateway_lib::db::{repo, Db};
use llm_gateway_lib::domain::{Provider, RemoteAccessKey};
use llm_gateway_lib::proxy::dedup::DedupMode;
use llm_gateway_lib::proxy::server::GatewayState;
use llm_gateway_lib::proxy_control::gateway::GatewayService;
use reqwest::StatusCode;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const KEY_ID: &str = "n3-reopen-key";
const KEY_SECRET: &str = "fixture-only-n3-reopen-remote-key";
const PRIMARY: &str = "n3-primary-physical";
const BACKUP: &str = "n3-backup-physical";
const LIMIT: i64 = 3;
const RESERVED: i64 = 2;

#[derive(Debug)]
struct Post {
    method: String,
    path: String,
    body: Value,
    sent_done: bool,
}

struct Loopback {
    primary_done: bool,
    posts: Mutex<Vec<Post>>,
}

struct OwnedTask(JoinHandle<()>);

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn upstream(State(owned): State<Arc<Loopback>>, req: Request) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_owned();
    let bytes = to_bytes(req.into_body(), 262_144).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let done = owned.primary_done || body["model"] == BACKUP;
    owned.posts.lock().unwrap().push(Post {
        method,
        path,
        body,
        sent_done: done,
    });
    // Both branches report the same usage and finish_reason. Only the final
    // protocol terminator proves that the primary stream finished.
    let wire = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\n{}",
        json!({"choices":[{"index":0,"delta":{"content":"owned partial"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":0,"completion_tokens":1,"total_tokens":1}}),
        if done { "data: [DONE]\n\n" } else { "" },
    );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(wire))
        .unwrap()
}

async fn provision(pool: &SqlitePool, addr: SocketAddr) {
    let now = Utc::now();
    for (id, priority, physical) in [("primary", 0, PRIMARY), ("backup", 100, BACKUP)] {
        let provider: Provider = serde_json::from_value(json!({
            "id":id,"name":id,"dialect":"openai","base_url":format!("http://{addr}"),
            "api_key_enc":"","enabled":true,"priority":priority,"rpm_limit":0,"intelligence":50,
            "created_at":now,"updated_at":now,
            "models":[{"enabled":true,"alias":"owned-model","upstream":physical,"context_window":32768,
                "supports_tools":false,"supports_vision":false,"supports_audio":false,
                "supports_video":false,"supports_thinking":false,"supports_stream":true,"model_type":"chat",
                "price":{"prompt":0.0,"completion":1.0,"currency":"usd"}}]
        }))
        .unwrap();
        repo::upsert_provider(pool, &provider).await.unwrap();
    }
    repo::create_remote_access_key(
        pool,
        &RemoteAccessKey {
            id: KEY_ID.into(),
            label: "owned N3 reopen".into(),
            key_hash: format!("{:x}", Sha256::digest(KEY_SECRET)),
            enabled: true,
            rpm_limit: 100,
            monthly_budget_micros: LIMIT,
            budget_currency: "USD".into(),
            allowed_models: Vec::new(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
}

async fn start_gateway(db: &Db) -> (GatewayService, SocketAddr) {
    let mut cfg = AppConfig {
        port: 0,
        unified_key: "fixture-only-n3-reopen-local-key".into(),
        remote_mode: RemoteModeConfig {
            enabled: true,
            public_url: Some("https://gateway.example.test".into()),
        },
        ..Default::default()
    };
    cfg.cache.enabled = false;
    cfg.search.enabled = false;
    cfg.smart_routing.enabled = false;
    cfg.dedup.mode = DedupMode::Off;
    cfg.budget.enabled = true;
    // Both requests specify the physical route they need. A retry must never
    // turn a truncated first POST into the positive control.
    cfg.failover_enabled = false;
    let state = Arc::new(GatewayState::new(db.clone(), cfg));
    state.reload_providers().await.unwrap();
    let service = GatewayService::new(state);
    let status = service.set_enabled(true).await.unwrap();
    assert!(status.running && status.last_error.is_none(), "{status:?}");
    let addr: SocketAddr = status.bound_addr.unwrap().parse().unwrap();
    assert_eq!(addr.ip(), Ipv4Addr::LOCALHOST);
    (service, addr)
}

async fn close_gateway(service: GatewayService, db: Db) {
    let status = service.set_enabled(false).await.unwrap();
    assert!(!status.running && status.bound_addr.is_none(), "{status:?}");
    assert!(status.last_error.is_none(), "{status:?}");
    drop(service);
    db.pool().close().await;
    assert!(db.pool().is_closed());
}

async fn send(
    client: &reqwest::Client,
    addr: SocketAddr,
    model: &str,
    session: &str,
) -> (StatusCode, Option<String>, String) {
    let response = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .header("x-forwarded-for", "198.51.100.7")
        .header("x-forwarded-proto", "https")
        .header("x-session-id", session)
        .bearer_auth(KEY_SECRET)
        .json(&json!({
            "model":model,"messages":[{"role":"user","content":"owned N3"}],
            "stream":true,"max_tokens":RESERVED
        }))
        .send()
        .await
        .unwrap();
    let trace = response
        .headers()
        .get("x-trace-id")
        .map(|value| value.to_str().unwrap().to_owned());
    let status = response.status();
    (status, trace, response.text().await.unwrap())
}

async fn linked_audit(pool: &SqlitePool, trace: &str) -> SqliteRow {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = sqlx::query(
                "SELECT r.id,r.status,r.access_key_id,h.hold_id,h.state,h.admission_month,\
                 h.reserved_micros,h.reported_reference_micros FROM requests r \
                 JOIN budget_holds h ON h.audit_request_id=r.id WHERE r.trace_id=?",
            )
            .bind(trace)
            .fetch_all(pool)
            .await
            .unwrap();
            assert!(rows.len() <= 1, "one trace linked to multiple holds");
            if let Some(row) = rows.into_iter().next() {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("own stream's audit/hold link was not written")
}

async fn counts(pool: &SqlitePool) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM requests),\
         (SELECT COUNT(*) FROM budget_legacy_archive),(SELECT COUNT(*) FROM budget_holds)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn observe(pool: &SqlitePool, at: DateTime<Utc>, complete: bool) -> observation::Snapshot {
    let snapshot = observation::read(pool, KEY_ID, at)
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(snapshot.currency.as_deref(), Some("usd"));
    assert_eq!(snapshot.limit_micros, LIMIT.to_string());
    assert_eq!(snapshot.period_start_utc, at);
    assert_eq!(snapshot.blocked_reason, None);
    assert_eq!(snapshot.mode, "reference_headroom");
    let expected = if complete {
        (Some("1"), Some("0"), Some(0), Some("1"), Some("2"))
    } else {
        (Some("0"), Some("2"), Some(1), Some("2"), Some("1"))
    };
    assert_eq!(
        (
            snapshot.ledger_settled_micros.as_deref(),
            snapshot.outstanding_micros.as_deref(),
            snapshot.outstanding_calls,
            snapshot.total_micros.as_deref(),
            snapshot.remaining_micros.as_deref(),
        ),
        expected,
    );
    snapshot
}

fn check_posts(owned: &Loopback, complete: bool, backup_allowed: bool) -> usize {
    let posts = owned.posts.lock().unwrap();
    let expected = if backup_allowed {
        vec![(PRIMARY, complete), (BACKUP, true)]
    } else {
        vec![(PRIMARY, complete)]
    };
    assert_eq!(posts.len(), expected.len(), "{posts:?}");
    for (post, (physical, done)) in posts.iter().zip(expected) {
        assert_eq!((&*post.method, &*post.path), ("POST", "/chat/completions"));
        assert_eq!(post.body["model"], physical);
        assert_eq!(post.body["stream"], true);
        assert_eq!(post.body["max_tokens"], RESERVED);
        assert_eq!(post.sent_done, done);
    }
    posts.len()
}

async fn clean_file(path: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match std::fs::remove_file(path) {
            Ok(()) => break,
            Err(error)
                if cfg!(windows)
                    && matches!(error.raw_os_error(), Some(32 | 33))
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("could not remove owned SQLite file: {error}"),
        }
    }
    std::fs::remove_dir(path.parent().unwrap()).expect("owned SQLite sidecars were not released");
}

async fn case(complete: bool) -> bool {
    let owned = Arc::new(Loopback {
        primary_done: complete,
        posts: Mutex::new(Vec::new()),
    });
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new().fallback(upstream).with_state(owned.clone());
    let upstream = OwnedTask(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();

    let dir = std::env::temp_dir().join(format!("llmgw-n3-reopen-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("owned.sqlite3");
    let db = Db::connect_path(&path).await.unwrap();
    provision(db.pool(), addr).await;
    let (gateway, origin) = start_gateway(&db).await;
    let (status, trace, body) =
        send(&client, origin, "primary:owned-model", "before-retention").await;
    assert_eq!(status, StatusCode::OK, "stream headers precede EOF: {body}");
    assert!(body.contains("owned partial"), "{body}");
    assert_eq!(body.contains("data: [DONE]"), complete, "{body}");
    let original_posts = check_posts(&owned, complete, false);
    let row = linked_audit(db.pool(), trace.as_deref().unwrap()).await;
    let audit = row.get::<i64, _>("status");
    let hold_state = row.get::<String, _>("state");
    let reported = complete.then_some(1i64);
    assert_eq!(audit, if complete { 200 } else { 502 });
    assert_eq!(hold_state, if complete { "settled" } else { "dispatched" });
    assert_eq!(row.get::<String, _>("access_key_id"), KEY_ID);
    assert_eq!(row.get::<i64, _>("reserved_micros"), RESERVED);
    assert_eq!(
        row.get::<Option<i64>, _>("reported_reference_micros"),
        reported
    );
    assert_eq!(counts(db.pool()).await, (1, 0, 1));

    let month = row.get::<i64, _>("admission_month");
    let month_start = DateTime::from_timestamp(month, 0).unwrap();
    let hold_id = row.get::<String, _>("hold_id");
    let before = observe(db.pool(), month_start, complete).await;
    // Expire only the audit row. The hold still belongs to its actual UTC
    // admission month, and an already linked request is never legacy spend.
    let aged = sqlx::query("UPDATE requests SET ts=? WHERE id=?")
        .bind(Utc::now().timestamp() - 2 * 86_400)
        .bind(row.get::<i64, _>("id"))
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(aged.rows_affected(), 1);
    for _ in 0..2 {
        repo::purge_old_requests(db.pool(), 1).await.unwrap();
        assert_eq!(counts(db.pool()).await, (0, 0, 1));
    }
    close_gateway(gateway, db).await;

    assert!(path.is_file(), "must reopen the original owned SQLite file");
    let reopened = Db::connect_path(&path).await.unwrap();
    let pool = reopened.pool();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(pool)
        .await
        .unwrap();
    let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(integrity, "ok");
    assert_eq!(counts(pool).await, (0, 0, 1));
    let saved: (String, i64, Option<i64>, Option<i64>, i64, String) = sqlx::query_as(
        "SELECT state,reserved_micros,reported_reference_micros,audit_request_id,admission_month,key_id \
         FROM budget_holds WHERE hold_id=?",
    )
    .bind(&hold_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        saved,
        (
            hold_state.clone(),
            RESERVED,
            reported,
            None,
            month,
            KEY_ID.into()
        )
    );
    let after = observe(pool, month_start, complete).await;
    assert_eq!(after.total_micros, before.total_micros);
    assert_eq!(after.remaining_micros, before.remaining_micros);

    let (gateway, origin) = start_gateway(&reopened).await;
    let (next, trace, body) = send(&client, origin, "backup:owned-model", "after-reopen").await;
    // A request dispatched during a UTC month rollover spends from a different
    // budget. Discard this pair and re-run once with a new owned file.
    let same_month = month_start_secs(Utc::now().timestamp()) == month;
    let new_posts = if !same_month {
        None
    } else {
        if complete {
            assert_eq!(next, StatusCode::OK, "{body}");
            assert!(body.contains("data: [DONE]"), "{body}");
            let second = linked_audit(pool, trace.as_deref().unwrap()).await;
            assert_ne!(second.get::<String, _>("hold_id"), hold_id);
            assert_eq!(second.get::<String, _>("state"), "settled");
            assert_eq!(
                second.get::<Option<i64>, _>("reported_reference_micros"),
                Some(1)
            );
        } else {
            assert_eq!(next, StatusCode::TOO_MANY_REQUESTS, "{body}");
            let error: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(error["error"]["code"], "budget_exceeded", "{body}");
            assert_eq!(counts(pool).await, (0, 0, 1));
        }
        Some(check_posts(&owned, complete, complete) - original_posts)
    };
    close_gateway(gateway, reopened).await;
    upstream.0.abort();
    drop(client);
    clean_file(&path).await;
    if let Some(new_posts) = new_posts {
        eprintln!(
            "N3_REOPEN_WITNESS terminal={} audit={audit} hold={hold_state} retained_micros={} remaining_micros={} post_reopen_http={} new_upstream_posts={new_posts}",
            if complete { "complete" } else { "truncated" },
            after.total_micros.as_deref().unwrap(),
            after.remaining_micros.as_deref().unwrap(),
            next.as_u16(),
        );
    }
    same_month
}

#[tokio::test]
async fn n3_stream_budget_survives_retention_and_wal_reopen() {
    tokio::time::timeout(Duration::from_secs(75), async {
        for complete in [true, false] {
            if !case(complete).await {
                assert!(
                    case(complete).await,
                    "UTC month changed during both owned runs"
                );
            }
        }
    })
    .await
    .expect("owned N3 WAL and loopback deadline");
}
