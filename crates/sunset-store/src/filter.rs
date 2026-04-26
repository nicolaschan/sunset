//! Subscription / iteration filters and the events delivered on a subscription stream.

use serde::{Deserialize, Serialize};

use crate::types::{Cursor, Hash, SignedKvEntry, VerifyingKey};

/// Expression of a set of `(verifying_key, name)` pairs of interest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filter {
    /// Single exact entry.
    Specific(VerifyingKey, bytes::Bytes),
    /// All entries written by this verifying key.
    Keyspace(VerifyingKey),
    /// All entries with this exact name (across all writers).
    Namespace(bytes::Bytes),
    /// All entries whose name starts with this prefix.
    NamePrefix(bytes::Bytes),
    /// OR composition of multiple filters.
    Union(Vec<Filter>),
}

impl Filter {
    /// True if this filter matches the given (verifying_key, name) pair.
    pub fn matches(&self, vk: &VerifyingKey, name: &[u8]) -> bool {
        match self {
            Filter::Specific(want_vk, want_name) => want_vk == vk && want_name.as_ref() == name,
            Filter::Keyspace(want_vk) => want_vk == vk,
            Filter::Namespace(want_name) => want_name.as_ref() == name,
            Filter::NamePrefix(prefix) => name.starts_with(prefix.as_ref()),
            Filter::Union(filters) => filters.iter().any(|f| f.matches(vk, name)),
        }
    }
}

/// Replay mode for `Store::subscribe`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replay {
    /// Only future events; do not replay history.
    None,
    /// All historical matching entries first, then live updates.
    All,
    /// Events with sequence `>= cursor`, then live updates.
    ///
    /// Cursors are "next-to-be-assigned" sequence numbers (see
    /// `Store::current_cursor`). A cursor captured at time T thus represents
    /// the boundary just after every entry written before T; replaying with
    /// `Since(c)` therefore re-emits entries whose sequence is `>= c.0`,
    /// which in practice means everything written at or after T.
    Since(Cursor),
}

/// Event delivered on a subscription stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A new entry was inserted (no previous entry existed for this key).
    Inserted(SignedKvEntry),
    /// An existing entry was replaced by a higher-priority one.
    Replaced {
        old: SignedKvEntry,
        new: SignedKvEntry,
    },
    /// An entry was removed by TTL expiration.
    Expired(SignedKvEntry),
    /// A new ContentBlock arrived.
    BlobAdded(Hash),
    /// A ContentBlock was reclaimed by GC.
    BlobRemoved(Hash),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(b: &'static [u8]) -> VerifyingKey {
        VerifyingKey::new(bytes::Bytes::from_static(b))
    }
    fn n(b: &'static [u8]) -> bytes::Bytes {
        bytes::Bytes::from_static(b)
    }

    #[test]
    fn filter_specific_matches_exact() {
        let f = Filter::Specific(vk(b"alice"), n(b"room/x"));
        assert!(f.matches(&vk(b"alice"), b"room/x"));
        assert!(!f.matches(&vk(b"alice"), b"room/y"));
        assert!(!f.matches(&vk(b"bob"), b"room/x"));
    }

    #[test]
    fn filter_keyspace_matches_any_name() {
        let f = Filter::Keyspace(vk(b"alice"));
        assert!(f.matches(&vk(b"alice"), b"room/x"));
        assert!(f.matches(&vk(b"alice"), b""));
        assert!(!f.matches(&vk(b"bob"), b"room/x"));
    }

    #[test]
    fn filter_namespace_matches_any_writer() {
        let f = Filter::Namespace(n(b"room/x"));
        assert!(f.matches(&vk(b"alice"), b"room/x"));
        assert!(f.matches(&vk(b"bob"), b"room/x"));
        assert!(!f.matches(&vk(b"alice"), b"room/y"));
    }

    #[test]
    fn filter_name_prefix_matches() {
        let f = Filter::NamePrefix(n(b"room/"));
        assert!(f.matches(&vk(b"x"), b"room/general"));
        assert!(f.matches(&vk(b"x"), b"room/"));
        assert!(!f.matches(&vk(b"x"), b"presence/"));
    }

    #[test]
    fn filter_union_is_or() {
        let f = Filter::Union(vec![
            Filter::Keyspace(vk(b"alice")),
            Filter::Namespace(n(b"room/x")),
        ]);
        assert!(f.matches(&vk(b"alice"), b"random"));
        assert!(f.matches(&vk(b"bob"), b"room/x"));
        assert!(!f.matches(&vk(b"bob"), b"room/y"));
    }

    #[test]
    fn filter_postcard_roundtrip() {
        let f = Filter::Union(vec![
            Filter::Specific(vk(b"a"), n(b"n")),
            Filter::NamePrefix(n(b"p/")),
        ]);
        let bytes = postcard::to_stdvec(&f).unwrap();
        let back: Filter = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(f, back);
    }
}
