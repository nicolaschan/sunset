//! Test-only frame recorder. Compiled in only with `feature = "test-hooks"`.
//!
//! `RecordingFrameSink` wraps an existing `FrameSink` and records every
//! frame delivered per-peer into a ring buffer. Used by Playwright
//! tests to assert content-level correctness (frame count, ordering,
//! checksums) without touching production code paths.
//!
//! The recorder accepts opaque `(payload, codec_id)` tuples (the runtime
//! is codec-agnostic). When `codec_id == "pcm-f32-le"` it decodes the
//! embedded counter from the first sample so tests can correlate
//! sender → receiver. For other codecs (e.g. WebCodecs Opus) the
//! `seq_in_frame` field is left at `-1` and tests should rely on
//! frame count + length instead.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::rc::Rc;

use sha2::Digest;
use sunset_sync::PeerId;
use sunset_voice::FrameSink;

const RING_PER_PEER: usize = 1024;

/// A single recorded frame: embedded counter (decoded from `pcm[0]`
/// when the payload is `pcm-f32-le`, else `-1`), length in bytes of
/// the encoded payload, codec identifier, and SHA-256 checksum (hex)
/// of the payload bytes.
#[derive(Clone)]
pub struct RecordedFrame {
    pub seq_in_frame: i32,
    pub len: u32,
    pub codec_id: String,
    pub checksum: String,
}

struct Inner {
    frames: HashMap<PeerId, VecDeque<RecordedFrame>>,
}

/// `FrameSink` wrapper that records every `(peer, payload, codec_id)` triple.
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
    fn deliver(&self, peer: &PeerId, payload: &[u8], codec_id: &str) {
        let seq = if codec_id == sunset_voice::CODEC_ID {
            decode_counter_from_bytes(payload)
        } else {
            -1
        };
        let mut hasher = sha2::Sha256::new();
        hasher.update(payload);
        let checksum = hex::encode(hasher.finalize());
        let frame = RecordedFrame {
            seq_in_frame: seq,
            len: payload.len() as u32,
            codec_id: codec_id.to_string(),
            checksum,
        };
        let mut inner = self.inner.borrow_mut();
        let q = inner.frames.entry(peer.clone()).or_default();
        if q.len() >= RING_PER_PEER {
            q.pop_front();
        }
        q.push_back(frame);
        drop(inner);
        self.forward.deliver(peer, payload, codec_id);
    }

    fn drop_peer(&self, peer: &PeerId) {
        self.forward.drop_peer(peer);
    }
}

/// Generate a synthetic PCM frame with an embedded counter in `pcm[0]`.
/// `pcm[0] = counter / 1_000_000.0`. Remaining samples follow a
/// deterministic pattern so each counter value produces a unique checksum.
pub fn synth_pcm_with_counter(counter: i32) -> Vec<f32> {
    let mut v = vec![0.0_f32; sunset_voice::FRAME_SAMPLES];
    v[0] = (counter as f32) / 1_000_000.0;
    for i in 1..v.len() {
        v[i] = ((counter.wrapping_add(i as i32) as f32) / 1_000_000.0).sin();
    }
    v
}

/// Decode the counter embedded in `pcm[0]` by `synth_pcm_with_counter`.
pub fn decode_counter(pcm: &[f32]) -> i32 {
    if pcm.is_empty() {
        return -1;
    }
    (pcm[0] * 1_000_000.0).round() as i32
}

fn decode_counter_from_bytes(payload: &[u8]) -> i32 {
    if payload.len() < 4 {
        return -1;
    }
    let arr: [u8; 4] = match payload[0..4].try_into() {
        Ok(a) => a,
        Err(_) => return -1,
    };
    decode_counter(&[f32::from_le_bytes(arr)])
}
