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

use rand_core::{RngCore, SeedableRng};
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
    pub next_attempt_at: Option<web_time::SystemTime>,
}

pub(crate) struct SupervisorState {
    pub intents: HashMap<PeerAddr, IntentEntry>,
    /// Reverse map: peer_id → addr. Populated when an intent transitions
    /// to Connected; cleared on disconnect.
    pub peer_to_addr: HashMap<PeerId, PeerAddr>,
    /// Live-state subscribers. Each receives an `IntentSnapshot` every
    /// time an intent transitions. Senders whose receiver is dropped are
    /// pruned at broadcast time.
    pub subscribers: Vec<mpsc::UnboundedSender<IntentSnapshot>>,
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
                subscribers: Vec::new(),
            })),
            policy,
        })
    }

    /// Register a durable intent. Returns when the FIRST connection
    /// completes (success → Ok; failure → Err). Subsequent disconnects
    /// after first success are absorbed silently and trigger redial.
    /// If `addr` is already registered, returns Ok immediately.
    pub async fn add(&self, addr: PeerAddr) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCommand::Add { addr, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Cancel a durable intent. Tears down the connection if connected.
    pub async fn remove(&self, addr: PeerAddr) {
        let (ack, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SupervisorCommand::Remove { addr, ack })
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

    /// Subscribe to live intent state changes. The returned stream emits a
    /// snapshot of an intent every time it transitions (Connecting →
    /// Connected → Backoff → ...). Stream ends when the supervisor's
    /// run loop exits.
    pub fn subscribe(&self) -> futures::stream::LocalBoxStream<'static, IntentSnapshot> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.state.borrow_mut().subscribers.push(tx);
        Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
    }

    /// Broadcast the current snapshot of `addr` to all subscribers.
    /// Drops senders whose receiver has been dropped.
    fn broadcast(state: &mut SupervisorState, addr: &PeerAddr) {
        let Some(entry) = state.intents.get(addr) else {
            return;
        };
        let snap = IntentSnapshot {
            addr: addr.clone(),
            state: entry.state,
            peer_id: entry.peer_id.clone(),
            attempt: entry.attempt,
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
                    self.clone().handle_command(cmd, &mut rng).await;
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
            EngineEvent::PeerAdded { peer_id, .. } => {
                // The supervisor's dial wrapper already populated peer_id
                // from add_peer's return value; this event is just a
                // confirmation latch. No action required beyond
                // syncing state in case the event arrives first.
                let mut state = self.state.borrow_mut();
                if let Some(addr) = state.peer_to_addr.get(&peer_id).cloned() {
                    if let Some(entry) = state.intents.get_mut(&addr) {
                        entry.state = IntentState::Connected;
                        entry.attempt = 0;
                        entry.next_attempt_at = None;
                    }
                    Self::broadcast(&mut state, &addr);
                }
            }
            EngineEvent::PeerRemoved { peer_id } => {
                let mut state = self.state.borrow_mut();
                if let Some(addr) = state.peer_to_addr.remove(&peer_id) {
                    let mut transitioned = false;
                    if let Some(entry) = state.intents.get_mut(&addr) {
                        if entry.state != IntentState::Cancelled {
                            entry.state = IntentState::Backoff;
                            entry.peer_id = None;
                            // Schedule first redial immediately (attempt
                            // counter starts at the *current* attempt; the
                            // dial-failure handler increments).
                            let delay = self.policy.delay(entry.attempt, _rng);
                            entry.next_attempt_at = Some(web_time::SystemTime::now() + delay);
                            transitioned = true;
                        }
                    }
                    if transitioned {
                        Self::broadcast(&mut state, &addr);
                    }
                }
            }
        }
    }

    async fn handle_command(
        self: Rc<Self>,
        cmd: SupervisorCommand,
        rng: &mut rand_chacha::ChaCha20Rng,
    ) {
        match cmd {
            SupervisorCommand::Add { addr, ack } => {
                {
                    let state = self.state.borrow();
                    if state.intents.contains_key(&addr) {
                        // Already an intent. Idempotent.
                        let _ = ack.send(Ok(()));
                        return;
                    }
                }
                {
                    let mut state = self.state.borrow_mut();
                    state.intents.insert(
                        addr.clone(),
                        IntentEntry {
                            state: IntentState::Connecting,
                            attempt: 0,
                            peer_id: None,
                            next_attempt_at: None,
                        },
                    );
                    Self::broadcast(&mut state, &addr);
                }
                let engine = self.engine.clone();
                let state = self.state.clone();
                let addr_for_dial = addr.clone();
                crate::spawn::spawn_local(async move {
                    let r = engine.add_peer(addr_for_dial.clone()).await;
                    match r {
                        Ok(peer_id) => {
                            // Decide whether the intent has been cancelled
                            // in a short critical section, then drop the
                            // borrow before any await.
                            let cancelled = {
                                let mut s = state.borrow_mut();
                                let outcome = match s.intents.get_mut(&addr_for_dial) {
                                    Some(entry) if entry.state == IntentState::Cancelled => true,
                                    Some(entry) => {
                                        entry.state = IntentState::Connected;
                                        entry.peer_id = Some(peer_id.clone());
                                        entry.attempt = 0;
                                        entry.next_attempt_at = None;
                                        s.peer_to_addr
                                            .insert(peer_id.clone(), addr_for_dial.clone());
                                        false
                                    }
                                    None => false,
                                };
                                if !outcome {
                                    Self::broadcast(&mut s, &addr_for_dial);
                                }
                                outcome
                            };
                            if cancelled {
                                // Removed before connection landed; tear down.
                                let _ = engine.remove_peer(peer_id).await;
                            }
                            let _ = ack.send(Ok(()));
                        }
                        Err(e) => {
                            // First-dial failure: remove the intent so the
                            // caller's Err is observable but no zombie state
                            // remains.
                            state.borrow_mut().intents.remove(&addr_for_dial);
                            let _ = ack.send(Err(e));
                        }
                    }
                });
            }
            SupervisorCommand::Remove { addr, ack } => {
                let peer_id_to_remove = {
                    let mut state = self.state.borrow_mut();
                    if let Some(entry) = state.intents.get_mut(&addr) {
                        entry.state = IntentState::Cancelled;
                        let pid = entry.peer_id.clone();
                        if let Some(p) = &pid {
                            state.peer_to_addr.remove(p);
                        }
                        pid
                    } else {
                        None
                    }
                };
                if let Some(pid) = peer_id_to_remove {
                    let _ = self.engine.remove_peer(pid).await;
                }
                {
                    let mut state = self.state.borrow_mut();
                    state.intents.remove(&addr);
                }
                let _ = ack.send(());
            }
            SupervisorCommand::Snapshot { ack } => {
                let state = self.state.borrow();
                let snap: Vec<IntentSnapshot> = state
                    .intents
                    .iter()
                    .map(|(addr, e)| IntentSnapshot {
                        addr: addr.clone(),
                        state: e.state,
                        peer_id: e.peer_id.clone(),
                        attempt: e.attempt,
                    })
                    .collect();
                let _ = ack.send(snap);
            }
        }
        let _ = rng; // silence unused
    }

    async fn fire_due_backoffs(self: Rc<Self>, rng: &mut rand_chacha::ChaCha20Rng) {
        let now = web_time::SystemTime::now();
        // Collect addrs whose backoff has fired.
        let due: Vec<PeerAddr> = {
            let state = self.state.borrow();
            state
                .intents
                .iter()
                .filter(|(_, e)| {
                    e.state == IntentState::Backoff
                        && e.next_attempt_at.map(|at| at <= now).unwrap_or(false)
                })
                .map(|(a, _)| a.clone())
                .collect()
        };

        for addr in due {
            // Mark as Connecting before dialing so a second backoff tick
            // doesn't double-fire.
            {
                let mut state = self.state.borrow_mut();
                if let Some(entry) = state.intents.get_mut(&addr) {
                    if entry.state != IntentState::Backoff {
                        continue;
                    }
                    entry.state = IntentState::Connecting;
                    entry.next_attempt_at = None;
                }
            }
            let engine = self.engine.clone();
            let state = self.state.clone();
            let policy = self.policy.clone();
            let addr_for_dial = addr.clone();
            // Sample a delay-seed for the next backoff if this fails.
            let next_seed = rng.next_u64();
            crate::spawn::spawn_local(async move {
                let r = engine.add_peer(addr_for_dial.clone()).await;
                // Compute the new entry state in a short critical section
                // and capture whether we must tear down a connection that
                // landed after the intent was cancelled.
                let cancelled_peer: Option<PeerId> = {
                    let mut s = state.borrow_mut();
                    let Some(entry) = s.intents.get_mut(&addr_for_dial) else {
                        return;
                    };
                    let (outcome, transitioned) = if entry.state == IntentState::Cancelled {
                        (r.ok(), false)
                    } else {
                        match r {
                            Ok(peer_id) => {
                                entry.state = IntentState::Connected;
                                entry.peer_id = Some(peer_id.clone());
                                entry.attempt = 0;
                                entry.next_attempt_at = None;
                                s.peer_to_addr.insert(peer_id, addr_for_dial.clone());
                                (None, true)
                            }
                            Err(_) => {
                                entry.attempt = entry.attempt.saturating_add(1);
                                entry.state = IntentState::Backoff;
                                // Use a tiny RNG seeded from `next_seed` so this
                                // standalone task can compute a delay without sharing
                                // the parent's RNG.
                                let mut local_rng =
                                    rand_chacha::ChaCha20Rng::seed_from_u64(next_seed);
                                let delay = policy.delay(entry.attempt, &mut local_rng);
                                entry.next_attempt_at = Some(web_time::SystemTime::now() + delay);
                                (None, true)
                            }
                        }
                    };
                    if transitioned {
                        Self::broadcast(&mut s, &addr_for_dial);
                    }
                    outcome
                };
                if let Some(peer_id) = cancelled_peer {
                    let _ = engine.remove_peer(peer_id).await;
                }
            });
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
    async fn first_dial_success() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice = engine_with_addr(&net, b"alice", "alice");
                let bob = engine_with_addr(&net, b"bob", "bob");

                // Start both engines.
                crate::spawn::spawn_local({
                    let a = alice.clone();
                    async move { a.run().await }
                });
                crate::spawn::spawn_local({
                    let b = bob.clone();
                    async move { b.run().await }
                });

                // Supervisor on alice's side.
                let sup = PeerSupervisor::new(alice.clone(), BackoffPolicy::default());
                crate::spawn::spawn_local({
                    let s = sup.clone();
                    async move { s.run().await }
                });

                let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
                sup.add(bob_addr.clone()).await.unwrap();

                let snap = sup.snapshot().await;
                assert_eq!(snap.len(), 1);
                assert_eq!(snap[0].state, IntentState::Connected);
                assert_eq!(snap[0].attempt, 0);
                assert!(snap[0].peer_id.is_some());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_dial_failure_returns_err_and_clears_intent() {
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

                // No engine listening at "ghost".
                let ghost = PeerAddr::new(Bytes::from_static(b"ghost"));
                let res = sup.add(ghost.clone()).await;
                assert!(res.is_err());

                // No zombie intent.
                let snap = sup.snapshot().await;
                assert!(snap.iter().find(|s| s.addr == ghost).is_none());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idempotent_add() {
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
                sup.add(bob_addr.clone()).await.unwrap();
                sup.add(bob_addr.clone()).await.unwrap();
                sup.add(bob_addr.clone()).await.unwrap();

                let snap = sup.snapshot().await;
                assert_eq!(snap.len(), 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subscribe_emits_state_transitions() {
        use futures::StreamExt as _;
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

                let mut sub = sup.subscribe();

                let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
                sup.add(bob_addr.clone()).await.unwrap();

                // Drain until we see a Connected event for bob_addr or 1 s passes.
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
                let mut saw_connected = false;
                while tokio::time::Instant::now() < deadline {
                    let timeout = deadline - tokio::time::Instant::now();
                    match tokio::time::timeout(timeout, sub.next()).await {
                        Ok(Some(snap)) => {
                            if snap.addr == bob_addr && snap.state == IntentState::Connected {
                                saw_connected = true;
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                assert!(
                    saw_connected,
                    "subscribe should have observed bob transition to Connected"
                );
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
                sup.add(bob_addr.clone()).await.unwrap();
                sup.remove(bob_addr.clone()).await;

                let snap = sup.snapshot().await;
                assert!(snap.is_empty());

                // Engine no longer has bob in peer_outbound.
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
}
