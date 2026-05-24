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

use crate::{Event, Filter, Hash, Result};

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
    /// `BlobAdded` / `BlobRemoved` have no key and are delivered to all
    /// subscribers regardless of filter.
    pub fn broadcast(&self, event: &Event) {
        let (vk, name) = match event {
            Event::Inserted(e) | Event::Expired(e) => (Some(&e.verifying_key), Some(&e.name)),
            Event::Replaced { new, .. } => (Some(&new.verifying_key), Some(&new.name)),
            Event::BlobAdded(_) | Event::BlobRemoved(_) => (None, None),
        };

        let mut g = self.entries.lock().unwrap();
        g.retain(|w| {
            let Some(s) = w.upgrade() else {
                return false;
            };
            let interested = match (vk, name) {
                (Some(v), Some(n)) => s.filter.matches(v, n.as_ref()),
                _ => true,
            };
            if interested {
                let _ = s.tx.send(Ok(event.clone()));
            }
            true
        });
    }

    /// Broadcast the entry event for an `insert`, then — if a new blob was
    /// added by the same operation — broadcast `BlobAdded` for it. Both
    /// fire inside the caller's writer-critical section (the caller still
    /// holds the lock that synchronizes broadcasts with `subscribe`).
    ///
    /// The ordering "entry event first, then blob event" is a documented
    /// store invariant; it lives here so each backend cannot forget it.
    pub fn publish_insert(&self, entry_event: &Event, blob_added: Option<Hash>) {
        self.broadcast(entry_event);
        if let Some(h) = blob_added {
            self.broadcast(&Event::BlobAdded(h));
        }
    }
}
