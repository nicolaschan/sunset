//! Per-peer identity used by the Noise handshake.
//!
//! sunset-core's `Identity` implements `NoiseIdentity` (downstream),
//! so this crate doesn't need to depend on sunset-core.

use sha2::{Digest, Sha512};
use zeroize::Zeroizing;

/// Identity capability the Noise wrapper needs from any host:
/// the public Ed25519 key (the on-the-wire identity) and the secret
/// seed used to derive the X25519 static secret for ECDH during the
/// handshake.
pub trait NoiseIdentity: Send + Sync {
    /// The Ed25519 verifying key — the peer's identity.
    fn ed25519_public(&self) -> [u8; 32];

    /// 32-byte secret seed for the matching Ed25519 keypair. The Noise
    /// layer derives the X25519 static secret from this.
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]>;
}

/// Standard Ed25519 → X25519 static-secret derivation.
///
/// Per RFC 7748 § 5 + Signal's well-documented practice: hash the Ed25519
/// secret seed with SHA-512, take the first 32 bytes, clamp them per
/// X25519's clamping rules.
pub fn ed25519_seed_to_x25519_secret(seed: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let mut hasher = Sha512::new();
    hasher.update(seed);
    let h = hasher.finalize();
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&h[..32]);
    out[0] &= 248;
    out[31] &= 127;
    out[31] |= 64;
    out
}

/// Convert an Ed25519 public verifying key to its corresponding X25519
/// public key via the Edwards-to-Montgomery point map.
pub fn ed25519_public_to_x25519(ed_pub: &[u8; 32]) -> Result<[u8; 32], crate::error::Error> {
    use curve25519_dalek::edwards::CompressedEdwardsY;
    let edwards = CompressedEdwardsY::from_slice(ed_pub)
        .map_err(|e| crate::error::Error::Snow(format!("ed25519 pub parse: {:?}", e)))?
        .decompress()
        .ok_or_else(|| crate::error::Error::Snow("ed25519 pub decompress".into()))?;
    Ok(edwards.to_montgomery().to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frozen vector: ed25519_seed_to_x25519_secret of a fixed seed.
    /// If this changes, every NoiseTransport handshake derives a different
    /// X25519 key and previously-deployed peers won't authenticate.
    #[test]
    fn x25519_secret_frozen_vector() {
        let seed = [7u8; 32];
        let x = ed25519_seed_to_x25519_secret(&seed);
        assert_eq!(
            hex::encode(*x),
            "28ad39fefd7fa3e200a9c626eef599e61a2d055c48a8288a4e7e4c4bca392878",
            "If this fails, Ed25519→X25519 derivation has drifted — DO NOT update without a wire-format bump.",
        );
    }
}
