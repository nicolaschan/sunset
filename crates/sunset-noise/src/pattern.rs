//! Frozen Noise pattern — part of the v1 wire format.

pub const NOISE_PATTERN: &str = "Noise_IK_25519_XChaChaPoly_BLAKE2b";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pattern_is_pinned() {
        assert_eq!(NOISE_PATTERN, "Noise_IK_25519_XChaChaPoly_BLAKE2b");
    }
}
