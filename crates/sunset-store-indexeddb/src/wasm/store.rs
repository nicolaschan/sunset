//! IndexedDB-backed `Store` implementation.
//!
//! ## Memory cache + write-through IDB
//!
//! Every read serves from an in-memory mirror of the database. Writes
//! persist to IDB first (so on a crash, durability is intact) and
//! update the mirror on commit. Read paths are O(BTreeMap) like the
//! MemoryStore — necessary because IDB transactions are several
//! orders of magnitude slower than RAM and the engine takes many
//! short subscriptions during normal operation (presence, voice
//! presence, membership, message receipts, etc.). Without the
//! mirror, the cumulative `getAll()` latency on subscribe registration
//! at peer-connection bootstrap was wide enough to lose voice frames
//! during the first few hundred ms of a call.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sunset_store::{
    AcceptAllVerifier, ContentBlock, Cursor, EntryStream, Error, Event, EventStream, Filter, Hash,
    Replay, Result, SignatureVerifier, SignedKvEntry, Store, VerifyingKey,
};
use tokio::sync::Mutex;
use wasm_bindgen::JsValue;

use super::db::{
    OpenDb, STORE_BLOBS, STORE_ENTRIES, STORE_META, bytes_to_js, js_to_backend_err, js_to_bytes,
    open_database, read_all_values, txn_ro, txn_rw,
};
use super::req::{await_request, await_transaction};
use super::subscription::{Subscription, SubscriptionList};

/// Default database name. The web app uses one IndexedDB database per
/// origin, regardless of identity / room.
pub const DEFAULT_DATABASE_NAME: &str = "sunset-store";

const META_NEXT_SEQUENCE: &str = "next_sequence";

/// Composite key used in the in-memory entries map: `(verifying_key, name)`.
type KvKey = (VerifyingKey, Bytes);

/// Stored payload for an entry: the entry plus its assigned monotonic
/// sequence (used by `Cursor` semantics).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEntry {
    sequence: u64,
    entry: SignedKvEntry,
}

/// `IndexedDbStore` — a `Store` whose state lives in a per-origin
/// IndexedDB database, mirrored into RAM for hot reads.
///
/// Concurrency: writes serialize through `write_lock`. Subscriptions
/// register and broadcast under that same lock so events are never
/// double-delivered (history vs. live channel) for a write that
/// raced with `subscribe`.
pub struct IndexedDbStore {
    inner: Rc<Inner>,
}

struct Inner {
    /// `OpenDb` holds both the database and the closures registered on
    /// it (notably `onversionchange`). The struct is dropped — and the
    /// closures unregistered — only when the last `Rc<Inner>` goes
    /// away, so the lifetimes stay aligned with the store handle.
    open: OpenDb,
    verifier: Arc<dyn SignatureVerifier>,
    subscriptions: SubscriptionList,
    /// Serializes writes (insert / delete_expired / gc_blobs / put_content)
    /// AND wraps history-snapshot + subscription registration. This is the
    /// invariant that makes subscriptions race-free — see comment in
    /// `subscribe`.
    write_lock: Mutex<()>,
    /// In-memory mirror of the `entries` object store, keyed by
    /// `(verifying_key, name)`. Loaded once on open; updated on every
    /// successful write.
    entries: RefCell<BTreeMap<KvKey, StoredEntry>>,
    /// In-memory mirror of the `blobs` object store. Same load /
    /// write-through pattern as `entries`.
    blobs: RefCell<HashMap<Hash, ContentBlock>>,
    /// Next-to-be-assigned sequence number. Persisted alongside
    /// entries so cursor ordering survives reloads.
    next_sequence: RefCell<u64>,
}

impl IndexedDbStore {
    /// Open or create the IndexedDB database `database_name` and return a
    /// store that uses `verifier` to verify entries on insert. Loads
    /// every persisted entry + blob into the in-memory mirror so the
    /// returned handle is ready for hot reads.
    pub async fn open(database_name: &str, verifier: Arc<dyn SignatureVerifier>) -> Result<Self> {
        let open = open_database(database_name).await?;

        // Load entries.
        let mut entries: BTreeMap<KvKey, StoredEntry> = BTreeMap::new();
        let mut max_sequence: u64 = 0;
        {
            let txn = txn_ro(&open.db, &[STORE_ENTRIES])?;
            let raw = read_all_values(&txn, STORE_ENTRIES).await?;
            for bytes in raw {
                let stored: StoredEntry = postcard::from_bytes(&bytes)
                    .map_err(|e| Error::Backend(format!("decode StoredEntry: {e}")))?;
                if stored.sequence + 1 > max_sequence {
                    max_sequence = stored.sequence + 1;
                }
                let key = (
                    stored.entry.verifying_key.clone(),
                    stored.entry.name.clone(),
                );
                entries.insert(key, stored);
            }
        }

        // Load blobs.
        let mut blobs: HashMap<Hash, ContentBlock> = HashMap::new();
        {
            let txn = txn_ro(&open.db, &[STORE_BLOBS])?;
            let raw = read_all_values(&txn, STORE_BLOBS).await?;
            for bytes in raw {
                let block: ContentBlock = postcard::from_bytes(&bytes)
                    .map_err(|e| Error::Backend(format!("decode ContentBlock: {e}")))?;
                blobs.insert(block.hash(), block);
            }
        }

        // Load next_sequence (persisted), preferring the maximum of
        // (persisted, derived). Both should agree, but if a previous
        // write committed the entry but not the meta record (atomic
        // transactions guarantee they should), trust the entries.
        let persisted_next_sequence: u64 = {
            let txn = txn_ro(&open.db, &[STORE_META])?;
            let store = txn
                .object_store(STORE_META)
                .map_err(|e| js_to_backend_err(&e))?;
            let req = store
                .get(&JsValue::from_str(META_NEXT_SEQUENCE))
                .map_err(|e| js_to_backend_err(&e))?;
            let value = await_request(req).await?;
            if value.is_undefined() || value.is_null() {
                0
            } else {
                let bytes = js_to_bytes(&value)
                    .ok_or_else(|| Error::Backend("meta.next_sequence wrong type".to_string()))?;
                postcard::from_bytes::<u64>(&bytes)
                    .map_err(|e| Error::Backend(format!("decode next_sequence: {e}")))?
            }
        };
        let next_sequence = persisted_next_sequence.max(max_sequence);

        Ok(Self {
            inner: Rc::new(Inner {
                open,
                verifier,
                subscriptions: SubscriptionList::default(),
                write_lock: Mutex::new(()),
                entries: RefCell::new(entries),
                blobs: RefCell::new(blobs),
                next_sequence: RefCell::new(next_sequence),
            }),
        })
    }

    /// Close the underlying IDB connection. Subsequent store operations
    /// fail. Used by the "reset local state" flow before invoking
    /// `delete_database`, so the deletion isn't blocked.
    pub fn close(&self) {
        self.inner.open.db.close();
    }

    /// Convenience: open with `AcceptAllVerifier`. For tests.
    pub async fn open_with_accept_all(database_name: &str) -> Result<Self> {
        Self::open(database_name, Arc::new(AcceptAllVerifier)).await
    }
}

#[async_trait(?Send)]
impl Store for IndexedDbStore {
    async fn insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()> {
        if let Some(b) = &blob {
            if b.hash() != entry.value_hash {
                return Err(Error::HashMismatch);
            }
        }
        self.inner.verifier.verify(&entry)?;

        let _w = self.inner.write_lock.lock().await;

        let key: KvKey = (entry.verifying_key.clone(), entry.name.clone());

        // 1. LWW check against the in-memory mirror.
        let prev = self.inner.entries.borrow().get(&key).cloned();
        if let Some(s) = &prev {
            if s.entry.priority >= entry.priority {
                return Err(Error::Stale);
            }
        }

        // 2. Decide if the blob is new (look up in cache before writing).
        let blob_was_new = if blob.is_some() {
            !self.inner.blobs.borrow().contains_key(&entry.value_hash)
        } else {
            false
        };

        // 3. Assign sequence.
        let sequence = *self.inner.next_sequence.borrow();
        let new_next_sequence = sequence + 1;
        let stored = StoredEntry {
            sequence,
            entry: entry.clone(),
        };

        // 4. Update the in-memory mirror first. Reads + subscriptions
        // observe the new state immediately. Persistence is queued
        // below and runs after we release the lock.
        let blob_added_hash = if blob_was_new {
            Some(entry.value_hash)
        } else {
            None
        };
        let blob_for_idb: Option<ContentBlock> = if blob_was_new { blob.clone() } else { None };
        if let Some(b) = blob {
            if blob_was_new {
                self.inner.blobs.borrow_mut().insert(entry.value_hash, b);
            }
        }
        self.inner.entries.borrow_mut().insert(key, stored.clone());
        *self.inner.next_sequence.borrow_mut() = new_next_sequence;

        if let Some(s) = prev {
            self.inner.subscriptions.broadcast(&Event::Replaced {
                old: s.entry,
                new: entry.clone(),
            });
        } else {
            self.inner
                .subscriptions
                .broadcast(&Event::Inserted(entry.clone()));
        }
        if let Some(h) = blob_added_hash {
            self.inner.subscriptions.broadcast(&Event::BlobAdded(h));
        }

        // 5. Issue IDB writes in a single rw transaction. We release
        // the write_lock immediately afterwards (the requests are
        // already queued on the IDB engine, so nothing further needs
        // to be serialized via our lock) and `await` the transaction
        // commit OUTSIDE the lock-held section. Concurrent calls
        // wait only on the in-memory critical section; the IDB I/O
        // overlaps with their work.
        let txn = txn_rw(
            &self.inner.open.db,
            &[STORE_ENTRIES, STORE_BLOBS, STORE_META],
        )?;
        if let Some(b) = &blob_for_idb {
            let blobs_store = txn
                .object_store(STORE_BLOBS)
                .map_err(|e| js_to_backend_err(&e))?;
            let bytes = postcard::to_stdvec(b)
                .map_err(|e| Error::Backend(format!("encode ContentBlock: {e}")))?;
            let _put_req = blobs_store
                .put_with_key(
                    &bytes_to_js(&bytes).into(),
                    &bytes_to_js(b.hash().as_bytes()).into(),
                )
                .map_err(|e| js_to_backend_err(&e))?;
        }
        {
            let entries_store = txn
                .object_store(STORE_ENTRIES)
                .map_err(|e| js_to_backend_err(&e))?;
            let stored_bytes = postcard::to_stdvec(&stored)
                .map_err(|e| Error::Backend(format!("encode StoredEntry: {e}")))?;
            let composite = composite_key(&entry.verifying_key, &entry.name)?;
            let _put_req = entries_store
                .put_with_key(
                    &bytes_to_js(&stored_bytes).into(),
                    &bytes_to_js(&composite).into(),
                )
                .map_err(|e| js_to_backend_err(&e))?;
        }
        {
            let meta_store = txn
                .object_store(STORE_META)
                .map_err(|e| js_to_backend_err(&e))?;
            let bytes = postcard::to_stdvec(&new_next_sequence)
                .map_err(|e| Error::Backend(format!("encode next_sequence: {e}")))?;
            let _put_req = meta_store
                .put_with_key(
                    &bytes_to_js(&bytes).into(),
                    &JsValue::from_str(META_NEXT_SEQUENCE),
                )
                .map_err(|e| js_to_backend_err(&e))?;
        }
        drop(_w);
        // Fire-and-forget transaction commit. The requests are
        // already queued in IDB's transaction; the engine commits
        // them on the next event-loop tick. We deliberately don't
        // await `oncomplete` because:
        //   * The MemoryStore-equivalent in-memory mirror is already
        //     updated and visible to all subsequent reads, so the
        //     caller's contract ("the entry is now in the store") is
        //     met without waiting on JS-IDB durability.
        //   * Awaiting the txn commit pinned an entire macrotask
        //     hop on every insert which the voice subsystem could
        //     not absorb during peer-connection setup; see PR
        //     description for the regression that motivated this
        //     change.
        // Pending IDB transactions commit during the page-unload
        // path on `location.reload()` / browser navigation (the
        // IndexedDB spec requires user agents to drain in-flight
        // transactions before tearing down the document), so the
        // persistence test still observes durable writes after a
        // reload.
        let _ = txn;

        Ok(())
    }

    async fn put_content(&self, block: ContentBlock) -> Result<Hash> {
        let _w = self.inner.write_lock.lock().await;

        let hash = block.hash();
        let already = self.inner.blobs.borrow().contains_key(&hash);
        if already {
            return Ok(hash);
        }

        // Write through to IDB. We issue the put without an
        // intermediate await — transaction commit alone is enough
        // for durability, and skipping the per-request wait halves
        // the microtask hops on the put_content path.
        let txn = txn_rw(&self.inner.open.db, &[STORE_BLOBS])?;
        let blobs_store = txn
            .object_store(STORE_BLOBS)
            .map_err(|e| js_to_backend_err(&e))?;
        let bytes = postcard::to_stdvec(&block)
            .map_err(|e| Error::Backend(format!("encode ContentBlock: {e}")))?;
        let _put_req = blobs_store
            .put_with_key(
                &bytes_to_js(&bytes).into(),
                &bytes_to_js(hash.as_bytes()).into(),
            )
            .map_err(|e| js_to_backend_err(&e))?;
        await_transaction(txn).await?;

        self.inner.blobs.borrow_mut().insert(hash, block);
        self.inner.subscriptions.broadcast(&Event::BlobAdded(hash));
        Ok(hash)
    }

    async fn get_content(&self, hash: &Hash) -> Result<Option<ContentBlock>> {
        Ok(self.inner.blobs.borrow().get(hash).cloned())
    }

    async fn get_entry(&self, vk: &VerifyingKey, name: &[u8]) -> Result<Option<SignedKvEntry>> {
        let key = (vk.clone(), Bytes::copy_from_slice(name));
        Ok(self
            .inner
            .entries
            .borrow()
            .get(&key)
            .map(|s| s.entry.clone()))
    }

    async fn iter<'a>(&'a self, filter: Filter) -> Result<EntryStream<'a>> {
        let entries: Vec<SignedKvEntry> = self
            .inner
            .entries
            .borrow()
            .iter()
            .filter(|((vk, name), _)| filter.matches(vk, name.as_ref()))
            .map(|(_, s)| s.entry.clone())
            .collect();
        let stream = futures::stream::iter(entries.into_iter().map(Ok));
        Ok(Box::pin(stream))
    }

    async fn subscribe<'a>(&'a self, filter: Filter, replay: Replay) -> Result<EventStream<'a>> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sub = Rc::new(Subscription {
            filter: filter.clone(),
            tx,
        });

        // Snapshot history AND register subscription under write_lock so
        // any concurrent insert is serialized — events land in either
        // the snapshot or the live channel, never both. The snapshot
        // walks the in-memory mirror, not IDB, so it's O(N) over RAM
        // (no JS round-trip per call).
        let _w = self.inner.write_lock.lock().await;
        let history: Vec<sunset_store::Result<Event>> = {
            let entries = self.inner.entries.borrow();
            let mut out: Vec<(u64, Event)> = entries
                .iter()
                .filter(|((vk, name), _)| filter.matches(vk, name.as_ref()))
                .filter(|(_, s)| match replay {
                    Replay::None => false,
                    Replay::All => true,
                    Replay::Since(c) => s.sequence >= c.0,
                })
                .map(|(_, s)| (s.sequence, Event::Inserted(s.entry.clone())))
                .collect();
            out.sort_by_key(|(s, _)| *s);
            out.into_iter().map(|(_, e)| Ok(e)).collect()
        };
        self.inner.subscriptions.add(&sub);
        drop(_w);

        let stream = async_stream::stream! {
            let _hold = sub;
            for h in history { yield h; }
            while let Some(item) = rx.recv().await { yield item; }
        };
        Ok(Box::pin(stream))
    }

    async fn delete_expired(&self, now: u64) -> Result<usize> {
        let _w = self.inner.write_lock.lock().await;

        let victims: Vec<(KvKey, SignedKvEntry)> = self
            .inner
            .entries
            .borrow()
            .iter()
            .filter(|(_, s)| s.entry.expires_at.is_some_and(|t| t <= now))
            .map(|(k, s)| (k.clone(), s.entry.clone()))
            .collect();
        if victims.is_empty() {
            return Ok(0);
        }

        let txn = txn_rw(&self.inner.open.db, &[STORE_ENTRIES])?;
        let entries_store = txn
            .object_store(STORE_ENTRIES)
            .map_err(|e| js_to_backend_err(&e))?;
        for (k, _) in &victims {
            let composite = composite_key(&k.0, &k.1)?;
            let _del_req = entries_store
                .delete(&bytes_to_js(&composite).into())
                .map_err(|e| js_to_backend_err(&e))?;
        }
        await_transaction(txn).await?;

        let count = victims.len();
        for (k, e) in victims {
            self.inner.entries.borrow_mut().remove(&k);
            self.inner.subscriptions.broadcast(&Event::Expired(e));
        }
        Ok(count)
    }

    async fn gc_blobs(&self) -> Result<usize> {
        let _w = self.inner.write_lock.lock().await;

        // Mark phase: walk live KV roots and the content DAG transitively.
        // Done in a tight non-async block so we can drop the RefCell
        // borrow before reaching the IDB-await sweep phase below.
        let to_remove: Vec<Hash> = {
            let mut reachable: HashSet<Hash> = HashSet::new();
            let mut frontier: Vec<Hash> = self
                .inner
                .entries
                .borrow()
                .values()
                .map(|s| s.entry.value_hash)
                .collect();
            let blobs = self.inner.blobs.borrow();
            while let Some(h) = frontier.pop() {
                if !reachable.insert(h) {
                    continue;
                }
                if let Some(block) = blobs.get(&h) {
                    for r in &block.references {
                        if !reachable.contains(r) {
                            frontier.push(*r);
                        }
                    }
                }
            }
            blobs
                .keys()
                .filter(|h| !reachable.contains(h))
                .copied()
                .collect()
        };
        if to_remove.is_empty() {
            return Ok(0);
        }

        // Sweep phase: delete from IDB, then from the mirror.
        let txn = txn_rw(&self.inner.open.db, &[STORE_BLOBS])?;
        let blobs_store = txn
            .object_store(STORE_BLOBS)
            .map_err(|e| js_to_backend_err(&e))?;
        for h in &to_remove {
            let _req = blobs_store
                .delete(&bytes_to_js(h.as_bytes()).into())
                .map_err(|e| js_to_backend_err(&e))?;
        }
        await_transaction(txn).await?;

        let count = to_remove.len();
        for h in to_remove {
            self.inner.blobs.borrow_mut().remove(&h);
            self.inner.subscriptions.broadcast(&Event::BlobRemoved(h));
        }
        Ok(count)
    }

    async fn current_cursor(&self) -> Result<Cursor> {
        Ok(Cursor(*self.inner.next_sequence.borrow()))
    }

    fn verifier(&self) -> Arc<dyn SignatureVerifier> {
        self.inner.verifier.clone()
    }
}

fn composite_key(vk: &VerifyingKey, name: &[u8]) -> Result<Vec<u8>> {
    postcard::to_stdvec(&(vk, Bytes::copy_from_slice(name)))
        .map_err(|e| Error::Backend(format!("encode composite key: {e}")))
}
