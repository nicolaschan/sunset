//! Generic conformance suite. Any `Store` implementation can be exercised
//! against this suite to verify it satisfies the documented contract.
//!
//! Gated by the `test-helpers` feature so production builds don't pull these in.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::filter::{Event, Filter, Replay};
use crate::store::Store;
use crate::types::{ContentBlock, SignedKvEntry, VerifyingKey};
use crate::verifier::SignatureVerifier;

/// Helper: a verifying key from static bytes.
pub fn vk(b: &'static [u8]) -> VerifyingKey {
    VerifyingKey::new(bytes::Bytes::from_static(b))
}

/// Helper: a name from static bytes.
pub fn n(b: &'static [u8]) -> bytes::Bytes {
    bytes::Bytes::from_static(b)
}

/// Helper: a small leaf block.
pub fn block(payload: &'static [u8]) -> ContentBlock {
    ContentBlock {
        data: bytes::Bytes::from_static(payload),
        references: vec![],
    }
}

/// Helper: an entry pointing at `block`'s hash, with the given key/name/priority.
pub fn entry(
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

/// A verifier that asserts entries pass through it. Useful to detect when a
/// backend forgets to call its verifier on insert.
pub struct CountingVerifier(pub Arc<std::sync::atomic::AtomicUsize>);
impl SignatureVerifier for CountingVerifier {
    fn verify(&self, _entry: &SignedKvEntry) -> Result<()> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// Drain a subscription stream until an event matching `predicate` is found.
/// Times out after a short duration. The conformance suite uses this so tests
/// don't get tripped up by interleaved BlobAdded events.
async fn next_matching<P>(
    stream: &mut futures::stream::LocalBoxStream<'_, Result<Event>>,
    predicate: P,
) -> Event
where
    P: Fn(&Event) -> bool,
{
    use futures::StreamExt;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            panic!("timed out waiting for matching event");
        }
        match tokio::time::timeout(deadline - now, stream.next()).await {
            Ok(Some(Ok(e))) if predicate(&e) => return e,
            Ok(Some(Ok(_))) => continue, // not what we want; skip
            Ok(Some(Err(err))) => panic!("stream error: {:?}", err),
            Ok(None) => panic!("stream ended unexpectedly"),
            Err(_) => panic!("timed out waiting for matching event"),
        }
    }
}

/// Run the full conformance suite against `store_factory`. The factory is
/// called once per test case to create a fresh store.
pub async fn run_conformance_suite<S, F>(store_factory: F)
where
    S: Store,
    F: Fn() -> S,
{
    insert_get_roundtrip(&store_factory()).await;
    lww_supersession(&store_factory()).await;
    stale_rejection(&store_factory()).await;
    hash_mismatch_rejection(&store_factory()).await;
    lazy_dangling_ref(&store_factory()).await;
    ttl_pruning(&store_factory()).await;
    blob_gc_reachability(&store_factory()).await;
    iter_filters(&store_factory()).await;
    subscribe_replay_modes(&store_factory()).await;
    subscribe_replay_since_cursor(&store_factory()).await;
    subscribe_emits_replaced_event(&store_factory()).await;
    subscribe_emits_expired_event(&store_factory()).await;
    subscribe_emits_blob_added_event(&store_factory()).await;
    subscribe_emits_blob_removed_event(&store_factory()).await;
}

/// Test: insert + get_entry roundtrip.
pub async fn insert_get_roundtrip<S: Store>(store: &S) {
    let b = block(b"hello");
    let e = entry(&b, b"alice", b"r", 1);
    store.insert(e.clone(), Some(b.clone())).await.unwrap();
    let got = store
        .get_entry(&e.verifying_key, &e.name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, e, "insert/get roundtrip");
    let got_blob = store.get_content(&b.hash()).await.unwrap().unwrap();
    assert_eq!(got_blob, b, "blob roundtrip");
}

/// Test: higher-priority insert replaces lower; the value is reachable.
pub async fn lww_supersession<S: Store>(store: &S) {
    let b1 = block(b"v1");
    let b2 = block(b"v2");
    store
        .insert(entry(&b1, b"a", b"r", 1), Some(b1))
        .await
        .unwrap();
    let v2 = entry(&b2, b"a", b"r", 2);
    store.insert(v2.clone(), Some(b2)).await.unwrap();
    let now = store
        .get_entry(&v2.verifying_key, &v2.name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(now, v2, "higher priority replaces");
}

/// Test: equal-or-lower priority is rejected.
pub async fn stale_rejection<S: Store>(store: &S) {
    let b = block(b"x");
    store
        .insert(entry(&b, b"a", b"r", 5), Some(b.clone()))
        .await
        .unwrap();
    let same = entry(&b, b"a", b"r", 5);
    assert!(matches!(
        store.insert(same, Some(b.clone())).await,
        Err(Error::Stale)
    ));
    let lower = entry(&b, b"a", b"r", 4);
    assert!(matches!(
        store.insert(lower, Some(b)).await,
        Err(Error::Stale)
    ));
}

/// Test: insert rejects mismatched (entry.value_hash, blob.hash()).
pub async fn hash_mismatch_rejection<S: Store>(store: &S) {
    let real = block(b"real");
    let fake = block(b"fake");
    let mut e = entry(&real, b"a", b"r", 1);
    // Force value_hash to point to `real` while passing `fake`.
    e.value_hash = real.hash();
    assert!(matches!(
        store.insert(e, Some(fake)).await,
        Err(Error::HashMismatch)
    ));
}

/// Test: an entry can be inserted without its blob; blob can land later.
pub async fn lazy_dangling_ref<S: Store>(store: &S) {
    let b = block(b"future");
    let e = entry(&b, b"a", b"r", 1);
    store.insert(e, None).await.unwrap();
    assert!(store.get_content(&b.hash()).await.unwrap().is_none());
    store.put_content(b.clone()).await.unwrap();
    assert!(store.get_content(&b.hash()).await.unwrap().is_some());
}

/// Test: `delete_expired(now)` removes entries with `expires_at <= now` (boundary inclusive).
pub async fn ttl_pruning<S: Store>(store: &S) {
    let b = block(b"x");
    let mut old = entry(&b, b"a", b"old", 1);
    old.expires_at = Some(100);
    let mut future = entry(&b, b"a", b"future", 1);
    future.expires_at = Some(1000);
    let forever = entry(&b, b"a", b"forever", 1);
    store.insert(old, Some(b.clone())).await.unwrap();
    store.insert(future, Some(b.clone())).await.unwrap();
    store.insert(forever, Some(b.clone())).await.unwrap();
    let removed = store.delete_expired(100).await.unwrap();
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

/// Test: gc_blobs keeps reachable blobs and reclaims orphans.
pub async fn blob_gc_reachability<S: Store>(store: &S) {
    let leaf = block(b"leaf");
    let head = ContentBlock {
        data: bytes::Bytes::from_static(b"head"),
        references: vec![leaf.hash()],
    };
    let orphan = block(b"orphan");
    let e = entry(&head, b"a", b"r", 1);
    store.put_content(leaf.clone()).await.unwrap();
    store.insert(e, Some(head.clone())).await.unwrap();
    store.put_content(orphan.clone()).await.unwrap();
    let n = store.gc_blobs().await.unwrap();
    assert_eq!(n, 1, "exactly one orphan reclaimed");
    assert!(store.get_content(&head.hash()).await.unwrap().is_some());
    assert!(store.get_content(&leaf.hash()).await.unwrap().is_some());
    assert!(store.get_content(&orphan.hash()).await.unwrap().is_none());
}

/// Test: iter respects each filter variant.
pub async fn iter_filters<S: Store>(store: &S) {
    use futures::StreamExt;
    let b = block(b"x");
    store
        .insert(entry(&b, b"a", b"room/g", 1), Some(b.clone()))
        .await
        .unwrap();
    store
        .insert(entry(&b, b"a", b"presence/x", 1), Some(b.clone()))
        .await
        .unwrap();
    store
        .insert(entry(&b, b"b", b"room/g", 1), Some(b.clone()))
        .await
        .unwrap();

    async fn collect<S: Store>(s: &S, f: Filter) -> Vec<SignedKvEntry> {
        let mut st = s.iter(f).await.unwrap();
        let mut out = vec![];
        while let Some(item) = st.next().await {
            out.push(item.unwrap());
        }
        out
    }

    let r_keyspace = collect(store, Filter::Keyspace(vk(b"a"))).await;
    assert_eq!(r_keyspace.len(), 2);
    let r_namespace = collect(store, Filter::Namespace(n(b"room/g"))).await;
    assert_eq!(r_namespace.len(), 2);
    let r_prefix = collect(store, Filter::NamePrefix(n(b"room/"))).await;
    assert_eq!(r_prefix.len(), 2);
    let r_specific = collect(store, Filter::Specific(vk(b"a"), n(b"room/g"))).await;
    assert_eq!(r_specific.len(), 1);
    let r_union = collect(
        store,
        Filter::Union(vec![
            Filter::NamePrefix(n(b"room/")),
            Filter::NamePrefix(n(b"presence/")),
        ]),
    )
    .await;
    assert_eq!(r_union.len(), 3);
}

/// Test: subscribe under each `Replay` mode delivers correctly.
pub async fn subscribe_replay_modes<S: Store>(store: &S) {
    use futures::StreamExt;
    let b = block(b"x");
    store
        .insert(entry(&b, b"a", b"r1", 1), Some(b.clone()))
        .await
        .unwrap();
    store
        .insert(entry(&b, b"a", b"r2", 1), Some(b.clone()))
        .await
        .unwrap();

    // Replay::None — only future events.
    let mut s = store
        .subscribe(Filter::Keyspace(vk(b"a")), Replay::None)
        .await
        .unwrap();
    store
        .insert(entry(&b, b"a", b"r3", 1), Some(b.clone()))
        .await
        .unwrap();
    let evt = tokio::time::timeout(std::time::Duration::from_millis(500), s.next())
        .await
        .expect("subscribe should deliver new event")
        .unwrap()
        .unwrap();
    assert!(matches!(evt, Event::Inserted(e) if e.name.as_ref() == b"r3"));

    // Replay::All — historical first, then live.
    let mut s = store
        .subscribe(Filter::Keyspace(vk(b"a")), Replay::All)
        .await
        .unwrap();
    for _ in 0..3 {
        tokio::time::timeout(std::time::Duration::from_millis(500), s.next())
            .await
            .expect("history should be replayed")
            .unwrap()
            .unwrap();
    }
    store
        .insert(entry(&b, b"a", b"r4", 1), Some(b.clone()))
        .await
        .unwrap();
    let evt = tokio::time::timeout(std::time::Duration::from_millis(500), s.next())
        .await
        .expect("subscribe should deliver new event after replay")
        .unwrap()
        .unwrap();
    assert!(matches!(evt, Event::Inserted(e) if e.name.as_ref() == b"r4"));
}

/// Test: `Replay::Since(cursor)` emits only entries written after the cursor.
pub async fn subscribe_replay_since_cursor<S: Store>(store: &S) {
    use futures::StreamExt;
    let b = block(b"x");
    // Two entries before the cursor snapshot.
    store
        .insert(entry(&b, b"a", b"r1", 1), Some(b.clone()))
        .await
        .unwrap();
    store
        .insert(entry(&b, b"a", b"r2", 1), Some(b.clone()))
        .await
        .unwrap();
    let cursor = store.current_cursor().await.unwrap();
    // Two entries after the cursor snapshot.
    store
        .insert(entry(&b, b"a", b"r3", 1), Some(b.clone()))
        .await
        .unwrap();
    store
        .insert(entry(&b, b"a", b"r4", 1), Some(b.clone()))
        .await
        .unwrap();

    let mut s = store
        .subscribe(Filter::Keyspace(vk(b"a")), Replay::Since(cursor))
        .await
        .unwrap();

    // Should replay only r3, r4 (in order).
    let mut names = vec![];
    for _ in 0..2 {
        let evt = tokio::time::timeout(std::time::Duration::from_millis(500), s.next())
            .await
            .expect("Since-cursor replay should deliver post-cursor entries")
            .unwrap()
            .unwrap();
        if let Event::Inserted(e) = evt {
            names.push(e.name.clone());
        } else {
            panic!("expected Inserted, got {:?}", evt);
        }
    }
    assert_eq!(names, vec![n(b"r3"), n(b"r4")]);
}

/// Test: a higher-priority insert emits `Event::Replaced` to active subscribers.
pub async fn subscribe_emits_replaced_event<S: Store>(store: &S) {
    use futures::StreamExt;
    let b1 = block(b"v1");
    let b2 = block(b"v2");
    store
        .insert(entry(&b1, b"a", b"r", 1), Some(b1))
        .await
        .unwrap();

    let mut s = store
        .subscribe(Filter::Keyspace(vk(b"a")), Replay::None)
        .await
        .unwrap();
    store
        .insert(entry(&b2, b"a", b"r", 2), Some(b2))
        .await
        .unwrap();

    // The subscriber will receive Replaced and BlobAdded (the replacement
    // points at a fresh blob); we look for Replaced.
    let evt = next_matching(&mut s, |e| matches!(e, Event::Replaced { .. })).await;
    let (replaced_vk, replaced_name) = if let Event::Replaced { old, new } = evt {
        assert_eq!(old.priority, 1, "old entry priority");
        assert_eq!(new.priority, 2, "new entry priority");
        (new.verifying_key.clone(), new.name.clone())
    } else {
        unreachable!()
    };

    // Negative assertion: a buggy backend that emitted a phantom Inserted for
    // the replaced key after Replaced would still satisfy the predicate above,
    // so drain the stream briefly and assert no such event appears.
    let drain_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
    loop {
        let now = tokio::time::Instant::now();
        if now >= drain_deadline {
            break;
        }
        match tokio::time::timeout(drain_deadline - now, s.next()).await {
            Ok(Some(Ok(Event::Inserted(e))))
                if e.verifying_key == replaced_vk && e.name == replaced_name =>
            {
                panic!(
                    "phantom Inserted for replaced key {:?}/{:?}",
                    e.verifying_key, e.name
                );
            }
            Ok(Some(Ok(_))) => continue, // unrelated events (e.g. BlobAdded) are fine
            Ok(Some(Err(err))) => panic!("stream error during drain: {:?}", err),
            Ok(None) => break,
            Err(_) => break, // timeout: drain window elapsed
        }
    }
}

/// Test: `delete_expired(now)` emits `Event::Expired` to active subscribers for each removed entry.
pub async fn subscribe_emits_expired_event<S: Store>(store: &S) {
    let b = block(b"x");
    let mut e = entry(&b, b"a", b"will-expire", 1);
    e.expires_at = Some(100);
    store.insert(e.clone(), Some(b)).await.unwrap();

    let mut s = store
        .subscribe(Filter::Keyspace(vk(b"a")), Replay::None)
        .await
        .unwrap();
    let removed = store.delete_expired(100).await.unwrap();
    assert_eq!(removed, 1);

    let evt = next_matching(&mut s, |evt| matches!(evt, Event::Expired(_))).await;
    if let Event::Expired(expired) = evt {
        assert_eq!(expired.name.as_ref(), b"will-expire");
    } else {
        unreachable!()
    }
}

/// Test: a successful insert with a new blob emits `Event::BlobAdded` to all subscribers.
///
/// The subscriber filter intentionally does NOT match the writer's verifying
/// key; this pins down the contract that BlobAdded is delivered regardless
/// of subscription filter (a buggy backend that filtered BlobAdded by
/// keyspace would fail this test).
pub async fn subscribe_emits_blob_added_event<S: Store>(store: &S) {
    let b = block(b"new-blob");

    let mut s = store
        .subscribe(Filter::Keyspace(vk(b"unrelated-watcher")), Replay::None)
        .await
        .unwrap();
    store
        .insert(entry(&b, b"a", b"r", 1), Some(b.clone()))
        .await
        .unwrap();

    let evt = next_matching(&mut s, |e| matches!(e, Event::BlobAdded(_))).await;
    if let Event::BlobAdded(h) = evt {
        assert_eq!(h, b.hash());
    } else {
        unreachable!()
    }
}

/// Test: `gc_blobs()` emits `Event::BlobRemoved` to all subscribers for each reclaimed blob.
///
/// Like the BlobAdded test, the subscriber filter intentionally does NOT
/// match anything in the store; this pins down the contract that
/// BlobRemoved is delivered regardless of subscription filter.
pub async fn subscribe_emits_blob_removed_event<S: Store>(store: &S) {
    let orphan = block(b"orphan");
    store.put_content(orphan.clone()).await.unwrap();

    let mut s = store
        .subscribe(Filter::Keyspace(vk(b"unrelated-watcher")), Replay::None)
        .await
        .unwrap();
    let reclaimed = store.gc_blobs().await.unwrap();
    assert_eq!(reclaimed, 1);

    let evt = next_matching(&mut s, |e| matches!(e, Event::BlobRemoved(_))).await;
    if let Event::BlobRemoved(h) = evt {
        assert_eq!(h, orphan.hash());
    } else {
        unreachable!()
    }
}
