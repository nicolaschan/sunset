//! `PeerSupervisor` — durable connection intents above `SyncEngine`.
//!
//! The supervisor takes a list of `PeerAddr`s the application wants to keep
//! connected, dials them via `engine.add_peer`, watches `EngineEvent::PeerRemoved`,
//! and redials with exponential backoff when a connection drops.
//!
//! See `docs/superpowers/specs/2026-04-29-connection-liveness-and-supervision-design.md`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use sunset_store::Store;
use tokio::sync::{mpsc, oneshot};

use crate::engine::{EngineEvent, SyncEngine};
use crate::error::{Error, Result};
use crate::transport::Transport;
use crate::types::{PeerAddr, PeerId};

/// Exponential backoff with jitter. Defaults: 1 s → 30 s, ×2 per attempt, ±20 %.
#[derive(Clone, Debug)]
pub struct BackoffPolicy {
    pub initial: Duration,
    pub max: Duration,
    pub multiplier: f32,
    pub jitter: f32,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: 0.2,
        }
    }
}

impl BackoffPolicy {
    /// Compute the delay for the `n`-th attempt (0-indexed). Includes
    /// multiplicative jitter `1.0 ± self.jitter` (uniformly sampled).
    pub fn delay(&self, attempt: u32, rng: &mut impl rand_core::RngCore) -> Duration {
        let base = self.initial.as_secs_f64()
            * (self.multiplier as f64).powi(attempt as i32);
        let capped = base.min(self.max.as_secs_f64());
        let jitter_lo = 1.0 - self.jitter as f64;
        let jitter_hi = 1.0 + self.jitter as f64;
        // Use rng.next_u64() / u64::MAX for a uniform [0,1) draw.
        let r = rng.next_u64() as f64 / (u64::MAX as f64 + 1.0);
        let factor = jitter_lo + r * (jitter_hi - jitter_lo);
        Duration::from_secs_f64(capped * factor)
    }
}

/// Per-intent state observed via `snapshot()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentState {
    Connecting,
    Connected,
    Backoff,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct IntentSnapshot {
    pub addr: PeerAddr,
    pub state: IntentState,
    pub peer_id: Option<PeerId>,
    pub attempt: u32,
}

pub(crate) struct IntentEntry {
    pub state: IntentState,
    pub attempt: u32,
    pub peer_id: Option<PeerId>,
    /// Earliest moment the next dial attempt may run. None when not in Backoff.
    pub next_attempt_at: Option<std::time::SystemTime>,
}

pub(crate) struct SupervisorState {
    pub intents: HashMap<PeerAddr, IntentEntry>,
    /// Reverse map: peer_id → addr. Populated when an intent transitions
    /// to Connected; cleared on disconnect.
    pub peer_to_addr: HashMap<PeerId, PeerAddr>,
}

pub(crate) enum SupervisorCommand {
    Add {
        addr: PeerAddr,
        ack: oneshot::Sender<Result<()>>,
    },
    Remove {
        addr: PeerAddr,
        ack: oneshot::Sender<()>,
    },
    Snapshot {
        ack: oneshot::Sender<Vec<IntentSnapshot>>,
    },
}

pub struct PeerSupervisor<S: Store, T: Transport> {
    pub(crate) engine: Rc<SyncEngine<S, T>>,
    pub(crate) cmd_tx: mpsc::UnboundedSender<SupervisorCommand>,
    pub(crate) cmd_rx: RefCell<Option<mpsc::UnboundedReceiver<SupervisorCommand>>>,
    pub(crate) state: Rc<RefCell<SupervisorState>>,
    pub(crate) policy: BackoffPolicy,
}

impl<S, T> PeerSupervisor<S, T>
where
    S: Store + 'static,
    T: Transport + 'static,
    T::Connection: 'static,
{
    pub fn new(engine: Rc<SyncEngine<S, T>>, policy: BackoffPolicy) -> Rc<Self> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        Rc::new(Self {
            engine,
            cmd_tx,
            cmd_rx: RefCell::new(Some(cmd_rx)),
            state: Rc::new(RefCell::new(SupervisorState {
                intents: HashMap::new(),
                peer_to_addr: HashMap::new(),
            })),
            policy,
        })
    }
}
