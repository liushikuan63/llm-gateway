//! Human annotations are separate from immutable machine audit/usage facts.
pub(crate) const SCHEMA: &[(&str, &str)] = &[(
    "request_feedback",
    r#"
CREATE TABLE IF NOT EXISTS request_feedback (
    request_id INTEGER PRIMARY KEY REFERENCES requests(id) ON DELETE CASCADE,
    confirmed_class TEXT CHECK (confirmed_class IS NULL OR confirmed_class IN ('simple','vision','reasoning')),
    outcome TEXT CHECK (outcome IS NULL OR outcome IN ('met_need','did_not_meet_need','unsure')),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    source TEXT NOT NULL CHECK (source = 'local_user'),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
"#,
)];
