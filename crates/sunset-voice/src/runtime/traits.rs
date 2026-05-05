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

/// Sink for codec-encoded voice frames the runtime hands out at the
/// jitter pump cadence. The runtime is codec-agnostic — it forwards
/// `(payload, codec_id)` tuples opaquely; decoding to PCM is the host's
/// responsibility.
///
/// `codec_id` matches the `VoicePacket::Frame.codec_id` from the wire
/// (e.g. `"opus"` for WebCodecs Opus, `"pcm-f32-le"` for the
/// uncompressed fallback). Hosts should be tolerant of unknown
/// codec IDs (treat as drop / skip rather than panic).
pub trait FrameSink {
    fn deliver(&self, peer: &PeerId, payload: &[u8], codec_id: &str);
    /// Peer transitioned from in-call to gone. Host should release
    /// per-peer playback resources (worklet node, gain node, decoder, etc.).
    fn drop_peer(&self, peer: &PeerId);
}

/// Sink for `VoicePeerState` change events. Called once per peer per
/// state transition (debounced — the runtime suppresses no-op repeats).
pub trait PeerStateSink {
    fn emit(&self, state: &VoicePeerState);
}
