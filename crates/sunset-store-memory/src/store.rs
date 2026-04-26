//! In-memory implementation of `sunset-store::Store`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use sunset_store::{
    ContentBlock, Cursor, Hash, SignedKvEntry, SignatureVerifier, VerifyingKey,
};
use tokio::sync::Mutex;

/// Composite key: `(verifying_key, name)`.
type KvKey = (VerifyingKey, bytes::Bytes);

#[derive(Debug)]
pub(crate) struct StoredEntry {
    pub entry:    SignedKvEntry,
    pub sequence: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Inner {
    pub entries:      BTreeMap<KvKey, StoredEntry>,
    pub blobs:        HashMap<Hash, ContentBlock>,
    pub next_sequence: u64,
}

impl Inner {
    pub fn assign_sequence(&mut self) -> u64 {
        let s = self.next_sequence;
        self.next_sequence += 1;
        s
    }
}

/// In-memory `Store` implementation.
pub struct MemoryStore {
    pub(crate) verifier: Arc<dyn SignatureVerifier>,
    pub(crate) inner:    Arc<Mutex<Inner>>,
}

impl MemoryStore {
    /// Construct with the given signature verifier.
    pub fn new(verifier: Arc<dyn SignatureVerifier>) -> Self {
        Self {
            verifier,
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Convenience: construct with `AcceptAllVerifier`. For tests.
    pub fn with_accept_all() -> Self {
        Self::new(Arc::new(sunset_store::AcceptAllVerifier))
    }

    /// Returns the current cursor (last assigned sequence).
    pub async fn current_cursor_now(&self) -> Cursor {
        let inner = self.inner.lock().await;
        Cursor(inner.next_sequence)
    }
}

use async_trait::async_trait;
use sunset_store::{Error, Result, Store};

#[async_trait(?Send)]
impl Store for MemoryStore {
    async fn put_content(&self, block: ContentBlock) -> Result<Hash> {
        let hash = block.hash();
        let mut inner = self.inner.lock().await;
        inner.blobs.entry(hash).or_insert(block);
        Ok(hash)
    }

    async fn get_content(&self, hash: &Hash) -> Result<Option<ContentBlock>> {
        let inner = self.inner.lock().await;
        Ok(inner.blobs.get(hash).cloned())
    }

    // ===== to be filled in subsequent tasks =====
    async fn insert(&self, _entry: SignedKvEntry, _blob: Option<ContentBlock>) -> Result<()> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn get_entry(&self, _vk: &VerifyingKey, _name: &[u8]) -> Result<Option<SignedKvEntry>> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn iter<'a>(&'a self, _filter: sunset_store::Filter) -> Result<sunset_store::EntryStream<'a>> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn subscribe<'a>(&'a self, _filter: sunset_store::Filter, _replay: sunset_store::Replay) -> Result<sunset_store::EventStream<'a>> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn delete_expired(&self, _now: u64) -> Result<usize> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn gc_blobs(&self) -> Result<usize> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn current_cursor(&self) -> Result<Cursor> {
        Ok(self.current_cursor_now().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunset_store::Store;

    #[tokio::test]
    async fn new_store_starts_at_cursor_zero() {
        let store = MemoryStore::with_accept_all();
        assert_eq!(store.current_cursor_now().await, Cursor(0));
    }

    #[tokio::test]
    async fn put_then_get_content_roundtrip() {
        let store = MemoryStore::with_accept_all();
        let block = ContentBlock {
            data: bytes::Bytes::from_static(b"hello"),
            references: vec![],
        };
        let h = store.put_content(block.clone()).await.unwrap();
        assert_eq!(h, block.hash());
        let back = store.get_content(&h).await.unwrap().unwrap();
        assert_eq!(back, block);
    }

    #[tokio::test]
    async fn get_content_returns_none_for_unknown_hash() {
        let store = MemoryStore::with_accept_all();
        let h = Hash::from_bytes([7u8; 32]);
        assert!(store.get_content(&h).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn put_content_is_idempotent_on_same_block() {
        let store = MemoryStore::with_accept_all();
        let block = ContentBlock {
            data: bytes::Bytes::from_static(b"x"),
            references: vec![],
        };
        let h1 = store.put_content(block.clone()).await.unwrap();
        let h2 = store.put_content(block.clone()).await.unwrap();
        assert_eq!(h1, h2);
    }
}
