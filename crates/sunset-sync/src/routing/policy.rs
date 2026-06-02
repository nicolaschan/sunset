//! Per-filter policy parameters for the receiver-side routing loop.
//!
//! Currently one knob:
//!
//! - `freshness_threshold` — how long an outbound subscription survives
//!   before the routing tick re-publishes it (`entry_ttl`), and the
//!   cadence at which the routing tick re-publishes a still-active
//!   subscription (`refresh_interval = freshness_threshold / 2`).
//!
//! Constructors (`store_data`, `relay_broad`) name *intents*, not
//! parameter tuples, so the underlying tuple can change without
//! disturbing call sites.

use std::time::Duration;

use bytes::Bytes;
use sunset_store::Filter;

/// The wire filter that pairs with [`SubscriptionPolicy::relay_broad`]
/// — a `NamePrefix("")` matching every entry. Production relays pass
/// this to `engine.subscribe(...)` to declare "I want everything".
pub fn relay_broad_filter() -> Filter {
    Filter::NamePrefix(Bytes::new())
}

/// Per-filter routing policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscriptionPolicy {
    pub freshness_threshold: Duration,
}

impl SubscriptionPolicy {
    /// Reactive single-provider policy with a 5-second freshness budget
    /// — the default for reliable store-data subscriptions.
    pub const fn store_data() -> Self {
        Self {
            freshness_threshold: Duration::from_secs(5),
        }
    }

    /// Broadcast-style relay subscription with a 30s freshness threshold.
    /// A relay maintains many concurrent subscriptions; a slower refresh
    /// (vs. store_data's 5s) keeps the routing-tick churn manageable at
    /// relay scale.
    pub const fn relay_broad() -> Self {
        Self {
            freshness_threshold: Duration::from_secs(30),
        }
    }

    /// TTL set on the published `SubscriptionEntry`.
    pub fn entry_ttl(&self) -> Duration {
        self.freshness_threshold
    }

    /// Cadence at which the routing tick re-publishes a still-active
    /// subscription. Half-freshness so the receiver has at least one
    /// refresh window of slack between writes before the entry expires.
    pub fn refresh_interval(&self) -> Duration {
        self.freshness_threshold / 2
    }
}

impl Default for SubscriptionPolicy {
    /// Defaults to `store_data()` — the safe, low-bandwidth choice.
    fn default() -> Self {
        Self::store_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_data_entry_ttl_and_refresh_interval() {
        let p = SubscriptionPolicy::store_data();
        assert_eq!(p.entry_ttl(), Duration::from_secs(5));
        assert_eq!(p.refresh_interval(), Duration::from_millis(2500));
    }

    #[test]
    fn relay_broad_entry_ttl_and_refresh_interval() {
        let p = SubscriptionPolicy::relay_broad();
        assert_eq!(p.entry_ttl(), Duration::from_secs(30));
        assert_eq!(p.refresh_interval(), Duration::from_secs(15));
    }
}
