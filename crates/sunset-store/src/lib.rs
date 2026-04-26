//! sunset-store: signed CRDT KV + content-addressed blob store with pluggable backends.
//!
//! See `docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md` for design.

pub mod types;

pub use types::{Cursor, Hash, VerifyingKey};
