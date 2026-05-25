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

pub struct Routes {
    me: PeerId,
    pub my_subs: HashMap<OutboundKey, Outbound>,
    pub broadcast_intents: HashMap<FilterHash, BroadcastIntent>,
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

    /// Keys of `my_subs` whose `last_published_ms` is at least
    /// `policy.freshness_threshold / 2` behind `now_ms`.
    pub fn due_for_refresh(&self, now_ms: u64) -> Vec<OutboundKey> {
        self.my_subs
            .iter()
            .filter(|(_, ob)| {
                let half = ob.policy.freshness_threshold.as_millis() as u64 / 2;
                now_ms.saturating_sub(ob.last_published_ms) >= half
            })
            .map(|(k, _)| k.clone())
            .collect()
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
                target_n: 1,
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
        routes.my_subs.insert(key.clone(), outbound(1000, 0));
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
        routes.my_subs.insert(key, outbound(1000, 800));
        assert!(routes.due_for_refresh(1000).is_empty());
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
