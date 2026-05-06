//! Audio constants, types, and codec for the sunset.chat voice path.
//!
//! Holds the things every voice-related consumer needs to agree on
//! (sample rate, frame size, codec) plus a `VoiceEncoder` /
//! `VoiceDecoder` pair that consumers feed PCM frames in / encoded
//! bytes out.
//!
//! ## Codec
//!
//! Opus 1.5.2, vendored under `vendor/libopus/` (git submodule),
//! built statically by `build.rs` and called via the FFI surface in
//! `codec::ffi`. Configuration: 48 kHz mono, `OPUS_APPLICATION_VOIP`,
//! 24 kbps target with inband FEC enabled. See `docs/superpowers/
//! specs/2026-04-30-sunset-voice-codec-decision.md` for the history
//! of how we got here (passthrough → libopus, take 2).
//!
//! ## Migration mechanics already done
//!
//! - `CODEC_ID` is `"opus"`.
//! - `VoicePacket::Frame.payload` is variable-length (postcard
//!   `Vec<u8>`). Opus frames at 24 kbps land around 60 bytes/frame
//!   versus the 3840 the passthrough emitted.
//! - The encoder/decoder are stateful — code that previously created
//!   them per-frame must hold them for the lifetime of the peer
//!   stream.

use thiserror::Error;

mod codec;
mod denoise;

use codec::{OpusFrameDecoder, OpusFrameEncoder};

pub use denoise::Denoiser;

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

/// Codec identifier — pinned to the WebCodecs Codec Registry name
/// (`"opus"`). `VoiceFrame` carries this in the wire envelope so
/// receivers refuse frames they cannot decode.
pub const CODEC_ID: &str = "opus";

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid frame size: expected {expected} samples, got {got}")]
    BadFrameSize { expected: usize, got: usize },
    #[error("encoded packet is empty")]
    EmptyEncoded,
    #[error("opus codec error: {0}")]
    Codec(String),
}

pub type Result<T> = core::result::Result<T, Error>;

/// Voice encoder. Consumes a `FRAME_SAMPLES`-long mono f32 PCM frame,
/// emits encoded Opus bytes. Stateful: each call carries adaptive
/// codec state forward, so callers must hold one encoder per outbound
/// stream.
pub struct VoiceEncoder {
    inner: OpusFrameEncoder,
}

impl VoiceEncoder {
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: OpusFrameEncoder::new()?,
        })
    }

    /// Encode exactly one 20 ms frame. Returns the encoded Opus
    /// packet bytes (variable length, typically ~60 bytes at our
    /// 24 kbps target — silence compresses smaller, speech can spike
    /// higher).
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        self.inner.encode(pcm)
    }
}

/// Voice decoder. Mirror of `VoiceEncoder`. Stateful in the same way
/// — Opus uses prior packets to predict missing/dropped frames, so
/// callers must hold one decoder per inbound stream.
pub struct VoiceDecoder {
    inner: OpusFrameDecoder,
}

impl VoiceDecoder {
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: OpusFrameDecoder::new()?,
        })
    }

    /// Decode one Opus packet. Returns the decoded mono f32 PCM
    /// samples — typically `FRAME_SAMPLES` for a 20 ms frame.
    pub fn decode(&mut self, encoded: &[u8]) -> Result<Vec<f32>> {
        self.inner.decode(encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        let sum: f32 = samples.iter().map(|s| s * s).sum();
        (sum / samples.len() as f32).sqrt()
    }

    #[test]
    fn round_trip_silence_stays_silent() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        let silence = vec![0.0_f32; FRAME_SAMPLES];
        // Opus needs a few frames of priming for its predictor.
        // Match `round_trip_preserves_sine_energy`'s 20-frame budget
        // so a libopus update that lengthens the predictor's
        // priming window doesn't fail spuriously.
        let mut last = Vec::new();
        for _ in 0..20 {
            let bytes = enc.encode(&silence).unwrap();
            last = dec.decode(&bytes).unwrap();
        }
        assert_eq!(last.len(), FRAME_SAMPLES);
        // Opus's silence encoding is essentially zero; tolerate
        // numerical fuzz from the predictor's internal state.
        let energy = rms(&last);
        assert!(
            energy < 1e-3,
            "decoded silence has too much energy: {}",
            energy
        );
    }

    #[test]
    fn round_trip_preserves_sine_energy() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        // 440 Hz sine at amplitude 0.5 — well within Opus's nominal
        // input range. Feed several frames so the encoder reaches a
        // stable state before we measure energy preservation.
        let mut decoded_last = Vec::new();
        let input_rms_per_frame = 0.5 / 2f32.sqrt();
        for frame in 0..20 {
            let mut input = vec![0.0_f32; FRAME_SAMPLES];
            for (i, s) in input.iter_mut().enumerate() {
                let n = frame * FRAME_SAMPLES + i;
                let t = n as f32 / SAMPLE_RATE as f32;
                *s = 0.5 * (2.0 * core::f32::consts::PI * 440.0 * t).sin();
            }
            let bytes = enc.encode(&input).unwrap();
            decoded_last = dec.decode(&bytes).unwrap();
        }
        let out_rms = rms(&decoded_last);
        // 440 Hz sine at amplitude 0.5 has RMS = 0.5 / sqrt(2)
        // ≈ 0.354. Opus is lossy; allow ±20% (the threshold the codec
        // decision doc commits us to).
        let lower = input_rms_per_frame * 0.8;
        let upper = input_rms_per_frame * 1.2;
        assert!(
            (lower..=upper).contains(&out_rms),
            "decoded RMS {} out of range [{}, {}]",
            out_rms,
            lower,
            upper
        );
    }

    #[test]
    fn encode_wrong_frame_size_errors() {
        let mut enc = VoiceEncoder::new().unwrap();
        let result = enc.encode(&[0.0_f32; 480]);
        assert!(matches!(
            result,
            Err(Error::BadFrameSize {
                expected: 960,
                got: 480
            })
        ));
    }

    #[test]
    fn decode_empty_packet_errors() {
        let mut dec = VoiceDecoder::new().unwrap();
        let result = dec.decode(&[]);
        assert!(matches!(result, Err(Error::EmptyEncoded)));
    }
}

pub mod packet;
pub mod runtime;

pub use runtime::{Dialer, FrameSink, PeerStateSink, VoicePeerState, VoiceRuntime, VoiceTasks};
