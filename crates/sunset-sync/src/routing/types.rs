//! Wire types for the cooperative-relay routing layer.

use serde::{Deserialize, Serialize};
use sunset_store::Filter;

use crate::types::PeerId;

/// Subscription state asserted by a receiver, addressed to one provider.
///
/// Stored at `(receiver_pubkey, naming::subscription_name(filter, provider))`
/// with normal LWW/TTL semantics. `Withdrawn` is published at the same
/// key with `expires_at` ≥ the previous entry's so it propagates through
/// the network like any other update before being garbage-collected.
///
/// `Active` carries `filter` and `provider` redundantly with the entry
/// name (which is `subscription_name(filter, provider)`). Providers read
/// the value directly rather than parsing the name, and receivers can
/// reject any entry whose value disagrees with its key — so the
/// "duplication" is a single-source claim verified at the consumer, not
/// two independent writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionEntry {
    /// The receiver wants `filter` from `provider`.
    Active { filter: Filter, provider: PeerId },
    /// The receiver no longer wants any data at this key.
    Withdrawn,
}

/// Self-published gossip of the publisher's direct neighbors.
///
/// Stored at `(self_pubkey, naming::LINKS_NAME)`. Receivers read this
/// from any peer they care about as input to the candidate ranking.
/// The publisher reports its own heartbeat measurements; no other field
/// (broad-subscriber flag, load hint) is carried because both are
/// derivable from data already replicated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkState {
    pub neighbors: Vec<Neighbor>,
}

/// One row of `LinkState`: a peer the publisher is directly connected to,
/// with the publisher's most recent heartbeat-measured RTT and the
/// timestamp of the last successful exchange.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Neighbor {
    pub peer: PeerId,
    pub rtt_ms: u16,
    pub last_success_ts: u64,
}

/// Monotonic liveness beacon published by a provider.
///
/// Stored at `(self_pubkey, naming::PROVIDER_TICK_NAME)`. Receivers
/// observe arrival cadence on their subscribed path as the provider's
/// liveness signal; for active data streams (e.g. voice frames) the
/// data itself serves the same role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTick {
    pub seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    #[test]
    fn subscription_entry_active_postcard_roundtrip() {
        let entry = SubscriptionEntry::Active {
            filter: Filter::NamePrefix(Bytes::from_static(b"room/")),
            provider: PeerId(vk(b"provider-key")),
        };
        let bytes = postcard::to_stdvec(&entry).unwrap();
        let back: SubscriptionEntry = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn subscription_entry_withdrawn_postcard_roundtrip() {
        let entry = SubscriptionEntry::Withdrawn;
        let bytes = postcard::to_stdvec(&entry).unwrap();
        let back: SubscriptionEntry = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn subscription_entry_active_wire_format_pinned_v1() {
        // Pinning postcard wire format. If this test breaks, you have either:
        //   - changed the SubscriptionEntry / Filter / PeerId encoding, or
        //   - bumped a postcard semver across an incompatible change.
        // Either is a wire-format break that needs a coordinated rollout.
        let entry = SubscriptionEntry::Active {
            filter: Filter::Namespace(Bytes::from_static(b"room/general")),
            provider: PeerId(VerifyingKey::new(Bytes::from_static(b"P"))),
        };
        let bytes = postcard::to_stdvec(&entry).unwrap();
        assert_eq!(hex::encode(&bytes), EXPECTED_HEX);
    }

    // Computed from the input above; regenerate intentionally on a real wire
    // change with `cargo test ... -- --nocapture` then update.
    //
    // Decomposition (so a breaking change is easy to diagnose):
    //   00                                  SubscriptionEntry::Active (variant 0)
    //   02                                  Filter::Namespace         (variant 2)
    //     0c                                length 12                 (postcard varint)
    //     726f6f6d2f67656e6572616c          "room/general"
    //   01                                  VerifyingKey inner Bytes len 1
    //     50                                "P"
    const EXPECTED_HEX: &str = "00020c726f6f6d2f67656e6572616c0150";

    #[test]
    fn link_state_postcard_roundtrip() {
        let ls = LinkState {
            neighbors: vec![
                Neighbor {
                    peer: PeerId(vk(b"n1")),
                    rtt_ms: 12,
                    last_success_ts: 1_700_000_000,
                },
                Neighbor {
                    peer: PeerId(vk(b"n2")),
                    rtt_ms: 280,
                    last_success_ts: 1_700_000_005,
                },
            ],
        };
        let bytes = postcard::to_stdvec(&ls).unwrap();
        let back: LinkState = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(ls, back);
    }

    #[test]
    fn link_state_empty_roundtrip() {
        let ls = LinkState { neighbors: vec![] };
        let bytes = postcard::to_stdvec(&ls).unwrap();
        let back: LinkState = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(ls, back);
    }

    #[test]
    fn provider_tick_postcard_roundtrip() {
        for seq in [0u64, 1, 42, u64::MAX] {
            let t = ProviderTick { seq };
            let bytes = postcard::to_stdvec(&t).unwrap();
            let back: ProviderTick = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(t, back);
        }
    }
}
