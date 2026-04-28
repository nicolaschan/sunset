//! Frozen Noise pattern — part of the v1 wire format.

pub const NOISE_PATTERN: &str = "Noise_IK_25519_XChaChaPoly_BLAKE2b";

/// Noise pattern used by signaling exchanges (e.g., WebRTC SDP/ICE).
/// KK = both statics known a priori. Full PFS via mutual ephemerals.
pub const NOISE_KK_PATTERN: &str = "Noise_KK_25519_XChaChaPoly_BLAKE2b";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pattern_is_pinned() {
        assert_eq!(NOISE_PATTERN, "Noise_IK_25519_XChaChaPoly_BLAKE2b");
    }

    #[test]
    fn kk_pattern_is_pinned() {
        assert_eq!(NOISE_KK_PATTERN, "Noise_KK_25519_XChaChaPoly_BLAKE2b");
    }
}
