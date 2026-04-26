//! Per-store subscription manager: maintains a list of broadcast channels,
//! one per active subscription, each filtered by the subscriber's `Filter`.

use std::sync::{Arc, Mutex, Weak};

use sunset_store::{Event, Filter};
use tokio::sync::mpsc;

#[derive(Debug)]
pub(crate) struct Subscription {
    pub filter: Filter,
    pub tx: mpsc::UnboundedSender<sunset_store::Result<Event>>,
}

#[derive(Debug, Default)]
pub(crate) struct SubscriptionList {
    pub entries: Mutex<Vec<Weak<Subscription>>>,
}

impl SubscriptionList {
    pub fn add(&self, sub: &Arc<Subscription>) {
        let mut g = self.entries.lock().unwrap();
        g.retain(|w| w.upgrade().is_some());
        g.push(Arc::downgrade(sub));
    }

    /// Broadcast an event to every live subscription whose filter matches.
    pub fn broadcast(&self, event: &Event) {
        // Caller passes us an event whose vk/name we can extract; we match per-subscription.
        let (vk, name) = match event {
            Event::Inserted(e) | Event::Expired(e) => (Some(&e.verifying_key), Some(&e.name)),
            Event::Replaced { new, .. } => (Some(&new.verifying_key), Some(&new.name)),
            Event::BlobAdded(_) | Event::BlobRemoved(_) => (None, None),
        };

        let mut g = self.entries.lock().unwrap();
        g.retain(|w| {
            if let Some(s) = w.upgrade() {
                let interested = match (vk, name) {
                    (Some(v), Some(n)) => s.filter.matches(v, n.as_ref()),
                    _ => true, // BlobAdded / BlobRemoved are delivered to all subscribers
                };
                if interested {
                    let _ = s.tx.send(Ok(event.clone()));
                }
                true
            } else {
                false
            }
        });
    }
}
