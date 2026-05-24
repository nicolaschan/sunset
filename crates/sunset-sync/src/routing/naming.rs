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
    let filter_bytes = postcard::to_stdvec(filter).expect("postcard filter encode is infallible");
    let filter_hash = blake3::hash(&filter_bytes);
    let filter_hex = hex::encode(filter_hash.as_bytes());
    let provider_hex = hex::encode(provider.0.as_bytes());
    Bytes::from(format!(
        "_sunset-sync/subscribe/{filter_hex}/{provider_hex}"
    ))
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
        assert_eq!(filter_hex.len(), 64); // blake3 = 32 bytes = 64 hex chars
        assert_eq!(provider_hex, hex::encode(b"provider-1"));
    }

    #[test]
    fn reserved_constants_are_under_sunset_sync_prefix() {
        assert!(LINKS_NAME.starts_with(b"_sunset-sync/"));
        assert!(PROVIDER_TICK_NAME.starts_with(b"_sunset-sync/"));
        assert!(SUBSCRIBE_PREFIX.starts_with(b"_sunset-sync/"));
    }
}
