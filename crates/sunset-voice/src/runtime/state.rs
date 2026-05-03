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

use crate::VoiceEncoder;
use crate::runtime::dyn_bus::DynBus;
use crate::runtime::traits::{Dialer, FrameSink, PeerStateSink};

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

    pub frame_liveness: Arc<Liveness>,
    pub membership_liveness: Arc<Liveness>,

    /// Per-peer jitter buffers (`VecDeque<Vec<f32>>`). Used by the
    /// subscribe loop (push) and the jitter pump (pop).
    pub jitter: RefCell<HashMap<PeerId, VecDeque<Vec<f32>>>>,
    pub last_delivered: RefCell<HashMap<PeerId, LastDelivered>>,
    pub auto_connect_state: RefCell<HashMap<PeerId, AutoConnectState>>,
    pub last_emitted: RefCell<HashMap<PeerId, EmittedState>>,

    /// Channel for auto-connect notifications from the subscribe loop.
    pub auto_connect_chan: AutoConnectChan,
}

pub(crate) struct LastDelivered {
    pub pcm: Vec<f32>,
    pub underruns: u32,
}

pub(crate) struct AutoConnectChan {
    pub tx: tokio::sync::mpsc::UnboundedSender<PeerId>,
    pub rx: RefCell<Option<tokio::sync::mpsc::UnboundedReceiver<PeerId>>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AutoConnectState {
    /// No heartbeat seen, or peer just transitioned Gone.
    Unknown,
    /// `dialer.ensure_direct` has been called; treat further heartbeats
    /// as no-op for dial purposes.
    Dialing,
}

/// Shape of the last `VoicePeerState` we emitted for a peer (for debounce).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct EmittedState {
    pub in_call: bool,
    pub talking: bool,
    pub is_muted: bool,
}

impl RuntimeInner {
    /// Record the `is_muted` flag from a heartbeat.
    /// Returns true if is_muted differs from previously stored.
    pub(crate) fn last_emitted_set_muted_seen(&self, peer: PeerId, is_muted: bool) -> bool {
        let mut map = self.last_emitted.borrow_mut();
        let entry = map.entry(peer).or_insert(EmittedState {
            in_call: false,
            talking: false,
            is_muted: false,
        });
        if entry.is_muted != is_muted {
            entry.is_muted = is_muted;
            true
        } else {
            false
        }
    }
}
