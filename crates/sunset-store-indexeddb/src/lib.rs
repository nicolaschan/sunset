//! IndexedDB-backed implementation of `sunset-store::Store`.
//!
//! Browser-only: compiles to a no-op stub on non-wasm targets so the
//! workspace builds cleanly on native (e.g. for `cargo test --workspace`).
//! All real functionality lives under `cfg(target_arch = "wasm32")`.
//!
//! Schema (database `sunset-store` by default):
//! - `entries` object store keyed by postcard-encoded `(verifying_key, name)`.
//!   Value is the postcard-encoded `(sequence, SignedKvEntry)` pair.
//! - `blobs` object store keyed by 32-byte hash. Value is the postcard
//!   encoding of the `ContentBlock`.
//! - `meta` object store keyed by string. Stores `next_sequence: u64`.

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{DEFAULT_DATABASE_NAME, IndexedDbStore, delete_database};

#[cfg(not(target_arch = "wasm32"))]
mod stub;
#[cfg(not(target_arch = "wasm32"))]
pub use stub::{DEFAULT_DATABASE_NAME, IndexedDbStore, delete_database};
