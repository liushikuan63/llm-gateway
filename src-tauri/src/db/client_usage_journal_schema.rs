//! Versioned CLIENT journal contract. No legacy-data promotion or total quota.
//! Existing startup migrations remain separate; this contract is atomic itself.
use std::collections::BTreeMap;

use futures_util::TryStreamExt;
use sqlx::{Row, SqliteConnection, SqlitePool};

const DDL: &str = include_str!("client_usage_journal_v2.sql");
const INITIALIZE: &str = include_str!("client_usage_journal_init_v2.sql");
pub const VERSION: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractMeta {
    pub db_generation: String,
    pub last_entry_generation: i64,
    pub activated_at: i64,
}

fn storage_error(_: sqlx::Error) -> anyhow::Error {
    anyhow::anyhow!("client_usage_journal_storage_unavailable")
}

fn uuid_valid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

// Match the whitespace tokens in the vendored SQLite tokenizer, rather than
// Rust's Unicode whitespace. For example, SQLite treats U+00A0 as an identifier character.
fn is_sqlite_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

/// Collapse SQLite whitespace outside literals without joining SQL tokens. Deleting
/// all whitespace would accept TEXTPRIMARYKEY in place of TEXT PRIMARY KEY.
fn canonical_sql(sql: &str) -> String {
    let mut result = String::new();
    let mut quote = None;
    let mut whitespace = false;
    let mut chars = sql
        .trim_matches(is_sqlite_space)
        .trim_end_matches(';')
        .chars()
        .peekable();
    while let Some(ch) = chars.next() {
        if let Some(delimiter) = quote {
            result.push(ch);
            if ch == delimiter {
                if chars.peek() == Some(&delimiter) {
                    result.push(chars.next().unwrap_or(delimiter));
                } else {
                    quote = None;
                }
            }
        } else if is_sqlite_space(ch) {
            whitespace = true;
        } else {
            if whitespace && !result.is_empty() {
                result.push(' ');
            }
            whitespace = false;
            if ch == '\'' || ch == '"' {
                quote = Some(ch);
            }
            result.push(ch);
        }
    }
    result
}

fn definitions() -> anyhow::Result<BTreeMap<String, String>> {
    let mut objects = BTreeMap::new();
    let mut name: Option<String> = None;
    let mut statement = String::new();
    for line in DDL.lines().chain(std::iter::once("-- object: __end")) {
        if let Some(next) = line.strip_prefix("-- object: ") {
            if let Some(previous) = name.take() {
                anyhow::ensure!(
                    !statement.trim().is_empty()
                        && objects.insert(previous, statement.clone()).is_none(),
                    "client_usage_journal_definition_invalid"
                );
            }
            statement.clear();
            anyhow::ensure!(
                next.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "client_usage_journal_definition_invalid"
            );
            name = Some(next.to_owned());
        } else if name.is_some() {
            statement.push_str(line);
            statement.push('\n');
        }
    }
    anyhow::ensure!(
        objects.len() == 17,
        "client_usage_journal_definition_invalid"
    );
    Ok(objects)
}

pub(crate) async fn inspect_conn(
    connection: &mut SqliteConnection,
) -> anyhow::Result<Option<ContractMeta>> {
    let expected = definitions()?;
    // Names come only from the checked, shipped definition markers above.
    // Include every trigger/index attached to these tables, including unknown
    // names, so an additional side-effecting trigger is not silently accepted.
    let names = expected
        .keys()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(",");
    // SQLite resolves identifiers case-insensitively for ASCII. Discover
    // collisions before legacy migration, then require exact names and DDL below.
    let query = format!("SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL AND (tbl_name COLLATE NOCASE IN ('client_usage_journal_meta_v2','usage_attempt_journal_v2','usage_attempt_versions_v2') OR name COLLATE NOCASE IN ({names}))");
    let rows = sqlx::query(&query)
        .fetch_all(&mut *connection)
        .await
        .map_err(storage_error)?;
    if rows.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        rows.len() == expected.len(),
        "client_usage_journal_partial_contract"
    );
    for row in rows {
        let name: String = row.try_get("name").map_err(storage_error)?;
        let sql: String = row.try_get("sql").map_err(storage_error)?;
        let kind: String = row.try_get("type").map_err(storage_error)?;
        let definition = expected
            .get(&name)
            .ok_or_else(|| anyhow::anyhow!("client_usage_journal_unexpected_object"))?;
        let expected_kind = if definition.trim_start().starts_with("CREATE TABLE") {
            "table"
        } else if definition.trim_start().starts_with("CREATE INDEX") {
            "index"
        } else if definition.trim_start().starts_with("CREATE TRIGGER") {
            "trigger"
        } else {
            anyhow::bail!("client_usage_journal_definition_invalid")
        };
        anyhow::ensure!(
            kind == expected_kind && canonical_sql(&sql) == canonical_sql(definition),
            "client_usage_journal_schema_mismatch"
        );
    }
    let invalid_meta: i64 = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM client_usage_journal_meta_v2 WHERE typeof(singleton)<>'integer' OR singleton<>1 OR typeof(contract_version)<>'integer' OR contract_version<>2 OR typeof(db_generation)<>'text' OR length(db_generation)<>36 OR typeof(last_entry_generation)<>'integer' OR last_entry_generation<0 OR typeof(activated_at)<>'integer' OR activated_at<0)")
        .fetch_one(&mut *connection).await.map_err(storage_error)?;
    anyhow::ensure!(invalid_meta == 0, "client_usage_journal_invalid_metadata");
    let rows = sqlx::query("SELECT singleton,contract_version,db_generation,last_entry_generation,activated_at FROM client_usage_journal_meta_v2 LIMIT 2")
        .fetch_all(&mut *connection).await.map_err(storage_error)?;
    anyhow::ensure!(rows.len() == 1, "client_usage_journal_invalid_metadata");
    let row = &rows[0];
    let singleton: i64 = row.try_get("singleton").map_err(storage_error)?;
    let version: i64 = row.try_get("contract_version").map_err(storage_error)?;
    let meta = ContractMeta {
        db_generation: row.try_get("db_generation").map_err(storage_error)?,
        last_entry_generation: row
            .try_get("last_entry_generation")
            .map_err(storage_error)?,
        activated_at: row.try_get("activated_at").map_err(storage_error)?,
    };
    anyhow::ensure!(
        singleton == 1
            && version == VERSION
            && uuid_valid(&meta.db_generation)
            && meta.last_entry_generation >= 0
            && meta.activated_at >= 0
            && chrono::DateTime::from_timestamp(meta.activated_at, 0).is_some(),
        "client_usage_journal_invalid_metadata"
    );
    let last: Option<i64> =
        sqlx::query_scalar("SELECT MAX(entry_generation) FROM usage_attempt_versions_v2")
            .fetch_one(&mut *connection)
            .await
            .map_err(storage_error)?;
    anyhow::ensure!(
        last.map_or(true, |last| last > 0 && last <= meta.last_entry_generation),
        "client_usage_journal_invalid_generation"
    );
    Ok(Some(meta))
}

async fn validate_rows(connection: &mut SqliteConnection) -> anyhow::Result<()> {
    let inconsistent: i64 = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM usage_attempt_journal_v2 j LEFT JOIN usage_attempt_versions_v2 v ON j.attempt_id=v.attempt_id WHERE v.attempt_id IS NULL OR v.present<>1) OR EXISTS(SELECT 1 FROM usage_attempt_versions_v2 v LEFT JOIN usage_attempt_journal_v2 j ON j.attempt_id=v.attempt_id WHERE (v.present=1 AND j.attempt_id IS NULL) OR (v.present=0 AND j.attempt_id IS NOT NULL) OR typeof(v.attempt_id)<>'text' OR length(v.attempt_id)<>36 OR typeof(v.entry_generation)<>'integer' OR v.entry_generation<=0 OR typeof(v.revision)<>'integer' OR v.revision<=0 OR typeof(v.present)<>'integer' OR v.present NOT IN (0,1))")
        .fetch_one(&mut *connection).await.map_err(storage_error)?;
    anyhow::ensure!(
        inconsistent == 0,
        "client_usage_journal_inconsistent_versions"
    );
    // CHECKs constrain future ordinary writers, but SQLite can open a file
    // previously written with ignore_check_constraints. Validate retained data
    // too; merely matching sqlite_master does not establish valid evidence.
    let invalid: i64 = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM usage_attempt_journal_v2 WHERE typeof(attempt_id)<>'text' OR length(attempt_id)<>36 OR typeof(scope_id)<>'text' OR length(scope_id)<>36 OR kind IS NULL OR kind NOT IN ('buffered','stream','auxiliary','probe','passthrough','compaction','summary','agent_intent','native_exact') OR typeof(created_at)<>'integer' OR created_at<0 OR typeof(known_not_sent)<>'integer' OR known_not_sent NOT IN (0,1) OR (agent_gate_reason IS NOT NULL AND (agent_gate_reason NOT IN ('quota','concurrency') OR kind<>'agent_intent' OR known_not_sent<>1)) OR (reviewed_at IS NOT NULL AND (typeof(reviewed_at)<>'integer' OR reviewed_at<0 OR kind<>'agent_intent' OR known_not_sent<>1)) OR (audit_request_id IS NOT NULL AND (typeof(audit_request_id)<>'integer' OR audit_request_id<=0)) OR (background_scope_id IS NOT NULL AND typeof(background_scope_id)<>'text') OR (audit_request_id IS NOT NULL AND background_scope_id IS NOT NULL))")
        .fetch_one(&mut *connection).await.map_err(storage_error)?;
    anyhow::ensure!(invalid == 0, "client_usage_journal_invalid_row");
    // Startup validation streams retained IDs; unlimited retention is not an
    // excuse to materialize the whole journal or to discard tombstones.
    let mut rows = sqlx::query(
        "SELECT attempt_id,scope_id,created_at,reviewed_at FROM usage_attempt_journal_v2",
    )
    .fetch(&mut *connection);
    while let Some(row) = rows.try_next().await.map_err(storage_error)? {
        let id: String = row.try_get("attempt_id").map_err(storage_error)?;
        let scope: String = row.try_get("scope_id").map_err(storage_error)?;
        let created: i64 = row.try_get("created_at").map_err(storage_error)?;
        let reviewed: Option<i64> = row.try_get("reviewed_at").map_err(storage_error)?;
        anyhow::ensure!(
            uuid_valid(&id)
                && uuid_valid(&scope)
                && chrono::DateTime::from_timestamp(created, 0).is_some()
                && reviewed.map_or(true, |stamp| chrono::DateTime::from_timestamp(stamp, 0)
                    .is_some()),
            "client_usage_journal_invalid_row"
        );
    }
    drop(rows);
    let mut rows =
        sqlx::query("SELECT attempt_id FROM usage_attempt_versions_v2").fetch(&mut *connection);
    while let Some(row) = rows.try_next().await.map_err(storage_error)? {
        let id: String = row.try_get("attempt_id").map_err(storage_error)?;
        anyhow::ensure!(uuid_valid(&id), "client_usage_journal_invalid_row");
    }
    Ok(())
}

pub async fn preflight(pool: &SqlitePool) -> anyhow::Result<()> {
    // One snapshot avoids false inconsistency across concurrent WAL writers.
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    if inspect_conn(&mut transaction).await?.is_some() {
        validate_rows(&mut transaction).await?;
    }
    transaction.commit().await.map_err(storage_error)
}

pub async fn metadata(pool: &SqlitePool) -> anyhow::Result<ContractMeta> {
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    let meta = inspect_conn(&mut transaction)
        .await?
        .ok_or_else(|| anyhow::anyhow!("client_usage_journal_contract_unavailable"))?;
    transaction.commit().await.map_err(storage_error)?;
    Ok(meta)
}

pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_error)?;
    let work: anyhow::Result<()> = async {
        if inspect_conn(&mut transaction).await?.is_none() {
            // Keep dependency order (tables before indexes/triggers). The map
            // used for preflight compares names and must never order creation.
            sqlx::raw_sql(DDL)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            sqlx::query(INITIALIZE)
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(chrono::Utc::now().timestamp())
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
        }
        anyhow::ensure!(
            inspect_conn(&mut transaction).await?.is_some(),
            "client_usage_journal_creation_incomplete"
        );
        validate_rows(&mut transaction).await
    }
    .await;
    match work {
        Ok(()) => transaction
            .commit()
            .await
            .map_err(|_| anyhow::anyhow!("client_usage_journal_commit_unknown")),
        Err(error) => {
            transaction
                .rollback()
                .await
                .map_err(|_| anyhow::anyhow!("client_usage_journal_rollback_unknown"))?;
            Err(error)
        }
    }
}
