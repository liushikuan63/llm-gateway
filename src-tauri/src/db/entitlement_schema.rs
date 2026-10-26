//! Additive local ledger only. Rows are accounting, never authority to issue a
//! free-entitlement proof; no FK to replaceable provider/model/account rows.
pub const SCHEMA: &[(&str, &str)] = &[(
    "free_entitlement_ledger",
    r#"
CREATE TABLE IF NOT EXISTS free_entitlement_budgets (
    id TEXT PRIMARY KEY,
    ceiling INTEGER NOT NULL CHECK(ceiling >= 0 AND ceiling <= 2305843009213693951),
    held INTEGER NOT NULL DEFAULT 0 CHECK(held >= 0 AND held <= 2305843009213693951),
    spent INTEGER NOT NULL DEFAULT 0 CHECK(spent >= 0 AND spent <= 2305843009213693951),
    blocked INTEGER NOT NULL DEFAULT 0 CHECK(blocked IN (0,1))
);
CREATE TABLE IF NOT EXISTS free_entitlement_bindings (
    id TEXT PRIMARY KEY,
    principal TEXT NOT NULL,
    epoch INTEGER NOT NULL CHECK(epoch > 0),
    credential_fingerprint TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    snapshot_fingerprint TEXT NOT NULL,
    observed_at INTEGER NOT NULL CHECK(observed_at >= 0),
    expires_at INTEGER NOT NULL CHECK(expires_at > observed_at),
    last_clock INTEGER NOT NULL CHECK(last_clock >= observed_at),
    revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1))
);
CREATE TABLE IF NOT EXISTS free_entitlement_reservations (
    id TEXT PRIMARY KEY,
    budget TEXT NOT NULL REFERENCES free_entitlement_budgets(id) ON DELETE RESTRICT,
    binding TEXT NOT NULL REFERENCES free_entitlement_bindings(id) ON DELETE RESTRICT,
    attempt TEXT NOT NULL UNIQUE,
    epoch INTEGER NOT NULL,
    credential_fingerprint TEXT NOT NULL,
    revision INTEGER NOT NULL,
    snapshot_fingerprint TEXT NOT NULL,
    model TEXT NOT NULL,
    purpose TEXT NOT NULL,
    unit TEXT NOT NULL,
    bound INTEGER NOT NULL CHECK(bound > 0 AND bound <= 2305843009213693951),
    phase TEXT NOT NULL CHECK(phase IN ('reserved','dispatched','cancelled','settled')),
    confirmed_units INTEGER CHECK(confirmed_units >= 0 AND confirmed_units <= bound),
    CHECK((phase = 'settled') = (confirmed_units IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS idx_free_entitlement_budget_phase
    ON free_entitlement_reservations(budget,phase);
"#,
)];
