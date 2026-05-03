//! `PeerSupervisor` — durable connection intents above `SyncEngine`.
//!
//! The supervisor takes a list of `Connectable`s the application wants to keep
//! connected, dials them via `engine.add_peer`, watches `EngineEvent::PeerRemoved`,
//! and redials with exponential backoff when a connection drops.
//!
//! See `docs/superpowers/specs/2026-04-29-connection-liveness-and-supervision-design.md`
//! and `docs/superpowers/specs/2026-05-03-durable-relay-connect-design.md`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use rand_core::SeedableRng;
use sunset_store::Store;
use tokio::sync::{mpsc, oneshot};

use crate::engine::{EngineEvent, SyncEngine};
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
        let base = self.initial.as_secs_f64() * (self.multiplier as f64).powi(attempt as i32);
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

/// Opaque, monotonically-allocated identifier for a registered intent.
/// Stable across reconnect cycles (peer_id may not be known yet on
/// first attempt; the IntentId always is).
pub type IntentId = u64;

#[derive(Clone, Debug)]
pub struct IntentSnapshot {
    pub id: IntentId,
    pub state: IntentState,
    pub peer_id: Option<PeerId>,
    pub kind: Option<crate::transport::TransportKind>,
    pub attempt: u32,
    /// Display label — `Connectable::label()` of the intent that
    /// created this snapshot. Stable across reconnect cycles.
    pub label: String,
    /// Wall-clock ms of the most recent Pong observed from this peer.
    /// `None` until the first Pong of the *first* connection lands.
    /// Preserved across Backoff transitions (the popover should show
    /// "heard from 12s ago" while reconnecting), cleared only when
    /// the intent itself is removed (`SupervisorCommand::Remove`).
    pub last_pong_at_unix_ms: Option<u64>,
    /// Round-trip time of the most recent Pong, in milliseconds.
    /// `None` under the same conditions as `last_pong_at_unix_ms`.
    pub last_rtt_ms: Option<u64>,
}

/// Key used to clean up either dedup map without a second branch on the
/// connectable. Matches the shape of `direct_dedup` / `resolving_dedup`
/// so callers stay readable even with three different cleanup paths
/// (Remove, Parse-in-cmd, Parse-in-dial).
enum DedupKey {
    Direct(PeerAddr),
    Resolving(String),
}

pub(crate) struct IntentEntry {
    pub id: IntentId,
    pub state: IntentState,
    pub attempt: u32,
    pub peer_id: Option<PeerId>,
    pub kind: Option<crate::transport::TransportKind>,
    /// Earliest moment the next dial attempt may run. None when not in Backoff.
    pub next_attempt_at: Option<web_time::SystemTime>,
    /// What to dial. Used by every retry attempt; for `Resolving`,
    /// the resolver runs on every attempt so a rotated identity is
    /// picked up automatically.
    pub connectable: crate::connectable::Connectable,
    /// Cached `Connectable::label()` for snapshot construction.
    pub label: String,
    pub last_pong_at_unix_ms: Option<u64>,
    pub last_rtt_ms: Option<u64>,
}

pub(crate) struct SupervisorState {
    pub next_id: IntentId,
    pub intents: HashMap<IntentId, IntentEntry>,
    /// Reverse map: connected peer_id → intent that owns the connection.
    pub peer_to_intent: HashMap<PeerId, IntentId>,
    /// Dedup: existing `Direct(addr)` intent for this address, if any.
    pub direct_dedup: HashMap<PeerAddr, IntentId>,
    /// Dedup: existing `Resolving { input }` intent for this input, if any.
    pub resolving_dedup: HashMap<String, IntentId>,
    /// Live-state subscribers. Each receives an `IntentSnapshot` every
    /// time an intent transitions, plus an initial replay on subscribe.
    /// Senders whose receiver is dropped are pruned at broadcast time.
    pub subscribers: Vec<mpsc::UnboundedSender<IntentSnapshot>>,
}

pub(crate) enum SupervisorCommand {
    Add {
        connectable: crate::connectable::Connectable,
        ack: oneshot::Sender<Result<IntentId, crate::connectable::ResolveErr>>,
    },
    Remove {
        id: IntentId,
        ack: oneshot::Sender<()>,
    },
    Snapshot {
        ack: oneshot::Sender<Vec<IntentSnapshot>>,
    },
    Subscribe {
        ack: oneshot::Sender<mpsc::UnboundedReceiver<IntentSnapshot>>,
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
                next_id: 0,
                intents: HashMap::new(),
                peer_to_intent: HashMap::new(),
                direct_dedup: HashMap::new(),
                resolving_dedup: HashMap::new(),
                subscribers: Vec::new(),
            })),
            policy,
        })
    }

    /// Register a durable intent. Returns once the supervisor has
    /// allocated an `IntentId` and recorded the intent (one cmd-channel
    /// round-trip; does NOT wait for the first connection). The only
    /// `Err` is `ResolveErr::Parse` — typed garbage that the resolver
    /// can never make sense of. Transient failures (resolver fetch,
    /// dial, Hello) never bubble out: the supervisor retries with the
    /// existing exponential backoff.
    ///
    /// If `connectable` matches an existing intent (same `Direct` addr,
    /// or same `Resolving { input }`), the existing `IntentId` is
    /// returned and no new intent is created.
    pub async fn add(
        &self,
        connectable: crate::connectable::Connectable,
    ) -> Result<IntentId, crate::connectable::ResolveErr> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::Add { connectable, ack })
            .map_err(|_| crate::connectable::ResolveErr::Transient("supervisor closed".into()))?;
        rx.await
            .map_err(|_| crate::connectable::ResolveErr::Transient("supervisor closed".into()))?
    }

    /// Cancel a durable intent. Tears down the connection if connected.
    pub async fn remove(&self, id: IntentId) {
        let (ack, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SupervisorCommand::Remove { id, ack })
            .is_ok()
        {
            let _ = rx.await;
        }
    }

    /// Snapshot every intent's current state. For UI / debugging.
    pub async fn snapshot(&self) -> Vec<IntentSnapshot> {
        let (ack, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SupervisorCommand::Snapshot { ack })
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Subscribe to intent state changes. The returned receiver is fed
    /// the current snapshot of every existing intent on subscribe (so
    /// late subscribers don't miss state), then every change after that.
    pub async fn subscribe_intents(&self) -> mpsc::UnboundedReceiver<IntentSnapshot> {
        let (ack, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SupervisorCommand::Subscribe { ack })
            .is_err()
        {
            let (_tx, rx) = mpsc::unbounded_channel();
            return rx;
        }
        rx.await.unwrap_or_else(|_| {
            let (_tx, rx) = mpsc::unbounded_channel();
            rx
        })
    }

    /// Broadcast the current snapshot of `id` to all subscribers.
    /// Drops senders whose receiver has been dropped. Caller must hold
    /// the inner state lock.
    fn broadcast(state: &mut SupervisorState, id: IntentId) {
        let Some(entry) = state.intents.get(&id) else {
            return;
        };
        let snap = IntentSnapshot {
            id,
            state: entry.state,
            peer_id: entry.peer_id.clone(),
            kind: entry.kind,
            attempt: entry.attempt,
            label: entry.label.clone(),
            last_pong_at_unix_ms: entry.last_pong_at_unix_ms,
            last_rtt_ms: entry.last_rtt_ms,
        };
        state.subscribers.retain(|tx| tx.send(snap.clone()).is_ok());
    }

    /// Long-running task. Caller spawns this with `spawn_local`.
    pub async fn run(self: Rc<Self>) {
        let mut cmd_rx = match self.cmd_rx.borrow_mut().take() {
            Some(rx) => rx,
            None => return, // run() called twice
        };
        let mut events = self.engine.subscribe_engine_events().await;

        // Seed RNG. We use a simple counter-based seed so this works
        // identically on wasm32 and native without pulling in OsRng.
        let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(
            web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        );

        loop {
            // Compute the soonest backoff wakeup, if any. We use a
            // computed `Duration` from `Instant::now()` rather than
            // `sleep_until` to keep the wasm/native code path identical
            // and to avoid relying on `Instant::from_std` on wasmtimer.
            let sleep_dur = self.next_backoff_sleep();

            #[cfg(not(target_arch = "wasm32"))]
            let sleep_fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> =
                match sleep_dur {
                    Some(dur) => Box::pin(tokio::time::sleep(dur)),
                    None => Box::pin(std::future::pending::<()>()),
                };
            #[cfg(target_arch = "wasm32")]
            let sleep_fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> =
                match sleep_dur {
                    Some(dur) => Box::pin(wasmtimer::tokio::sleep(dur)),
                    None => Box::pin(std::future::pending::<()>()),
                };

            tokio::select! {
                Some(ev) = events.recv() => {
                    self.clone().handle_engine_event(ev, &mut rng).await;
                }
                Some(cmd) = cmd_rx.recv() => {
                    self.clone().handle_command(cmd).await;
                }
                _ = sleep_fut => {
                    self.clone().fire_due_backoffs(&mut rng).await;
                }
                else => return,
            }
        }
    }

    /// Returns the duration until the soonest `next_attempt_at` across
    /// all Backoff intents (saturating at zero if already past).
    fn next_backoff_sleep(&self) -> Option<Duration> {
        let state = self.state.borrow();
        let earliest = state
            .intents
            .values()
            .filter(|e| e.state == IntentState::Backoff)
            .filter_map(|e| e.next_attempt_at)
            .min()?;
        let now = web_time::SystemTime::now();
        Some(earliest.duration_since(now).unwrap_or(Duration::ZERO))
    }

    async fn handle_engine_event(
        self: Rc<Self>,
        ev: EngineEvent,
        _rng: &mut rand_chacha::ChaCha20Rng,
    ) {
        match ev {
            EngineEvent::PeerAdded { peer_id, kind } => {
                let mut state = self.state.borrow_mut();
                if let Some(id) = state.peer_to_intent.get(&peer_id).copied() {
                    if let Some(entry) = state.intents.get_mut(&id) {
                        entry.state = IntentState::Connected;
                        entry.kind = Some(kind);
                        entry.attempt = 0;
                        entry.next_attempt_at = None;
                        Self::broadcast(&mut state, id);
                    }
                }
            }
            EngineEvent::PeerRemoved { peer_id } => {
                let mut state = self.state.borrow_mut();
                if let Some(id) = state.peer_to_intent.remove(&peer_id) {
                    if let Some(entry) = state.intents.get_mut(&id) {
                        if entry.state != IntentState::Cancelled {
                            entry.state = IntentState::Backoff;
                            entry.peer_id = None;
                            entry.kind = None;
                            let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(
                                id.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                                    .wrapping_add(entry.attempt as u64),
                            );
                            let delay = self.policy.delay(entry.attempt, &mut rng);
                            entry.next_attempt_at = Some(web_time::SystemTime::now() + delay);
                            Self::broadcast(&mut state, id);
                        }
                    }
                }
            }
            // Placeholder: real handler wired in Task 5.
            EngineEvent::PongObserved { .. } => {}
        }
    }

    async fn handle_command(self: Rc<Self>, cmd: SupervisorCommand) {
        match cmd {
            SupervisorCommand::Add { connectable, ack } => {
                // Dedup by Connectable identity.
                let dedup_id: Option<IntentId> = {
                    let state = self.state.borrow();
                    match &connectable {
                        crate::connectable::Connectable::Direct(addr) => {
                            state.direct_dedup.get(addr).copied()
                        }
                        crate::connectable::Connectable::Resolving { input, .. } => {
                            state.resolving_dedup.get(input).copied()
                        }
                    }
                };
                if let Some(existing) = dedup_id {
                    let _ = ack.send(Ok(existing));
                    return;
                }

                // Eager parse-check for Resolving inputs. The resolver
                // does this internally on every attempt, but we run it
                // once here so unparseable inputs never become a zombie
                // intent that retries forever.
                if let crate::connectable::Connectable::Resolving { input, .. } = &connectable {
                    if let Err(sunset_relay_resolver::Error::MalformedInput(e)) =
                        sunset_relay_resolver::parse_input(input)
                    {
                        let _ = ack.send(Err(crate::connectable::ResolveErr::Parse(e)));
                        return;
                    }
                }

                let id = {
                    let mut state = self.state.borrow_mut();
                    let id = state.next_id;
                    state.next_id += 1;

                    match &connectable {
                        crate::connectable::Connectable::Direct(addr) => {
                            state.direct_dedup.insert(addr.clone(), id);
                        }
                        crate::connectable::Connectable::Resolving { input, .. } => {
                            state.resolving_dedup.insert(input.clone(), id);
                        }
                    }

                    let label = connectable.label();
                    state.intents.insert(
                        id,
                        IntentEntry {
                            id,
                            state: IntentState::Connecting,
                            attempt: 0,
                            peer_id: None,
                            kind: None,
                            next_attempt_at: None,
                            connectable: connectable.clone(),
                            label,
                            last_pong_at_unix_ms: None,
                            last_rtt_ms: None,
                        },
                    );
                    Self::broadcast(&mut state, id);
                    id
                };

                // Reply to the caller now that the intent is registered.
                // The first dial happens in the background; failures
                // transition the intent to Backoff (no longer surfaced).
                let _ = ack.send(Ok(id));

                self.clone().spawn_dial(id);
            }

            SupervisorCommand::Remove { id, ack } => {
                let (peer_id_to_remove, addr_to_unmap, input_to_unmap) = {
                    let mut state = self.state.borrow_mut();
                    let entry = state.intents.get_mut(&id);
                    let (pid, addr, input) = if let Some(entry) = entry {
                        entry.state = IntentState::Cancelled;
                        let pid = entry.peer_id.clone();
                        let addr = match &entry.connectable {
                            crate::connectable::Connectable::Direct(a) => Some(a.clone()),
                            _ => None,
                        };
                        let input = match &entry.connectable {
                            crate::connectable::Connectable::Resolving { input, .. } => {
                                Some(input.clone())
                            }
                            _ => None,
                        };
                        (pid, addr, input)
                    } else {
                        (None, None, None)
                    };
                    if let Some(p) = &pid {
                        state.peer_to_intent.remove(p);
                    }
                    Self::broadcast(&mut state, id);
                    (pid, addr, input)
                };
                if let Some(pid) = peer_id_to_remove {
                    let _ = self.engine.remove_peer(pid).await;
                }
                {
                    let mut state = self.state.borrow_mut();
                    if let Some(addr) = addr_to_unmap {
                        state.direct_dedup.remove(&addr);
                    }
                    if let Some(input) = input_to_unmap {
                        state.resolving_dedup.remove(&input);
                    }
                    state.intents.remove(&id);
                }
                let _ = ack.send(());
            }

            SupervisorCommand::Snapshot { ack } => {
                let state = self.state.borrow();
                let snap: Vec<IntentSnapshot> = state
                    .intents
                    .values()
                    .map(|e| IntentSnapshot {
                        id: e.id,
                        state: e.state,
                        peer_id: e.peer_id.clone(),
                        kind: e.kind,
                        attempt: e.attempt,
                        label: e.label.clone(),
                        last_pong_at_unix_ms: e.last_pong_at_unix_ms,
                        last_rtt_ms: e.last_rtt_ms,
                    })
                    .collect();
                let _ = ack.send(snap);
            }

            SupervisorCommand::Subscribe { ack } => {
                let (tx, rx) = mpsc::unbounded_channel();
                // Replay current state, then register for live updates.
                // Replay happens BEFORE register so callers can't see a
                // change for an intent without first having received its
                // baseline snapshot.
                {
                    let mut state = self.state.borrow_mut();
                    let snaps: Vec<IntentSnapshot> = state
                        .intents
                        .values()
                        .map(|e| IntentSnapshot {
                            id: e.id,
                            state: e.state,
                            peer_id: e.peer_id.clone(),
                            kind: e.kind,
                            attempt: e.attempt,
                            label: e.label.clone(),
                            last_pong_at_unix_ms: e.last_pong_at_unix_ms,
                            last_rtt_ms: e.last_rtt_ms,
                        })
                        .collect();
                    for snap in snaps {
                        if tx.send(snap).is_err() {
                            let _ = ack.send(rx);
                            return;
                        }
                    }
                    state.subscribers.push(tx);
                }
                let _ = ack.send(rx);
            }
        }
    }

    /// Spawn a single dial attempt for the given intent. On success,
    /// transitions the intent to Connected and populates peer_id +
    /// kind. On failure, transitions to Backoff and schedules the next
    /// attempt with the existing `BackoffPolicy`.
    fn spawn_dial(self: Rc<Self>, id: IntentId) {
        let engine = self.engine.clone();
        let state = self.state.clone();
        let policy = self.policy.clone();
        crate::spawn::spawn_local(async move {
            // Snapshot the connectable for this attempt.
            let connectable = {
                let s = state.borrow();
                match s.intents.get(&id) {
                    Some(entry) if entry.state != IntentState::Cancelled => {
                        entry.connectable.clone()
                    }
                    _ => return,
                }
            };

            let attempt_result = match connectable.resolve_addr().await {
                Ok(addr) => engine
                    .add_peer(addr)
                    .await
                    .map_err(crate::connectable::ResolveErr::from),
                Err(e) => Err(e),
            };

            // Compute next state. The borrow is split into two scopes so
            // no `Ref` is held across the post-cancel
            // `engine.remove_peer().await`: scope 1 inspects state and
            // returns whether to tear down a late-arriving connection;
            // scope 2 (entered only on `Proceed`) applies the success /
            // parse-error / transient-error transition.
            //
            // Note: `Remove` deletes the intent entry entirely (not just
            // setting `state = Cancelled`), so by the time this scope
            // runs after a `remove(id).await`, `intents.get(&id)` is
            // `None`. The `None` arm therefore must teardown a
            // late-arriving connection just like the `Cancelled` arm —
            // otherwise the engine ends up holding an orphan peer.
            enum CancelAction {
                /// Intent is cancelled or already gone; the dial
                /// succeeded, so we own a connection at the engine that
                /// needs tearing down before we exit.
                TearDownLatePeer(PeerId),
                /// Intent is cancelled or already gone; the dial
                /// failed, so there's nothing to clean up — just exit.
                ExitNoOp,
                /// Intent is not cancelled — proceed with the normal
                /// state transition in scope 2.
                Proceed,
            }
            let action = {
                let s = state.borrow();
                match s.intents.get(&id) {
                    Some(entry) if entry.state == IntentState::Cancelled => match &attempt_result {
                        Ok((pid, _)) => CancelAction::TearDownLatePeer(pid.clone()),
                        Err(_) => CancelAction::ExitNoOp,
                    },
                    Some(_) => CancelAction::Proceed,
                    None => match &attempt_result {
                        Ok((pid, _)) => CancelAction::TearDownLatePeer(pid.clone()),
                        Err(_) => CancelAction::ExitNoOp,
                    },
                }
            };
            match action {
                CancelAction::ExitNoOp => return,
                CancelAction::TearDownLatePeer(pid) => {
                    let _ = engine.remove_peer(pid).await;
                    return;
                }
                CancelAction::Proceed => {}
            }
            {
                let mut s = state.borrow_mut();
                let Some(entry) = s.intents.get_mut(&id) else {
                    return;
                };
                if entry.state == IntentState::Cancelled {
                    return;
                }
                match attempt_result {
                    Ok((peer_id, kind)) => {
                        entry.state = IntentState::Connected;
                        entry.peer_id = Some(peer_id.clone());
                        entry.kind = Some(kind);
                        entry.attempt = 0;
                        entry.next_attempt_at = None;
                        s.peer_to_intent.insert(peer_id, id);
                        Self::broadcast(&mut s, id);
                    }
                    Err(crate::connectable::ResolveErr::Parse(_)) => {
                        // Permanent — cancel the intent and clean up
                        // dedup so a future `add()` with the same
                        // `Connectable` can produce a fresh intent
                        // (and discover the same parse error eagerly
                        // in the cmd handler) rather than dedup'ing
                        // to this dead Cancelled one.
                        entry.state = IntentState::Cancelled;
                        let dedup_key: DedupKey = match &entry.connectable {
                            crate::connectable::Connectable::Direct(addr) => {
                                DedupKey::Direct(addr.clone())
                            }
                            crate::connectable::Connectable::Resolving { input, .. } => {
                                DedupKey::Resolving(input.clone())
                            }
                        };
                        Self::broadcast(&mut s, id);
                        match dedup_key {
                            DedupKey::Direct(addr) => {
                                s.direct_dedup.remove(&addr);
                            }
                            DedupKey::Resolving(input) => {
                                s.resolving_dedup.remove(&input);
                            }
                        }
                        s.intents.remove(&id);
                    }
                    Err(_transient) => {
                        entry.attempt = entry.attempt.saturating_add(1);
                        entry.state = IntentState::Backoff;
                        // Use a deterministic-ish RNG seeded from the
                        // entry id + attempt so retries don't pile up
                        // at the same wall time across crates that
                        // share a global RNG.
                        let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(
                            id.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                                .wrapping_add(entry.attempt as u64),
                        );
                        let delay = policy.delay(entry.attempt, &mut rng);
                        entry.next_attempt_at = Some(web_time::SystemTime::now() + delay);
                        Self::broadcast(&mut s, id);
                    }
                }
            }
        });
    }

    async fn fire_due_backoffs(self: Rc<Self>, _rng: &mut rand_chacha::ChaCha20Rng) {
        let now = web_time::SystemTime::now();
        let due: Vec<IntentId> = {
            let state = self.state.borrow();
            state
                .intents
                .iter()
                .filter(|(_, e)| {
                    e.state == IntentState::Backoff
                        && e.next_attempt_at.map(|at| at <= now).unwrap_or(false)
                })
                .map(|(id, _)| *id)
                .collect()
        };

        for id in due {
            {
                let mut state = self.state.borrow_mut();
                if let Some(entry) = state.intents.get_mut(&id) {
                    if entry.state != IntentState::Backoff {
                        continue;
                    }
                    entry.state = IntentState::Connecting;
                    entry.next_attempt_at = None;
                    Self::broadcast(&mut state, id);
                }
            }
            self.clone().spawn_dial(id);
        }
    }
}

#[cfg(all(test, feature = "test-helpers"))]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;
    use sunset_store::VerifyingKey;
    use sunset_store_memory::MemoryStore;

    use crate::engine::SyncEngine;
    use crate::test_transport::{TestNetwork, TestTransport};
    use crate::types::SyncConfig;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    struct StubSigner(VerifyingKey);
    impl crate::Signer for StubSigner {
        fn verifying_key(&self) -> VerifyingKey {
            self.0.clone()
        }
        fn sign(&self, _: &[u8]) -> Bytes {
            Bytes::from_static(&[0u8; 64])
        }
    }

    fn engine_with_addr(
        net: &TestNetwork,
        peer_label: &[u8],
        addr: &str,
    ) -> Rc<SyncEngine<MemoryStore, TestTransport>> {
        let store = Arc::new(MemoryStore::with_accept_all());
        let local_peer = PeerId(vk(peer_label));
        let transport = net.transport(
            local_peer.clone(),
            PeerAddr::new(Bytes::copy_from_slice(addr.as_bytes())),
        );
        let signer = Arc::new(StubSigner(local_peer.0.clone()));
        Rc::new(SyncEngine::new(
            store,
            transport,
            SyncConfig::default(),
            local_peer,
            signer,
        ))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_dial_success_returns_id_and_connects() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");
                let _bob = engine_with_addr(&net, b"bob", "bob");

                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });
                crate::spawn::spawn_local({
                    let b = _bob.clone();
                    async move { b.run().await }
                });

                let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
                let id = sup
                    .add(crate::connectable::Connectable::Direct(bob_addr))
                    .await
                    .unwrap();

                // Wait until the snapshot reports Connected (with a
                // bounded retry — the dial happens asynchronously now).
                let mut connected = false;
                for _ in 0..50 {
                    let snap = sup.snapshot().await;
                    if snap
                        .iter()
                        .any(|s| s.id == id && s.state == IntentState::Connected)
                    {
                        connected = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                assert!(connected, "intent did not reach Connected");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_dial_failure_enters_backoff_and_retries() {
        // NOTE: we intentionally do NOT use `start_paused = true` here.
        // The supervisor's backoff scheduling reads `web_time::SystemTime::now()`
        // (real wall clock), while the dial-loop wakeup uses `tokio::time::sleep`.
        // Pausing tokio time desynchronises those two clocks: the
        // backoff target is ~1 s of real time in the future, but tokio's
        // sleep only fires when virtual time advances by that much, which
        // only ever happens if we advance by ≥1 real second. The cleanest
        // fix is to run on real time with a tight backoff policy so the
        // test finishes in <1 s of wall clock time.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");

                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });

                // Tight backoff so the redial completes well within the
                // test's wait window.
                let policy = BackoffPolicy {
                    initial: std::time::Duration::from_millis(50),
                    max: std::time::Duration::from_millis(50),
                    multiplier: 1.0,
                    jitter: 0.0,
                };
                let sup = PeerSupervisor::new(alice.clone(), policy);
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                // No engine is listening at "ghost" yet.
                let ghost = PeerAddr::new(Bytes::from_static(b"ghost"));
                let id = sup
                    .add(crate::connectable::Connectable::Direct(ghost.clone()))
                    .await
                    .expect("add must return Ok even if first dial will fail");

                // Wait for the intent to enter Backoff (first dial failed).
                let mut backoff_seen = false;
                for _ in 0..100 {
                    let snap = sup.snapshot().await;
                    if snap
                        .iter()
                        .any(|s| s.id == id && s.state == IntentState::Backoff)
                    {
                        backoff_seen = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                assert!(
                    backoff_seen,
                    "intent did not enter Backoff after first dial failure"
                );

                // Bring "ghost" online; intent should reconnect.
                let _ghost_engine = engine_with_addr(&net, b"ghost", "ghost");
                crate::spawn::spawn_local({
                    let g = _ghost_engine.clone();
                    async move { g.run().await }
                });

                // The supervisor's backoff timer should fire (50 ms initial,
                // no jitter) and the next dial should land. Allow generous
                // slack — 2 s is plenty for a 50 ms backoff.
                let mut connected = false;
                for _ in 0..200 {
                    let snap = sup.snapshot().await;
                    if snap
                        .iter()
                        .any(|s| s.id == id && s.state == IntentState::Connected)
                    {
                        connected = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                assert!(connected, "intent did not reconnect after engine came up");

                // Intent is still in the table (no first-dial cleanup).
                assert!(sup.snapshot().await.iter().any(|s| s.id == id));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idempotent_add_returns_same_id() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");
                let _bob = engine_with_addr(&net, b"bob", "bob");

                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });
                crate::spawn::spawn_local({
                    let b = _bob.clone();
                    async move { b.run().await }
                });

                let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
                let id1 = sup
                    .add(crate::connectable::Connectable::Direct(bob_addr.clone()))
                    .await
                    .unwrap();
                let id2 = sup
                    .add(crate::connectable::Connectable::Direct(bob_addr.clone()))
                    .await
                    .unwrap();
                let id3 = sup
                    .add(crate::connectable::Connectable::Direct(bob_addr))
                    .await
                    .unwrap();

                assert_eq!(id1, id2);
                assert_eq!(id2, id3);
                assert_eq!(sup.snapshot().await.len(), 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remove_cancels_intent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");
                let bob = engine_with_addr(&net, b"bob", "bob");

                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });
                crate::spawn::spawn_local({
                    let b = bob.clone();
                    async move { b.run().await }
                });

                let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
                let id = sup
                    .add(crate::connectable::Connectable::Direct(bob_addr))
                    .await
                    .unwrap();

                // Wait for connect.
                for _ in 0..50 {
                    let snap = sup.snapshot().await;
                    if snap
                        .iter()
                        .any(|s| s.id == id && s.state == IntentState::Connected)
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }

                sup.remove(id).await;

                assert!(sup.snapshot().await.iter().all(|s| s.id != id));
                let connected = alice.connected_peers().await;
                assert!(
                    connected
                        .iter()
                        .find(|p| p.0.as_bytes() == b"bob")
                        .is_none()
                );
            })
            .await;
    }

    /// Regression: every `Connected` snapshot must carry `kind: Some(_)`.
    /// `kind` is the Direct/Relay distinction surfaced to the frontend, so
    /// "kind populated by a follow-up `PeerAdded` broadcast that races
    /// against `spawn_dial`'s post-await borrow_mut" is observable as a
    /// permanent `kind: None` whenever the broadcast runs first.
    #[tokio::test(flavor = "current_thread")]
    async fn connected_snapshot_carries_kind() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");
                let _bob = engine_with_addr(&net, b"bob", "bob");

                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });
                crate::spawn::spawn_local({
                    let b = _bob.clone();
                    async move { b.run().await }
                });

                let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
                let id = sup
                    .add(crate::connectable::Connectable::Direct(bob_addr))
                    .await
                    .unwrap();

                // Wait for Connected.
                for _ in 0..100 {
                    let snap = sup.snapshot().await;
                    if let Some(s) = snap.iter().find(|s| s.id == id) {
                        if s.state == IntentState::Connected {
                            // Must have kind populated by the time state is Connected.
                            assert!(
                                s.kind.is_some(),
                                "Connected snapshot must carry kind, got {s:?}"
                            );
                            return;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                panic!("intent never reached Connected");
            })
            .await;
    }

    /// Regression: the cmd-handler eager parse rejection must not
    /// poison the dedup state. The eager check runs `parse_input`
    /// before anything is inserted into `intents` / `direct_dedup` /
    /// `resolving_dedup`, so a rejected `Resolving` add must leave the
    /// supervisor's tables exactly as it found them and a subsequent
    /// valid `add()` must allocate a fresh intent.
    ///
    /// Note: this does NOT exercise `spawn_dial`'s `Err(Parse(_))`
    /// cleanup arm. That arm is currently unreachable in production —
    /// the eager check filters out anything `parse_input` rejects, and
    /// `resolver.resolve()` calls the same `parse_input` first. The
    /// dial-side cleanup is retained as forward-compatible
    /// defense-in-depth (in case the resolver's mapping ever changes
    /// to surface Parse errors that the eager check misses), but no
    /// existing test proves it works.
    #[tokio::test(flavor = "current_thread")]
    async fn parse_error_releases_dedup_so_fresh_add_proceeds() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");
                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });

                let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                // First add — eager parse-check rejects, returns Err.
                // No intent recorded.
                let bad_fetch: std::rc::Rc<dyn sunset_relay_resolver::HttpFetch> =
                    std::rc::Rc::new(NoopFetch);
                let _err = sup
                    .add(crate::connectable::Connectable::Resolving {
                        input: "".into(),
                        fetch: bad_fetch.clone(),
                    })
                    .await
                    .expect_err("empty input must Parse-fail");

                // Now register a Direct intent — must succeed with a fresh id,
                // not dedup to a dead Cancelled one.
                let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
                let _bob = engine_with_addr(&net, b"bob", "bob");
                crate::spawn::spawn_local({
                    let b = _bob.clone();
                    async move { b.run().await }
                });
                let id = sup
                    .add(crate::connectable::Connectable::Direct(bob_addr))
                    .await
                    .expect("direct add after parse-fail must succeed");

                // Snapshot should contain exactly one intent (the new one).
                let snap = sup.snapshot().await;
                assert_eq!(snap.len(), 1);
                assert_eq!(snap[0].id, id);
            })
            .await;
    }

    /// Regression: cancelling an intent while its dial is in flight
    /// must not leave a zombie engine-side connection. Without the
    /// explicit `engine.remove_peer` call in `spawn_dial`'s
    /// `Cancelled` branch, a late dial that lands after `remove(id)`
    /// stays connected at the engine until the remote drops it.
    #[tokio::test(flavor = "current_thread")]
    async fn remove_during_in_flight_dial_tears_down_connection() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");
                let _bob = engine_with_addr(&net, b"bob", "bob");
                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });
                crate::spawn::spawn_local({
                    let b = _bob.clone();
                    async move { b.run().await }
                });

                let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
                let id = sup
                    .add(crate::connectable::Connectable::Direct(bob_addr))
                    .await
                    .unwrap();

                // Cancel BEFORE waiting for Connected. The dial is likely
                // already in flight; we want to verify the late dial result
                // doesn't leave a zombie engine-side connection.
                sup.remove(id).await;

                // Wait long enough for the in-flight dial to complete
                // (TestNetwork connect + Hello round-trip is sub-millisecond
                // on localhost) AND for spawn_dial's post-await scope to
                // run. A polling loop that exits as soon as `bob` is absent
                // is racy in the supervisor's favor: right after `remove`,
                // bob isn't connected yet (the dial hasn't completed), and
                // the loop exits before the leak materializes. Use a fixed
                // settle instead.
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                let connected = alice.connected_peers().await;
                assert!(
                    connected.iter().all(|p| p.0.as_bytes() != b"bob"),
                    "bob still connected to engine after intent removal: {connected:?}"
                );
            })
            .await;
    }

    struct NoopFetch;

    #[async_trait::async_trait(?Send)]
    impl sunset_relay_resolver::HttpFetch for NoopFetch {
        async fn get(&self, _: &str) -> sunset_relay_resolver::Result<String> {
            unreachable!("parse-check should reject empty input before fetch")
        }
    }

    /// Fake fetcher that fails the first `fail_first` calls with HTTP
    /// 503 then returns `body`. `seen` counts every call so tests can
    /// assert how many resolves the supervisor performed.
    struct CountingFakeFetch {
        body: String,
        fail_first: std::cell::Cell<usize>,
        seen: std::cell::Cell<usize>,
    }

    impl CountingFakeFetch {
        fn new(body: String, fail_first: usize) -> Rc<Self> {
            Rc::new(Self {
                body,
                fail_first: std::cell::Cell::new(fail_first),
                seen: std::cell::Cell::new(0),
            })
        }
    }

    #[async_trait::async_trait(?Send)]
    impl sunset_relay_resolver::HttpFetch for CountingFakeFetch {
        async fn get(&self, _url: &str) -> sunset_relay_resolver::Result<String> {
            self.seen.set(self.seen.get() + 1);
            let n = self.fail_first.get();
            if n > 0 {
                self.fail_first.set(n - 1);
                return Err(sunset_relay_resolver::Error::Http("status 503".into()));
            }
            Ok(self.body.clone())
        }
    }

    /// Resolver runs once per dial attempt — a rotated relay identity
    /// is picked up automatically rather than getting cached forever.
    /// We start a fetcher that fails twice with HTTP 503, then succeeds.
    /// The supervisor should call the fetcher 3× total and end Connected.
    #[tokio::test(flavor = "current_thread")]
    async fn resolving_intent_re_resolves_each_attempt() {
        // Same real-time + tight-policy strategy as
        // `first_dial_failure_enters_backoff_and_retries` — the
        // supervisor's backoff scheduler reads `web_time::SystemTime::now()`,
        // which `tokio::time::pause` doesn't virtualize.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();

                // Body the resolver will eventually accept. The
                // `address` field is unused by the resolver (it
                // appends `#x25519=<hex>` to the parsed ws_url) so
                // its content is irrelevant to canonicalization.
                let body = format!(
                    "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"ws://bob\"}}",
                    "00".repeat(32),
                    "ab".repeat(32),
                );

                // Pre-compute what the resolver will produce so we
                // can register the engine at exactly that addr —
                // TestNetwork routes by exact PeerAddr bytes.
                let probe_fetch: Rc<dyn sunset_relay_resolver::HttpFetch> =
                    CountingFakeFetch::new(body.clone(), 0);
                let probe_resolver = sunset_relay_resolver::Resolver::new(probe_fetch);
                let canonical = probe_resolver
                    .resolve("bob")
                    .await
                    .expect("probe resolve must succeed");

                let alice = engine_with_addr(&net, b"alice", "alice");
                let _bob = engine_with_addr(&net, b"bob", &canonical);
                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });
                crate::spawn::spawn_local({
                    let b = _bob.clone();
                    async move { b.run().await }
                });

                // Real fetcher: fails twice with HTTP 503, then succeeds.
                let fetch = CountingFakeFetch::new(body, 2);

                let policy = BackoffPolicy {
                    initial: std::time::Duration::from_millis(50),
                    max: std::time::Duration::from_millis(50),
                    multiplier: 1.0,
                    jitter: 0.0,
                };
                let sup = PeerSupervisor::new(alice.clone(), policy);
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                let id = sup
                    .add(crate::connectable::Connectable::Resolving {
                        input: "bob".into(),
                        fetch: fetch.clone(),
                    })
                    .await
                    .unwrap();

                let mut connected = false;
                for _ in 0..200 {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    let snap = sup.snapshot().await;
                    if snap
                        .iter()
                        .any(|s| s.id == id && s.state == IntentState::Connected)
                    {
                        connected = true;
                        break;
                    }
                }
                assert!(connected, "intent never reached Connected");
                assert_eq!(
                    fetch.seen.get(),
                    3,
                    "resolver should have been called once per attempt"
                );
            })
            .await;
    }

    /// `subscribe_intents` replays the current snapshot of every
    /// existing intent on subscribe — late subscribers don't miss
    /// state. We register two intents (one that connects, one that
    /// won't), wait for them to settle, then subscribe and verify
    /// both intents' snapshots flow through.
    #[tokio::test(flavor = "current_thread")]
    async fn subscribe_intents_replays_current_state() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");
                let _bob = engine_with_addr(&net, b"bob", "bob");
                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });
                crate::spawn::spawn_local({
                    let b = _bob.clone();
                    async move { b.run().await }
                });

                let policy = BackoffPolicy {
                    initial: std::time::Duration::from_millis(50),
                    max: std::time::Duration::from_millis(50),
                    multiplier: 1.0,
                    jitter: 0.0,
                };
                let sup = PeerSupervisor::new(alice.clone(), policy);
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                // One intent connects ("bob"), one doesn't ("ghost").
                let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
                let ghost = PeerAddr::new(Bytes::from_static(b"ghost"));
                let id_bob = sup
                    .add(crate::connectable::Connectable::Direct(bob_addr))
                    .await
                    .unwrap();
                let id_ghost = sup
                    .add(crate::connectable::Connectable::Direct(ghost))
                    .await
                    .unwrap();

                // Wait until both intents have settled into terminal
                // pre-subscribe states (Connected for bob, Backoff for
                // ghost after the first dial fails).
                for _ in 0..100 {
                    let snap = sup.snapshot().await;
                    let ready = snap
                        .iter()
                        .any(|s| s.id == id_bob && s.state == IntentState::Connected)
                        && snap
                            .iter()
                            .any(|s| s.id == id_ghost && s.state == IntentState::Backoff);
                    if ready {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }

                // Late subscribe — must receive a snapshot for both.
                let mut rx = sup.subscribe_intents().await;
                let mut seen_bob = false;
                let mut seen_ghost = false;
                // The subscriber may also pick up background transitions
                // (ghost re-attempts on its 50 ms timer); accept up to
                // 6 events before giving up.
                for _ in 0..6 {
                    let snap =
                        match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                            .await
                        {
                            Ok(Some(s)) => s,
                            Ok(None) => break,
                            Err(_) => break,
                        };
                    if snap.id == id_bob {
                        seen_bob = true;
                    } else if snap.id == id_ghost {
                        seen_ghost = true;
                    }
                    if seen_bob && seen_ghost {
                        break;
                    }
                }
                assert!(seen_bob, "bob's snapshot was not replayed");
                assert!(seen_ghost, "ghost's snapshot was not replayed");
            })
            .await;
    }

    /// `add()` returns near-instant on `Resolving` even when the
    /// resolver would block forever. Proves the API contract that
    /// `add()` does not await the first dial. We use `start_paused`
    /// so the spawned dial task can't actually run; if `add()` were
    /// gated on the dial completing, the test would hang.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn add_returns_immediately_on_resolving() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");
                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });

                // Always-failing fetcher — if `add()` were waiting for
                // the first dial to complete, it would block forever.
                let fetch = CountingFakeFetch::new(String::new(), usize::MAX);

                let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                let started = tokio::time::Instant::now();
                let _ = sup
                    .add(crate::connectable::Connectable::Resolving {
                        input: "relay.example.com".into(),
                        fetch,
                    })
                    .await
                    .unwrap();
                let elapsed = started.elapsed();
                assert!(
                    elapsed < std::time::Duration::from_millis(50),
                    "add() returned in {elapsed:?}; should be near-instant"
                );
            })
            .await;
    }
}
