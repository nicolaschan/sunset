//! In-engine routing state: outbound subscriptions and broadcast intents.
//!
//! See `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`.
//!
//! Inbound interests (what other peers want from me) live in
//! `engine::PeerSession::interests`, not here. Per-peer state belongs with
//! the rest of the peer-keyed connection state so peer drop is one removal.

use std::collections::HashMap;

use sunset_store::Filter;

use crate::routing::policy::SubscriptionPolicy;
use crate::types::PeerId;

/// 32-byte blake3 hash of postcard(filter). Used as a key wherever the
/// filter itself would be redundant, or would force a `Hash` impl on
/// `Filter` (which the store doesn't currently provide).
pub type FilterHash = [u8; 32];

/// Length, in hex characters, of a `FilterHash` when rendered into a
/// subscription entry name (see `routing::naming::subscription_name`).
/// Derived from `FilterHash` so it never drifts from the underlying
/// hash size. Used by `decode_filter_hash_from_name` for the prefix
/// length check.
pub const FILTER_HASH_HEX_LEN: usize = 2 * std::mem::size_of::<FilterHash>();

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct OutboundKey {
    pub filter_hash: FilterHash,
    pub provider: PeerId,
}

#[derive(Clone, Debug)]
pub struct Outbound {
    pub filter: Filter,
    pub policy: SubscriptionPolicy,
    pub last_published_ms: u64,
}

#[derive(Clone, Debug)]
pub struct BroadcastIntent {
    pub filter: Filter,
    pub policy: SubscriptionPolicy,
}

/// In-engine routing state. Owns the per-(filter, provider) outbound
/// subscriptions we have published and the per-filter broadcast intents
/// we are tracking.
///
/// Both maps are private — callers go through the accessor methods so
/// the engine doesn't reach into the routing data structures directly.
/// This isolates "where is this data stored?" decisions to this module.
pub struct Routes {
    me: PeerId,
    my_subs: HashMap<OutboundKey, Outbound>,
    broadcast_intents: HashMap<FilterHash, BroadcastIntent>,
}

impl Routes {
    pub fn new(me: PeerId) -> Self {
        Self {
            me,
            my_subs: HashMap::new(),
            broadcast_intents: HashMap::new(),
        }
    }

    pub fn me(&self) -> &PeerId {
        &self.me
    }

    /// Keys of outbound subscriptions whose `last_published_ms` is at
    /// least `policy.refresh_interval()` behind `now_ms`.
    pub fn due_for_refresh(&self, now_ms: u64) -> Vec<OutboundKey> {
        self.my_subs
            .iter()
            .filter(|(_, ob)| {
                let refresh = ob.policy.refresh_interval().as_millis() as u64;
                now_ms.saturating_sub(ob.last_published_ms) >= refresh
            })
            .map(|(k, _)| k.clone())
            .collect()
    }

    // ----- outbound (my_subs) -----

    /// Record an outbound subscription, replacing any previous entry at
    /// the same key. Returns the previous outbound, if any.
    pub fn insert_outbound(&mut self, key: OutboundKey, ob: Outbound) -> Option<Outbound> {
        self.my_subs.insert(key, ob)
    }

    /// Remove and return the outbound at `key`, if any.
    pub fn take_outbound(&mut self, key: &OutboundKey) -> Option<Outbound> {
        self.my_subs.remove(key)
    }

    /// `last_published_ms` for the outbound at `key`, if any.
    pub fn outbound_last_published(&self, key: &OutboundKey) -> Option<u64> {
        self.my_subs.get(key).map(|ob| ob.last_published_ms)
    }

    /// `(filter, policy)` for the outbound at `key`, if any.
    pub fn outbound_filter_policy(
        &self,
        key: &OutboundKey,
    ) -> Option<(Filter, SubscriptionPolicy)> {
        self.my_subs
            .get(key)
            .map(|ob| (ob.filter.clone(), ob.policy))
    }

    /// Providers we currently maintain outbound subscriptions to for
    /// the given filter hash.
    pub fn outbound_providers_for_filter(&self, filter_hash: &FilterHash) -> Vec<PeerId> {
        self.my_subs
            .keys()
            .filter(|k| &k.filter_hash == filter_hash)
            .map(|k| k.provider.clone())
            .collect()
    }

    // ----- broadcast intents -----

    /// Record a broadcast intent for `filter_hash`. Returns the previous
    /// intent, if any.
    pub fn insert_broadcast_intent(
        &mut self,
        filter_hash: FilterHash,
        intent: BroadcastIntent,
    ) -> Option<BroadcastIntent> {
        self.broadcast_intents.insert(filter_hash, intent)
    }

    /// Remove and return the broadcast intent at `filter_hash`, if any.
    pub fn take_broadcast_intent(&mut self, filter_hash: &FilterHash) -> Option<BroadcastIntent> {
        self.broadcast_intents.remove(filter_hash)
    }

    /// Snapshot of every current broadcast intent. Cloned so callers
    /// can drop the routing lock before iterating.
    pub fn broadcast_intents_snapshot(&self) -> Vec<BroadcastIntent> {
        self.broadcast_intents.values().cloned().collect()
    }
}

/// Compute the `FilterHash` for a filter. Single source of truth used by
/// `routing::naming::subscription_name` and by callers that already have
/// the hash (e.g., decoded from an entry name).
pub fn filter_hash(filter: &Filter) -> FilterHash {
    let bytes = postcard::to_stdvec(filter).expect("postcard filter encode is infallible");
    *blake3::hash(&bytes).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::time::Duration;
    use sunset_store::VerifyingKey;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    fn pid(seed: &[u8]) -> PeerId {
        PeerId(vk(seed))
    }

    fn outbound(threshold_ms: u64, last_ms: u64) -> Outbound {
        Outbound {
            filter: Filter::NamePrefix(Bytes::from_static(b"x/")),
            policy: SubscriptionPolicy {
                freshness_threshold: Duration::from_millis(threshold_ms),
            },
            last_published_ms: last_ms,
        }
    }

    #[test]
    fn due_for_refresh_returns_entries_past_half_threshold() {
        let mut routes = Routes::new(pid(b"me"));
        let key = OutboundKey {
            filter_hash: [0u8; 32],
            provider: pid(b"p"),
        };
        routes.insert_outbound(key.clone(), outbound(1000, 0));
        assert!(routes.due_for_refresh(499).is_empty());
        assert_eq!(routes.due_for_refresh(500), vec![key]);
    }

    #[test]
    fn due_for_refresh_skips_fresh_entries() {
        let mut routes = Routes::new(pid(b"me"));
        let key = OutboundKey {
            filter_hash: [0u8; 32],
            provider: pid(b"p"),
        };
        routes.insert_outbound(key, outbound(1000, 800));
        assert!(routes.due_for_refresh(1000).is_empty());
    }

    #[test]
    fn due_for_refresh_honors_relay_broad_policy_cadence() {
        let mut routes = Routes::new(pid(b"me"));
        let key = OutboundKey {
            filter_hash: [0u8; 32],
            provider: pid(b"p"),
        };
        let ob = Outbound {
            filter: Filter::NamePrefix(Bytes::from_static(b"x/")),
            policy: SubscriptionPolicy::relay_broad(),
            last_published_ms: 0,
        };
        routes.insert_outbound(key.clone(), ob);
        // relay_broad: freshness_threshold = 30s, refresh_interval = 15s
        assert!(
            routes.due_for_refresh(14_999).is_empty(),
            "should not be due before 15s"
        );
        assert_eq!(
            routes.due_for_refresh(15_000),
            vec![key],
            "should be due at 15s"
        );
    }

    #[test]
    fn filter_hash_is_deterministic() {
        let f = Filter::Namespace(Bytes::from_static(b"x"));
        assert_eq!(filter_hash(&f), filter_hash(&f));
    }

    #[test]
    fn filter_hash_differs_per_filter() {
        let a = Filter::Namespace(Bytes::from_static(b"x"));
        let b = Filter::Namespace(Bytes::from_static(b"y"));
        assert_ne!(filter_hash(&a), filter_hash(&b));
    }
}
