//! Browser-side `sunset_sync::RawTransport` over `web_sys::RtcPeerConnection`
//! datachannel.
//!
//! Pair with `sunset_noise::NoiseTransport<R>` (Plan C) for the
//! authenticated encrypted layer over the bytes pipe.
//!
//! See `docs/superpowers/specs/2026-04-27-sunset-sync-webrtc-browser-design.md`.

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{WebRtcRawConnection, WebRtcRawTransport};

#[cfg(not(target_arch = "wasm32"))]
mod stub;
#[cfg(not(target_arch = "wasm32"))]
pub use stub::{WebRtcRawConnection, WebRtcRawTransport};
