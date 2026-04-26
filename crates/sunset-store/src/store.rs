//! The `Store` trait: the public surface every backend implements.

use async_trait::async_trait;
use futures::stream::LocalBoxStream;

use crate::error::Result;
use crate::filter::{Event, Filter, Replay};
use crate::types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};

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
    /// Validation order:
    /// 1. If `blob` is `Some`, `entry.value_hash` must equal `blob.hash()`.
    /// 2. The configured `SignatureVerifier` must accept the entry.
    /// 3. LWW: an existing entry with `priority >= entry.priority` causes `Error::Stale`.
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
    /// store (the `Since` predicate is `sequence >= c.0`). A cursor captured
    /// before any insert is `Cursor(0)`.
    async fn current_cursor(&self) -> Result<Cursor>;
}
