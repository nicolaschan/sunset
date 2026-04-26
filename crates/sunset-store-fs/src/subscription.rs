//! Subscription list. Mirrors the broadcast-under-writer-lock invariant from
//! sunset-store-memory.

use std::sync::{Arc, Mutex, Weak};

use sunset_store::{Event, Filter, Result};
use tokio::sync::mpsc;

/// A live subscription: a filter and the sender half of the live-event channel.
///
/// `tx` MUST stay an `UnboundedSender`. `broadcast` runs while the FsStore
/// writer mutex is held; switching to a bounded channel would let a slow
/// subscriber stall every writer.
pub struct Subscription {
    pub filter: Filter,
    pub tx: mpsc::UnboundedSender<Result<Event>>,
}

#[derive(Default)]
pub struct SubscriptionList {
    /// Holds `Weak<Subscription>` so dropped streams clean up automatically.
    /// The `Mutex` is `std::sync::Mutex` (not tokio): the critical sections are
    /// trivially short, allocation-only, with no `.await` inside. The unwraps
    /// can only panic if a panic occurred *inside* the lock — `add` and
    /// `broadcast` do nothing that can panic (`Vec::retain`, `Vec::push`,
    /// `Filter::matches`, `Event::clone`, `mpsc::send` are all non-panicking,
    /// barring an allocator panic which would already have aborted the
    /// process). Lock poisoning is therefore unreachable in production.
    entries: Mutex<Vec<Weak<Subscription>>>,
}

impl SubscriptionList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, sub: &Arc<Subscription>) {
        let mut g = self.entries.lock().unwrap();
        g.retain(|w| w.upgrade().is_some());
        g.push(Arc::downgrade(sub));
    }

    pub fn broadcast(&self, event: &Event) {
        // Extract (vk, name) for filter matching when the event is keyed.
        // BlobAdded / BlobRemoved have no key and are delivered to all subscribers.
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
}
