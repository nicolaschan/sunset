//! `forward_targets`: given an event, which peers should I forward it to?
//!
//! Free function because the input data — per-peer interests — lives in
//! `engine::EngineState::peer_sessions`, not in `routing::Routes`.

use std::collections::{HashMap, HashSet};

use sunset_store::{Filter, VerifyingKey};

use crate::routing::FilterHash;
use crate::types::PeerId;

/// Generic over the per-peer container so this function is unit-testable
/// without depending on `engine::PeerSession`. The engine adapter passes
/// `&peer_sessions` plus a closure that pulls the interests map out of
/// each `PeerSession`.
pub fn forward_targets<S, F>(
    peers: &HashMap<PeerId, S>,
    interests: F,
    vk: &VerifyingKey,
    name: &[u8],
) -> HashSet<PeerId>
where
    F: Fn(&S) -> &HashMap<FilterHash, Filter>,
{
    peers
        .iter()
        .filter(|(_, sess)| interests(sess).values().any(|f| f.matches(vk, name)))
        .map(|(p, _)| p.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    fn pid(seed: &[u8]) -> PeerId {
        PeerId(vk(seed))
    }

    fn one(filter: Filter) -> HashMap<FilterHash, Filter> {
        let mut m = HashMap::new();
        m.insert(crate::routing::filter_hash(&filter), filter);
        m
    }

    #[test]
    fn returns_each_matching_peer_once() {
        let mut peers: HashMap<PeerId, HashMap<FilterHash, Filter>> = HashMap::new();
        peers.insert(
            pid(b"alice"),
            one(Filter::NamePrefix(Bytes::from_static(b"room/"))),
        );
        peers.insert(pid(b"bob"), one(Filter::Keyspace(vk(b"writer"))));
        peers.insert(pid(b"carol"), HashMap::new());

        let targets = forward_targets(&peers, |s| s, &vk(b"writer"), b"room/x");
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&pid(b"alice")));
        assert!(targets.contains(&pid(b"bob")));
    }

    #[test]
    fn empty_peers_returns_empty_set() {
        let peers: HashMap<PeerId, HashMap<FilterHash, Filter>> = HashMap::new();
        let targets = forward_targets(&peers, |s| s, &vk(b"x"), b"name");
        assert!(targets.is_empty());
    }

    #[test]
    fn multi_interest_peer_matches_on_any_one() {
        let mut interests = HashMap::new();
        let f1 = Filter::NamePrefix(Bytes::from_static(b"a/"));
        let f2 = Filter::NamePrefix(Bytes::from_static(b"b/"));
        interests.insert(crate::routing::filter_hash(&f1), f1);
        interests.insert(crate::routing::filter_hash(&f2), f2);
        let mut peers = HashMap::new();
        peers.insert(pid(b"p"), interests);

        let t1 = forward_targets(&peers, |s| s, &vk(b"x"), b"a/x");
        let t2 = forward_targets(&peers, |s| s, &vk(b"x"), b"b/y");
        let t3 = forward_targets(&peers, |s| s, &vk(b"x"), b"c/z");
        assert!(t1.contains(&pid(b"p")));
        assert!(t2.contains(&pid(b"p")));
        assert!(t3.is_empty());
    }
}
