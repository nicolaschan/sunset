//! `SyncEngine` — the top-level coordinator.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use sunset_store::{Event, Filter, Replay, Store};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::error::{Error, Result};
use crate::message::SyncMessage;
use crate::peer::{InboundEvent, run_peer};
use crate::reserved;
use crate::subscription_registry::SubscriptionRegistry;
use crate::transport::{Transport, TransportConnection};
use crate::types::{PeerAddr, PeerId, SyncConfig, TrustSet};

/// A command sent from the public API into the running engine.
pub(crate) enum EngineCommand {
    AddPeer {
        addr: PeerAddr,
        ack: oneshot::Sender<Result<()>>,
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
}

/// Mutable state inside the engine. Held under a `tokio::sync::Mutex` so
/// command processing and per-peer task callbacks can both update it.
pub(crate) struct EngineState {
    pub trust: TrustSet,
    pub registry: SubscriptionRegistry,
    /// Per-peer outbound message senders.
    pub peer_outbound: HashMap<PeerId, mpsc::UnboundedSender<SyncMessage>>,
}

pub struct SyncEngine<S: Store, T: Transport> {
    pub(crate) store: Arc<S>,
    pub(crate) transport: Arc<T>,
    pub(crate) config: SyncConfig,
    /// Local peer's identity. Required for signing `_sunset-sync/subscribe`
    /// entries.
    pub(crate) local_peer: PeerId,
    pub(crate) state: Arc<Mutex<EngineState>>,
    pub(crate) cmd_tx: mpsc::UnboundedSender<EngineCommand>,
    /// Held inside `run()`. `new()` creates the (tx, rx) pair; `run()`
    /// takes the rx out via Mutex<Option<...>>.
    pub(crate) cmd_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<EngineCommand>>>>,
}

impl<S: Store + 'static, T: Transport + 'static> SyncEngine<S, T>
where
    T::Connection: 'static,
{
    pub fn new(store: Arc<S>, transport: T, config: SyncConfig, local_peer: PeerId) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        Self {
            store,
            transport: Arc::new(transport),
            config,
            local_peer,
            state: Arc::new(Mutex::new(EngineState {
                trust: TrustSet::default(),
                registry: SubscriptionRegistry::new(),
                peer_outbound: HashMap::new(),
            })),
            cmd_tx,
            cmd_rx: Arc::new(Mutex::new(Some(cmd_rx))),
        }
    }

    /// Initiate an outbound connection to `addr`. Returns when the connection
    /// is established + Hello-exchanged, or fails.
    pub async fn add_peer(&self, addr: PeerAddr) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::AddPeer { addr, ack })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Publish this peer's subscription filter. Writes a signed KV entry
    /// under `(local_peer, "_sunset-sync/subscribe")` with `value_hash =
    /// blake3(postcard(filter))` and priority = unix-timestamp-now,
    /// expires_at = priority + ttl.
    ///
    /// **Note:** v1 uses a stub signature (empty bytes) — the
    /// `sunset_store::AcceptAllVerifier` accepts everything. When a real
    /// signing scheme lands (sunset-core / identity subsystem), this
    /// function will sign the entry with the local key.
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

        // Local store subscription. Initially a `Filter::Namespace(_sunset-sync/subscribe)`;
        // refreshed whenever the registry changes (Task 14 expands the union).
        let mut local_sub = self
            .store
            .subscribe(
                Filter::Namespace(Bytes::from_static(reserved::SUBSCRIBE_NAME)),
                Replay::None,
            )
            .await?;

        loop {
            tokio::select! {
                maybe_conn = self.transport.accept() => {
                    match maybe_conn {
                        Ok(conn) => self.spawn_peer(conn, inbound_tx.clone()).await,
                        Err(e) => return Err(e),
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
            }
        }
    }

    pub(crate) async fn handle_command(
        &self,
        cmd: EngineCommand,
        inbound_tx: &mpsc::UnboundedSender<InboundEvent>,
    ) {
        match cmd {
            EngineCommand::AddPeer { addr, ack } => {
                let r = self.do_add_peer(addr, inbound_tx.clone()).await;
                let _ = ack.send(r);
            }
            EngineCommand::PublishSubscription { filter, ttl, ack } => {
                let r = self.do_publish_subscription(filter, ttl).await;
                let _ = ack.send(r);
            }
            EngineCommand::SetTrust { trust, ack } => {
                self.state.lock().await.trust = trust;
                let _ = ack.send(Ok(()));
            }
        }
    }

    async fn do_add_peer(
        &self,
        addr: PeerAddr,
        inbound_tx: mpsc::UnboundedSender<InboundEvent>,
    ) -> Result<()> {
        let conn = self.transport.connect(addr).await?;
        self.spawn_peer(conn, inbound_tx).await;
        Ok(())
    }

    async fn spawn_peer(
        &self,
        conn: T::Connection,
        inbound_tx: mpsc::UnboundedSender<InboundEvent>,
    ) {
        let conn = Rc::new(conn);
        let (out_tx, out_rx) = mpsc::unbounded_channel::<SyncMessage>();
        let local_peer = self.local_peer.clone();
        let proto = self.config.protocol_version;

        // Register the outbound sender under the connection's peer_id. For
        // the TestTransport this is the peer's actual identity (the network
        // tracks addr -> peer_id). For production transports that don't
        // surface peer_id until after a handshake, the connection should
        // either delay TransportConnection::peer_id() until it's authoritative
        // or accept a re-key on PeerHello — handle that when those transports
        // are added.
        let peer_id = conn.peer_id();
        self.state
            .lock()
            .await
            .peer_outbound
            .insert(peer_id, out_tx);

        tokio::task::spawn_local(run_peer(
            conn,
            local_peer,
            proto,
            out_rx,
            inbound_tx,
        ));
    }

    async fn handle_inbound_event(&self, event: InboundEvent) {
        match event {
            InboundEvent::PeerHello { .. } => {
                // The outbound channel was already registered under the
                // connection's peer_id in spawn_peer. PeerHello is just a
                // signal that the handshake completed; bootstrap fires from
                // here in Task 14.
            }
            InboundEvent::Message { from, message } => {
                self.handle_peer_message(from, message).await;
            }
            InboundEvent::Disconnected { peer_id, .. } => {
                self.state.lock().await.peer_outbound.remove(&peer_id);
            }
        }
    }

    async fn handle_peer_message(&self, from: PeerId, message: SyncMessage) {
        // Tasks 12–15 fill this in. For now, all messages other than Hello
        // (which never reaches here) are ignored.
        let _ = (from, message);
    }

    async fn handle_local_store_event(&self, ev: Event) {
        // Push flow: route to peers whose filter matches.
        let entry = match ev {
            Event::Inserted(e) => e,
            Event::Replaced { new, .. } => new,
            // Expired / BlobAdded / BlobRemoved: not pushed in v1.
            _ => return,
        };
        // Look up the corresponding blob (best-effort).
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
        // Find matching peers and forward.
        let state = self.state.lock().await;
        for peer in state
            .registry
            .peers_matching(&entry.verifying_key, &entry.name)
        {
            if let Some(tx) = state.peer_outbound.get(&peer) {
                let _ = tx.send(msg.clone());
            }
        }
    }

    /// Real implementation of `publish_subscription`'s server side.
    async fn do_publish_subscription(
        &self,
        filter: Filter,
        ttl: std::time::Duration,
    ) -> Result<()> {
        use sunset_store::{ContentBlock, SignedKvEntry};

        let value = postcard::to_stdvec(&filter)
            .map_err(|e| Error::Decode(format!("encode filter: {e}")))?;
        let block = ContentBlock {
            data: Bytes::from(value),
            references: vec![],
        };
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let entry = SignedKvEntry {
            verifying_key: self.local_peer.0.clone(),
            name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
            value_hash: block.hash(),
            priority: now_secs,
            expires_at: Some(now_secs.saturating_add(ttl.as_secs())),
            // v1 stub signature; real signing lands in identity subsystem.
            signature: Bytes::new(),
        };
        self.store.insert(entry, Some(block)).await?;
        Ok(())
    }
}

#[cfg(all(test, feature = "test-helpers"))]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;
    use sunset_store_memory::MemoryStore;

    use crate::test_transport::{TestNetwork, TestTransport};

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    fn make_engine(addr: &str, peer_label: &[u8]) -> SyncEngine<MemoryStore, TestTransport> {
        let net = TestNetwork::new();
        let local_peer = PeerId(vk(peer_label));
        let transport = net.transport(
            local_peer.clone(),
            PeerAddr::new(Bytes::copy_from_slice(addr.as_bytes())),
        );
        let store = Arc::new(MemoryStore::with_accept_all());
        SyncEngine::new(store, transport, SyncConfig::default(), local_peer)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_drains_set_trust_command() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let engine = Arc::new(make_engine("alice", b"alice"));
                let h = tokio::task::spawn_local({
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
}
