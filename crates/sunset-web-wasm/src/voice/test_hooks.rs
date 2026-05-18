//! Test-only frame recorder. Compiled in only with `feature = "test-hooks"`.
//!
//! `RecordingFrameSink` wraps an existing `FrameSink` and records every
//! frame delivered per-peer into a ring buffer. Used by Playwright
//! tests to assert content-level correctness (frame count, ordering,
//! checksums) without touching production code paths.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::rc::Rc;

use sha2::Digest;
use sunset_sync::PeerId;
use sunset_voice::FrameSink;

const RING_PER_PEER: usize = 1024;

/// A single recorded frame: length in samples, SHA-256 checksum
/// (hex) of the raw f32 bytes, root-mean-square amplitude, the
/// wire sequence number (low 32 bits of `VoicePacket::Frame::seq`),
/// and a 440 Hz tone-purity ratio.
///
/// `checksum` is a stuck-frame tripwire — distinct decoded frames
/// land at distinct checksums even after Opus, so consecutive
/// identical checksums catch stuttering. It also doubles as a
/// "frame is not silence" signal: zero-PCM has a known checksum.
///
/// `rms` is a "real-audio-vs-silence" signal: an Opus-decoded sine
/// wave at amplitude 0.5 lands around RMS 0.35, while silence lands
/// at 0.
///
/// `tone_purity_440` is a "is this still a clean 440 Hz tone?" signal:
/// it's the ratio `signal_energy_at_440Hz / total_energy` measured on
/// the left channel via a phase-invariant sinusoid least-squares fit.
/// 1.0 = pure 440 Hz sine, 0.0 = no energy at 440 Hz. Clean Opus
/// round-trip of `synth_pcm_with_counter` lands above 0.95 at Maximum
/// quality; amplitude-modulated or otherwise broken-up audio (e.g.
/// the receiver of a peer who's emitting interleaved silence + tone
/// from leaked capture worklets) lands far lower. Both 440 Hz
/// reference signals available to the e2e suite — the Rust
/// `synth_pcm_with_counter` and the real-mic `sweep.wav` — are
/// continuous 440 Hz sines, so this single fixed-frequency probe
/// covers both fixtures.
#[derive(Clone)]
pub struct RecordedFrame {
    pub len: u32,
    pub checksum: String,
    pub rms: f32,
    pub seq: u32,
    pub tone_purity_440: f32,
}

struct Inner {
    frames: HashMap<PeerId, VecDeque<RecordedFrame>>,
}

/// `FrameSink` wrapper that records every `(peer, pcm)` pair.
pub struct RecordingFrameSink {
    inner: RefCell<Inner>,
    forward: Rc<dyn FrameSink>,
}

impl RecordingFrameSink {
    pub fn new(forward: Rc<dyn FrameSink>) -> Self {
        Self {
            inner: RefCell::new(Inner {
                frames: HashMap::new(),
            }),
            forward,
        }
    }

    /// Return a snapshot of recorded frames for `peer` (oldest first).
    pub fn get_frames(&self, peer: &PeerId) -> Vec<RecordedFrame> {
        self.inner
            .borrow()
            .frames
            .get(peer)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// 440 Hz tone-purity ratio for one channel of decoded PCM. See the
/// `RecordedFrame::tone_purity_440` docs for the metric definition.
fn tone_purity_440_left_channel(stereo_interleaved: &[f32]) -> f32 {
    // Decoder always emits stereo interleaved (L R L R …). For a
    // sine-wave fixture the two channels are identical, so probing
    // just the left channel is enough and halves the cost.
    const FREQ_HZ: f32 = 440.0;
    let sr = sunset_voice::SAMPLE_RATE as f32;
    let omega = 2.0 * core::f32::consts::PI * FREQ_HZ / sr;
    let mut n: f32 = 0.0;
    let mut a = 0.0_f32;
    let mut b = 0.0_f32;
    let mut total = 0.0_f32;
    for (idx, sample) in stereo_interleaved.iter().step_by(2).enumerate() {
        let phase = omega * idx as f32;
        a += sample * phase.cos();
        b += sample * phase.sin();
        total += sample * sample;
        n += 1.0;
    }
    if n == 0.0 || total <= f32::EPSILON {
        return 0.0;
    }
    a *= 2.0 / n;
    b *= 2.0 / n;
    let signal_energy = (n / 2.0) * (a * a + b * b);
    (signal_energy / total).clamp(0.0, 1.0)
}

impl FrameSink for RecordingFrameSink {
    fn deliver(&self, peer: &PeerId, seq: u32, pcm: &[f32]) {
        let mut hasher = sha2::Sha256::new();
        for s in pcm {
            hasher.update(s.to_le_bytes());
        }
        let checksum = hex::encode(hasher.finalize());
        let sum_sq: f32 = pcm.iter().map(|s| s * s).sum();
        let rms = (sum_sq / pcm.len().max(1) as f32).sqrt();
        let tone_purity_440 = tone_purity_440_left_channel(pcm);
        let frame = RecordedFrame {
            len: pcm.len() as u32,
            checksum,
            rms,
            seq,
            tone_purity_440,
        };
        let mut inner = self.inner.borrow_mut();
        let q = inner.frames.entry(peer.clone()).or_default();
        if q.len() >= RING_PER_PEER {
            q.pop_front();
        }
        q.push_back(frame);
        drop(inner);
        self.forward.deliver(peer, seq, pcm);
    }

    fn drop_peer(&self, peer: &PeerId) {
        self.forward.drop_peer(peer);
    }
}

/// Generate one 20 ms PCM frame of continuous 440 Hz sine at
/// amplitude 0.5. `counter` advances the phase by exactly one frame
/// so concatenated frames sound like one continuous tone — which is
/// what an Opus encoder is built to compress efficiently and
/// reproduce faithfully.
///
/// (Pre-Opus this function packed `counter` into `pcm[0]` so the
/// per-frame checksum was deterministic for the test recorder. With
/// a lossy codec that approach is unsound — Opus does not preserve
/// individual sample values — so callers identify frames by the
/// recorder's per-peer ordering and `checksum`-distinctness signal
/// instead of by an embedded counter.)
pub fn synth_pcm_with_counter(counter: i32) -> Vec<f32> {
    const FREQ_HZ: f32 = 440.0;
    let sr = sunset_voice::SAMPLE_RATE as f32;
    let per_channel = sunset_voice::FRAME_SAMPLES_PER_CHANNEL;
    let channels = sunset_voice::PLAYBACK_CHANNELS as usize;
    let frame_offset = (counter as i64).wrapping_mul(per_channel as i64);
    let mut out = vec![0.0_f32; per_channel * channels];
    for i in 0..per_channel {
        let n = frame_offset.wrapping_add(i as i64);
        let t = n as f32 / sr;
        let s = 0.5 * (2.0 * core::f32::consts::PI * FREQ_HZ * t).sin();
        // Interleaved L/R; both channels carry the same tone.
        for c in 0..channels {
            out[i * channels + c] = s;
        }
    }
    out
}
