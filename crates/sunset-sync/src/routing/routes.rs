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

    #[test]
    fn me_returns_constructed_peer_id() {
        let me = pid(b"alice");
        let routes = Routes::new(me.clone());
        assert_eq!(routes.me(), &me);
    }

    #[test]
    fn broadcast_intents_snapshot_empty_when_none_inserted() {
        let routes = Routes::new(pid(b"me"));
        assert!(routes.broadcast_intents_snapshot().is_empty());
    }

    #[test]
    fn broadcast_intents_snapshot_returns_inserted_intents() {
        let mut routes = Routes::new(pid(b"me"));
        let f1 = Filter::Keyspace(vk(b"writer1"));
        let f2 = Filter::Keyspace(vk(b"writer2"));
        let intent = |f: &Filter| BroadcastIntent {
            filter: f.clone(),
            policy: SubscriptionPolicy::store_data(),
        };
        assert!(
            routes
                .insert_broadcast_intent(filter_hash(&f1), intent(&f1))
                .is_none()
        );
        assert!(
            routes
                .insert_broadcast_intent(filter_hash(&f2), intent(&f2))
                .is_none()
        );
        let mut filters: Vec<Filter> = routes
            .broadcast_intents_snapshot()
            .into_iter()
            .map(|bi| bi.filter)
            .collect();
        // Order is unspecified (HashMap), so sort by filter_hash for a
        // stable assertion.
        filters.sort_by_key(filter_hash);
        let mut want = [f1, f2];
        want.sort_by_key(filter_hash);
        assert_eq!(filters, want);
    }

    #[test]
    fn take_broadcast_intent_removes_and_returns_intent() {
        let mut routes = Routes::new(pid(b"me"));
        let f = Filter::Keyspace(vk(b"writer"));
        let bi = BroadcastIntent {
            filter: f.clone(),
            policy: SubscriptionPolicy::relay_broad(),
        };
        routes.insert_broadcast_intent(filter_hash(&f), bi);
        let taken = routes
            .take_broadcast_intent(&filter_hash(&f))
            .expect("intent");
        assert_eq!(taken.filter, f);
        assert!(routes.broadcast_intents_snapshot().is_empty());
    }

    #[test]
    fn take_broadcast_intent_returns_none_when_absent() {
        let mut routes = Routes::new(pid(b"me"));
        assert!(routes.take_broadcast_intent(&[0u8; 32]).is_none());
    }

    #[test]
    fn insert_broadcast_intent_replaces_and_returns_previous() {
        let mut routes = Routes::new(pid(b"me"));
        let f = Filter::Keyspace(vk(b"writer"));
        let first = BroadcastIntent {
            filter: f.clone(),
            policy: SubscriptionPolicy::store_data(),
        };
        let second = BroadcastIntent {
            filter: f.clone(),
            policy: SubscriptionPolicy::relay_broad(),
        };
        routes.insert_broadcast_intent(filter_hash(&f), first);
        let prev = routes
            .insert_broadcast_intent(filter_hash(&f), second)
            .expect("replaced");
        assert_eq!(prev.policy, SubscriptionPolicy::store_data());
        // The new one survives.
        let snap = routes.broadcast_intents_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].policy, SubscriptionPolicy::relay_broad());
    }

    #[test]
    fn outbound_providers_for_filter_only_returns_matching_filter() {
        let mut routes = Routes::new(pid(b"me"));
        let f1: FilterHash = [1u8; 32];
        let f2: FilterHash = [2u8; 32];
        let p1 = pid(b"p1");
        let p2 = pid(b"p2");
        let p3 = pid(b"p3");
        // Two providers under f1, one under f2.
        routes.insert_outbound(
            OutboundKey {
                filter_hash: f1,
                provider: p1.clone(),
            },
            outbound(1000, 0),
        );
        routes.insert_outbound(
            OutboundKey {
                filter_hash: f1,
                provider: p2.clone(),
            },
            outbound(1000, 0),
        );
        routes.insert_outbound(
            OutboundKey {
                filter_hash: f2,
                provider: p3.clone(),
            },
            outbound(1000, 0),
        );
        let mut f1_providers = routes.outbound_providers_for_filter(&f1);
        f1_providers.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let mut want = vec![p1, p2];
        want.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        assert_eq!(f1_providers, want);
        assert_eq!(routes.outbound_providers_for_filter(&f2), vec![p3]);
        let absent: FilterHash = [3u8; 32];
        assert!(routes.outbound_providers_for_filter(&absent).is_empty());
    }

    #[test]
    fn outbound_filter_policy_returns_none_when_absent() {
        let routes = Routes::new(pid(b"me"));
        let key = OutboundKey {
            filter_hash: [0u8; 32],
            provider: pid(b"p"),
        };
        assert!(routes.outbound_filter_policy(&key).is_none());
        assert!(routes.outbound_last_published(&key).is_none());
    }

    #[test]
    fn outbound_filter_policy_returns_stored_pair() {
        let mut routes = Routes::new(pid(b"me"));
        let key = OutboundKey {
            filter_hash: [7u8; 32],
            provider: pid(b"p"),
        };
        let ob = Outbound {
            filter: Filter::NamePrefix(Bytes::from_static(b"x/")),
            policy: SubscriptionPolicy::relay_broad(),
            last_published_ms: 42,
        };
        routes.insert_outbound(key.clone(), ob.clone());
        let (filter, policy) = routes.outbound_filter_policy(&key).expect("present");
        assert_eq!(filter, ob.filter);
        assert_eq!(policy, ob.policy);
        assert_eq!(routes.outbound_last_published(&key), Some(42));
    }
}
