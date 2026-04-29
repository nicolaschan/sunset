//! Voice codec wrappers (Opus) and audio constants for sunset.chat.
//!
//! Used by sunset-web-wasm's voice module for browser-side capture and
//! playback. Pure Rust — no JS, no wasm-bindgen, no Bus integration.
//! Networking and encryption land in C2b on top of this crate.

// FFI into the vendored libopus C library is inherently unsafe.
#![allow(unsafe_code)]

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

// ---------------------------------------------------------------------------
// libopus FFI
// ---------------------------------------------------------------------------

use std::ffi::CStr;

// Constants verified against vendor/libopus/include/opus_defines.h.
const OPUS_OK: i32 = 0;
const OPUS_APPLICATION_VOIP: i32 = 2048;
const OPUS_SET_BITRATE_REQUEST: i32 = 4002;

/// Opaque encoder state type. Only ever held behind a raw pointer.
#[repr(C)]
struct OpusEncoder {
    _private: [u8; 0],
}

/// Opaque decoder state type. Only ever held behind a raw pointer.
#[repr(C)]
struct OpusDecoder {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn opus_encoder_create(
        fs: i32,
        channels: i32,
        application: i32,
        error: *mut i32,
    ) -> *mut OpusEncoder;

    fn opus_encoder_destroy(st: *mut OpusEncoder);

    // We bind only the fixed-arity form we need: SET_BITRATE takes one i32
    // value argument. Variadic FFI is avoided.
    fn opus_encoder_ctl(st: *mut OpusEncoder, request: i32, value: i32) -> i32;

    fn opus_encode_float(
        st: *mut OpusEncoder,
        pcm: *const f32,
        frame_size: i32,
        data: *mut u8,
        max_data_bytes: i32,
    ) -> i32;

    fn opus_decoder_create(fs: i32, channels: i32, error: *mut i32) -> *mut OpusDecoder;

    fn opus_decoder_destroy(st: *mut OpusDecoder);

    fn opus_decode_float(
        st: *mut OpusDecoder,
        data: *const u8,
        len: i32,
        pcm: *mut f32,
        frame_size: i32,
        decode_fec: i32,
    ) -> i32;

    fn opus_strerror(error: i32) -> *const std::os::raw::c_char;
}

/// Convert a libopus error code to an owned `String` via `opus_strerror`.
fn opus_err_string(code: i32) -> String {
    // Safety: `opus_strerror` always returns a valid, null-terminated,
    // static C string. We immediately copy it into an owned String so no
    // lifetime issue arises.
    unsafe {
        let ptr = opus_strerror(code);
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("opus error: {0}")]
    Opus(String),
    #[error("invalid frame size: expected {expected} samples, got {got}")]
    BadFrameSize { expected: usize, got: usize },
}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Private raw encoder wrapper (owns the pointer, calls destroy on drop)
// ---------------------------------------------------------------------------

struct OpusEncoderRaw(*mut OpusEncoder);

impl Drop for OpusEncoderRaw {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // Safety: pointer was returned by opus_encoder_create and has not
            // been freed yet. We own it exclusively.
            unsafe { opus_encoder_destroy(self.0) };
        }
    }
}

// ---------------------------------------------------------------------------
// VoiceEncoder — public API
// ---------------------------------------------------------------------------

/// Opus voice encoder configured for 48 kHz mono, 20 ms frames, VoIP
/// application, 24 kbit/s bitrate.
pub struct VoiceEncoder {
    raw: OpusEncoderRaw,
}

impl VoiceEncoder {
    /// Construct a new encoder. Errors if libopus rejects the parameters
    /// (shouldn't happen with the constants we use).
    pub fn new() -> Result<Self> {
        let mut err: i32 = OPUS_OK;
        // Safety: all arguments are valid constants; `err` is a local
        // out-parameter. The returned pointer is non-null on success.
        let ptr = unsafe {
            opus_encoder_create(
                SAMPLE_RATE as i32,
                CHANNELS as i32,
                OPUS_APPLICATION_VOIP,
                &mut err,
            )
        };

        if ptr.is_null() || err != OPUS_OK {
            return Err(Error::Opus(format!(
                "encoder create: {}",
                opus_err_string(err)
            )));
        }

        // Set bitrate to 24 000 bit/s.
        // Safety: `ptr` is a valid, non-null encoder state.
        let rc = unsafe { opus_encoder_ctl(ptr, OPUS_SET_BITRATE_REQUEST, 24_000) };
        if rc != OPUS_OK {
            // Destroy the encoder before returning the error.
            unsafe { opus_encoder_destroy(ptr) };
            return Err(Error::Opus(format!("set_bitrate: {}", opus_err_string(rc))));
        }

        Ok(Self {
            raw: OpusEncoderRaw(ptr),
        })
    }

    /// Encode exactly one 20 ms frame (960 samples mono float). Returns
    /// the variable-length Opus packet bytes.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(Error::BadFrameSize {
                expected: FRAME_SAMPLES,
                got: pcm.len(),
            });
        }

        // 1500 bytes is well above what 24 kbit/s ever produces per 20 ms frame.
        let mut out = vec![0u8; 1500];
        // Safety: `self.raw.0` is non-null (we checked on construction and
        // Drop handles cleanup). `pcm` and `out` are valid slices for the
        // duration of this call.
        let n = unsafe {
            opus_encode_float(
                self.raw.0,
                pcm.as_ptr(),
                FRAME_SAMPLES as i32,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };

        if n < 0 {
            return Err(Error::Opus(format!("encode_float: {}", opus_err_string(n))));
        }

        out.truncate(n as usize);
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Private raw decoder wrapper (owns the pointer, calls destroy on drop)
// ---------------------------------------------------------------------------

struct OpusDecoderRaw(*mut OpusDecoder);

impl Drop for OpusDecoderRaw {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // Safety: pointer was returned by opus_decoder_create and has not
            // been freed yet. We own it exclusively.
            unsafe { opus_decoder_destroy(self.0) };
        }
    }
}

// ---------------------------------------------------------------------------
// VoiceDecoder — public API
// ---------------------------------------------------------------------------

/// Opus voice decoder configured for 48 kHz mono.
pub struct VoiceDecoder {
    raw: OpusDecoderRaw,
}

impl VoiceDecoder {
    /// Construct a new decoder. Errors if libopus rejects the parameters
    /// (shouldn't happen with the constants we use).
    pub fn new() -> Result<Self> {
        let mut err: i32 = OPUS_OK;
        // Safety: all arguments are valid constants; `err` is a local
        // out-parameter. The returned pointer is non-null on success.
        let ptr = unsafe { opus_decoder_create(SAMPLE_RATE as i32, CHANNELS as i32, &mut err) };

        if ptr.is_null() || err != OPUS_OK {
            return Err(Error::Opus(format!(
                "decoder create: {}",
                opus_err_string(err)
            )));
        }

        Ok(Self {
            raw: OpusDecoderRaw(ptr),
        })
    }

    /// Decode one Opus packet. Returns exactly 960 samples of mono float
    /// PCM (one 20 ms frame). The `decode_fec` flag is 0 — we don't use
    /// forward error correction in C2a.
    ///
    /// An empty `opus_bytes` slice is rejected immediately — callers should
    /// never pass a zero-length packet; a lost packet should be handled at a
    /// higher layer (e.g. by not calling decode at all for that frame slot).
    pub fn decode(&mut self, opus_bytes: &[u8]) -> Result<Vec<f32>> {
        if opus_bytes.is_empty() {
            return Err(Error::Opus("decode_float: empty packet".to_owned()));
        }

        let mut out = vec![0.0_f32; FRAME_SAMPLES];
        // Safety: `self.raw.0` is non-null (checked on construction; Drop
        // handles cleanup). `opus_bytes` is non-empty (checked above) and
        // `out` are valid slices for the duration of this call.
        let n = unsafe {
            opus_decode_float(
                self.raw.0,
                opus_bytes.as_ptr(),
                opus_bytes.len() as i32,
                out.as_mut_ptr(),
                FRAME_SAMPLES as i32,
                0, // decode_fec = false
            )
        };

        if n < 0 {
            return Err(Error::Opus(format!("decode_float: {}", opus_err_string(n))));
        }

        out.truncate(n as usize);
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_constructs() {
        let enc = VoiceEncoder::new();
        assert!(enc.is_ok(), "encoder construction should succeed");
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
    fn encode_silence_produces_short_packet() {
        let mut enc = VoiceEncoder::new().unwrap();
        let bytes = enc.encode(&[0.0_f32; FRAME_SAMPLES]).unwrap();
        // Opus encodes silence very compactly (often <10 bytes).
        assert!(
            bytes.len() < 100,
            "silence packet should be small, got {} bytes",
            bytes.len()
        );
        assert!(!bytes.is_empty(), "packet should never be empty");
    }

    #[test]
    fn decoder_constructs() {
        assert!(VoiceDecoder::new().is_ok());
    }

    #[test]
    fn decode_empty_packet_errors() {
        let mut dec = VoiceDecoder::new().unwrap();
        let err = dec.decode(&[]);
        assert!(
            matches!(err, Err(Error::Opus(_))),
            "empty packet should produce an Opus error, got {err:?}",
        );
    }

    #[test]
    fn round_trip_preserves_silence() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        let silence = vec![0.0_f32; FRAME_SAMPLES];
        let bytes = enc.encode(&silence).unwrap();
        let decoded = dec.decode(&bytes).unwrap();
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        let max_abs = decoded.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
        assert!(
            max_abs < 1e-3,
            "silence should decode close to zero; max |sample| = {max_abs}",
        );
    }

    #[test]
    fn round_trip_preserves_sine_energy() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        // 440 Hz at 48 kHz, amplitude 0.5. Run several frames so
        // libopus has time to ramp up its internal state — Opus
        // typically suppresses the very first frame's transient.
        let mut decoded_rms = 0.0_f64;
        for _frame in 0..5 {
            let mut input = vec![0.0_f32; FRAME_SAMPLES];
            for (i, s) in input.iter_mut().enumerate() {
                let t = i as f32 / SAMPLE_RATE as f32;
                *s = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            }
            let bytes = enc.encode(&input).unwrap();
            let decoded = dec.decode(&bytes).unwrap();
            decoded_rms = (decoded.iter().map(|s| (*s as f64).powi(2)).sum::<f64>()
                / decoded.len() as f64)
                .sqrt();
        }
        // Input RMS is 0.5 / sqrt(2) ≈ 0.354. Decoded should be within
        // 20% — Opus is lossy but a steady sine in the speech band is
        // preserved well.
        let expected = 0.5_f64 / 2.0_f64.sqrt();
        let ratio = decoded_rms / expected;
        assert!(
            (0.8..=1.2).contains(&ratio),
            "decoded RMS {decoded_rms} (expected ≈ {expected}, ratio {ratio}) outside ±20%",
        );
    }

    #[test]
    fn sequential_frames_decode_independently() {
        let mut enc = VoiceEncoder::new().unwrap();
        let mut dec = VoiceDecoder::new().unwrap();
        // Encode three different sine wave frames at different
        // frequencies. Each decode should produce a non-empty,
        // 960-sample output, with energy in roughly the right range.
        // We don't assert exact spectrum — that would be flaky. Just
        // that the decoder keeps producing valid frames in sequence.
        for freq in [220.0_f32, 440.0, 880.0] {
            let mut input = vec![0.0_f32; FRAME_SAMPLES];
            for (i, s) in input.iter_mut().enumerate() {
                let t = i as f32 / SAMPLE_RATE as f32;
                *s = 0.3 * (2.0 * std::f32::consts::PI * freq * t).sin();
            }
            let bytes = enc.encode(&input).unwrap();
            let decoded = dec.decode(&bytes).unwrap();
            assert_eq!(decoded.len(), FRAME_SAMPLES);
            let max_abs = decoded.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
            assert!(
                max_abs > 0.05,
                "decoded frame at {freq} Hz should have audible energy",
            );
        }
    }
}
