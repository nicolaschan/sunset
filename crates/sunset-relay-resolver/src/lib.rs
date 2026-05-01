//! Resolves a user-typed relay address (e.g. `relay.sunset.chat:8443`)
//! into the canonical `wss://host[:port]#x25519=<hex>` PeerAddr the
//! Noise IK handshake expects, by querying the relay's `GET /` JSON
//! identity endpoint.
//!
//! This crate ships no HTTP implementation: callers supply an
//! `HttpFetch` impl. `sunset-relay` uses a `reqwest`-based one;
//! `sunset-web-wasm` uses a `web-sys::fetch`-based one. The pure
//! parsing / JSON-extraction code is unit-testable without any HTTP
//! dependency.

pub mod error;
pub mod json;
pub mod parse;
pub mod resolver;

pub use error::{Error, Result};
