//! Audio constants, types, and codec for the sunset.chat voice path.
//!
//! Holds the things every voice-related consumer needs to agree on
//! (sample rate, frame size, codec) plus a `VoiceEncoder` /
//! `VoiceDecoder` pair that consumers feed PCM frames in / encoded
//! bytes out.
//!
//! ## Today: PCM passthrough
//!
//! The current implementation is a **passthrough** — `encode` writes
//! the f32 PCM samples out as little-endian bytes; `decode` reads
//! them back. There is no compression. We took this path because:
//!
//! - libopus C source would not link cleanly into the wasm32-unknown
//!   cdylib (`opus_*` symbols ended up as `env` imports in the
//!   bundle); see `docs/superpowers/specs/2026-04-29-sunset-voice-codec-decision.md`.
//! - WebCodecs `AudioEncoder` for Opus had spotty Firefox support at
//!   the time we tried it, and pulling in a JS-side encoder pushed
//!   the codec choice into the JS layer in a way that fights the
//!   "all business logic in Rust" principle.
//!
//! Passthrough is fine for the C2a loopback demo (no networking) and
//! for early C2b round-trip testing on a LAN; production-bandwidth
//! voice will need a real codec eventually.
//!
//! ## Migration to a real codec
//!
//! Replace this file's encoder/decoder implementations. The
//! `VoiceEncoder` / `VoiceDecoder` API is the public abstraction;
//! nothing above this crate looks at the encoded bytes' shape, so
//! switching to Opus / a pure-Rust codec / a Rust-bound WebCodecs
//! shim is a contained change here.

use std::convert::TryInto;

use thiserror::Error;

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

/// Bytes per encoded frame in the passthrough codec
/// (`FRAME_SAMPLES` * 4 bytes per f32). When this crate switches to a
/// real codec, callers will need to stop assuming a fixed encoded
/// size — but no caller currently does, so this constant is purely
/// informational.
pub const PASSTHROUGH_ENCODED_BYTES: usize = FRAME_SAMPLES * 4;

/// Codec identifier. For the passthrough implementation it's
/// `"pcm-f32-le"` — explicit so `VoiceFrame` (C2b) can't accidentally
/// be interpreted as Opus on the receive side. Real codecs will
/// reuse the WebCodecs Codec Registry strings (`"opus"`, etc).
pub const CODEC_ID: &str = "pcm-f32-le";

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid frame size: expected {expected} samples, got {got}")]
    BadFrameSize { expected: usize, got: usize },
    #[error("invalid encoded length: expected {expected} bytes, got {got}")]
    BadEncodedSize { expected: usize, got: usize },
}

pub type Result<T> = core::result::Result<T, Error>;

/// Voice encoder. Consumes a `FRAME_SAMPLES`-long mono f32 PCM frame,
/// emits encoded bytes. Stateless today (passthrough), but the API
/// allows a future stateful codec (Opus carries internal state across
/// frames).
#[derive(Default)]
pub struct VoiceEncoder {
    _private: (),
}

impl VoiceEncoder {
    pub fn new() -> Result<Self> {
        Ok(Self { _private: () })
    }

    /// Encode exactly one 20 ms frame. Returns the encoded bytes.
    /// For the passthrough codec, encoded bytes are little-endian
    /// IEEE-754 f32 — the PCM samples cast to bytes.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(Error::BadFrameSize {
                expected: FRAME_SAMPLES,
                got: pcm.len(),
            });
        }
        let mut out = Vec::with_capacity(pcm.len() * 4);
        for sample in pcm {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(out)
    }
}

/// Voice decoder. Mirror of `VoiceEncoder`.
#[derive(Default)]
pub struct VoiceDecoder {
    _private: (),
}

impl VoiceDecoder {
    pub fn new() -> Result<Self> {
        Ok(Self { _private: () })
    }

    /// Decode one encoded packet. Returns exactly `FRAME_SAMPLES`
    /// samples of mono f32 PCM.
    pub fn decode(&mut self, encoded: &[u8]) -> Result<Vec<f32>> {
        if encoded.len() != PASSTHROUGH_ENCODED_BYTES {
            return Err(Error::BadEncodedSize {
                expected: PASSTHROUGH_ENCODED_BYTES,
                got: encoded.len(),
            });
        }
        let mut out = Vec::with_capacity(FRAME_SAMPLES);
        for chunk in encoded.chunks_exact(4) {
            // chunk has length 4 by construction.
            let arr: [u8; 4] = chunk.try_into().expect("chunks_exact(4) yields [u8; 4]");
            out.push(f32::from_le_bytes(arr));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_silence() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        let silence = vec![0.0_f32; FRAME_SAMPLES];
        let bytes = enc.encode(&silence).unwrap();
        assert_eq!(bytes.len(), PASSTHROUGH_ENCODED_BYTES);
        let decoded = dec.decode(&bytes).unwrap();
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        assert!(decoded.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn round_trip_sine_is_bit_exact() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        let mut input = vec![0.0_f32; FRAME_SAMPLES];
        for (i, s) in input.iter_mut().enumerate() {
            let t = i as f32 / SAMPLE_RATE as f32;
            *s = 0.5 * (2.0 * core::f32::consts::PI * 440.0 * t).sin();
        }
        let decoded = dec.decode(&enc.encode(&input).unwrap()).unwrap();
        // Passthrough is bit-exact.
        assert_eq!(input, decoded);
    }

    #[test]
    fn encode_wrong_frame_size_errors() {
        let mut enc = VoiceEncoder::new().unwrap();
        let result = enc.encode(&[0.0_f32; 480]);
        assert!(matches!(
            result,
            Err(Error::BadFrameSize { expected: 960, got: 480 })
        ));
    }

    #[test]
    fn decode_wrong_encoded_size_errors() {
        let mut dec = VoiceDecoder::new().unwrap();
        let result = dec.decode(&[0_u8; 100]);
        assert!(matches!(
            result,
            Err(Error::BadEncodedSize { expected: 3840, got: 100 })
        ));
    }
}

pub mod packet;
