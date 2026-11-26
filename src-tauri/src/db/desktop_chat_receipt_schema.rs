//! Local write-ahead receipts are separate from conversation history and request retention.
//! Deleting a claim would let the same operation ID dispatch again.
pub(super) const SCHEMA: &[(&str, &str)] = &[(
    "desktop_chat_operation_receipts",
    r#"
CREATE TABLE IF NOT EXISTS desktop_chat_operation_receipts (
    cursor INTEGER PRIMARY KEY CHECK (cursor BETWEEN 1 AND 9007199254740991),
    operation_id TEXT NOT NULL UNIQUE CHECK (length(operation_id) = 36),
    window_label TEXT NOT NULL,
    webview_label TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('claimed', 'completed'))
);
CREATE INDEX IF NOT EXISTS desktop_chat_receipts_owner_cursor
    ON desktop_chat_operation_receipts (window_label, webview_label, cursor DESC);
"#,
)];
