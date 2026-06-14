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

use crate::{Hash, SignedDatagram, SignedKvEntry, VerifyingKey};

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

/// Canonical bytes covered by `SignedDatagram::signature`. Postcard
/// encoding of `(verifying_key, name, payload, seq)`. Frozen by the
/// `datagram_payload_frozen_vector` test below.
pub fn datagram_signing_payload(d: &SignedDatagram) -> Vec<u8> {
    #[derive(Serialize)]
    struct UnsignedDatagramRef<'a> {
        verifying_key: &'a VerifyingKey,
        name: &'a Bytes,
        payload: &'a Bytes,
        seq: &'a u64,
    }
    let unsigned = UnsignedDatagramRef {
        verifying_key: &d.verifying_key,
        name: &d.name,
        payload: &d.payload,
        seq: &d.seq,
    };
    postcard::to_stdvec(&unsigned).expect("postcard encoding of UnsignedDatagramRef is infallible")
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

    fn sample_datagram() -> SignedDatagram {
        SignedDatagram {
            verifying_key: VerifyingKey::new(Bytes::from_static(
                b"sample-vk-32-bytes-aaaaaaaaaaaaa",
            )),
            name: Bytes::from_static(b"room/general/voice/alice/0042"),
            payload: Bytes::from_static(b"opaque-payload-bytes"),
            seq: 0,
            signature: Bytes::from_static(b"ignored"),
        }
    }

    #[test]
    fn datagram_payload_excludes_signature_field() {
        // Regenerated 2026-06-01: deliberate pre-1.0 ephemeral wire bump (added SignedDatagram.seq).
        let mut a = sample_datagram();
        let mut b = sample_datagram();
        b.signature = Bytes::from_static(b"completely different");
        assert_eq!(datagram_signing_payload(&a), datagram_signing_payload(&b));
        a.payload = Bytes::from_static(b"different payload");
        assert_ne!(datagram_signing_payload(&a), datagram_signing_payload(&b));
        // seq is a covered field: changing it changes the payload.
        let mut c = sample_datagram();
        c.seq = 1;
        assert_ne!(
            datagram_signing_payload(&sample_datagram()),
            datagram_signing_payload(&c)
        );
    }

    #[test]
    fn datagram_signing_payload_covers_seq() {
        let mk = |seq| SignedDatagram {
            verifying_key: VerifyingKey::new(Bytes::from_static(b"alice")),
            name: Bytes::from_static(b"voice/r/alice"),
            payload: Bytes::from_static(b"hi"),
            seq,
            signature: Bytes::new(),
        };
        assert_ne!(
            datagram_signing_payload(&mk(7)),
            datagram_signing_payload(&mk(8))
        );
    }

    /// Frozen wire-format vector. If this hex changes, every existing
    /// SignedDatagram signature in the wild becomes invalid — bump the
    /// wire-format version before updating the constant.
    #[test]
    fn datagram_payload_frozen_vector() {
        let d = sample_datagram();
        let payload = datagram_signing_payload(&d);
        let digest = blake3::hash(&payload);
        // Regenerated 2026-06-01: deliberate pre-1.0 ephemeral wire bump (added SignedDatagram.seq).
        assert_eq!(
            digest.to_hex().as_str(),
            "104a438b82af1f671aaf554105793959a4fb3288bafb776288098c9910ecbe70",
            "If this fails the canonical signing encoding has drifted — DO NOT update this hex without bumping the wire-format version.",
        );
    }
}
