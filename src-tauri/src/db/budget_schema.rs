// Additive local reference-budget ledger. No credentials, prompts or replies.
pub(super) const SCHEMA: &[(&str, &str)] = &[(
    "budget_holds",
    r#"
CREATE TABLE IF NOT EXISTS budget_holds (
 hold_id TEXT PRIMARY KEY,
 gateway_request_uuid TEXT NOT NULL,
 call_ordinal INTEGER NOT NULL CHECK(call_ordinal >= 0),
 key_id TEXT NOT NULL,
 admission_month INTEGER NOT NULL,
 currency TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('reserved','dispatched','settled','released')),
 reserved_micros INTEGER NOT NULL CHECK(reserved_micros >= 0),
 reported_reference_micros INTEGER CHECK(reported_reference_micros >= 0),
 target_price_json TEXT NOT NULL,
 audit_request_id INTEGER REFERENCES requests(id) ON DELETE SET NULL,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 UNIQUE(gateway_request_uuid, call_ordinal)
);
CREATE INDEX IF NOT EXISTS idx_budget_key_month ON budget_holds(key_id,admission_month,currency,state);
CREATE INDEX IF NOT EXISTS idx_budget_audit ON budget_holds(audit_request_id);
"#,
)];
