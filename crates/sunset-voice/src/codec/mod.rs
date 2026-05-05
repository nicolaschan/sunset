//! Opus codec wrapping vendored libopus 1.5.2.
//!
//! Float-API encoder + decoder driven by `VoiceEncoder` /
//! `VoiceDecoder` from the parent module. Frame size is fixed at
//! `FRAME_SAMPLES` (20 ms at 48 kHz mono) — that's the only cadence
//! the audio worklets ever produce, so we don't need
//! variable-frame-size code paths.

#![allow(unsafe_code)]

mod ffi;

use ffi::{
    OPUS_APPLICATION_VOIP, OPUS_OK, OPUS_SET_BITRATE_REQUEST, OPUS_SET_INBAND_FEC_REQUEST,
    OPUS_SET_PACKET_LOSS_PERC_REQUEST, OpusDecoder, OpusEncoder, opus_decode_float,
    opus_decoder_create, opus_decoder_destroy, opus_encode_float, opus_encoder_create,
    opus_encoder_ctl, opus_encoder_destroy,
};

use crate::{Error, FRAME_SAMPLES, Result, SAMPLE_RATE};

/// VoIP-targeted Opus bitrate. 24 kbps is the common point in the
/// "good enough for speech, won't drop on a wifi hop" band — Opus's
/// own VoIP recommendations sit at 16–32 kbps for narrowband to
/// fullband mono speech.
const TARGET_BITRATE_BPS: i32 = 24_000;

/// Upper bound on encoded packet size, per libopus's documented
/// "always large enough" ceiling for one Opus packet at any
/// configuration. Used as the encode-output buffer length.
const MAX_OPUS_PACKET_BYTES: usize = 4000;

/// Encoder for one peer's outgoing audio stream. libopus encoders are
/// stateful — each frame's encoded output depends on prior frames'
/// internal state — so this struct owns its `OpusEncoder` for its
/// whole lifetime.
pub struct OpusFrameEncoder {
    encoder: *mut OpusEncoder,
}

fn opus_err(code: i32) -> Error {
    Error::Codec(format!("opus error {}", code))
}

impl OpusFrameEncoder {
    pub fn new() -> Result<Self> {
        let mut err: i32 = 0;
        // SAFETY: opus_encoder_create returns a heap-allocated
        // OpusEncoder owned by the caller. We forward the error code
        // out of the &mut and check it before using the pointer.
        let encoder =
            unsafe { opus_encoder_create(SAMPLE_RATE as i32, 1, OPUS_APPLICATION_VOIP, &mut err) };
        if err != OPUS_OK || encoder.is_null() {
            return Err(opus_err(err));
        }
        // Configure target bitrate and enable inband FEC so a single
        // dropped packet is recoverable from the next packet.
        // SAFETY: encoder is a valid heap-allocated `*mut OpusEncoder`
        // we just constructed; opus_encoder_ctl reads/writes only its
        // internal state and the variadic int we pass.
        unsafe {
            let rc = opus_encoder_ctl(encoder, OPUS_SET_BITRATE_REQUEST, TARGET_BITRATE_BPS);
            if rc != OPUS_OK {
                opus_encoder_destroy(encoder);
                return Err(opus_err(rc));
            }
            let rc = opus_encoder_ctl(encoder, OPUS_SET_INBAND_FEC_REQUEST, 1);
            if rc != OPUS_OK {
                opus_encoder_destroy(encoder);
                return Err(opus_err(rc));
            }
            // Tell Opus to plan for ~5% packet loss; this nudges its
            // FEC reservation up a touch but stays well under the
            // bitrate ceiling.
            let rc = opus_encoder_ctl(encoder, OPUS_SET_PACKET_LOSS_PERC_REQUEST, 5);
            if rc != OPUS_OK {
                opus_encoder_destroy(encoder);
                return Err(opus_err(rc));
            }
        }
        Ok(Self { encoder })
    }

    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(Error::BadFrameSize {
                expected: FRAME_SAMPLES,
                got: pcm.len(),
            });
        }
        let mut out = vec![0u8; MAX_OPUS_PACKET_BYTES];
        // SAFETY: `self.encoder` is the libopus state we own;
        // `pcm`/`out` are valid slices we hand directly to the
        // C function.
        let written = unsafe {
            opus_encode_float(
                self.encoder,
                pcm.as_ptr(),
                FRAME_SAMPLES as i32,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        if written < 0 {
            return Err(opus_err(written));
        }
        out.truncate(written as usize);
        Ok(out)
    }
}

impl Drop for OpusFrameEncoder {
    fn drop(&mut self) {
        if !self.encoder.is_null() {
            // SAFETY: pointer originated from opus_encoder_create and
            // we never alias or free it elsewhere.
            unsafe { opus_encoder_destroy(self.encoder) };
        }
    }
}

pub struct OpusFrameDecoder {
    decoder: *mut OpusDecoder,
}

impl OpusFrameDecoder {
    pub fn new() -> Result<Self> {
        let mut err: i32 = 0;
        // SAFETY: see OpusFrameEncoder::new.
        let decoder = unsafe { opus_decoder_create(SAMPLE_RATE as i32, 1, &mut err) };
        if err != OPUS_OK || decoder.is_null() {
            return Err(opus_err(err));
        }
        Ok(Self { decoder })
    }

    pub fn decode(&mut self, encoded: &[u8]) -> Result<Vec<f32>> {
        if encoded.is_empty() {
            return Err(Error::EmptyEncoded);
        }
        let mut out = vec![0f32; FRAME_SAMPLES];
        // SAFETY: same as encode — owned decoder, valid slices.
        let samples = unsafe {
            opus_decode_float(
                self.decoder,
                encoded.as_ptr(),
                encoded.len() as i32,
                out.as_mut_ptr(),
                FRAME_SAMPLES as i32,
                0,
            )
        };
        if samples < 0 {
            return Err(opus_err(samples));
        }
        out.truncate(samples as usize);
        Ok(out)
    }
}

impl Drop for OpusFrameDecoder {
    fn drop(&mut self) {
        if !self.decoder.is_null() {
            // SAFETY: pointer originated from opus_decoder_create and
            // we never alias or free it elsewhere.
            unsafe { opus_decoder_destroy(self.decoder) };
        }
    }
}

// libopus encoder/decoder state is opaque heap memory we own
// exclusively for the lifetime of these structs; the C library
// neither aliases nor mutates it from anywhere else, so it's safe to
// move between threads. (This is also how the upstream `opus` /
// `audiopus` Rust bindings declare their wrappers.)
unsafe impl Send for OpusFrameEncoder {}
unsafe impl Send for OpusFrameDecoder {}

#[cfg(target_arch = "wasm32")]
mod wasm_runtime;
