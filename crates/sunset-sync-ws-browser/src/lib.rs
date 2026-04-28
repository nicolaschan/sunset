//! Browser-side `sunset_sync::RawTransport` over `web_sys::WebSocket`.
//!
//! Pair with `sunset_noise::NoiseTransport<R>` to get an authenticated
//! encrypted `Transport` ready for `SyncEngine`.
//!
//! See `docs/superpowers/specs/2026-04-27-sunset-sync-ws-browser-design.md`.
//!
//! Native (non-wasm) compilation produces stub types that `cargo build`
//! happily, but actual calls to `connect` / `send_reliable` etc. return
//! `sunset_sync::Error::Transport`. This keeps the workspace buildable
//! without wasm tooling while still letting wasm consumers pull the crate
//! in directly.

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{WebSocketRawConnection, WebSocketRawTransport};

#[cfg(not(target_arch = "wasm32"))]
mod stub;
#[cfg(not(target_arch = "wasm32"))]
pub use stub::{WebSocketRawConnection, WebSocketRawTransport};
