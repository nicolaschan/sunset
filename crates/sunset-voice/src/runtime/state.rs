//! Shared `RuntimeInner` — interior-mutable state every task references
//! through a `Weak`. Dropping the only `Rc<RuntimeInner>` (held by
//! `VoiceRuntime`) lets every task observe the upgrade failure and exit.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use rand_chacha::ChaCha20Rng;

use sunset_core::liveness::Liveness;
use sunset_core::{Identity, Room};
use sunset_sync::PeerId;

use crate::runtime::dyn_bus::DynBus;
use crate::runtime::traits::{Dialer, FrameSink, PeerStateSink};
use crate::{Denoiser, VoiceEncoder};

pub(crate) struct RuntimeInner {
    pub identity: Identity,
    pub room: Rc<Room>,
    pub bus: Rc<dyn DynBus>,
    pub dialer: Rc<dyn Dialer>,
    /// Interior-mutable so `test-hooks` can swap in a recording wrapper
    /// via `VoiceRuntime::set_frame_sink` without changing the contract.
    pub frame_sink: RefCell<Rc<dyn FrameSink>>,
    pub peer_state_sink: Rc<dyn PeerStateSink>,

    pub encoder: RefCell<VoiceEncoder>,
    pub seq: RefCell<u64>,
    pub rng: RefCell<ChaCha20Rng>,

    pub muted: RefCell<bool>,
    pub deafened: RefCell<bool>,
    /// Receiver-side RNNoise denoiser toggle. Defaults to true (on).
    /// Toggle via `VoiceRuntime::set_denoise`. When false, `denoisers`
    /// is left intact so flipping back on resumes with the existing
    /// per-peer state instead of starting cold.
    pub denoise: RefCell<bool>,
    /// Per-peer denoiser state. Lazily inserted on first frame from a
    /// peer; entries are kept for the lifetime of the runtime so peers
    /// that briefly disappear and return don't lose their tuning.
    pub denoisers: RefCell<HashMap<PeerId, Denoiser>>,

    pub frame_liveness: Arc<Liveness>,
    pub membership_liveness: Arc<Liveness>,
    /// Tracks "this peer published a fresh durable `voice-presence`
    /// entry recently" — the source of truth for `in_voice_channel`.
    /// Independent of frame/heartbeat liveness because presence
    /// propagates through the sync layer (relay-replicated CRDT) and
    /// reaches us regardless of whether we've established a P2P
    /// connection yet.
    pub voice_presence_liveness: Arc<Liveness>,

    /// Last per-peer wire sequence number delivered to the
    /// `FrameSink`. The runtime keeps no audio buffer of its own —
    /// the host's playback path absorbs jitter. This map is read by
    /// test hooks (`observed_voice_peers`) so a peer remains "seen
    /// via frames" even when nothing else stores their PCM.
    pub last_delivered_seq: RefCell<HashMap<PeerId, u64>>,
    pub auto_connect_state: RefCell<HashMap<PeerId, AutoConnectState>>,
    pub last_emitted: RefCell<HashMap<PeerId, EmittedState>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AutoConnectState {
    /// No heartbeat seen, or peer just transitioned Gone.
    Unknown,
    /// `dialer.ensure_direct` has been called; treat further heartbeats
    /// as no-op for dial purposes.
    Dialing,
}

/// Shape of the last `VoicePeerState` we emitted for a peer (for debounce),
/// plus the underlying liveness flags so the combiner can compute
/// `in_call = frame_alive || membership_alive` deterministically regardless
/// of the order frame/membership Stale events arrive in.
///
/// Without `frame_alive` / `membership_alive` the combiner can't distinguish
/// "I never received a heartbeat from this peer" (membership has no entry,
/// so no Stale event will ever fire) from "membership Stale already fired."
/// In the former case, after a hard departure the frame Stale event would
/// drop `talking` to false but leave `in_call` stuck at true forever.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct EmittedState {
    pub in_call: bool,
    pub talking: bool,
    pub is_muted: bool,
    pub in_voice_channel: bool,
    pub frame_alive: bool,
    pub membership_alive: bool,
    pub presence_alive: bool,
}

impl RuntimeInner {
    /// Record the `is_muted` flag from a heartbeat.
    ///
    /// Returns `Some(state)` with the updated entry when `is_muted` changed
    /// so the caller can emit it directly, or `None` when unchanged.
    /// Returning the state avoids a second borrow of `last_emitted` at the
    /// call site and eliminates the `unwrap()` that was previously needed
    /// to re-fetch the just-inserted entry.
    pub(crate) fn last_emitted_set_muted_seen(
        &self,
        peer: PeerId,
        is_muted: bool,
    ) -> Option<EmittedState> {
        let mut map = self.last_emitted.borrow_mut();
        let entry = map.entry(peer).or_insert(EmittedState {
            in_call: false,
            talking: false,
            is_muted: false,
            in_voice_channel: false,
            frame_alive: false,
            membership_alive: false,
            presence_alive: false,
        });
        if entry.is_muted != is_muted {
            entry.is_muted = is_muted;
            Some(*entry)
        } else {
            None
        }
    }
}
