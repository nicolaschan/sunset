//! Core data types for sunset-store.

use serde::{Deserialize, Serialize};

/// 32-byte BLAKE3 hash, used as content-addressed identifier for `ContentBlock`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hash([u8; 32]);

impl Hash {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }
    pub const fn as_bytes(&self) -> &[u8; 32] { &self.0 }
    pub fn to_hex(&self) -> String { blake3::Hash::from_bytes(self.0).to_hex().to_string() }
}

impl From<blake3::Hash> for Hash {
    fn from(h: blake3::Hash) -> Self { Self(*h.as_bytes()) }
}
impl From<Hash> for blake3::Hash {
    fn from(h: Hash) -> Self { blake3::Hash::from_bytes(h.0) }
}

/// A writer's verifying (public) key. Opaque bytes — sunset-store does not
/// know about specific signature schemes; the application's `SignatureVerifier`
/// interprets these bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VerifyingKey(pub bytes::Bytes);

impl VerifyingKey {
    pub fn new(bytes: impl Into<bytes::Bytes>) -> Self { Self(bytes.into()) }
    pub fn as_bytes(&self) -> &[u8] { &self.0 }
}

/// Opaque cursor; backends maintain a per-store monotonic sequence number.
/// Consumers persist these and pass them back to `Store::subscribe` for resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Cursor(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_roundtrip_through_blake3() {
        let blake = blake3::hash(b"hello world");
        let h: Hash = blake.into();
        assert_eq!(h.as_bytes(), blake.as_bytes());
        let back: blake3::Hash = h.into();
        assert_eq!(back, blake);
    }

    #[test]
    fn hash_postcard_roundtrip() {
        let h = Hash::from_bytes([7u8; 32]);
        let bytes = postcard::to_stdvec(&h).unwrap();
        let back: Hash = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn verifying_key_postcard_roundtrip() {
        let k = VerifyingKey::new(bytes::Bytes::from_static(b"alice-key"));
        let bytes = postcard::to_stdvec(&k).unwrap();
        let back: VerifyingKey = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(k, back);
    }

    #[test]
    fn cursor_ordering_and_postcard() {
        let a = Cursor(1);
        let b = Cursor(2);
        assert!(a < b);
        let bytes = postcard::to_stdvec(&b).unwrap();
        let back: Cursor = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(b, back);
    }
}
