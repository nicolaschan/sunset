//! `SyncEngine` — the top-level coordinator.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use sunset_store::{Event, Filter, Replay, Store};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::digest::{BloomFilter, build_digest, entries_missing_from_remote};
use crate::error::{Error, Result};
use crate::message::{DigestRange, SyncMessage};
use crate::peer::{InboundEvent, run_peer};
use crate::signer::Signer;
use crate::transport::Transport;
use crate::types::{PeerAddr, PeerId, SyncConfig, TrustSet};

/// Per-connection identity used to filter stale events from defunct
/// connections (a delayed `Disconnected` from generation N must not kill
/// a freshly-established generation N+1 connection to the same peer).
///
/// Allocated by the engine when a new per-peer task is spawned (both
/// `add_peer` and `accept` paths). Never escapes the crate — public
/// `EngineEvent` carries only `PeerId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConnectionId(u64);

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conn#{}", self.0)
    }
}

impl ConnectionId {
    /// `pub(crate)` constructor used only by tests in adjacent modules.
    /// Production allocation goes through `SyncEngine::alloc_conn_id`.
    #[cfg(test)]
    pub(crate) fn for_test(id: u64) -> Self {
        ConnectionId(id)
    }
}

/// Free helper that spins up the outbound channel + spawns the per-peer
/// task. Extracted from `SyncEngine::spawn_peer` so the AddPeer command
/// handler can call it from a `'static` spawned task without holding
/// `&self`.
fn spawn_run_peer<C: crate::transport::TransportConnection + 'static>(
    conn: C,
    env: crate::peer::PeerEnv,
    conn_id: ConnectionId,
    inbound_tx: mpsc::UnboundedSender<InboundEvent>,
    hello_done: Option<oneshot::Sender<Result<(PeerId, crate::transport::TransportKind)>>>,
) {
    let conn = Rc::new(conn);
    let (out_tx, out_rx) = mpsc::unbounded_channel::<SyncMessage>();
    crate::spawn::spawn_local(run_peer(
        conn, env, conn_id, out_tx, out_rx, inbound_tx, hello_done,
    ));
}

/// Long-running accept loop. Spawned by `SyncEngine::run` once at
/// startup, lives for the engine's lifetime. Owns its own clones of the
/// `Arc<T>` transport and the `next_conn_id` counter, and a clone of
/// the `inbound_tx` sender that per-peer tasks fan into.
///
/// Why this exists as a separate task instead of a `select!` arm: the
/// secondary transport's `accept()` (e.g. `NoiseTransport::accept`)
/// internally awaits a multi-RTT cryptographic handshake on the raw
/// connection it just dequeued. Dropping that future mid-handshake
/// drops the dequeued connection; the connection cannot be reclaimed
/// (it's already off the per-transport completed channel). Running
/// here means cancellation only happens on engine shutdown.
fn spawn_accept_loop<T: crate::transport::Transport + 'static>(
    transport: Arc<T>,
    next_conn_id: Arc<Mutex<u64>>,
    env: crate::peer::PeerEnv,
    inbound_tx: mpsc::UnboundedSender<InboundEvent>,
) where
    T::Connection: 'static,
{
    crate::spawn::spawn_local(async move {
        loop {
            match transport.accept().await {
                Ok(conn) => {
                    let conn_id = {
                        let mut n = next_conn_id.lock().await;
                        let id = *n;
                        *n += 1;
                        ConnectionId(id)
                    };
                    spawn_run_peer(conn, env.clone(), conn_id, inbound_tx.clone(), None);
                }
                Err(e) => {
                    // A single accept failure (e.g. an upstream pump
                    // that's shutting down) must not tear down the
                    // engine — log and keep accepting. If the channel
                    // underneath has truly closed, every subsequent
                    // accept will return an error too; that's fine —
                    // eventually the engine task is aborted by the host.
                    tracing::warn!(error = %e, "transport accept failed; continuing");
                }
            }
        }
    });
}

/// A command sent from the public API into the running engine.
pub(crate) enum EngineCommand {
    AddPeer {
        addr: PeerAddr,
        ack: oneshot::Sender<Result<(PeerId, crate::transport::TransportKind)>>,
    },
    SubscribeVia {
        filter: Filter,
        provider: PeerId,
        policy: crate::routing::SubscriptionPolicy,
        ack: oneshot::Sender<Result<()>>,
    },
    UnsubscribeVia {
        filter: Filter,
        provider: PeerId,
        ack: oneshot::Sender<Result<()>>,
    },
    Subscribe {
        filter: Filter,
        policy: crate::routing::SubscriptionPolicy,
        ack: oneshot::Sender<Result<()>>,
    },
    Unsubscribe {
        filter: Filter,
        ack: oneshot::Sender<Result<()>>,
    },
    SetTrust {
        trust: TrustSet,
        ack: oneshot::Sender<Result<()>>,
    },
    RemovePeer {
        peer_id: PeerId,
        ack: oneshot::Sender<Result<()>>,
    },
}

/// Lifecycle events emitted by the engine. Subscribers receive
/// every event from the moment they subscribe; events emitted
/// before subscription are NOT replayed.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    PeerAdded {
        peer_id: PeerId,
        kind: crate::transport::TransportKind,
    },
    PeerRemoved {
        peer_id: PeerId,
    },
    /// A liveness `Pong` round-tripped from a connected peer. Carries
    /// the measured RTT and the wall-clock time the Pong was observed.
    /// Subscribers (e.g. `PeerSupervisor`) use this to surface live
    /// per-peer health to applications. Fired once per heartbeat per
    /// peer (default cadence: every `heartbeat_interval`, 15 s).
    PongObserved {
        peer_id: PeerId,
        rtt_ms: u64,
        observed_at_unix_ms: u64,
    },
    /// A remote peer's `SubscriptionEntry::Active` naming us as the
    /// provider has been replicated to us and accepted: from this point
    /// on, application entries matching `filter` written by anyone we
    /// trust will be forwarded to `receiver` over the routing-filtered
    /// push path. Fired exactly once per `(receiver, filter_hash)`
    /// transition from "not armed" to "armed", AFTER the matching
    /// `DigestRequest` has been queued to the receiver (so an observer
    /// who sees this event is guaranteed the backfill exchange is in
    /// flight or has completed). Self-authored subscription entries
    /// also fire this event so an engine can observe its own
    /// arming for tests.
    PeerInterestArmed {
        receiver: PeerId,
        filter: sunset_store::Filter,
    },
    /// A remote peer's `SubscriptionEntry::Withdrawn` for a previously-
    /// armed filter has been replicated to us and accepted: from this
    /// point on, entries matching `filter` will no longer be forwarded
    /// to `receiver`. Fired exactly once per `(receiver, filter_hash)`
    /// transition from "armed" to "not armed". The receiver may still
    /// have other independent filters armed.
    PeerInterestWithdrawn {
        receiver: PeerId,
        filter: sunset_store::Filter,
    },
}

/// Per-peer connection state. Bundles outbound channel, transport identity,
/// the per-peer task shutdown handle, and the inbound interests (what this
/// peer currently wants from me).
///
/// `conn_id` is checked when handling `InboundEvent::Disconnected` so a
/// stale event from a defunct connection can't tear down a fresh one.
pub(crate) struct PeerSession {
    /// Identifies the connection generation that owns this outbound channel.
    /// Compared against `InboundEvent::Disconnected.conn_id` to filter stale
    /// disconnects from defunct generations (see `handle_inbound_event`).
    pub(crate) conn_id: ConnectionId,
    /// Which transport produced this connection. Surfaced to callers via
    /// `current_peers()` so a UI can render the routing state.
    pub(crate) kind: crate::transport::TransportKind,
    pub(crate) tx: mpsc::UnboundedSender<SyncMessage>,
    /// Structural shutdown handle for the per-peer task that owns this
    /// connection. When `PeerSession` is dropped (entry removed via
    /// `remove_peer` or replaced by a fresher conn's `PeerHello`), this
    /// `watch::Sender` drops, which makes
    /// `watch::Receiver::changed().await` in the per-peer task's
    /// `recv_reliable_task`, `send_task`, and `liveness_task` return
    /// `Err`. Those tasks then exit cleanly so the connection's
    /// underlying socket can release.
    ///
    /// Without this, the per-peer task's outbound channel stayed open
    /// indefinitely (a stack-scope clone of the outbound `Sender` kept
    /// it alive), and the only paths out of `send_task` were channel-
    /// close (impossible) and a failing reliable send — neither of
    /// which fires when the peer is still responsive on the wire. Each
    /// reconnect for the same `PeerId` therefore leaked one zombie
    /// `run_peer` task plus one TCP socket on the relay; over many
    /// cycles the relay would run out of file descriptors and stop
    /// accepting new WebSocket upgrades.
    pub(crate) _shutdown: tokio::sync::watch::Sender<()>,
    /// What this peer currently wants from me, keyed by `FilterHash` for
    /// O(1) Withdrawn lookups (the entry name carries the hash, not the
    /// filter). Populated by the SUBSCRIBE_PREFIX branch in
    /// handle_local_store_event; cleared with the session on peer drop.
    pub(crate) interests:
        std::collections::HashMap<crate::routing::FilterHash, sunset_store::Filter>,
}

/// Mutable state inside the engine. Held under a `tokio::sync::Mutex` so
/// command processing and per-peer task callbacks can both update it.
pub(crate) struct EngineState {
    pub trust: TrustSet,
    pub routes: crate::routing::Routes,
    /// Per-peer session state, keyed by `PeerId`. Each entry carries
    /// the sender, the connection generation, and the transport kind
    /// — kept together so a late `current_peers()` snapshot can't see
    /// a peer in one map but not the other.
    pub peer_sessions: HashMap<PeerId, PeerSession>,
    /// Live `EngineEvent` subscribers. Dead senders (closed by the
    /// receiver being dropped) are evicted lazily on the next emit.
    pub event_subs: Vec<mpsc::UnboundedSender<EngineEvent>>,
    /// Active in-process ephemeral subscribers. Each is a (filter,
    /// sender) pair; the engine dispatches a `SignedDatagram` to
    /// every subscriber whose filter matches the datagram's name.
    /// Dead senders (closed receivers) are evicted lazily on the
    /// next dispatch.
    pub ephemeral_subs: Vec<(Filter, mpsc::UnboundedSender<sunset_store::SignedDatagram>)>,
}

pub struct SyncEngine<S: Store, T: Transport> {
    pub(crate) store: Arc<S>,
    pub(crate) transport: Arc<T>,
    pub(crate) config: SyncConfig,
    /// Local peer's identity. Required for signing `_sunset-sync/subscribe`
    /// entries.
    pub(crate) local_peer: PeerId,
    pub(crate) signer: Arc<dyn Signer>,
    pub(crate) state: Arc<Mutex<EngineState>>,
    pub(crate) cmd_tx: mpsc::UnboundedSender<EngineCommand>,
    /// Held inside `run()`. `new()` creates the (tx, rx) pair; `run()`
    /// takes the rx out via Mutex<Option<...>>.
    pub(crate) cmd_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<EngineCommand>>>>,
    /// Monotonic counter for allocating `ConnectionId`s. Single-threaded
    /// (`?Send`); a `RefCell<u64>` would also work, but `Arc<Mutex<…>>` keeps
    /// the same shape as the rest of the engine state.
    pub(crate) next_conn_id: Arc<Mutex<u64>>,
}

impl<S: Store + 'static, T: Transport + 'static> SyncEngine<S, T>
where
    T::Connection: 'static,
{
    pub fn new(
        store: Arc<S>,
        transport: T,
        config: SyncConfig,
        local_peer: PeerId,
        signer: Arc<dyn Signer>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        Self {
            store,
            transport: Arc::new(transport),
            config,
            local_peer: local_peer.clone(),
            signer,
            state: Arc::new(Mutex::new(EngineState {
                trust: TrustSet::default(),
                routes: crate::routing::Routes::new(local_peer),
                peer_sessions: HashMap::new(),
                event_subs: Vec::new(),
                ephemeral_subs: Vec::new(),
            })),
            cmd_tx,
            cmd_rx: Arc::new(Mutex::new(Some(cmd_rx))),
            next_conn_id: Arc::new(Mutex::new(0)),
        }
    }

    /// Allocate a fresh `ConnectionId`. Single-writer, monotonic.
    pub(crate) async fn alloc_conn_id(&self) -> ConnectionId {
        let mut next = self.next_conn_id.lock().await;
        let id = *next;
        *next += 1;
        ConnectionId(id)
    }

    /// Snapshot of the engine-level config that every per-peer task needs.
    fn peer_env(&self) -> crate::peer::PeerEnv {
        crate::peer::PeerEnv {
            local_peer: self.local_peer.clone(),
            protocol_version: self.config.protocol_version,
            heartbeat_interval: self.config.heartbeat_interval,
            heartbeat_timeout: self.config.heartbeat_timeout,
        }
    }

    /// Initiate an outbound connection to `addr`. Returns when the connection
    /// is established + Hello-exchanged, or fails. The success value carries
    /// the peer's identity *and* its `TransportKind` so callers can record
    /// both atomically; this is what lets the supervisor populate
    /// `IntentSnapshot::kind` on the same write that flips `state` to
    /// `Connected`, removing the otherwise-racy dependency on the
    /// `EngineEvent::PeerAdded` broadcast arriving first.
    pub async fn add_peer(
        &self,
        addr: PeerAddr,
    ) -> Result<(PeerId, crate::transport::TransportKind)> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::AddPeer { addr, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Tear down the connection to `peer_id` if one exists. Drops the
    /// outbound channel; the per-peer task's send-loop drains, sends
    /// Goodbye, and closes the underlying connection. The corresponding
    /// `Disconnected` event then triggers the standard `PeerRemoved`
    /// fan-out. No-op if the peer isn't connected.
    pub async fn remove_peer(&self, peer_id: PeerId) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::RemovePeer { peer_id, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Subscribe to lifecycle events emitted by the engine. Each call
    /// returns a fresh receiver. Events are delivered to every live
    /// subscriber; subscribers receive only events that happen after
    /// they subscribe (no replay).
    pub async fn subscribe_engine_events(&self) -> mpsc::UnboundedReceiver<EngineEvent> {
        let (tx, rx) = mpsc::unbounded_channel::<EngineEvent>();
        self.state.lock().await.event_subs.push(tx);
        rx
    }

    /// Subscribe to ephemeral datagrams matching `filter`. Returns a
    /// fresh receiver. The engine dispatches a clone of every received
    /// `SignedDatagram` whose `(verifying_key, name)` matches the
    /// filter to this receiver. Subscription is in-process only; for
    /// remote peers to route ephemeral traffic to us, the caller must
    /// also publish the filter via `subscribe` / `subscribe_via` (the
    /// Bus layer does this transparently in `bus.subscribe`).
    pub async fn subscribe_ephemeral(
        &self,
        filter: Filter,
    ) -> mpsc::UnboundedReceiver<sunset_store::SignedDatagram> {
        let (tx, rx) = mpsc::unbounded_channel::<sunset_store::SignedDatagram>();
        self.state.lock().await.ephemeral_subs.push((filter, tx));
        rx
    }

    /// Publish a signed ephemeral datagram. Routes via the routing
    /// substrate (peer_sessions interests / Routes): every peer whose
    /// filter matches receives the datagram over the unreliable
    /// channel. Locally, in-process subscribers whose filter matches
    /// also receive a copy. Fire-and-forget — does NOT verify the
    /// signature on send (the caller is the signer); does NOT
    /// persist; does NOT retry. Returns `Ok(())` even if no peers
    /// match.
    pub async fn publish_ephemeral(&self, datagram: sunset_store::SignedDatagram) -> Result<()> {
        // Loopback: deliver to local subscribers first.
        self.dispatch_ephemeral_local(&datagram).await;

        // Fan-out to remote peers whose subscription filter matches.
        let msg = SyncMessage::EphemeralDelivery {
            datagram: datagram.clone(),
        };
        let state = self.state.lock().await;
        let targets: Vec<PeerId> = crate::routing::forward_targets(
            &state.peer_sessions,
            |s| &s.interests,
            &datagram.verifying_key,
            &datagram.name,
        )
        .into_iter()
        .collect();
        for peer in targets {
            if let Some(po) = state.peer_sessions.get(&peer) {
                let _ = po.tx.send(msg.clone());
            }
        }
        Ok(())
    }

    /// Snapshot the engine's currently-connected peer set with each
    /// peer's transport kind. Pairs with `subscribe_engine_events()`
    /// to seed initial state for a subscriber that joins after some
    /// peers are already connected (no-replay race).
    pub async fn current_peers(&self) -> Vec<(PeerId, crate::transport::TransportKind)> {
        let state = self.state.lock().await;
        state
            .peer_sessions
            .iter()
            .map(|(pk, po)| (pk.clone(), po.kind))
            .collect()
    }

    /// Subscribe to `filter` from one specific peer. The provider, on
    /// receiving the resulting SubscriptionEntry, starts forwarding
    /// matching store events to me; the existing DigestRequest/Exchange
    /// pipeline backfills already-stored entries.
    pub async fn subscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
        policy: crate::routing::SubscriptionPolicy,
    ) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::SubscribeVia {
                filter,
                provider,
                policy,
                ack,
            })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Withdraw a `subscribe_via(filter, provider)` subscription.
    /// Publishes `SubscriptionEntry::Withdrawn` at the same key;
    /// idempotent (returns Ok if not currently subscribed).
    pub async fn unsubscribe_via(&self, filter: Filter, provider: PeerId) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::UnsubscribeVia {
                filter,
                provider,
                ack,
            })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Declare interest in `filter` from any directly-connected peer.
    /// Implemented as an auto-resubscriber: for each currently-connected
    /// peer, calls `subscribe_via`; on future peer connects, the
    /// engine's AddPeer handler re-runs `subscribe_via` for any active
    /// intent.
    pub async fn subscribe(
        &self,
        filter: Filter,
        policy: crate::routing::SubscriptionPolicy,
    ) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::Subscribe {
                filter,
                policy,
                ack,
            })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Cancel a `subscribe()`. Idempotent.
    pub async fn unsubscribe(&self, filter: Filter) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::Unsubscribe { filter, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Replace the trust set. Subsequent inbound events are filtered
    /// against the new set.
    pub async fn set_trust(&self, trust: TrustSet) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::SetTrust { trust, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Run the engine until it's closed. This is a long-running future
    /// that drives the `select!` loop, per-peer tasks (via `spawn_local`),
    /// and the anti-entropy timer.
    ///
    /// Caller must invoke this inside a `LocalSet` (native) or directly on
    /// a single-threaded executor (WASM).
    pub async fn run(&self) -> Result<()> {
        // Take ownership of the command receiver. If `run()` is called
        // twice, the second call observes None and returns Error::Closed.
        let mut cmd_rx = self.cmd_rx.lock().await.take().ok_or(Error::Closed)?;

        // Channel for per-peer tasks to talk back to us.
        let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<InboundEvent>();

        // Walk the local store for `_sunset-sync/subscribe/*` entries
        // and populate `peer_sessions[*].interests` for any already-
        // connected peers. In practice `peer_sessions` is empty here
        // (the inbound loop hasn't started) so this is a no-op, but
        // see `bootstrap_routes` for the defensive rationale and the
        // spec link.
        self.bootstrap_routes().await?;

        // Local store subscription. Match every entry (an empty NamePrefix
        // matches all names): the engine needs to see both subscribe-
        // namespace entries (to update per-peer interests) and any
        // application entry that might match a peer's filter (for push
        // routing). Per-peer fanout is filtered downstream in
        // `handle_local_store_event`.
        let mut local_sub = self
            .store
            .subscribe(Filter::NamePrefix(Bytes::new()), Replay::None)
            .await?;

        // Anti-entropy timer. tokio::time::interval works on native; on
        // wasm32 we use the wasmtimer drop-in (browser timers via setTimeout).
        #[cfg(not(target_arch = "wasm32"))]
        let mut anti_entropy = tokio::time::interval(self.config.anti_entropy_interval);
        #[cfg(target_arch = "wasm32")]
        let mut anti_entropy = wasmtimer::tokio::interval(self.config.anti_entropy_interval);
        // First tick fires immediately; skip it so the bootstrap exchange
        // isn't duplicated immediately after PeerHello.
        anti_entropy.tick().await;

        // Routing tick: refresh subscriptions whose TTL is past half-life.
        // MissedTickBehavior::Skip avoids tick storms after a long pause
        // (e.g. WASM tab backgrounded); a single catch-up fire is enough,
        // since `due_for_refresh` scans the full set every tick anyway.
        // wasmtimer ships its own MissedTickBehavior; on WASM we have to
        // reach for that type rather than tokio's.
        #[cfg(not(target_arch = "wasm32"))]
        let mut routing_tick = {
            let mut t = tokio::time::interval(std::time::Duration::from_millis(500));
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            t
        };
        #[cfg(target_arch = "wasm32")]
        let mut routing_tick = {
            let mut t = wasmtimer::tokio::interval(std::time::Duration::from_millis(500));
            t.set_missed_tick_behavior(wasmtimer::tokio::MissedTickBehavior::Skip);
            t
        };
        // Burn the initial immediate-fire tick so the first real fire is one period out.
        routing_tick.tick().await;

        // Spawn the accept loop in its own task so its in-flight
        // post-accept work (e.g. NoiseTransport's IK handshake on the
        // raw connection that `transport.accept().await` just returned)
        // is NOT a `tokio::select!` arm in the engine's main loop.
        //
        // Why this matters: `select!` cancels every other branch the
        // moment any one branch returns Ready. If the accept arm sat in
        // `select!`, then any unrelated wakeup (a peer's PeerHello
        // landing on `inbound_rx`, an outbound command on `cmd_rx`, an
        // anti-entropy tick) while the secondary transport's Noise
        // responder was awaiting msg1 would cancel the accept future
        // mid-handshake. The dequeued `WebRtcRawConnection` (already
        // taken off the per-transport `completed_rx`) would be dropped
        // along with it, and that connection is gone for good — the
        // remote peer's dial sits in `Connecting` until its
        // `engine.add_peer` await eventually times out (or, for
        // `WebRtcRawTransport::connect`, never).
        //
        // Running accept in its own task isolates it from the select!:
        // dequeue + Noise + run_peer spawn all happen without competing
        // for poll cycles with the engine's other branches.
        spawn_accept_loop(
            self.transport.clone(),
            self.next_conn_id.clone(),
            self.peer_env(),
            inbound_tx.clone(),
        );

        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    self.handle_command(cmd, &inbound_tx).await;
                }
                Some(event) = inbound_rx.recv() => {
                    self.handle_inbound_event(event).await;
                }
                Some(item) = local_sub.next() => {
                    match item {
                        Ok(ev) => self.handle_local_store_event(ev).await,
                        Err(e) => return Err(Error::Store(e)),
                    }
                }
                _ = anti_entropy.tick() => {
                    self.tick_anti_entropy().await;
                }
                _ = routing_tick.tick() => {
                    let now_ms = web_time::SystemTime::now()
                        .duration_since(web_time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let due = {
                        let state = self.state.lock().await;
                        state.routes.due_for_refresh(now_ms)
                    };
                    for key in due {
                        if let Err(e) = self.republish_subscription(&key).await {
                            tracing::warn!(?e, ?key, "subscription refresh failed");
                        }
                    }
                }
            }
        }
    }

    async fn tick_anti_entropy(&self) {
        // Snapshot before per-peer fires; send_filter_digest re-acquires
        // the lock and skips peers that disconnected mid-tick.
        let peers: Vec<PeerId> = {
            let state = self.state.lock().await;
            state.peer_sessions.keys().cloned().collect()
        };
        if peers.is_empty() {
            return;
        }
        for peer in &peers {
            self.fan_out_digests_to_peer(peer).await;
        }
    }

    pub(crate) async fn handle_command(
        &self,
        cmd: EngineCommand,
        inbound_tx: &mpsc::UnboundedSender<InboundEvent>,
    ) {
        match cmd {
            EngineCommand::AddPeer { addr, ack } => {
                // Spawn the connect+spawn_peer chain as a background task
                // so the engine's `select!` loop stays responsive during
                // the handshake. This is load-bearing for transports
                // whose `connect()` depends on the engine making forward
                // progress (e.g. WebRTC, where SDP/ICE flows over the
                // existing CRDT replication).
                let transport = self.transport.clone();
                let env = self.peer_env();
                let inbound_tx = inbound_tx.clone();
                let conn_id = self.alloc_conn_id().await;
                crate::spawn::spawn_local(async move {
                    let connect_res = transport.connect(addr).await;
                    let r = match connect_res {
                        Ok(conn) => {
                            let (hello_tx, hello_rx) = oneshot::channel::<
                                Result<(PeerId, crate::transport::TransportKind)>,
                            >();
                            spawn_run_peer(conn, env, conn_id, inbound_tx, Some(hello_tx));
                            // Wait for the Hello exchange to complete.
                            match hello_rx.await {
                                Ok(Ok(pair)) => Ok(pair),
                                Ok(Err(e)) => Err(e),
                                Err(_) => Err(Error::Closed),
                            }
                        }
                        Err(e) => Err(e),
                    };
                    let _ = ack.send(r);
                });
            }
            EngineCommand::SubscribeVia {
                filter,
                provider,
                policy,
                ack,
            } => {
                let r = self.do_subscribe_via(filter, provider, policy).await;
                let _ = ack.send(r);
            }
            EngineCommand::UnsubscribeVia {
                filter,
                provider,
                ack,
            } => {
                let r = self.do_unsubscribe_via(filter, provider).await;
                let _ = ack.send(r);
            }
            EngineCommand::Subscribe {
                filter,
                policy,
                ack,
            } => {
                let r = self.do_subscribe(filter, policy).await;
                let _ = ack.send(r);
            }
            EngineCommand::Unsubscribe { filter, ack } => {
                let r = self.do_unsubscribe(filter).await;
                let _ = ack.send(r);
            }
            EngineCommand::SetTrust { trust, ack } => {
                self.state.lock().await.trust = trust;
                let _ = ack.send(Ok(()));
            }
            EngineCommand::RemovePeer { peer_id, ack } => {
                let removed = {
                    let mut state = self.state.lock().await;
                    state.peer_sessions.remove(&peer_id).is_some()
                };
                if removed {
                    self.emit_engine_event(EngineEvent::PeerRemoved { peer_id })
                        .await;
                }
                let _ = ack.send(Ok(()));
            }
        }
    }

    async fn handle_inbound_event(&self, event: InboundEvent) {
        match event {
            InboundEvent::PeerHello {
                peer_id,
                conn_id,
                kind,
                out_tx,
                shutdown,
                registered,
            } => {
                // Register the outbound state under the Hello-declared
                // peer_id. The insert here implicitly drops any previous
                // PeerSession for the same peer_id (e.g. a stale
                // generation from before a reconnect). That drop fires
                // the OLD `_shutdown` watch::Sender, telling the OLD
                // run_peer's tasks to wind down — see the field doc on
                // `PeerSession::_shutdown`.
                {
                    let mut state = self.state.lock().await;
                    state.peer_sessions.insert(
                        peer_id.clone(),
                        PeerSession {
                            conn_id,
                            kind,
                            tx: out_tx,
                            _shutdown: shutdown,
                            interests: std::collections::HashMap::new(),
                        },
                    );
                }
                // Auto-resubscriber: replay every current BroadcastIntent
                // for this newly-connected peer. Errors are logged-and-
                // continued; failing AddPeer because a broadcast intent
                // couldn't bind would be worse than the inconsistency
                // (the next refresh tick will surface persistent failures).
                let intents: Vec<crate::routing::BroadcastIntent> = {
                    let state = self.state.lock().await;
                    state.routes.broadcast_intents.values().cloned().collect()
                };
                for intent in intents {
                    if let Err(e) = self
                        .do_subscribe_via(intent.filter, peer_id.clone(), intent.policy)
                        .await
                    {
                        tracing::warn!(?e, ?peer_id, "auto-resubscribe failed on new peer");
                    }
                }
                self.emit_engine_event(EngineEvent::PeerAdded {
                    peer_id: peer_id.clone(),
                    kind,
                })
                .await;
                // Wake the `add_peer().await` caller now that
                // `peer_sessions` is populated, so an immediately-
                // following `subscribe` / `insert` lands a peer to
                // push to. We also pass `kind` through the
                // oneshot so the supervisor can write
                // `(peer_id, kind)` atomically — without it, the
                // supervisor would have to wait for the separate
                // `EngineEvent::PeerAdded` broadcast to populate
                // `IntentSnapshot::kind`, which races against
                // `spawn_dial`'s post-await borrow_mut.
                if let Some(s) = registered {
                    let _ = s.send(Ok((peer_id.clone(), kind)));
                }
                self.fan_out_digests_to_peer(&peer_id).await;
            }
            InboundEvent::Message { from, message } => {
                self.handle_peer_message(from, message).await;
            }
            InboundEvent::Disconnected {
                peer_id,
                conn_id,
                reason,
            } => {
                tracing::info!(
                    peer_id = ?peer_id,
                    conn_id = %conn_id,
                    reason = %reason,
                    "peer disconnected",
                );
                let removed = {
                    let mut state = self.state.lock().await;
                    match state.peer_sessions.get(&peer_id) {
                        Some(po) if po.conn_id == conn_id => {
                            state.peer_sessions.remove(&peer_id);
                            true
                        }
                        _ => false,
                    }
                };
                if removed {
                    self.emit_engine_event(EngineEvent::PeerRemoved { peer_id })
                        .await;
                }
            }
            InboundEvent::PongObserved {
                peer_id,
                rtt_ms,
                observed_at_unix_ms,
            } => {
                self.emit_engine_event(EngineEvent::PongObserved {
                    peer_id,
                    rtt_ms,
                    observed_at_unix_ms,
                })
                .await;
            }
        }
    }

    /// Walk the local store for `_sunset-sync/subscribe/*` entries at
    /// startup and, for each `Active { provider == me }`, populate the
    /// matching peer's `interests` *if that peer is already in
    /// `peer_sessions`*. Otherwise skip — `handle_local_store_event`'s
    /// SUBSCRIBE_PREFIX branch will re-fire for that entry when the
    /// receiver connects and the PeerHello bootstrap digest exchange
    /// replicates it back to us.
    ///
    /// In current Phase 2 code `peer_sessions` is always empty when
    /// `run()` first invokes this, so the scan is unconditionally a
    /// no-op in practice — but the function matches the spec's
    /// described behaviour (see
    /// `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`,
    /// "Bootstrap") for defensive hygiene against future code paths
    /// that pre-populate `peer_sessions`, and because reading
    /// SUBSCRIBE_PREFIX entries on startup is the right thing to do
    /// regardless.
    ///
    /// `my_subs` and `broadcast_intents` are NOT rehydrated from disk
    /// in Phase 2 (subsystems re-call subscribe on startup).
    async fn bootstrap_routes(&self) -> Result<()> {
        let mut iter = self
            .store
            .iter(Filter::NamePrefix(Bytes::from_static(
                crate::routing::SUBSCRIBE_PREFIX,
            )))
            .await?;
        while let Some(entry_result) = iter.next().await {
            let Ok(entry) = entry_result else {
                continue;
            };
            // Reuse the same parsing path as handle_local_store_event.
            let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await else {
                continue;
            };
            let Ok(sub_entry) =
                postcard::from_bytes::<crate::routing::SubscriptionEntry>(&block.data)
            else {
                continue;
            };
            let Some(filter_hash) = crate::routing::decode_filter_hash_from_name(&entry.name)
            else {
                continue;
            };
            let receiver = PeerId(entry.verifying_key.clone());
            if let crate::routing::SubscriptionEntry::Active { filter, provider } = sub_entry
                && provider == self.local_peer
            {
                let mut state = self.state.lock().await;
                if let Some(session) = state.peer_sessions.get_mut(&receiver) {
                    session.interests.insert(filter_hash, filter);
                }
                // Else: receiver not connected yet; their entry will
                // re-fire through handle_local_store_event when they
                // connect and bootstrap digest exchange delivers it.
            }
        }
        Ok(())
    }

    /// Send the bootstrap digest to `peer`. The bootstrap filter covers
    /// the `_sunset-sync/subscribe/` namespace so a (re)connected peer
    /// rehydrates its view of our outstanding per-(filter, provider)
    /// subscription entries. Application-data digests are driven on
    /// demand by `do_subscribe_via` / `do_subscribe` and by inbound
    /// `SubscriptionEntry` replay.
    async fn fan_out_digests_to_peer(&self, peer: &PeerId) {
        let bootstrap = &self.config.bootstrap_filter;
        self.send_filter_digest(peer, bootstrap).await;
    }

    /// Handle an inbound `DigestRequest`: the peer is asking us to send
    /// our digest over `filter` so it can compute the diff and push
    /// any entries we are missing. Responds by firing `send_filter_digest`
    /// back at the requesting peer, which drives the existing
    /// `handle_digest_exchange` path on their side.
    async fn handle_digest_request(&self, from: PeerId, filter: Filter, _range: DigestRange) {
        self.send_filter_digest(&from, &filter).await;
    }

    /// Send a `DigestExchange` over `filter` to `to`. Builds a Bloom
    /// of our entries matching `filter`; the receiver uses its own
    /// store + the bloom to compute "entries we have that the sender
    /// doesn't" and replies with those as `EventDelivery`. Reused
    /// for both the per-peer bootstrap exchange (filter prefix =
    /// SUBSCRIBE_PREFIX) and the post-`subscribe` catch-up exchange
    /// (filter = the just-subscribed filter).
    async fn send_filter_digest(&self, to: &PeerId, filter: &Filter) {
        let bloom = match build_digest(
            &*self.store,
            filter,
            &DigestRange::All,
            self.config.bloom_size_bits,
            self.config.bloom_hash_fns,
        )
        .await
        {
            Ok(b) => b,
            Err(_) => return,
        };
        let msg = SyncMessage::DigestExchange {
            filter: filter.clone(),
            range: DigestRange::All,
            bloom: bloom.to_bytes(),
        };
        let state = self.state.lock().await;
        if let Some(po) = state.peer_sessions.get(to) {
            let _ = po.tx.send(msg);
        }
    }

    async fn handle_peer_message(&self, from: PeerId, message: SyncMessage) {
        match message {
            SyncMessage::EventDelivery { entries, blobs } => {
                self.handle_event_delivery(from, entries, blobs).await;
            }
            SyncMessage::BlobRequest { hash } => {
                self.handle_blob_request(from, hash).await;
            }
            SyncMessage::BlobResponse { block } => {
                self.handle_blob_response(block).await;
            }
            SyncMessage::DigestExchange {
                filter,
                range,
                bloom,
            } => {
                self.handle_digest_exchange(from, filter, range, bloom)
                    .await;
            }
            SyncMessage::Fetch { .. } => {
                // v1: Fetch is a future-extension when DigestRange grows
                // beyond All; nothing to do today.
            }
            SyncMessage::EphemeralDelivery { datagram } => {
                self.handle_ephemeral_delivery(from, datagram).await;
            }
            SyncMessage::Hello { .. } | SyncMessage::Goodbye { .. } => {
                // Handled by the per-peer task; engine ignores.
            }
            SyncMessage::Ping { .. } | SyncMessage::Pong { .. } => {
                // Handled by the per-peer task's liveness loop; engine ignores.
            }
            SyncMessage::DigestRequest { filter, range } => {
                self.handle_digest_request(from, filter, range).await;
            }
        }
    }

    async fn handle_digest_exchange(
        &self,
        from: PeerId,
        filter: Filter,
        _range: DigestRange,
        bloom: Bytes,
    ) {
        let remote_bloom = BloomFilter::from_bytes(&bloom, self.config.bloom_hash_fns);
        let missing = match entries_missing_from_remote(&*self.store, &filter, &remote_bloom).await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "digest scan failed");
                return;
            }
        };
        if missing.is_empty() {
            return;
        }
        // Look up corresponding blobs (best-effort).
        let mut blobs = Vec::with_capacity(missing.len());
        for entry in &missing {
            if let Ok(Some(b)) = self.store.get_content(&entry.value_hash).await {
                blobs.push(b);
            }
        }
        let msg = SyncMessage::EventDelivery {
            entries: missing,
            blobs,
        };
        let state = self.state.lock().await;
        if let Some(po) = state.peer_sessions.get(&from) {
            let _ = po.tx.send(msg);
        }
    }

    async fn handle_blob_request(&self, from: PeerId, hash: sunset_store::Hash) {
        let block = match self.store.get_content(&hash).await {
            Ok(Some(b)) => b,
            // We don't have it (or I/O failed); drop silently in v1.
            _ => return,
        };
        let state = self.state.lock().await;
        if let Some(po) = state.peer_sessions.get(&from) {
            let _ = po.tx.send(SyncMessage::BlobResponse { block });
        }
    }

    async fn handle_blob_response(&self, block: sunset_store::ContentBlock) {
        // Idempotent insert; if we already have it, no-op.
        let _ = self.store.put_content(block).await;
    }

    async fn handle_event_delivery(
        &self,
        from: PeerId,
        entries: Vec<sunset_store::SignedKvEntry>,
        blobs: Vec<sunset_store::ContentBlock>,
    ) {
        // Trust filter — discard entries from non-trusted writers before
        // touching the store.
        let trusted: Vec<_> = {
            let state = self.state.lock().await;
            entries
                .into_iter()
                .filter(|e| state.trust.contains(&e.verifying_key))
                .collect()
        };

        // Index blobs by hash so we can look up each entry's blob in O(1).
        let blobs_by_hash: HashMap<_, _> = blobs.into_iter().map(|b| (b.hash(), b)).collect();

        for entry in trusted {
            let blob = blobs_by_hash.get(&entry.value_hash).cloned();
            let blob_was_supplied = blob.is_some();

            // We pass the blob if we have it; if not, the entry inserts as a
            // dangling ref and the engine issues a BlobRequest below.
            match self.store.insert(entry.clone(), blob).await {
                Ok(()) => {
                    // Successful insert. The store will fire an event on our
                    // local subscription, which will trigger push flow to
                    // other peers (transitive delivery).
                }
                Err(sunset_store::Error::Stale) => {
                    // Already have a higher-priority version; drop silently.
                }
                Err(e) => {
                    tracing::warn!(
                        verifying_key = ?entry.verifying_key,
                        error = %e,
                        "insert failed for delivered entry",
                    );
                    continue;
                }
            }

            if !blob_was_supplied {
                // Check if we already have it (e.g., from an earlier round).
                let have = self
                    .store
                    .get_content(&entry.value_hash)
                    .await
                    .ok()
                    .flatten()
                    .is_some();
                if !have {
                    let state = self.state.lock().await;
                    if let Some(po) = state.peer_sessions.get(&from) {
                        let _ = po.tx.send(SyncMessage::BlobRequest {
                            hash: entry.value_hash,
                        });
                    }
                }
            }
        }
    }

    async fn handle_local_store_event(&self, ev: Event) {
        let entry = match ev {
            Event::Inserted(e) => e,
            Event::Replaced { new, .. } => new,
            // Expired / BlobAdded / BlobRemoved: not pushed in v1.
            _ => return,
        };

        // If this entry is a per-(filter, provider) `SubscriptionEntry`
        // naming us as the provider, mirror the receiver's interest into
        // their `PeerSession`. Forwarding consults `peer_sessions[*].interests`
        // (via `routing::forward_targets`); without this mirror, the
        // receiver would never see matching application entries.
        if entry
            .name
            .as_ref()
            .starts_with(crate::routing::SUBSCRIBE_PREFIX)
        {
            let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await else {
                return;
            };
            let Ok(sub_entry) =
                postcard::from_bytes::<crate::routing::SubscriptionEntry>(&block.data)
            else {
                tracing::warn!(
                    name = %String::from_utf8_lossy(&entry.name),
                    "malformed SubscriptionEntry value; ignoring"
                );
                return;
            };
            let Some(filter_hash) = crate::routing::decode_filter_hash_from_name(&entry.name)
            else {
                tracing::warn!(
                    name = %String::from_utf8_lossy(&entry.name),
                    "SUBSCRIBE_PREFIX entry with malformed name; ignoring"
                );
                return;
            };
            let receiver = PeerId(entry.verifying_key.clone());
            let is_self_authored = entry.verifying_key == self.local_peer.0;

            match sub_entry {
                crate::routing::SubscriptionEntry::Active { filter, provider }
                    if provider == self.local_peer =>
                {
                    let was_new = {
                        let mut state = self.state.lock().await;
                        if let Some(session) = state.peer_sessions.get_mut(&receiver) {
                            session
                                .interests
                                .insert(filter_hash, filter.clone())
                                .is_none()
                        } else {
                            // Receiver isn't connected. Their SubscriptionEntry
                            // stays in our local store; when they reconnect, the
                            // PeerHello bootstrap digest exchange (see
                            // `fan_out_digests_to_peer`) replicates it to them
                            // via EventDelivery, their store insert fires this
                            // engine's local subscription, and this branch
                            // re-runs with the peer session now present.
                            return;
                        }
                    };
                    if was_new && !is_self_authored {
                        let state = self.state.lock().await;
                        if let Some(session) = state.peer_sessions.get(&receiver) {
                            let _ = session.tx.send(SyncMessage::DigestRequest {
                                filter: filter.clone(),
                                range: DigestRange::All,
                            });
                        }
                    }
                    // Emit AFTER the DigestRequest send so a test
                    // observer that gates on this event is guaranteed
                    // the backfill exchange is queued or in flight.
                    // Fired for both self-authored (loopback through
                    // sync) and foreign-authored entries so the same
                    // gate works whether the receiver subscribes via
                    // us directly or through an intermediary relay.
                    if was_new {
                        self.emit_engine_event(EngineEvent::PeerInterestArmed {
                            receiver: receiver.clone(),
                            filter,
                        })
                        .await;
                    }
                }
                crate::routing::SubscriptionEntry::Withdrawn => {
                    let prev_filter = {
                        let mut state = self.state.lock().await;
                        state
                            .peer_sessions
                            .get_mut(&receiver)
                            .and_then(|session| session.interests.remove(&filter_hash))
                    };
                    if let Some(filter) = prev_filter {
                        self.emit_engine_event(EngineEvent::PeerInterestWithdrawn {
                            receiver: receiver.clone(),
                            filter,
                        })
                        .await;
                    }
                }
                // Active naming someone else: Phase 3 recursive subscription
                // will revisit (we may want to subscribe upstream). Phase 2
                // ignores.
                _ => {}
            }
        }

        self.fanout_application_entry(&entry).await;
    }

    /// Fan out a locally-inserted (or just-replicated-and-inserted)
    /// application entry to peer outbound channels.
    ///
    /// Two regimes:
    ///
    /// - **Self-authored** (`entry.verifying_key == self.local_peer.0`)
    ///   broadcasts to every connected peer regardless of `interests`.
    ///   This is the documented Phase 2 invariant (see
    ///   `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`,
    ///   "What stays the same"). It is load-bearing for `subscribe_via`
    ///   to take effect promptly: the `SubscriptionEntry` is itself a
    ///   self-authored write, so without this broadcast it would only
    ///   reach the named provider via anti-entropy (default 30 s) and
    ///   `subscribe_via` would have multi-tens-of-seconds latency. It
    ///   also matches the application UX: a user's own writes reach
    ///   the peers they're connected to without those peers having to
    ///   pre-announce interest.
    ///
    /// - **Foreign-authored** routes via `forward_targets`, which
    ///   consults each peer's `interests` map. This is the
    ///   relay-correctness branch: a relay MUST NOT broadcast a third
    ///   party's traffic to peers whose subscription doesn't match.
    async fn fanout_application_entry(&self, entry: &sunset_store::SignedKvEntry) {
        let blob = self
            .store
            .get_content(&entry.value_hash)
            .await
            .ok()
            .flatten();
        let msg = SyncMessage::EventDelivery {
            entries: vec![entry.clone()],
            blobs: blob.into_iter().collect(),
        };
        let state = self.state.lock().await;
        let targets: Vec<PeerId> = if entry.verifying_key == self.local_peer.0 {
            state.peer_sessions.keys().cloned().collect()
        } else {
            crate::routing::forward_targets(
                &state.peer_sessions,
                |s| &s.interests,
                &entry.verifying_key,
                &entry.name,
            )
            .into_iter()
            .collect()
        };
        for peer in targets {
            if let Some(po) = state.peer_sessions.get(&peer) {
                let _ = po.tx.send(msg.clone());
            }
        }
    }

    /// Snapshot of currently connected peers (peers for which the engine has
    /// an outbound channel, i.e. that completed `PeerHello`). Order is
    /// unspecified.
    pub async fn connected_peers(&self) -> Vec<PeerId> {
        self.state
            .lock()
            .await
            .peer_sessions
            .keys()
            .cloned()
            .collect()
    }

    /// Snapshot of `(PeerId, Filter)` for every interest a currently-
    /// connected peer has registered with us via a
    /// `_sunset-sync/subscribe/<hash>/<provider>` entry naming this
    /// engine as the provider. One row per (peer, filter); a peer
    /// subscribing to multiple disjoint namespaces produces multiple
    /// rows. Used by the relay dashboard.
    pub async fn subscriptions_snapshot(&self) -> Vec<(PeerId, Filter)> {
        let state = self.state.lock().await;
        let mut out = Vec::new();
        for (peer_id, session) in &state.peer_sessions {
            for filter in session.interests.values() {
                out.push((peer_id.clone(), filter.clone()));
            }
        }
        out
    }

    /// Wait until this engine has accepted a `SubscriptionEntry::Active`
    /// from `receiver` naming us as the provider for `filter` — i.e.
    /// from this engine's perspective, `receiver` wants matching
    /// application entries forwarded to them and the matching
    /// `DigestRequest` has already been queued. Returns `true` on
    /// observation, `false` on timeout.
    ///
    /// The snapshot of current interests and the subscription to
    /// future events happen under the same `state` lock as the emit
    /// site, so there is no race window between "already armed" and
    /// "armed later"; exactly one of those two arms will resolve.
    ///
    /// Integration tests use this as the public completion signal for
    /// `subscribe` / `subscribe_via` — once it returns `true`, the
    /// forwarding gate is open and a write that matches `filter` will
    /// be routed to `receiver` (subject to verifier / trust).
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn wait_for_peer_interest(
        &self,
        receiver: &PeerId,
        filter: &Filter,
        deadline: std::time::Duration,
    ) -> bool {
        let (already, mut events) = {
            let mut state = self.state.lock().await;
            let already = state
                .peer_sessions
                .get(receiver)
                .is_some_and(|s| s.interests.values().any(|f| f == filter));
            let (tx, rx) = mpsc::unbounded_channel::<EngineEvent>();
            state.event_subs.push(tx);
            (already, rx)
        };
        if already {
            return true;
        }
        let fut = async {
            while let Some(ev) = events.recv().await {
                if let EngineEvent::PeerInterestArmed {
                    receiver: r,
                    filter: f,
                } = ev
                    && &r == receiver
                    && &f == filter
                {
                    return true;
                }
            }
            false
        };
        tokio::time::timeout(deadline, fut).await.unwrap_or(false)
    }

    /// Mirror of [`Self::wait_for_peer_interest`] for the withdrawal
    /// transition: wait until this engine has accepted a
    /// `SubscriptionEntry::Withdrawn` from `receiver` that retracts a
    /// previously-armed `filter`. Returns `true` on observation,
    /// `false` on timeout (or if the filter was never armed in the
    /// first place — there is no withdrawal to observe).
    ///
    /// Snapshot + event subscription happen under the same lock as
    /// the emit, identical to `wait_for_peer_interest`.
    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn wait_for_peer_interest_withdrawn(
        &self,
        receiver: &PeerId,
        filter: &Filter,
        deadline: std::time::Duration,
    ) -> bool {
        let (already_withdrawn, mut events) = {
            let mut state = self.state.lock().await;
            let armed = state
                .peer_sessions
                .get(receiver)
                .is_some_and(|s| s.interests.values().any(|f| f == filter));
            let (tx, rx) = mpsc::unbounded_channel::<EngineEvent>();
            state.event_subs.push(tx);
            (!armed, rx)
        };
        if already_withdrawn {
            return true;
        }
        let fut = async {
            while let Some(ev) = events.recv().await {
                if let EngineEvent::PeerInterestWithdrawn {
                    receiver: r,
                    filter: f,
                } = ev
                    && &r == receiver
                    && &f == filter
                {
                    return true;
                }
            }
            false
        };
        tokio::time::timeout(deadline, fut).await.unwrap_or(false)
    }

    /// Test-only helper: bypass the command channel and update trust
    /// directly. Used to set up state without spinning up `run()`.
    #[cfg(test)]
    pub(crate) async fn set_trust_direct(&self, trust: TrustSet) {
        self.state.lock().await.trust = trust;
    }

    /// Fan-out an event to every live subscriber. Drops senders whose
    /// receivers have been dropped (lazy GC).
    async fn emit_engine_event(&self, ev: EngineEvent) {
        let mut state = self.state.lock().await;
        state.event_subs.retain(|tx| tx.send(ev.clone()).is_ok());
    }

    /// Fan-out a datagram to every in-process subscriber whose filter
    /// matches `(datagram.verifying_key, datagram.name)`. Drops dead
    /// senders (closed receivers) lazily.
    async fn dispatch_ephemeral_local(&self, datagram: &sunset_store::SignedDatagram) {
        let mut state = self.state.lock().await;
        state.ephemeral_subs.retain(|(filter, tx)| {
            if filter.matches(&datagram.verifying_key, &datagram.name) {
                tx.send(datagram.clone()).is_ok()
            } else {
                !tx.is_closed()
            }
        });
    }

    /// Handle an inbound `EphemeralDelivery`: verify the datagram's
    /// signature against the store's configured verifier and, on
    /// success, fan it out to every in-process subscriber whose filter
    /// matches.
    async fn handle_ephemeral_delivery(
        &self,
        from: PeerId,
        datagram: sunset_store::SignedDatagram,
    ) {
        let payload = sunset_store::canonical::datagram_signing_payload(&datagram);
        let verifier = self.store.verifier();
        if verifier
            .verify_raw(&datagram.verifying_key, &payload, &datagram.signature)
            .is_err()
        {
            tracing::debug!(from = ?from, "dropping ephemeral datagram — bad signature");
            return;
        }
        self.dispatch_ephemeral_local(&datagram).await;
    }

    /// Re-publish an active subscription to refresh its TTL. Called by the
    /// routing tick for each entry returned from `routes.due_for_refresh`.
    /// Returns Ok if the entry was already removed between scan and refresh.
    async fn republish_subscription(&self, key: &crate::routing::OutboundKey) -> Result<()> {
        let (filter, policy) = {
            let state = self.state.lock().await;
            let Some(ob) = state.routes.my_subs.get(key) else {
                return Ok(());
            };
            (ob.filter.clone(), ob.policy)
        };
        self.do_subscribe_via(filter, key.provider.clone(), policy)
            .await
    }

    /// Publish a per-pair `SubscriptionEntry::Active` for `(filter,
    /// provider)` and record the outbound in `routes.my_subs` for
    /// refresh.
    async fn do_subscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
        policy: crate::routing::SubscriptionPolicy,
    ) -> Result<()> {
        use sunset_store::canonical::signing_payload;
        use sunset_store::{ContentBlock, SignedKvEntry};

        let filter_hash = crate::routing::filter_hash(&filter);
        let name = crate::routing::subscription_name(&filter, &provider);
        let entry_value = crate::routing::SubscriptionEntry::Active {
            filter: filter.clone(),
            provider: provider.clone(),
        };
        let value = postcard::to_stdvec(&entry_value)
            .map_err(|e| Error::Decode(format!("encode SubscriptionEntry: {e}")))?;
        let block = ContentBlock {
            data: Bytes::from(value),
            references: vec![],
        };
        // `priority` must be strictly greater than the previous
        // subscription entry at the same `(verifying_key, name)` —
        // otherwise the store rejects the write as `Stale` (LWW by
        // priority). Wall-clock millisecond resolution isn't enough
        // when two subscribe/unsubscribe transitions happen back to
        // back in the same async runtime tick; force monotonicity by
        // snapping to `prev_priority + 1` whenever wall-clock is
        // behind. Read `prev` from `my_subs` (which we own as the
        // local-author side); a fresh subscribe-after-withdraw still
        // wins because the withdrawn `Outbound` was removed from
        // `my_subs` on unsubscribe.
        let key = crate::routing::OutboundKey {
            filter_hash,
            provider: provider.clone(),
        };
        let prev_published = self
            .state
            .lock()
            .await
            .routes
            .my_subs
            .get(&key)
            .map(|ob| ob.last_published_ms);
        let now_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let priority = match prev_published {
            Some(prev) if prev >= now_ms => prev.saturating_add(1),
            _ => now_ms,
        };
        let ttl_ms = policy.freshness_threshold.as_millis() as u64;
        let vk = self.signer.verifying_key();
        let value_hash = block.hash();
        let mut entry = SignedKvEntry {
            verifying_key: vk,
            name,
            value_hash,
            priority,
            expires_at: Some(priority.saturating_add(ttl_ms)),
            signature: Bytes::new(),
        };
        let payload = signing_payload(&entry);
        entry.signature = self.signer.sign(&payload);
        self.store
            .insert(entry, Some(block))
            .await
            .map_err(Error::Store)?;

        let mut state = self.state.lock().await;
        state.routes.my_subs.insert(
            key,
            crate::routing::Outbound {
                filter,
                policy,
                last_published_ms: priority,
            },
        );
        Ok(())
    }

    /// Withdraw a per-pair `SubscriptionEntry`. Idempotent: returns Ok
    /// without writing if no matching outbound is recorded.
    async fn do_unsubscribe_via(&self, filter: Filter, provider: PeerId) -> Result<()> {
        use sunset_store::canonical::signing_payload;
        use sunset_store::{ContentBlock, SignedKvEntry};

        let filter_hash = crate::routing::filter_hash(&filter);
        let key = crate::routing::OutboundKey {
            filter_hash,
            provider: provider.clone(),
        };
        let prev = {
            let mut state = self.state.lock().await;
            state.routes.my_subs.remove(&key)
        };
        let Some(prev) = prev else { return Ok(()) };
        let name = crate::routing::subscription_name(&filter, &provider);
        let entry_value = crate::routing::SubscriptionEntry::Withdrawn;
        let value = postcard::to_stdvec(&entry_value)
            .map_err(|e| Error::Decode(format!("encode SubscriptionEntry: {e}")))?;
        let block = ContentBlock {
            data: Bytes::from(value),
            references: vec![],
        };
        let now_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // See `do_subscribe_via`: priority must strictly exceed the
        // previous entry's priority at `(verifying_key, name)`, or the
        // store rejects this as Stale. Withdraw immediately after
        // subscribe (same async tick) collides at millisecond
        // resolution otherwise.
        let priority = if prev.last_published_ms >= now_ms {
            prev.last_published_ms.saturating_add(1)
        } else {
            now_ms
        };
        let ttl_ms = prev.policy.freshness_threshold.as_millis() as u64;
        let vk = self.signer.verifying_key();
        let value_hash = block.hash();
        let mut entry = SignedKvEntry {
            verifying_key: vk,
            name,
            value_hash,
            priority,
            expires_at: Some(priority.saturating_add(ttl_ms)),
            signature: Bytes::new(),
        };
        let payload = signing_payload(&entry);
        entry.signature = self.signer.sign(&payload);
        self.store
            .insert(entry, Some(block))
            .await
            .map_err(Error::Store)?;
        Ok(())
    }

    /// Declare interest in `filter` from any directly-connected peer.
    /// Records a `BroadcastIntent` and calls `subscribe_via` for every
    /// peer currently in `peer_sessions`. The auto-resubscriber hook on
    /// AddPeer (added in a later task) replays for future peer
    /// connects.
    async fn do_subscribe(
        &self,
        filter: Filter,
        policy: crate::routing::SubscriptionPolicy,
    ) -> Result<()> {
        let filter_hash = crate::routing::filter_hash(&filter);
        let peers: Vec<PeerId> = {
            let mut state = self.state.lock().await;
            state.routes.broadcast_intents.insert(
                filter_hash,
                crate::routing::BroadcastIntent {
                    filter: filter.clone(),
                    policy,
                },
            );
            state.peer_sessions.keys().cloned().collect()
        };
        for peer in peers {
            if let Err(e) = self
                .do_subscribe_via(filter.clone(), peer.clone(), policy)
                .await
            {
                tracing::warn!(?e, ?peer, "subscribe per-peer fanout failed");
            }
        }
        Ok(())
    }

    /// Cancel a `subscribe()` — clear the broadcast intent and
    /// `unsubscribe_via` every `(filter, provider)` pair it produced.
    /// Idempotent.
    async fn do_unsubscribe(&self, filter: Filter) -> Result<()> {
        let filter_hash = crate::routing::filter_hash(&filter);
        let providers: Vec<PeerId> = {
            let mut state = self.state.lock().await;
            if state
                .routes
                .broadcast_intents
                .remove(&filter_hash)
                .is_none()
            {
                return Ok(());
            }
            state
                .routes
                .my_subs
                .keys()
                .filter(|k| k.filter_hash == filter_hash)
                .map(|k| k.provider.clone())
                .collect()
        };
        for provider in providers {
            if let Err(e) = self
                .do_unsubscribe_via(filter.clone(), provider.clone())
                .await
            {
                tracing::warn!(?e, ?provider, "unsubscribe per-provider fanout failed");
            }
        }
        Ok(())
    }

    /// Test-only entry point to emit an engine event from outside the
    /// engine's own tests module. Mirrors what real engine internals do
    /// (private `emit_engine_event`); gated to match the supervisor
    /// tests' cfg so it isn't compiled (and therefore can't be flagged
    /// dead) in plain `cargo test` without `--features test-helpers`.
    #[cfg(all(test, feature = "test-helpers"))]
    pub(crate) async fn emit_engine_event_for_test(&self, ev: EngineEvent) {
        self.emit_engine_event(ev).await;
    }
}

#[cfg(all(test, feature = "test-helpers"))]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;
    use sunset_store_memory::MemoryStore;

    use crate::Signer;
    use crate::test_transport::{TestNetwork, TestTransport};
    use crate::transport::TransportKind;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    /// Test-only signer that returns a non-empty stub signature. Adequate when
    /// the receiving store uses `AcceptAllVerifier`.
    struct StubSigner {
        vk: VerifyingKey,
    }

    impl Signer for StubSigner {
        fn verifying_key(&self) -> VerifyingKey {
            self.vk.clone()
        }

        fn sign(&self, _payload: &[u8]) -> Bytes {
            Bytes::from_static(&[0u8; 64])
        }
    }

    fn make_engine(addr: &str, peer_label: &[u8]) -> SyncEngine<MemoryStore, TestTransport> {
        make_engine_with_store(addr, peer_label, Arc::new(MemoryStore::with_accept_all()))
    }

    fn make_engine_with_store(
        addr: &str,
        peer_label: &[u8],
        store: Arc<MemoryStore>,
    ) -> SyncEngine<MemoryStore, TestTransport> {
        let net = TestNetwork::new();
        let local_peer = PeerId(vk(peer_label));
        let transport = net.transport(
            local_peer.clone(),
            PeerAddr::new(Bytes::copy_from_slice(addr.as_bytes())),
        );
        let signer = Arc::new(StubSigner {
            vk: local_peer.0.clone(),
        });
        SyncEngine::new(store, transport, SyncConfig::default(), local_peer, signer)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn event_delivery_inserts_trusted_entries() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                let block = ContentBlock {
                    data: Bytes::from_static(b"hello"),
                    references: vec![],
                };
                let entry = SignedKvEntry {
                    verifying_key: vk(b"trusted-writer"),
                    name: Bytes::from_static(b"chat/k1"),
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };

                // Default trust is All; deliver directly.
                engine
                    .handle_event_delivery(
                        PeerId(vk(b"some-peer")),
                        vec![entry.clone()],
                        vec![block],
                    )
                    .await;

                let stored = engine
                    .store
                    .get_entry(&vk(b"trusted-writer"), b"chat/k1")
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(stored, entry);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn event_delivery_drops_untrusted_entries() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                let mut wl = std::collections::HashSet::new();
                wl.insert(vk(b"trusted-writer"));
                engine.set_trust_direct(TrustSet::Whitelist(wl)).await;

                let block = ContentBlock {
                    data: Bytes::from_static(b"x"),
                    references: vec![],
                };
                let entry = SignedKvEntry {
                    verifying_key: vk(b"untrusted-writer"),
                    name: Bytes::from_static(b"chat/k1"),
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };

                engine
                    .handle_event_delivery(PeerId(vk(b"some-peer")), vec![entry], vec![block])
                    .await;

                let result = engine
                    .store
                    .get_entry(&vk(b"untrusted-writer"), b"chat/k1")
                    .await
                    .unwrap();
                assert!(result.is_none(), "untrusted entry should not be stored");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blob_request_returns_existing_block() {
        use sunset_store::{ContentBlock, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                let block = ContentBlock {
                    data: Bytes::from_static(b"data"),
                    references: vec![],
                };
                let hash = block.hash();
                engine.store.put_content(block.clone()).await.unwrap();

                // Pre-register a fake outbound channel so handle_blob_request has somewhere to send.
                let (tx, mut rx) = mpsc::unbounded_channel::<SyncMessage>();
                engine.state.lock().await.peer_sessions.insert(
                    PeerId(vk(b"requester")),
                    PeerSession {
                        conn_id: ConnectionId::for_test(99),
                        kind: TransportKind::Unknown,
                        tx,
                        _shutdown: tokio::sync::watch::channel(()).0,
                        interests: std::collections::HashMap::new(),
                    },
                );

                engine
                    .handle_blob_request(PeerId(vk(b"requester")), hash)
                    .await;

                let response = rx.recv().await.unwrap();
                match response {
                    SyncMessage::BlobResponse { block: got } => assert_eq!(got, block),
                    other => panic!("expected BlobResponse, got {other:?}"),
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blob_response_stores_block() {
        use sunset_store::{ContentBlock, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                let block = ContentBlock {
                    data: Bytes::from_static(b"data"),
                    references: vec![],
                };
                let hash = block.hash();
                engine.handle_blob_response(block.clone()).await;
                let got = engine.store.get_content(&hash).await.unwrap();
                assert_eq!(got, Some(block));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn digest_exchange_pushes_missing_entries_to_remote() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                let block = ContentBlock {
                    data: Bytes::from_static(b"x"),
                    references: vec![],
                };
                let entry = SignedKvEntry {
                    verifying_key: vk(b"writer"),
                    name: Bytes::from_static(b"chat/k"),
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine
                    .store
                    .insert(entry.clone(), Some(block.clone()))
                    .await
                    .unwrap();

                let (tx, mut rx) = mpsc::unbounded_channel::<SyncMessage>();
                engine.state.lock().await.peer_sessions.insert(
                    PeerId(vk(b"remote")),
                    PeerSession {
                        conn_id: ConnectionId::for_test(99),
                        kind: TransportKind::Unknown,
                        tx,
                        _shutdown: tokio::sync::watch::channel(()).0,
                        interests: std::collections::HashMap::new(),
                    },
                );

                // Remote sends an empty bloom over a filter that matches the entry.
                let empty = BloomFilter::new(4096, 4);
                engine
                    .handle_digest_exchange(
                        PeerId(vk(b"remote")),
                        Filter::Keyspace(vk(b"writer")),
                        DigestRange::All,
                        empty.to_bytes(),
                    )
                    .await;

                let msg = rx.recv().await.unwrap();
                match msg {
                    SyncMessage::EventDelivery { entries, blobs } => {
                        assert_eq!(entries.len(), 1);
                        assert_eq!(entries[0], entry);
                        assert_eq!(blobs.len(), 1);
                        assert_eq!(blobs[0], block);
                    }
                    other => panic!("expected EventDelivery, got {other:?}"),
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tick_anti_entropy_with_no_peers_is_noop() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                engine.tick_anti_entropy().await;
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn engine_event_fan_out_to_multiple_subscribers() {
        use crate::engine::EngineEvent;
        use crate::transport::TransportKind;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                let mut sub_a = engine.subscribe_engine_events().await;
                let mut sub_b = engine.subscribe_engine_events().await;

                engine
                    .emit_engine_event(EngineEvent::PeerAdded {
                        peer_id: PeerId(vk(b"bob")),
                        kind: TransportKind::Primary,
                    })
                    .await;

                let a = sub_a.recv().await.expect("sub_a got event");
                let b = sub_b.recv().await.expect("sub_b got event");
                match (a, b) {
                    (
                        EngineEvent::PeerAdded {
                            peer_id: pa,
                            kind: ka,
                        },
                        EngineEvent::PeerAdded {
                            peer_id: pb,
                            kind: kb,
                        },
                    ) => {
                        assert_eq!(pa, PeerId(vk(b"bob")));
                        assert_eq!(pb, PeerId(vk(b"bob")));
                        assert_eq!(ka, TransportKind::Primary);
                        assert_eq!(kb, TransportKind::Primary);
                    }
                    _ => panic!("expected PeerAdded events"),
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn engine_event_drops_dead_subscriber() {
        use crate::engine::EngineEvent;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                let sub_a = engine.subscribe_engine_events().await;
                drop(sub_a);

                engine
                    .emit_engine_event(EngineEvent::PeerRemoved {
                        peer_id: PeerId(vk(b"bob")),
                    })
                    .await;

                assert!(engine.state.lock().await.event_subs.is_empty());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_ephemeral_delivery_drops_bad_signature() {
        use sunset_store::SignedDatagram;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Use a verifier that rejects everything to simulate
                // signature failure deterministically.
                let store = Arc::new(MemoryStore::new(Arc::new(RejectAllVerifier)));
                let engine = Rc::new(make_engine_with_store("alice", b"alice", store));

                let mut sub = engine
                    .subscribe_ephemeral(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                    .await;

                engine
                    .handle_ephemeral_delivery(
                        PeerId(vk(b"bob")),
                        SignedDatagram {
                            verifying_key: vk(b"bob"),
                            name: Bytes::from_static(b"voice/bob/0001"),
                            payload: Bytes::from_static(b"forged"),
                            signature: Bytes::from_static(&[0u8; 64]),
                        },
                    )
                    .await;

                let got =
                    tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await;
                assert!(got.is_err(), "bad-signature datagram must be dropped");
            })
            .await;
    }

    /// Verifier that rejects every signature. Used to test the
    /// "drop on bad signature" path deterministically.
    struct RejectAllVerifier;

    impl sunset_store::SignatureVerifier for RejectAllVerifier {
        fn verify(
            &self,
            _entry: &sunset_store::SignedKvEntry,
        ) -> std::result::Result<(), sunset_store::Error> {
            Err(sunset_store::Error::SignatureInvalid)
        }
        fn verify_raw(
            &self,
            _vk: &VerifyingKey,
            _payload: &[u8],
            _sig: &[u8],
        ) -> std::result::Result<(), sunset_store::Error> {
            Err(sunset_store::Error::SignatureInvalid)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publish_ephemeral_loopback_delivers_to_local_subscriber() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                let mut sub = engine
                    .subscribe_ephemeral(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                    .await;

                let datagram = sunset_store::SignedDatagram {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(b"voice/alice/0001"),
                    payload: Bytes::from_static(b"frame"),
                    signature: Bytes::from_static(&[0u8; 64]),
                };

                engine.publish_ephemeral(datagram.clone()).await.unwrap();

                let got = sub.recv().await.expect("loopback delivery");
                assert_eq!(got, datagram);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publish_ephemeral_skips_subscriber_whose_filter_does_not_match() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                let mut sub = engine
                    .subscribe_ephemeral(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                    .await;

                let datagram = sunset_store::SignedDatagram {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(b"chat/alice/0001"),
                    payload: Bytes::from_static(b"frame"),
                    signature: Bytes::from_static(&[0u8; 64]),
                };

                engine.publish_ephemeral(datagram).await.unwrap();

                let got =
                    tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await;
                assert!(
                    got.is_err(),
                    "subscriber must NOT receive a non-matching datagram"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_drains_set_trust_command() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                let h = crate::spawn::spawn_local({
                    let engine = engine.clone();
                    async move { engine.run().await }
                });
                let mut wl = std::collections::HashSet::new();
                wl.insert(vk(b"trusted"));
                engine
                    .set_trust(TrustSet::Whitelist(wl.clone()))
                    .await
                    .unwrap();
                let s = engine.state.lock().await;
                assert_eq!(s.trust, TrustSet::Whitelist(wl));
                drop(s);
                // The engine holds the only cmd_tx; we can't drop it from
                // outside. Abort the task to terminate run().
                h.abort();
                let _ = h.await;
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_disconnected_from_old_connection_is_filtered() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                // Simulate two generations: register peer with conn_id=1,
                // then replace with conn_id=2, then deliver a stale
                // Disconnected for conn_id=1.
                let peer = PeerId(vk(b"bob"));

                // Generation 1.
                let (tx1, _rx1) = mpsc::unbounded_channel::<SyncMessage>();
                let conn1 = ConnectionId::for_test(1);
                engine.state.lock().await.peer_sessions.insert(
                    peer.clone(),
                    PeerSession {
                        conn_id: conn1,
                        kind: TransportKind::Unknown,
                        tx: tx1,
                        _shutdown: tokio::sync::watch::channel(()).0,
                        interests: std::collections::HashMap::new(),
                    },
                );

                // Replace with generation 2 (simulating a fresh PeerHello).
                let (tx2, mut rx2) = mpsc::unbounded_channel::<SyncMessage>();
                let conn2 = ConnectionId::for_test(2);
                engine.state.lock().await.peer_sessions.insert(
                    peer.clone(),
                    PeerSession {
                        conn_id: conn2,
                        kind: TransportKind::Unknown,
                        tx: tx2,
                        _shutdown: tokio::sync::watch::channel(()).0,
                        interests: std::collections::HashMap::new(),
                    },
                );

                // Subscribe to engine events to assert NO PeerRemoved fires.
                let mut events = engine.subscribe_engine_events().await;

                // Deliver a stale Disconnected for the old generation.
                engine
                    .handle_inbound_event(InboundEvent::Disconnected {
                        peer_id: peer.clone(),
                        conn_id: conn1,
                        reason: "stale".into(),
                    })
                    .await;

                // No PeerRemoved should arrive within a short timeout.
                let got =
                    tokio::time::timeout(std::time::Duration::from_millis(50), events.recv()).await;
                assert!(
                    got.is_err(),
                    "stale Disconnected for old conn must NOT emit PeerRemoved"
                );

                // The fresh sender (gen 2) must still be live: a manual
                // send through it should succeed.
                let state = engine.state.lock().await;
                let po = state.peer_sessions.get(&peer).expect("gen2 still present");
                assert_eq!(po.conn_id, conn2);
                let _ = po.tx.send(SyncMessage::Goodbye {});
                drop(state);
                let received = rx2.recv().await.expect("gen2 sender still alive");
                assert!(matches!(received, SyncMessage::Goodbye { .. }));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn matching_disconnected_removes_peer_and_emits_removed() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                let peer = PeerId(vk(b"bob"));

                let (tx, _rx) = mpsc::unbounded_channel::<SyncMessage>();
                let conn = ConnectionId::for_test(7);
                engine.state.lock().await.peer_sessions.insert(
                    peer.clone(),
                    PeerSession {
                        conn_id: conn,
                        kind: TransportKind::Unknown,
                        tx,
                        _shutdown: tokio::sync::watch::channel(()).0,
                        interests: std::collections::HashMap::new(),
                    },
                );

                let mut events = engine.subscribe_engine_events().await;

                engine
                    .handle_inbound_event(InboundEvent::Disconnected {
                        peer_id: peer.clone(),
                        conn_id: conn,
                        reason: "matching".into(),
                    })
                    .await;

                match tokio::time::timeout(std::time::Duration::from_millis(100), events.recv())
                    .await
                    .expect("PeerRemoved should fire")
                    .expect("event channel open")
                {
                    EngineEvent::PeerRemoved { peer_id } => assert_eq!(peer_id, peer),
                    other => panic!("expected PeerRemoved, got {other:?}"),
                }

                assert!(!engine.state.lock().await.peer_sessions.contains_key(&peer));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_disconnected_for_same_conn_emits_only_once() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                let peer = PeerId(vk(b"bob"));

                let (tx, _rx) = mpsc::unbounded_channel::<SyncMessage>();
                let conn = ConnectionId::for_test(7);
                engine.state.lock().await.peer_sessions.insert(
                    peer.clone(),
                    PeerSession {
                        conn_id: conn,
                        kind: TransportKind::Unknown,
                        tx,
                        _shutdown: tokio::sync::watch::channel(()).0,
                        interests: std::collections::HashMap::new(),
                    },
                );

                let mut events = engine.subscribe_engine_events().await;

                // First Disconnected → emits PeerRemoved.
                engine
                    .handle_inbound_event(InboundEvent::Disconnected {
                        peer_id: peer.clone(),
                        conn_id: conn,
                        reason: "first".into(),
                    })
                    .await;

                // Second Disconnected for the SAME conn — should NOT emit again.
                engine
                    .handle_inbound_event(InboundEvent::Disconnected {
                        peer_id: peer.clone(),
                        conn_id: conn,
                        reason: "duplicate".into(),
                    })
                    .await;

                // Drain: expect exactly one PeerRemoved then nothing.
                let first =
                    tokio::time::timeout(std::time::Duration::from_millis(100), events.recv())
                        .await
                        .expect("first PeerRemoved arrives")
                        .expect("channel open");
                assert!(matches!(first, EngineEvent::PeerRemoved { .. }));

                let second =
                    tokio::time::timeout(std::time::Duration::from_millis(50), events.recv()).await;
                assert!(second.is_err(), "no second PeerRemoved");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remove_peer_drops_outbound_and_emits_removed() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                let peer = PeerId(vk(b"bob"));

                let (tx, mut rx) = mpsc::unbounded_channel::<SyncMessage>();
                let conn = ConnectionId::for_test(1);
                engine.state.lock().await.peer_sessions.insert(
                    peer.clone(),
                    PeerSession {
                        conn_id: conn,
                        kind: TransportKind::Unknown,
                        tx,
                        _shutdown: tokio::sync::watch::channel(()).0,
                        interests: std::collections::HashMap::new(),
                    },
                );

                let mut events = engine.subscribe_engine_events().await;

                // run() handles commands; spawn it.
                let h = crate::spawn::spawn_local({
                    let engine = engine.clone();
                    async move { engine.run().await }
                });

                engine.remove_peer(peer.clone()).await.unwrap();

                // PeerRemoved fires.
                match tokio::time::timeout(std::time::Duration::from_millis(100), events.recv())
                    .await
                    .expect("PeerRemoved arrives")
                    .expect("channel open")
                {
                    EngineEvent::PeerRemoved { peer_id } => assert_eq!(peer_id, peer),
                    other => panic!("expected PeerRemoved, got {other:?}"),
                }

                // The outbound sender was dropped; the receiver sees None.
                assert!(rx.recv().await.is_none());

                h.abort();
                let _ = h.await;
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pong_observed_inbound_event_propagates_as_engine_event() {
        use crate::engine::EngineEvent;
        use crate::peer::InboundEvent;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));
                let mut sub = engine.subscribe_engine_events().await;
                let pid = PeerId(vk(b"bob"));

                engine
                    .handle_inbound_event(InboundEvent::PongObserved {
                        peer_id: pid.clone(),
                        rtt_ms: 42,
                        observed_at_unix_ms: 1_700_000_000_000,
                    })
                    .await;

                let ev = tokio::time::timeout(std::time::Duration::from_millis(200), sub.recv())
                    .await
                    .expect("no engine event")
                    .expect("subscriber closed");
                match ev {
                    EngineEvent::PongObserved {
                        peer_id,
                        rtt_ms,
                        observed_at_unix_ms,
                    } => {
                        assert_eq!(peer_id, pid);
                        assert_eq!(rtt_ms, 42);
                        assert_eq!(observed_at_unix_ms, 1_700_000_000_000);
                    }
                    other => panic!("unexpected event: {other:?}"),
                }
            })
            .await;
    }

    // -- PeerInterestArmed / PeerInterestWithdrawn substrate tests --
    //
    // These exercise the public completion-signal API
    // (`wait_for_peer_interest{_withdrawn}` + the underlying
    // `EngineEvent::PeerInterest…` variants) end-to-end through a
    // realistic two-engine fixture: the engine under test observes a
    // remote peer's `SubscriptionEntry::Active` / `Withdrawn` via
    // sync's normal replication path. They are the contract the
    // integration tests in `tests/phase2_subscribe.rs` (and similar)
    // depend on; if these regress, all those gates degrade silently to
    // sleeps.

    /// Spin up alice + bob over `TestNetwork`, run both engines, and
    /// dial bob → alice. Returns the engines, their peer ids, and the
    /// join handles so the caller can `.abort()` on exit.
    async fn spawn_pair() -> (
        Rc<SyncEngine<MemoryStore, TestTransport>>,
        Rc<SyncEngine<MemoryStore, TestTransport>>,
        PeerId,
        PeerId,
        tokio::task::JoinHandle<Result<()>>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        use crate::types::PeerAddr;
        let net = TestNetwork::new();
        let alice_addr = PeerAddr::new("alice");
        let bob_addr = PeerAddr::new("bob");
        let alice_id = PeerId(vk(b"alice"));
        let bob_id = PeerId(vk(b"bob"));

        let alice_transport = net.transport(alice_id.clone(), alice_addr.clone());
        let bob_transport = net.transport(bob_id.clone(), bob_addr);

        let alice_store = Arc::new(MemoryStore::with_accept_all());
        let bob_store = Arc::new(MemoryStore::with_accept_all());

        let alice_signer = Arc::new(StubSigner {
            vk: alice_id.0.clone(),
        });
        let bob_signer = Arc::new(StubSigner {
            vk: bob_id.0.clone(),
        });

        let alice = Rc::new(SyncEngine::new(
            alice_store,
            alice_transport,
            SyncConfig::default(),
            alice_id.clone(),
            alice_signer,
        ));
        let bob = Rc::new(SyncEngine::new(
            bob_store,
            bob_transport,
            SyncConfig::default(),
            bob_id.clone(),
            bob_signer,
        ));

        let alice_run = crate::spawn::spawn_local({
            let e = alice.clone();
            async move { e.run().await }
        });
        let bob_run = crate::spawn::spawn_local({
            let e = bob.clone();
            async move { e.run().await }
        });

        bob.add_peer(alice_addr).await.unwrap();

        (alice, bob, alice_id, bob_id, alice_run, bob_run)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn peer_interest_armed_fires_once_per_subscribe() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (alice, bob, _alice_id, bob_id, alice_run, bob_run) = spawn_pair().await;
                let mut events = alice.subscribe_engine_events().await;
                let filter = Filter::Keyspace(vk(b"chat"));

                bob.subscribe_via(
                    filter.clone(),
                    PeerId(vk(b"alice")),
                    crate::routing::SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

                // Drain events until we see PeerInterestArmed for
                // (bob, filter). It must fire exactly once for this
                // transition; assert by counting matches inside a
                // bounded window.
                let mut armed_count = 0;
                let deadline = std::time::Duration::from_secs(2);
                let _ = tokio::time::timeout(deadline, async {
                    while let Some(ev) = events.recv().await {
                        if let EngineEvent::PeerInterestArmed {
                            receiver,
                            filter: f,
                        } = ev
                            && receiver == bob_id
                            && f == filter
                        {
                            armed_count += 1;
                            // Wait a bit longer to catch a second emit
                            // if the engine ever double-fires.
                            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                            break;
                        }
                    }
                })
                .await;
                assert_eq!(armed_count, 1, "PeerInterestArmed must fire exactly once");

                alice_run.abort();
                bob_run.abort();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn peer_interest_withdrawn_fires_once_per_unsubscribe() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (alice, bob, _alice_id, bob_id, alice_run, bob_run) = spawn_pair().await;
                let filter = Filter::Keyspace(vk(b"chat"));

                bob.subscribe_via(
                    filter.clone(),
                    PeerId(vk(b"alice")),
                    crate::routing::SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();
                assert!(
                    alice
                        .wait_for_peer_interest(&bob_id, &filter, std::time::Duration::from_secs(2))
                        .await,
                    "alice did not arm bob's interest"
                );

                let mut events = alice.subscribe_engine_events().await;

                bob.unsubscribe_via(filter.clone(), PeerId(vk(b"alice")))
                    .await
                    .unwrap();

                let mut withdrawn_count = 0;
                let deadline = std::time::Duration::from_secs(2);
                let _ = tokio::time::timeout(deadline, async {
                    while let Some(ev) = events.recv().await {
                        if let EngineEvent::PeerInterestWithdrawn {
                            receiver,
                            filter: f,
                        } = ev
                            && receiver == bob_id
                            && f == filter
                        {
                            withdrawn_count += 1;
                            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                            break;
                        }
                    }
                })
                .await;
                assert_eq!(
                    withdrawn_count, 1,
                    "PeerInterestWithdrawn must fire exactly once"
                );

                alice_run.abort();
                bob_run.abort();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_for_peer_interest_returns_immediately_when_already_armed() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (alice, bob, _alice_id, bob_id, alice_run, bob_run) = spawn_pair().await;
                let filter = Filter::Keyspace(vk(b"chat"));

                bob.subscribe_via(
                    filter.clone(),
                    PeerId(vk(b"alice")),
                    crate::routing::SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();
                assert!(
                    alice
                        .wait_for_peer_interest(&bob_id, &filter, std::time::Duration::from_secs(2))
                        .await,
                    "alice did not arm bob's interest"
                );

                // Second call must observe the snapshot path (already
                // armed) and return immediately — never time out.
                let start = tokio::time::Instant::now();
                let result = alice
                    .wait_for_peer_interest(&bob_id, &filter, std::time::Duration::from_secs(5))
                    .await;
                assert!(result, "second wait_for_peer_interest should succeed");
                assert!(
                    start.elapsed() < std::time::Duration::from_millis(100),
                    "wait_for_peer_interest must return immediately when already armed"
                );

                alice_run.abort();
                bob_run.abort();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_for_peer_interest_resolves_on_later_emit() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (alice, bob, _alice_id, bob_id, alice_run, bob_run) = spawn_pair().await;
                let filter = Filter::Keyspace(vk(b"chat"));

                // Start waiting BEFORE bob subscribes — exercises the
                // event-channel path (not the snapshot path).
                let alice_for_wait = alice.clone();
                let filter_for_wait = filter.clone();
                let bob_id_for_wait = bob_id.clone();
                let waiter = crate::spawn::spawn_local(async move {
                    alice_for_wait
                        .wait_for_peer_interest(
                            &bob_id_for_wait,
                            &filter_for_wait,
                            std::time::Duration::from_secs(2),
                        )
                        .await
                });

                bob.subscribe_via(
                    filter,
                    PeerId(vk(b"alice")),
                    crate::routing::SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

                let armed = waiter.await.expect("waiter task joined");
                assert!(armed, "waiter must resolve true on later emit");

                alice_run.abort();
                bob_run.abort();
            })
            .await;
    }

    // -- bootstrap_routes tests --
    //
    // `bootstrap_routes` walks `_sunset-sync/subscribe/*` entries in the
    // local store on startup and, for each `Active{provider == me}`,
    // records the filter into the matching peer's `interests` if that
    // peer is already in `peer_sessions`. In practice `peer_sessions` is
    // empty at startup so it's a no-op, but the function is documented
    // to populate interests when the session is present; both branches
    // are exercised below so a regression of either is caught.

    #[tokio::test(flavor = "current_thread")]
    async fn bootstrap_routes_is_noop_when_no_peer_sessions() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let store = Arc::new(MemoryStore::with_accept_all());
                let engine = Rc::new(make_engine_with_store("alice", b"alice", store.clone()));
                let local_peer = PeerId(vk(b"alice"));

                // Pre-insert an Active{provider=alice} SubscriptionEntry
                // authored by some other peer (bob), but without ever
                // adding bob to peer_sessions.
                let filter = Filter::Keyspace(vk(b"chat"));
                let sub_value = crate::routing::SubscriptionEntry::Active {
                    filter: filter.clone(),
                    provider: local_peer.clone(),
                };
                let block = ContentBlock {
                    data: Bytes::from(postcard::to_stdvec(&sub_value).unwrap()),
                    references: vec![],
                };
                let name = crate::routing::subscription_name(&filter, &local_peer);
                let entry = SignedKvEntry {
                    verifying_key: vk(b"bob"),
                    name,
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                store.insert(entry, Some(block)).await.unwrap();

                // Sanity: peer_sessions is empty.
                assert!(engine.state.lock().await.peer_sessions.is_empty());

                // Should return Ok and not panic.
                engine.bootstrap_routes().await.expect("bootstrap_routes");

                // And it must NOT have magicked up a peer_session.
                assert!(
                    engine.state.lock().await.peer_sessions.is_empty(),
                    "bootstrap_routes must not create peer_sessions"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bootstrap_routes_populates_interests_for_connected_peer() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let store = Arc::new(MemoryStore::with_accept_all());
                let engine = Rc::new(make_engine_with_store("alice", b"alice", store.clone()));
                let local_peer = PeerId(vk(b"alice"));
                let bob = PeerId(vk(b"bob"));

                // Manually install bob's peer_session so the scan has
                // somewhere to land.
                let (tx, _rx) = mpsc::unbounded_channel::<SyncMessage>();
                engine.state.lock().await.peer_sessions.insert(
                    bob.clone(),
                    PeerSession {
                        conn_id: ConnectionId::for_test(1),
                        kind: TransportKind::Unknown,
                        tx,
                        _shutdown: tokio::sync::watch::channel(()).0,
                        interests: std::collections::HashMap::new(),
                    },
                );

                // Pre-insert bob's Active{provider=alice} subscription
                // for some filter.
                let filter = Filter::Keyspace(vk(b"chat"));
                let sub_value = crate::routing::SubscriptionEntry::Active {
                    filter: filter.clone(),
                    provider: local_peer.clone(),
                };
                let block = ContentBlock {
                    data: Bytes::from(postcard::to_stdvec(&sub_value).unwrap()),
                    references: vec![],
                };
                let name = crate::routing::subscription_name(&filter, &local_peer);
                let entry = SignedKvEntry {
                    verifying_key: bob.0.clone(),
                    name: name.clone(),
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                store.insert(entry, Some(block)).await.unwrap();

                // Run the scan.
                engine.bootstrap_routes().await.expect("bootstrap_routes");

                // bob's interests should now carry the filter.
                let state = engine.state.lock().await;
                let session = state.peer_sessions.get(&bob).expect("bob's session");
                let filter_hash = crate::routing::filter_hash(&filter);
                let installed = session.interests.get(&filter_hash);
                assert_eq!(
                    installed,
                    Some(&filter),
                    "bootstrap_routes must populate interests for connected peers"
                );
            })
            .await;
    }
}
