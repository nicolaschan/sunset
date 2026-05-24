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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionEntry {
    /// The receiver wants `filter` from `provider`.
    Active { filter: Filter, provider: PeerId },
    /// The receiver no longer wants any data at this key.
    Withdrawn,
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
    const EXPECTED_HEX: &str = "00020c726f6f6d2f67656e6572616c0150";
}
