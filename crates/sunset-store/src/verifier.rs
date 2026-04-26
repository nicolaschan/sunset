//! The trait the host supplies so the store can verify the math on stored
//! signatures. Identity-context validation (delegation chains, room
//! membership, etc.) is the host's concern, not the verifier's — the verifier
//! only checks that `signature` is mathematically valid over the canonical
//! encoding of the rest of the entry, made with `verifying_key`.

use crate::error::Result;
use crate::types::SignedKvEntry;

pub trait SignatureVerifier: Send + Sync {
    /// Verify the structural validity of an entry's signature.
    ///
    /// Implementations must check that `entry.signature` is a mathematically
    /// valid signature over the canonical encoding of the entry's other
    /// fields, made with `entry.verifying_key`. They must NOT make any
    /// application-context judgment (delegation chains, trust, etc.).
    fn verify(&self, entry: &SignedKvEntry) -> Result<()>;
}

/// A verifier that accepts everything. Used in tests and in scenarios where
/// signature verification is performed elsewhere.
#[derive(Debug, Default, Clone, Copy)]
pub struct AcceptAllVerifier;

impl SignatureVerifier for AcceptAllVerifier {
    fn verify(&self, _entry: &SignedKvEntry) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Hash, VerifyingKey};

    fn dummy_entry() -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: VerifyingKey::new(bytes::Bytes::from_static(b"k")),
            name: bytes::Bytes::from_static(b"n"),
            value_hash: Hash::from_bytes([0u8; 32]),
            priority: 0,
            expires_at: None,
            signature: bytes::Bytes::from_static(b"s"),
        }
    }

    #[test]
    fn accept_all_verifier_accepts() {
        let v = AcceptAllVerifier;
        assert!(v.verify(&dummy_entry()).is_ok());
    }
}
