//! The `Store` trait: the public surface every backend implements.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::LocalBoxStream;

use crate::error::{Error, Result};
use crate::filter::{Event, Filter, Replay};
use crate::types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};
use crate::verifier::SignatureVerifier;

/// Stream of `SignedKvEntry` values yielded by `Store::iter`.
pub type EntryStream<'a> = LocalBoxStream<'a, Result<SignedKvEntry>>;
/// Stream of `Event` values yielded by `Store::subscribe`.
pub type EventStream<'a> = LocalBoxStream<'a, Result<Event>>;

/// Pluggable backend trait. Implementations live in separate crates.
///
/// Implementations are expected to:
/// - Call the configured `SignatureVerifier` on every insert.
/// - Apply LWW by `(verifying_key, name)` priority (higher wins; ties are stale).
/// - Reject inserts whose `entry.value_hash` does not match `blob.hash()` when
///   `blob` is supplied.
/// - Make `(blob, entry)` writes atomic (both succeed or neither).
/// - Accept entries whose referenced blob is not yet locally present (lazy refs).
/// - Maintain a monotonic per-store sequence used for cursors.
///
/// `?Send` futures are used so that non-`Send` WASM backends are accepted.
#[async_trait(?Send)]
pub trait Store {
    /// Insert an entry, optionally with its referenced blob.
    ///
    /// Backends delegate to [`run_insert`], which performs the shared
    /// validation (hash-check → signature-verify) and event ordering once;
    /// the backend supplies only the atomic locked write via
    /// [`InsertCommitter::commit_insert`].
    async fn insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()>;

    /// Insert a content block by itself; returns its hash.
    async fn put_content(&self, block: ContentBlock) -> Result<Hash>;

    /// Get a content block by hash.
    async fn get_content(&self, hash: &Hash) -> Result<Option<ContentBlock>>;

    /// Get the current entry for `(vk, name)`, if any.
    async fn get_entry(&self, vk: &VerifyingKey, name: &[u8]) -> Result<Option<SignedKvEntry>>;

    /// Stream all entries currently in the store matching `filter`.
    /// Backends should walk indexes appropriate to the filter.
    async fn iter<'a>(&'a self, filter: Filter) -> Result<EntryStream<'a>>;

    /// Subscribe to events. `replay` controls whether historical entries are
    /// emitted before live updates.
    async fn subscribe<'a>(&'a self, filter: Filter, replay: Replay) -> Result<EventStream<'a>>;

    /// Delete all entries with `expires_at <= now`. Returns the count removed.
    /// Should emit `Event::Expired` for each on active subscriptions.
    async fn delete_expired(&self, now: u64) -> Result<usize>;

    /// Mark-and-sweep over content blobs reachable from live KV entries.
    /// Returns the count reclaimed.
    async fn gc_blobs(&self) -> Result<usize>;

    /// Returns the current monotonic cursor: the next-to-be-assigned sequence
    /// number. Passing the returned cursor to `subscribe(..., Replay::Since(c))`
    /// replays entries written at or after the moment this method observed the
    /// store (the `Since` predicate is `sequence >= c.0`). The absolute starting
    /// value is backend-specific (memory backends may start at 0; SQLite-backed
    /// stores start at 1 due to AUTOINCREMENT semantics) — only the *relative*
    /// ordering is guaranteed: a cursor captured later is strictly greater than
    /// one captured earlier (assuming intervening inserts).
    async fn current_cursor(&self) -> Result<Cursor>;

    /// The signature verifier this store was constructed with.
    /// Engines reuse this for verifying messages outside the store
    /// itself (e.g. ephemeral datagrams in `sunset-sync`).
    fn verifier(&self) -> Arc<dyn SignatureVerifier>;
}

/// The backend-specific half of an insert: acquire the writer lock, atomically
/// persist `(blob, entry)`, assign the monotonic sequence, and publish the
/// resulting events.
#[async_trait(?Send)]
pub trait InsertCommitter {
    /// Persist `(blob, entry)` atomically and publish the insert events.
    ///
    /// `publish_insert` MUST be called while the writer lock acquired here is
    /// still held — broadcasting under the lock is what serializes the history
    /// snapshot taken by `subscribe` against the live channel.
    async fn commit_insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()>;
}

/// The shared insert envelope: validate hash + signature, then hand off to the
/// backend's [`InsertCommitter::commit_insert`] for the atomic locked write.
pub async fn run_insert<C: InsertCommitter>(
    committer: &C,
    verifier: &dyn SignatureVerifier,
    entry: SignedKvEntry,
    blob: Option<ContentBlock>,
) -> Result<()> {
    if let Some(b) = &blob {
        if b.hash() != entry.value_hash {
            return Err(Error::HashMismatch);
        }
    }
    verifier.verify(&entry)?;
    committer.commit_insert(entry, blob).await
}
