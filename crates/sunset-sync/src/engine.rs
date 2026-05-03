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
use crate::reserved;
use crate::signer::Signer;
use crate::subscription_registry::{SubscriptionRegistry, parse_subscription_entry};
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

/// A command sent from the public API into the running engine.
pub(crate) enum EngineCommand {
    AddPeer {
        addr: PeerAddr,
        ack: oneshot::Sender<Result<(PeerId, crate::transport::TransportKind)>>,
    },
    PublishSubscription {
        filter: Filter,
        ttl: std::time::Duration,
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
}

/// Sender + connection identity for one peer's currently-active connection.
/// `conn_id` is checked when handling `InboundEvent::Disconnected` so a
/// stale event from a defunct connection can't tear down a fresh one.
pub(crate) struct PeerOutbound {
    /// Identifies the connection generation that owns this outbound channel.
    /// Compared against `InboundEvent::Disconnected.conn_id` to filter stale
    /// disconnects from defunct generations (see `handle_inbound_event`).
    pub(crate) conn_id: ConnectionId,
    pub(crate) tx: mpsc::UnboundedSender<SyncMessage>,
}

/// Mutable state inside the engine. Held under a `tokio::sync::Mutex` so
/// command processing and per-peer task callbacks can both update it.
pub(crate) struct EngineState {
    pub trust: TrustSet,
    pub registry: SubscriptionRegistry,
    /// Per-peer outbound message senders, keyed by `(peer_id, conn_id)`.
    pub peer_outbound: HashMap<PeerId, PeerOutbound>,
    /// Per-peer transport kind (Primary vs Secondary). Updated in
    /// lockstep with `peer_outbound` so callers can snapshot
    /// `(peer, kind)` after subscribing late and missing the
    /// corresponding `PeerAdded` event.
    pub peer_kinds: HashMap<PeerId, crate::transport::TransportKind>,
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
    /// All filters that the local peer has called `publish_subscription`
    /// with, deduped by `PartialEq`. The engine writes the **union** of
    /// these as the single per-peer SUBSCRIBE_NAME entry, so a client
    /// that subscribes to multiple disjoint namespaces (e.g. chat
    /// `<fp>/`, voice frames `voice/<fp>/`, voice presence
    /// `voice-presence/<fp>/`) gets all of them routed by the relay's
    /// registry. Without this accumulation, each call would clobber the
    /// previous (HashMap-per-peer in `SubscriptionRegistry`), silently
    /// breaking sync for every prior subscription.
    pub(crate) own_filters: Arc<Mutex<Vec<Filter>>>,
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
            local_peer,
            signer,
            state: Arc::new(Mutex::new(EngineState {
                trust: TrustSet::default(),
                registry: SubscriptionRegistry::new(),
                peer_outbound: HashMap::new(),
                peer_kinds: HashMap::new(),
                event_subs: Vec::new(),
                ephemeral_subs: Vec::new(),
            })),
            cmd_tx,
            cmd_rx: Arc::new(Mutex::new(Some(cmd_rx))),
            next_conn_id: Arc::new(Mutex::new(0)),
            own_filters: Arc::new(Mutex::new(Vec::new())),
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
    /// also publish the filter via `publish_subscription` (the Bus
    /// layer does this transparently in `bus.subscribe`).
    pub async fn subscribe_ephemeral(
        &self,
        filter: Filter,
    ) -> mpsc::UnboundedReceiver<sunset_store::SignedDatagram> {
        let (tx, rx) = mpsc::unbounded_channel::<sunset_store::SignedDatagram>();
        self.state.lock().await.ephemeral_subs.push((filter, tx));
        rx
    }

    /// Publish a signed ephemeral datagram. Routes via the subscription
    /// registry: every peer whose filter matches receives the datagram
    /// over the unreliable channel. Locally, in-process subscribers
    /// whose filter matches also receive a copy. Fire-and-forget — does
    /// NOT verify the signature on send (the caller is the signer); does
    /// NOT persist; does NOT retry. Returns `Ok(())` even if no peers
    /// match.
    pub async fn publish_ephemeral(&self, datagram: sunset_store::SignedDatagram) -> Result<()> {
        // Loopback: deliver to local subscribers first.
        self.dispatch_ephemeral_local(&datagram).await;

        // Fan-out to remote peers whose subscription filter matches.
        let msg = SyncMessage::EphemeralDelivery {
            datagram: datagram.clone(),
        };
        let state = self.state.lock().await;
        for peer in state
            .registry
            .peers_matching(&datagram.verifying_key, &datagram.name)
        {
            if let Some(po) = state.peer_outbound.get(&peer) {
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
            .peer_kinds
            .iter()
            .map(|(pk, k)| (pk.clone(), *k))
            .collect()
    }

    /// Publish this peer's subscription filter. Writes a signed KV entry
    /// under `(local_peer, "_sunset-sync/subscribe")` with `value_hash =
    /// blake3(postcard(filter))` and priority = unix-timestamp-now,
    /// expires_at = priority + ttl.
    pub async fn publish_subscription(
        &self,
        filter: Filter,
        ttl: std::time::Duration,
    ) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::PublishSubscription { filter, ttl, ack })
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

        // Rebuild the in-memory subscription_registry from existing
        // `SUBSCRIBE_NAME` entries on disk. Persistent backends (e.g.
        // `sunset-store-fs` used by the relay) hold these across process
        // restarts; the registry must reflect that state before we start
        // routing chat traffic. Without this step, after a relay restart
        // the relay would receive forwarded chat messages but find no
        // peers to fan them out to (registry is empty), and clients would
        // never see each other's messages until they happened to publish
        // a fresh subscribe entry.
        self.replay_existing_subscriptions().await?;

        // Local store subscription. Match every entry (an empty NamePrefix
        // matches all names): the engine needs to see both subscribe-
        // namespace entries (to maintain the registry) and any application
        // entry that might match a peer's filter (for push routing). Per-
        // peer fanout is filtered downstream in `handle_local_store_event`.
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

        loop {
            tokio::select! {
                accept_res = self.transport.accept() => {
                    match accept_res {
                        Ok(conn) => self.spawn_peer(conn, inbound_tx.clone()).await,
                        Err(e) => {
                            // A single accept failure (e.g. an upstream pump that's
                            // shutting down) must not tear down the engine — log and
                            // keep accepting. If the channel underneath has truly
                            // closed, every subsequent accept will return an error
                            // too; that's fine — eventually the engine task is
                            // aborted by the host.
                            tracing::warn!(error = %e, "transport accept failed; continuing");
                        }
                    }
                }
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
            }
        }
    }

    async fn tick_anti_entropy(&self) {
        // Snapshot before per-peer fires; send_filter_digest re-acquires
        // the lock and skips peers that disconnected mid-tick.
        let peers: Vec<PeerId> = {
            let state = self.state.lock().await;
            state.peer_outbound.keys().cloned().collect()
        };
        if peers.is_empty() {
            return;
        }
        let own_filters = self.own_published_filters().await;
        for peer in &peers {
            self.fan_out_digests_to_peer(peer, &own_filters).await;
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
            EngineCommand::PublishSubscription { filter, ttl, ack } => {
                let r = self.do_publish_subscription(filter, ttl).await;
                let _ = ack.send(r);
            }
            EngineCommand::SetTrust { trust, ack } => {
                self.state.lock().await.trust = trust;
                let _ = ack.send(Ok(()));
            }
            EngineCommand::RemovePeer { peer_id, ack } => {
                let removed = {
                    let mut state = self.state.lock().await;
                    state.peer_kinds.remove(&peer_id);
                    state.peer_outbound.remove(&peer_id).is_some()
                };
                if removed {
                    self.emit_engine_event(EngineEvent::PeerRemoved { peer_id })
                        .await;
                }
                let _ = ack.send(Ok(()));
            }
        }
    }

    async fn spawn_peer(
        &self,
        conn: T::Connection,
        inbound_tx: mpsc::UnboundedSender<InboundEvent>,
    ) {
        let conn_id = self.alloc_conn_id().await;
        spawn_run_peer(conn, self.peer_env(), conn_id, inbound_tx, None);
    }

    async fn handle_inbound_event(&self, event: InboundEvent) {
        match event {
            InboundEvent::PeerHello {
                peer_id,
                conn_id,
                kind,
                out_tx,
                registered,
            } => {
                // Register the outbound sender + transport kind under the
                // Hello-declared peer_id. peer_outbound + peer_kinds move
                // together so a late `current_peers()` snapshot reflects
                // the same set as the live subscription stream.
                {
                    let mut state = self.state.lock().await;
                    state.peer_outbound.insert(
                        peer_id.clone(),
                        PeerOutbound {
                            conn_id,
                            tx: out_tx,
                        },
                    );
                    state.peer_kinds.insert(peer_id.clone(), kind);
                }
                self.emit_engine_event(EngineEvent::PeerAdded {
                    peer_id: peer_id.clone(),
                    kind,
                })
                .await;
                // Wake the `add_peer().await` caller now that
                // `peer_outbound` is populated, so an immediately-
                // following `publish_subscription` / `insert` lands a
                // peer to push to. We also pass `kind` through the
                // oneshot so the supervisor can write
                // `(peer_id, kind)` atomically — without it, the
                // supervisor would have to wait for the separate
                // `EngineEvent::PeerAdded` broadcast to populate
                // `IntentSnapshot::kind`, which races against
                // `spawn_dial`'s post-await borrow_mut.
                if let Some(s) = registered {
                    let _ = s.send(Ok((peer_id.clone(), kind)));
                }
                let own_filters = self.own_published_filters().await;
                self.fan_out_digests_to_peer(&peer_id, &own_filters).await;
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
                    match state.peer_outbound.get(&peer_id) {
                        Some(po) if po.conn_id == conn_id => {
                            state.peer_kinds.remove(&peer_id);
                            state.peer_outbound.remove(&peer_id);
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
        }
    }

    /// Walk the local store for existing `_sunset-sync/subscribe` entries
    /// and seed the in-memory `subscription_registry`. Called once at the
    /// top of `run()` so that persistent backends survive process restart
    /// without losing their routing state. Pure "read existing entries
    /// and update local in-memory state" — does NOT push events to peers
    /// (that's the live `local_sub` path's job, only for *new* inserts).
    async fn replay_existing_subscriptions(&self) -> Result<()> {
        let filter = Filter::Namespace(Bytes::from_static(reserved::SUBSCRIBE_NAME));
        let mut entries = self.store.iter(filter).await.map_err(Error::Store)?;
        while let Some(item) = entries.next().await {
            let entry = match item {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "replay_existing_subscriptions: store iteration");
                    continue;
                }
            };
            let block = match self.store.get_content(&entry.value_hash).await {
                Ok(Some(b)) => b,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "replay_existing_subscriptions: get_content");
                    continue;
                }
            };
            if let Ok(parsed_filter) = parse_subscription_entry(&entry, &block) {
                let _ = self
                    .state
                    .lock()
                    .await
                    .registry
                    .insert(entry.verifying_key, parsed_filter);
            }
        }
        Ok(())
    }

    /// Walk the local store for `_sunset-sync/subscribe` entries authored
    /// by `self.local_peer` and return their parsed filters. Used by
    /// `PeerHello` and `tick_anti_entropy` to fire per-filter digests so
    /// a (re)connected client catches up on whatever it missed under
    /// each of its own published interests.
    ///
    /// Other peers' subscribe entries are intentionally skipped — they
    /// own their own catch-up, and this engine has no signing key for
    /// them. Iteration errors and parse errors are logged-and-skipped,
    /// matching `replay_existing_subscriptions`.
    async fn own_published_filters(&self) -> Vec<Filter> {
        let mut out = Vec::new();
        let filter = Filter::Specific(
            self.local_peer.0.clone(),
            Bytes::from_static(reserved::SUBSCRIBE_NAME),
        );
        let mut entries = match self.store.iter(filter).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "own_published_filters: store iteration");
                return out;
            }
        };
        while let Some(item) = entries.next().await {
            let entry = match item {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "own_published_filters: store iteration");
                    continue;
                }
            };
            let block = match self.store.get_content(&entry.value_hash).await {
                Ok(Some(b)) => b,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "own_published_filters: get_content");
                    continue;
                }
            };
            if let Ok(parsed) = parse_subscription_entry(&entry, &block) {
                out.push(parsed);
            }
        }
        out
    }

    /// Send the bootstrap digest plus a per-own-filter digest to `peer`,
    /// skipping any own filter that equals `bootstrap_filter` so the
    /// receiver doesn't see two `DigestExchange`s for the same filter.
    async fn fan_out_digests_to_peer(&self, peer: &PeerId, own_filters: &[Filter]) {
        let bootstrap = &self.config.bootstrap_filter;
        self.send_filter_digest(peer, bootstrap).await;
        for filter in own_filters {
            if filter == bootstrap {
                continue;
            }
            self.send_filter_digest(peer, filter).await;
        }
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
    /// for both the per-peer bootstrap exchange (filter =
    /// SUBSCRIBE_NAME) and the post-`publish_subscription` catch-up
    /// exchange (filter = the just-published subscription's filter).
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
        if let Some(po) = state.peer_outbound.get(to) {
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
        if let Some(po) = state.peer_outbound.get(&from) {
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
        if let Some(po) = state.peer_outbound.get(&from) {
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
                    if let Some(po) = state.peer_outbound.get(&from) {
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

        // If this is a subscription announcement, update the registry so
        // future push routing knows about the peer's interests.
        //
        // On a new or changed filter, also backfill the peer with already-
        // stored entries that match the filter. This closes the receiver-side
        // race where third-party-authored entries arrive in our local store
        // *before* the recipient's SUBSCRIBE_NAME is parsed; without the
        // backfill, those entries sit in our store with no forwarding trigger
        // until anti-entropy fires (well past the latency budget for, e.g.,
        // WebRTC SDP signaling).
        if entry.name.as_ref() == reserved::SUBSCRIBE_NAME {
            if let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await {
                if let Ok(filter) = parse_subscription_entry(&entry, &block) {
                    let peer_vk = entry.verifying_key.clone();
                    let peer_id = PeerId(peer_vk.clone());
                    let prev = self
                        .state
                        .lock()
                        .await
                        .registry
                        .insert(peer_vk.clone(), filter.clone());
                    let filter_changed = prev.as_ref() != Some(&filter);
                    let is_self = peer_vk == self.local_peer.0;
                    if filter_changed && !is_self {
                        // Send a DigestRequest to the peer so they respond
                        // with a DigestExchange (their bloom over `filter`).
                        // The existing handle_digest_exchange path then
                        // computes the diff and pushes only the entries the
                        // peer is missing — bandwidth-efficient via bloom
                        // dedup, safe when the peer already has overlapping
                        // state (browser persistence, federated relays).
                        let msg = SyncMessage::DigestRequest {
                            filter: filter.clone(),
                            range: DigestRange::All,
                        };
                        let state = self.state.lock().await;
                        if let Some(po) = state.peer_outbound.get(&peer_id) {
                            let _ = po.tx.send(msg);
                        }
                    }
                }
            }
        }

        // Push flow.
        //
        // Self-authored entries are broadcast to every currently-connected
        // peer regardless of registry filter. The registry-driven push is
        // only safe once bootstrap-digest has primed the registry with the
        // peer's filter; for an entry inserted right after `add_peer`
        // returns (e.g., the very first `publish_subscription` of a
        // freshly-connected client), the registry may still be empty and
        // a registry-filtered push would lose the entry on the floor
        // until anti-entropy (default 30 s) caught up. My own entries
        // going to my own peers don't need filter consent — the receiving
        // peer is free to route or drop based on its own rules.
        //
        // Forwarded entries (authored by someone else) keep registry-
        // filtered semantics: a relay MUST NOT broadcast a third party's
        // chat traffic to peers whose subscription doesn't match.
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
        if entry.verifying_key == self.local_peer.0 {
            for po in state.peer_outbound.values() {
                let _ = po.tx.send(msg.clone());
            }
        } else {
            for peer in state
                .registry
                .peers_matching(&entry.verifying_key, &entry.name)
            {
                if let Some(po) = state.peer_outbound.get(&peer) {
                    let _ = po.tx.send(msg.clone());
                }
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
            .peer_outbound
            .keys()
            .cloned()
            .collect()
    }

    /// Snapshot of `(PeerId, Filter)` for every peer whose
    /// `_sunset-sync/subscribe` entry is currently in the registry.
    pub async fn subscriptions_snapshot(&self) -> Vec<(PeerId, Filter)> {
        self.state
            .lock()
            .await
            .registry
            .iter()
            .map(|(vk, f)| (PeerId(vk.clone()), f.clone()))
            .collect()
    }

    /// Test-only helper: bypass the command channel and update trust
    /// directly. Used to set up state without spinning up `run()`.
    #[cfg(test)]
    pub(crate) async fn set_trust_direct(&self, trust: TrustSet) {
        self.state.lock().await.trust = trust;
    }

    /// Test-only: true if this engine has learned the given peer's
    /// subscription filter via the bootstrap digest exchange. Available
    /// only with the `test-helpers` feature.
    #[cfg(feature = "test-helpers")]
    pub async fn knows_peer_subscription(&self, vk: &sunset_store::VerifyingKey) -> bool {
        self.state
            .lock()
            .await
            .registry
            .iter()
            .any(|(k, _)| k == vk)
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

    /// Real implementation of `publish_subscription`'s server side.
    ///
    /// Uses millisecond-precision priority (matching `presence_publisher`)
    /// so two `publish_subscription` calls separated by sub-second wall
    /// clock — most commonly a process restart sharing a `data_dir` with
    /// the previous instance, where `Relay::new` + bind + banner takes
    /// well under one second — produce monotonically-increasing priorities.
    /// On the unlikely Stale (equal- or greater-priority on-disk entry,
    /// e.g. clock-stepped backwards by NTP), retry once with
    /// `existing.priority + 1` so the call always succeeds for our own
    /// subscribe entry. The contract is "after Ok, the subscription is
    /// active"; previously, restart within the same UNIX second of the
    /// prior run silently violated that contract and caused the relay
    /// process to exit on startup.
    async fn do_publish_subscription(
        &self,
        filter: Filter,
        ttl: std::time::Duration,
    ) -> Result<()> {
        use sunset_store::canonical::signing_payload;
        use sunset_store::{ContentBlock, SignedKvEntry};

        // Accumulate this filter into the local per-engine set, then
        // publish the union of all locally-published filters. The
        // SUBSCRIBE_NAME entry carries one filter per peer (LWW); without
        // accumulation, a second `publish_subscription(other_filter)`
        // would silently clobber the first peer's prior interest at the
        // relay, breaking every higher-layer subsystem (chat / voice /
        // signaling) that has its own subscribe call.
        let union_filter = {
            let mut filters = self.own_filters.lock().await;
            if !filters.iter().any(|f| f == &filter) {
                filters.push(filter.clone());
            }
            // Single-element optimization: avoid wrapping in Union when
            // unnecessary, so the wire format stays identical to the
            // pre-accumulation behavior in the common one-subsystem case
            // and the existing test vectors aren't disturbed.
            if filters.len() == 1 {
                filters[0].clone()
            } else {
                Filter::Union(filters.clone())
            }
        };

        let value = postcard::to_stdvec(&union_filter)
            .map_err(|e| Error::Decode(format!("encode filter: {e}")))?;
        let block = ContentBlock {
            data: Bytes::from(value),
            references: vec![],
        };
        let now_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let ttl_ms = ttl.as_millis() as u64;
        let vk = self.signer.verifying_key();
        let name = Bytes::from_static(reserved::SUBSCRIBE_NAME);
        let value_hash = block.hash();

        let mut priority = now_ms;
        // At most one retry: the second iteration uses
        // `existing.priority + 1`, which is strictly greater than any
        // priority we've ever stored, so the third insert (if reached)
        // would be impossible by construction.
        let mut inserted = false;
        for _ in 0..2 {
            let mut entry = SignedKvEntry {
                verifying_key: vk.clone(),
                name: name.clone(),
                value_hash,
                priority,
                expires_at: Some(priority.saturating_add(ttl_ms)),
                signature: Bytes::new(),
            };
            let payload = signing_payload(&entry);
            entry.signature = self.signer.sign(&payload);
            match self.store.insert(entry, Some(block.clone())).await {
                Ok(()) => {
                    inserted = true;
                    break;
                }
                Err(sunset_store::Error::Stale) => {
                    let existing = self
                        .store
                        .get_entry(&vk, &name)
                        .await
                        .map_err(Error::Store)?;
                    match existing {
                        Some(e) => priority = e.priority.saturating_add(1),
                        // Stale without an existing entry shouldn't happen
                        // (LWW only rejects against an existing-or-equal
                        // priority), but propagate cleanly if it does.
                        None => return Err(Error::Store(sunset_store::Error::Stale)),
                    }
                }
                Err(e) => return Err(Error::Store(e)),
            }
        }
        if !inserted {
            // The retry loop is bounded by construction — see above.
            // This branch is unreachable; we return Stale to satisfy
            // the type system without inventing a new error variant.
            return Err(Error::Store(sunset_store::Error::Stale));
        }

        // Fire a digest exchange to every connected peer over the
        // newly-published filter. The peer responds with any matching
        // entries it has that aren't in our bloom — closing the
        // late-subscriber gap where an entry already at the peer
        // (e.g., a relay holding A's WebRTC offer for us) wouldn't
        // otherwise reach us until anti-entropy fires for
        // SUBSCRIBE_NAME, which doesn't carry chat/webrtc data.
        // Snapshotting peer ids first lets us drop the state lock
        // before the per-peer bloom build + send work.
        let peers: Vec<PeerId> = self
            .state
            .lock()
            .await
            .peer_outbound
            .keys()
            .cloned()
            .collect();
        for peer_id in &peers {
            self.send_filter_digest(peer_id, &filter).await;
        }
        Ok(())
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

    /// Register a fake connected peer on the engine and return the
    /// receiver end of its outbound channel.
    async fn add_test_peer(
        engine: &SyncEngine<MemoryStore, TestTransport>,
        label: &[u8],
        conn_id: ConnectionId,
    ) -> (PeerId, mpsc::UnboundedReceiver<SyncMessage>) {
        let (tx, rx) = mpsc::unbounded_channel::<SyncMessage>();
        let peer = PeerId(vk(label));
        engine
            .state
            .lock()
            .await
            .peer_outbound
            .insert(peer.clone(), PeerOutbound { conn_id, tx });
        (peer, rx)
    }

    /// Drain `rx` (non-blocking), collecting `DigestExchange.filter`
    /// values. Panics on any other queued message kind.
    fn drain_digest_filters(rx: &mut mpsc::UnboundedReceiver<SyncMessage>) -> Vec<Filter> {
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(SyncMessage::DigestExchange { filter, .. }) => out.push(filter),
                Ok(other) => panic!("expected DigestExchange, got {other:?}"),
                Err(_) => break,
            }
        }
        out
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
                engine.state.lock().await.peer_outbound.insert(
                    PeerId(vk(b"requester")),
                    PeerOutbound {
                        conn_id: ConnectionId::for_test(99),
                        tx,
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
                engine.state.lock().await.peer_outbound.insert(
                    PeerId(vk(b"remote")),
                    PeerOutbound {
                        conn_id: ConnectionId::for_test(99),
                        tx,
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
                engine.state.lock().await.peer_outbound.insert(
                    peer.clone(),
                    PeerOutbound {
                        conn_id: conn1,
                        tx: tx1,
                    },
                );

                // Replace with generation 2 (simulating a fresh PeerHello).
                let (tx2, mut rx2) = mpsc::unbounded_channel::<SyncMessage>();
                let conn2 = ConnectionId::for_test(2);
                engine.state.lock().await.peer_outbound.insert(
                    peer.clone(),
                    PeerOutbound {
                        conn_id: conn2,
                        tx: tx2,
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
                let po = state.peer_outbound.get(&peer).expect("gen2 still present");
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
                engine
                    .state
                    .lock()
                    .await
                    .peer_outbound
                    .insert(peer.clone(), PeerOutbound { conn_id: conn, tx });

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

                assert!(!engine.state.lock().await.peer_outbound.contains_key(&peer));
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
                engine
                    .state
                    .lock()
                    .await
                    .peer_outbound
                    .insert(peer.clone(), PeerOutbound { conn_id: conn, tx });

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
                engine
                    .state
                    .lock()
                    .await
                    .peer_outbound
                    .insert(peer.clone(), PeerOutbound { conn_id: conn, tx });
                engine
                    .state
                    .lock()
                    .await
                    .peer_kinds
                    .insert(peer.clone(), crate::transport::TransportKind::Unknown);

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

    /// Regression test for the Stale-on-restart bug. The relay's
    /// `Relay::run()` calls `publish_subscription` on startup; if the
    /// data_dir already contains a subscribe entry written by a prior
    /// process run with priority equal to (or greater than) the current
    /// `now_ms`, the LWW store rejects the insert as Stale. Pre-fix,
    /// the engine surfaced that as an error → `Relay::run` returned
    /// Err → process exited → clients saw ECONNREFUSED on every
    /// reconnect attempt, breaking restart-based redial.
    ///
    /// `do_publish_subscription` must absorb the Stale and retry with
    /// `existing.priority + 1`, so the call is always Ok for our own
    /// subscribe entry.
    #[tokio::test(flavor = "current_thread")]
    async fn publish_subscription_bumps_priority_past_existing_stale_entry() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let store = Arc::new(MemoryStore::with_accept_all());
                let engine = make_engine_with_store("alice", b"alice", store.clone());

                // Pre-populate a subscribe entry under our own
                // verifying_key with a priority well in the future,
                // simulating a prior-process-run entry that the new
                // call's `now_ms` cannot beat.
                let block = ContentBlock {
                    data: Bytes::from_static(b"prior-filter"),
                    references: vec![],
                };
                let future_priority: u64 = 9_999_999_999_999; // ~year 2286 in ms
                let prior = SignedKvEntry {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
                    value_hash: block.hash(),
                    priority: future_priority,
                    expires_at: Some(future_priority.saturating_add(1)),
                    signature: Bytes::from_static(&[0u8; 64]),
                };
                store
                    .insert(prior.clone(), Some(block))
                    .await
                    .expect("prior insert");

                // The freshly-issued publish_subscription would race a
                // `now_ms` that is far below `future_priority` and
                // therefore hit Stale on the first attempt. The retry
                // bump must raise priority above the prior one and
                // succeed.
                let new_filter = Filter::NamePrefix(Bytes::from_static(b"chat/"));
                engine
                    .do_publish_subscription(new_filter, std::time::Duration::from_secs(60))
                    .await
                    .expect("publish_subscription must succeed even when an existing entry has a higher priority");

                let stored = store
                    .get_entry(&vk(b"alice"), reserved::SUBSCRIBE_NAME)
                    .await
                    .expect("get_entry")
                    .expect("entry stored");
                assert!(
                    stored.priority > prior.priority,
                    "new priority {} must strictly beat prior priority {}",
                    stored.priority,
                    prior.priority
                );
                assert_ne!(
                    stored.value_hash, prior.value_hash,
                    "new entry must replace the prior one (different filter → different value_hash)"
                );
            })
            .await;
    }

    /// Regression for the late-subscriber routing race: an entry that
    /// already lives at our peer when we publish a new subscription
    /// wouldn't otherwise reach us. The per-event push at the peer
    /// fired against an empty registry (we hadn't subscribed yet),
    /// and the periodic anti-entropy / bootstrap-digest exchange
    /// only carries SUBSCRIBE_NAME entries — chat / webrtc data
    /// stays stranded. Pre-fix this manifested as `connect_direct`
    /// hangs in CI: A's WebRTC offer reaches the relay, the relay
    /// stores it, and B (who joined a moment later) never sees it.
    ///
    /// Fix: `do_publish_subscription` follows the local store insert
    /// with a `DigestExchange` over the new filter, sent to every
    /// connected peer. The peer responds with whatever matching
    /// entries we don't already have, reusing the existing
    /// `handle_digest_exchange` machinery.
    ///
    /// This test stands in for the publishing client (B): it sets
    /// up B with one connected peer (e.g. the relay), calls
    /// `do_publish_subscription`, and asserts that a digest
    /// addressed to that peer hits the outbound channel with the
    /// just-published filter.
    #[tokio::test(flavor = "current_thread")]
    async fn publish_subscription_sends_filter_digest_to_connected_peers() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("bob", b"bob"));

                // Stand up "relay" as a connected peer with an
                // outbound channel we can drain, mirroring what
                // `handle_inbound_event(PeerHello)` does on a real
                // connection.
                let relay = PeerId(vk(b"relay"));
                let (tx, mut rx) = mpsc::unbounded_channel::<SyncMessage>();
                {
                    let mut state = engine.state.lock().await;
                    state.peer_outbound.insert(
                        relay.clone(),
                        PeerOutbound {
                            conn_id: ConnectionId::for_test(0),
                            tx,
                        },
                    );
                }

                // Publish a subscription whose filter covers a
                // chat-like namespace. After Ok we expect a
                // DigestExchange with this exact filter on the
                // relay's outbound channel.
                let filter = Filter::NamePrefix(Bytes::from_static(b"room/"));
                engine
                    .do_publish_subscription(filter.clone(), std::time::Duration::from_secs(60))
                    .await
                    .expect("publish_subscription must succeed");

                // Drain everything the engine sent to the relay and
                // assert: at least one DigestExchange whose filter
                // matches the freshly-published one.
                let mut saw_filter_digest = false;
                while let Ok(msg) = rx.try_recv() {
                    if let SyncMessage::DigestExchange { filter: f, .. } = msg {
                        if f == filter {
                            saw_filter_digest = true;
                        }
                    }
                }
                assert!(
                    saw_filter_digest,
                    "publish_subscription must send a DigestExchange over the new filter to each connected peer so the peer can backfill matching entries we're missing"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn own_published_filters_returns_self_authored_subscribe_entries_only() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                // Self-authored subscribe entry — should be returned.
                let mine = Filter::NamePrefix(Bytes::from_static(b"room/"));
                let mine_bytes = postcard::to_stdvec(&mine).unwrap();
                let mine_block = ContentBlock {
                    data: Bytes::from(mine_bytes),
                    references: vec![],
                };
                let mine_entry = SignedKvEntry {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
                    value_hash: mine_block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine
                    .store
                    .insert(mine_entry, Some(mine_block))
                    .await
                    .unwrap();

                // Someone else's subscribe entry — must NOT be returned.
                let theirs = Filter::NamePrefix(Bytes::from_static(b"other/"));
                let theirs_bytes = postcard::to_stdvec(&theirs).unwrap();
                let theirs_block = ContentBlock {
                    data: Bytes::from(theirs_bytes),
                    references: vec![],
                };
                let theirs_entry = SignedKvEntry {
                    verifying_key: vk(b"bob"),
                    name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
                    value_hash: theirs_block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine
                    .store
                    .insert(theirs_entry, Some(theirs_block))
                    .await
                    .unwrap();

                // Self-authored entry under a non-subscribe name — must NOT be returned.
                let chat_block = ContentBlock {
                    data: Bytes::from_static(b"hi"),
                    references: vec![],
                };
                let chat_entry = SignedKvEntry {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(b"room/msg/1"),
                    value_hash: chat_block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine
                    .store
                    .insert(chat_entry, Some(chat_block))
                    .await
                    .unwrap();

                let filters = engine.own_published_filters().await;
                assert_eq!(filters, vec![mine]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn peer_hello_fires_filter_digest_for_own_published_subscriptions() {
        use crate::peer::InboundEvent;
        use crate::transport::TransportKind;
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                let chat_filter = Filter::NamePrefix(Bytes::from_static(b"room/"));
                let filter_bytes = postcard::to_stdvec(&chat_filter).unwrap();
                let block = ContentBlock {
                    data: Bytes::from(filter_bytes),
                    references: vec![],
                };
                let entry = SignedKvEntry {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine.store.insert(entry, Some(block)).await.unwrap();

                let (tx, mut rx) = mpsc::unbounded_channel::<SyncMessage>();
                engine
                    .handle_inbound_event(InboundEvent::PeerHello {
                        peer_id: PeerId(vk(b"relay")),
                        conn_id: ConnectionId::for_test(1),
                        kind: TransportKind::Primary,
                        out_tx: tx,
                        registered: None,
                    })
                    .await;

                let filters = drain_digest_filters(&mut rx);
                let bootstrap = Filter::Namespace(Bytes::from_static(reserved::SUBSCRIBE_NAME));
                assert!(
                    filters.contains(&bootstrap),
                    "bootstrap digest must still fire (got {filters:?})"
                );
                assert!(
                    filters.contains(&chat_filter),
                    "per-filter digest must fire for own published subscription (got {filters:?})"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn anti_entropy_tick_fires_filter_digest_for_own_published_subscriptions() {
        use sunset_store::{ContentBlock, SignedKvEntry, Store as _};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Rc::new(make_engine("alice", b"alice"));

                let chat_filter = Filter::NamePrefix(Bytes::from_static(b"room/"));
                let filter_bytes = postcard::to_stdvec(&chat_filter).unwrap();
                let block = ContentBlock {
                    data: Bytes::from(filter_bytes),
                    references: vec![],
                };
                let entry = SignedKvEntry {
                    verifying_key: vk(b"alice"),
                    name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                engine.store.insert(entry, Some(block)).await.unwrap();

                let (_peer, mut rx) =
                    add_test_peer(&engine, b"relay", ConnectionId::for_test(1)).await;

                engine.tick_anti_entropy().await;

                let filters = drain_digest_filters(&mut rx);
                let bootstrap = Filter::Namespace(Bytes::from_static(reserved::SUBSCRIBE_NAME));
                assert!(
                    filters.contains(&bootstrap),
                    "bootstrap digest must fire (got {filters:?})"
                );
                assert!(
                    filters.contains(&chat_filter),
                    "per-filter digest must fire on anti-entropy tick (got {filters:?})"
                );
            })
            .await;
    }
}
