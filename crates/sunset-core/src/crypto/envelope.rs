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

/// An image attached to a `MessageBody::Text` payload. Lives inside the
/// AEAD plaintext just like the text body. The image bytes are carried
/// base64-encoded so the same string can flow unchanged from the wasm
/// boundary into the encrypted envelope and back out as a
/// `<img src="data:...">` attribute on the receiver.
///
/// `mime_type` is the IANA media type produced by the preprocessing
/// pipeline — currently always `"image/jpeg"` for re-encoded inputs,
/// or `"image/gif"` / `"image/webp"` for formats that pass through
/// unchanged (see `sunset-image`).
///
/// The canonical sender-side constructor is [`Self::preprocess`], which
/// takes raw file bytes and runs them through
/// [`sunset_image::preprocess`] before composing the wire form. The
/// hidden [`Self::raw`] constructor exists for the receive path and
/// tests; production callers should always go through `preprocess` so
/// every client (web, future TUI / desktop / Minecraft mod) produces a
/// consistent normalised payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub mime_type: String,
    pub data_base64: String,
}

impl ImageAttachment {
    /// Preprocess raw image bytes (whatever the file picker handed us)
    /// into a wire-ready attachment using the default
    /// [`sunset_image::Config`].
    ///
    /// Returns an error if the bytes aren't a recognised image format,
    /// the codec fails, or the input is HEIC/HEIF (currently
    /// unsupported pending a workspace licence decision — see
    /// `docs/superpowers/specs/2026-05-13-image-preprocessing-design.md`).
    /// Callers should surface the error to the user rather than
    /// silently dropping the attachment.
    pub fn preprocess(bytes: &[u8]) -> Result<Self, sunset_image::Error> {
        Self::preprocess_with(bytes, &sunset_image::Config::default())
    }

    /// Same as [`Self::preprocess`] but with an explicit
    /// [`sunset_image::Config`]. Production callers should prefer
    /// [`Self::preprocess`]; this hook exists for tests that need to
    /// shrink the resize cap to keep fixtures small.
    pub fn preprocess_with(
        bytes: &[u8],
        cfg: &sunset_image::Config,
    ) -> Result<Self, sunset_image::Error> {
        use base64::Engine as _;
        let out = sunset_image::preprocess(bytes, cfg)?;
        Ok(Self {
            mime_type: out.mime_type,
            data_base64: base64::engine::general_purpose::STANDARD.encode(&out.bytes),
        })
    }

    /// Construct an `ImageAttachment` from already-encoded base64
    /// bytes and a claimed MIME type. **No preprocessing applied.**
    ///
    /// Reserved for the receive path (where the bytes arrive
    /// preprocessed by the sender already) and for tests that want to
    /// inject a fixed payload without going through the encoder.
    /// Production sender code should always go through
    /// [`Self::preprocess`] so every client produces a consistent
    /// normalised wire form.
    #[doc(hidden)]
    pub fn raw(mime_type: String, data_base64: String) -> Self {
        Self {
            mime_type,
            data_base64,
        }
    }
}

/// Discriminator for the inner plaintext of a chat-room entry. All
/// variants ride the same `<room_fp>/msg/<value_hash>` namespace and
/// share the AEAD envelope; only the plaintext shape differs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageBody {
    /// A user-authored chat post. Carries free-form text (may be empty
    /// when the post is image-only) and a possibly-empty list of
    /// inline image attachments. The on-wire encoding is `tag(0) +
    /// text + images` — a plain text-only post still encodes the
    /// trailing empty-Vec varint, so this is a wire-format break from
    /// any pre-images stored `Text` entries.
    Text {
        text: String,
        images: Vec<ImageAttachment>,
    },
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

impl MessageBody {
    /// Build a text-only `MessageBody::Text` (empty attachments).
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text {
            text: s.into(),
            images: Vec::new(),
        }
    }
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
            body: MessageBody::text("hello"),
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
        let a = inner_sig_payload_bytes(&fp, 0, 100, &g, &MessageBody::text("hi"));
        let b = inner_sig_payload_bytes(&fp, 1, 100, &g, &MessageBody::text("hi")); // epoch differs
        let c = inner_sig_payload_bytes(&fp, 0, 101, &g, &MessageBody::text("hi")); // sent_at differs
        let d = inner_sig_payload_bytes(&fp, 0, 100, &g, &MessageBody::text("hello")); // body differs
        let e = inner_sig_payload_bytes(
            &RoomFingerprint([2u8; 32]),
            0,
            100,
            &g,
            &MessageBody::text("hi"),
        ); // room differs
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_ne!(a, e);
    }

    #[test]
    fn message_body_text_roundtrips_via_postcard() {
        let body = MessageBody::text("hello");
        let bytes = postcard::to_stdvec(&body).unwrap();
        let decoded: MessageBody = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn message_body_text_with_images_roundtrips_via_postcard() {
        let body = MessageBody::Text {
            text: "look at this".to_owned(),
            images: vec![
                ImageAttachment {
                    mime_type: "image/png".to_owned(),
                    data_base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAQMAAAAl".to_owned(),
                },
                ImageAttachment {
                    mime_type: "image/jpeg".to_owned(),
                    data_base64: "/9j/4AAQSkZJRgABAQEASABIAAD".to_owned(),
                },
            ],
        };
        let bytes = postcard::to_stdvec(&body).unwrap();
        let decoded: MessageBody = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn message_body_image_only_roundtrips_via_postcard() {
        let body = MessageBody::Text {
            text: String::new(),
            images: vec![ImageAttachment {
                mime_type: "image/gif".to_owned(),
                data_base64: "R0lGODlhAQABAAAAACw=".to_owned(),
            }],
        };
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
    fn image_attachment_preprocess_transcodes_png_to_jpeg() {
        // Minimal RGBA PNG written via `image` (dev-dep -free path:
        // hand-craft the bytes is too brittle, but `image` here would
        // pull in the workspace's preprocessing dep tree which we
        // already use in this crate). We test via a known-good PNG
        // header + IDAT — using the public `preprocess` API rather
        // than reaching into sunset-image internals so a future
        // signature change is caught by this test.
        let png = png_fixture_2x2();
        let att = ImageAttachment::preprocess(&png).expect("png must preprocess");
        assert_eq!(att.mime_type, "image/jpeg");
        assert!(
            !att.data_base64.is_empty(),
            "preprocessed image must carry a non-empty base64 payload"
        );

        // The base64 must round-trip into bytes that the `image`
        // crate (via sunset-image's loader) recognises as JPEG.
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&att.data_base64)
            .expect("base64 round-trip");
        assert!(
            raw.starts_with(&[0xff, 0xd8, 0xff]),
            "preprocessed bytes must start with the JPEG SOI marker"
        );
    }

    #[test]
    fn image_attachment_preprocess_surfaces_heic_unsupported() {
        // ftyp box + `heic` brand — the sniffer should route this to
        // the HEIC branch and surface the dedicated error variant.
        let mut bytes = vec![0u8; 4];
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"heic");
        bytes.extend_from_slice(&[0; 8]);
        let err = ImageAttachment::preprocess(&bytes).unwrap_err();
        assert!(matches!(err, sunset_image::Error::HeicUnsupported));
    }

    #[test]
    fn image_attachment_preprocess_rejects_garbage() {
        let err = ImageAttachment::preprocess(b"this is not an image").unwrap_err();
        assert!(matches!(err, sunset_image::Error::UnrecognisedFormat));
    }

    /// Bake a small RGBA PNG via the `image` crate (dev-dep). Used to
    /// drive [`ImageAttachment::preprocess`] without committing a
    /// binary blob; the cost of decoding our own fixture is irrelevant
    /// next to the encode the preprocessor itself does.
    fn png_fixture_2x2() -> Vec<u8> {
        use image::{ImageBuffer, ImageEncoder, Rgba};
        let buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(2, 2, |x, y| {
            Rgba([(x * 100) as u8, (y * 100) as u8, 0x80, 0xff])
        });
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(buf.as_raw(), 2, 2, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    #[test]
    fn message_body_text_postcard_hex_pin() {
        // Pin the postcard encoding so accidental drift breaks the build.
        // postcard encodes a struct variant as: enum-tag, then each field
        // in declaration order. `Text { text, images }` is: tag(0) +
        // len-prefixed UTF-8 string + len-prefixed image-vec.
        let body = MessageBody::text("hi");
        let bytes = postcard::to_stdvec(&body).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        // 00 = Text variant tag; 02 = string length (varint); 6869 = "hi";
        // 00 = empty-images Vec length (varint).
        assert_eq!(hex, "0002686900", "MessageBody::Text wire encoding drifted");
    }

    #[test]
    fn message_body_text_with_one_image_postcard_hex_pin() {
        // Encoding shape: tag(0) | text(len + utf8) | images Vec
        // (len + repeated ImageAttachment). ImageAttachment is a struct,
        // so its fields encode in declaration order: mime_type then
        // data_base64, each as len-prefixed UTF-8.
        let body = MessageBody::Text {
            text: "hi".to_owned(),
            images: vec![ImageAttachment {
                mime_type: "image/png".to_owned(),
                data_base64: "abc".to_owned(),
            }],
        };
        let bytes = postcard::to_stdvec(&body).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        // 00            = Text tag
        // 02 68 69      = text "hi"
        // 01            = images.len() = 1
        // 09 696d6167652f706e67 = mime_type "image/png" (9 bytes)
        // 03 616263     = data_base64 "abc" (3 bytes)
        assert_eq!(
            hex, "000268690109696d6167652f706e6703616263",
            "MessageBody::Text(with image) wire encoding drifted"
        );
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
        // Sentinel: the Text tag must remain 0 even as new fields are
        // added inside the Text variant, and as new variants are tacked
        // onto the enum. If this hex prefix drifts, every persisted
        // entry from a prior on-disk version becomes undecodable.
        let body = MessageBody::text("hi");
        let bytes = postcard::to_stdvec(&body).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "0002686900");
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
            body: MessageBody::text("hi"),
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
        // [74..]: MessageBody::text("hi") tail =
        //   00 (Text tag) 02 6869 (text "hi") 00 (empty images Vec).
        assert_eq!(&bytes[74..], &[0x00, 0x02, 0x68, 0x69, 0x00]);
    }

    #[test]
    fn signed_message_round_trips_channel() {
        let m = SignedMessage {
            inner_signature: Signature([7u8; 64]),
            sent_at_ms: 42,
            channel: ChannelLabel::try_new("links").unwrap(),
            body: MessageBody::text("hello"),
        };
        let bytes = postcard::to_stdvec(&m).unwrap();
        let back: SignedMessage = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.channel.as_str(), "links");
    }

    #[test]
    fn inner_sig_payload_changes_with_channel() {
        let fp = RoomFingerprint([1u8; 32]);
        let body = MessageBody::text("hi");
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
