//! Shared `RuntimeInner` — interior-mutable state every task references
//! through a `Weak`. Dropping the only `Rc<RuntimeInner>` (held by
//! `VoiceRuntime`) lets every task observe the upgrade failure and exit.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use rand_chacha::ChaCha20Rng;

use sunset_core::liveness::Liveness;
use sunset_core::{Identity, Room};
use sunset_sync::PeerId;

use crate::runtime::dyn_bus::DynBus;
use crate::runtime::traits::{Dialer, FrameSink, PeerStateSink, VoicePeerState};
use crate::{Denoiser, VoiceDecoder, VoiceEncoder};

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
    /// Peers the local user has explicitly disabled denoising for.
    /// Denoising is on by default for every peer (absence = enabled);
    /// the popover's per-member toggle inserts/removes entries here
    /// via `VoiceRuntime::set_peer_denoise`. The corresponding entry
    /// in `denoisers` is left intact so flipping back on resumes with
    /// the existing per-peer state instead of starting cold.
    pub denoise_disabled: RefCell<HashSet<PeerId>>,
    /// Per-peer denoiser state. Lazily inserted on first frame from a
    /// peer; entries are kept for the lifetime of the runtime so peers
    /// that briefly disappear and return don't lose their tuning.
    pub denoisers: RefCell<HashMap<PeerId, Denoiser>>,
    /// Per-peer Opus decoder state. Lazily inserted on first frame
    /// from a peer; kept for the runtime's lifetime to match the
    /// `denoisers` semantics (a peer that briefly disappears and
    /// returns resumes with the same decoder state rather than
    /// re-initializing). One decoder *cannot* be shared across peers
    /// because libopus's predictor history, SILK state, and CELT
    /// pitch tracking all assume a single continuous stream — feeding
    /// it interleaved packets from different senders corrupts every
    /// decoded frame on a stream change.
    pub decoders: RefCell<HashMap<PeerId, VoiceDecoder>>,

    pub frame_liveness: Arc<Liveness>,
    pub membership_liveness: Arc<Liveness>,
    /// Tracks "this peer published a fresh durable `voice-presence`
    /// entry recently" — the source of truth for `in_voice_channel`.
    /// Independent of frame/heartbeat liveness because presence
    /// propagates through the sync layer (relay-replicated CRDT) and
    /// reaches us regardless of whether we've established a P2P
    /// connection yet.
    pub voice_presence_liveness: Arc<Liveness>,

    /// Per-peer highest envelope `seq` accepted by the receiver dedup
    /// gate — the high-water mark that drops a frame seen twice during a
    /// direct/relay switchover. Advanced once per accepted frame
    /// (before decode), so it also marks a peer as "seen via frames"
    /// even when the receiver is deafened. The runtime keeps no audio
    /// buffer of its own — the host's playback path absorbs jitter. Read
    /// by test hooks (`observed_voice_peers`).
    pub last_delivered_seq: RefCell<HashMap<PeerId, u64>>,
    pub auto_connect_state: RefCell<HashMap<PeerId, AutoConnectState>>,
    pub last_emitted: RefCell<HashMap<PeerId, EmittedState>>,

    /// `false` ⇒ the runtime is in observer mode: it consumes durable
    /// `voice-presence/...` events (so the UI can render who is in the
    /// channel) but does not publish presence, send heartbeats, or
    /// auto-dial peers. `true` ⇒ the user has joined the call; the
    /// active tasks resume normal operation. Toggled via
    /// `VoiceRuntime::set_active`.
    pub is_active: RefCell<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AutoConnectState {
    /// No heartbeat seen, or peer just transitioned Gone.
    Unknown,
    /// `dialer.ensure_direct` has been called; treat further heartbeats
    /// as no-op for dial purposes.
    Dialing,
}

/// Per-peer source facts the combiner debounces on. Holds only the
/// independent inputs — frame/heartbeat/presence liveness plus the
/// directly-observed talking/muted flags. The observable `VoicePeerState`
/// (in_call, in_voice_channel, talking, is_muted) is a pure function of
/// these, computed by `in_call()` / `in_voice_channel()` / `project()`.
///
/// The three liveness sources are tracked independently because they
/// arrive on separate streams and in any order: a peer can register via
/// frames before any heartbeat (membership has no entry to ever time
/// out), or appear in durable presence before any P2P connection exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct EmittedState {
    pub frame_alive: bool,
    pub membership_alive: bool,
    pub presence_alive: bool,
    pub talking: bool,
    pub is_muted: bool,
}

impl EmittedState {
    fn in_call(&self) -> bool {
        self.frame_alive || self.membership_alive
    }

    fn in_voice_channel(&self) -> bool {
        self.in_call() || self.presence_alive
    }

    pub(crate) fn project(&self, peer: PeerId) -> VoicePeerState {
        VoicePeerState {
            peer,
            in_call: self.in_call(),
            talking: self.talking,
            is_muted: self.is_muted,
            in_voice_channel: self.in_voice_channel(),
        }
    }
}

impl RuntimeInner {
    /// Apply `mutate` to the debounce entry for `peer`, and emit a fresh
    /// `VoicePeerState` iff the observable projection changed.
    ///
    /// The `last_emitted` borrow is dropped before `emit` so a sink
    /// callback may re-enter the runtime without a BorrowMutError.
    pub(crate) fn apply(&self, peer: PeerId, mutate: impl FnOnce(&mut EmittedState)) {
        let state = {
            let mut map = self.last_emitted.borrow_mut();
            let entry = map.entry(peer.clone()).or_default();
            let mut next = *entry;
            mutate(&mut next);
            if next == *entry {
                return;
            }
            *entry = next;
            next.project(peer)
        };
        self.peer_state_sink.emit(&state);
    }
}
