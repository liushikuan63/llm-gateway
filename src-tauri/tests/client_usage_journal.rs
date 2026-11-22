//! CLIENT v2 contract startup: only newly owned SQLite fixtures, no accounts.
use llm_gateway_lib::db::{migrations, Db};

#[tokio::test]
async fn startup_installs_new_versioned_journal_and_preserves_identity_on_repeat() {
    let db = Db::connect_in_memory().await.unwrap();
    let engine: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        engine, "3.46.0",
        "run the project vendor engine, not Python SQLite"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE tbl_name IN ('client_usage_journal_meta_v2','usage_attempt_journal_v2','usage_attempt_versions_v2') AND name NOT LIKE 'sqlite_autoindex_%'")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        count, 17,
        "startup must install the complete journal contract"
    );
    let before: (i64, String, i64) = sqlx::query_as("SELECT contract_version,db_generation,last_entry_generation FROM client_usage_journal_meta_v2")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(before.0, 2);
    assert_eq!(before.2, 0);
    assert_eq!(
        uuid::Uuid::parse_str(&before.1).unwrap().to_string(),
        before.1
    );
    migrations::run(db.pool()).await.unwrap();
    let after: (i64, String, i64) = sqlx::query_as("SELECT contract_version,db_generation,last_entry_generation FROM client_usage_journal_meta_v2")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        after, before,
        "repeat initialization must not reset identity or counters"
    );
    let journal: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempt_journal_v2")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        journal, 0,
        "old audit history is never promoted to v2 evidence"
    );
    db.pool().close().await;
}

#[tokio::test]
async fn partial_v2_contract_is_rejected_before_legacy_migration_writes() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE client_usage_journal_meta_v2(singleton INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        migrations::run(&pool).await.is_err(),
        "partial contract cannot be masked by DDL replay"
    );
    let legacy: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name='requests'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        legacy, 0,
        "known-bad new schema must stop before old writes"
    );
    pool.close().await;
}

#[tokio::test]
async fn case_variants_of_v2_objects_are_rejected_before_legacy_migration_writes() {
    for ddl in [
        "CREATE TABLE Usage_Attempt_Journal_V2(sentinel TEXT)",
        "CREATE TABLE owned_stub(sentinel TEXT); CREATE INDEX IDX_USAGE_ATTEMPT_JOURNAL_V2_UNLINKED ON owned_stub(sentinel)",
    ] {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(ddl).execute(&pool).await.unwrap();

        let error = migrations::run(&pool).await.unwrap_err();
        let legacy: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name='requests'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            legacy, 0,
            "case-variant V2 object must be rejected before legacy writes: {ddl}"
        );
        assert_eq!(error.to_string(), "client_usage_journal_partial_contract");
        pool.close().await;
    }
}

use journal::{AgentGateReason, AttemptKind};
use llm_gateway_lib::db::{
    client_usage_journal_schema as schema, repo::client_usage_journal as journal,
};
const DDL: &str = include_str!("../src/db/client_usage_journal_v2.sql");
const INIT: &str = include_str!("../src/db/client_usage_journal_init_v2.sql");

fn scope() -> String {
    uuid::Uuid::new_v4().to_string()
}
async fn gate(db: &Db) -> String {
    let id = journal::record_intent(db, &scope(), AttemptKind::AgentIntent)
        .await
        .unwrap();
    journal::mark_agent_gate(db, &id, AgentGateReason::Quota)
        .await
        .unwrap();
    id
}
async fn version(db: &Db, id: &str) -> (i64, i64, i64) {
    sqlx::query_as("SELECT entry_generation,revision,present FROM usage_attempt_versions_v2 WHERE attempt_id=?")
        .bind(id).fetch_one(db.pool()).await.unwrap()
}
async fn reviewed(db: &Db, id: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT reviewed_at FROM usage_attempt_journal_v2 WHERE attempt_id=?")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn list_reviewable_selects_only_unreviewed_known_agent_gates_without_writes() {
    let db = Db::connect_in_memory().await.unwrap();
    let quota = gate(&db).await;
    let concurrency = journal::record_intent(&db, &scope(), AttemptKind::AgentIntent)
        .await
        .unwrap();
    journal::mark_agent_gate(&db, &concurrency, AgentGateReason::Concurrency)
        .await
        .unwrap();
    journal::record_intent(&db, &scope(), AttemptKind::AgentIntent)
        .await
        .unwrap();
    journal::record_intent(&db, &scope(), AttemptKind::Buffered)
        .await
        .unwrap();
    let done = gate(&db).await;
    let pending = journal::prepare_review(&db, std::slice::from_ref(&done))
        .await
        .unwrap();
    journal::confirm_review(&db, pending).await.unwrap();
    let deleted = gate(&db).await;
    sqlx::query("DELETE FROM usage_attempt_journal_v2 WHERE attempt_id=?")
        .bind(&deleted)
        .execute(db.pool())
        .await
        .unwrap();
    let before:Vec<(String,i64,i64,i64)>=sqlx::query_as("SELECT attempt_id,entry_generation,revision,present FROM usage_attempt_versions_v2 ORDER BY attempt_id").fetch_all(db.pool()).await.unwrap();
    let meta:(i64,String,i64,i64)=sqlx::query_as("SELECT contract_version,db_generation,activated_at,last_entry_generation FROM client_usage_journal_meta_v2").fetch_one(db.pool()).await.unwrap();
    sqlx::query("PRAGMA query_only=ON")
        .execute(db.pool())
        .await
        .unwrap();
    let page = journal::list_reviewable(&db, None).await.unwrap();
    assert_eq!(page.schema_version, 2);
    assert_eq!(page.items.len(), 2);
    assert!(page.next_after.is_none());
    assert!(page
        .items
        .iter()
        .any(|item| item.attempt_id == quota && item.gate_reason == AgentGateReason::Quota));
    assert!(page
        .items
        .iter()
        .any(|item| item.attempt_id == concurrency
            && item.gate_reason == AgentGateReason::Concurrency));
    let after:Vec<(String,i64,i64,i64)>=sqlx::query_as("SELECT attempt_id,entry_generation,revision,present FROM usage_attempt_versions_v2 ORDER BY attempt_id").fetch_all(db.pool()).await.unwrap();
    assert_eq!(after, before);
    let after_meta:(i64,String,i64,i64)=sqlx::query_as("SELECT contract_version,db_generation,activated_at,last_entry_generation FROM client_usage_journal_meta_v2").fetch_one(db.pool()).await.unwrap();
    assert_eq!(after_meta, meta);
    db.pool().close().await;
}

#[tokio::test]
async fn list_reviewable_uses_keyset_pages_without_a_total_cap_and_rejects_bad_cursors() {
    let db = Db::connect_in_memory().await.unwrap();
    let mut ids = Vec::new();
    for _ in 0..52 {
        ids.push(gate(&db).await);
    }
    ids.sort();
    let first = journal::list_reviewable(&db, None).await.unwrap();
    assert_eq!(first.items.len(), 50);
    assert_eq!(first.next_after.as_ref(), Some(&ids[49]));
    let second = journal::list_reviewable(&db, first.next_after.as_deref())
        .await
        .unwrap();
    assert_eq!(second.items.len(), 2);
    assert!(second.next_after.is_none());
    let all: Vec<_> = first
        .items
        .iter()
        .chain(second.items.iter())
        .map(|item| item.attempt_id.clone())
        .collect();
    assert_eq!(all, ids);
    assert!(journal::list_reviewable(&db, Some(&ids[51]))
        .await
        .unwrap()
        .items
        .is_empty());
    for cursor in [
        "not-a-uuid",
        "00000000-0000-0000-0000-000000000000",
        "11111111-1111-4111-8111-AAAAAAAAAAAA",
        "{11111111-1111-4111-8111-aaaaaaaaaaaa}",
    ] {
        assert!(journal::list_reviewable(&db, Some(cursor))
            .await
            .unwrap_err()
            .to_string()
            .contains("client_usage_journal_invalid_cursor"));
    }
    sqlx::query("DELETE FROM usage_attempt_journal_v2 WHERE attempt_id IN (?,?)")
        .bind(&ids[50])
        .bind(&ids[51])
        .execute(db.pool())
        .await
        .unwrap();
    let exactly = journal::list_reviewable(&db, None).await.unwrap();
    assert_eq!(exactly.items.len(), 50);
    assert!(exactly.next_after.is_none());
    db.pool().close().await;
}
fn definition(name: &str) -> &str {
    let marker = format!("-- object: {name}");
    DDL.split(&marker)
        .nth(1)
        .unwrap()
        .trim_start()
        .split("-- object:")
        .next()
        .unwrap()
}
async fn corrupt_counter(db: &Db, value: i64) {
    sqlx::query("DROP TRIGGER client_usage_meta_v2_update_guard")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE client_usage_journal_meta_v2 SET last_entry_generation=?")
        .bind(value)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::raw_sql(definition("client_usage_meta_v2_update_guard"))
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn schema_token_literal_missing_and_extra_objects_are_rejected_without_legacy_writes() {
    let bad_literal = DDL.replace(
        "client_usage_meta_delete_denied",
        "client_usage_meta_delete_allowed",
    );
    let token = DDL.replacen(
        "attempt_id TEXT PRIMARY KEY NOT NULL",
        "attempt_id TEXTPRIMARYKEY NOT NULL",
        1,
    );
    // The old whitespace-deleting comparator would accept this loss of PK.
    let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    assert_eq!(compact(DDL), compact(&token));
    for (label, ddl, extra) in [
        ("token", token, ""),
        ("literal", bad_literal, ""),
        ("missing", DDL.to_owned(), "DROP TRIGGER usage_attempt_journal_v2_update_guard"),
        ("extra", DDL.to_owned(), "CREATE TRIGGER unexpected_side_effect AFTER INSERT ON usage_attempt_journal_v2 BEGIN SELECT 1; END"),
    ] {
        let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(&ddl).execute(&pool).await.unwrap();
        sqlx::query(INIT).bind(scope()).bind(1_i64).execute(&pool).await.unwrap();
        sqlx::raw_sql("CREATE TABLE requests(sentinel TEXT); INSERT INTO requests VALUES('owned-before-preflight')").execute(&pool).await.unwrap();
        if !extra.is_empty() { sqlx::raw_sql(extra).execute(&pool).await.unwrap(); }
        if label == "token" {
            let pk: i64 = sqlx::query_scalar("SELECT pk FROM pragma_table_info('usage_attempt_journal_v2') WHERE name='attempt_id'").fetch_one(&pool).await.unwrap();
            assert_eq!(pk, 0, "negative fixture really lost its PK on vendor SQLite");
        }
        let before: Vec<(String,String)> = sqlx::query_as("SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name").fetch_all(&pool).await.unwrap();
        assert!(migrations::run(&pool).await.is_err(), "must reject {label}");
        let after: Vec<(String,String)> = sqlx::query_as("SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name").fetch_all(&pool).await.unwrap();
        assert_eq!(after, before, "no DDL repair or legacy writes for {label}");
        let sentinel: String = sqlx::query_scalar("SELECT sentinel FROM requests").fetch_one(&pool).await.unwrap();
        assert_eq!(sentinel, "owned-before-preflight");
        pool.close().await;
    }
}

#[tokio::test]
async fn sqlite_identifier_whitespace_cannot_mask_missing_primary_key() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let malformed = DDL.replacen(
        "attempt_id TEXT PRIMARY KEY NOT NULL",
        "attempt_id TEXT PRIMARY\u{00a0}KEY NOT NULL",
        1,
    );
    sqlx::raw_sql(&malformed).execute(&pool).await.unwrap();
    sqlx::query(INIT)
        .bind(scope())
        .bind(1_i64)
        .execute(&pool)
        .await
        .unwrap();
    let pk: i64 = sqlx::query_scalar(
        "SELECT pk FROM pragma_table_info('usage_attempt_journal_v2') WHERE name='attempt_id'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pk, 0, "the malformed table must lack its primary key");

    let result = migrations::run(&pool).await;
    let legacy: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name='requests'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(legacy, 0, "missing V2 primary key must block legacy writes");
    assert_eq!(
        result.unwrap_err().to_string(),
        "client_usage_journal_schema_mismatch"
    );
    pool.close().await;
}

#[tokio::test]
async fn identical_schema_does_not_hide_invalid_retained_rows() {
    for change in [
        "kind='invalid'",
        "known_not_sent=2",
        "created_at=-1",
        "created_at=9223372036854775807",
        "reviewed_at=-1",
        "kind='buffered',reviewed_at=1,known_not_sent=1",
        "agent_gate_reason='bad'",
        "audit_request_id=-1",
        "audit_request_id=1,background_scope_id='background'",
        "scope_id='xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'",
        "scope_id='00000000-0000-0000-0000-000000000000'",
        "scope_id='ABCDEF00-0000-4000-8000-000000000001'",
    ] {
        let db = Db::connect_in_memory().await.unwrap();
        let id = gate(&db).await;
        sqlx::query("PRAGMA ignore_check_constraints=ON")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query(&format!(
            "UPDATE usage_attempt_journal_v2 SET {change} WHERE attempt_id=?"
        ))
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints=OFF")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(
            schema::preflight(db.pool()).await.is_err(),
            "reject retained {change}"
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempt_journal_v2")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            count, 1,
            "invalid evidence is retained, never silently cleaned"
        );
        assert_eq!(version(&db, &id).await.2, 1);
        db.pool().close().await;
    }
}

#[tokio::test]
async fn invalid_metadata_uuid_is_rejected_on_same_full_schema() {
    for id in [
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "00000000-0000-0000-0000-000000000000",
        "ABCDEF00-0000-4000-8000-000000000001",
    ] {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(DDL).execute(&pool).await.unwrap();
        sqlx::query(INIT)
            .bind(id)
            .bind(1_i64)
            .execute(&pool)
            .await
            .unwrap();
        assert!(schema::preflight(&pool).await.is_err());
        let retained: String =
            sqlx::query_scalar("SELECT db_generation FROM client_usage_journal_meta_v2")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(retained, id);
        pool.close().await;
    }
}

#[tokio::test]
async fn review_success_reads_after_trigger_version_and_retains_gate_evidence() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = gate(&db).await;
    let before = version(&db, &id).await;
    let pending = journal::prepare_review(&db, std::slice::from_ref(&id))
        .await
        .unwrap();
    assert_eq!(pending.items()[0].attempt_id, id);
    assert_eq!(pending.items()[0].gate_reason, AgentGateReason::Quota);
    let clone = db.clone();
    assert_eq!(journal::confirm_review(&clone, pending).await.unwrap(), 1);
    assert!(reviewed(&db, &id).await.is_some());
    assert_eq!(version(&db, &id).await, (before.0, before.1 + 1, 1));
    assert!(journal::prepare_review(&db, std::slice::from_ref(&id))
        .await
        .is_err());
    let row: (i64,String,Option<i64>) = sqlx::query_as("SELECT known_not_sent,agent_gate_reason,audit_request_id FROM usage_attempt_journal_v2 WHERE attempt_id=?").bind(&id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(row, (1, "quota".into(), None));
    db.pool().close().await;
}

#[tokio::test]
async fn review_rejects_unknown_and_duplicate_selection() {
    let db = Db::connect_in_memory().await.unwrap();
    let unknown = journal::record_intent(&db, &scope(), AttemptKind::AgentIntent)
        .await
        .unwrap();
    assert!(journal::prepare_review(&db, std::slice::from_ref(&unknown))
        .await
        .is_err());
    assert!(journal::prepare_review(&db, &[]).await.is_err());
    let id = gate(&db).await;
    assert!(journal::prepare_review(&db, &[id.clone(), id.clone()])
        .await
        .is_err());
    assert!(reviewed(&db, &id).await.is_none());
    let known: i64 = sqlx::query_scalar(
        "SELECT known_not_sent FROM usage_attempt_journal_v2 WHERE attempt_id=?",
    )
    .bind(unknown)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        known, 0,
        "review cannot convert an unknown dispatch to not-sent"
    );
    db.pool().close().await;
}

#[tokio::test]
async fn same_value_update_and_a_b_a_reject_old_confirmation() {
    let db = Db::connect_in_memory().await.unwrap();
    for aba in [false, true] {
        let id = gate(&db).await;
        let before = version(&db, &id).await;
        let pending = journal::prepare_review(&db, std::slice::from_ref(&id))
            .await
            .unwrap();
        if aba {
            sqlx::query("UPDATE usage_attempt_journal_v2 SET agent_gate_reason='concurrency' WHERE attempt_id=?").bind(&id).execute(db.pool()).await.unwrap();
        }
        sqlx::query(
            "UPDATE usage_attempt_journal_v2 SET agent_gate_reason='quota' WHERE attempt_id=?",
        )
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
        assert_eq!(
            version(&db, &id).await.1,
            before.1 + if aba { 2 } else { 1 }
        );
        assert_eq!(
            journal::confirm_review(&db, pending)
                .await
                .unwrap_err()
                .to_string(),
            "client_usage_journal_state_conflict"
        );
        assert!(reviewed(&db, &id).await.is_none());
    }
    db.pool().close().await;
}

#[tokio::test]
async fn delete_reinsert_preserves_tombstone_and_rejects_old_confirmation() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = gate(&db).await;
    let pending = journal::prepare_review(&db, std::slice::from_ref(&id))
        .await
        .unwrap();
    let item = pending.items().remove(0);
    let before = version(&db, &id).await;
    sqlx::query("DELETE FROM usage_attempt_journal_v2 WHERE attempt_id=?")
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(version(&db, &id).await, (before.0, before.1 + 1, 0));
    assert!(
        sqlx::query("DELETE FROM usage_attempt_versions_v2 WHERE attempt_id=?")
            .bind(&id)
            .execute(db.pool())
            .await
            .is_err()
    );
    sqlx::query("INSERT INTO usage_attempt_journal_v2(attempt_id,scope_id,kind,created_at,known_not_sent,agent_gate_reason) VALUES(?,?,'agent_intent',?,1,'quota')").bind(&id).bind(item.scope_id).bind(item.created_at).execute(db.pool()).await.unwrap();
    let after = version(&db, &id).await;
    assert!(after.0 > before.0);
    assert_eq!(after.1, before.1 + 2);
    assert_eq!(after.2, 1);
    assert!(journal::confirm_review(&db, pending).await.is_err());
    assert!(reviewed(&db, &id).await.is_none());
    db.pool().close().await;
}

#[tokio::test]
async fn second_cas_conflict_rolls_back_entire_batch_and_revision() {
    let db = Db::connect_in_memory().await.unwrap();
    let a = gate(&db).await;
    let b = gate(&db).await;
    let pending = journal::prepare_review(&db, &[a.clone(), b.clone()])
        .await
        .unwrap();
    sqlx::query("UPDATE usage_attempt_journal_v2 SET created_at=created_at WHERE attempt_id=?")
        .bind(&b)
        .execute(db.pool())
        .await
        .unwrap();
    let va = version(&db, &a).await;
    let vb = version(&db, &b).await;
    assert!(journal::confirm_review(&db, pending).await.is_err());
    assert!(reviewed(&db, &a).await.is_none());
    assert!(reviewed(&db, &b).await.is_none());
    assert_eq!(version(&db, &a).await, va);
    assert_eq!(version(&db, &b).await, vb);
    db.pool().close().await;
}

#[tokio::test]
async fn outer_conflict_strategies_cannot_swallow_duplicate_guard() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = gate(&db).await;
    let before = version(&db, &id).await;
    let meta = schema::metadata(db.pool()).await.unwrap();
    for fk in [false, true] {
        for recursive in [false, true] {
            sqlx::raw_sql(&format!(
                "PRAGMA foreign_keys={}; PRAGMA recursive_triggers={};",
                i32::from(fk),
                i32::from(recursive)
            ))
            .execute(db.pool())
            .await
            .unwrap();
            for (prefix, suffix) in [
                ("INSERT", ""),
                ("INSERT OR IGNORE", ""),
                ("INSERT OR REPLACE", ""),
                (
                    "INSERT",
                    " ON CONFLICT(attempt_id) DO UPDATE SET created_at=excluded.created_at",
                ),
            ] {
                let sql=format!("{prefix} INTO usage_attempt_journal_v2(attempt_id,scope_id,kind,created_at) VALUES(?,?,'agent_intent',1){suffix}");
                assert!(sqlx::query(&sql)
                    .bind(&id)
                    .bind(scope())
                    .execute(db.pool())
                    .await
                    .is_err());
                assert_eq!(version(&db, &id).await, before);
                assert_eq!(schema::metadata(db.pool()).await.unwrap(), meta);
                assert!(reviewed(&db, &id).await.is_none());
            }
        }
    }
    db.pool().close().await;
}

#[tokio::test]
async fn overflow_rejects_real_writes_without_real_coercion_or_partial_batch() {
    let db = Db::connect_in_memory().await.unwrap();
    let a = gate(&db).await;
    let b = gate(&db).await;
    corrupt_counter(&db, i64::MAX).await;
    let before = schema::metadata(db.pool()).await.unwrap();
    assert!(
        journal::record_intent(&db, &scope(), AttemptKind::AgentIntent)
            .await
            .is_err()
    );
    assert_eq!(schema::metadata(db.pool()).await.unwrap(), before);
    sqlx::query("DROP TRIGGER usage_attempt_versions_v2_update_guard")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE usage_attempt_versions_v2 SET revision=? WHERE attempt_id=?")
        .bind(i64::MAX)
        .bind(&b)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::raw_sql(definition("usage_attempt_versions_v2_update_guard"))
        .execute(db.pool())
        .await
        .unwrap();
    schema::preflight(db.pool()).await.unwrap();
    let pending = journal::prepare_review(&db, &[a.clone(), b.clone()])
        .await
        .unwrap();
    let va = version(&db, &a).await;
    assert!(journal::confirm_review(&db, pending).await.is_err());
    assert_eq!(version(&db, &a).await, va);
    assert!(reviewed(&db, &a).await.is_none());
    assert!(reviewed(&db, &b).await.is_none());
    assert!(
        sqlx::query("DELETE FROM usage_attempt_journal_v2 WHERE attempt_id=?")
            .bind(&b)
            .execute(db.pool())
            .await
            .is_err()
    );
    let types: (String,String)=sqlx::query_as("SELECT typeof(last_entry_generation),typeof(v.revision) FROM client_usage_journal_meta_v2 JOIN usage_attempt_versions_v2 v ON v.attempt_id=?").bind(&b).fetch_one(db.pool()).await.unwrap();
    assert_eq!(types, ("integer".into(), "integer".into()));
    db.pool().close().await;
}

#[tokio::test]
async fn internal_ignore_rolls_back_business_change_and_version() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = gate(&db).await;
    let before = version(&db, &id).await;
    sqlx::raw_sql("CREATE TRIGGER owned_ignore BEFORE UPDATE ON usage_attempt_versions_v2 BEGIN SELECT RAISE(IGNORE); END;").execute(db.pool()).await.unwrap();
    assert!(
        schema::preflight(db.pool()).await.is_err(),
        "extra trigger rejects protected API access"
    );
    // Direct raw write checks the AFTER guard independently of Rust preflight.
    assert!(sqlx::query(
        "UPDATE usage_attempt_journal_v2 SET agent_gate_reason='concurrency' WHERE attempt_id=?"
    )
    .bind(&id)
    .execute(db.pool())
    .await
    .is_err());
    assert_eq!(version(&db, &id).await, before);
    let reason: String = sqlx::query_scalar(
        "SELECT agent_gate_reason FROM usage_attempt_journal_v2 WHERE attempt_id=?",
    )
    .bind(&id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(reason, "quota");
    db.pool().close().await;
}

#[tokio::test]
async fn ten_thousand_and_one_unknown_intents_have_no_total_gate_or_cleanup() {
    let db = Db::connect_in_memory().await.unwrap();
    let sid = scope();
    let mut transaction = db.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
    let mut first = String::new();
    for i in 0..10_001 {
        let id = scope();
        if i == 0 {
            first = id.clone();
        }
        sqlx::query("INSERT INTO usage_attempt_journal_v2(attempt_id,scope_id,kind,created_at) VALUES(?,?,'agent_intent',1)").bind(id).bind(&sid).execute(&mut *transaction).await.unwrap();
    }
    transaction.commit().await.unwrap();
    let count: i64=sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempt_journal_v2 WHERE known_not_sent=0 AND reviewed_at IS NULL AND audit_request_id IS NULL AND background_scope_id IS NULL").fetch_one(db.pool()).await.unwrap();
    assert_eq!(count, 10_001);
    journal::mark_agent_gate(&db, &first, AgentGateReason::Quota)
        .await
        .unwrap();
    let pending = journal::prepare_review(&db, &[first]).await.unwrap();
    assert_eq!(journal::confirm_review(&db, pending).await.unwrap(), 1);
    journal::record_intent(&db, &scope(), AttemptKind::Buffered)
        .await
        .unwrap();
    let (physical,versions):(i64,i64)=sqlx::query_as("SELECT (SELECT COUNT(*) FROM usage_attempt_journal_v2),(SELECT COUNT(*) FROM usage_attempt_versions_v2)").fetch_one(db.pool()).await.unwrap();
    assert_eq!((physical, versions), (10_002, 10_002));
    schema::preflight(db.pool()).await.unwrap();
    db.pool().close().await;
}

struct OwnedWal {
    directory: std::path::PathBuf,
    path: std::path::PathBuf,
}
const OWNER: &[u8] = br#"{"purpose":"r23 CLIENT SQLx WAL fixture; no user data"}"#;
impl OwnedWal {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("llmgw-owned-r23-client-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("owner.json"), OWNER).unwrap();
        let path = directory.join("owned.sqlite3");
        assert!(!path.exists());
        Self { directory, path }
    }
    async fn connect(&self) -> Db {
        assert_eq!(
            std::fs::read(self.directory.join("owner.json")).unwrap(),
            OWNER
        );
        let db = Db::connect_path(&self.path).await.unwrap();
        let (mode, engine): (String, String) = sqlx::query_as(
            "SELECT (SELECT journal_mode FROM pragma_journal_mode),sqlite_version()",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(engine, "3.46.0");
        let actual: String =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name='main'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            std::path::Path::new(&actual).canonicalize().unwrap(),
            self.path.canonicalize().unwrap()
        );
        db
    }
    async fn cleanup(self) {
        assert_eq!(
            std::fs::read(self.directory.join("owner.json")).unwrap(),
            OWNER
        );
        let absolute = self.directory.canonicalize().unwrap();
        assert_eq!(
            absolute.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(absolute
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("llmgw-owned-r23-client-"));
        // SQLx may still be dropping an asynchronously returned connection after
        // the pool closes. Verify release on owned files before deleting any
        // owner marker; this retry only handles Windows sharing violations.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
            for file in ["owned.sqlite3", "owned.sqlite3-wal", "owned.sqlite3-shm"] {
                let path = absolute.join(file);
                loop {
                    match std::fs::OpenOptions::new()
                        .read(true)
                        .share_mode(0)
                        .open(&path)
                    {
                        Ok(handle) => {
                            drop(handle);
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                        Err(error)
                            if matches!(error.raw_os_error(), Some(32) | Some(33))
                                && tokio::time::Instant::now() < deadline =>
                        {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                        Err(error) => panic!("owned fixture handles not released: {error}"),
                    }
                }
            }
        }
        std::fs::remove_dir_all(absolute).unwrap();
    }
}

#[tokio::test]
async fn independent_wal_pools_reject_aba_and_runtime_reopen_preserves_tombstone() {
    let fixture = OwnedWal::new();
    let first = fixture.connect().await;
    let second = fixture.connect().await;
    let id = scope();
    let sid = scope();
    let query="INSERT INTO usage_attempt_journal_v2(attempt_id,scope_id,kind,created_at) VALUES(?,?,'agent_intent',1)";
    let (a, b) = tokio::join!(
        sqlx::query(query)
            .bind(&id)
            .bind(&sid)
            .execute(first.pool()),
        sqlx::query(query)
            .bind(&id)
            .bind(&sid)
            .execute(second.pool())
    );
    assert_eq!(
        usize::from(a.is_ok()) + usize::from(b.is_ok()),
        1,
        "duplicate race has exactly one winner"
    );
    let meta = schema::metadata(first.pool()).await.unwrap();
    assert_eq!(meta.last_entry_generation, 1);
    journal::mark_agent_gate(&first, &id, AgentGateReason::Quota)
        .await
        .unwrap();
    let pending = journal::prepare_review(&first, std::slice::from_ref(&id))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE usage_attempt_journal_v2 SET agent_gate_reason='concurrency' WHERE attempt_id=?",
    )
    .bind(&id)
    .execute(second.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE usage_attempt_journal_v2 SET agent_gate_reason='quota' WHERE attempt_id=?")
        .bind(&id)
        .execute(second.pool())
        .await
        .unwrap();
    assert!(journal::confirm_review(&first, pending).await.is_err());
    let old_runtime = journal::prepare_review(&first, std::slice::from_ref(&id))
        .await
        .unwrap();
    let foreign = journal::prepare_review(&first, std::slice::from_ref(&id))
        .await
        .unwrap();
    assert_eq!(
        journal::confirm_review(&second, foreign)
            .await
            .unwrap_err()
            .to_string(),
        "client_usage_journal_runtime_changed"
    );
    sqlx::query("DELETE FROM usage_attempt_journal_v2 WHERE attempt_id=?")
        .bind(&id)
        .execute(second.pool())
        .await
        .unwrap();
    let tombstone = version(&first, &id).await;
    assert_eq!(tombstone.2, 0);
    first.pool().close().await;
    second.pool().close().await;
    drop(first);
    drop(second);
    let third = fixture.connect().await;
    assert_eq!(schema::metadata(third.pool()).await.unwrap(), meta);
    assert_eq!(version(&third, &id).await, tombstone);
    assert_eq!(
        journal::confirm_review(&third, old_runtime)
            .await
            .unwrap_err()
            .to_string(),
        "client_usage_journal_runtime_changed"
    );
    sqlx::query(query)
        .bind(&id)
        .bind(&sid)
        .execute(third.pool())
        .await
        .unwrap();
    let rebuilt = version(&third, &id).await;
    assert!(rebuilt.0 > tombstone.0);
    assert_eq!(rebuilt.1, tombstone.1 + 1);
    assert_eq!(rebuilt.2, 1);
    third.pool().close().await;
    drop(third);
    fixture.cleanup().await;
}

#[tokio::test]
async fn read_only_storage_refuses_intent_and_links_retain_evidence_after_audit_deletion() {
    let db = Db::connect_in_memory().await.unwrap();
    sqlx::query("PRAGMA query_only=ON")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(journal::record_intent(&db, &scope(), AttemptKind::Buffered)
        .await
        .is_err());
    sqlx::query("PRAGMA query_only=OFF")
        .execute(db.pool())
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempt_journal_v2")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        schema::metadata(db.pool())
            .await
            .unwrap()
            .last_entry_generation,
        0
    );
    let id = journal::record_intent(&db, &scope(), AttemptKind::Buffered)
        .await
        .unwrap();
    assert!(
        journal::link_audit(&db, &id, 1).await.is_err(),
        "missing request cannot be linked"
    );
    let request: i64 = sqlx::query_scalar(
        "INSERT INTO requests(ts,requested_model) VALUES(1,'owned-model') RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    journal::link_audit(&db, &id, request).await.unwrap();
    assert!(
        journal::link_audit(&db, &id, request).await.is_err(),
        "no implicit replay of link"
    );
    assert_eq!(version(&db, &id).await.1, 2);
    sqlx::query("DELETE FROM requests WHERE id=?")
        .bind(request)
        .execute(db.pool())
        .await
        .unwrap();
    let link: Option<i64> = sqlx::query_scalar(
        "SELECT audit_request_id FROM usage_attempt_journal_v2 WHERE attempt_id=?",
    )
    .bind(&id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        link,
        Some(request),
        "old requests retention must not cascade journal deletion"
    );
    assert!(journal::prepare_review(&db, &[id]).await.is_err());
    schema::preflight(db.pool()).await.unwrap();
    db.pool().close().await;
}

#[tokio::test]
async fn metadata_identity_cannot_be_replaced_reset_or_deleted() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = gate(&db).await;
    let before = schema::metadata(db.pool()).await.unwrap();
    for statement in [
        "INSERT OR REPLACE INTO client_usage_journal_meta_v2 SELECT * FROM client_usage_journal_meta_v2",
        "UPDATE OR IGNORE client_usage_journal_meta_v2 SET db_generation='00000000-0000-4000-8000-000000000001',last_entry_generation=last_entry_generation+1",
        "UPDATE OR REPLACE client_usage_journal_meta_v2 SET contract_version=1,last_entry_generation=last_entry_generation+1",
        "UPDATE client_usage_journal_meta_v2 SET last_entry_generation=0",
        "DELETE FROM client_usage_journal_meta_v2",
    ] {
        assert!(sqlx::query(statement).execute(db.pool()).await.is_err());
        assert_eq!(schema::metadata(db.pool()).await.unwrap(),before);
        assert_eq!(version(&db,&id).await,(1,2,1));
    }
    db.pool().close().await;
}

#[tokio::test]
async fn failed_preflight_preserves_existing_wal_db_and_wal_bytes() {
    let fixture = OwnedWal::new();
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&fixture.path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query("PRAGMA wal_autocheckpoint=0")
        .execute(&pool)
        .await
        .unwrap();
    schema::migrate(&pool).await.unwrap();
    sqlx::raw_sql("CREATE TABLE requests(sentinel TEXT); INSERT INTO requests VALUES('owned-wal-keep'); DROP TRIGGER usage_attempt_journal_v2_update_guard;").execute(&pool).await.unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    let wal = fixture.path.with_file_name("owned.sqlite3-wal");
    let before_db = std::fs::read(&fixture.path).unwrap();
    let before_wal = std::fs::read(&wal).unwrap();
    assert!(!before_wal.is_empty());
    assert!(migrations::run(&pool).await.is_err());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), before_db);
    assert_eq!(
        std::fs::read(&wal).unwrap(),
        before_wal,
        "SELECT preflight does not repair legacy DDL or add WAL frames"
    );
    let sentinel: String = sqlx::query_scalar("SELECT sentinel FROM requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sentinel, "owned-wal-keep");
    // This deliberately tests migrations::run on an already-open WAL pool.
    // It does not claim connect_path's preceding WAL pragma or SHM is unwritten.
    pool.close().await;
    drop(pool);
    fixture.cleanup().await;
}

#[tokio::test]
async fn unrelated_new_intent_allows_review_but_replaced_database_identity_rejects_it() {
    let db = Db::connect_in_memory().await.unwrap();
    let id = gate(&db).await;
    let pending = journal::prepare_review(&db, std::slice::from_ref(&id))
        .await
        .unwrap();
    journal::record_intent(&db, &scope(), AttemptKind::Buffered)
        .await
        .unwrap();
    assert_eq!(
        journal::confirm_review(&db, pending).await.unwrap(),
        1,
        "unrelated growth does not change this entry version"
    );
    let other = gate(&db).await;
    let pending = journal::prepare_review(&db, std::slice::from_ref(&other))
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER client_usage_meta_v2_update_guard")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE client_usage_journal_meta_v2 SET db_generation=?")
        .bind(scope())
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::raw_sql(definition("client_usage_meta_v2_update_guard"))
        .execute(db.pool())
        .await
        .unwrap();
    schema::preflight(db.pool()).await.unwrap();
    assert_eq!(
        journal::confirm_review(&db, pending)
            .await
            .unwrap_err()
            .to_string(),
        "client_usage_journal_database_changed"
    );
    assert!(reviewed(&db, &other).await.is_none());
    db.pool().close().await;
}

#[tokio::test]
async fn tombstone_revision_overflow_refuses_reinsert_and_gate_classification_is_narrow() {
    let db = Db::connect_in_memory().await.unwrap();
    let regular = journal::record_intent(&db, &scope(), AttemptKind::Buffered)
        .await
        .unwrap();
    assert!(
        journal::mark_agent_gate(&db, &regular, AgentGateReason::Quota)
            .await
            .is_err()
    );
    let id = gate(&db).await;
    assert!(
        journal::mark_agent_gate(&db, &id, AgentGateReason::Concurrency)
            .await
            .is_err()
    );
    let request: i64 = sqlx::query_scalar(
        "INSERT INTO requests(ts,requested_model) VALUES(1,'owned-model') RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        journal::link_audit(&db, &id, request).await.is_err(),
        "known-not-sent gate cannot link a dispatched request"
    );
    let agent = journal::record_intent(&db, &scope(), AttemptKind::AgentIntent)
        .await
        .unwrap();
    journal::link_audit(&db, &agent, request).await.unwrap();
    assert!(
        journal::mark_agent_gate(&db, &agent, AgentGateReason::Quota)
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM usage_attempt_journal_v2 WHERE attempt_id=?")
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER usage_attempt_versions_v2_update_guard")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE usage_attempt_versions_v2 SET revision=? WHERE attempt_id=?")
        .bind(i64::MAX)
        .bind(&id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::raw_sql(definition("usage_attempt_versions_v2_update_guard"))
        .execute(db.pool())
        .await
        .unwrap();
    let before = schema::metadata(db.pool()).await.unwrap();
    assert!(sqlx::query("INSERT INTO usage_attempt_journal_v2(attempt_id,scope_id,kind,created_at) VALUES(?,?,'agent_intent',1)").bind(&id).bind(scope()).execute(db.pool()).await.is_err());
    assert_eq!(schema::metadata(db.pool()).await.unwrap(), before);
    assert_eq!(version(&db, &id).await.1, i64::MAX);
    assert_eq!(version(&db, &id).await.2, 0);
    db.pool().close().await;
}
