//! Audio constants, types, and codec for the sunset.chat voice path.
//!
//! Holds the things every voice-related consumer needs to agree on
//! (sample rate, frame size, codec) plus a `VoiceEncoder` /
//! `VoiceDecoder` pair that consumers feed PCM frames in / encoded
//! bytes out.
//!
//! ## Codec
//!
//! Opus 1.5.2, vendored under `vendor/libopus/`, built statically by
//! `build.rs` and called via the FFI surface in `codec::ffi`. See
//! `docs/superpowers/specs/2026-04-30-sunset-voice-codec-decision.md`
//! for the history of how we got here (passthrough → libopus, take 2).
//!
//! ## Quality presets
//!
//! `VoiceQuality` selects the encoder's bitrate, channel count, and
//! application mode. Default is `Maximum` (510 kbps stereo, fullband
//! AUDIO mode); a `Voice` preset (24 kbps mono, VOIP) is available
//! for bandwidth-constrained senders. The preset is **send-side**:
//! receivers always decode through a fixed 2-channel decoder, which
//! libopus auto-upmixes mono input into. That keeps the receiver
//! agnostic to whoever is sending — different peers in the same call
//! can pick different quality settings without renegotiation.

use thiserror::Error;

mod codec;

use codec::{OpusFrameDecoder, OpusFrameEncoder};

/// Sample rate of every audio buffer that crosses the voice path.
/// Opus's native rate; the browser AudioContext is created at this
/// rate so we never resample.
pub const SAMPLE_RATE: u32 = 48_000;

/// Channels carried across the playback path. Always 2 (stereo)
/// regardless of encoder preset — mono Opus packets are upmixed to
/// stereo at decode time so the playback worklet doesn't need to
/// reconfigure when a peer's quality changes.
pub const PLAYBACK_CHANNELS: u32 = 2;

/// Samples per 20 ms frame at 48 kHz, **per channel**. The audio
/// worklet buffers 128-sample quanta into frames of this length per
/// channel before handing them to the encoder. A mono frame is
/// `FRAME_SAMPLES_PER_CHANNEL` long total; a stereo frame is
/// interleaved L/R and `FRAME_SAMPLES_PER_CHANNEL * 2` long.
pub const FRAME_SAMPLES_PER_CHANNEL: usize = 960;

/// Frame duration in milliseconds.
pub const FRAME_DURATION_MS: u32 = 20;

/// Codec identifier — pinned to the WebCodecs Codec Registry name
/// (`"opus"`). `VoiceFrame` carries this in the wire envelope so
/// receivers refuse frames they cannot decode.
pub const CODEC_ID: &str = "opus";

// Backwards-compatible alias — pre-stereo, every frame was mono so
// `FRAME_SAMPLES` meant the same thing as `FRAME_SAMPLES_PER_CHANNEL`.
// Kept so existing callers (test fixtures, recorder, etc.) compile
// without churn until we sweep them. New code should prefer the
// per-channel constant + the decoded-frame helpers below.
pub const FRAME_SAMPLES: usize = FRAME_SAMPLES_PER_CHANNEL;

/// Send-side codec quality preset.
///
/// Selecting a preset only changes the encoder; receivers always
/// decode through a fixed 2-channel decoder. The default is
/// `Maximum` because the wire bandwidth even at 510 kbps (~4 KB
/// every 50 ms) is well under what any modern network or our
/// existing P2P transport handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoiceQuality {
    /// 24 kbps mono, `OPUS_APPLICATION_VOIP`. Heaviest perceptual
    /// processing (DTX, comfort noise, aggressive band reduction);
    /// best for bandwidth-constrained senders or when mic input is
    /// known to be voice-only.
    Voice,
    /// 96 kbps stereo, `OPUS_APPLICATION_AUDIO`. Transparent for
    /// speech, comfortable for music; ~2× the wire bandwidth of
    /// `Voice` mode.
    High,
    /// 510 kbps stereo (libopus's documented hard ceiling),
    /// `OPUS_APPLICATION_AUDIO`. Indistinguishable from uncompressed
    /// 16-bit stereo for the listener; chosen as the default per
    /// the principle that voice over P2P is rarely the bottleneck.
    #[default]
    Maximum,
}

impl VoiceQuality {
    /// Channel count the encoder will be configured with.
    pub fn channels(self) -> u32 {
        match self {
            Self::Voice => 1,
            Self::High | Self::Maximum => 2,
        }
    }

    /// Target bitrate in bits per second. Passed to libopus via
    /// `OPUS_SET_BITRATE`.
    pub fn bitrate_bps(self) -> i32 {
        match self {
            Self::Voice => 24_000,
            Self::High => 96_000,
            Self::Maximum => 510_000,
        }
    }

    /// libopus application mode (VOIP vs AUDIO). VOIP applies more
    /// speech-oriented post-processing; AUDIO preserves musical
    /// content.
    pub fn opus_application(self) -> codec::OpusApplication {
        match self {
            Self::Voice => codec::OpusApplication::Voip,
            Self::High | Self::Maximum => codec::OpusApplication::Audio,
        }
    }

    /// Stable string label for serialization to/from JS.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::High => "high",
            Self::Maximum => "maximum",
        }
    }

    /// Parse a `VoiceQuality` from its `as_str` form. Used by the
    /// wasm bindings to read a localStorage value.
    pub fn from_str_label(s: &str) -> Option<Self> {
        match s {
            "voice" => Some(Self::Voice),
            "high" => Some(Self::High),
            "maximum" => Some(Self::Maximum),
            _ => None,
        }
    }
}

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

/// Voice encoder. Stateful — each call carries adaptive codec state
/// forward, so callers must hold one encoder per outbound stream.
///
/// The expected input shape depends on `quality()`:
///   - `VoiceQuality::Voice` (mono): `FRAME_SAMPLES_PER_CHANNEL`
///     samples per `encode` call.
///   - stereo presets: `FRAME_SAMPLES_PER_CHANNEL * 2` interleaved
///     L/R samples per call.
pub struct VoiceEncoder {
    inner: OpusFrameEncoder,
    quality: VoiceQuality,
}

impl VoiceEncoder {
    pub fn new(quality: VoiceQuality) -> Result<Self> {
        Ok(Self {
            inner: OpusFrameEncoder::new(quality)?,
            quality,
        })
    }

    /// The preset this encoder was constructed with.
    pub fn quality(&self) -> VoiceQuality {
        self.quality
    }

    /// Channel count of the input PCM the encoder expects per frame.
    pub fn channels(&self) -> u32 {
        self.quality.channels()
    }

    /// Number of f32 samples expected per `encode` call (channel
    /// count × per-channel frame size).
    pub fn samples_per_frame(&self) -> usize {
        FRAME_SAMPLES_PER_CHANNEL * self.channels() as usize
    }

    /// Encode exactly one 20 ms frame. Returns the encoded Opus
    /// packet bytes (variable length).
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        self.inner.encode(pcm)
    }
}

/// Voice decoder. Always 2-channel stereo regardless of which
/// preset the sender used — libopus auto-upmixes mono Opus packets
/// onto both output channels. Output PCM is always
/// `FRAME_SAMPLES_PER_CHANNEL * 2` interleaved L/R samples per
/// frame.
pub struct VoiceDecoder {
    inner: OpusFrameDecoder,
}

impl VoiceDecoder {
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: OpusFrameDecoder::new()?,
        })
    }

    /// Decode one Opus packet into stereo interleaved f32 PCM.
    /// Returns `FRAME_SAMPLES_PER_CHANNEL * 2` samples; mono packets
    /// have identical L and R.
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

    fn sine_frame(channels: usize, frame_idx: usize) -> Vec<f32> {
        let samples = FRAME_SAMPLES_PER_CHANNEL * channels;
        let mut out = vec![0.0_f32; samples];
        for i in 0..FRAME_SAMPLES_PER_CHANNEL {
            let n = frame_idx * FRAME_SAMPLES_PER_CHANNEL + i;
            let t = n as f32 / SAMPLE_RATE as f32;
            let s = 0.5 * (2.0 * core::f32::consts::PI * 440.0 * t).sin();
            for c in 0..channels {
                out[i * channels + c] = s;
            }
        }
        out
    }

    #[test]
    fn round_trip_silence_stays_silent_voice_preset() {
        let mut enc = VoiceEncoder::new(VoiceQuality::Voice).unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        // Voice preset is mono — one channel × FRAME_SAMPLES_PER_CHANNEL.
        let silence = vec![0.0_f32; FRAME_SAMPLES_PER_CHANNEL];
        let mut last = Vec::new();
        for _ in 0..20 {
            let bytes = enc.encode(&silence).unwrap();
            last = dec.decode(&bytes).unwrap();
        }
        // Decoder always emits stereo: 960 samples × 2 channels.
        assert_eq!(last.len(), FRAME_SAMPLES_PER_CHANNEL * 2);
        let energy = rms(&last);
        assert!(
            energy < 1e-3,
            "decoded silence has too much energy: {}",
            energy
        );
    }

    #[test]
    fn round_trip_preserves_sine_energy_voice_preset() {
        let mut enc = VoiceEncoder::new(VoiceQuality::Voice).unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        let mut decoded_last = Vec::new();
        for frame in 0..20 {
            let input = sine_frame(1, frame);
            let bytes = enc.encode(&input).unwrap();
            decoded_last = dec.decode(&bytes).unwrap();
        }
        let out_rms = rms(&decoded_last);
        let target = 0.5 / 2f32.sqrt();
        assert!(
            (target * 0.8..=target * 1.2).contains(&out_rms),
            "voice preset RMS {} out of [{}, {}]",
            out_rms,
            target * 0.8,
            target * 1.2,
        );
    }

    #[test]
    fn round_trip_preserves_sine_energy_high_preset() {
        let mut enc = VoiceEncoder::new(VoiceQuality::High).unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        let mut decoded_last = Vec::new();
        for frame in 0..20 {
            let input = sine_frame(2, frame);
            let bytes = enc.encode(&input).unwrap();
            decoded_last = dec.decode(&bytes).unwrap();
        }
        assert_eq!(decoded_last.len(), FRAME_SAMPLES_PER_CHANNEL * 2);
        let out_rms = rms(&decoded_last);
        let target = 0.5 / 2f32.sqrt();
        assert!(
            (target * 0.8..=target * 1.2).contains(&out_rms),
            "high preset RMS {} out of [{}, {}]",
            out_rms,
            target * 0.8,
            target * 1.2,
        );
    }

    #[test]
    fn round_trip_preserves_sine_energy_maximum_preset() {
        let mut enc = VoiceEncoder::new(VoiceQuality::Maximum).unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        let mut decoded_last = Vec::new();
        for frame in 0..20 {
            let input = sine_frame(2, frame);
            let bytes = enc.encode(&input).unwrap();
            decoded_last = dec.decode(&bytes).unwrap();
        }
        assert_eq!(decoded_last.len(), FRAME_SAMPLES_PER_CHANNEL * 2);
        let out_rms = rms(&decoded_last);
        let target = 0.5 / 2f32.sqrt();
        // Maximum preset is essentially transparent — tighter bound.
        assert!(
            (target * 0.9..=target * 1.1).contains(&out_rms),
            "maximum preset RMS {} out of [{}, {}]",
            out_rms,
            target * 0.9,
            target * 1.1,
        );
    }

    #[test]
    fn quality_default_is_maximum() {
        assert_eq!(VoiceQuality::default(), VoiceQuality::Maximum);
    }

    #[test]
    fn quality_round_trips_through_str_label() {
        for q in [
            VoiceQuality::Voice,
            VoiceQuality::High,
            VoiceQuality::Maximum,
        ] {
            assert_eq!(VoiceQuality::from_str_label(q.as_str()), Some(q));
        }
        assert!(VoiceQuality::from_str_label("nonsense").is_none());
    }

    #[test]
    fn encode_wrong_frame_size_errors() {
        let mut enc = VoiceEncoder::new(VoiceQuality::Voice).unwrap();
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
    fn encode_wrong_frame_size_for_stereo_errors() {
        let mut enc = VoiceEncoder::new(VoiceQuality::High).unwrap();
        // Stereo expects 1920 samples; passing 960 (mono-shaped)
        // should error.
        let result = enc.encode(&[0.0_f32; 960]);
        assert!(matches!(
            result,
            Err(Error::BadFrameSize {
                expected: 1920,
                got: 960
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
