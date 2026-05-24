//! Per-filter policy parameters for the receiver-side routing loop.
//!
//! There are exactly two knobs:
//!
//! - `target_n` — how many healthy providers to maintain (1 = reactive,
//!   2 = dual-delivery for gap-free failover).
//! - `freshness_threshold` — how long the receiver waits without hearing
//!   anything via a provider before declaring it dead.
//!
//! Adding any third knob (per-provider weights, dwell times, switch
//! thresholds) would re-introduce the enumerated-cases-as-algorithm
//! anti-pattern the cooperative-relay design explicitly avoids.

use std::time::Duration;

/// Per-filter routing policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscriptionPolicy {
    pub target_n: usize,
    pub freshness_threshold: Duration,
}

impl SubscriptionPolicy {
    /// Reactive single-provider policy with a 5-second freshness budget
    /// — the default for reliable store-data subscriptions.
    pub const fn store_data() -> Self {
        Self {
            target_n: 1,
            freshness_threshold: Duration::from_secs(5),
        }
    }

    /// Dual-delivery policy with a 200ms freshness budget — used by the
    /// voice subsystem while a call is active, where gaps are perceptible.
    pub const fn voice_active_call() -> Self {
        Self {
            target_n: 2,
            freshness_threshold: Duration::from_millis(200),
        }
    }

    /// Broadcast-style relay subscription: target_n=0 (the broadcast
    /// intent doesn't map to a per-provider count yet; Phase 3 will
    /// give this meaning), 30s freshness threshold. A relay maintains
    /// many concurrent subscriptions; a slower refresh (vs.
    /// store_data's 5s) keeps the routing-tick churn manageable at
    /// relay scale.
    pub const fn relay_broad() -> Self {
        Self {
            target_n: 0,
            freshness_threshold: Duration::from_secs(30),
        }
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
    fn store_data_defaults() {
        let p = SubscriptionPolicy::store_data();
        assert_eq!(p.target_n, 1);
        assert_eq!(p.freshness_threshold, Duration::from_secs(5));
    }

    #[test]
    fn voice_active_call_uses_dual_delivery() {
        let p = SubscriptionPolicy::voice_active_call();
        assert_eq!(p.target_n, 2);
        assert_eq!(p.freshness_threshold, Duration::from_millis(200));
    }

    #[test]
    fn relay_broad_uses_slower_refresh() {
        let p = SubscriptionPolicy::relay_broad();
        assert_eq!(p.target_n, 0);
        assert_eq!(p.freshness_threshold, Duration::from_secs(30));
    }

    #[test]
    fn default_matches_store_data() {
        assert_eq!(
            SubscriptionPolicy::default(),
            SubscriptionPolicy::store_data()
        );
    }
}
