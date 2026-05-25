//! `forward_targets`: given an event, which peers should I forward it to?
//!
//! Free function because the input data — per-peer interests — lives in
//! `engine::EngineState::peer_sessions`, not in `routing::Routes`.
//!
//! The function is generic over a [`PeerInterests`] trait rather than
//! reaching into a concrete `engine::PeerSession`: routing is a lower
//! layer than the engine, so we describe the shape we need ("a thing
//! that has an interests map") via a trait and let the engine adapter
//! impl it on `PeerSession`. The previous closure-generic shape
//! achieved the same decoupling but forced callers to round-trip
//! through `HashSet<PeerId>` + a second `peer_sessions.get(&peer)`
//! lookup to recover the session they already had in hand.

use std::collections::HashMap;

use sunset_store::{Filter, VerifyingKey};

use crate::routing::FilterHash;
use crate::types::PeerId;

/// The seam between routing and the engine's per-peer session state.
/// Implemented on `engine::PeerSession` so [`forward_targets`] can talk
/// about "peers with an interests map" without depending on the
/// concrete session type. Test fixtures impl this on simple stand-ins
/// (e.g. the interests map itself; see the tests below).
pub trait PeerInterests {
    fn interests(&self) -> &HashMap<FilterHash, Filter>;
}

/// Yield each `(peer, session)` whose interests contain at least one
/// filter matching `(vk, name)`. Order is unspecified (HashMap
/// iteration order). Each peer appears at most once even if multiple
/// of its filters match — the predicate short-circuits via `any`.
///
/// Returning borrowed pairs lets callers send via `session.tx` (or
/// whatever sender the session carries) without a second
/// `peer_sessions.get(&peer)` lookup.
pub fn forward_targets<'a, T: PeerInterests>(
    peers: &'a HashMap<PeerId, T>,
    vk: &'a VerifyingKey,
    name: &'a [u8],
) -> impl Iterator<Item = (&'a PeerId, &'a T)> + 'a {
    peers
        .iter()
        .filter(move |(_, sess)| sess.interests().values().any(|f| f.matches(vk, name)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    /// Test fixture: a bare interests map IS a `PeerInterests`. Lets
    /// the unit tests build `HashMap<PeerId, HashMap<FilterHash, Filter>>`
    /// directly instead of wrapping each one in a stand-in session struct.
    impl PeerInterests for HashMap<FilterHash, Filter> {
        fn interests(&self) -> &HashMap<FilterHash, Filter> {
            self
        }
    }

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

    fn collect_ids<'a, T: PeerInterests + 'a>(
        it: impl Iterator<Item = (&'a PeerId, &'a T)>,
    ) -> Vec<PeerId> {
        it.map(|(p, _)| p.clone()).collect()
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

        let targets = collect_ids(forward_targets(&peers, &vk(b"writer"), b"room/x"));
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&pid(b"alice")));
        assert!(targets.contains(&pid(b"bob")));
    }

    #[test]
    fn empty_peers_returns_empty_set() {
        let peers: HashMap<PeerId, HashMap<FilterHash, Filter>> = HashMap::new();
        let targets = collect_ids(forward_targets(&peers, &vk(b"x"), b"name"));
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

        let t1 = collect_ids(forward_targets(&peers, &vk(b"x"), b"a/x"));
        let t2 = collect_ids(forward_targets(&peers, &vk(b"x"), b"b/y"));
        let t3 = collect_ids(forward_targets(&peers, &vk(b"x"), b"c/z"));
        assert!(t1.contains(&pid(b"p")));
        assert!(t2.contains(&pid(b"p")));
        assert!(t3.is_empty());
    }

    /// Regression for the previous test name's "once" claim — make a
    /// peer whose interests have *two* filters that both match the
    /// same `(vk, name)`, and assert the peer is yielded once rather
    /// than once-per-matching-filter.
    #[test]
    fn peer_with_multiple_matching_filters_appears_once() {
        // Both filters match "room/x" authored by `vk(b"writer")`.
        let f1 = Filter::NamePrefix(Bytes::from_static(b"room/"));
        let f2 = Filter::Keyspace(vk(b"writer"));
        let mut interests = HashMap::new();
        interests.insert(crate::routing::filter_hash(&f1), f1);
        interests.insert(crate::routing::filter_hash(&f2), f2);

        let mut peers: HashMap<PeerId, HashMap<FilterHash, Filter>> = HashMap::new();
        peers.insert(pid(b"alice"), interests);

        let targets = collect_ids(forward_targets(&peers, &vk(b"writer"), b"room/x"));
        assert_eq!(
            targets,
            vec![pid(b"alice")],
            "peer with two matching filters should appear exactly once"
        );
    }
}
