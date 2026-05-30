//! Per-store subscription manager shared by all `Store` backends.
//!
//! Maintains a list of broadcast channels, one per active subscription,
//! each filtered by the subscriber's [`Filter`]. Backends register a
//! [`Subscription`] inside their writer-critical-section and call
//! [`SubscriptionList::broadcast`] from the same section; this is what
//! serializes history-snapshot vs. live-channel delivery and prevents
//! events from being delivered both ways or neither.

use std::sync::{Arc, Mutex, Weak};

use tokio::sync::mpsc;

use crate::{Event, Filter, Hash, Result, SignedKvEntry};

/// Result of the backend's LWW write: whether the entry was new, or replaced
/// an existing one (carrying the displaced entry for the `Replaced` event).
pub enum InsertOutcome {
    Inserted,
    Replaced { old: SignedKvEntry },
}

/// A live subscription: the filter the subscriber asked for, and the
/// sender half of its live-event channel.
///
/// `tx` MUST stay an `UnboundedSender`. [`SubscriptionList::broadcast`]
/// runs while the backend's writer lock is held; switching to a bounded
/// channel would let one slow subscriber stall every writer.
#[derive(Debug)]
pub struct Subscription {
    pub filter: Filter,
    pub tx: mpsc::UnboundedSender<Result<Event>>,
}

/// Subscriptions registered with a store; weak references so dropped
/// streams are reclaimed lazily on the next `add` or `broadcast`.
///
/// The internal `Mutex` is `std::sync::Mutex` (not `tokio::sync::Mutex`)
/// because the critical section is bounded, synchronous, and contains no
/// `.await`. The `.unwrap()`s on `lock()` rely on the invariant that
/// nothing inside the critical section panics: `Arc::downgrade`,
/// `Vec::push`, `Vec::retain`, `UnboundedSender::send` (returns
/// `Result<(), _>` which we discard), `Event::clone`, and `Filter::matches`
/// are all non-panicking, barring an allocator panic which would already
/// have aborted the process. Lock poisoning is therefore unreachable in
/// production.
#[derive(Debug, Default)]
pub struct SubscriptionList {
    entries: Mutex<Vec<Weak<Subscription>>>,
}

impl SubscriptionList {
    /// Register a new subscription. Also sweeps dropped subscriptions.
    pub fn add(&self, sub: &Arc<Subscription>) {
        let mut g = self.entries.lock().unwrap();
        g.retain(|w| w.upgrade().is_some());
        g.push(Arc::downgrade(sub));
    }

    /// Broadcast an event to every live subscription whose filter matches.
    /// Per-variant matching (including the "blob events go to all" rule)
    /// lives on [`Filter::matches_event`].
    pub fn broadcast(&self, event: &Event) {
        let mut g = self.entries.lock().unwrap();
        g.retain(|w| {
            let Some(s) = w.upgrade() else {
                return false;
            };
            if s.filter.matches_event(event) {
                let _ = s.tx.send(Ok(event.clone()));
            }
            true
        });
    }

    /// Map the LWW `outcome` to its `Inserted`/`Replaced` event, broadcast it,
    /// then — if a new blob was added by the same operation — broadcast
    /// `BlobAdded`. Both fire inside the caller's writer-critical section (the
    /// caller still holds the lock that synchronizes broadcasts with
    /// `subscribe`). The outcome→event mapping and the "entry event first,
    /// then blob event" ordering are store invariants; they live here so each
    /// backend cannot forget them.
    pub fn publish_insert(
        &self,
        outcome: InsertOutcome,
        new_entry: SignedKvEntry,
        blob_added: Option<Hash>,
    ) {
        let entry_event = match outcome {
            InsertOutcome::Inserted => Event::Inserted(new_entry),
            InsertOutcome::Replaced { old } => Event::Replaced {
                old,
                new: new_entry,
            },
        };
        self.broadcast(&entry_event);
        if let Some(h) = blob_added {
            self.broadcast(&Event::BlobAdded(h));
        }
    }
}
