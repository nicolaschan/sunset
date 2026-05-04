//! Application-level body of presence heartbeat entries.
//!
//! Encoded as the `ContentBlock.data` of every
//! `<room_fp>/presence/<my_pk>` entry (postcard). The signed-entry
//! `value_hash` already covers the block, so integrity end-to-end is
//! unchanged from the empty-body baseline.
//!
//! Forward extensibility: postcard does NOT auto-tolerate added fields
//! the way protobuf does. Any future addition must be `Option<T>` with
//! `#[serde(default)]`, AND the pinned wire-vector tests below must be
//! updated in lockstep so accidental wire drift fails CI.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresenceBody {
    /// User-chosen display name. None ⇒ no name set; receivers fall
    /// back to short_pubkey rendering. The publisher trims leading/
    /// trailing whitespace and truncates to 64 `chars()` (Unicode
    /// scalar values, NOT grapheme clusters); receivers do not
    /// re-validate.
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_none() {
        let body = PresenceBody { name: None };
        let bytes = postcard::to_stdvec(&body).unwrap();
        let back: PresenceBody = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, body);
    }

    #[test]
    fn roundtrip_ascii() {
        let body = PresenceBody {
            name: Some("alice".to_owned()),
        };
        let bytes = postcard::to_stdvec(&body).unwrap();
        let back: PresenceBody = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, body);
    }

    #[test]
    fn roundtrip_utf8_multibyte() {
        // 2-byte and 4-byte sequences in one string.
        let body = PresenceBody {
            name: Some("naïve 🌅".to_owned()),
        };
        let bytes = postcard::to_stdvec(&body).unwrap();
        let back: PresenceBody = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, body);
    }

    #[test]
    fn roundtrip_64_char_name() {
        let body = PresenceBody {
            name: Some("a".repeat(64)),
        };
        let bytes = postcard::to_stdvec(&body).unwrap();
        let back: PresenceBody = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, body);
    }

    /// Pin the v1 wire encoding for `name = Some("alice")`. If this
    /// fails, the wire format has shifted — bump the spec and update
    /// downstream clients before changing this vector.
    #[test]
    fn wire_vector_some_alice() {
        let body = PresenceBody {
            name: Some("alice".to_owned()),
        };
        let bytes = postcard::to_stdvec(&body).unwrap();
        // Option discriminant 0x01 (Some), varint length 0x05, "alice"
        // bytes (0x61 0x6c 0x69 0x63 0x65).
        assert_eq!(bytes, vec![0x01, 0x05, 0x61, 0x6c, 0x69, 0x63, 0x65]);
    }

    #[test]
    fn wire_vector_none() {
        let body = PresenceBody::default();
        let bytes = postcard::to_stdvec(&body).unwrap();
        // Option discriminant 0x00 (None).
        assert_eq!(bytes, vec![0x00]);
    }

    /// Garbage bytes yield Err — the receive path uses this as the
    /// signal to log a warn and treat the peer as `name: None`.
    #[test]
    fn decode_garbage_errors() {
        let bad: &[u8] = &[0xff, 0xff, 0xff];
        assert!(postcard::from_bytes::<PresenceBody>(bad).is_err());
    }
}
