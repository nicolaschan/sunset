//! FsStore + Store impl.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use sunset_store::{
    AcceptAllVerifier, ContentBlock, Cursor, EntryStream, Error, Event, EventStream, Filter, Hash,
    Replay, Result, SignatureVerifier, SignedKvEntry, Store, VerifyingKey,
};
use tokio::sync::Mutex;
use tokio_rusqlite::Connection;

use crate::schema;
use crate::subscription::SubscriptionList;
use crate::{blobs, kv};

pub struct FsStore {
    pub(crate) root: Arc<PathBuf>,
    pub(crate) conn: Connection,
    pub(crate) verifier: Arc<dyn SignatureVerifier>,
    pub(crate) subscriptions: Arc<SubscriptionList>,
    pub(crate) writer_mutex: Arc<Mutex<()>>,
}

impl FsStore {
    /// Open or create an FsStore rooted at `root`. Creates `root/content/`
    /// and `root/db.sqlite`, applies the schema, and returns a ready-to-use
    /// store. Default verifier is `AcceptAllVerifier`; use
    /// `with_verifier` to override.
    pub async fn new<P: AsRef<Path>>(root: P) -> Result<Self> {
        Self::with_verifier(root, Arc::new(AcceptAllVerifier)).await
    }

    pub async fn with_verifier<P: AsRef<Path>>(
        root: P,
        verifier: Arc<dyn SignatureVerifier>,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let content_dir = root.join("content");
        let db_path = root.join("db.sqlite");

        // Sync I/O: startup-only, single call. Adding tokio's `fs` feature is
        // not worth the dependency cost for one mkdir at startup.
        std::fs::create_dir_all(&content_dir)
            .map_err(|e| Error::Backend(format!("create content dir: {e}")))?;

        let conn = Connection::open(&db_path)
            .await
            .map_err(|e| Error::Backend(format!("open sqlite: {e}")))?;

        conn.call(|c| schema::apply_schema(c).map_err(tokio_rusqlite::Error::from))
            .await
            .map_err(|e| Error::Backend(format!("apply schema: {e}")))?;

        Ok(Self {
            root: Arc::new(root),
            conn,
            verifier,
            subscriptions: Arc::new(SubscriptionList::new()),
            writer_mutex: Arc::new(Mutex::new(())),
        })
    }
}

/// Convert `tokio_rusqlite::Error<sunset_store::Error>` back to our `Error`.
fn unwrap_store_error(e: tokio_rusqlite::Error<Error>) -> Error {
    match e {
        tokio_rusqlite::Error::Error(store_err) => store_err,
        other => Error::Backend(format!("sqlite: {other}")),
    }
}

#[async_trait(?Send)]
impl Store for FsStore {
    async fn insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()> {
        let _w = self.writer_mutex.lock().await;

        if let Some(b) = &blob {
            if entry.value_hash != b.hash() {
                return Err(Error::HashMismatch);
            }
        }
        self.verifier
            .verify(&entry)
            .map_err(|_| Error::SignatureInvalid)?;

        // Persist the blob first (idempotent, content-addressed). Lazy refs are
        // allowed by spec, so a subsequent SQLite failure leaves at most an
        // orphaned blob, which gc_blobs reclaims later.
        let blob_was_new = if let Some(b) = &blob {
            blobs::write_blob_atomic(&self.root, b).await?
        } else {
            false
        };

        let entry_clone = entry.clone();
        let outcome: kv::InsertOutcome = self
            .conn
            .call(move |c| -> std::result::Result<kv::InsertOutcome, Error> {
                let txn = c
                    .transaction()
                    .map_err(|e| Error::Backend(format!("begin transaction: {e}")))?;
                let outcome = kv::insert_lww(&txn, &entry_clone)?;
                txn.commit()
                    .map_err(|e| Error::Backend(format!("commit transaction: {e}")))?;
                Ok(outcome)
            })
            .await
            .map_err(unwrap_store_error)?;

        // Broadcasts are sent under the writer_mutex by virtue of `_w` above —
        // do not drop the guard before this block.
        match outcome {
            kv::InsertOutcome::Inserted { .. } => {
                self.subscriptions.broadcast(&Event::Inserted(entry));
            }
            kv::InsertOutcome::Replaced { old, .. } => {
                self.subscriptions
                    .broadcast(&Event::Replaced { old, new: entry });
            }
        }
        if blob_was_new {
            if let Some(b) = blob {
                self.subscriptions.broadcast(&Event::BlobAdded(b.hash()));
            }
        }

        Ok(())
    }

    async fn put_content(&self, block: ContentBlock) -> Result<Hash> {
        let _w = self.writer_mutex.lock().await;
        let hash = block.hash();
        if blobs::write_blob_atomic(&self.root, &block).await? {
            self.subscriptions.broadcast(&Event::BlobAdded(hash));
        }
        Ok(hash)
    }

    async fn get_content(&self, hash: &Hash) -> Result<Option<ContentBlock>> {
        blobs::read_blob(&self.root, hash).await
    }

    async fn get_entry(&self, vk: &VerifyingKey, name: &[u8]) -> Result<Option<SignedKvEntry>> {
        let vk = vk.clone();
        let name = name.to_vec();
        self.conn
            .call(move |c| kv::get_entry(c, &vk, &name))
            .await
            .map_err(|e| Error::Backend(format!("get_entry: {e}")))
    }

    async fn iter<'a>(&'a self, filter: Filter) -> Result<EntryStream<'a>> {
        let entries = self
            .conn
            .call(move |c| -> std::result::Result<Vec<SignedKvEntry>, Error> {
                kv::iter_with_filter(c, &filter)
            })
            .await
            .map_err(unwrap_store_error)?;
        let stream = async_stream::stream! {
            for e in entries {
                yield Ok(e);
            }
        };
        Ok(Box::pin(stream))
    }

    async fn subscribe<'a>(&'a self, _filter: Filter, _replay: Replay) -> Result<EventStream<'a>> {
        // implemented in Task 8
        unimplemented!("subscribe — implemented in Task 8")
    }

    async fn delete_expired(&self, now: u64) -> Result<usize> {
        let _w = self.writer_mutex.lock().await;
        let victims: Vec<SignedKvEntry> = self
            .conn
            .call(move |c| -> std::result::Result<Vec<SignedKvEntry>, Error> {
                let txn = c
                    .transaction()
                    .map_err(|e| Error::Backend(format!("begin transaction: {e}")))?;
                let v = kv::delete_expired(&txn, now)?;
                txn.commit()
                    .map_err(|e| Error::Backend(format!("commit transaction: {e}")))?;
                Ok(v)
            })
            .await
            .map_err(unwrap_store_error)?;
        let count = victims.len();
        for e in victims {
            self.subscriptions.broadcast(&Event::Expired(e));
        }
        Ok(count)
    }

    async fn gc_blobs(&self) -> Result<usize> {
        let _w = self.writer_mutex.lock().await;
        let roots = self
            .conn
            .call(|c| crate::gc::read_roots(c).map_err(tokio_rusqlite::Error::from))
            .await
            .map_err(|e| Error::Backend(format!("gc roots: {e}")))?;
        let removed = crate::gc::mark_and_sweep(&self.root, roots).await?;
        let count = removed.len();
        for h in removed {
            self.subscriptions.broadcast(&Event::BlobRemoved(h));
        }
        Ok(count)
    }

    async fn current_cursor(&self) -> Result<Cursor> {
        self.conn
            .call(|c| kv::current_cursor(c))
            .await
            .map_err(|e| Error::Backend(format!("current_cursor: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn new_creates_directory_and_database() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        assert!(dir.path().join("content").is_dir());
        assert!(dir.path().join("db.sqlite").is_file());
        // Re-opening the same path must succeed (idempotent DDL).
        drop(store);
        let _store2 = FsStore::new(dir.path()).await.unwrap();
    }
}

#[cfg(test)]
mod iter_tests {
    use super::*;
    use bytes::Bytes;
    use futures::StreamExt;
    use sunset_store::{ContentBlock, SignedKvEntry, VerifyingKey};
    use tempfile::TempDir;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey(Bytes::copy_from_slice(b))
    }
    fn block(d: &[u8]) -> ContentBlock {
        ContentBlock {
            data: Bytes::copy_from_slice(d),
            references: vec![],
        }
    }
    fn entry(vk_bytes: &[u8], name: &[u8], priority: u64, blob: &ContentBlock) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name: Bytes::copy_from_slice(name),
            value_hash: blob.hash(),
            priority,
            expires_at: None,
            signature: Bytes::copy_from_slice(b"sig"),
        }
    }

    #[tokio::test]
    async fn iter_keyspace_returns_only_matching_writer() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b = block(b"v");
        store
            .insert(entry(b"a", b"k1", 1, &b), Some(b.clone()))
            .await
            .unwrap();
        store
            .insert(entry(b"a", b"k2", 1, &b), Some(b.clone()))
            .await
            .unwrap();
        store
            .insert(entry(b"b", b"k1", 1, &b), Some(b))
            .await
            .unwrap();
        let got: Vec<_> = store
            .iter(Filter::Keyspace(vk(b"a")))
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|e| e.verifying_key == vk(b"a")));
    }
}

#[cfg(test)]
mod insert_tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::{ContentBlock, SignedKvEntry, VerifyingKey};
    use tempfile::TempDir;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey(Bytes::copy_from_slice(b))
    }
    fn block(d: &[u8]) -> ContentBlock {
        ContentBlock {
            data: Bytes::copy_from_slice(d),
            references: vec![],
        }
    }
    fn entry(vk_bytes: &[u8], name: &[u8], priority: u64, blob: &ContentBlock) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name: Bytes::copy_from_slice(name),
            value_hash: blob.hash(),
            priority,
            expires_at: None,
            signature: Bytes::copy_from_slice(b"sig"),
        }
    }

    #[tokio::test]
    async fn insert_then_get_entry() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b = block(b"v");
        let e = entry(b"a", b"k", 1, &b);
        store.insert(e.clone(), Some(b)).await.unwrap();
        let got = store.get_entry(&vk(b"a"), b"k").await.unwrap().unwrap();
        assert_eq!(got, e);
    }

    #[tokio::test]
    async fn insert_lww_higher_priority_wins() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b1 = block(b"v1");
        let b2 = block(b"v2");
        store
            .insert(entry(b"a", b"k", 1, &b1), Some(b1))
            .await
            .unwrap();
        store
            .insert(entry(b"a", b"k", 2, &b2.clone()), Some(b2.clone()))
            .await
            .unwrap();
        let got = store.get_entry(&vk(b"a"), b"k").await.unwrap().unwrap();
        assert_eq!(got.priority, 2);
    }

    #[tokio::test]
    async fn insert_lww_equal_priority_is_stale() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b = block(b"v");
        store
            .insert(entry(b"a", b"k", 1, &b), Some(b.clone()))
            .await
            .unwrap();
        let err = store
            .insert(entry(b"a", b"k", 1, &b), Some(b))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Stale));
    }

    #[tokio::test]
    async fn insert_rejects_hash_mismatch() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b1 = block(b"v1");
        let b2 = block(b"v2");
        let mut e = entry(b"a", b"k", 1, &b1);
        e.value_hash = b1.hash();
        // supply b2 (whose hash differs) — must be rejected.
        let err = store.insert(e, Some(b2)).await.unwrap_err();
        assert!(matches!(err, Error::HashMismatch));
    }

    #[tokio::test]
    async fn current_cursor_advances_with_inserts() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        assert_eq!(store.current_cursor().await.unwrap(), Cursor(1));
        let b = block(b"v");
        store
            .insert(entry(b"a", b"k", 1, &b), Some(b))
            .await
            .unwrap();
        assert_eq!(store.current_cursor().await.unwrap(), Cursor(2));
    }

    #[tokio::test]
    async fn delete_expired_removes_at_boundary() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b = block(b"v");
        let mut e = entry(b"a", b"k", 1, &b);
        e.expires_at = Some(100);
        store.insert(e, Some(b)).await.unwrap();
        let n = store.delete_expired(100).await.unwrap();
        assert_eq!(n, 1);
        assert!(store.get_entry(&vk(b"a"), b"k").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn entries_persist_across_reopen() {
        let dir = TempDir::new().unwrap();
        let b = block(b"v");
        let e = entry(b"a", b"k", 1, &b);
        {
            let store = FsStore::new(dir.path()).await.unwrap();
            store.insert(e.clone(), Some(b)).await.unwrap();
        }
        let store2 = FsStore::new(dir.path()).await.unwrap();
        let got = store2.get_entry(&vk(b"a"), b"k").await.unwrap().unwrap();
        assert_eq!(got, e);
    }
}

#[cfg(test)]
mod gc_tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::{ContentBlock, SignedKvEntry, VerifyingKey};
    use tempfile::TempDir;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey(Bytes::copy_from_slice(b))
    }
    fn block(d: &[u8]) -> ContentBlock {
        ContentBlock {
            data: Bytes::copy_from_slice(d),
            references: vec![],
        }
    }
    fn entry(vk_bytes: &[u8], name: &[u8], priority: u64, blob: &ContentBlock) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name: Bytes::copy_from_slice(name),
            value_hash: blob.hash(),
            priority,
            expires_at: None,
            signature: Bytes::copy_from_slice(b"sig"),
        }
    }

    #[tokio::test]
    async fn gc_blobs_continues_past_corrupt_reachable_blob() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        // Insert a normal blob + entry referencing it (the "good root").
        let b_good = block(b"good");
        store
            .insert(entry(b"a", b"k", 1, &b_good), Some(b_good.clone()))
            .await
            .unwrap();
        // Insert a second blob that's a root, then corrupt it on disk.
        let b_corrupt = block(b"to-be-corrupted");
        store
            .insert(entry(b"a", b"k2", 1, &b_corrupt), Some(b_corrupt.clone()))
            .await
            .unwrap();
        // Corrupt the on-disk blob (overwrite with garbage).
        let hex = b_corrupt.hash().to_hex();
        let corrupt_path = dir.path().join("content").join(&hex[0..2]).join(&hex[2..]);
        std::fs::write(&corrupt_path, b"garbage-not-a-valid-postcard").unwrap();
        // Add an unrelated orphan blob.
        let b_orphan = block(b"orphan");
        store.put_content(b_orphan.clone()).await.unwrap();
        // GC should NOT abort due to the corrupt blob; it should still sweep the orphan.
        let n = store.gc_blobs().await.unwrap();
        assert_eq!(
            n, 1,
            "orphan must be reclaimed despite corrupt reachable blob"
        );
        assert!(store.get_content(&b_orphan.hash()).await.unwrap().is_none());
        // The good blob remains (it was a leaf with no references; not corrupted).
        assert!(store.get_content(&b_good.hash()).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn gc_blobs_keeps_reachable_drops_orphans() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b_used = block(b"used");
        let b_orphan = block(b"orphan");
        store.put_content(b_orphan.clone()).await.unwrap();
        store
            .insert(entry(b"a", b"k", 1, &b_used), Some(b_used.clone()))
            .await
            .unwrap();
        let n = store.gc_blobs().await.unwrap();
        assert_eq!(n, 1);
        assert!(store.get_content(&b_used.hash()).await.unwrap().is_some());
        assert!(store.get_content(&b_orphan.hash()).await.unwrap().is_none());
    }
}
