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

/// A signed KV entry. Last-write-wins by `priority` for a given
/// `(verifying_key, name)` pair. `value_hash` points into the content store.
///
/// `signature` covers the canonical postcard encoding of all other fields.
/// Verification is performed by the host-supplied `SignatureVerifier` on insert.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedKvEntry {
    pub verifying_key: VerifyingKey,
    pub name:          bytes::Bytes,
    pub value_hash:    Hash,
    pub priority:      u64,
    pub expires_at:    Option<u64>,
    pub signature:     bytes::Bytes,
}

/// Content-addressed blob. `references` form a DAG over content blocks;
/// `hash(self) = blake3(postcard::to_stdvec(self))`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlock {
    pub data:       bytes::Bytes,
    pub references: Vec<Hash>,
}

impl ContentBlock {
    /// Compute the canonical hash of this content block.
    ///
    /// The hash is `blake3(postcard::to_stdvec(self))`, evaluated against the
    /// frozen v1 wire format. Two `ContentBlock` values with equal canonical
    /// bytes hash identically across all peers.
    ///
    /// # Panics
    ///
    /// Cannot fail in practice: postcard serialization of `bytes::Bytes` and
    /// `Vec<Hash>` is infallible. The `expect` is present only because
    /// `postcard::to_stdvec` returns `Result`. Any code change that makes this
    /// panic reachable is a bug — the canonical encoding must remain pure.
    pub fn hash(&self) -> Hash {
        let bytes = postcard::to_stdvec(self).expect("ContentBlock must serialize");
        blake3::hash(&bytes).into()
    }
}

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

    #[test]
    fn signed_kv_entry_postcard_roundtrip() {
        let entry = SignedKvEntry {
            verifying_key: VerifyingKey::new(bytes::Bytes::from_static(b"vk")),
            name:          bytes::Bytes::from_static(b"room/general"),
            value_hash:    Hash::from_bytes([3u8; 32]),
            priority:      42,
            expires_at:    Some(99),
            signature:     bytes::Bytes::from_static(b"sig"),
        };
        let bytes = postcard::to_stdvec(&entry).unwrap();
        let back: SignedKvEntry = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn content_block_hash_is_deterministic() {
        let block = ContentBlock {
            data:       bytes::Bytes::from_static(b"hello"),
            references: vec![Hash::from_bytes([1u8; 32])],
        };
        let h1 = block.hash();
        let h2 = block.hash();
        assert_eq!(h1, h2);
    }

    #[test]
    fn content_block_hash_distinguishes_data() {
        let a = ContentBlock { data: bytes::Bytes::from_static(b"a"), references: vec![] };
        let b = ContentBlock { data: bytes::Bytes::from_static(b"b"), references: vec![] };
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn content_block_hash_distinguishes_refs() {
        let a = ContentBlock { data: bytes::Bytes::from_static(b"x"), references: vec![] };
        let b = ContentBlock {
            data: bytes::Bytes::from_static(b"x"),
            references: vec![Hash::from_bytes([0u8; 32])],
        };
        assert_ne!(a.hash(), b.hash());
    }

    /// Frozen test vector. If this fails, the canonical wire format has changed.
    /// Updating the expected hash without changing the wire-format version
    /// constitutes a backward-incompatible change to content addressing and
    /// must be rejected.
    #[test]
    fn content_block_hash_frozen_vector() {
        let block = ContentBlock {
            data:       bytes::Bytes::from_static(b"sunset.chat frozen v1"),
            references: vec![
                Hash::from_bytes([0u8; 32]),
                Hash::from_bytes([1u8; 32]),
            ],
        };
        assert_eq!(
            block.hash().to_hex(),
            "ca24b1d5ebf7c3024cfe5ed5b62cd0097c176de517c6f55c0ada94660f9e104a",
            "If this fails, the canonical encoding has changed — DO NOT update this hex without bumping the wire-format version.",
        );
    }
}
