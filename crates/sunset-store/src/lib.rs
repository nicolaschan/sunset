//! sunset-store: signed CRDT KV + content-addressed blob store with pluggable backends.
//!
//! See `docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md` for design.

pub mod canonical;
pub mod error;
pub mod filter;
pub mod store;
pub mod types;
pub mod verifier;

#[cfg(feature = "test-helpers")]
pub mod test_helpers;

pub use canonical::{datagram_signing_payload, signing_payload};
pub use error::{Error, Result};
pub use filter::{Event, Filter, Replay};
pub use store::{EntryStream, EventStream, Store};
pub use types::{ContentBlock, Cursor, Hash, SignedDatagram, SignedKvEntry, VerifyingKey};
pub use verifier::{AcceptAllVerifier, SignatureVerifier};
