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
        let rc =
            unsafe { opus_encoder_ctl(ptr, OPUS_SET_BITRATE_REQUEST, 24_000) };
        if rc != OPUS_OK {
            // Destroy the encoder before returning the error.
            unsafe { opus_encoder_destroy(ptr) };
            return Err(Error::Opus(format!(
                "set_bitrate: {}",
                opus_err_string(rc)
            )));
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
            return Err(Error::Opus(format!(
                "encode_float: {}",
                opus_err_string(n)
            )));
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
            Err(Error::BadFrameSize { expected: 960, got: 480 })
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
}
