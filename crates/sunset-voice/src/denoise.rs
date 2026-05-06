//! RNNoise-based receiver-side denoiser.
//!
//! Wraps `nnnoiseless::DenoiseState` so callers can hand it a sunset-voice
//! frame (`FRAME_SAMPLES` = 960 samples = 20 ms at 48 kHz, mono `f32` PCM
//! in `[-1.0, 1.0]`) and get a denoised frame back in the same format.
//! RNNoise's native frame size is 480 samples (10 ms), so each call splits
//! the input into two sub-frames and runs them through the model in order.
//!
//! Two impedance mismatches are handled here so the rest of the pipeline
//! doesn't see them:
//!
//! - **Sample scale.** RNNoise was trained on 16-bit PCM and expects
//!   `f32` samples in `[-32768.0, 32767.0]`, not `[-1.0, 1.0]`. We scale
//!   on the way in and the inverse on the way out.
//! - **Frame-size split.** The model is stateful — sub-frame N+1 sees
//!   the activations from sub-frame N — so the two halves must be fed
//!   in their original order with the same `Denoiser` instance.
//!
//! `Denoiser` is per-stream: hold one per remote peer so each sender's
//! voice state stays separate.

use nnnoiseless::DenoiseState;

use crate::FRAME_SAMPLES;

/// Number of samples per RNNoise sub-frame. 480 samples = 10 ms at 48 kHz.
const RNNOISE_FRAME: usize = DenoiseState::FRAME_SIZE;

const _: () = assert!(
    FRAME_SAMPLES % RNNOISE_FRAME == 0,
    "voice frame must split evenly into RNNoise sub-frames",
);

/// Stateful per-stream denoiser. Construct one per remote peer.
pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
}

impl Denoiser {
    /// Allocate a fresh denoiser. Internally heap-allocates the model
    /// state (the struct is large; `nnnoiseless` recommends boxing).
    pub fn start() -> Self {
        Self {
            state: DenoiseState::new(),
        }
    }

    /// Denoise one voice frame in place.
    ///
    /// `pcm` must be exactly `FRAME_SAMPLES` long; samples in `[-1.0, 1.0]`.
    /// Returns `Err(BadFrameSize)` otherwise — callers in the runtime drop
    /// the frame on size mismatch, matching how the encoder/decoder behave.
    pub fn denoise_in_place(&mut self, pcm: &mut [f32]) -> crate::Result<()> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(crate::Error::BadFrameSize {
                expected: FRAME_SAMPLES,
                got: pcm.len(),
            });
        }
        // Two sub-frames per voice frame. Process in place via a small
        // stack buffer for each half so we don't allocate per-frame.
        let mut scratch = [0.0_f32; RNNOISE_FRAME];
        for chunk in pcm.chunks_exact_mut(RNNOISE_FRAME) {
            for (dst, src) in scratch.iter_mut().zip(chunk.iter()) {
                *dst = src * I16_MAX_F32;
            }
            self.state.process_frame(chunk, &scratch);
            for s in chunk.iter_mut() {
                *s *= I16_MAX_F32_INV;
            }
        }
        Ok(())
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

    #[test]
    fn rejects_wrong_frame_size() {
        let mut d = Denoiser::start();
        let mut wrong = vec![0.0_f32; FRAME_SAMPLES + 1];
        let err = d.denoise_in_place(&mut wrong).unwrap_err();
        assert!(matches!(
            err,
            crate::Error::BadFrameSize {
                expected: FRAME_SAMPLES,
                got: _,
            }
        ));
    }

    #[test]
    fn silence_stays_silent() {
        let mut d = Denoiser::start();
        let mut frame = vec![0.0_f32; FRAME_SAMPLES];
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
            let mut frame = vec![0.0_f32; FRAME_SAMPLES];
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
        let mut frame = vec![0.1_f32; FRAME_SAMPLES];
        d.denoise_in_place(&mut frame).unwrap();
        assert_eq!(frame.len(), FRAME_SAMPLES);
    }
}
