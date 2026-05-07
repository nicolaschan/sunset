//! RNNoise-based receiver-side denoiser.
//!
//! Wraps `nnnoiseless::DenoiseState` so callers can hand it a sunset-voice
//! decoded frame (`FRAME_SAMPLES_PER_CHANNEL * 2` = 1920 samples = 20 ms
//! at 48 kHz, **stereo interleaved** L/R, `f32` PCM in `[-1.0, 1.0]`)
//! and get a denoised frame back in the same shape. The decoder always
//! produces stereo regardless of which preset the sender used (mono
//! senders are upmixed at decode time), so the denoiser only ever sees
//! 1920-sample stereo frames.
//!
//! Three impedance mismatches are handled here so the rest of the
//! pipeline doesn't see them:
//!
//! - **Sample scale.** RNNoise was trained on 16-bit PCM and expects
//!   `f32` samples in `[-32768.0, 32767.0]`, not `[-1.0, 1.0]`. We scale
//!   on the way in and the inverse on the way out.
//! - **Frame-size split.** RNNoise's native frame is 480 samples (10 ms);
//!   our per-channel frame is 960. Each channel is fed to its own
//!   `DenoiseState` as two ordered sub-frames per voice frame. The model
//!   is stateful (sub-frame N+1 sees activations from sub-frame N) so
//!   the halves must be fed in order with the same state.
//! - **Stereo de-interleave / re-interleave.** RNNoise is mono-only; we
//!   keep two `DenoiseState`s per peer (one per channel) so each side
//!   tunes to its own noise profile and we don't lose stereo separation.
//!
//! `Denoiser` is per-stream: hold one per remote peer so each sender's
//! voice state stays separate.

use nnnoiseless::DenoiseState;

use crate::FRAME_SAMPLES_PER_CHANNEL;

/// Number of samples per RNNoise sub-frame. 480 samples = 10 ms at 48 kHz.
const RNNOISE_FRAME: usize = DenoiseState::FRAME_SIZE;

/// Channel count of frames the denoiser accepts. Always stereo because
/// the receiver-side decoder is fixed at 2-channel output.
const DENOISE_CHANNELS: usize = 2;

/// Stereo interleaved frame length the denoiser accepts.
const DENOISE_FRAME_SAMPLES: usize = FRAME_SAMPLES_PER_CHANNEL * DENOISE_CHANNELS;

const _: () = assert!(
    FRAME_SAMPLES_PER_CHANNEL % RNNOISE_FRAME == 0,
    "per-channel frame must split evenly into RNNoise sub-frames",
);

/// Stateful per-stream denoiser. Construct one per remote peer.
///
/// Owns one `DenoiseState` per channel — RNNoise is mono-only and its
/// predictor is stateful, so giving each channel its own state preserves
/// stereo separation and stops cross-channel artifacts.
pub struct Denoiser {
    left: Box<DenoiseState<'static>>,
    right: Box<DenoiseState<'static>>,
}

impl Denoiser {
    /// Allocate a fresh denoiser. Internally heap-allocates two model
    /// states (the struct is large; `nnnoiseless` recommends boxing).
    pub fn start() -> Self {
        Self {
            left: DenoiseState::new(),
            right: DenoiseState::new(),
        }
    }

    /// Denoise one stereo interleaved voice frame in place.
    ///
    /// `pcm` must be exactly `FRAME_SAMPLES_PER_CHANNEL * 2` samples
    /// long, interleaved L/R, in `[-1.0, 1.0]`. Returns
    /// `Err(BadFrameSize)` otherwise — callers in the runtime drop the
    /// frame on size mismatch, matching the encoder/decoder convention.
    pub fn denoise_in_place(&mut self, pcm: &mut [f32]) -> crate::Result<()> {
        if pcm.len() != DENOISE_FRAME_SAMPLES {
            return Err(crate::Error::BadFrameSize {
                expected: DENOISE_FRAME_SAMPLES,
                got: pcm.len(),
            });
        }
        // De-interleave into per-channel scratch buffers, denoise each
        // half-frame, then re-interleave. The per-channel buffer is
        // sized to one full per-channel frame so we can run both
        // 10 ms sub-frames through the model under a single de/interleave.
        let mut left_buf = [0.0_f32; FRAME_SAMPLES_PER_CHANNEL];
        let mut right_buf = [0.0_f32; FRAME_SAMPLES_PER_CHANNEL];
        for (i, frame) in pcm.chunks_exact(DENOISE_CHANNELS).enumerate() {
            left_buf[i] = frame[0];
            right_buf[i] = frame[1];
        }
        process_channel(&mut self.left, &mut left_buf);
        process_channel(&mut self.right, &mut right_buf);
        for (i, frame) in pcm.chunks_exact_mut(DENOISE_CHANNELS).enumerate() {
            frame[0] = left_buf[i];
            frame[1] = right_buf[i];
        }
        Ok(())
    }
}

/// Run one channel's worth of samples (`FRAME_SAMPLES_PER_CHANNEL`)
/// through `state` in 10 ms sub-frames, in place. Handles the i16 scale
/// conversion at the boundary so the buffer is left in `[-1.0, 1.0]`.
fn process_channel(state: &mut DenoiseState<'static>, channel: &mut [f32]) {
    let mut scratch = [0.0_f32; RNNOISE_FRAME];
    for chunk in channel.chunks_exact_mut(RNNOISE_FRAME) {
        for (dst, src) in scratch.iter_mut().zip(chunk.iter()) {
            *dst = src * I16_MAX_F32;
        }
        state.process_frame(chunk, &scratch);
        for s in chunk.iter_mut() {
            *s *= I16_MAX_F32_INV;
        }
    }
}

/// Scale `[-1.0, 1.0]` → `[-32768.0, 32767.0]`.
const I16_MAX_F32: f32 = i16::MAX as f32;
/// Inverse of `I16_MAX_F32`.
const I16_MAX_F32_INV: f32 = 1.0 / I16_MAX_F32;

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        let sum: f32 = samples.iter().map(|s| s * s).sum();
        (sum / samples.len() as f32).sqrt()
    }

    /// Stereo-interleaved frame (1920 samples = 960 L + 960 R).
    fn stereo_frame(fill: impl Fn(usize) -> f32) -> Vec<f32> {
        (0..DENOISE_FRAME_SAMPLES).map(fill).collect()
    }

    #[test]
    fn rejects_wrong_frame_size() {
        let mut d = Denoiser::start();
        let mut wrong = vec![0.0_f32; DENOISE_FRAME_SAMPLES + 2];
        let err = d.denoise_in_place(&mut wrong).unwrap_err();
        assert!(matches!(
            err,
            crate::Error::BadFrameSize {
                expected: DENOISE_FRAME_SAMPLES,
                got: _,
            }
        ));
    }

    #[test]
    fn rejects_mono_sized_frame() {
        // A previous mono frame size (960) must now be rejected. Caller
        // bug if anyone tries to feed a mono frame to this stage.
        let mut d = Denoiser::start();
        let mut mono = vec![0.0_f32; FRAME_SAMPLES_PER_CHANNEL];
        let err = d.denoise_in_place(&mut mono).unwrap_err();
        assert!(matches!(err, crate::Error::BadFrameSize { .. }));
    }

    #[test]
    fn silence_stays_silent() {
        let mut d = Denoiser::start();
        let mut frame = stereo_frame(|_| 0.0);
        // A few frames to step past RNNoise's fade-in.
        for _ in 0..10 {
            d.denoise_in_place(&mut frame).unwrap();
        }
        assert!(rms(&frame) < 1e-3);
    }

    #[test]
    fn attenuates_white_noise() {
        let mut d = Denoiser::start();
        // Deterministic pseudo-random noise at amplitude 0.05 (well clear
        // of clipping after i16 scaling). Run for ~400 ms — RNNoise needs
        // a handful of frames before its gain decisions stabilize.
        let mut seed: u32 = 0xC0FFEE;
        let mut input_rms_sum = 0.0_f32;
        let mut output_rms_sum = 0.0_f32;
        let mut measured_frames = 0;
        for frame_idx in 0..20 {
            let mut frame = vec![0.0_f32; DENOISE_FRAME_SAMPLES];
            for s in frame.iter_mut() {
                // Lehmer LCG — small, deterministic.
                seed = seed.wrapping_mul(48271) % 0x7FFFFFFF;
                let n = (seed as f32 / 0x7FFFFFFF as f32) * 2.0 - 1.0;
                *s = n * 0.05;
            }
            let in_rms = rms(&frame);
            d.denoise_in_place(&mut frame).unwrap();
            let out_rms = rms(&frame);
            // Skip the first 5 frames — fade-in / RNN warmup.
            if frame_idx >= 5 {
                input_rms_sum += in_rms;
                output_rms_sum += out_rms;
                measured_frames += 1;
            }
        }
        let avg_in = input_rms_sum / measured_frames as f32;
        let avg_out = output_rms_sum / measured_frames as f32;
        // RNNoise should drop white noise by a wide margin once it's
        // steady-state. A real-world UX threshold: at least 6 dB
        // (energy halved); we assert 3× attenuation as a conservative
        // floor that survives small model-output drift.
        assert!(
            avg_out * 3.0 < avg_in,
            "expected white-noise attenuation: avg_in={avg_in}, avg_out={avg_out}",
        );
    }

    #[test]
    fn frame_count_preserved() {
        let mut d = Denoiser::start();
        let mut frame = stereo_frame(|_| 0.1);
        d.denoise_in_place(&mut frame).unwrap();
        assert_eq!(frame.len(), DENOISE_FRAME_SAMPLES);
    }

    /// Each channel must be denoised independently — feeding noise on
    /// L and silence on R should leave R essentially untouched (RNNoise
    /// can only output silence for a silent input). Catches a regression
    /// where we'd accidentally route both channels through one state.
    #[test]
    fn channels_are_independent() {
        let mut d = Denoiser::start();
        // L = noise, R = silence. 0.05 amplitude on L only.
        let mut seed: u32 = 0xBADC0DE;
        for _ in 0..10 {
            let mut frame = vec![0.0_f32; DENOISE_FRAME_SAMPLES];
            for pair in frame.chunks_exact_mut(DENOISE_CHANNELS) {
                seed = seed.wrapping_mul(48271) % 0x7FFFFFFF;
                let n = (seed as f32 / 0x7FFFFFFF as f32) * 2.0 - 1.0;
                pair[0] = n * 0.05;
                pair[1] = 0.0;
            }
            d.denoise_in_place(&mut frame).unwrap();
            // Right channel should still be ~silent regardless of left.
            let right_rms = {
                let r: f32 = frame
                    .chunks_exact(DENOISE_CHANNELS)
                    .map(|p| p[1] * p[1])
                    .sum();
                (r / FRAME_SAMPLES_PER_CHANNEL as f32).sqrt()
            };
            assert!(
                right_rms < 1e-3,
                "silent right channel polluted by noisy left: {right_rms}",
            );
        }
    }
}
