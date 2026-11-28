//! N3 evidence from two owned WAL files, fixed UTC periods and production reopen.
//! Holds are synthetic ledger fixtures, not proof of a physical upstream send or
//! supplier billing. The observation write guards cover six budget tables only.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use llm_gateway_lib::budget::{month_start_secs, observation};
use llm_gateway_lib::db::{repo, Db};
use llm_gateway_lib::domain::RemoteAccessKey;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{Connection, Row, SqliteConnection, SqlitePool};

const KEY: &str = "r157-same-key-in-independent-files";
const TABLES: [&str; 6] = [
    "remote_access_keys",
    "requests",
    "budget_holds",
    "budget_legacy_archive",
    "budget_key_history",
    "budget_archive_meta",
];
const PERIODS: [&str; 2] = ["2024-10-31T23:59:59Z", "2024-11-01T00:00:00Z"];

struct Fixture {
    label: &'static str,
    limit: i64,
    legacy: [Option<i64>; 2],
    settled: [i64; 2],
    pending: [i64; 2],
}

const A: Fixture = Fixture {
    label: "owned-A",
    limit: 100,
    legacy: [Some(11), Some(13)],
    settled: [7, 3],
    pending: [5, 2],
};
const B: Fixture = Fixture {
    label: "owned-B",
    limit: 200,
    legacy: [None, Some(2)],
    settled: [4, 8],
    pending: [9, 1],
};

fn at(period: usize) -> DateTime<Utc> {
    PERIODS[period].parse().unwrap()
}

fn identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

type SchemaRow = (String, String, String, i64, Option<String>);

#[derive(Debug, PartialEq)]
struct Stored {
    tables: Vec<Vec<Vec<String>>>,
    schema: Vec<SchemaRow>,
    schema_version: i64,
    user_version: i64,
    application_id: i64,
}

async fn stored(pool: &SqlitePool) -> Stored {
    let mut tables = Vec::new();
    for table in TABLES {
        let columns = sqlx::query(&format!("PRAGMA main.table_info({})", identifier(table)))
            .fetch_all(pool)
            .await
            .unwrap();
        assert!(!columns.is_empty());
        // typeof + quote preserves NULL, integer, real, text and blob distinctions;
        // every column is included, rather than comparing only row counts.
        let expressions: Vec<_> = columns
            .iter()
            .map(|column| {
                let name = identifier(&column.get::<String, _>("name"));
                format!("typeof({name}) || ':' || quote({name})")
            })
            .collect();
        let rows = sqlx::query(&format!(
            "SELECT {} FROM main.{}",
            expressions.join(","),
            identifier(table)
        ))
        .fetch_all(pool)
        .await
        .unwrap();
        let mut values: Vec<Vec<String>> = rows
            .iter()
            .map(|row| (0..columns.len()).map(|i| row.get(i)).collect())
            .collect();
        values.sort();
        tables.push(values);
    }
    Stored {
        tables,
        schema: sqlx::query_as(
            "SELECT type,name,tbl_name,rootpage,sql FROM main.sqlite_schema ORDER BY type,name",
        )
        .fetch_all(pool)
        .await
        .unwrap(),
        schema_version: sqlx::query_scalar("PRAGMA main.schema_version")
            .fetch_one(pool)
            .await
            .unwrap(),
        user_version: sqlx::query_scalar("PRAGMA main.user_version")
            .fetch_one(pool)
            .await
            .unwrap(),
        application_id: sqlx::query_scalar("PRAGMA main.application_id")
            .fetch_one(pool)
            .await
            .unwrap(),
    }
}

async fn physical_database(pool: &SqlitePool, path: &Path) -> PathBuf {
    let actual: String = sqlx::query("PRAGMA database_list")
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.get::<String, _>("name") == "main")
        .unwrap()
        .get("file");
    let actual = std::fs::canonicalize(actual).unwrap();
    assert_eq!(actual, std::fs::canonicalize(path).unwrap());
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(integrity, "ok");
    actual
}

async fn seed(db: &Db, fixture: &Fixture) {
    let origin = DateTime::from_timestamp(month_start_secs(at(0).timestamp()), 0).unwrap();
    assert!(
        at(1).timestamp() < Utc::now().timestamp() - 2 * 86_400,
        "fixed UTC fixtures must precede the production retention cutoff"
    );
    repo::create_remote_access_key(
        db.pool(),
        &RemoteAccessKey {
            id: KEY.into(),
            label: fixture.label.into(),
            key_hash: "r157-fixture-hash-never-a-real-credential".into(),
            enabled: true,
            rpm_limit: 20,
            monthly_budget_micros: fixture.limit,
            budget_currency: "usd".into(),
            allowed_models: vec![],
            created_at: origin,
            updated_at: origin,
        },
    )
    .await
    .unwrap();
    for period in 0..2 {
        sqlx::query("INSERT INTO requests(ts,requested_model,access_key_id,cost,currency) VALUES (?,'r157-legacy',?,?, 'usd')")
            .bind(at(period).timestamp())
            .bind(KEY)
            .bind(fixture.legacy[period].map(|v| v as f64 / 1_000_000.0))
            .execute(db.pool()).await.unwrap();
        for (state, reserved, reported) in [
            ("settled", 50, Some(fixture.settled[period])),
            ("dispatched", fixture.pending[period], None),
        ] {
            let id = format!("{}-{period}-{state}", fixture.label);
            sqlx::query("INSERT INTO budget_holds(hold_id,gateway_request_uuid,call_ordinal,key_id,admission_month,currency,state,reserved_micros,reported_reference_micros,target_price_json,created_at,updated_at) VALUES(?,?,0,?,?,'usd',?,?,?,'r157-private-target-fixture',?,?)")
                .bind(&id).bind(&id).bind(KEY).bind(month_start_secs(at(period).timestamp()))
                .bind(state).bind(reserved).bind(reported)
                .bind(at(period).timestamp()).bind(at(period).timestamp())
                .execute(db.pool()).await.unwrap();
        }
    }
    // This unkeyed, recent canary survives purge so UPDATE/DELETE guards on
    // requests have a real row to challenge after reopening. It is not spend.
    sqlx::query("INSERT INTO requests(ts,requested_model) VALUES (?,'r157-write-guard-canary')")
        .bind(Utc::now().timestamp())
        .execute(db.pool())
        .await
        .unwrap();
}

async fn observe(db: &Db, fixture: &Fixture, period: usize) -> Value {
    let before = stored(db.pool()).await;
    let started = Utc::now();
    let result = observation::read(db.pool(), KEY, at(period))
        .await
        .unwrap()
        .with_policy(true);
    assert_eq!(stored(db.pool()).await, before, "observation wrote storage");
    assert_eq!(result.key_id, KEY);
    assert_eq!(result.key_label, fixture.label);
    assert_eq!(result.limit_micros, fixture.limit.to_string());
    assert!(result.key_enabled && result.budget_enabled);
    assert_eq!(result.currency.as_deref(), Some("usd"));
    assert_eq!(
        result.key_updated_at.timestamp(),
        month_start_secs(at(0).timestamp())
    );
    assert_eq!(
        result.period_start_utc.timestamp(),
        month_start_secs(at(period).timestamp())
    );
    assert!(result.observed_at_utc >= started && result.observed_at_utc <= Utc::now());
    let (known, settled, pending, total, remaining) = match (fixture.label, period) {
        ("owned-A", 0) => (Some("18"), "7", "5", Some("23"), Some("77")),
        ("owned-A", 1) => (Some("16"), "3", "2", Some("18"), Some("82")),
        ("owned-B", 0) => (None, "4", "9", None, None),
        ("owned-B", 1) => (Some("10"), "8", "1", Some("11"), Some("189")),
        _ => panic!("unexpected fixture identity"),
    };
    assert_eq!(result.known_spend_micros.as_deref(), known);
    assert_eq!(result.ledger_settled_micros.as_deref(), Some(settled));
    assert_eq!(result.outstanding_micros.as_deref(), Some(pending));
    assert_eq!(result.outstanding_calls, Some(1));
    assert_eq!(result.other_currency_outstanding_calls, Some(0));
    assert_eq!(result.total_micros.as_deref(), total);
    assert_eq!(result.remaining_micros.as_deref(), remaining);
    assert_eq!(
        result.blocked_reason,
        total.is_none().then_some("legacy_cost_unknown")
    );
    assert_eq!(
        result.mode,
        if total.is_none() {
            "blocked_unknown"
        } else {
            "reference_headroom"
        }
    );
    let mut wire = serde_json::to_value(result).unwrap();
    let text = wire.to_string();
    assert!(!text.contains("r157-fixture-hash") && !text.contains("r157-private-target"));
    // The rest of the response must be identical before retention and after
    // production reopen; observed_at is the real read completion time.
    wire.as_object_mut().unwrap().remove("observed_at_utc");
    wire
}

async fn archive_twice(db: &Db) {
    repo::purge_old_requests(db.pool(), 1).await.unwrap();
    let first = stored(db.pool()).await;
    assert_eq!(first.tables[1].len(), 1, "only the unkeyed canary survives");
    assert_eq!(first.tables[2].len(), 4);
    assert_eq!(first.tables[3].len(), 2);
    repo::purge_old_requests(db.pool(), 1).await.unwrap();
    assert_eq!(stored(db.pool()).await, first, "purge must be idempotent");
}

async fn install_guards(db: &Db) {
    for table in TABLES {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let name = identifier(&format!("r157_guard_{table}_{event}"));
            sqlx::query(&format!(
                "CREATE TRIGGER main.{name} BEFORE {event} ON {} BEGIN SELECT RAISE(ABORT,'r157 guard {table} {event}'); END",
                identifier(table)
            )).execute(db.pool()).await.unwrap();
        }
    }
}

async fn challenge_guards(connection: &mut SqliteConnection) {
    let guards: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM main.sqlite_schema WHERE type='trigger' AND name GLOB 'r157_guard_*'",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(guards, 18);
    let temporary: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_temp_master")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(
        temporary, 0,
        "guards must not be connection-local TEMP triggers"
    );
    for table in TABLES {
        let name = identifier(table);
        let column: String = sqlx::query(&format!("PRAGMA main.table_info({name})"))
            .fetch_one(&mut *connection)
            .await
            .unwrap()
            .get("name");
        let column = identifier(&column);
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM main.{name}"))
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert!(
            count > 0,
            "a zero-row statement would not exercise the guard"
        );
        for (event, statement) in [
            ("INSERT", format!("INSERT INTO main.{name} SELECT * FROM main.{name} LIMIT 1")),
            ("UPDATE", format!("UPDATE main.{name} SET {column}={column} WHERE rowid=(SELECT rowid FROM main.{name} LIMIT 1)")),
            ("DELETE", format!("DELETE FROM main.{name} WHERE rowid=(SELECT rowid FROM main.{name} LIMIT 1)")),
        ] {
            let error = sqlx::query(&statement).execute(&mut *connection).await.unwrap_err();
            assert_eq!(error.as_database_error().unwrap().message(), format!("r157 guard {table} {event}"));
        }
    }
}

async fn verify_all_connections(db: &Db, path: &Path) {
    let before = stored(db.pool()).await;
    // Keep all eight leases simultaneously, forcing distinct physical pool
    // connections. A separate ninth connection must see the same main guards.
    let mut connections = Vec::new();
    for _ in 0..8 {
        connections.push(db.pool().acquire().await.unwrap());
    }
    for connection in &mut connections {
        challenge_guards(connection).await;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let mut fresh = SqliteConnection::connect_with(&options).await.unwrap();
    challenge_guards(&mut fresh).await;
    fresh.close().await.unwrap();
    drop(connections);
    assert_eq!(
        stored(db.pool()).await,
        before,
        "guard challenges changed storage"
    );
}

async fn close(db: Db) {
    db.pool().close().await;
    assert!(db.pool().is_closed());
    drop(db);
}

#[tokio::test]
async fn n3_two_wal_period_observations_survive_full_close_and_reopen() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let root = std::env::temp_dir().join(format!("llmgw-r157-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let a_path = root.join("a.db");
        let b_path = root.join("b.db");
        let a = Db::connect_path(&a_path).await.unwrap();
        let b = Db::connect_path(&b_path).await.unwrap();
        seed(&a, &A).await;
        seed(&b, &B).await;
        assert_ne!(physical_database(a.pool(), &a_path).await, physical_database(b.pool(), &b_path).await);
        assert!(root.join("a.db-wal").is_file() && root.join("b.db-wal").is_file());
        let expected = [observe(&a, &A, 0).await, observe(&b, &B, 0).await,
                        observe(&a, &A, 1).await, observe(&b, &B, 1).await];
        archive_twice(&a).await;
        archive_twice(&b).await;
        let a_stored = stored(a.pool()).await;
        let b_stored = stored(b.pool()).await;
        // No transaction, leased connection, cloned Db or secondary pool remains.
        close(a).await;
        close(b).await;
        let a = Db::connect_path(&a_path).await.unwrap();
        let b = Db::connect_path(&b_path).await.unwrap();
        assert_ne!(physical_database(a.pool(), &a_path).await, physical_database(b.pool(), &b_path).await);
        for (db, previous) in [(&a, a_stored), (&b, b_stored)] {
            let reopened = stored(db.pool()).await;
            assert_eq!(reopened.tables, previous.tables);
            assert_eq!(reopened.schema, previous.schema);
            assert_eq!(reopened.user_version, previous.user_version);
            assert_eq!(reopened.application_id, previous.application_id);
            // Production initialization recreates catalog triggers. Do not
            // mistake that legal schema_version change for an observation write.
            install_guards(db).await;
        }
        verify_all_connections(&a, &a_path).await;
        verify_all_connections(&b, &b_path).await;
        for index in [0, 1, 2, 3, 0, 1] {
            let (db, fixture) = if index % 2 == 0 { (&a, &A) } else { (&b, &B) };
            assert_eq!(observe(db, fixture, index / 2).await, expected[index]);
        }
        close(a).await;
        close(b).await;
        std::fs::remove_file(a_path).unwrap();
        std::fs::remove_file(b_path).unwrap();
        std::fs::remove_dir(&root).expect("all owned SQLite connections and sidecars must be released");
        eprintln!("R157_N3_WITNESS databases=2 wal=2 periods=2 fully_closed_reopened=2 observations=10 main_guards=36 rejected_writes=324 known_totals=23,18,11 unknown_total=null retained_unknown=9 cleanup=complete");
    }).await.expect("owned two-WAL observation test timed out");
}
