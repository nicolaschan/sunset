//! sunset-core: chat-semantics layer on top of sunset-store.
//!
//! See `docs/superpowers/specs/2026-04-25-sunset-chat-architecture-design.md`
//! for the layering, `docs/superpowers/specs/2026-04-26-sunset-crypto-design.md`
//! for the crypto subsystem, and the v1 plan at
//! `docs/superpowers/plans/2026-04-26-sunset-core-identity-and-encrypted-messages.md`
//! for the scope of this layer.

pub mod bus;
pub mod canonical;
pub mod crypto;
pub mod error;
pub mod filters;
pub mod identity;
pub mod liveness;
pub mod membership;
pub mod message;
pub mod reactions;
pub mod verifier;

pub use bus::{Bus, BusEvent, BusImpl};
pub use crypto::envelope::{EncryptedMessage, MessageBody, ReactionAction, SignedMessage};
pub use crypto::room::{Room, RoomFingerprint};
pub use error::{Error, Result};
pub use filters::{room_filter, room_messages_filter};
pub use identity::{Identity, IdentityKey};
pub use liveness::{
    Clock, HasSenderTime, Liveness, LivenessState, PeerLivenessChange, SystemClock,
};
pub use message::{
    ComposedMessage, DecodedMessage, compose_message, compose_reaction, compose_receipt,
    compose_text, decode_message,
};
pub use verifier::Ed25519Verifier;
