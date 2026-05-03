//! Host-agnostic voice runtime.
//!
//! `VoiceRuntime` owns the protocol state (heartbeat + subscribe +
//! liveness + auto-connect + jitter buffer + mute/deafen). Hosts
//! provide three traits: `Dialer` (ensure direct WebRTC connection),
//! `FrameSink` (deliver decoded PCM to the audio output), and
//! `PeerStateSink` (receive `VoicePeerState` change events).
//!
//! `?Send` throughout — single-threaded, matches the project's WASM
//! constraint. Hosts spawn the returned futures with whatever
//! single-threaded local-spawn primitive they have
//! (`wasm_bindgen_futures::spawn_local` for browser, `LocalSet::spawn_local`
//! for native).

mod traits;

pub use traits::{Dialer, FrameSink, PeerStateSink, VoicePeerState};
