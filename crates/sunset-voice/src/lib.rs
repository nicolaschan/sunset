//! Voice codec wrappers (Opus) and audio constants for sunset.chat.
//!
//! Used by sunset-web-wasm's voice module for browser-side capture and
//! playback. Pure Rust — no JS, no wasm-bindgen, no Bus integration.
//! Networking and encryption land in C2b on top of this crate.

/// Sample rate used everywhere in the voice path. Opus's native rate
/// for VoIP; the browser AudioContext is created at this rate so we
/// never resample.
pub const SAMPLE_RATE: u32 = 48_000;

/// Mono. Voice doesn't benefit from stereo at the bandwidth budgets
/// we care about.
pub const CHANNELS: usize = 1;

/// Samples per 20 ms frame at 48 kHz mono. Opus's standard VoIP frame
/// duration; the audio worklet buffers 128-sample quanta into frames
/// of this size before handing them to the encoder.
pub const FRAME_SAMPLES: usize = 960;

/// Frame duration in milliseconds.
pub const FRAME_DURATION_MS: u32 = 20;
