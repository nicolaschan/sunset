//! SQLite KV index layer.

use bytes::Bytes;
use sunset_store::{Cursor, Error, Filter, InsertOutcome, Result, SignedKvEntry, VerifyingKey};
use tokio_rusqlite::rusqlite::{self, OptionalExtension, Row, params};

/// Canonical column projection for the `entries` table, in `row_to_entry`
/// order. Append a `WHERE …`/`ORDER BY …` clause to build a full query.
const ENTRY_SELECT: &str = "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature FROM entries";

/// Decode a `value_hash` BLOB column (exactly 32 bytes) into a `Hash`.
pub(crate) fn hash_from_blob(bytes: &[u8]) -> rusqlite::Result<sunset_store::Hash> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            bytes.len(),
            rusqlite::types::Type::Blob,
            Box::<dyn std::error::Error + Send + Sync>::from("value_hash not 32 bytes"),
        )
    })?;
    Ok(sunset_store::Hash::from(arr))
}

/// Row → SignedKvEntry. The `sequence` column is also returned so callers can
/// use it for cursors / events.
fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<(u64, SignedKvEntry)> {
    let sequence: i64 = row.get("sequence")?;
    let verifying_key: Vec<u8> = row.get("verifying_key")?;
    let name: Vec<u8> = row.get("name")?;
    let value_hash: Vec<u8> = row.get("value_hash")?;
    let priority: i64 = row.get("priority")?;
    let expires_at: Option<i64> = row.get("expires_at")?;
    let signature: Vec<u8> = row.get("signature")?;
    let entry = SignedKvEntry {
        verifying_key: VerifyingKey(Bytes::from(verifying_key)),
        name: Bytes::from(name),
        value_hash: hash_from_blob(&value_hash)?,
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
        &format!("{ENTRY_SELECT} WHERE verifying_key = ?1 AND name = ?2"),
        params![vk.as_bytes(), name],
        |row| row_to_entry(row).map(|(_, e)| e),
    )
    .optional()
}

/// Apply LWW + insert under an open transaction. Caller is responsible for
/// running this inside `conn.call(|c| { let txn = c.transaction()?; ... txn.commit()?; })`.
pub fn insert_lww(txn: &rusqlite::Transaction<'_>, entry: &SignedKvEntry) -> Result<InsertOutcome> {
    let existing = txn
        .query_row(
            &format!("{ENTRY_SELECT} WHERE verifying_key = ?1 AND name = ?2"),
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

    debug_assert!(
        entry.priority <= i64::MAX as u64,
        "priority {} exceeds i64::MAX; SQLite stores INTEGER as i64 and would silently wrap",
        entry.priority,
    );
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

    Ok(match existing {
        Some((_, old)) => InsertOutcome::Replaced { old },
        None => InsertOutcome::Inserted,
    })
}

/// Delete all entries with `expires_at <= now`. Returns the deleted entries
/// (so the caller can broadcast `Event::Expired` for each).
pub fn delete_expired(txn: &rusqlite::Transaction<'_>, now: u64) -> Result<Vec<SignedKvEntry>> {
    let victims = query_entries(
        txn,
        "WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        params![now as i64],
    )?;
    txn.execute(
        "DELETE FROM entries WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        params![now as i64],
    )
    .map_err(|e| Error::Backend(format!("delete: {e}")))?;
    Ok(victims)
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

/// Run `{ENTRY_SELECT} {tail}` (tail = a `WHERE …`/`ORDER BY …` clause) and
/// collect the matching entries.
fn query_entries(
    conn: &rusqlite::Connection,
    tail: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<SignedKvEntry>> {
    let mut stmt = conn
        .prepare(&format!("{ENTRY_SELECT} {tail}"))
        .map_err(|e| Error::Backend(format!("prep: {e}")))?;
    let rows = stmt
        .query_map(params, |r| row_to_entry(r).map(|(_, e)| e))
        .map_err(|e| Error::Backend(format!("query: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| Error::Backend(format!("row: {e}")))?);
    }
    Ok(out)
}

/// Collect all entries with `sequence >= cursor`, ordered by `sequence ASC`.
/// Used for `Replay::Since`; the caller applies the subscription filter.
pub fn iter_since(conn: &rusqlite::Connection, cursor: Cursor) -> Result<Vec<SignedKvEntry>> {
    query_entries(
        conn,
        "WHERE sequence >= ?1 ORDER BY sequence ASC",
        params![cursor.0 as i64],
    )
}

/// Collect all entries matching `filter` into a `Vec`. Ordered by
/// `sequence ASC` within each sub-query for determinism in tests.
pub fn iter_with_filter(
    conn: &rusqlite::Connection,
    filter: &Filter,
) -> Result<Vec<SignedKvEntry>> {
    let mut out = Vec::new();
    match filter {
        Filter::Specific(vk, name) => {
            if let Some(e) = get_entry(conn, vk, name.as_ref())
                .map_err(|e| Error::Backend(format!("specific: {e}")))?
            {
                out.push(e);
            }
        }
        Filter::Keyspace(vk) => {
            out = query_entries(
                conn,
                "WHERE verifying_key = ?1 ORDER BY sequence ASC",
                params![vk.as_bytes()],
            )?;
        }
        Filter::Namespace(name) => {
            out = query_entries(
                conn,
                "WHERE name = ?1 ORDER BY sequence ASC",
                params![name.as_ref()],
            )?;
        }
        Filter::NamePrefix(prefix) => {
            out = query_entries(
                conn,
                "WHERE substr(name, 1, ?2) = ?1 ORDER BY sequence ASC",
                params![prefix.as_ref(), prefix.len() as i64],
            )?;
        }
        Filter::Union(filters) => {
            let mut seen = std::collections::HashSet::<(Vec<u8>, Vec<u8>)>::new();
            for f in filters {
                for e in iter_with_filter(conn, f)? {
                    let key = (e.verifying_key.as_bytes().to_vec(), e.name.to_vec());
                    if seen.insert(key) {
                        out.push(e);
                    }
                }
            }
        }
    }
    Ok(out)
}
