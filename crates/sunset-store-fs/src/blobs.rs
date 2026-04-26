//! Filesystem blob layer.

#![allow(dead_code)] // Functions used by Tasks 4 (insert) and 7 (gc_blobs); remove when integrated.

use std::path::{Path, PathBuf};

use sunset_store::{ContentBlock, Error, Hash, Result};
use tempfile::NamedTempFile;

/// Path on disk for a given content hash, under the store root's `content/` dir.
pub fn blob_path(root: &Path, hash: &Hash) -> PathBuf {
    let hex = hash.to_hex();
    root.join("content").join(&hex[0..2]).join(&hex[2..])
}

/// Write a content block to its sharded path, atomically. Idempotent: if the
/// file already exists with the same content (which it will, since the path
/// is content-addressed), the rename is a no-op-or-overwrite. Returns `true`
/// if the blob was newly created on disk, `false` if it already existed.
pub async fn write_blob_atomic(root: &Path, block: &ContentBlock) -> Result<bool> {
    let target = blob_path(root, &block.hash());
    if tokio::fs::try_exists(&target)
        .await
        .map_err(|e| Error::Backend(format!("blob exists check: {e}")))?
    {
        return Ok(false);
    }
    let parent = target.parent().expect("blob_path always has parent");
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| Error::Backend(format!("create blob shard dir: {e}")))?;

    let bytes =
        postcard::to_stdvec(block).map_err(|e| Error::Backend(format!("encode block: {e}")))?;
    let parent = parent.to_path_buf();
    let target_clone = target.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut tmp = NamedTempFile::new_in(&parent)
            .map_err(|e| Error::Backend(format!("create temp blob: {e}")))?;
        std::io::Write::write_all(&mut tmp, &bytes)
            .map_err(|e| Error::Backend(format!("write blob: {e}")))?;
        tmp.persist(&target_clone)
            .map_err(|e| Error::Backend(format!("persist blob: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| Error::Backend(format!("blob join: {e}")))??;
    Ok(true)
}

/// Read a content block by hash. Returns `Ok(None)` if absent.
pub async fn read_blob(root: &Path, hash: &Hash) -> Result<Option<ContentBlock>> {
    let path = blob_path(root, hash);
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Backend(format!("read blob: {e}"))),
    };
    let block: ContentBlock =
        postcard::from_bytes(&bytes).map_err(|e| Error::Corrupt(format!("decode blob: {e}")))?;
    if block.hash() != *hash {
        return Err(Error::Corrupt(format!(
            "blob hash mismatch on disk: expected {hash}, got {}",
            block.hash()
        )));
    }
    Ok(Some(block))
}

/// Yield the hash of every blob currently on disk under `root/content/`.
/// Used only by gc.
pub async fn list_blob_hashes(root: &Path) -> Result<Vec<Hash>> {
    let content_dir = root.join("content");
    let mut hashes = Vec::new();
    let mut shard_iter = match tokio::fs::read_dir(&content_dir).await {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(hashes),
        Err(e) => return Err(Error::Backend(format!("read content dir: {e}"))),
    };
    while let Some(shard) = shard_iter
        .next_entry()
        .await
        .map_err(|e| Error::Backend(format!("iter content dir: {e}")))?
    {
        if !shard.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let mut blob_iter = tokio::fs::read_dir(shard.path())
            .await
            .map_err(|e| Error::Backend(format!("read shard dir: {e}")))?;
        while let Some(blob) = blob_iter
            .next_entry()
            .await
            .map_err(|e| Error::Backend(format!("iter shard dir: {e}")))?
        {
            let shard_name = shard.file_name();
            let blob_name = blob.file_name();
            let shard_str = shard_name.to_string_lossy();
            let blob_str = blob_name.to_string_lossy();
            let mut hex = String::with_capacity(64);
            hex.push_str(&shard_str);
            hex.push_str(&blob_str);
            if hex.len() != 64 {
                continue; // not a valid blob filename; skip
            }
            let mut bytes = [0u8; 32];
            if hex::decode_to_slice(&hex, &mut bytes).is_err() {
                continue;
            }
            hashes.push(Hash::from(bytes));
        }
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn block(data: &[u8]) -> ContentBlock {
        ContentBlock {
            data: bytes::Bytes::copy_from_slice(data),
            references: vec![],
        }
    }

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        tokio::fs::create_dir_all(dir.path().join("content"))
            .await
            .unwrap();
        let b = block(b"hello");
        let h = b.hash();
        assert!(write_blob_atomic(dir.path(), &b).await.unwrap());
        let got = read_blob(dir.path(), &h).await.unwrap().unwrap();
        assert_eq!(got, b);
    }

    #[tokio::test]
    async fn write_is_idempotent_on_same_hash() {
        let dir = TempDir::new().unwrap();
        tokio::fs::create_dir_all(dir.path().join("content"))
            .await
            .unwrap();
        let b = block(b"hello");
        assert!(write_blob_atomic(dir.path(), &b).await.unwrap());
        assert!(!write_blob_atomic(dir.path(), &b).await.unwrap()); // already present
    }

    #[tokio::test]
    async fn read_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        let missing = Hash::from([0u8; 32]);
        assert!(read_blob(dir.path(), &missing).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_returns_all_blobs() {
        let dir = TempDir::new().unwrap();
        tokio::fs::create_dir_all(dir.path().join("content"))
            .await
            .unwrap();
        let b1 = block(b"one");
        let b2 = block(b"two");
        write_blob_atomic(dir.path(), &b1).await.unwrap();
        write_blob_atomic(dir.path(), &b2).await.unwrap();
        let mut listed = list_blob_hashes(dir.path()).await.unwrap();
        listed.sort_by_key(|h| *h.as_bytes());
        let mut expected = vec![b1.hash(), b2.hash()];
        expected.sort_by_key(|h| *h.as_bytes());
        assert_eq!(listed, expected);
    }
}
