//! Host-supplied trait surface and the event type the runtime emits.

use async_trait::async_trait;

use sunset_sync::PeerId;

/// Per-peer voice state surfaced to the UI. The runtime emits a new
/// `VoicePeerState` whenever any of the three booleans changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoicePeerState {
    pub peer: PeerId,
    /// Heartbeats have arrived recently (or a frame, which implies
    /// the peer is in the call).
    pub in_call: bool,
    /// Frame heard within the last ~1 s.
    pub talking: bool,
    /// Last heartbeat reported `is_muted: true`. Default false until
    /// the first heartbeat lands.
    pub is_muted: bool,
}

/// Idempotent connection-establishment hook. The runtime calls this
/// when it sees a peer's heartbeat for the first time (or after the
/// peer was previously considered Gone). The host should ensure a
/// direct WebRTC connection exists. Repeat calls for an already-
/// connected peer must be cheap — the runtime does not deduplicate.
#[async_trait(?Send)]
pub trait Dialer {
    async fn ensure_direct(&self, peer: PeerId);
}

/// Sink for decoded PCM frames the runtime hands out at the jitter
/// pump cadence. PCM is `FRAME_SAMPLES` (960) f32 mono @ 48 kHz.
pub trait FrameSink {
    fn deliver(&self, peer: &PeerId, pcm: &[f32]);
    /// Peer transitioned from in-call to gone. Host should release
    /// per-peer playback resources (worklet node, gain node, etc.).
    fn drop_peer(&self, peer: &PeerId);
}

/// Sink for `VoicePeerState` change events. Called once per peer per
/// state transition (debounced — the runtime suppresses no-op repeats).
pub trait PeerStateSink {
    fn emit(&self, state: &VoicePeerState);
}
