//! Host-supplied trait surface and the event type the runtime emits.

use async_trait::async_trait;

use sunset_sync::PeerId;

/// Per-peer voice state surfaced to the UI. The runtime emits a new
/// `VoicePeerState` whenever any of the four booleans changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoicePeerState {
    pub peer: PeerId,
    /// Heartbeats (ephemeral, over the P2P channel) or frames have
    /// arrived recently — i.e. the local client is currently
    /// connected to this peer's audio path. Distinct from
    /// `in_voice_channel` (peer announced membership via durable
    /// presence but we may not yet have a connection).
    pub in_call: bool,
    /// Frame heard within the last ~1 s.
    pub talking: bool,
    /// Last heartbeat reported `is_muted: true`. Default false until
    /// the first heartbeat lands.
    pub is_muted: bool,
    /// The peer has a fresh durable `voice-presence/<room_fp>/<peer>`
    /// entry — i.e. they're announcing membership in the voice
    /// channel via the sync layer. Stays true even when no P2P
    /// connection has been established yet (or when one exists,
    /// since presence is republished while in the call). Driven by
    /// the durable presence stream, with TTL slightly longer than
    /// the publisher's republish interval so a single missed
    /// republish doesn't visibly drop the peer from the roster.
    pub in_voice_channel: bool,
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

/// Sink for decoded PCM frames the runtime delivers immediately on
/// network arrival. PCM is `FRAME_SAMPLES_PER_CHANNEL * 2` (1920) f32
/// interleaved L/R stereo @ 48 kHz.
///
/// `seq` is the low 32 bits of `VoicePacket::Frame::seq`, exposed so
/// the host can do sequence-indexed buffering (e.g. a worklet-side
/// jitter buffer) and detect gaps. The runtime itself does no
/// buffering — frames arrive at network cadence and the host is
/// responsible for pacing playback against its audio clock.
pub trait FrameSink {
    fn deliver(&self, peer: &PeerId, seq: u32, pcm: &[f32]);
    /// Peer transitioned from in-call to gone. Host should release
    /// per-peer playback resources (worklet node, gain node, etc.).
    fn drop_peer(&self, peer: &PeerId);
}

/// Sink for `VoicePeerState` change events. Called once per peer per
/// state transition (debounced — the runtime suppresses no-op repeats).
pub trait PeerStateSink {
    fn emit(&self, state: &VoicePeerState);
}
