//! SQL schema for the FsStore KV index.

/// Idempotent DDL applied on every open. Uses `IF NOT EXISTS` so re-opening
/// an existing store is a no-op.
pub const SCHEMA_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
    sequence       INTEGER PRIMARY KEY AUTOINCREMENT,
    verifying_key  BLOB NOT NULL,
    name           BLOB NOT NULL,
    value_hash     BLOB NOT NULL,
    priority       INTEGER NOT NULL,
    expires_at     INTEGER,
    signature      BLOB NOT NULL,
    UNIQUE(verifying_key, name)
);

CREATE INDEX IF NOT EXISTS idx_entries_name
    ON entries(name);
"#;
