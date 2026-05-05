//! Audio constants, packet types, and the host-agnostic `VoiceRuntime`
//! for the sunset.chat voice path.
//!
//! Holds the things every voice-related consumer needs to agree on
//! (sample rate, frame size, codec ID for the uncompressed fallback)
//! plus the encrypted `VoicePacket` wire format and the runtime that
//! drives heartbeat / subscribe / jitter / auto-connect / liveness.
//!
//! ## The runtime is codec-agnostic
//!
//! Encoded audio bytes flow opaquely between the wire and the host's
//! `FrameSink` keyed only by `codec_id`. The browser host plugs
//! WebCodecs `AudioEncoder` / `AudioDecoder` (Opus) in at the JS
//! boundary; the runtime never executes a codec itself. The
//! `pcm-f32-le` codec ID is reserved for the uncompressed fallback
//! (host implementations or environments without WebCodecs Opus
//! support — e.g. Firefox at the time the encoder gap closes — should
//! send raw little-endian f32 PCM under this ID).
//!
//! See `docs/superpowers/specs/2026-04-30-sunset-voice-codec-decision.md`
//! for the history of how we got here (libopus link attempts, the
//! WebCodecs Firefox encoder gap circa 2026-04-30, etc.).

/// Sample rate of every audio buffer that crosses the voice path.
/// Opus's native rate; the browser AudioContext is created at this
/// rate so we never resample.
pub const SAMPLE_RATE: u32 = 48_000;

/// Mono. Voice doesn't benefit from stereo at the bandwidth we care about.
pub const CHANNELS: u32 = 1;

/// Samples per 20 ms frame at 48 kHz mono. Standard VoIP cadence;
/// the audio worklet buffers 128-sample quanta into frames of this
/// size before handing them to the encoder.
pub const FRAME_SAMPLES: usize = 960;

/// Frame duration in milliseconds.
pub const FRAME_DURATION_MS: u32 = 20;

/// Codec identifier for the uncompressed PCM fallback. Encoded payload
/// is `FRAME_SAMPLES` little-endian f32 samples (3840 bytes for a
/// 20 ms / 960-sample frame).
///
/// Hosts running WebCodecs Opus advertise `"opus"` instead. The
/// receiver dispatches by codec_id and runs the appropriate
/// browser-side decoder; the runtime is opaque to either choice.
pub const CODEC_ID: &str = "pcm-f32-le";

/// WebCodecs Opus codec identifier (per the WebCodecs Codec Registry).
/// Carried in `VoicePacket::Frame.codec_id` when the browser's
/// `AudioEncoder` is encoding voice; the receive side dispatches on
/// this string to pick its `AudioDecoder` configuration.
pub const CODEC_ID_OPUS: &str = "opus";

pub mod packet;

pub mod runtime;

pub use runtime::{Dialer, FrameSink, PeerStateSink, VoicePeerState, VoiceRuntime, VoiceTasks};
