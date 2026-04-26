//! `SyncEngine` — the top-level coordinator.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use sunset_store::{Filter, Store};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::error::{Error, Result};
use crate::reserved;
use crate::subscription_registry::SubscriptionRegistry;
use crate::transport::Transport;
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
    pub peer_outbound: HashMap<PeerId, mpsc::UnboundedSender<crate::message::SyncMessage>>,
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

impl<S: Store + 'static, T: Transport + 'static> SyncEngine<S, T> {
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

        // Tasks 11–15 fill in the loop body. For now, drain commands until
        // the channel closes.
        while let Some(cmd) = cmd_rx.recv().await {
            self.handle_command(cmd).await;
        }
        Ok(())
    }

    /// Stub command handler — replaced fully in Tasks 11–15.
    pub(crate) async fn handle_command(&self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::AddPeer { ack, .. } => {
                let _ = ack.send(Err(Error::Protocol(
                    "add_peer not implemented (Task 13)".into(),
                )));
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
    async fn set_trust_updates_state() {
        let engine = make_engine("alice", b"alice");
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let h = tokio::task::spawn_local({
                    let cmd_rx = engine.cmd_rx.clone();
                    let state = engine.state.clone();
                    async move {
                        let mut rx = cmd_rx.lock().await.take().unwrap();
                        while let Some(cmd) = rx.recv().await {
                            if let EngineCommand::SetTrust { trust, ack } = cmd {
                                state.lock().await.trust = trust;
                                let _ = ack.send(Ok(()));
                            }
                        }
                    }
                });
                let mut whitelist = std::collections::HashSet::new();
                whitelist.insert(vk(b"trusted"));
                engine
                    .set_trust(TrustSet::Whitelist(whitelist.clone()))
                    .await
                    .unwrap();
                let s = engine.state.lock().await;
                assert_eq!(s.trust, TrustSet::Whitelist(whitelist));
                drop(s);
                h.abort();
                let _ = h.await;
            })
            .await;
    }
}
