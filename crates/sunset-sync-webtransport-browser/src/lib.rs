//! Browser-side `sunset_sync::RawTransport` over `web_sys::WebTransport`.
//!
//! Pairs with `sunset_noise::NoiseTransport<R>` for the authenticated /
//! encrypted layer; this crate is crypto-unaware.
//!
//! See `docs/superpowers/specs/2026-05-04-sunset-webtransport-design.md`.

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{WebTransportRawConnection, WebTransportRawTransport};

#[cfg(not(target_arch = "wasm32"))]
mod stub;
#[cfg(not(target_arch = "wasm32"))]
pub use stub::{WebTransportRawConnection, WebTransportRawTransport};
