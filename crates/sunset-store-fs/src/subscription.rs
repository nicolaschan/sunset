//! SubscriptionList placeholder — Task 8 fills in real broadcast/subscribe.

use sunset_store::Event;

#[derive(Default)]
pub struct SubscriptionList;

impl SubscriptionList {
    pub fn new() -> Self {
        Self
    }

    pub fn broadcast(&self, _event: &Event) {
        // Task 8
    }
}
