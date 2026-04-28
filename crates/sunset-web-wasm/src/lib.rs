//! WASM bundle: sunset-core + sunset-store-memory + sunset-sync +
//! sunset-noise + sunset-sync-ws-browser, exposed to JS as a `Client` class.
//!
//! See `docs/superpowers/specs/2026-04-27-sunset-web-e2e-design.md`.

#[cfg(target_arch = "wasm32")]
mod client;
#[cfg(target_arch = "wasm32")]
mod identity;
#[cfg(target_arch = "wasm32")]
mod messages;
#[cfg(target_arch = "wasm32")]
mod relay_signaler;

#[cfg(target_arch = "wasm32")]
pub use client::Client;
#[cfg(target_arch = "wasm32")]
pub use messages::IncomingMessage;
#[cfg(target_arch = "wasm32")]
pub use relay_signaler::{RelaySignaler, signaling_filter};

#[cfg(not(target_arch = "wasm32"))]
pub struct Client;
#[cfg(not(target_arch = "wasm32"))]
pub struct IncomingMessage;
