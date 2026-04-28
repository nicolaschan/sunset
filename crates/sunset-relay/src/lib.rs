//! Native sunset.chat relay binary + library for in-process testing.
//!
//! See `docs/superpowers/specs/2026-04-27-sunset-relay-design.md`.

pub mod config;
pub mod error;
pub mod identity;
pub mod relay;
pub(crate) mod status;

pub use config::Config;
pub use error::{Error, Result};
pub use relay::{Relay, RelayHandle};
