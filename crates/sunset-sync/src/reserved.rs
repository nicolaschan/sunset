//! Reserved name constants for sunset-sync metadata.
//!
//! These name prefixes are reserved by *convention*. Application-layer code
//! (sunset-core, downstream consumers) does not write under these names,
//! so sunset-sync's interpretation of those entries isn't ambiguous. The
//! convention isn't enforced by the store — the store just verifies
//! signatures, and a peer with a valid signing key could in principle sign
//! an entry under any name. Defense against deliberately hostile values is
//! a separate concern handled by the trust filter.

/// Subscription filter entries — `(local_pubkey, "_sunset-sync/subscribe")`
/// stores a postcard-encoded `Filter` describing what events the peer wants.
pub const SUBSCRIBE_NAME: &[u8] = b"_sunset-sync/subscribe";

/// Optional liveness/health summaries (not used in v1).
#[allow(dead_code)]
pub const PEER_HEALTH_NAME: &[u8] = b"_sunset-sync/peer-health";

/// True if `name` is reserved for sunset-sync internal use.
pub fn is_reserved(name: &[u8]) -> bool {
    name.starts_with(b"_sunset-sync/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_name_is_reserved() {
        assert!(is_reserved(SUBSCRIBE_NAME));
    }

    #[test]
    fn application_names_are_not_reserved() {
        assert!(!is_reserved(b"chat/room/123"));
        assert!(!is_reserved(b"identity/alice"));
    }
}
