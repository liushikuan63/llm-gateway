//! Additive durable catalog facts. Model policies deliberately do not reference
//! models(id): the existing provider editor deletes and reinserts those rows.

use sqlx::SqlitePool;

pub(crate) const SCHEMA: &[(&str, &str)] = &[
    (
        "catalog_provider_revision",
        r#"
CREATE TABLE IF NOT EXISTS catalog_provider_revision (
    provider_id TEXT PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL DEFAULT 0 CHECK(revision >= 0)
);
"#,
    ),
    (
        "catalog_scan_state",
        r#"
CREATE TABLE IF NOT EXISTS catalog_scan_state (
    provider_id TEXT PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,
    target_fingerprint TEXT NOT NULL DEFAULT '',
    run_id TEXT,
    fence INTEGER NOT NULL DEFAULT 0 CHECK(fence >= 0),
    lease_owner TEXT,
    lease_started_at INTEGER,
    lease_expires_at INTEGER,
    last_attempt_at INTEGER,
    last_complete_success_at INTEGER,
    next_due_at INTEGER,
    next_retry_at INTEGER,
    status TEXT NOT NULL DEFAULT 'never',
    failure_kind TEXT,
    warning_codes_json TEXT NOT NULL DEFAULT '[]',
    counters_json TEXT NOT NULL DEFAULT '{}',
    failure_count INTEGER NOT NULL DEFAULT 0 CHECK(failure_count >= 0),
    clock_anomaly INTEGER NOT NULL DEFAULT 0 CHECK(clock_anomaly IN (0,1))
    ,runtime_pending INTEGER NOT NULL DEFAULT 0 CHECK(runtime_pending IN (0,1))
);
"#,
    ),
    (
        "catalog_scan_runs",
        r#"
CREATE TABLE IF NOT EXISTS catalog_scan_runs (
    run_id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    target_fingerprint TEXT NOT NULL,
    captured_revision INTEGER NOT NULL,
    fence INTEGER NOT NULL,
    lease_owner TEXT NOT NULL,
    trigger_kind TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    status TEXT NOT NULL,
    completeness TEXT,
    source_contract TEXT,
    nonempty INTEGER NOT NULL DEFAULT 0,
    raw_count INTEGER NOT NULL DEFAULT 0,
    valid_count INTEGER NOT NULL DEFAULT 0,
    duplicate_count INTEGER NOT NULL DEFAULT 0,
    rejected_count INTEGER NOT NULL DEFAULT 0,
    failure_kind TEXT,
    warning_codes_json TEXT NOT NULL DEFAULT '[]',
    changes_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_catalog_runs_provider_time ON catalog_scan_runs(provider_id,started_at DESC);
"#,
    ),
    (
        "catalog_inventory",
        r#"
CREATE TABLE IF NOT EXISTS catalog_inventory (
    provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    upstream_id TEXT NOT NULL,
    target_fingerprint TEXT NOT NULL,
    observation_generation INTEGER NOT NULL DEFAULT 0,
    observed_at INTEGER NOT NULL,
    source_contract TEXT NOT NULL,
    fee_status TEXT NOT NULL CHECK(fee_status IN ('free','paid','unknown')),
    fee_reason_codes_json TEXT NOT NULL DEFAULT '[]',
    fee_evidence_json TEXT NOT NULL DEFAULT '{}',
    fee_expires_at INTEGER,
    model_json TEXT NOT NULL,
    lifecycle TEXT NOT NULL DEFAULT 'active' CHECK(lifecycle IN ('active','missing_pending','soft_offline')),
    first_missing_at INTEGER,
    last_missing_observation_at INTEGER,
    missing_confirmations INTEGER NOT NULL DEFAULT 0 CHECK(missing_confirmations >= 0),
    last_seen_run_id TEXT NOT NULL,
    last_complete_run_id TEXT,
    PRIMARY KEY(provider_id,upstream_id)
);
"#,
    ),
    (
        "catalog_model_policy",
        r#"
CREATE TABLE IF NOT EXISTS catalog_model_policy (
    provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    alias TEXT NOT NULL,
    upstream_id TEXT NOT NULL,
    managed_by TEXT NOT NULL DEFAULT 'user' CHECK(managed_by IN ('user','catalog')),
    user_override TEXT NOT NULL DEFAULT 'none' CHECK(user_override IN ('none','enabled','disabled','removed')),
    user_revision INTEGER NOT NULL DEFAULT 0 CHECK(user_revision >= 0),
    field_overrides_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER,
    updated_at INTEGER,
    PRIMARY KEY(provider_id,alias)
);
"#,
    ),
];

pub(crate) async fn finish(pool: &SqlitePool) -> anyhow::Result<()> {
    // Serialize only this short catalog migration; no external I/O occurs here.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::raw_sql(r#"
INSERT OR IGNORE INTO catalog_provider_revision(provider_id,revision) SELECT id,0 FROM providers;
INSERT OR IGNORE INTO catalog_model_policy(provider_id,alias,upstream_id,managed_by,user_override)
    SELECT provider_id,alias,upstream,'user','none' FROM models;

-- Refresh only our trigger definitions atomically. An outer INSERT OR REPLACE
-- also controls conflicts inside triggers, so seed without causing a conflict.
DROP TRIGGER IF EXISTS catalog_provider_insert_revision;
DROP TRIGGER IF EXISTS catalog_model_insert_revision;
CREATE TRIGGER catalog_provider_insert_revision AFTER INSERT ON providers BEGIN
    UPDATE catalog_provider_revision SET revision=revision+1 WHERE provider_id=NEW.id;
    INSERT INTO catalog_provider_revision(provider_id,revision)
    SELECT NEW.id,1 WHERE NOT EXISTS(SELECT 1 FROM catalog_provider_revision WHERE provider_id=NEW.id);
END;
CREATE TRIGGER IF NOT EXISTS catalog_provider_update_revision AFTER UPDATE ON providers BEGIN
    UPDATE catalog_provider_revision SET revision=revision+1 WHERE provider_id=NEW.id;
END;
CREATE TRIGGER catalog_model_insert_revision AFTER INSERT ON models BEGIN
    UPDATE catalog_provider_revision SET revision=revision+1 WHERE provider_id=NEW.provider_id;
    INSERT INTO catalog_provider_revision(provider_id,revision)
    SELECT NEW.provider_id,1 WHERE NOT EXISTS(SELECT 1 FROM catalog_provider_revision WHERE provider_id=NEW.provider_id);
    INSERT INTO catalog_model_policy(provider_id,alias,upstream_id,managed_by,user_override)
    SELECT NEW.provider_id,NEW.alias,NEW.upstream,'user','none'
    WHERE NOT EXISTS(SELECT 1 FROM catalog_model_policy WHERE provider_id=NEW.provider_id AND alias=NEW.alias);
END;
CREATE TRIGGER IF NOT EXISTS catalog_model_update_revision AFTER UPDATE ON models BEGIN
    UPDATE catalog_provider_revision SET revision=revision+1 WHERE provider_id IN (OLD.provider_id,NEW.provider_id);
END;
CREATE TRIGGER IF NOT EXISTS catalog_model_delete_revision AFTER DELETE ON models BEGIN
    UPDATE catalog_provider_revision SET revision=revision+1 WHERE provider_id=OLD.provider_id;
END;
"#).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
