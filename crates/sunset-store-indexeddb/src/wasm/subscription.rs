//! Per-store subscription manager. Mirrors the design of
//! `sunset-store-memory::subscription` — same invariants, same
//! reasoning. The list lives behind `std::sync::Mutex` because the
//! critical section is bounded and synchronous.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use sunset_store::{Event, Filter};
use tokio::sync::mpsc;

#[derive(Debug)]
pub(crate) struct Subscription {
    pub filter: Filter,
    /// MUST stay unbounded: `broadcast` runs while holding the
    /// per-store write lock, so a bounded channel would let one slow
    /// subscriber stall every writer.
    pub tx: mpsc::UnboundedSender<sunset_store::Result<Event>>,
}

#[derive(Debug, Default)]
pub(crate) struct SubscriptionList {
    pub entries: RefCell<Vec<Weak<Subscription>>>,
}

impl SubscriptionList {
    pub fn add(&self, sub: &Rc<Subscription>) {
        let mut g = self.entries.borrow_mut();
        g.retain(|w| w.upgrade().is_some());
        g.push(Rc::downgrade(sub));
    }

    /// Broadcast `event` to every live subscription whose filter matches.
    /// `BlobAdded` / `BlobRemoved` are delivered regardless of filter
    /// (they have no key to match on); other events match by `(vk, name)`.
    pub fn broadcast(&self, event: &Event) {
        let (vk, name) = match event {
            Event::Inserted(e) | Event::Expired(e) => (Some(&e.verifying_key), Some(&e.name)),
            Event::Replaced { new, .. } => (Some(&new.verifying_key), Some(&new.name)),
            Event::BlobAdded(_) | Event::BlobRemoved(_) => (None, None),
        };

        let mut g = self.entries.borrow_mut();
        g.retain(|w| {
            if let Some(s) = w.upgrade() {
                let interested = match (vk, name) {
                    (Some(v), Some(n)) => s.filter.matches(v, n.as_ref()),
                    _ => true,
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
