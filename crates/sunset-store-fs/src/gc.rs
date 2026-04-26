//! Mark-and-sweep blob GC.
//!
//! Marks every blob reachable from the current set of `entries.value_hash`
//! values (transitively, by walking `ContentBlock.references`), then deletes
//! every on-disk blob not in the marked set.

use std::collections::HashSet;
use std::path::Path;

use sunset_store::{Error, Hash, Result};
use tokio_rusqlite::rusqlite::{self, params};

use crate::blobs;

/// Returns all `value_hash`es currently in the entries table — the GC roots.
pub fn read_roots(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<Hash>> {
    let mut stmt = conn.prepare("SELECT value_hash FROM entries")?;
    let rows = stmt.query_map(params![], |row| {
        let bytes: Vec<u8> = row.get(0)?;
        let mut buf = [0u8; 32];
        if bytes.len() != 32 {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                bytes.len(),
                rusqlite::types::Type::Blob,
                Box::<dyn std::error::Error + Send + Sync>::from("value_hash != 32 bytes"),
            ));
        }
        buf.copy_from_slice(&bytes);
        Ok(Hash::from(buf))
    })?;
    rows.collect()
}

/// Iteratively walk content references from `roots` (DFS via explicit stack).
pub async fn reachable_set(root: &Path, roots: Vec<Hash>) -> Result<HashSet<Hash>> {
    let mut visited: HashSet<Hash> = HashSet::new();
    let mut stack = roots;
    while let Some(h) = stack.pop() {
        if !visited.insert(h) {
            continue;
        }
        if let Some(block) = blobs::read_blob(root, &h).await? {
            for r in block.references {
                if !visited.contains(&r) {
                    stack.push(r);
                }
            }
        }
        // If absent: lazy dangling ref; skip silently.
    }
    Ok(visited)
}

/// Returns the hashes of blobs that were deleted (so the caller can emit
/// `BlobRemoved` events for each).
pub async fn mark_and_sweep(root: &Path, roots: Vec<Hash>) -> Result<Vec<Hash>> {
    let reachable = reachable_set(root, roots).await?;
    let on_disk = blobs::list_blob_hashes(root).await?;
    let mut removed = Vec::new();
    for h in on_disk {
        if reachable.contains(&h) {
            continue;
        }
        let path = blobs::blob_path(root, &h);
        match tokio::fs::remove_file(&path).await {
            Ok(_) => removed.push(h),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::Backend(format!("rm blob: {e}"))),
        }
    }
    Ok(removed)
}
