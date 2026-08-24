//! FsStore + Store impl.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use sunset_store::{
    AcceptAllVerifier, ContentBlock, Cursor, EntryStream, Error, Event, EventStream, Filter, Hash,
    InsertCommitter, InsertOutcome, Replay, Result, SignatureVerifier, SignedKvEntry, Store,
    Subscription, SubscriptionList, VerifyingKey, run_insert,
};
use tokio::sync::Mutex;
use tokio_rusqlite::Connection;

use crate::schema;
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

        conn.call(|c| {
            c.execute_batch(schema::SCHEMA_DDL)
                .map_err(tokio_rusqlite::Error::from)
        })
        .await
        .map_err(|e| Error::Backend(format!("apply schema: {e}")))?;

        Ok(Self {
            root: Arc::new(root),
            conn,
            verifier,
            subscriptions: Arc::new(SubscriptionList::default()),
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
impl InsertCommitter for FsStore {
    async fn commit_insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()> {
        let _w = self.writer_mutex.lock().await;

        // Persist the blob first (idempotent, content-addressed). A subsequent
        // SQLite failure therefore leaves at most an orphaned blob on disk,
        // never an entry whose blob is missing from a peer that already has it.
        let blob_added = match &blob {
            Some(b) if blobs::write_blob_atomic(&self.root, b).await? => Some(b.hash()),
            _ => None,
        };

        let entry_clone = entry.clone();
        let outcome: InsertOutcome = self
            .conn
            .call(move |c| -> std::result::Result<InsertOutcome, Error> {
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

        // Broadcasts run WHILE holding `_w` above — do not drop the guard
        // before this block.
        self.subscriptions
            .publish_insert(outcome, entry, blob_added);

        Ok(())
    }
}

#[async_trait(?Send)]
impl Store for FsStore {
    async fn insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()> {
        run_insert(self, &*self.verifier, entry, blob).await
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

    async fn subscribe<'a>(&'a self, filter: Filter, replay: Replay) -> Result<EventStream<'a>> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<Event>>();
        let sub = Arc::new(Subscription {
            filter: filter.clone(),
            tx,
        });

        // Take history snapshot AND register subscription under the writer mutex,
        // so any concurrent insert is serialized and either lands in the snapshot
        // or in the live channel — never both.
        let _w = self.writer_mutex.lock().await;

        let history: Vec<SignedKvEntry> = match replay {
            Replay::None => Vec::new(),
            Replay::All => self
                .conn
                .call({
                    let f = filter.clone();
                    move |c| -> std::result::Result<Vec<SignedKvEntry>, Error> {
                        kv::iter_with_filter(c, &f)
                    }
                })
                .await
                .map_err(unwrap_store_error)?,
        };

        self.subscriptions.add(&sub);
        drop(_w);

        let stream = async_stream::stream! {
            for e in history {
                yield Ok(Event::Inserted(e));
            }
            let _keep_alive = sub;
            while let Some(item) = rx.recv().await {
                yield item;
            }
        };
        Ok(Box::pin(stream))
    }

    async fn current_cursor(&self) -> Result<Cursor> {
        self.conn
            .call(|c| kv::current_cursor(c))
            .await
            .map_err(|e| Error::Backend(format!("current_cursor: {e}")))
    }

    fn verifier(&self) -> Arc<dyn SignatureVerifier> {
        self.verifier.clone()
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
    use futures::StreamExt;
    use sunset_store::test_helpers::{block, entry, vk};
    use tempfile::TempDir;

    #[tokio::test]
    async fn iter_keyspace_returns_only_matching_writer() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b = block(b"v");
        store
            .insert(entry(&b, b"a", b"k1", 1), Some(b.clone()))
            .await
            .unwrap();
        store
            .insert(entry(&b, b"a", b"k2", 1), Some(b.clone()))
            .await
            .unwrap();
        store
            .insert(entry(&b, b"b", b"k1", 1), Some(b))
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
    use sunset_store::test_helpers::{block, entry, vk};
    use tempfile::TempDir;

    #[tokio::test]
    async fn insert_then_get_entry() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b = block(b"v");
        let e = entry(&b, b"a", b"k", 1);
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
            .insert(entry(&b1, b"a", b"k", 1), Some(b1))
            .await
            .unwrap();
        store
            .insert(entry(&b2, b"a", b"k", 2), Some(b2.clone()))
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
            .insert(entry(&b, b"a", b"k", 1), Some(b.clone()))
            .await
            .unwrap();
        let err = store
            .insert(entry(&b, b"a", b"k", 1), Some(b))
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
        let mut e = entry(&b1, b"a", b"k", 1);
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
            .insert(entry(&b, b"a", b"k", 1), Some(b))
            .await
            .unwrap();
        assert_eq!(store.current_cursor().await.unwrap(), Cursor(2));
    }

    #[tokio::test]
    async fn entries_persist_across_reopen() {
        let dir = TempDir::new().unwrap();
        let b = block(b"v");
        let e = entry(&b, b"a", b"k", 1);
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
mod subscribe_tests {
    use super::*;
    use futures::StreamExt;
    use sunset_store::test_helpers::{block, entry, vk};
    use tempfile::TempDir;

    #[tokio::test]
    async fn subscribe_replay_all_then_live() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b1 = block(b"v1");
        store
            .insert(entry(&b1, b"a", b"k1", 1), Some(b1))
            .await
            .unwrap();
        let mut s = store
            .subscribe(Filter::Keyspace(vk(b"a")), Replay::All)
            .await
            .unwrap();
        let first = tokio::time::timeout(std::time::Duration::from_millis(200), s.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(first, Event::Inserted(_)));

        let b2 = block(b"v2");
        store
            .insert(entry(&b2, b"a", b"k2", 1), Some(b2.clone()))
            .await
            .unwrap();
        // The next event the subscriber receives that is NOT BlobAdded should be Inserted for k2.
        loop {
            let evt = tokio::time::timeout(std::time::Duration::from_millis(200), s.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if matches!(evt, Event::Inserted(_)) {
                if let Event::Inserted(e) = evt {
                    assert_eq!(e.name.as_ref(), b"k2");
                    break;
                }
            }
        }
    }
}
