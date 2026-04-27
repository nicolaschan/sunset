//! Canonical signing payload for `SignedKvEntry`.
//!
//! The store-layer `SignatureVerifier` contract requires implementations to
//! verify a signature over "the canonical encoding of the rest of the entry"
//! (every field except `signature`). This module pins that encoding to
//! `postcard::to_stdvec(&UnsignedEntryRef { ... })` with fields in the order
//! they appear in `SignedKvEntry`.
//!
//! The frozen test vector at the bottom of this file is what keeps the wire
//! format honest. If it ever fails, the canonical encoding has changed and
//! every signature ever produced under the old encoding becomes invalid —
//! treat that as a wire-format version bump, not a "fix the test" moment.

use bytes::Bytes;
use serde::Serialize;

use crate::{Hash, SignedKvEntry, VerifyingKey};

/// The fields of `SignedKvEntry` that are covered by the signature, in the
/// frozen canonical order.
#[derive(Serialize)]
struct UnsignedEntryRef<'a> {
    verifying_key: &'a VerifyingKey,
    name: &'a Bytes,
    value_hash: &'a Hash,
    priority: u64,
    expires_at: Option<u64>,
}

/// Build the canonical byte payload that an `Ed25519Verifier` (or any
/// future verifier) signs and verifies over.
pub fn signing_payload(entry: &SignedKvEntry) -> Vec<u8> {
    let unsigned = UnsignedEntryRef {
        verifying_key: &entry.verifying_key,
        name: &entry.name,
        value_hash: &entry.value_hash,
        priority: entry.priority,
        expires_at: entry.expires_at,
    };
    postcard::to_stdvec(&unsigned).expect("postcard encoding of UnsignedEntryRef is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: VerifyingKey::new(Bytes::from_static(
                b"sample-vk-32-bytes-aaaaaaaaaaaaa",
            )),
            name: Bytes::from_static(b"room/general/msg/abc"),
            value_hash: Hash::from_bytes([7u8; 32]),
            priority: 42,
            expires_at: Some(99),
            // signature is *not* included in the payload.
            signature: Bytes::from_static(b"ignored"),
        }
    }

    #[test]
    fn payload_excludes_signature_field() {
        let mut a = sample_entry();
        let mut b = sample_entry();
        b.signature = Bytes::from_static(b"completely different");
        assert_eq!(signing_payload(&a), signing_payload(&b));
        // Sanity: changing a covered field does change the payload.
        a.priority = 43;
        assert_ne!(signing_payload(&a), signing_payload(&b));
    }

    /// Frozen wire-format vector. If this hex changes, every existing
    /// signature in the wild becomes invalid — bump the wire-format version
    /// before updating the constant.
    #[test]
    fn payload_frozen_vector() {
        let entry = sample_entry();
        let payload = signing_payload(&entry);
        let digest = blake3::hash(&payload);
        assert_eq!(
            digest.to_hex().as_str(),
            "d15d46aa02779b076df6f8223577aead0385307e3817112c65297661af2b3094",
            "If this fails the canonical signing encoding has drifted — DO NOT update this hex without bumping the wire-format version.",
        );
    }
}
