//! Native sunset.chat relay binary + library for in-process testing.
//!
//! The relay binds a single TCP port (default 8443). Connections are routed by
//! `router.rs`: WebSocket upgrades go to the sync engine; `GET /dashboard`
//! requests are answered inline with a plaintext status page.
//!
//! See `docs/superpowers/specs/2026-04-27-sunset-relay-design.md`.

pub mod config;
pub mod error;
pub mod identity;
pub mod relay;
pub(crate) mod resolver_adapter;
pub(crate) mod router;
pub(crate) mod status;

pub use config::Config;
pub use error::{Error, Result};
pub use relay::{Relay, RelayHandle};
