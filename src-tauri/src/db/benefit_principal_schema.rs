//! Additive ownership metadata. Historical receipts/runs are never assigned a
//! principal from the account that happens to be logged in during migration.
pub const SCHEMA: &[(&str, &str)] = &[(
    "benefit_principal_claims",
    r#"
CREATE TABLE IF NOT EXISTS benefit_run_principals (
    run_id INTEGER PRIMARY KEY REFERENCES benefit_runs(id) ON DELETE RESTRICT,
    platform TEXT NOT NULL,
    principal_hash TEXT NOT NULL,
    source_fingerprint TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS benefit_principal_claims (
    platform TEXT NOT NULL,
    principal_hash TEXT NOT NULL,
    window_key TEXT NOT NULL,
    attempt_id TEXT NOT NULL UNIQUE,
    owner_account_id TEXT NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('reserved','dispatched','terminal')),
    run_id INTEGER REFERENCES benefit_runs(id) ON DELETE RESTRICT,
    verdict TEXT CHECK(verdict IN ('granted','replayed','error','no_claimable')),
    CHECK((phase = 'terminal') = (run_id IS NOT NULL)),
    CHECK((run_id IS NULL) = (verdict IS NULL)),
    PRIMARY KEY(platform,principal_hash,window_key)
);
CREATE INDEX IF NOT EXISTS idx_benefit_principal_window ON benefit_principal_claims(platform,window_key);
"#,
)];
