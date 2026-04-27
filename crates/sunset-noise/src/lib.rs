//! Noise tunnel decorator over any `sunset_sync::RawTransport`.
//!
//! See `docs/superpowers/specs/2026-04-27-sunset-sync-ws-native-design.md`.

pub mod error;
pub mod handshake;
pub mod identity;
pub mod pattern;

pub use error::{Error, Result};
pub use handshake::{NoiseConnection, NoiseTransport};
pub use identity::{NoiseIdentity, ed25519_public_to_x25519, ed25519_seed_to_x25519_secret};
pub use pattern::NOISE_PATTERN;
