//! sunset-core: chat-semantics layer on top of sunset-store.
//!
//! See `docs/superpowers/specs/2026-04-25-sunset-chat-architecture-design.md`
//! for the layering, `docs/superpowers/specs/2026-04-26-sunset-crypto-design.md`
//! for the crypto subsystem, and the v1 plan at
//! `docs/superpowers/plans/2026-04-26-sunset-core-identity-and-encrypted-messages.md`
//! for the scope of this layer.

pub mod canonical;
pub mod crypto;
pub mod error;
pub mod filters;
pub mod identity;
pub mod message;
pub mod verifier;

pub use crypto::room::{Room, RoomFingerprint};
pub use error::{Error, Result};
