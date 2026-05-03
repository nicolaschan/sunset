//! In-memory tracker mapping `PeerId -> Filter`, built from KV entries
//! under `_sunset-sync/subscribe`.

use std::collections::HashMap;

use sunset_store::{Filter, SignedKvEntry, VerifyingKey};

use crate::error::Error;
use crate::types::PeerId;

#[derive(Default, Debug)]
pub struct SubscriptionRegistry {
    /// Peer's verifying key → declared filter. The peer's PeerId is
    /// `PeerId(verifying_key)` so the map is effectively
    /// `PeerId -> Filter`.
    by_peer: HashMap<VerifyingKey, Filter>,
}

impl SubscriptionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the filter for `vk` with `filter`. Returns the previous filter
    /// for that peer, if any. Mirrors `HashMap::insert`'s return semantics so
    /// callers can distinguish new / changed / unchanged subscriptions.
    pub fn insert(&mut self, vk: VerifyingKey, filter: Filter) -> Option<Filter> {
        self.by_peer.insert(vk, filter)
    }

    /// Remove `vk`'s registration (e.g., on TTL expiration).
    pub fn remove(&mut self, vk: &VerifyingKey) {
        self.by_peer.remove(vk);
    }

    /// All currently-registered peer filters.
    pub fn iter(&self) -> impl Iterator<Item = (&VerifyingKey, &Filter)> {
        self.by_peer.iter()
    }

    /// All `PeerId`s whose filter matches the given `(vk, name)`.
    pub fn peers_matching<'a>(
        &'a self,
        vk: &'a VerifyingKey,
        name: &'a [u8],
    ) -> impl Iterator<Item = PeerId> + 'a {
        self.by_peer.iter().filter_map(move |(peer_vk, filter)| {
            if filter.matches(vk, name) {
                Some(PeerId(peer_vk.clone()))
            } else {
                None
            }
        })
    }

    /// Union of all currently-registered filters. Returns `None` if no
    /// peers are registered. Used by the engine to subscribe to the local
    /// store with a single filter that covers all peer interests.
    pub fn union_filter(&self) -> Option<Filter> {
        if self.by_peer.is_empty() {
            None
        } else {
            Some(Filter::Union(self.by_peer.values().cloned().collect()))
        }
    }

    pub fn len(&self) -> usize {
        self.by_peer.len()
    }
    pub fn is_empty(&self) -> bool {
        self.by_peer.is_empty()
    }
}

/// Decode a `SignedKvEntry` whose value is supposed to be a postcard-encoded
/// `Filter`, given the corresponding `ContentBlock`. Returns `Error::Decode`
/// on parse failure.
pub fn parse_subscription_entry(
    entry: &SignedKvEntry,
    block: &sunset_store::ContentBlock,
) -> std::result::Result<Filter, Error> {
    if entry.value_hash != block.hash() {
        return Err(Error::Protocol(
            "subscription entry value_hash does not match supplied ContentBlock".into(),
        ));
    }
    postcard::from_bytes(&block.data)
        .map_err(|e| Error::Decode(format!("subscription filter: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::{ContentBlock, Filter, VerifyingKey};

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    #[test]
    fn insert_and_lookup() {
        let mut r = SubscriptionRegistry::new();
        r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
        let peers: Vec<_> = r.peers_matching(&vk(b"chat-1"), b"k").collect();
        assert_eq!(peers, vec![PeerId(vk(b"alice"))]);
    }

    #[test]
    fn no_match_returns_empty() {
        let mut r = SubscriptionRegistry::new();
        r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
        let peers: Vec<_> = r.peers_matching(&vk(b"chat-2"), b"k").collect();
        assert!(peers.is_empty());
    }

    #[test]
    fn union_filter_combines_all_peers() {
        let mut r = SubscriptionRegistry::new();
        r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
        r.insert(vk(b"bob"), Filter::Keyspace(vk(b"chat-2")));
        let union = r.union_filter().unwrap();
        assert!(union.matches(&vk(b"chat-1"), b"k"));
        assert!(union.matches(&vk(b"chat-2"), b"k"));
        assert!(!union.matches(&vk(b"chat-3"), b"k"));
    }

    #[test]
    fn union_empty_returns_none() {
        let r = SubscriptionRegistry::new();
        assert!(r.union_filter().is_none());
    }

    #[test]
    fn parse_subscription_decodes_filter() {
        let filter = Filter::Keyspace(vk(b"chat-1"));
        let bytes = postcard::to_stdvec(&filter).unwrap();
        let block = ContentBlock {
            data: Bytes::from(bytes),
            references: vec![],
        };
        let entry = SignedKvEntry {
            verifying_key: vk(b"alice"),
            name: Bytes::from_static(b"_sunset-sync/subscribe"),
            value_hash: block.hash(),
            priority: 1,
            expires_at: None,
            signature: Bytes::from_static(b"sig"),
        };
        let parsed = parse_subscription_entry(&entry, &block).unwrap();
        assert_eq!(parsed, filter);
    }

    #[test]
    fn insert_returns_none_for_new_peer() {
        let mut r = SubscriptionRegistry::new();
        let prev = r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
        assert!(prev.is_none(), "expected None when inserting a new peer");
    }

    #[test]
    fn insert_returns_previous_filter_for_existing_peer() {
        let mut r = SubscriptionRegistry::new();
        r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-1")));
        let prev = r.insert(vk(b"alice"), Filter::Keyspace(vk(b"chat-2")));
        assert_eq!(prev, Some(Filter::Keyspace(vk(b"chat-1"))));
    }

    #[test]
    fn insert_returns_same_filter_when_unchanged() {
        let mut r = SubscriptionRegistry::new();
        let f = Filter::Keyspace(vk(b"chat-1"));
        r.insert(vk(b"alice"), f.clone());
        let prev = r.insert(vk(b"alice"), f.clone());
        assert_eq!(prev, Some(f));
    }

    #[test]
    fn parse_subscription_rejects_wrong_block() {
        let filter = Filter::Keyspace(vk(b"chat-1"));
        let bytes = postcard::to_stdvec(&filter).unwrap();
        let block_a = ContentBlock {
            data: Bytes::from(bytes),
            references: vec![],
        };
        let block_b = ContentBlock {
            data: Bytes::from_static(b"different"),
            references: vec![],
        };
        let entry = SignedKvEntry {
            verifying_key: vk(b"alice"),
            name: Bytes::from_static(b"_sunset-sync/subscribe"),
            value_hash: block_a.hash(),
            priority: 1,
            expires_at: None,
            signature: Bytes::from_static(b"sig"),
        };
        let err = parse_subscription_entry(&entry, &block_b).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }
}
