//! SQL schema for the FsStore KV index.

use tokio_rusqlite::rusqlite;

pub const SCHEMA_VERSION: i32 = 1;

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

CREATE INDEX IF NOT EXISTS idx_entries_expires_at
    ON entries(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS schema_meta (
    key   TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
"#;

/// Apply the DDL and record the schema version. Called by `FsStore::new`.
pub fn apply_schema(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_DDL)?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_meta(key, value) VALUES ('version', ?1)",
        [SCHEMA_VERSION],
    )?;
    Ok(())
}
