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
/// (hex) of the raw f32 bytes, and root-mean-square amplitude.
///
/// `checksum` is a stuck-frame tripwire — distinct decoded frames
/// land at distinct checksums even after Opus, so consecutive
/// identical checksums catch jitter-pump stuttering. It also doubles
/// as a "frame is not silence" signal: zero-PCM has a known checksum.
///
/// `rms` is a "real-audio-vs-silence" signal: an Opus-decoded sine
/// wave at amplitude 0.5 lands around RMS 0.35, while silence /
/// underrun-padding lands at 0.
#[derive(Clone)]
pub struct RecordedFrame {
    pub len: u32,
    pub checksum: String,
    pub rms: f32,
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

impl FrameSink for RecordingFrameSink {
    fn deliver(&self, peer: &PeerId, pcm: &[f32]) {
        let mut hasher = sha2::Sha256::new();
        for s in pcm {
            hasher.update(s.to_le_bytes());
        }
        let checksum = hex::encode(hasher.finalize());
        let sum_sq: f32 = pcm.iter().map(|s| s * s).sum();
        let rms = (sum_sq / pcm.len().max(1) as f32).sqrt();
        let frame = RecordedFrame {
            len: pcm.len() as u32,
            checksum,
            rms,
        };
        let mut inner = self.inner.borrow_mut();
        let q = inner.frames.entry(peer.clone()).or_default();
        if q.len() >= RING_PER_PEER {
            q.pop_front();
        }
        q.push_back(frame);
        drop(inner);
        self.forward.deliver(peer, pcm);
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
