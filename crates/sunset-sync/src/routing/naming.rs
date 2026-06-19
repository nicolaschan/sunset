//! Routing-layer reserved-name constants and the deterministic encoder
//! for per-(filter, provider) subscription entry names.

use bytes::Bytes;
use sunset_store::Filter;

use crate::types::PeerId;

/// Reserved name for self-published link-state advertisements
/// (one entry per peer at `(self_pubkey, LINKS_NAME)`).
pub const LINKS_NAME: &[u8] = b"_sunset-sync/links";

/// Reserved name for the monotonic provider-tick liveness beacon
/// (one entry per peer at `(self_pubkey, PROVIDER_TICK_NAME)`).
pub const PROVIDER_TICK_NAME: &[u8] = b"_sunset-sync/provider-tick";

/// Common prefix of every per-(filter, provider) subscription entry name.
/// Useful as a filter prefix when subscribing to the control plane.
pub const SUBSCRIBE_PREFIX: &[u8] = b"_sunset-sync/subscribe/";

/// Build the entry name for a `(filter, provider)` subscription.
///
/// Format: `_sunset-sync/subscribe/<blake3(postcard(filter))_hex>/<provider_pubkey_hex>`.
///
/// Re-publishing the same `(filter, provider)` always lands at the same
/// key, so LWW just refreshes the TTL. Distinct pairs land at distinct
/// keys, so multiple providers per filter (e.g. during failover) coexist.
///
/// Hex (not raw bytes) so the resulting name is utf-8, slash-safe, and
/// human-debuggable. The 2× size cost is negligible at the wire level
/// and pays for cheap grepping of live entries during incident response.
pub fn subscription_name(filter: &Filter, provider: &PeerId) -> Bytes {
    let filter_hex = hex::encode(crate::routing::filter_hash(filter));
    let provider_hex = hex::encode(provider.0.as_bytes());
    let prefix = std::str::from_utf8(SUBSCRIBE_PREFIX).expect("SUBSCRIBE_PREFIX is ASCII");
    Bytes::from(format!("{prefix}{filter_hex}/{provider_hex}"))
}

/// True if `name` is one of the per-(filter, provider) subscription entry
/// names produced by `subscription_name`.
pub fn is_subscription_name(name: &[u8]) -> bool {
    name.starts_with(SUBSCRIBE_PREFIX)
}

/// Extract the provider `PeerId` from a `_sunset-sync/subscribe/<hex>/<hex>`
/// entry name. Returns None if the name doesn't have the expected shape.
/// Inverse of the provider half of [`subscription_name`].
///
/// The provider lives in the entry *name*, not in the
/// [`crate::routing::SubscriptionEntry::Withdrawn`] value (which is a unit
/// variant), so this is how a `Withdrawn` is scoped to the provider it
/// targets — the symmetric counterpart of the `provider` field an `Active`
/// carries. An engine must only act on a `Withdrawn` that names *it* as the
/// provider, exactly as it only arms an `Active` that does.
pub fn decode_provider_from_name(name: &[u8]) -> Option<PeerId> {
    let rest = name.strip_prefix(SUBSCRIBE_PREFIX)?;
    let rest = std::str::from_utf8(rest).ok()?;
    let (_hash_hex, provider_hex) = rest.split_once('/')?;
    let bytes = hex::decode(provider_hex).ok()?;
    Some(PeerId(sunset_store::VerifyingKey::new(
        Bytes::copy_from_slice(&bytes),
    )))
}

/// Extract the filter-hash component from a `_sunset-sync/subscribe/<hex>/<hex>`
/// entry name. Returns None if the name doesn't have the expected shape.
/// Inverse of `subscription_name`.
pub fn decode_filter_hash_from_name(name: &[u8]) -> Option<crate::routing::FilterHash> {
    let rest = name.strip_prefix(SUBSCRIBE_PREFIX)?;
    let rest = std::str::from_utf8(rest).ok()?;
    let (hash_hex, _) = rest.split_once('/')?;
    if hash_hex.len() != crate::routing::FILTER_HASH_HEX_LEN {
        return None;
    }
    let mut out = [0u8; std::mem::size_of::<crate::routing::FilterHash>()];
    hex::decode_to_slice(hash_hex, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunset_store::VerifyingKey;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    fn pid(seed: &[u8]) -> PeerId {
        PeerId(vk(seed))
    }

    #[test]
    fn same_pair_produces_same_name() {
        let f = Filter::Namespace(Bytes::from_static(b"room/general"));
        let p = pid(b"provider-1");
        assert_eq!(subscription_name(&f, &p), subscription_name(&f, &p));
    }

    #[test]
    fn different_filters_produce_different_names() {
        let p = pid(b"provider-1");
        let f1 = Filter::Namespace(Bytes::from_static(b"room/general"));
        let f2 = Filter::Namespace(Bytes::from_static(b"room/other"));
        assert_ne!(subscription_name(&f1, &p), subscription_name(&f2, &p));
    }

    #[test]
    fn different_providers_produce_different_names() {
        let f = Filter::Namespace(Bytes::from_static(b"room/general"));
        let p1 = pid(b"provider-1");
        let p2 = pid(b"provider-2");
        assert_ne!(subscription_name(&f, &p1), subscription_name(&f, &p2));
    }

    #[test]
    fn name_has_expected_prefix_and_shape() {
        let f = Filter::Specific(vk(b"writer"), Bytes::from_static(b"k"));
        let p = pid(b"provider-1");
        let name = subscription_name(&f, &p);
        let s = std::str::from_utf8(&name).expect("name is utf-8");
        assert!(s.starts_with("_sunset-sync/subscribe/"));
        // /<filter-hash-hex 64 chars>/<provider-hex>
        let rest = &s["_sunset-sync/subscribe/".len()..];
        let mut parts = rest.split('/');
        let filter_hex = parts.next().unwrap();
        let provider_hex = parts.next().unwrap();
        assert!(parts.next().is_none());
        // Literal 64 (not crate::routing::FILTER_HASH_HEX_LEN): deliberate
        // pin so a regression that changes FILTER_HASH_HEX_LEN behind our
        // back (e.g. swapping the underlying hash to something other than
        // blake3-256) trips this assertion instead of silently passing.
        assert_eq!(filter_hex.len(), 64); // blake3 = 32 bytes = 64 hex chars
        assert_eq!(provider_hex, hex::encode(b"provider-1"));
    }

    #[test]
    fn subscription_name_decode_round_trip() {
        let f = Filter::Namespace(Bytes::from_static(b"room/x"));
        let p = pid(b"provider");
        let name = subscription_name(&f, &p);
        let hash = decode_filter_hash_from_name(&name).expect("decode");
        assert_eq!(hash, crate::routing::filter_hash(&f));
    }

    #[test]
    fn decode_provider_from_name_round_trip() {
        let f = Filter::Namespace(Bytes::from_static(b"room/x"));
        for p in [pid(b"provider-1"), pid(b"a-much-longer-provider-key-32by")] {
            let name = subscription_name(&f, &p);
            assert_eq!(decode_provider_from_name(&name), Some(p));
        }
    }

    /// The decoded provider distinguishes two subscriptions to the same
    /// filter via different providers — the property the provider-scoped
    /// `Withdrawn` handling relies on.
    #[test]
    fn decode_provider_from_name_distinguishes_providers() {
        let f = Filter::Namespace(Bytes::from_static(b"room/x"));
        let p1 = pid(b"provider-1");
        let p2 = pid(b"provider-2");
        assert_eq!(
            decode_provider_from_name(&subscription_name(&f, &p1)),
            Some(p1)
        );
        assert_eq!(
            decode_provider_from_name(&subscription_name(&f, &p2)),
            Some(p2)
        );
    }

    #[test]
    fn decode_provider_from_name_rejects_non_subscription_names() {
        assert_eq!(decode_provider_from_name(b""), None);
        assert_eq!(decode_provider_from_name(b"chat/room/general"), None);
        assert_eq!(decode_provider_from_name(LINKS_NAME), None);
        // Missing the `/provider` segment.
        let mut name = Vec::from(SUBSCRIBE_PREFIX);
        name.extend_from_slice(&b"a".repeat(64));
        assert_eq!(decode_provider_from_name(&name), None);
    }

    #[test]
    fn is_subscription_name_round_trip() {
        let f = Filter::Namespace(Bytes::from_static(b"room/x"));
        let p = pid(b"provider");
        let name = subscription_name(&f, &p);
        assert!(is_subscription_name(&name));
    }

    #[test]
    fn is_subscription_name_rejects_other_reserved_names() {
        assert!(!is_subscription_name(LINKS_NAME));
        assert!(!is_subscription_name(PROVIDER_TICK_NAME));
    }

    #[test]
    fn is_subscription_name_rejects_application_names() {
        assert!(!is_subscription_name(b"chat/room/general"));
        assert!(!is_subscription_name(b""));
    }

    #[test]
    fn reserved_constants_are_under_sunset_sync_prefix() {
        use crate::reserved::RESERVED_PREFIX;
        assert!(LINKS_NAME.starts_with(RESERVED_PREFIX));
        assert!(PROVIDER_TICK_NAME.starts_with(RESERVED_PREFIX));
        assert!(SUBSCRIBE_PREFIX.starts_with(RESERVED_PREFIX));
    }

    /// Wire-format pin for `SUBSCRIBE_PREFIX`. Subscription entry names
    /// produced by every sunset-sync deploy embed this byte string;
    /// changing it without coordinated rollout silently splits the
    /// network into peers whose `is_subscription_name` / `decode_*`
    /// reject each other's entries.
    #[test]
    fn subscribe_prefix_wire_format_pin() {
        assert_eq!(SUBSCRIBE_PREFIX, b"_sunset-sync/subscribe/");
        assert_eq!(LINKS_NAME, b"_sunset-sync/links");
        assert_eq!(PROVIDER_TICK_NAME, b"_sunset-sync/provider-tick");
    }

    /// Frozen hex vector for `filter_hash`. The wire format is
    /// `blake3(postcard(filter))`; this test fails if either the hash
    /// function or the postcard encoding of `Filter` changes. A wire-
    /// format break here means any in-flight `_sunset-sync/subscribe/*`
    /// entry from an older peer becomes un-decodable to a newer peer
    /// (and vice-versa) — the routing tick republishes under a new
    /// hash but the old peer's interest map still keys on the old one.
    ///
    /// Mirrors the `ContentBlock::hash` hex pin in
    /// `sunset-store/src/types.rs` for the same reason.
    #[test]
    fn filter_hash_namespace_room_general_hex_vector() {
        let f = Filter::Namespace(Bytes::from_static(b"room/general"));
        let got = hex::encode(crate::routing::filter_hash(&f));
        assert_eq!(
            got, "dc56a60a0a4023f23916e4e5ba861f6b42152786ddab9280291b0706187843b6",
            "filter_hash(Namespace(\"room/general\")) wire format changed — \
             this is a breaking change to the SUBSCRIBE_PREFIX entry namespace"
        );
    }

    // -- decode_filter_hash_from_name rejection paths --

    #[test]
    fn decode_filter_hash_from_name_rejects_empty_name() {
        assert_eq!(decode_filter_hash_from_name(b""), None);
    }

    #[test]
    fn decode_filter_hash_from_name_rejects_unrelated_name() {
        assert_eq!(decode_filter_hash_from_name(b"chat/room/general"), None);
    }

    #[test]
    fn decode_filter_hash_from_name_rejects_wrong_reserved_prefix() {
        assert_eq!(decode_filter_hash_from_name(LINKS_NAME), None);
        assert_eq!(decode_filter_hash_from_name(PROVIDER_TICK_NAME), None);
    }

    #[test]
    fn decode_filter_hash_from_name_rejects_short_hash() {
        let mut name = Vec::from(SUBSCRIBE_PREFIX);
        name.extend_from_slice(&b"00".repeat(16)); // 32 hex chars = 16 bytes
        name.extend_from_slice(b"/abcd");
        assert_eq!(decode_filter_hash_from_name(&name), None);
    }

    #[test]
    fn decode_filter_hash_from_name_rejects_long_hash() {
        let mut name = Vec::from(SUBSCRIBE_PREFIX);
        name.extend_from_slice(&b"00".repeat(64));
        name.extend_from_slice(b"/abcd");
        assert_eq!(decode_filter_hash_from_name(&name), None);
    }

    #[test]
    fn decode_filter_hash_from_name_rejects_non_hex_hash() {
        let mut name = Vec::from(SUBSCRIBE_PREFIX);
        name.extend_from_slice(&b"z".repeat(64));
        name.extend_from_slice(b"/abcd");
        assert_eq!(decode_filter_hash_from_name(&name), None);
    }

    #[test]
    fn decode_filter_hash_from_name_rejects_missing_provider_segment() {
        let mut name = Vec::from(SUBSCRIBE_PREFIX);
        name.extend_from_slice(&b"a".repeat(64));
        assert_eq!(decode_filter_hash_from_name(&name), None);
    }

    #[test]
    fn decode_filter_hash_from_name_rejects_non_utf8_after_prefix() {
        let mut name = Vec::from(SUBSCRIBE_PREFIX);
        name.extend_from_slice(&[0xff, 0xfe, 0xfd]);
        assert_eq!(decode_filter_hash_from_name(&name), None);
    }
}
