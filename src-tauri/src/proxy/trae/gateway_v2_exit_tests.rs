//! Forced exits around the final native cancellation audit transaction, after
//! the original V2 dispatch intent has committed and been observed on the wire.

use super::*;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteOperation},
    Connection, Row, SqliteConnection, SqlitePool,
};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const ROLE: &str = "LLMGW_NATIVE_CANCEL_V2_EXIT_ROLE";
const NONCE: &str = "LLMGW_NATIVE_CANCEL_V2_EXIT_NONCE";
const TRACE: &str = "LLMGW_NATIVE_CANCEL_V2_EXIT_TRACE";
const EXACT: &str =
    "proxy::server::native_exact::tests::v2_exit_tests::owned_native_cancel_exit_preserves_atomic_v2_audit";
const PROMPT: i64 = 37;

fn fixture_root(nonce: &str) -> PathBuf {
    let id = uuid::Uuid::parse_str(nonce).expect("expected a test-owned UUID");
    assert_eq!(nonce, id.simple().to_string());
    std::env::temp_dir().join(format!("llmgw-personal-test-native-cancel-v2-{nonce}"))
}

#[derive(Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct CommitWrites {
    request: Option<i64>,
    intent: Option<i64>,
    versions: bool,
    daily: bool,
}

async fn gate_commit(
    pool: &SqlitePool,
) -> (
    tokio::sync::oneshot::Receiver<CommitWrites>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered, observation) = tokio::sync::oneshot::channel();
    let entered = Arc::new(parking_lot::Mutex::new(Some(entered)));
    let (release, wait) = std::sync::mpsc::channel();
    let wait = Arc::new(parking_lot::Mutex::new(wait));
    let mut connections = Vec::new();
    // Each hook is local to its handle. Hold every pooled connection so the
    // writer cannot select an uninstrumented one once the client disconnects.
    assert_eq!(pool.options().get_max_connections(), 8);
    for _ in 0..pool.options().get_max_connections() {
        connections.push(pool.acquire().await.unwrap());
    }
    for conn in &mut connections {
        let pending = Arc::new(parking_lot::Mutex::new(CommitWrites::default()));
        let updates = pending.clone();
        let rollback = pending.clone();
        let entered = entered.clone();
        let wait = wait.clone();
        let mut handle = conn.lock_handle().await.unwrap();
        handle.set_update_hook(move |update| {
            if update.database == "main" {
                match (update.table, update.operation) {
                    ("requests", SqliteOperation::Insert) => {
                        updates.lock().request = Some(update.rowid);
                    }
                    ("usage_attempt_journal_v2", SqliteOperation::Update) => {
                        updates.lock().intent = Some(update.rowid);
                    }
                    ("usage_attempt_versions_v2", SqliteOperation::Update) => {
                        updates.lock().versions = true;
                    }
                    ("usage_daily", SqliteOperation::Insert | SqliteOperation::Update) => {
                        updates.lock().daily = true;
                    }
                    _ => (),
                }
            }
        });
        handle.set_rollback_hook(move || {
            *rollback.lock() = CommitWrites::default();
        });
        handle.set_commit_hook(move || {
            let staged = std::mem::take(&mut *pending.lock());
            if staged.request.is_none() {
                return true;
            }
            if let Some(entered) = entered.lock().take() {
                if entered.send(staged).is_ok() {
                    // This hook runs on the SQLx SQLite worker at real COMMIT.
                    // Without release, unwinding the fixture rolls it back.
                    return wait.lock().recv().is_ok();
                }
            }
            false
        });
    }
    (observation, release)
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
struct Intent {
    row_id: i64,
    attempt_id: String,
    scope_id: String,
    kind: String,
    created_at: i64,
    known_not_sent: i64,
    agent_gate_reason: Option<String>,
    reviewed_at: Option<i64>,
    audit_request_id: Option<i64>,
    background_scope_id: Option<String>,
    db_generation: String,
    last_entry_generation: i64,
    entry_generation: i64,
    revision: i64,
    present: i64,
}

async fn read_db(path: &Path) -> SqliteConnection {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true)
        .pragma("query_only", "ON");
    SqliteConnection::connect_with(&options).await.unwrap()
}

async fn read_intent_rows(reader: &mut SqliteConnection) -> Vec<Intent> {
    const ROWS: &str = "SELECT j.rowid AS row_id, j.attempt_id, j.scope_id, j.kind, j.created_at, \
        j.known_not_sent, j.agent_gate_reason, j.reviewed_at, j.audit_request_id, j.background_scope_id, \
        m.db_generation, m.last_entry_generation, v.entry_generation, v.revision, v.present \
        FROM usage_attempt_journal_v2 j JOIN usage_attempt_versions_v2 v ON v.attempt_id=j.attempt_id \
        CROSS JOIN client_usage_journal_meta_v2 m";
    sqlx::query_as::<_, Intent>(ROWS)
        .fetch_all(&mut *reader)
        .await
        .unwrap()
}

#[derive(Clone)]
pub(super) struct PostIntentObserver {
    path: PathBuf,
    observations: Arc<parking_lot::Mutex<Vec<Vec<Intent>>>>,
}

impl PostIntentObserver {
    pub(super) async fn observe(&self) {
        // This is an independent query-only connection at loopback POST entry.
        let mut reader = read_db(&self.path).await;
        let rows = read_intent_rows(&mut reader).await;
        reader.close().await.unwrap();
        self.observations.lock().push(rows);
    }
}

async fn read_intent(reader: &mut SqliteConnection) -> Intent {
    let mut rows = read_intent_rows(reader).await;
    assert_eq!(rows.len(), 1, "the dispatch must have one journal version");
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM usage_attempt_journal_v2),             (SELECT COUNT(*) FROM usage_attempt_versions_v2),             (SELECT COUNT(*) FROM client_usage_journal_meta_v2)",
    )
    .fetch_one(&mut *reader)
    .await
    .unwrap();
    assert_eq!(counts, (1, 1, 1), "no duplicate intent or hidden tombstone");
    let intent = rows.pop().unwrap();
    assert_eq!(intent.kind, "native_exact");
    assert_eq!(intent.known_not_sent, 0);
    assert_eq!(intent.agent_gate_reason, None);
    assert_eq!(intent.reviewed_at, None);
    assert_eq!(intent.background_scope_id, None);
    assert_eq!(intent.present, 1);
    assert_eq!(intent.last_entry_generation, 1);
    assert_eq!(intent.entry_generation, 1);
    assert!(intent.created_at > 0 && intent.row_id > 0);
    for id in [&intent.attempt_id, &intent.scope_id, &intent.db_generation] {
        assert_eq!(uuid::Uuid::parse_str(id).unwrap().to_string(), *id);
    }
    intent
}

type DailyRow = (String, String, String, String, i64, i64, i64, i64, f64);

async fn check_wal(path: &Path, trace: &str, initial: &Intent, link: Option<i64>) -> Value {
    let mut reader = read_db(path).await;
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&mut reader)
        .await
        .unwrap();
    let integrity: Vec<String> = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_all(&mut reader)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(integrity, ["ok"]);
    let mut recovered = read_intent(&mut reader).await;
    assert_eq!(initial.audit_request_id, None);
    assert_eq!(initial.revision, 1);
    assert_eq!(recovered.audit_request_id, link);
    assert_eq!(
        recovered.revision,
        initial.revision + i64::from(link.is_some())
    );
    let journal = serde_json::to_value(&recovered).unwrap();
    recovered.audit_request_id = None;
    recovered.revision = initial.revision;
    assert_eq!(
        &recovered, initial,
        "the original intent and its scope must survive"
    );

    let requests = sqlx::query(
        "SELECT id, ts, trace_id, status, error, routed_provider, routed_model, prompt_tokens,          completion_tokens, fallback_attempts, cost, currency, rate_label, attempts_json FROM requests",
    )
    .fetch_all(&mut reader)
    .await
    .unwrap();
    let daily: Vec<DailyRow> = sqlx::query_as(
        "SELECT day, provider_id, model, currency, requests, prompt_tokens, completion_tokens,          errors, cost FROM usage_daily",
    )
    .fetch_all(&mut reader)
    .await
    .unwrap();
    reader.close().await.unwrap();

    let mut evidence = Value::Null;
    if let Some(rowid) = link {
        assert_eq!(requests.len(), 1);
        let audit = &requests[0];
        assert_eq!(audit.get::<i64, _>("id"), rowid);
        assert_eq!(audit.get::<&str, _>("trace_id"), trace);
        assert_eq!(audit.get::<i64, _>("status"), 499);
        assert_eq!(audit.get::<&str, _>("error"), "native_client_cancelled");
        assert_eq!(audit.get::<&str, _>("routed_provider"), "native-provider");
        assert_eq!(audit.get::<&str, _>("routed_model"), "Exact-A");
        assert_eq!(audit.get::<i64, _>("prompt_tokens"), PROMPT);
        assert_eq!(audit.get::<i64, _>("completion_tokens"), 0);
        assert_eq!(audit.get::<i64, _>("fallback_attempts"), 0);
        assert_eq!(audit.get::<Option<f64>, _>("cost"), None);
        assert_eq!(audit.get::<Option<String>, _>("currency"), None);
        assert_eq!(audit.get::<Option<String>, _>("rate_label"), None);
        let attempts: Vec<Value> =
            serde_json::from_str(audit.get::<&str, _>("attempts_json")).unwrap();
        assert_eq!(attempts.len(), 1);
        let attempt = &attempts[0];
        assert_eq!(attempt["trace_id"], trace);
        assert_eq!(attempt["reason"], "native_client_cancelled");
        assert_eq!(attempt["ok"], false);
        assert_eq!(attempt["retryable"], false);
        assert!(attempt["timing"].is_null());
        assert!(attempt["generation"].is_null());
        let native = &attempt["native_exact"];
        assert_eq!(native["upstream_post_started"], true);
        assert_eq!(native["scalar_usage_placeholder"], true);
        assert_eq!(native["price_status"], "unknown");
        assert!(native["price_evidence"].is_null());
        let usage = native["usage_evidence"].as_object().unwrap();
        assert_eq!(usage["state"], "partial");
        assert_eq!(usage["prompt_tokens"], PROMPT);
        assert_eq!(usage.get("completion_tokens"), Some(&Value::Null));
        assert_eq!(usage.get("total_tokens"), Some(&Value::Null));
        evidence = native["usage_evidence"].clone();
        let day = chrono::DateTime::from_timestamp(audit.get::<i64, _>("ts"), 0)
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(
            daily,
            [(
                day,
                "native-provider".into(),
                "Exact-A".into(),
                String::new(),
                1,
                PROMPT,
                0,
                1,
                0.0
            )]
        );
    } else {
        assert!(
            requests.is_empty(),
            "uncommitted 499 audit must not become visible"
        );
        assert!(
            daily.is_empty(),
            "uncommitted daily usage must not become visible"
        );
    }
    json!({
        "journal": mode, "quick_check": integrity, "intent": journal,
        "request_rows": requests.len(), "usage_rows": daily.len(), "trace": trace,
        "status": link.map(|_| 499), "usage_evidence": evidence
    })
}

async fn run_child(role: &str) {
    assert!(matches!(role, "before_commit" | "after_commit"));
    let nonce = std::env::var(NONCE).unwrap();
    let trace = std::env::var(TRACE).unwrap();
    let dir = fixture_root(&nonce).join(role);
    assert_eq!(
        std::env::current_dir().unwrap().canonicalize().unwrap(),
        dir.canonicalize().unwrap()
    );
    assert_eq!(
        std::env::var_os("LLMGW_DATA_DIR"),
        Some(dir.join("data").into_os_string())
    );
    let owner: Value =
        serde_json::from_slice(&std::fs::read(dir.join("owner.json")).unwrap()).unwrap();
    assert_eq!(
        owner,
        json!({"fixture":"native-cancel-exit-v2", "role":role, "nonce":nonce, "trace":trace})
    );
    let path = dir.join("owned.sqlite3");
    assert!(!path.exists(), "expected a fresh owned WAL database");

    let t = setup_with_database(false, &path).await;
    let observations = Arc::new(parking_lot::Mutex::new(Vec::new()));
    *t.fixture.v2_intent_observer.lock() = Some(PostIntentObserver {
        path: path.clone(),
        observations: observations.clone(),
    });
    let (entered, release) = gate_commit(t.state.db.pool()).await;
    let sentinel = native_exit_sentinel("s2-v2-owned-visible", &trace);
    t.fixture.stall.store(true, Ordering::SeqCst);
    // Usage is decoded before the distinct visible line. There is no terminal
    // frame, so the resulting audit must originate from the downstream drop.
    t.fixture.packets.lock().push_back(
        frame("token_usage", json!({"prompt_tokens":PROMPT}))
            + &frame("output", json!({"response":sentinel})),
    );
    let mut body = request();
    body["stream"] = json!(true);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        client
            .post(format!("{}/v1/chat/completions", t.url))
            .bearer_auth("fixture-gateway-key")
            .header("x-trace-id", &trace)
            .header("connection", "close")
            .json(&body)
            .send(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-trace-id"], trace.as_str());
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut reader = response.bytes_stream();
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut received = Vec::new();
        loop {
            received.extend_from_slice(&reader.next().await.unwrap().unwrap());
            assert!(received.len() <= 64 * 1024);
            let wire = std::str::from_utf8(&received).unwrap();
            assert!(!wire.contains("[DONE]"));
            if client_logical_channels(wire).get("content") == Some(&sentinel) {
                break;
            }
        }
    })
    .await
    .expect("client must see the owned native sentinel before dropping");
    assert_eq!(t.fixture.calls.load(Ordering::SeqCst), 1);
    {
        let seen = t.fixture.requests.lock();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            (seen[0].0.as_str(), seen[0].1.as_str()),
            ("POST", MODEL_PATH)
        );
    }
    let mut independent = read_db(&path).await;
    let initial = read_intent(&mut independent).await;
    independent.close().await.unwrap();
    assert_eq!(
        observations.lock().as_slice(),
        [vec![initial.clone()]],
        "the same committed revision-1 intent must already be visible at upstream POST entry"
    );
    check_wal(&path, &trace, &initial, None).await;

    drop(reader);
    let writes = tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .expect("the cancelled SSE must reach the real COMMIT hook")
        .unwrap();
    let rowid = writes.request.unwrap();
    assert!(rowid > 0);
    assert_eq!(writes.intent, Some(initial.row_id));
    assert!(
        writes.versions && writes.daily,
        "same COMMIT must stage the V2 revision and daily bucket"
    );

    let link = if role == "after_commit" {
        release.send(()).unwrap();
        t.state.drain_audits(Duration::from_secs(5)).await.unwrap();
        Some(rowid)
    } else {
        None
    };
    let witness = check_wal(&path, &trace, &initial, link).await;
    let receipt = json!({
        "role":role, "nonce":nonce, "trace":trace, "pid":std::process::id(),
        "sentinel":sentinel, "initial":initial, "writes":writes, "witness":witness
    });
    let temporary = dir.join("ready.tmp");
    std::fs::write(&temporary, receipt.to_string()).unwrap();
    std::fs::rename(&temporary, dir.join("ready.json")).unwrap();
    // Keep the listener, pool and (in the before case) the actual blocked
    // writer alive until the parent has independently verified the receipt.
    std::future::pending::<()>().await;
    drop((release, t, client));
}

#[tokio::test]
async fn owned_native_cancel_exit_preserves_atomic_v2_audit() {
    if let Ok(role) = std::env::var(ROLE) {
        run_child(&role).await;
        return;
    }
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let root = fixture_root(&nonce);
    std::fs::create_dir(&root).unwrap();
    eprintln!("owned native v2 cancel exit fixture: {}", root.display());
    for role in ["before_commit", "after_commit"] {
        let trace = crate::trace::new_trace_id();
        let dir = root.join(role);
        std::fs::create_dir(&dir).unwrap();
        let owner =
            json!({"fixture":"native-cancel-exit-v2", "role":role, "nonce":nonce, "trace":trace});
        std::fs::write(dir.join("owner.json"), owner.to_string()).unwrap();
        let log = dir.join("child.log");
        let output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log)
            .unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = NativeExitChild(
            command
                .args(["--exact", EXACT, "--nocapture"])
                .current_dir(&dir)
                .env(ROLE, role)
                .env(NONCE, &nonce)
                .env(TRACE, &trace)
                .env("LLMGW_DATA_DIR", dir.join("data"))
                .env("NO_PROXY", "127.0.0.1,localhost")
                .stdin(Stdio::null())
                .stderr(output.try_clone().unwrap())
                .stdout(output)
                .spawn()
                .unwrap(),
        );
        let ready_path = dir.join("ready.json");
        let ready: Value = tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "owned {role} fixture exited before receipt: {}",
                    log.display()
                );
                match std::fs::read(&ready_path) {
                    Ok(bytes) => break serde_json::from_slice(&bytes).unwrap(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => panic!("owned {role} fixture receipt failed: {error}"),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("owned {role} fixture timed out: {}", log.display()));
        assert_eq!(ready["role"], role);
        assert_eq!(ready["nonce"], nonce);
        assert_eq!(ready["trace"], trace);
        assert_eq!(ready["pid"], child.0.id());
        assert_eq!(
            ready["sentinel"],
            native_exit_sentinel("s2-v2-owned-visible", &trace)
        );
        let initial: Intent = serde_json::from_value(ready["initial"].clone()).unwrap();
        let writes: CommitWrites = serde_json::from_value(ready["writes"].clone()).unwrap();
        let rowid = writes.request.unwrap();
        assert!(rowid > 0 && writes.versions && writes.daily);
        assert_eq!(writes.intent, Some(initial.row_id));
        let link = (role == "after_commit").then_some(rowid);
        let db = dir.join("owned.sqlite3");
        let live = check_wal(&db, &trace, &initial, link).await;
        assert_eq!(live, ready["witness"]);
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "owned fixture no longer running"
        );
        child.0.kill().unwrap();
        let status = child.0.wait().unwrap();
        assert!(!status.success(), "expected forced process exit");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9));
        }
        let reopened = check_wal(&db, &trace, &initial, link).await;
        assert_eq!(
            reopened, live,
            "the same WAL state must survive forced exit"
        );
        eprintln!(
            "owned native v2 cancel exit {role}: pid={} exit={status} rowid={rowid} {reopened}",
            ready["pid"]
        );
    }
}
