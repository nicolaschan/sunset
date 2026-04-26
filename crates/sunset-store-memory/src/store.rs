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

    async fn insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()> {
        // 1. Hash-match check.
        if let Some(b) = &blob {
            if b.hash() != entry.value_hash {
                return Err(Error::HashMismatch);
            }
        }
        // 2. Signature verification.
        self.verifier.verify(&entry)?;
        // 3. LWW + atomic insert.
        let mut inner = self.inner.lock().await;
        let key: KvKey = (entry.verifying_key.clone(), entry.name.clone());
        if let Some(existing) = inner.entries.get(&key) {
            if existing.entry.priority >= entry.priority {
                return Err(Error::Stale);
            }
        }
        // Atomic: insert blob first (idempotent), then KV row.
        if let Some(b) = blob {
            inner.blobs.entry(entry.value_hash).or_insert(b);
        }
        let sequence = inner.assign_sequence();
        inner.entries.insert(key, StoredEntry { entry, sequence });
        Ok(())
    }

    async fn get_entry(&self, vk: &VerifyingKey, name: &[u8]) -> Result<Option<SignedKvEntry>> {
        let inner = self.inner.lock().await;
        let key = (vk.clone(), bytes::Bytes::copy_from_slice(name));
        Ok(inner.entries.get(&key).map(|s| s.entry.clone()))
    }

    async fn iter<'a>(&'a self, filter: sunset_store::Filter) -> Result<sunset_store::EntryStream<'a>> {
        // Snapshot current matching entries to avoid holding the lock during streaming.
        let inner = self.inner.lock().await;
        let matching: Vec<SignedKvEntry> = inner
            .entries
            .iter()
            .filter(|((vk, name), _)| filter.matches(vk, name.as_ref()))
            .map(|(_, stored)| stored.entry.clone())
            .collect();
        drop(inner);
        let stream = futures::stream::iter(matching.into_iter().map(Ok));
        Ok(Box::pin(stream))
    }
    // ===== to be filled in subsequent tasks =====
    async fn subscribe<'a>(&'a self, _filter: sunset_store::Filter, _replay: sunset_store::Replay) -> Result<sunset_store::EventStream<'a>> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn delete_expired(&self, now: u64) -> Result<usize> {
        let mut inner = self.inner.lock().await;
        // Collect keys to remove first; we mutate after.
        let to_remove: Vec<KvKey> = inner
            .entries
            .iter()
            .filter(|(_, stored)| stored.entry.expires_at.is_some_and(|e| e <= now))
            .map(|(k, _)| k.clone())
            .collect();
        let count = to_remove.len();
        for key in to_remove {
            inner.entries.remove(&key);
        }
        Ok(count)
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

    use sunset_store::{AcceptAllVerifier, Filter, Replay};

    fn vk(b: &'static [u8]) -> VerifyingKey { VerifyingKey::new(bytes::Bytes::from_static(b)) }
    fn n(b: &'static [u8]) -> bytes::Bytes { bytes::Bytes::from_static(b) }

    fn entry_pointing_to(block: &ContentBlock, vk_bytes: &'static [u8], name: &'static [u8], priority: u64) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name:          n(name),
            value_hash:    block.hash(),
            priority,
            expires_at:    None,
            signature:     bytes::Bytes::from_static(b"sig"),
        }
    }

    fn small_block(payload: &'static [u8]) -> ContentBlock {
        ContentBlock { data: bytes::Bytes::from_static(payload), references: vec![] }
    }

    #[tokio::test]
    async fn insert_then_get_entry_roundtrip() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"hello");
        let entry = entry_pointing_to(&block, b"alice", b"room/x", 1);
        store.insert(entry.clone(), Some(block)).await.unwrap();
        let back = store.get_entry(&vk(b"alice"), b"room/x").await.unwrap().unwrap();
        assert_eq!(back, entry);
    }

    #[tokio::test]
    async fn insert_rejects_hash_mismatch() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"hello");
        let mut entry = entry_pointing_to(&block, b"alice", b"r", 1);
        entry.value_hash = Hash::from_bytes([0u8; 32]);
        let other_block = small_block(b"goodbye");
        assert!(matches!(
            store.insert(entry, Some(other_block)).await,
            Err(Error::HashMismatch)
        ));
    }

    #[tokio::test]
    async fn insert_rejects_lower_or_equal_priority() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        let first = entry_pointing_to(&block, b"alice", b"r", 5);
        store.insert(first.clone(), Some(block.clone())).await.unwrap();

        // Equal priority -> Stale.
        let same = entry_pointing_to(&block, b"alice", b"r", 5);
        assert!(matches!(store.insert(same, Some(block.clone())).await, Err(Error::Stale)));

        // Lower priority -> Stale.
        let lower = entry_pointing_to(&block, b"alice", b"r", 4);
        assert!(matches!(store.insert(lower, Some(block.clone())).await, Err(Error::Stale)));
    }

    #[tokio::test]
    async fn insert_replaces_with_higher_priority() {
        let store = MemoryStore::with_accept_all();
        let block_v1 = small_block(b"v1");
        let block_v2 = small_block(b"v2");

        let v1 = entry_pointing_to(&block_v1, b"alice", b"r", 1);
        let v2 = entry_pointing_to(&block_v2, b"alice", b"r", 2);
        store.insert(v1, Some(block_v1)).await.unwrap();
        store.insert(v2.clone(), Some(block_v2.clone())).await.unwrap();

        let current = store.get_entry(&vk(b"alice"), b"r").await.unwrap().unwrap();
        assert_eq!(current, v2);
    }

    #[tokio::test]
    async fn insert_lazy_ref_succeeds_without_blob() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"future");
        let entry = entry_pointing_to(&block, b"alice", b"r", 1);
        // Insert entry only; blob is not yet here.
        store.insert(entry, None).await.unwrap();
        // Reading the blob via its hash returns None until it arrives.
        assert!(store.get_content(&block.hash()).await.unwrap().is_none());
        // Later, the blob can be put separately.
        store.put_content(block.clone()).await.unwrap();
        assert!(store.get_content(&block.hash()).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn insert_calls_signature_verifier() {
        struct RejectAll;
        impl SignatureVerifier for RejectAll {
            fn verify(&self, _e: &SignedKvEntry) -> sunset_store::Result<()> {
                Err(sunset_store::Error::SignatureInvalid)
            }
        }
        let store = MemoryStore::new(Arc::new(RejectAll));
        let block = small_block(b"x");
        let entry = entry_pointing_to(&block, b"alice", b"r", 1);
        assert!(matches!(
            store.insert(entry, Some(block)).await,
            Err(Error::SignatureInvalid)
        ));
    }

    use futures::StreamExt;

    async fn collect_iter(store: &MemoryStore, filter: Filter) -> Vec<SignedKvEntry> {
        let mut s = store.iter(filter).await.unwrap();
        let mut out = vec![];
        while let Some(item) = s.next().await {
            out.push(item.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn iter_keyspace_returns_only_matching_writer() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"alice", b"a", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"alice", b"b", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"bob",   b"a", 1), Some(block.clone())).await.unwrap();

        let results = collect_iter(&store, Filter::Keyspace(vk(b"alice"))).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.verifying_key == vk(b"alice")));
    }

    #[tokio::test]
    async fn iter_namespace_returns_all_writers_at_name() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"alice", b"room/g", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"bob",   b"room/g", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"alice", b"room/h", 1), Some(block.clone())).await.unwrap();

        let results = collect_iter(&store, Filter::Namespace(n(b"room/g"))).await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn iter_name_prefix_matches_prefix() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"a", b"room/g", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"a", b"room/h", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"a", b"presence/x", 1), Some(block.clone())).await.unwrap();

        let results = collect_iter(&store, Filter::NamePrefix(n(b"room/"))).await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn iter_specific_returns_at_most_one() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"a", b"x", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"b", b"x", 1), Some(block.clone())).await.unwrap();

        let results = collect_iter(&store, Filter::Specific(vk(b"a"), n(b"x"))).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verifying_key, vk(b"a"));
    }

    #[tokio::test]
    async fn iter_union_is_or() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"a", b"room/g", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"b", b"presence/x", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"c", b"unrelated", 1), Some(block.clone())).await.unwrap();

        let f = Filter::Union(vec![
            Filter::NamePrefix(n(b"room/")),
            Filter::NamePrefix(n(b"presence/")),
        ]);
        let results = collect_iter(&store, f).await;
        assert_eq!(results.len(), 2);
    }

    fn entry_with_expiry(block: &ContentBlock, vk_bytes: &'static [u8], name: &'static [u8], priority: u64, expires_at: u64) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name:          n(name),
            value_hash:    block.hash(),
            priority,
            expires_at:    Some(expires_at),
            signature:     bytes::Bytes::from_static(b"sig"),
        }
    }

    #[tokio::test]
    async fn delete_expired_removes_only_past_entries() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_with_expiry(&block, b"a", b"old", 1, 100), Some(block.clone())).await.unwrap();
        store.insert(entry_with_expiry(&block, b"a", b"future", 1, 1000), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"a", b"forever", 1), Some(block.clone())).await.unwrap();

        let removed = store.delete_expired(500).await.unwrap();
        assert_eq!(removed, 1);
        assert!(store.get_entry(&vk(b"a"), b"old").await.unwrap().is_none());
        assert!(store.get_entry(&vk(b"a"), b"future").await.unwrap().is_some());
        assert!(store.get_entry(&vk(b"a"), b"forever").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_expired_at_boundary_includes_equal() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_with_expiry(&block, b"a", b"x", 1, 100), Some(block.clone())).await.unwrap();
        let removed = store.delete_expired(100).await.unwrap();
        assert_eq!(removed, 1);
    }
}
