//! On-the-wire envelopes for an encrypted chat message.
//!
//! Wire layering (top is innermost — the AEAD plaintext):
//!
//!   SignedMessage   { inner_signature, sent_at_ms, body }
//!         |  postcard
//!         v
//!   <plaintext bytes>
//!         |  XChaCha20-Poly1305 with K_msg + AAD
//!         v
//!   EncryptedMessage { epoch_id, nonce, ciphertext }
//!         |  postcard
//!         v
//!   ContentBlock.data
//!
//! The `inner_signature` covers the canonical `InnerSigPayload` (defined
//! below) and is verified by recipients after AEAD-decrypt — this is the
//! authentication property from the crypto spec's third non-negotiable.

use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::crypto::room::RoomFingerprint;

/// Newtype wrapper for 64-byte signatures to support serde serialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature([u8; 64]);

impl Signature {
    pub fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl From<[u8; 64]> for Signature {
    fn from(bytes: [u8; 64]) -> Self {
        Signature(bytes)
    }
}

impl Serialize for Signature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Signature;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a 64-byte signature")
            }

            fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value.len() != 64 {
                    return Err(E::invalid_length(value.len(), &"64"));
                }
                let mut arr = [0u8; 64];
                arr.copy_from_slice(value);
                Ok(Signature(arr))
            }
        }
        deserializer.deserialize_bytes(Visitor)
    }
}

/// Plaintext-inside-the-AEAD. The author's Ed25519 signature over
/// `InnerSigPayload` is `inner_signature`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMessage {
    pub inner_signature: Signature,
    pub sent_at_ms: u64,
    pub body: String,
}

/// What the inner Ed25519 signature covers. Bound to room + epoch so a valid
/// signature in one room/epoch cannot be replayed into another.
#[derive(Serialize)]
pub struct InnerSigPayload<'a> {
    pub room_fingerprint: &'a [u8; 32],
    pub epoch_id: u64,
    pub sent_at_ms: u64,
    pub body: &'a str,
}

pub fn inner_sig_payload_bytes(
    room_fp: &RoomFingerprint,
    epoch_id: u64,
    sent_at_ms: u64,
    body: &str,
) -> Vec<u8> {
    postcard::to_stdvec(&InnerSigPayload {
        room_fingerprint: room_fp.as_bytes(),
        epoch_id,
        sent_at_ms,
        body,
    })
    .expect("postcard encoding of InnerSigPayload is infallible for in-memory inputs")
}

/// What lives inside `ContentBlock.data` for a chat message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedMessage {
    pub epoch_id: u64,
    pub nonce: [u8; 24],
    pub ciphertext: Bytes,
}

impl EncryptedMessage {
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard encoding of EncryptedMessage is infallible")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// Discriminator for the inner plaintext of a chat-room entry. Both
/// variants ride the same `<room_fp>/msg/<value_hash>` namespace and
/// share the AEAD envelope; only the plaintext shape differs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageBody {
    /// A user-authored chat message.
    Text(String),
    /// An acknowledgement that the author of this entry decoded the
    /// referenced `Text` message. The author of the receipt is the
    /// receiver of the original message.
    Receipt {
        for_value_hash: sunset_store::Hash,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_message_postcard_roundtrip() {
        let m = SignedMessage {
            inner_signature: Signature([9u8; 64]),
            sent_at_ms: 1_700_000_000_000,
            body: "hello".into(),
        };
        let bytes = postcard::to_stdvec(&m).unwrap();
        let back: SignedMessage = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn encrypted_message_roundtrip() {
        let e = EncryptedMessage {
            epoch_id: 0,
            nonce: [3u8; 24],
            ciphertext: Bytes::from_static(b"opaque-ct"),
        };
        let bytes = e.to_bytes();
        let back = EncryptedMessage::from_bytes(&bytes).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn inner_sig_payload_changes_with_each_field() {
        let fp = RoomFingerprint([1u8; 32]);
        let a = inner_sig_payload_bytes(&fp, 0, 100, "hi");
        let b = inner_sig_payload_bytes(&fp, 1, 100, "hi"); // epoch differs
        let c = inner_sig_payload_bytes(&fp, 0, 101, "hi"); // sent_at differs
        let d = inner_sig_payload_bytes(&fp, 0, 100, "hello"); // body differs
        let e = inner_sig_payload_bytes(&RoomFingerprint([2u8; 32]), 0, 100, "hi"); // room differs
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_ne!(a, e);
    }

    #[test]
    fn message_body_text_roundtrips_via_postcard() {
        let body = MessageBody::Text("hello".to_owned());
        let bytes = postcard::to_stdvec(&body).unwrap();
        let decoded: MessageBody = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn message_body_receipt_roundtrips_via_postcard() {
        let h: sunset_store::Hash = blake3::hash(b"target message").into();
        let body = MessageBody::Receipt { for_value_hash: h };
        let bytes = postcard::to_stdvec(&body).unwrap();
        let decoded: MessageBody = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, body);
    }

    /// Frozen wire-format vector for `EncryptedMessage`. Failing means the
    /// postcard encoding has drifted — bump the version before updating.
    #[test]
    fn encrypted_message_frozen_vector() {
        let e = EncryptedMessage {
            epoch_id: 0,
            nonce: [3u8; 24],
            ciphertext: Bytes::from_static(b"opaque-ct"),
        };
        let bytes = e.to_bytes();
        let digest = blake3::hash(&bytes);
        assert_eq!(
            digest.to_hex().as_str(),
            "494ec67563f226c0c317d0c48a24184e928c91b341e4a47a59f70f82f44002eb",
            "If this fails, the EncryptedMessage wire format has drifted — DO NOT update without a wire-format bump.",
        );
    }
}
