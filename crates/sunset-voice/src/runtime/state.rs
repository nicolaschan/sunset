//! Shared `RuntimeInner` — interior-mutable state every task references
//! through a `Weak`. Dropping the only `Rc<RuntimeInner>` (held by
//! `VoiceRuntime`) lets every task observe the upgrade failure and exit.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use rand_chacha::ChaCha20Rng;

use sunset_core::liveness::Liveness;
use sunset_core::{Identity, Room};
use sunset_sync::PeerId;

use crate::runtime::dyn_bus::DynBus;
use crate::runtime::traits::{Dialer, FrameSink, PeerStateSink};

/// One queued frame: opaque codec-encoded payload + codec identifier.
/// The runtime never decodes; the host's `FrameSink::deliver` does.
pub(crate) type QueuedFrame = (Vec<u8>, String);

pub(crate) struct RuntimeInner {
    pub identity: Identity,
    pub room: Rc<Room>,
    pub bus: Rc<dyn DynBus>,
    pub dialer: Rc<dyn Dialer>,
    /// Interior-mutable so `test-hooks` can swap in a recording wrapper
    /// via `VoiceRuntime::set_frame_sink` without changing the contract.
    pub frame_sink: RefCell<Rc<dyn FrameSink>>,
    pub peer_state_sink: Rc<dyn PeerStateSink>,

    pub seq: RefCell<u64>,
    pub rng: RefCell<ChaCha20Rng>,

    pub muted: RefCell<bool>,
    pub deafened: RefCell<bool>,

    pub frame_liveness: Arc<Liveness>,
    pub membership_liveness: Arc<Liveness>,

    /// Per-peer jitter buffers of opaque encoded frames. Used by the
    /// subscribe loop (push) and the jitter pump (pop). The runtime
    /// does not decode — `(payload, codec_id)` flows through to
    /// `FrameSink::deliver` unchanged.
    pub jitter: RefCell<HashMap<PeerId, VecDeque<QueuedFrame>>>,
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
    pub frame_alive: bool,
    pub membership_alive: bool,
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
            frame_alive: false,
            membership_alive: false,
        });
        if entry.is_muted != is_muted {
            entry.is_muted = is_muted;
            Some(*entry)
        } else {
            None
        }
    }
}
