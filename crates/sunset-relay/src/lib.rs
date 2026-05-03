//! Native sunset.chat relay binary + library for in-process testing.
//!
//! The relay binds a single TCP port (default 8443) and serves it via
//! axum. `GET /dashboard` returns a plaintext status page; `GET /` either
//! upgrades to a WebSocket (engine path) or returns a JSON identity
//! descriptor. WebSocket upgrades are handed to a `SpawningAcceptor` that
//! runs each Noise IK handshake on its own task, so slow clients can't
//! serialize the inbound pipeline.
//!
//! See `docs/superpowers/specs/2026-05-02-relay-axum-and-concurrent-handshakes-design.md`.

pub mod app;
pub mod bridge;
pub mod config;
pub mod error;
pub mod identity;
pub mod relay;
pub mod render;
pub(crate) mod resolver_adapter;
pub mod snapshot;

pub use config::Config;
pub use error::{Error, Result};
pub use relay::{Relay, RelayHandle};
