//! SQLite KV index layer.

use bytes::Bytes;
use sunset_store::{Cursor, Error, Result, SignedKvEntry, VerifyingKey};
use tokio_rusqlite::rusqlite::{self, OptionalExtension, Row, params};

/// Row → SignedKvEntry. The `sequence` column is also returned so callers can
/// use it for cursors / events.
pub fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<(u64, SignedKvEntry)> {
    let sequence: i64 = row.get("sequence")?;
    let verifying_key: Vec<u8> = row.get("verifying_key")?;
    let name: Vec<u8> = row.get("name")?;
    let value_hash: Vec<u8> = row.get("value_hash")?;
    let priority: i64 = row.get("priority")?;
    let expires_at: Option<i64> = row.get("expires_at")?;
    let signature: Vec<u8> = row.get("signature")?;
    let mut hash_bytes = [0u8; 32];
    if value_hash.len() != 32 {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            value_hash.len(),
            rusqlite::types::Type::Blob,
            Box::<dyn std::error::Error + Send + Sync>::from("value_hash not 32 bytes"),
        ));
    }
    hash_bytes.copy_from_slice(&value_hash);
    let entry = SignedKvEntry {
        verifying_key: VerifyingKey(Bytes::from(verifying_key)),
        name: Bytes::from(name),
        value_hash: sunset_store::Hash::from(hash_bytes),
        priority: priority as u64,
        expires_at: expires_at.map(|x| x as u64),
        signature: Bytes::from(signature),
    };
    Ok((sequence as u64, entry))
}

/// Get the entry for `(vk, name)` if present.
pub fn get_entry(
    conn: &rusqlite::Connection,
    vk: &VerifyingKey,
    name: &[u8],
) -> rusqlite::Result<Option<SignedKvEntry>> {
    conn.query_row(
        "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature
         FROM entries WHERE verifying_key = ?1 AND name = ?2",
        params![vk.as_bytes(), name],
        |row| row_to_entry(row).map(|(_, e)| e),
    )
    .optional()
}

/// Outcome of an attempted insert. The caller uses it to decide which event
/// variant to broadcast.
#[derive(Debug)]
pub enum InsertOutcome {
    #[allow(dead_code)] // sequence used in Tasks 5 + 8
    Inserted { sequence: u64 },
    #[allow(dead_code)] // sequence used in Tasks 5 + 8
    Replaced { old: SignedKvEntry, sequence: u64 },
}

/// Apply LWW + insert under an open transaction. Caller is responsible for
/// running this inside `conn.call(|c| { let txn = c.transaction()?; ... txn.commit()?; })`.
pub fn insert_lww(txn: &rusqlite::Transaction<'_>, entry: &SignedKvEntry) -> Result<InsertOutcome> {
    let existing = txn
        .query_row(
            "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature
             FROM entries WHERE verifying_key = ?1 AND name = ?2",
            params![entry.verifying_key.as_bytes(), entry.name.as_ref()],
            row_to_entry,
        )
        .optional()
        .map_err(|e| Error::Backend(format!("select existing: {e}")))?;

    if let Some((_, ref old)) = existing {
        if old.priority >= entry.priority {
            return Err(Error::Stale);
        }
    }

    txn.execute(
        "INSERT OR REPLACE INTO entries
            (verifying_key, name, value_hash, priority, expires_at, signature)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            entry.verifying_key.as_bytes(),
            entry.name.as_ref(),
            entry.value_hash.as_bytes(),
            entry.priority as i64,
            entry.expires_at.map(|x| x as i64),
            entry.signature.as_ref(),
        ],
    )
    .map_err(|e| Error::Backend(format!("insert entry: {e}")))?;

    let sequence = txn.last_insert_rowid() as u64;

    Ok(match existing {
        Some((_, old)) => InsertOutcome::Replaced { old, sequence },
        None => InsertOutcome::Inserted { sequence },
    })
}

/// Cursor query: next-to-be-assigned sequence.
pub fn current_cursor(conn: &rusqlite::Connection) -> rusqlite::Result<Cursor> {
    let last: Option<i64> = conn
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'entries'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(Cursor(last.unwrap_or(0) as u64 + 1))
}
