# sunset-store core + memory backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the foundational `sunset-store` crate (data types, traits, conformance suite) plus the in-memory `sunset-store-memory` backend that conforms to the suite.

**Architecture:** `sunset-store` defines the data plane — signed CRDT KV entries pointing at a content-addressed blob store, plus a `Store` trait that backends implement. `sunset-store-memory` is the simplest backend (in-memory, single-process), used by tests and by ephemeral client sessions. The conformance suite lives inside `sunset-store` behind a feature flag and is exercised by the memory backend's integration tests; subsequent backends (`sunset-store-fs`, `sunset-store-indexeddb`) will run the same suite.

**Tech Stack:** Rust 2024 edition, `async-trait` (with `?Send` futures for WASM compat), `postcard` (canonical serialization), `blake3` (content hashing), `bytes` (reference-counted byte buffers), `futures` (async streams), `tokio` (test runtime + in-memory synchronization primitives).

**Spec:** [`docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md`](../specs/2026-04-25-sunset-store-and-sync-design.md)

**Parent architecture spec:** [`docs/superpowers/specs/2026-04-25-sunset-chat-architecture-design.md`](../specs/2026-04-25-sunset-chat-architecture-design.md)

---

## File Structure

```
sunset/
├── Cargo.toml                                # workspace root
├── flake.nix                                 # Rust toolchain via nix
├── flake.lock
├── .envrc                                    # direnv: use flake
├── crates/
│   ├── sunset-store/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                        # module declarations + public re-exports
│   │       ├── types.rs                      # Hash, VerifyingKey, Cursor, SignedKvEntry, ContentBlock
│   │       ├── filter.rs                     # Filter, Replay, Event
│   │       ├── error.rs                      # Error enum + Result alias
│   │       ├── verifier.rs                   # SignatureVerifier trait + AcceptAllVerifier
│   │       ├── store.rs                      # Store trait
│   │       └── test_helpers.rs               # conformance suite (gated by `test-helpers` feature)
│   └── sunset-store-memory/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs                        # MemoryStore re-export
│           ├── store.rs                      # MemoryStore struct + Store impl
│           └── subscription.rs               # subscription channel manager
```

Boundaries:
- `types.rs` — pure data, `Serialize`/`Deserialize`/`PartialEq` derives, no async.
- `filter.rs` — query/subscription expression types, pure data.
- `error.rs` — single `Error` enum + `Result<T>` alias used everywhere.
- `verifier.rs` — synchronous trait; `AcceptAllVerifier` for tests.
- `store.rs` — async trait; the public surface backends implement.
- `test_helpers.rs` — generic conformance suite parameterized over `Store`; behind a feature flag.

---

## Tasks

### Task 1: Initialize workspace skeleton (cargo + nix + direnv)

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `flake.nix`
- Create: `flake.lock` (generated)
- Create: `.envrc`
- Create: `crates/sunset-store/Cargo.toml`
- Create: `crates/sunset-store/src/lib.rs`
- Create: `crates/sunset-store-memory/Cargo.toml`
- Create: `crates/sunset-store-memory/src/lib.rs`

- [ ] **Step 1: Write `Cargo.toml` workspace root**

```toml
# Cargo.toml
[workspace]
members = ["crates/sunset-store", "crates/sunset-store-memory"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT"
rust-version = "1.85"

[workspace.dependencies]
async-trait = "0.1"
blake3 = { version = "1", features = ["serde"] }
bytes = { version = "1", features = ["serde"] }
futures = "0.3"
postcard = { version = "1", features = ["use-std"] }
serde = { version = "1", features = ["derive"] }
thiserror = "2"
tokio = { version = "1", features = ["sync", "rt", "macros", "time"] }
sunset-store = { path = "crates/sunset-store" }
sunset-store-memory = { path = "crates/sunset-store-memory" }
```

- [ ] **Step 2: Write `flake.nix`**

```nix
{
  description = "sunset.chat";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };
  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" ];
        };
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = [ rustToolchain pkgs.cargo-watch pkgs.cargo-nextest ];
        };
      });
}
```

- [ ] **Step 3: Write `.envrc`**

```bash
use flake
```

- [ ] **Step 4: Write `crates/sunset-store/Cargo.toml`**

```toml
[package]
name = "sunset-store"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
async-trait.workspace = true
blake3.workspace = true
bytes.workspace = true
futures.workspace = true
postcard.workspace = true
serde.workspace = true
thiserror.workspace = true

[dev-dependencies]
tokio.workspace = true

[features]
test-helpers = []
```

- [ ] **Step 5: Write `crates/sunset-store/src/lib.rs`**

```rust
//! sunset-store: signed CRDT KV + content-addressed blob store with pluggable backends.
//!
//! See `docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md` for design.
```

- [ ] **Step 6: Write `crates/sunset-store-memory/Cargo.toml`**

```toml
[package]
name = "sunset-store-memory"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
async-trait.workspace = true
blake3.workspace = true
bytes.workspace = true
futures.workspace = true
sunset-store.workspace = true
tokio = { workspace = true, features = ["sync"] }

[dev-dependencies]
sunset-store = { workspace = true, features = ["test-helpers"] }
tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "time"] }
```

- [ ] **Step 7: Write `crates/sunset-store-memory/src/lib.rs`**

```rust
//! In-memory implementation of `sunset-store::Store`.
```

- [ ] **Step 8: Generate `flake.lock` and verify the workspace builds**

Run: `nix flake update && cargo build`
Expected: workspace builds without errors (will produce empty lib crates).

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml flake.nix flake.lock .envrc crates/
git commit -m "Initialize cargo workspace + nix flake + crate skeletons"
```

---

### Task 2: Define `Hash`, `VerifyingKey`, `Cursor` core types

**Files:**
- Create: `crates/sunset-store/src/types.rs`
- Modify: `crates/sunset-store/src/lib.rs` (add `pub mod types;` and re-exports)

- [ ] **Step 1: Write the failing test in `crates/sunset-store/src/types.rs`**

```rust
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
```

- [ ] **Step 2: Wire the module into `lib.rs`**

Replace `crates/sunset-store/src/lib.rs` with:

```rust
//! sunset-store: signed CRDT KV + content-addressed blob store with pluggable backends.
//!
//! See `docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md` for design.

pub mod types;

pub use types::{Cursor, Hash, VerifyingKey};
```

- [ ] **Step 3: Run tests, verify they pass**

Run: `cargo test -p sunset-store`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store/src/
git commit -m "Add Hash, VerifyingKey, Cursor core types with postcard roundtrip tests"
```

---

### Task 3: Define `SignedKvEntry` and `ContentBlock`

**Files:**
- Modify: `crates/sunset-store/src/types.rs` (append types)

- [ ] **Step 1: Append the new types and tests to `types.rs`**

Add at the bottom of `crates/sunset-store/src/types.rs` (before the `#[cfg(test)] mod tests`):

```rust
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
    pub fn hash(&self) -> Hash {
        let bytes = postcard::to_stdvec(self).expect("ContentBlock must serialize");
        blake3::hash(&bytes).into()
    }
}
```

- [ ] **Step 2: Append matching tests inside the existing `tests` module in `types.rs`**

Add inside `mod tests { ... }`:

```rust
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
```

- [ ] **Step 3: Update `lib.rs` re-exports**

Change `pub use types::{Cursor, Hash, VerifyingKey};` to:

```rust
pub use types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p sunset-store`
Expected: 8 passed (4 new + 4 from Task 2).

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-store/src/
git commit -m "Add SignedKvEntry and ContentBlock types with postcard + blake3 determinism tests"
```

---

### Task 4: Define `Filter`, `Replay`, `Event` enums

**Files:**
- Create: `crates/sunset-store/src/filter.rs`
- Modify: `crates/sunset-store/src/lib.rs`

- [ ] **Step 1: Write `crates/sunset-store/src/filter.rs`**

```rust
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
    /// Events with sequence > `cursor`, then live updates.
    Since(Cursor),
}

/// Event delivered on a subscription stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A new entry was inserted (no previous entry existed for this key).
    Inserted(SignedKvEntry),
    /// An existing entry was replaced by a higher-priority one.
    Replaced { old: SignedKvEntry, new: SignedKvEntry },
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

    fn vk(b: &'static [u8]) -> VerifyingKey { VerifyingKey::new(bytes::Bytes::from_static(b)) }
    fn n(b: &'static [u8]) -> bytes::Bytes { bytes::Bytes::from_static(b) }

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
```

- [ ] **Step 2: Update `lib.rs`**

```rust
pub mod filter;
pub mod types;

pub use filter::{Event, Filter, Replay};
pub use types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p sunset-store`
Expected: 14 passed (6 new + 8 prior).

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store/src/
git commit -m "Add Filter / Replay / Event enums with matcher tests"
```

---

### Task 5: Define `Error` and `Result` alias

**Files:**
- Create: `crates/sunset-store/src/error.rs`
- Modify: `crates/sunset-store/src/lib.rs`

- [ ] **Step 1: Write `crates/sunset-store/src/error.rs`**

```rust
//! Error type for sunset-store operations.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// Wrapped backend-specific failure (rusqlite, IndexedDB DOM exception, etc.).
    #[error("backend error: {0}")]
    Backend(String),

    /// `SignatureVerifier::verify` rejected the entry.
    #[error("signature verification failed")]
    SignatureInvalid,

    /// Write rejected because an existing entry has equal or higher priority.
    #[error("entry is stale (existing priority >= new)")]
    Stale,

    /// `entry.value_hash` did not match the hash of the supplied `ContentBlock`.
    #[error("entry value_hash does not match supplied blob hash")]
    HashMismatch,

    /// Read returned no result.
    #[error("not found")]
    NotFound,

    /// Internal invariant violation (entry signature unexpectedly fails on read,
    /// malformed ContentBlock, etc.). Indicates data integrity issue.
    #[error("data corruption: {0}")]
    Corrupt(String),

    /// Operation on a closed store handle.
    #[error("store handle is closed")]
    Closed,
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_display() {
        assert!(format!("{}", Error::SignatureInvalid).contains("signature"));
        assert!(format!("{}", Error::Stale).contains("stale"));
        assert!(format!("{}", Error::Backend("oops".into())).contains("oops"));
    }
}
```

- [ ] **Step 2: Update `lib.rs`**

```rust
pub mod error;
pub mod filter;
pub mod types;

pub use error::{Error, Result};
pub use filter::{Event, Filter, Replay};
pub use types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p sunset-store`
Expected: 15 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store/src/
git commit -m "Add Error enum and Result alias"
```

---

### Task 6: Define `SignatureVerifier` trait + `AcceptAllVerifier`

**Files:**
- Create: `crates/sunset-store/src/verifier.rs`
- Modify: `crates/sunset-store/src/lib.rs`

- [ ] **Step 1: Write `crates/sunset-store/src/verifier.rs`**

```rust
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
    fn verify(&self, _entry: &SignedKvEntry) -> Result<()> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Hash, VerifyingKey};

    fn dummy_entry() -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: VerifyingKey::new(bytes::Bytes::from_static(b"k")),
            name:          bytes::Bytes::from_static(b"n"),
            value_hash:    Hash::from_bytes([0u8; 32]),
            priority:      0,
            expires_at:    None,
            signature:     bytes::Bytes::from_static(b"s"),
        }
    }

    #[test]
    fn accept_all_verifier_accepts() {
        let v = AcceptAllVerifier;
        assert!(v.verify(&dummy_entry()).is_ok());
    }
}
```

- [ ] **Step 2: Update `lib.rs`**

```rust
pub mod error;
pub mod filter;
pub mod types;
pub mod verifier;

pub use error::{Error, Result};
pub use filter::{Event, Filter, Replay};
pub use types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};
pub use verifier::{AcceptAllVerifier, SignatureVerifier};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p sunset-store`
Expected: 16 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store/src/
git commit -m "Add SignatureVerifier trait and AcceptAllVerifier"
```

---

### Task 7: Define the `Store` trait

**Files:**
- Create: `crates/sunset-store/src/store.rs`
- Modify: `crates/sunset-store/src/lib.rs`

- [ ] **Step 1: Write `crates/sunset-store/src/store.rs`**

```rust
//! The `Store` trait: the public surface every backend implements.

use async_trait::async_trait;
use futures::stream::LocalBoxStream;

use crate::error::Result;
use crate::filter::{Event, Filter, Replay};
use crate::types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};

/// Stream of `SignedKvEntry` values yielded by `Store::iter`.
pub type EntryStream<'a> = LocalBoxStream<'a, Result<SignedKvEntry>>;
/// Stream of `Event` values yielded by `Store::subscribe`.
pub type EventStream<'a> = LocalBoxStream<'a, Result<Event>>;

/// Pluggable backend trait. Implementations live in separate crates.
///
/// Implementations are expected to:
/// - Call the configured `SignatureVerifier` on every insert.
/// - Apply LWW by `(verifying_key, name)` priority (higher wins; ties are stale).
/// - Reject inserts whose `entry.value_hash` does not match `blob.hash()` when
///   `blob` is supplied.
/// - Make `(blob, entry)` writes atomic (both succeed or neither).
/// - Accept entries whose referenced blob is not yet locally present (lazy refs).
/// - Maintain a monotonic per-store sequence used for cursors.
///
/// `?Send` futures are used so that non-`Send` WASM backends are accepted.
#[async_trait(?Send)]
pub trait Store {
    /// Insert an entry, optionally with its referenced blob.
    ///
    /// Validation order:
    /// 1. If `blob` is `Some`, `entry.value_hash` must equal `blob.hash()`.
    /// 2. The configured `SignatureVerifier` must accept the entry.
    /// 3. LWW: an existing entry with `priority >= entry.priority` causes `Error::Stale`.
    async fn insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()>;

    /// Insert a content block by itself; returns its hash.
    async fn put_content(&self, block: ContentBlock) -> Result<Hash>;

    /// Get a content block by hash.
    async fn get_content(&self, hash: &Hash) -> Result<Option<ContentBlock>>;

    /// Get the current entry for `(vk, name)`, if any.
    async fn get_entry(&self, vk: &VerifyingKey, name: &[u8]) -> Result<Option<SignedKvEntry>>;

    /// Stream all entries currently in the store matching `filter`.
    /// Backends should walk indexes appropriate to the filter.
    async fn iter<'a>(&'a self, filter: Filter) -> Result<EntryStream<'a>>;

    /// Subscribe to events. `replay` controls whether historical entries are
    /// emitted before live updates.
    async fn subscribe<'a>(&'a self, filter: Filter, replay: Replay) -> Result<EventStream<'a>>;

    /// Delete all entries with `expires_at <= now`. Returns the count removed.
    /// Should emit `Event::Expired` for each on active subscriptions.
    async fn delete_expired(&self, now: u64) -> Result<usize>;

    /// Mark-and-sweep over content blobs reachable from live KV entries.
    /// Returns the count reclaimed.
    async fn gc_blobs(&self) -> Result<usize>;

    /// Returns the current monotonic cursor (inclusive of the last commit).
    async fn current_cursor(&self) -> Result<Cursor>;
}
```

- [ ] **Step 2: Update `lib.rs`**

```rust
pub mod error;
pub mod filter;
pub mod store;
pub mod types;
pub mod verifier;

pub use error::{Error, Result};
pub use filter::{Event, Filter, Replay};
pub use store::{EntryStream, EventStream, Store};
pub use types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};
pub use verifier::{AcceptAllVerifier, SignatureVerifier};
```

- [ ] **Step 3: Verify the trait compiles**

Run: `cargo build -p sunset-store`
Expected: builds successfully.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store/src/
git commit -m "Add Store trait with EntryStream / EventStream type aliases"
```

---

### Task 8: Memory backend skeleton — `MemoryStore` struct + locking

**Files:**
- Create: `crates/sunset-store-memory/src/store.rs`
- Modify: `crates/sunset-store-memory/src/lib.rs`

- [ ] **Step 1: Write `crates/sunset-store-memory/src/store.rs`**

```rust
//! In-memory implementation of `sunset-store::Store`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use sunset_store::{
    ContentBlock, Cursor, Hash, SignedKvEntry, SignatureVerifier, VerifyingKey,
};
use tokio::sync::Mutex;

/// Composite key: `(verifying_key, name)`.
type KvKey = (VerifyingKey, bytes::Bytes);

#[derive(Debug)]
pub(crate) struct StoredEntry {
    pub entry:    SignedKvEntry,
    pub sequence: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Inner {
    pub entries:      BTreeMap<KvKey, StoredEntry>,
    pub blobs:        HashMap<Hash, ContentBlock>,
    pub next_sequence: u64,
}

impl Inner {
    pub fn assign_sequence(&mut self) -> u64 {
        let s = self.next_sequence;
        self.next_sequence += 1;
        s
    }
}

/// In-memory `Store` implementation.
pub struct MemoryStore {
    pub(crate) verifier: Arc<dyn SignatureVerifier>,
    pub(crate) inner:    Arc<Mutex<Inner>>,
}

impl MemoryStore {
    /// Construct with the given signature verifier.
    pub fn new(verifier: Arc<dyn SignatureVerifier>) -> Self {
        Self {
            verifier,
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Convenience: construct with `AcceptAllVerifier`. For tests.
    pub fn with_accept_all() -> Self {
        Self::new(Arc::new(sunset_store::AcceptAllVerifier))
    }

    /// Returns the current cursor (last assigned sequence).
    pub async fn current_cursor_now(&self) -> Cursor {
        let inner = self.inner.lock().await;
        Cursor(inner.next_sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_store_starts_at_cursor_zero() {
        let store = MemoryStore::with_accept_all();
        assert_eq!(store.current_cursor_now().await, Cursor(0));
    }
}
```

- [ ] **Step 2: Update `crates/sunset-store-memory/src/lib.rs`**

```rust
//! In-memory implementation of `sunset-store::Store`.

mod store;

pub use store::MemoryStore;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p sunset-store-memory`
Expected: 1 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store-memory/src/
git commit -m "Add MemoryStore struct skeleton with shared mutable inner state"
```

---

### Task 9: Memory backend — `put_content` / `get_content`

**Files:**
- Modify: `crates/sunset-store-memory/src/store.rs`
- Create: `crates/sunset-store-memory/src/subscription.rs` (placeholder, populated in Task 14)

- [ ] **Step 1: Add the impl block (partial Store trait) — content methods**

Append to `crates/sunset-store-memory/src/store.rs`:

```rust
use async_trait::async_trait;
use sunset_store::{Error, Result, Store};

#[async_trait(?Send)]
impl Store for MemoryStore {
    async fn put_content(&self, block: ContentBlock) -> Result<Hash> {
        let hash = block.hash();
        let mut inner = self.inner.lock().await;
        inner.blobs.entry(hash).or_insert(block);
        Ok(hash)
    }

    async fn get_content(&self, hash: &Hash) -> Result<Option<ContentBlock>> {
        let inner = self.inner.lock().await;
        Ok(inner.blobs.get(hash).cloned())
    }

    // ===== to be filled in subsequent tasks =====
    async fn insert(&self, _entry: SignedKvEntry, _blob: Option<ContentBlock>) -> Result<()> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn get_entry(&self, _vk: &VerifyingKey, _name: &[u8]) -> Result<Option<SignedKvEntry>> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn iter<'a>(&'a self, _filter: sunset_store::Filter) -> Result<sunset_store::EntryStream<'a>> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn subscribe<'a>(&'a self, _filter: sunset_store::Filter, _replay: sunset_store::Replay) -> Result<sunset_store::EventStream<'a>> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn delete_expired(&self, _now: u64) -> Result<usize> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn gc_blobs(&self) -> Result<usize> {
        Err(Error::Backend("not implemented".into()))
    }
    async fn current_cursor(&self) -> Result<Cursor> {
        Ok(self.current_cursor_now().await)
    }
}
```

- [ ] **Step 2: Add tests**

Append to the `tests` module in `crates/sunset-store-memory/src/store.rs`:

```rust
    #[tokio::test]
    async fn put_then_get_content_roundtrip() {
        let store = MemoryStore::with_accept_all();
        let block = ContentBlock {
            data: bytes::Bytes::from_static(b"hello"),
            references: vec![],
        };
        let h = store.put_content(block.clone()).await.unwrap();
        assert_eq!(h, block.hash());
        let back = store.get_content(&h).await.unwrap().unwrap();
        assert_eq!(back, block);
    }

    #[tokio::test]
    async fn get_content_returns_none_for_unknown_hash() {
        let store = MemoryStore::with_accept_all();
        let h = Hash::from_bytes([7u8; 32]);
        assert!(store.get_content(&h).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn put_content_is_idempotent_on_same_block() {
        let store = MemoryStore::with_accept_all();
        let block = ContentBlock {
            data: bytes::Bytes::from_static(b"x"),
            references: vec![],
        };
        let h1 = store.put_content(block.clone()).await.unwrap();
        let h2 = store.put_content(block.clone()).await.unwrap();
        assert_eq!(h1, h2);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p sunset-store-memory`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store-memory/src/
git commit -m "Implement MemoryStore put_content / get_content with idempotence test"
```

---

### Task 10: Memory backend — `insert` (signature, hash check, LWW)

**Files:**
- Modify: `crates/sunset-store-memory/src/store.rs` (replace `insert` stub + add `get_entry`)

- [ ] **Step 1: Replace the `insert` stub with the real implementation, and replace the `get_entry` stub**

In `crates/sunset-store-memory/src/store.rs`, replace these two methods inside the `impl Store for MemoryStore` block:

```rust
    async fn insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()> {
        // 1. Hash-match check.
        if let Some(b) = &blob {
            if b.hash() != entry.value_hash {
                return Err(Error::HashMismatch);
            }
        }
        // 2. Signature verification.
        self.verifier.verify(&entry)?;
        // 3. LWW + atomic insert.
        let mut inner = self.inner.lock().await;
        let key: KvKey = (entry.verifying_key.clone(), entry.name.clone());
        if let Some(existing) = inner.entries.get(&key) {
            if existing.entry.priority >= entry.priority {
                return Err(Error::Stale);
            }
        }
        // Atomic: insert blob first (idempotent), then KV row.
        if let Some(b) = blob {
            inner.blobs.entry(entry.value_hash).or_insert(b);
        }
        let sequence = inner.assign_sequence();
        inner.entries.insert(key, StoredEntry { entry, sequence });
        Ok(())
    }

    async fn get_entry(&self, vk: &VerifyingKey, name: &[u8]) -> Result<Option<SignedKvEntry>> {
        let inner = self.inner.lock().await;
        let key = (vk.clone(), bytes::Bytes::copy_from_slice(name));
        Ok(inner.entries.get(&key).map(|s| s.entry.clone()))
    }
```

- [ ] **Step 2: Add tests**

Append to the `tests` module:

```rust
    use sunset_store::{AcceptAllVerifier, Filter, Replay};

    fn vk(b: &'static [u8]) -> VerifyingKey { VerifyingKey::new(bytes::Bytes::from_static(b)) }
    fn n(b: &'static [u8]) -> bytes::Bytes { bytes::Bytes::from_static(b) }

    fn entry_pointing_to(block: &ContentBlock, vk_bytes: &'static [u8], name: &'static [u8], priority: u64) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name:          n(name),
            value_hash:    block.hash(),
            priority,
            expires_at:    None,
            signature:     bytes::Bytes::from_static(b"sig"),
        }
    }

    fn small_block(payload: &'static [u8]) -> ContentBlock {
        ContentBlock { data: bytes::Bytes::from_static(payload), references: vec![] }
    }

    #[tokio::test]
    async fn insert_then_get_entry_roundtrip() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"hello");
        let entry = entry_pointing_to(&block, b"alice", b"room/x", 1);
        store.insert(entry.clone(), Some(block)).await.unwrap();
        let back = store.get_entry(&vk(b"alice"), b"room/x").await.unwrap().unwrap();
        assert_eq!(back, entry);
    }

    #[tokio::test]
    async fn insert_rejects_hash_mismatch() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"hello");
        let mut entry = entry_pointing_to(&block, b"alice", b"r", 1);
        entry.value_hash = Hash::from_bytes([0u8; 32]);
        let other_block = small_block(b"goodbye");
        assert!(matches!(
            store.insert(entry, Some(other_block)).await,
            Err(Error::HashMismatch)
        ));
    }

    #[tokio::test]
    async fn insert_rejects_lower_or_equal_priority() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        let first = entry_pointing_to(&block, b"alice", b"r", 5);
        store.insert(first.clone(), Some(block.clone())).await.unwrap();

        // Equal priority -> Stale.
        let same = entry_pointing_to(&block, b"alice", b"r", 5);
        assert!(matches!(store.insert(same, Some(block.clone())).await, Err(Error::Stale)));

        // Lower priority -> Stale.
        let lower = entry_pointing_to(&block, b"alice", b"r", 4);
        assert!(matches!(store.insert(lower, Some(block.clone())).await, Err(Error::Stale)));
    }

    #[tokio::test]
    async fn insert_replaces_with_higher_priority() {
        let store = MemoryStore::with_accept_all();
        let block_v1 = small_block(b"v1");
        let block_v2 = small_block(b"v2");

        let v1 = entry_pointing_to(&block_v1, b"alice", b"r", 1);
        let v2 = entry_pointing_to(&block_v2, b"alice", b"r", 2);
        store.insert(v1, Some(block_v1)).await.unwrap();
        store.insert(v2.clone(), Some(block_v2.clone())).await.unwrap();

        let current = store.get_entry(&vk(b"alice"), b"r").await.unwrap().unwrap();
        assert_eq!(current, v2);
    }

    #[tokio::test]
    async fn insert_lazy_ref_succeeds_without_blob() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"future");
        let entry = entry_pointing_to(&block, b"alice", b"r", 1);
        // Insert entry only; blob is not yet here.
        store.insert(entry, None).await.unwrap();
        // Reading the blob via its hash returns None until it arrives.
        assert!(store.get_content(&block.hash()).await.unwrap().is_none());
        // Later, the blob can be put separately.
        store.put_content(block.clone()).await.unwrap();
        assert!(store.get_content(&block.hash()).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn insert_calls_signature_verifier() {
        struct RejectAll;
        impl SignatureVerifier for RejectAll {
            fn verify(&self, _e: &SignedKvEntry) -> sunset_store::Result<()> {
                Err(sunset_store::Error::SignatureInvalid)
            }
        }
        let store = MemoryStore::new(Arc::new(RejectAll));
        let block = small_block(b"x");
        let entry = entry_pointing_to(&block, b"alice", b"r", 1);
        assert!(matches!(
            store.insert(entry, Some(block)).await,
            Err(Error::SignatureInvalid)
        ));
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p sunset-store-memory`
Expected: 10 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store-memory/src/
git commit -m "Implement MemoryStore insert + get_entry with LWW, hash-match, and signature checks"
```

---

### Task 11: Memory backend — `iter` with filter matching

**Files:**
- Modify: `crates/sunset-store-memory/src/store.rs` (replace `iter` stub)

- [ ] **Step 1: Replace the `iter` stub**

In `impl Store for MemoryStore`, replace `iter` with:

```rust
    async fn iter<'a>(&'a self, filter: sunset_store::Filter) -> Result<sunset_store::EntryStream<'a>> {
        // Snapshot current matching entries to avoid holding the lock during streaming.
        let inner = self.inner.lock().await;
        let matching: Vec<SignedKvEntry> = inner
            .entries
            .iter()
            .filter(|((vk, name), _)| filter.matches(vk, name.as_ref()))
            .map(|(_, stored)| stored.entry.clone())
            .collect();
        drop(inner);
        let stream = futures::stream::iter(matching.into_iter().map(Ok));
        Ok(Box::pin(stream))
    }
```

- [ ] **Step 2: Add tests**

```rust
    use futures::StreamExt;

    async fn collect_iter(store: &MemoryStore, filter: Filter) -> Vec<SignedKvEntry> {
        let mut s = store.iter(filter).await.unwrap();
        let mut out = vec![];
        while let Some(item) = s.next().await {
            out.push(item.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn iter_keyspace_returns_only_matching_writer() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"alice", b"a", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"alice", b"b", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"bob",   b"a", 1), Some(block.clone())).await.unwrap();

        let results = collect_iter(&store, Filter::Keyspace(vk(b"alice"))).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.verifying_key == vk(b"alice")));
    }

    #[tokio::test]
    async fn iter_namespace_returns_all_writers_at_name() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"alice", b"room/g", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"bob",   b"room/g", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"alice", b"room/h", 1), Some(block.clone())).await.unwrap();

        let results = collect_iter(&store, Filter::Namespace(n(b"room/g"))).await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn iter_name_prefix_matches_prefix() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"a", b"room/g", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"a", b"room/h", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"a", b"presence/x", 1), Some(block.clone())).await.unwrap();

        let results = collect_iter(&store, Filter::NamePrefix(n(b"room/"))).await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn iter_specific_returns_at_most_one() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"a", b"x", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"b", b"x", 1), Some(block.clone())).await.unwrap();

        let results = collect_iter(&store, Filter::Specific(vk(b"a"), n(b"x"))).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].verifying_key, vk(b"a"));
    }

    #[tokio::test]
    async fn iter_union_is_or() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"a", b"room/g", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"b", b"presence/x", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"c", b"unrelated", 1), Some(block.clone())).await.unwrap();

        let f = Filter::Union(vec![
            Filter::NamePrefix(n(b"room/")),
            Filter::NamePrefix(n(b"presence/")),
        ]);
        let results = collect_iter(&store, f).await;
        assert_eq!(results.len(), 2);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p sunset-store-memory`
Expected: 15 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store-memory/src/
git commit -m "Implement MemoryStore iter with snapshot streaming and filter matching tests"
```

---

### Task 12: Memory backend — `delete_expired`

**Files:**
- Modify: `crates/sunset-store-memory/src/store.rs` (replace `delete_expired` stub)

- [ ] **Step 1: Replace the `delete_expired` stub**

```rust
    async fn delete_expired(&self, now: u64) -> Result<usize> {
        let mut inner = self.inner.lock().await;
        // Collect keys to remove first; we mutate after.
        let to_remove: Vec<KvKey> = inner
            .entries
            .iter()
            .filter(|(_, stored)| stored.entry.expires_at.is_some_and(|e| e <= now))
            .map(|(k, _)| k.clone())
            .collect();
        let count = to_remove.len();
        for key in to_remove {
            inner.entries.remove(&key);
        }
        Ok(count)
    }
```

- [ ] **Step 2: Add tests**

```rust
    fn entry_with_expiry(block: &ContentBlock, vk_bytes: &'static [u8], name: &'static [u8], priority: u64, expires_at: u64) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name:          n(name),
            value_hash:    block.hash(),
            priority,
            expires_at:    Some(expires_at),
            signature:     bytes::Bytes::from_static(b"sig"),
        }
    }

    #[tokio::test]
    async fn delete_expired_removes_only_past_entries() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_with_expiry(&block, b"a", b"old", 1, 100), Some(block.clone())).await.unwrap();
        store.insert(entry_with_expiry(&block, b"a", b"future", 1, 1000), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"a", b"forever", 1), Some(block.clone())).await.unwrap();

        let removed = store.delete_expired(500).await.unwrap();
        assert_eq!(removed, 1);
        assert!(store.get_entry(&vk(b"a"), b"old").await.unwrap().is_none());
        assert!(store.get_entry(&vk(b"a"), b"future").await.unwrap().is_some());
        assert!(store.get_entry(&vk(b"a"), b"forever").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_expired_at_boundary_includes_equal() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_with_expiry(&block, b"a", b"x", 1, 100), Some(block.clone())).await.unwrap();
        let removed = store.delete_expired(100).await.unwrap();
        assert_eq!(removed, 1);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p sunset-store-memory`
Expected: 17 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store-memory/src/
git commit -m "Implement MemoryStore delete_expired with boundary-inclusive TTL test"
```

---

### Task 13: Memory backend — `gc_blobs` (mark-and-sweep)

**Files:**
- Modify: `crates/sunset-store-memory/src/store.rs` (replace `gc_blobs` stub)

- [ ] **Step 1: Replace the `gc_blobs` stub**

```rust
    async fn gc_blobs(&self) -> Result<usize> {
        use std::collections::HashSet;
        let mut inner = self.inner.lock().await;

        // Mark phase: every live KV entry's value_hash is a root; walk references transitively.
        let mut reachable: HashSet<Hash> = HashSet::new();
        let mut frontier: Vec<Hash> = inner
            .entries
            .values()
            .map(|s| s.entry.value_hash)
            .collect();

        while let Some(h) = frontier.pop() {
            if !reachable.insert(h) { continue; }
            if let Some(block) = inner.blobs.get(&h) {
                for r in &block.references {
                    if !reachable.contains(r) {
                        frontier.push(*r);
                    }
                }
            }
        }

        // Sweep phase: drop unreachable.
        let to_remove: Vec<Hash> = inner
            .blobs
            .keys()
            .filter(|h| !reachable.contains(h))
            .copied()
            .collect();
        let count = to_remove.len();
        for h in to_remove {
            inner.blobs.remove(&h);
        }
        Ok(count)
    }
```

- [ ] **Step 2: Add tests**

```rust
    #[tokio::test]
    async fn gc_blobs_keeps_reachable_drops_orphans() {
        let store = MemoryStore::with_accept_all();
        // A live entry pointing at a block with a transitive reference.
        let leaf = small_block(b"leaf");
        let head = ContentBlock {
            data: bytes::Bytes::from_static(b"head"),
            references: vec![leaf.hash()],
        };
        let entry = entry_pointing_to(&head, b"a", b"x", 1);
        store.put_content(leaf.clone()).await.unwrap();
        store.insert(entry, Some(head.clone())).await.unwrap();

        // An orphan block, unreferenced.
        let orphan = small_block(b"orphan");
        store.put_content(orphan.clone()).await.unwrap();

        let reclaimed = store.gc_blobs().await.unwrap();
        assert_eq!(reclaimed, 1);
        assert!(store.get_content(&head.hash()).await.unwrap().is_some());
        assert!(store.get_content(&leaf.hash()).await.unwrap().is_some());
        assert!(store.get_content(&orphan.hash()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn gc_blobs_handles_dangling_value_hash() {
        // KV entry references a blob we don't have locally (lazy ref); GC must not crash.
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"future");
        let entry = entry_pointing_to(&block, b"a", b"x", 1);
        store.insert(entry, None).await.unwrap();   // no blob yet
        let reclaimed = store.gc_blobs().await.unwrap();
        assert_eq!(reclaimed, 0);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p sunset-store-memory`
Expected: 19 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store-memory/src/
git commit -m "Implement MemoryStore gc_blobs with mark-and-sweep over reachable refs"
```

---

### Task 14: Memory backend — `subscribe` (with Replay support and Event delivery)

**Files:**
- Create: `crates/sunset-store-memory/src/subscription.rs`
- Modify: `crates/sunset-store-memory/src/store.rs` (replace `subscribe` stub; emit events from `insert` / `delete_expired` / `gc_blobs`)
- Modify: `crates/sunset-store-memory/src/lib.rs` (declare module)

- [ ] **Step 1: Write `crates/sunset-store-memory/src/subscription.rs`**

```rust
//! Per-store subscription manager: maintains a list of broadcast channels,
//! one per active subscription, each filtered by the subscriber's `Filter`.

use std::sync::{Arc, Mutex, Weak};

use sunset_store::{Event, Filter};
use tokio::sync::mpsc;

#[derive(Debug)]
pub(crate) struct Subscription {
    pub filter: Filter,
    pub tx:     mpsc::UnboundedSender<sunset_store::Result<Event>>,
}

#[derive(Debug, Default)]
pub(crate) struct SubscriptionList {
    pub entries: Mutex<Vec<Weak<Subscription>>>,
}

impl SubscriptionList {
    pub fn add(&self, sub: Arc<Subscription>) {
        let mut g = self.entries.lock().unwrap();
        g.retain(|w| w.upgrade().is_some());
        g.push(Arc::downgrade(&sub));
    }

    /// Broadcast an event to every live subscription whose filter matches.
    pub fn broadcast(&self, event: Event) {
        // Caller passes us an event whose vk/name we can extract; we match per-subscription.
        let (vk, name) = match &event {
            Event::Inserted(e) | Event::Expired(e) => (Some(e.verifying_key.clone()), Some(e.name.clone())),
            Event::Replaced { new, .. } => (Some(new.verifying_key.clone()), Some(new.name.clone())),
            Event::BlobAdded(_) | Event::BlobRemoved(_) => (None, None),
        };

        let mut g = self.entries.lock().unwrap();
        g.retain(|w| {
            if let Some(s) = w.upgrade() {
                let interested = match (&vk, &name) {
                    (Some(v), Some(n)) => s.filter.matches(v, n.as_ref()),
                    _ => true, // BlobAdded / BlobRemoved are delivered to all subscribers
                };
                if interested {
                    let _ = s.tx.send(Ok(event.clone()));
                }
                true
            } else {
                false
            }
        });
    }
}
```

- [ ] **Step 2: Update `lib.rs`**

```rust
mod store;
mod subscription;

pub use store::MemoryStore;
```

- [ ] **Step 3: Modify `MemoryStore` to own a `SubscriptionList` and emit events**

In `crates/sunset-store-memory/src/store.rs`, change the struct and its methods.

Update the `Inner` and `MemoryStore` definitions and the methods that should emit events:

```rust
use crate::subscription::{Subscription, SubscriptionList};
use std::sync::Arc as StdArc;

#[derive(Debug, Default)]
pub(crate) struct Inner {
    pub entries:       BTreeMap<KvKey, StoredEntry>,
    pub blobs:         HashMap<Hash, ContentBlock>,
    pub next_sequence: u64,
}

pub struct MemoryStore {
    pub(crate) verifier:      Arc<dyn SignatureVerifier>,
    pub(crate) inner:         Arc<Mutex<Inner>>,
    pub(crate) subscriptions: Arc<SubscriptionList>,
}

impl MemoryStore {
    pub fn new(verifier: Arc<dyn SignatureVerifier>) -> Self {
        Self {
            verifier,
            inner:         Arc::new(Mutex::new(Inner::default())),
            subscriptions: Arc::new(SubscriptionList::default()),
        }
    }

    pub fn with_accept_all() -> Self {
        Self::new(Arc::new(sunset_store::AcceptAllVerifier))
    }

    pub async fn current_cursor_now(&self) -> Cursor {
        let inner = self.inner.lock().await;
        Cursor(inner.next_sequence)
    }
}
```

Update the `insert` method to emit an `Event::Inserted` or `Event::Replaced` after the entry is committed:

```rust
    async fn insert(&self, entry: SignedKvEntry, blob: Option<ContentBlock>) -> Result<()> {
        if let Some(b) = &blob {
            if b.hash() != entry.value_hash {
                return Err(Error::HashMismatch);
            }
        }
        self.verifier.verify(&entry)?;
        let event = {
            let mut inner = self.inner.lock().await;
            let key: KvKey = (entry.verifying_key.clone(), entry.name.clone());
            let prev = inner.entries.get(&key).map(|s| s.entry.clone());
            if let Some(existing) = &prev {
                if existing.priority >= entry.priority {
                    return Err(Error::Stale);
                }
            }
            let blob_added_hash = if let Some(b) = blob {
                let already = inner.blobs.contains_key(&entry.value_hash);
                inner.blobs.entry(entry.value_hash).or_insert(b);
                if already { None } else { Some(entry.value_hash) }
            } else {
                None
            };
            let sequence = inner.assign_sequence();
            inner.entries.insert(key, StoredEntry { entry: entry.clone(), sequence });
            (prev, blob_added_hash)
        };
        let (prev, blob_added) = event;
        if let Some(h) = blob_added {
            self.subscriptions.broadcast(Event::BlobAdded(h));
        }
        if let Some(old) = prev {
            self.subscriptions.broadcast(Event::Replaced { old, new: entry });
        } else {
            self.subscriptions.broadcast(Event::Inserted(entry));
        }
        Ok(())
    }
```

Update `delete_expired` to emit `Event::Expired`:

```rust
    async fn delete_expired(&self, now: u64) -> Result<usize> {
        let removed: Vec<SignedKvEntry> = {
            let mut inner = self.inner.lock().await;
            let to_remove: Vec<KvKey> = inner
                .entries
                .iter()
                .filter(|(_, s)| s.entry.expires_at.is_some_and(|e| e <= now))
                .map(|(k, _)| k.clone())
                .collect();
            to_remove
                .into_iter()
                .filter_map(|k| inner.entries.remove(&k).map(|s| s.entry))
                .collect()
        };
        let count = removed.len();
        for e in removed {
            self.subscriptions.broadcast(Event::Expired(e));
        }
        Ok(count)
    }
```

Update `gc_blobs` to emit `Event::BlobRemoved`:

```rust
    async fn gc_blobs(&self) -> Result<usize> {
        use std::collections::HashSet;
        let removed: Vec<Hash> = {
            let mut inner = self.inner.lock().await;
            let mut reachable: HashSet<Hash> = HashSet::new();
            let mut frontier: Vec<Hash> = inner.entries.values().map(|s| s.entry.value_hash).collect();
            while let Some(h) = frontier.pop() {
                if !reachable.insert(h) { continue; }
                if let Some(block) = inner.blobs.get(&h) {
                    for r in &block.references {
                        if !reachable.contains(r) { frontier.push(*r); }
                    }
                }
            }
            let to_remove: Vec<Hash> = inner.blobs.keys().filter(|h| !reachable.contains(h)).copied().collect();
            for h in &to_remove {
                inner.blobs.remove(h);
            }
            to_remove
        };
        let count = removed.len();
        for h in removed {
            self.subscriptions.broadcast(Event::BlobRemoved(h));
        }
        Ok(count)
    }
```

Replace the `subscribe` stub:

```rust
    async fn subscribe<'a>(&'a self, filter: sunset_store::Filter, replay: sunset_store::Replay) -> Result<sunset_store::EventStream<'a>> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sub = StdArc::new(Subscription { filter: filter.clone(), tx });
        self.subscriptions.add(sub.clone());

        // Build the historical replay portion (snapshot under the lock).
        let historical: Vec<sunset_store::Result<Event>> = {
            let inner = self.inner.lock().await;
            let mut out: Vec<(u64, Event)> = inner
                .entries
                .iter()
                .filter(|((vk, name), _)| filter.matches(vk, name.as_ref()))
                .filter(|(_, stored)| match replay {
                    sunset_store::Replay::None => false,
                    sunset_store::Replay::All => true,
                    sunset_store::Replay::Since(c) => stored.sequence >= c.0,
                })
                .map(|(_, stored)| (stored.sequence, Event::Inserted(stored.entry.clone())))
                .collect();
            out.sort_by_key(|(s, _)| *s);
            out.into_iter().map(|(_, e)| Ok(e)).collect()
        };

        // Stream historical, then transition to live events from the channel.
        // Hold sub-Arc inside the stream so the weak pointer stays alive.
        let live = async_stream::stream! {
            // (sub kept alive by being moved into closure below; see explicit move)
            let _hold = sub;
            for h in historical { yield h; }
            while let Some(item) = rx.recv().await { yield item; }
        };
        Ok(Box::pin(live))
    }
```

Note: `subscribe` uses `async_stream::stream!`. Add `async-stream = "0.3"` to `crates/sunset-store-memory/Cargo.toml` `[dependencies]`:

```toml
async-stream = "0.3"
```

- [ ] **Step 4: Add tests**

```rust
    #[tokio::test]
    async fn subscribe_replay_none_only_emits_future_events() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        // Pre-existing entry — should NOT replay.
        store.insert(entry_pointing_to(&block, b"a", b"r", 1), Some(block.clone())).await.unwrap();

        let mut sub = store.subscribe(Filter::Keyspace(vk(b"a")), Replay::None).await.unwrap();

        // Future event — should arrive.
        store.insert(entry_pointing_to(&block, b"a", b"r2", 1), Some(block.clone())).await.unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), sub.next()).await.unwrap().unwrap().unwrap();
        match evt { Event::Inserted(e) => assert_eq!(e.name.as_ref(), b"r2"), _ => panic!("unexpected event {:?}", evt) }
    }

    #[tokio::test]
    async fn subscribe_replay_all_emits_history_then_live() {
        let store = MemoryStore::with_accept_all();
        let block = small_block(b"x");
        store.insert(entry_pointing_to(&block, b"a", b"r1", 1), Some(block.clone())).await.unwrap();
        store.insert(entry_pointing_to(&block, b"a", b"r2", 1), Some(block.clone())).await.unwrap();

        let mut sub = store.subscribe(Filter::Keyspace(vk(b"a")), Replay::All).await.unwrap();
        // Two historical.
        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_millis(200), sub.next()).await.unwrap().unwrap().unwrap();
        }
        // One live.
        store.insert(entry_pointing_to(&block, b"a", b"r3", 1), Some(block.clone())).await.unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), sub.next()).await.unwrap().unwrap().unwrap();
        match evt { Event::Inserted(e) => assert_eq!(e.name.as_ref(), b"r3"), _ => panic!() }
    }

    #[tokio::test]
    async fn subscribe_replaced_event_on_higher_priority_overwrite() {
        let store = MemoryStore::with_accept_all();
        let b1 = small_block(b"v1");
        let b2 = small_block(b"v2");
        store.insert(entry_pointing_to(&b1, b"a", b"r", 1), Some(b1.clone())).await.unwrap();
        let mut sub = store.subscribe(Filter::Keyspace(vk(b"a")), Replay::None).await.unwrap();
        store.insert(entry_pointing_to(&b2, b"a", b"r", 2), Some(b2.clone())).await.unwrap();
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), sub.next()).await.unwrap().unwrap().unwrap();
        match evt {
            Event::Replaced { old, new } => {
                assert_eq!(old.priority, 1);
                assert_eq!(new.priority, 2);
            }
            other => panic!("expected Replaced, got {:?}", other),
        }
    }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p sunset-store-memory`
Expected: 22 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-store-memory/
git commit -m "Implement MemoryStore subscribe with replay modes and event broadcast on writes"
```

---

### Task 15: Conformance suite — set up `test_helpers` module + first test cases

**Files:**
- Create: `crates/sunset-store/src/test_helpers.rs`
- Modify: `crates/sunset-store/src/lib.rs`

- [ ] **Step 1: Write `crates/sunset-store/src/test_helpers.rs`**

```rust
//! Generic conformance suite. Any `Store` implementation can be exercised
//! against this suite to verify it satisfies the documented contract.
//!
//! Gated by the `test-helpers` feature so production builds don't pull these in.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::filter::{Event, Filter, Replay};
use crate::store::Store;
use crate::types::{ContentBlock, Hash, SignedKvEntry, VerifyingKey};
use crate::verifier::SignatureVerifier;

/// Helper: a verifying key from static bytes.
pub fn vk(b: &'static [u8]) -> VerifyingKey {
    VerifyingKey::new(bytes::Bytes::from_static(b))
}

/// Helper: a name from static bytes.
pub fn n(b: &'static [u8]) -> bytes::Bytes {
    bytes::Bytes::from_static(b)
}

/// Helper: a small leaf block.
pub fn block(payload: &'static [u8]) -> ContentBlock {
    ContentBlock { data: bytes::Bytes::from_static(payload), references: vec![] }
}

/// Helper: an entry pointing at `block`'s hash, with the given key/name/priority.
pub fn entry(block: &ContentBlock, vk_bytes: &'static [u8], name: &'static [u8], priority: u64) -> SignedKvEntry {
    SignedKvEntry {
        verifying_key: vk(vk_bytes),
        name:          n(name),
        value_hash:    block.hash(),
        priority,
        expires_at:    None,
        signature:     bytes::Bytes::from_static(b"sig"),
    }
}

/// A verifier that asserts entries pass through it. Useful to detect when a
/// backend forgets to call its verifier on insert.
pub struct CountingVerifier(pub Arc<std::sync::atomic::AtomicUsize>);
impl SignatureVerifier for CountingVerifier {
    fn verify(&self, _entry: &SignedKvEntry) -> Result<()> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// Run the full conformance suite against `store_factory`. The factory is
/// called once per test case to create a fresh store.
pub async fn run_conformance_suite<S, F>(store_factory: F)
where
    S: Store,
    F: Fn() -> S,
{
    insert_get_roundtrip(&store_factory()).await;
    lww_supersession(&store_factory()).await;
    stale_rejection(&store_factory()).await;
    hash_mismatch_rejection(&store_factory()).await;
    lazy_dangling_ref(&store_factory()).await;
    ttl_pruning(&store_factory()).await;
    blob_gc_reachability(&store_factory()).await;
    iter_filters(&store_factory()).await;
    subscribe_replay_modes(&store_factory()).await;
    subscribe_replay_since_cursor(&store_factory()).await;
}

/// Test: insert + get_entry roundtrip.
pub async fn insert_get_roundtrip<S: Store>(store: &S) {
    let b = block(b"hello");
    let e = entry(&b, b"alice", b"r", 1);
    store.insert(e.clone(), Some(b.clone())).await.unwrap();
    let got = store.get_entry(&e.verifying_key, &e.name).await.unwrap().unwrap();
    assert_eq!(got, e, "insert/get roundtrip");
    let got_blob = store.get_content(&b.hash()).await.unwrap().unwrap();
    assert_eq!(got_blob, b, "blob roundtrip");
}

/// Test: higher-priority insert replaces lower; the value is reachable.
pub async fn lww_supersession<S: Store>(store: &S) {
    let b1 = block(b"v1");
    let b2 = block(b"v2");
    store.insert(entry(&b1, b"a", b"r", 1), Some(b1)).await.unwrap();
    let v2 = entry(&b2, b"a", b"r", 2);
    store.insert(v2.clone(), Some(b2)).await.unwrap();
    let now = store.get_entry(&v2.verifying_key, &v2.name).await.unwrap().unwrap();
    assert_eq!(now, v2, "higher priority replaces");
}

/// Test: equal-or-lower priority is rejected.
pub async fn stale_rejection<S: Store>(store: &S) {
    let b = block(b"x");
    store.insert(entry(&b, b"a", b"r", 5), Some(b.clone())).await.unwrap();
    let same = entry(&b, b"a", b"r", 5);
    assert!(matches!(store.insert(same, Some(b.clone())).await, Err(Error::Stale)));
    let lower = entry(&b, b"a", b"r", 4);
    assert!(matches!(store.insert(lower, Some(b)).await, Err(Error::Stale)));
}

/// Test: insert rejects mismatched (entry.value_hash, blob.hash()).
pub async fn hash_mismatch_rejection<S: Store>(store: &S) {
    let real = block(b"real");
    let fake = block(b"fake");
    let mut e = entry(&real, b"a", b"r", 1);
    // Force value_hash to point to `real` while passing `fake`.
    e.value_hash = real.hash();
    assert!(matches!(store.insert(e, Some(fake)).await, Err(Error::HashMismatch)));
}

/// Test: an entry can be inserted without its blob; blob can land later.
pub async fn lazy_dangling_ref<S: Store>(store: &S) {
    let b = block(b"future");
    let e = entry(&b, b"a", b"r", 1);
    store.insert(e, None).await.unwrap();
    assert!(store.get_content(&b.hash()).await.unwrap().is_none());
    store.put_content(b.clone()).await.unwrap();
    assert!(store.get_content(&b.hash()).await.unwrap().is_some());
}

/// Test: `delete_expired(now)` removes entries with `expires_at <= now` (boundary inclusive).
pub async fn ttl_pruning<S: Store>(store: &S) {
    let b = block(b"x");
    let mut old = entry(&b, b"a", b"old", 1);
    old.expires_at = Some(100);
    let mut future = entry(&b, b"a", b"future", 1);
    future.expires_at = Some(1000);
    let forever = entry(&b, b"a", b"forever", 1);
    store.insert(old, Some(b.clone())).await.unwrap();
    store.insert(future, Some(b.clone())).await.unwrap();
    store.insert(forever, Some(b.clone())).await.unwrap();
    let removed = store.delete_expired(100).await.unwrap();
    assert_eq!(removed, 1);
    assert!(store.get_entry(&vk(b"a"), b"old").await.unwrap().is_none());
    assert!(store.get_entry(&vk(b"a"), b"future").await.unwrap().is_some());
    assert!(store.get_entry(&vk(b"a"), b"forever").await.unwrap().is_some());
}

/// Test: gc_blobs keeps reachable blobs and reclaims orphans.
pub async fn blob_gc_reachability<S: Store>(store: &S) {
    let leaf = block(b"leaf");
    let head = ContentBlock { data: bytes::Bytes::from_static(b"head"), references: vec![leaf.hash()] };
    let orphan = block(b"orphan");
    let e = entry(&head, b"a", b"r", 1);
    store.put_content(leaf.clone()).await.unwrap();
    store.insert(e, Some(head.clone())).await.unwrap();
    store.put_content(orphan.clone()).await.unwrap();
    let n = store.gc_blobs().await.unwrap();
    assert_eq!(n, 1, "exactly one orphan reclaimed");
    assert!(store.get_content(&head.hash()).await.unwrap().is_some());
    assert!(store.get_content(&leaf.hash()).await.unwrap().is_some());
    assert!(store.get_content(&orphan.hash()).await.unwrap().is_none());
}

/// Test: iter respects each filter variant.
pub async fn iter_filters<S: Store>(store: &S) {
    use futures::StreamExt;
    let b = block(b"x");
    store.insert(entry(&b, b"a", b"room/g", 1), Some(b.clone())).await.unwrap();
    store.insert(entry(&b, b"a", b"presence/x", 1), Some(b.clone())).await.unwrap();
    store.insert(entry(&b, b"b", b"room/g", 1), Some(b.clone())).await.unwrap();

    async fn collect<S: Store>(s: &S, f: Filter) -> Vec<SignedKvEntry> {
        let mut st = s.iter(f).await.unwrap();
        let mut out = vec![];
        while let Some(item) = st.next().await { out.push(item.unwrap()); }
        out
    }

    let r_keyspace = collect(store, Filter::Keyspace(vk(b"a"))).await;
    assert_eq!(r_keyspace.len(), 2);
    let r_namespace = collect(store, Filter::Namespace(n(b"room/g"))).await;
    assert_eq!(r_namespace.len(), 2);
    let r_prefix = collect(store, Filter::NamePrefix(n(b"room/"))).await;
    assert_eq!(r_prefix.len(), 2);
    let r_specific = collect(store, Filter::Specific(vk(b"a"), n(b"room/g"))).await;
    assert_eq!(r_specific.len(), 1);
    let r_union = collect(store, Filter::Union(vec![
        Filter::NamePrefix(n(b"room/")),
        Filter::NamePrefix(n(b"presence/")),
    ])).await;
    assert_eq!(r_union.len(), 3);
}

/// Test: subscribe under each `Replay` mode delivers correctly.
pub async fn subscribe_replay_modes<S: Store>(store: &S) {
    use futures::StreamExt;
    let b = block(b"x");
    store.insert(entry(&b, b"a", b"r1", 1), Some(b.clone())).await.unwrap();
    store.insert(entry(&b, b"a", b"r2", 1), Some(b.clone())).await.unwrap();

    // Replay::None — only future events.
    let mut s = store.subscribe(Filter::Keyspace(vk(b"a")), Replay::None).await.unwrap();
    store.insert(entry(&b, b"a", b"r3", 1), Some(b.clone())).await.unwrap();
    let evt = tokio::time::timeout(std::time::Duration::from_millis(500), s.next()).await
        .expect("subscribe should deliver new event").unwrap().unwrap();
    matches!(evt, Event::Inserted(e) if e.name.as_ref() == b"r3");

    // Replay::All — historical first, then live.
    let mut s = store.subscribe(Filter::Keyspace(vk(b"a")), Replay::All).await.unwrap();
    for _ in 0..3 {
        tokio::time::timeout(std::time::Duration::from_millis(500), s.next()).await
            .expect("history should be replayed").unwrap().unwrap();
    }
    store.insert(entry(&b, b"a", b"r4", 1), Some(b.clone())).await.unwrap();
    let evt = tokio::time::timeout(std::time::Duration::from_millis(500), s.next()).await
        .expect("subscribe should deliver new event after replay").unwrap().unwrap();
    matches!(evt, Event::Inserted(e) if e.name.as_ref() == b"r4");
}

/// Test: `Replay::Since(cursor)` emits only entries written after the cursor.
pub async fn subscribe_replay_since_cursor<S: Store>(store: &S) {
    use futures::StreamExt;
    let b = block(b"x");
    // Two entries before the cursor snapshot.
    store.insert(entry(&b, b"a", b"r1", 1), Some(b.clone())).await.unwrap();
    store.insert(entry(&b, b"a", b"r2", 1), Some(b.clone())).await.unwrap();
    let cursor = store.current_cursor().await.unwrap();
    // Two entries after the cursor snapshot.
    store.insert(entry(&b, b"a", b"r3", 1), Some(b.clone())).await.unwrap();
    store.insert(entry(&b, b"a", b"r4", 1), Some(b.clone())).await.unwrap();

    let mut s = store.subscribe(Filter::Keyspace(vk(b"a")), Replay::Since(cursor)).await.unwrap();

    // Should replay only r3, r4 (in order).
    let mut names = vec![];
    for _ in 0..2 {
        let evt = tokio::time::timeout(std::time::Duration::from_millis(500), s.next()).await
            .expect("Since-cursor replay should deliver post-cursor entries").unwrap().unwrap();
        if let Event::Inserted(e) = evt {
            names.push(e.name.clone());
        } else {
            panic!("expected Inserted, got {:?}", evt);
        }
    }
    assert_eq!(names, vec![n(b"r3"), n(b"r4")]);
}
```

- [ ] **Step 2: Add module declaration in `lib.rs` (gated by feature)**

```rust
pub mod error;
pub mod filter;
pub mod store;
pub mod types;
pub mod verifier;

#[cfg(feature = "test-helpers")]
pub mod test_helpers;

pub use error::{Error, Result};
pub use filter::{Event, Filter, Replay};
pub use store::{EntryStream, EventStream, Store};
pub use types::{ContentBlock, Cursor, Hash, SignedKvEntry, VerifyingKey};
pub use verifier::{AcceptAllVerifier, SignatureVerifier};
```

- [ ] **Step 3: Build with the feature enabled**

Run: `cargo build -p sunset-store --features test-helpers`
Expected: builds successfully.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store/src/
git commit -m "Add conformance test suite as feature-gated test_helpers module"
```

---

### Task 16: Run conformance suite against `MemoryStore`

**Files:**
- Create: `crates/sunset-store-memory/tests/conformance.rs`

- [ ] **Step 1: Write the integration test**

```rust
//! Verify that MemoryStore satisfies the sunset-store conformance suite.

use sunset_store::test_helpers::run_conformance_suite;
use sunset_store_memory::MemoryStore;

#[tokio::test]
async fn memory_store_passes_conformance_suite() {
    run_conformance_suite(|| MemoryStore::with_accept_all()).await;
}
```

- [ ] **Step 2: Run the conformance test**

Run: `cargo test -p sunset-store-memory --test conformance`
Expected: 1 passed.

- [ ] **Step 3: Run the full workspace test suite**

Run: `cargo test --workspace --all-features`
Expected: all tests pass (≥38 total).

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store-memory/tests/
git commit -m "Run conformance suite against MemoryStore (passes)"
```

---

### Task 17: Run `cargo clippy` and fix any warnings, then a final sanity sweep

**Files:**
- Whatever `clippy` flags as warnings.

- [ ] **Step 1: Run clippy across the workspace**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: no warnings, no errors.

- [ ] **Step 2: Fix any clippy warnings inline**

Common fixes: unnecessary clones, redundant `mut`, missing `Default` impls.

- [ ] **Step 3: Run `cargo fmt`**

Run: `cargo fmt --all`
Expected: no diff (file content is already formatted), or a clean reformat applied.

- [ ] **Step 4: Run `cargo test --workspace --all-features` one last time**

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "Pass clippy + fmt across the workspace"
```

---

## Self-Review Checklist (run before declaring the plan done)

**Spec coverage:**

- [x] `SignedKvEntry` and `ContentBlock` data model with postcard canonical serialization → Tasks 2-3.
- [x] `Filter` / `Replay` / `Event` / `Cursor` / `Error` / `Result` types → Tasks 4-5.
- [x] `SignatureVerifier` trait → Task 6.
- [x] `Store` trait with async streams and `?Send` futures → Task 7.
- [x] LWW semantics and `Stale` rejection → Tasks 10, 15.
- [x] Hash-match check on insert → Tasks 10, 15.
- [x] Lazy dangling references → Tasks 10, 15.
- [x] TTL pruning via `delete_expired` → Tasks 12, 15.
- [x] Mark-and-sweep `gc_blobs` → Tasks 13, 15.
- [x] Subset `iter` over all filter variants → Tasks 11, 15.
- [x] `subscribe` with `Replay::None | All | Since(cursor)` → Tasks 14, 15.
- [x] Per-event atomic insert (no batch atomicity) → Task 10 (single mutex guard around blob+entry write).
- [x] Read-after-write consistency → implicit in single-mutex backend.
- [x] Subscription delivery in serialization order → Task 14 (events broadcast inside the same lock release ordering).
- [x] Conformance suite shipped as a `test-helpers` feature → Tasks 15-16.
- [x] Workspace + nix flake + direnv → Task 1.

**No placeholders:** every task contains the actual code, the actual command, and an expected outcome.

**Type consistency:** `Hash`, `VerifyingKey`, `Cursor`, `SignedKvEntry`, `ContentBlock`, `Filter`, `Replay`, `Event`, `Error`, `Store`, `SignatureVerifier`, `EntryStream`, `EventStream` all defined in earlier tasks and used consistently in later ones. Re-exports in `lib.rs` updated as types are added.

**Out-of-scope (intentionally not in this plan):**

- Other backends (`sunset-store-fs`, `sunset-store-indexeddb`) — separate plans.
- The sync layer (`sunset-sync`) — separate plan, depends on this plan landing.
