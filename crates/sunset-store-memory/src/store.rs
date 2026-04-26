//! In-memory implementation of `sunset-store::Store`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use sunset_store::{
    ContentBlock, Cursor, Error, Event, Hash, Result, SignatureVerifier, SignedKvEntry, Store,
    VerifyingKey,
};
use tokio::sync::Mutex;

use crate::subscription::{Subscription, SubscriptionList};

/// Composite key: `(verifying_key, name)`.
type KvKey = (VerifyingKey, bytes::Bytes);

#[derive(Debug)]
pub(crate) struct StoredEntry {
    pub entry: SignedKvEntry,
    pub sequence: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Inner {
    pub entries: BTreeMap<KvKey, StoredEntry>,
    pub blobs: HashMap<Hash, ContentBlock>,
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
    pub(crate) inner: Arc<Mutex<Inner>>,
    pub(crate) subscriptions: Arc<SubscriptionList>,
}

impl MemoryStore {
    /// Construct with the given signature verifier.
    pub fn new(verifier: Arc<dyn SignatureVerifier>) -> Self {
        Self {
            verifier,
            inner: Arc::new(Mutex::new(Inner::default())),
            subscriptions: Arc::new(SubscriptionList::default()),
        }
    }

    /// Convenience: construct with `AcceptAllVerifier`. For tests.
    pub fn with_accept_all() -> Self {
        Self::new(Arc::new(sunset_store::AcceptAllVerifier))
    }

    /// Returns the current cursor (the next-to-be-assigned sequence number;
    /// `Cursor(0)` on a fresh store).
    pub async fn current_cursor_now(&self) -> Cursor {
        let inner = self.inner.lock().await;
        Cursor(inner.next_sequence)
    }
}

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
        if let Some(b) = &blob {
            if b.hash() != entry.value_hash {
                return Err(Error::HashMismatch);
            }
        }
        self.verifier.verify(&entry)?;
        let mut inner = self.inner.lock().await;
        let key: KvKey = (entry.verifying_key.clone(), entry.name.clone());
        let prev = inner.entries.get(&key).map(|s| s.entry.clone());
        if let Some(existing) = &prev {
            if existing.priority >= entry.priority {
                return Err(Error::Stale);
            }
        }
        let blob_added_hash = if let Some(b) = blob {
            let already = inner.blobs.contains_key(&entry.value_hash);
            inner.blobs.entry(entry.value_hash).or_insert(b);
            if already {
                None
            } else {
                Some(entry.value_hash)
            }
        } else {
            None
        };
        let sequence = inner.assign_sequence();
        inner.entries.insert(
            key,
            StoredEntry {
                entry: entry.clone(),
                sequence,
            },
        );
        // Broadcast WHILE holding the inner lock to serialize with subscribe.
        if let Some(old) = prev {
            self.subscriptions
                .broadcast(&Event::Replaced { old, new: entry });
        } else {
            self.subscriptions.broadcast(&Event::Inserted(entry));
        }
        if let Some(h) = blob_added_hash {
            self.subscriptions.broadcast(&Event::BlobAdded(h));
        }
        Ok(())
    }

    async fn get_entry(&self, vk: &VerifyingKey, name: &[u8]) -> Result<Option<SignedKvEntry>> {
        let inner = self.inner.lock().await;
        let key = (vk.clone(), bytes::Bytes::copy_from_slice(name));
        Ok(inner.entries.get(&key).map(|s| s.entry.clone()))
    }

    async fn iter<'a>(
        &'a self,
        filter: sunset_store::Filter,
    ) -> Result<sunset_store::EntryStream<'a>> {
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
    async fn subscribe<'a>(
        &'a self,
        filter: sunset_store::Filter,
        replay: sunset_store::Replay,
    ) -> Result<sunset_store::EventStream<'a>> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sub = Arc::new(Subscription {
            filter: filter.clone(),
            tx,
        });

        // Build the historical replay portion (snapshot under the lock). Register
        // the subscription INSIDE the inner lock so it serializes with broadcasts
        // from insert/delete_expired/gc_blobs (which now happen while the inner
        // lock is held). This prevents a race where an event is delivered both
        // via history replay and via the live channel.
        let historical: Vec<sunset_store::Result<Event>> = {
            let inner = self.inner.lock().await;
            self.subscriptions.add(&sub);
            let mut out: Vec<(u64, Event)> = inner
                .entries
                .iter()
                .filter(|((vk, name), _)| filter.matches(vk, name.as_ref()))
                .filter(|(_, stored)| match replay {
                    sunset_store::Replay::None => false,
                    sunset_store::Replay::All => true,
                    sunset_store::Replay::Since(c) => stored.sequence >= c.0,
                })
                .map(|(_, stored)| (stored.sequence, Event::Inserted(stored.entry.clone())))
                .collect();
            out.sort_by_key(|(s, _)| *s);
            out.into_iter().map(|(_, e)| Ok(e)).collect()
        };

        // Stream historical, then transition to live events from the channel.
        // Hold sub-Arc inside the stream so the weak pointer stays alive.
        let live = async_stream::stream! {
            // (sub kept alive by being moved into closure below; see explicit move)
            let _hold = sub;
            for h in historical { yield h; }
            while let Some(item) = rx.recv().await { yield item; }
        };
        Ok(Box::pin(live))
    }
    async fn delete_expired(&self, now: u64) -> Result<usize> {
        let mut inner = self.inner.lock().await;
        let to_remove: Vec<KvKey> = inner
            .entries
            .iter()
            .filter(|(_, s)| s.entry.expires_at.is_some_and(|e| e <= now))
            .map(|(k, _)| k.clone())
            .collect();
        let mut count = 0;
        for k in to_remove {
            if let Some(s) = inner.entries.remove(&k) {
                self.subscriptions.broadcast(&Event::Expired(s.entry));
                count += 1;
            }
        }
        Ok(count)
    }
    async fn gc_blobs(&self) -> Result<usize> {
        use std::collections::HashSet;
        let mut inner = self.inner.lock().await;
        let mut reachable: HashSet<Hash> = HashSet::new();
        let mut frontier: Vec<Hash> = inner.entries.values().map(|s| s.entry.value_hash).collect();
        while let Some(h) = frontier.pop() {
            if !reachable.insert(h) {
                continue;
            }
            if let Some(block) = inner.blobs.get(&h) {
                for r in &block.references {
                    if !reachable.contains(r) {
                        frontier.push(*r);
                    }
                }
            }
        }
        let to_remove: Vec<Hash> = inner
            .blobs
            .keys()
            .filter(|h| !reachable.contains(h))
            .copied()
            .collect();
        let count = to_remove.len();
        for h in to_remove {
            inner.blobs.remove(&h);
            self.subscriptions.broadcast(&Event::BlobRemoved(h));
        }
        Ok(count)
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

    use sunset_store::{Filter, Replay};

    fn vk(b: &'static [u8]) -> VerifyingKey {
        VerifyingKey::new(bytes::Bytes::from_static(b))
    }
    fn n(b: &'static [u8]) -> bytes::Bytes {
        bytes::Bytes::from_static(b)
    }

    fn entry_pointing_to(
        block: &ContentBlock,
        vk_bytes: &'static [u8],
        name: &'static [u8],
        priority: u64,
    ) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name: n(name),
            value_hash: block.hash(),
            priority,
            expires_at: None,
            signature: bytes::Bytes::from_static(b"sig"),
        }
    }

    fn small_block(payload: &'static [u8]) -> ContentBlock {
        ContentBlock {
            data: bytes::Bytes::from_static(payload),
            references: vec![],
        }
    }

    #[tokio::test]
    async fn insert_then_get_entry_roundtrip() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"hello");
        let entry = entry_pointing_to(&block, b"alice", b"room/x", 1);
        store.insert(entry.clone(), Some(block)).await.unwrap();
        let back = store
            .get_entry(&vk(b"alice"), b"room/x")
            .await
            .unwrap()
            .unwrap();
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
        store
            .insert(first.clone(), Some(block.clone()))
            .await
            .unwrap();

        // Equal priority -> Stale.
        let same = entry_pointing_to(&block, b"alice", b"r", 5);
        assert!(matches!(
            store.insert(same, Some(block.clone())).await,
            Err(Error::Stale)
        ));

        // Lower priority -> Stale.
        let lower = entry_pointing_to(&block, b"alice", b"r", 4);
        assert!(matches!(
            store.insert(lower, Some(block.clone())).await,
            Err(Error::Stale)
        ));
    }

    #[tokio::test]
    async fn insert_replaces_with_higher_priority() {
        let store = MemoryStore::with_accept_all();
        let block_v1 = small_block(b"v1");
        let block_v2 = small_block(b"v2");

        let v1 = entry_pointing_to(&block_v1, b"alice", b"r", 1);
        let v2 = entry_pointing_to(&block_v2, b"alice", b"r", 2);
        store.insert(v1, Some(block_v1)).await.unwrap();
        store
            .insert(v2.clone(), Some(block_v2.clone()))
            .await
            .unwrap();

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
        store
            .insert(
                entry_pointing_to(&block, b"alice", b"a", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"alice", b"b", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"bob", b"a", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();

        let results = collect_iter(&store, Filter::Keyspace(vk(b"alice"))).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.verifying_key == vk(b"alice")));
    }

    #[tokio::test]
    async fn iter_namespace_returns_all_writers_at_name() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store
            .insert(
                entry_pointing_to(&block, b"alice", b"room/g", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"bob", b"room/g", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"alice", b"room/h", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();

        let results = collect_iter(&store, Filter::Namespace(n(b"room/g"))).await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn iter_name_prefix_matches_prefix() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store
            .insert(
                entry_pointing_to(&block, b"a", b"room/g", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"a", b"room/h", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"a", b"presence/x", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();

        let results = collect_iter(&store, Filter::NamePrefix(n(b"room/"))).await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn iter_specific_returns_at_most_one() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store
            .insert(
                entry_pointing_to(&block, b"a", b"x", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"b", b"x", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();

        let results = collect_iter(&store, Filter::Specific(vk(b"a"), n(b"x"))).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verifying_key, vk(b"a"));
    }

    #[tokio::test]
    async fn iter_union_is_or() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store
            .insert(
                entry_pointing_to(&block, b"a", b"room/g", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"b", b"presence/x", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"c", b"unrelated", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();

        let f = Filter::Union(vec![
            Filter::NamePrefix(n(b"room/")),
            Filter::NamePrefix(n(b"presence/")),
        ]);
        let results = collect_iter(&store, f).await;
        assert_eq!(results.len(), 2);
    }

    fn entry_with_expiry(
        block: &ContentBlock,
        vk_bytes: &'static [u8],
        name: &'static [u8],
        priority: u64,
        expires_at: u64,
    ) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name: n(name),
            value_hash: block.hash(),
            priority,
            expires_at: Some(expires_at),
            signature: bytes::Bytes::from_static(b"sig"),
        }
    }

    #[tokio::test]
    async fn delete_expired_removes_only_past_entries() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store
            .insert(
                entry_with_expiry(&block, b"a", b"old", 1, 100),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_with_expiry(&block, b"a", b"future", 1, 1000),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"a", b"forever", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();

        let removed = store.delete_expired(500).await.unwrap();
        assert_eq!(removed, 1);
        assert!(store.get_entry(&vk(b"a"), b"old").await.unwrap().is_none());
        assert!(
            store
                .get_entry(&vk(b"a"), b"future")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_entry(&vk(b"a"), b"forever")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn delete_expired_at_boundary_includes_equal() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store
            .insert(
                entry_with_expiry(&block, b"a", b"x", 1, 100),
                Some(block.clone()),
            )
            .await
            .unwrap();
        let removed = store.delete_expired(100).await.unwrap();
        assert_eq!(removed, 1);
    }

    #[tokio::test]
    async fn gc_blobs_keeps_reachable_drops_orphans() {
        let store = MemoryStore::with_accept_all();
        // A live entry pointing at a block with a transitive reference.
        let leaf = small_block(b"leaf");
        let head = ContentBlock {
            data: bytes::Bytes::from_static(b"head"),
            references: vec![leaf.hash()],
        };
        let entry = entry_pointing_to(&head, b"a", b"x", 1);
        store.put_content(leaf.clone()).await.unwrap();
        store.insert(entry, Some(head.clone())).await.unwrap();

        // An orphan block, unreferenced.
        let orphan = small_block(b"orphan");
        store.put_content(orphan.clone()).await.unwrap();

        let reclaimed = store.gc_blobs().await.unwrap();
        assert_eq!(reclaimed, 1);
        assert!(store.get_content(&head.hash()).await.unwrap().is_some());
        assert!(store.get_content(&leaf.hash()).await.unwrap().is_some());
        assert!(store.get_content(&orphan.hash()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn gc_blobs_handles_dangling_value_hash() {
        // KV entry references a blob we don't have locally (lazy ref); GC must not crash.
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"future");
        let entry = entry_pointing_to(&block, b"a", b"x", 1);
        store.insert(entry, None).await.unwrap(); // no blob yet
        let reclaimed = store.gc_blobs().await.unwrap();
        assert_eq!(reclaimed, 0);
    }

    #[tokio::test]
    async fn subscribe_replay_none_only_emits_future_events() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        // Pre-existing entry — should NOT replay.
        store
            .insert(
                entry_pointing_to(&block, b"a", b"r", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();

        let mut sub = store
            .subscribe(Filter::Keyspace(vk(b"a")), Replay::None)
            .await
            .unwrap();

        // Future event — should arrive.
        store
            .insert(
                entry_pointing_to(&block, b"a", b"r2", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), sub.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match evt {
            Event::Inserted(e) => assert_eq!(e.name.as_ref(), b"r2"),
            _ => panic!("unexpected event {:?}", evt),
        }
    }

    #[tokio::test]
    async fn subscribe_replay_all_emits_history_then_live() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store
            .insert(
                entry_pointing_to(&block, b"a", b"r1", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        store
            .insert(
                entry_pointing_to(&block, b"a", b"r2", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();

        let mut sub = store
            .subscribe(Filter::Keyspace(vk(b"a")), Replay::All)
            .await
            .unwrap();
        // Two historical.
        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_millis(200), sub.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        // One live.
        store
            .insert(
                entry_pointing_to(&block, b"a", b"r3", 1),
                Some(block.clone()),
            )
            .await
            .unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), sub.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match evt {
            Event::Inserted(e) => assert_eq!(e.name.as_ref(), b"r3"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn subscribe_replaced_event_on_higher_priority_overwrite() {
        let store = MemoryStore::with_accept_all();
        let b1 = small_block(b"v1");
        let b2 = small_block(b"v2");
        store
            .insert(entry_pointing_to(&b1, b"a", b"r", 1), Some(b1.clone()))
            .await
            .unwrap();
        let mut sub = store
            .subscribe(Filter::Keyspace(vk(b"a")), Replay::None)
            .await
            .unwrap();
        store
            .insert(entry_pointing_to(&b2, b"a", b"r", 2), Some(b2.clone()))
            .await
            .unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), sub.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match evt {
            Event::Replaced { old, new } => {
                assert_eq!(old.priority, 1);
                assert_eq!(new.priority, 2);
            }
            other => panic!("expected Replaced, got {:?}", other),
        }
    }
}
