//! On-the-wire envelopes for an encrypted chat message.
//!
//! Wire layering (top is innermost — the AEAD plaintext):
//!
//!   SignedMessage   { inner_signature, sent_at_ms, channel, body }
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
//! The `channel` rides inside the AEAD plaintext (so the relay never sees
//! it) and is covered by the inner signature (so a peer can't tamper with
//! it). See `docs/superpowers/specs/2026-05-04-channels-within-rooms-design.md`.

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

/// Channel label carried by every chat message (Text, Receipt, Reaction).
/// Lives inside the AEAD plaintext and is covered by the inner Ed25519
/// signature, so the relay sees only `<room_fp>/msg/<hash>` — never the
/// channel. Validated to be 1..=64 bytes UTF-8, no ASCII control
/// characters, not all-whitespace.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChannelLabel(String);

/// The default channel name; every room implicitly has it.
pub const DEFAULT_CHANNEL: &str = "general";

impl ChannelLabel {
    pub fn try_new(s: impl Into<String>) -> crate::Result<Self> {
        let s = s.into();
        if s.is_empty() {
            return Err(crate::Error::BadChannel("empty".to_owned()));
        }
        if s.len() > 64 {
            return Err(crate::Error::BadChannel(format!(
                "too long ({} bytes)",
                s.len()
            )));
        }
        if s.chars().any(|c| c.is_control()) {
            return Err(crate::Error::BadChannel(
                "contains control character".to_owned(),
            ));
        }
        if s.chars().all(char::is_whitespace) {
            return Err(crate::Error::BadChannel("all whitespace".to_owned()));
        }
        Ok(Self(s))
    }

    pub fn default_general() -> Self {
        // Constructed by hand so we never panic at construction time
        // for the default constant.
        Self(DEFAULT_CHANNEL.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChannelLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ChannelLabel {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for ChannelLabel {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::try_new(raw).map_err(serde::de::Error::custom)
    }
}

/// Plaintext-inside-the-AEAD. The author's Ed25519 signature over
/// `InnerSigPayload` is `inner_signature`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMessage {
    pub inner_signature: Signature,
    pub sent_at_ms: u64,
    pub channel: ChannelLabel,
    pub body: MessageBody,
}

/// What the inner Ed25519 signature covers. Bound to room + epoch so a valid
/// signature in one room/epoch cannot be replayed into another.
#[derive(Serialize)]
pub struct InnerSigPayload<'a> {
    pub room_fingerprint: &'a [u8; 32],
    pub epoch_id: u64,
    pub sent_at_ms: u64,
    pub channel: &'a ChannelLabel,
    pub body: &'a MessageBody,
}

pub fn inner_sig_payload_bytes(
    room_fp: &RoomFingerprint,
    epoch_id: u64,
    sent_at_ms: u64,
    channel: &ChannelLabel,
    body: &MessageBody,
) -> Vec<u8> {
    postcard::to_stdvec(&InnerSigPayload {
        room_fingerprint: room_fp.as_bytes(),
        epoch_id,
        sent_at_ms,
        channel,
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

/// Add or Remove for a `MessageBody::Reaction` event. The application
/// layer folds a stream of these per `(author, target, emoji)` to derive
/// "is this author currently reacting with this emoji on this target?".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReactionAction {
    Add,
    Remove,
}

/// Discriminator for the inner plaintext of a chat-room entry. All
/// variants ride the same `<room_fp>/msg/<value_hash>` namespace and
/// share the AEAD envelope; only the plaintext shape differs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageBody {
    /// A user-authored chat message.
    Text(String),
    /// An acknowledgement that the author of this entry decoded the
    /// referenced `Text` message. The author of the receipt is the
    /// receiver of the original message.
    Receipt { for_value_hash: sunset_store::Hash },
    /// An emoji reaction attached to the referenced message. The
    /// author of the entry is the reactor; `for_value_hash` is the
    /// `value_hash` of the message being reacted to. Per
    /// `(author, for_value_hash, emoji)`, the application folds events
    /// LWW by `(sent_at_ms, value_hash)` to derive current state.
    Reaction {
        for_value_hash: sunset_store::Hash,
        emoji: String,
        action: ReactionAction,
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
            channel: ChannelLabel::default_general(),
            body: MessageBody::Text("hello".to_owned()),
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
        let g = ChannelLabel::default_general();
        let a = inner_sig_payload_bytes(&fp, 0, 100, &g, &MessageBody::Text("hi".to_owned()));
        let b = inner_sig_payload_bytes(&fp, 1, 100, &g, &MessageBody::Text("hi".to_owned())); // epoch differs
        let c = inner_sig_payload_bytes(&fp, 0, 101, &g, &MessageBody::Text("hi".to_owned())); // sent_at differs
        let d = inner_sig_payload_bytes(&fp, 0, 100, &g, &MessageBody::Text("hello".to_owned())); // body differs
        let e = inner_sig_payload_bytes(
            &RoomFingerprint([2u8; 32]),
            0,
            100,
            &g,
            &MessageBody::Text("hi".to_owned()),
        ); // room differs
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

    #[test]
    fn message_body_text_postcard_hex_pin() {
        // Pin the postcard encoding so accidental drift breaks the build.
        // postcard encodes: enum-tag (varint 0) + len-prefixed UTF-8 string.
        let body = MessageBody::Text("hi".to_owned());
        let bytes = postcard::to_stdvec(&body).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        // 00 = Text variant tag; 02 = string length (varint); 6869 = "hi".
        assert_eq!(hex, "00026869", "MessageBody::Text wire encoding drifted");
    }

    #[test]
    fn message_body_receipt_postcard_hex_pin() {
        // Receipt's payload is a 32-byte hash; pin a known input.
        let h: sunset_store::Hash = blake3::hash(b"x").into();
        let body = MessageBody::Receipt { for_value_hash: h };
        let bytes = postcard::to_stdvec(&body).unwrap();
        // 01 = Receipt variant tag; then 32 raw bytes of the hash.
        assert_eq!(bytes[0], 0x01, "MessageBody::Receipt variant tag drifted");
        assert_eq!(
            bytes.len(),
            1 + 32,
            "Receipt should encode as tag + 32 bytes"
        );
        let hash_hex: String = bytes[1..].iter().map(|b| format!("{b:02x}")).collect();
        let expected_hash: String = h.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hash_hex, expected_hash);
    }

    #[test]
    fn message_body_reaction_add_postcard_hex_pin() {
        let h: sunset_store::Hash = blake3::hash(b"x").into();
        let body = MessageBody::Reaction {
            for_value_hash: h,
            emoji: "👍".to_owned(),
            action: ReactionAction::Add,
        };
        let bytes = postcard::to_stdvec(&body).unwrap();
        // 02 = Reaction variant tag (third variant after Text=00, Receipt=01).
        assert_eq!(bytes[0], 0x02, "MessageBody::Reaction variant tag drifted");
        // Then 32 hash bytes; then varint emoji-len (4 for 👍 = F0 9F 91 8D);
        // then 4 emoji bytes; then enum-tag for ReactionAction (00 for Add).
        assert_eq!(
            bytes.len(),
            1 + 32 + 1 + 4 + 1,
            "Reaction Add encoding drifted: tag + hash + len + emoji + action"
        );
        assert_eq!(bytes[1 + 32], 0x04, "emoji length varint drifted");
        assert_eq!(&bytes[1 + 32 + 1..1 + 32 + 1 + 4], "👍".as_bytes());
        assert_eq!(
            bytes[1 + 32 + 1 + 4],
            0x00,
            "ReactionAction::Add tag drifted"
        );
    }

    #[test]
    fn message_body_reaction_remove_postcard_hex_pin() {
        let h: sunset_store::Hash = blake3::hash(b"x").into();
        let body = MessageBody::Reaction {
            for_value_hash: h,
            emoji: "❤".to_owned(), // 3-byte emoji, no VS-16
            action: ReactionAction::Remove,
        };
        let bytes = postcard::to_stdvec(&body).unwrap();
        assert_eq!(bytes[0], 0x02);
        assert_eq!(
            *bytes.last().unwrap(),
            0x01,
            "ReactionAction::Remove tag drifted"
        );
    }

    #[test]
    fn message_body_reaction_roundtrips_via_postcard() {
        let h: sunset_store::Hash = blake3::hash(b"target").into();
        let body = MessageBody::Reaction {
            for_value_hash: h,
            emoji: "🎉".to_owned(),
            action: ReactionAction::Add,
        };
        let bytes = postcard::to_stdvec(&body).unwrap();
        let decoded: MessageBody = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn message_body_text_postcard_hex_pin_unchanged() {
        // Confirms that adding the Reaction variant did not reorder existing
        // tag values: Text must still encode to 00026869.
        let body = MessageBody::Text("hi".to_owned());
        let bytes = postcard::to_stdvec(&body).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "00026869");
    }

    #[test]
    fn channel_label_accepts_default_general() {
        let c = ChannelLabel::try_new("general").unwrap();
        assert_eq!(c.as_str(), "general");
    }

    #[test]
    fn channel_label_accepts_unicode_and_spaces() {
        assert!(ChannelLabel::try_new("café 🌅").is_ok());
    }

    #[test]
    fn channel_label_rejects_empty() {
        assert!(matches!(
            ChannelLabel::try_new(""),
            Err(crate::error::Error::BadChannel(_))
        ));
    }

    #[test]
    fn channel_label_rejects_all_whitespace() {
        assert!(matches!(
            ChannelLabel::try_new("   \t  "),
            Err(crate::error::Error::BadChannel(_))
        ));
    }

    #[test]
    fn channel_label_rejects_control_chars() {
        assert!(matches!(
            ChannelLabel::try_new("hi\nthere"),
            Err(crate::error::Error::BadChannel(_))
        ));
        assert!(matches!(
            ChannelLabel::try_new("nul\0byte"),
            Err(crate::error::Error::BadChannel(_))
        ));
    }

    #[test]
    fn channel_label_rejects_over_64_bytes() {
        let s = "a".repeat(65);
        assert!(matches!(
            ChannelLabel::try_new(&s),
            Err(crate::error::Error::BadChannel(_))
        ));
    }

    #[test]
    fn channel_label_accepts_max_64_bytes() {
        let s = "a".repeat(64);
        assert!(ChannelLabel::try_new(&s).is_ok());
    }

    #[test]
    fn channel_label_default_general_constructor() {
        let c = ChannelLabel::default_general();
        assert_eq!(c.as_str(), "general");
    }

    #[test]
    fn channel_label_postcard_roundtrip() {
        let c = ChannelLabel::try_new("links").unwrap();
        let bytes = postcard::to_stdvec(&c).unwrap();
        let back: ChannelLabel = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn channel_label_postcard_decode_validates() {
        // Encode an empty string at the wire layer. The asymmetry below
        // proves the validator was invoked at decode time: postcard
        // happily decodes the bytes as a `String`, but `ChannelLabel`
        // must reject them. If a future change drops the custom
        // Deserialize impl in favour of `#[derive(Deserialize)]`, both
        // assertions would pass-through and an empty-string label would
        // silently round-trip — surfacing as a bug only at some
        // downstream consumer.
        let bad = postcard::to_stdvec(&"".to_owned()).unwrap();
        assert!(
            postcard::from_bytes::<String>(&bad).is_ok(),
            "wire format itself must be valid for String"
        );
        assert!(
            postcard::from_bytes::<ChannelLabel>(&bad).is_err(),
            "ChannelLabel must reject empty at decode (validator must run)"
        );
    }

    #[test]
    fn signed_message_postcard_hex_pin() {
        // Pin the SignedMessage wire format so accidental drift breaks the build.
        // postcard layout: signature(varint-len 0x40 + 64 raw bytes — Signature
        // serializes via `serialize_bytes` which postcard length-prefixes),
        // sent_at_ms(varint), channel(len-varint + utf8), body(...).
        let m = SignedMessage {
            inner_signature: Signature([0u8; 64]),
            sent_at_ms: 1,
            channel: ChannelLabel::default_general(),
            body: MessageBody::Text("hi".to_owned()),
        };
        let bytes = postcard::to_stdvec(&m).unwrap();
        // [0]: 0x40 = signature length varint (64 bytes).
        assert_eq!(bytes[0], 0x40);
        // [1..65]: zeroed signature bytes.
        assert!(bytes[1..65].iter().all(|b| *b == 0));
        // [65]: 0x01 = sent_at_ms varint(1).
        assert_eq!(bytes[65], 0x01);
        // [66]: 0x07 = channel length varint, "general" = 7 bytes.
        assert_eq!(bytes[66], 0x07);
        assert_eq!(&bytes[67..74], b"general");
        // [74..]: MessageBody::Text("hi") tail = 00 02 68 69.
        assert_eq!(&bytes[74..], &[0x00, 0x02, 0x68, 0x69]);
    }

    #[test]
    fn signed_message_round_trips_channel() {
        let m = SignedMessage {
            inner_signature: Signature([7u8; 64]),
            sent_at_ms: 42,
            channel: ChannelLabel::try_new("links").unwrap(),
            body: MessageBody::Text("hello".to_owned()),
        };
        let bytes = postcard::to_stdvec(&m).unwrap();
        let back: SignedMessage = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.channel.as_str(), "links");
    }

    #[test]
    fn inner_sig_payload_changes_with_channel() {
        let fp = RoomFingerprint([1u8; 32]);
        let body = MessageBody::Text("hi".to_owned());
        let general = ChannelLabel::default_general();
        let links = ChannelLabel::try_new("links").unwrap();
        let a = inner_sig_payload_bytes(&fp, 0, 100, &general, &body);
        let b = inner_sig_payload_bytes(&fp, 0, 100, &links, &body);
        assert_ne!(
            a, b,
            "channel must be domain-separated by the inner signature"
        );
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
