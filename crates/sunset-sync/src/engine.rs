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

/// Free helper that spins up the outbound channel + spawns the per-peer
/// task. Extracted from `SyncEngine::spawn_peer` so the AddPeer command
/// handler can call it from a `'static` spawned task without holding
/// `&self`.
fn spawn_run_peer<C: crate::transport::TransportConnection + 'static>(
    conn: C,
    local_peer: PeerId,
    proto: u32,
    inbound_tx: mpsc::UnboundedSender<InboundEvent>,
) {
    let conn = Rc::new(conn);
    let (out_tx, out_rx) = mpsc::unbounded_channel::<SyncMessage>();
    crate::spawn::spawn_local(run_peer(
        conn, local_peer, proto, out_tx, out_rx, inbound_tx,
    ));
}

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
    pub(crate) signer: Arc<dyn Signer>,
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
                _ = anti_entropy.tick() => {
                    self.tick_anti_entropy().await;
                }
            }
        }
    }

    async fn tick_anti_entropy(&self) {
        let bloom = match build_digest(
            &*self.store,
            &self.config.bootstrap_filter,
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
            filter: self.config.bootstrap_filter.clone(),
            range: DigestRange::All,
            bloom: bloom.to_bytes(),
        };
        let state = self.state.lock().await;
        for tx in state.peer_outbound.values() {
            let _ = tx.send(msg.clone());
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
                let local_peer = self.local_peer.clone();
                let proto = self.config.protocol_version;
                let inbound_tx = inbound_tx.clone();
                crate::spawn::spawn_local(async move {
                    let r = match transport.connect(addr).await {
                        Ok(conn) => {
                            spawn_run_peer(conn, local_peer, proto, inbound_tx);
                            Ok(())
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
        }
    }

    async fn spawn_peer(
        &self,
        conn: T::Connection,
        inbound_tx: mpsc::UnboundedSender<InboundEvent>,
    ) {
        spawn_run_peer(
            conn,
            self.local_peer.clone(),
            self.config.protocol_version,
            inbound_tx,
        );
    }

    async fn handle_inbound_event(&self, event: InboundEvent) {
        match event {
            InboundEvent::PeerHello {
                peer_id,
                kind: _,
                out_tx,
            } => {
                // Register the outbound sender under the Hello-declared peer_id.
                // This key matches what the subscription registry uses, so push
                // routing can find the sender correctly.
                self.state
                    .lock()
                    .await
                    .peer_outbound
                    .insert(peer_id.clone(), out_tx);
                // Fire bootstrap digest exchange on the subscribe namespace.
                self.send_bootstrap_digest(&peer_id).await;
            }
            InboundEvent::Message { from, message } => {
                self.handle_peer_message(from, message).await;
            }
            InboundEvent::Disconnected { peer_id, reason } => {
                eprintln!("sunset-sync: peer {peer_id:?} disconnected: {reason}");
                self.state.lock().await.peer_outbound.remove(&peer_id);
            }
        }
    }

    async fn send_bootstrap_digest(&self, to: &PeerId) {
        let bloom = match build_digest(
            &*self.store,
            &self.config.bootstrap_filter,
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
            filter: self.config.bootstrap_filter.clone(),
            range: DigestRange::All,
            bloom: bloom.to_bytes(),
        };
        let state = self.state.lock().await;
        if let Some(tx) = state.peer_outbound.get(to) {
            let _ = tx.send(msg);
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
            SyncMessage::Hello { .. } | SyncMessage::Goodbye { .. } => {
                // Handled by the per-peer task; engine ignores.
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
                eprintln!("sunset-sync: digest scan failed: {e}");
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
        if let Some(tx) = state.peer_outbound.get(&from) {
            let _ = tx.send(msg);
        }
    }

    async fn handle_blob_request(&self, from: PeerId, hash: sunset_store::Hash) {
        let block = match self.store.get_content(&hash).await {
            Ok(Some(b)) => b,
            // We don't have it (or I/O failed); drop silently in v1.
            _ => return,
        };
        let state = self.state.lock().await;
        if let Some(tx) = state.peer_outbound.get(&from) {
            let _ = tx.send(SyncMessage::BlobResponse { block });
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
                    eprintln!(
                        "sunset-sync: insert failed for entry from {:?}: {}",
                        entry.verifying_key, e
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
                    if let Some(tx) = state.peer_outbound.get(&from) {
                        let _ = tx.send(SyncMessage::BlobRequest {
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
        if entry.name.as_ref() == reserved::SUBSCRIBE_NAME {
            if let Ok(Some(block)) = self.store.get_content(&entry.value_hash).await {
                if let Ok(filter) = parse_subscription_entry(&entry, &block) {
                    self.state
                        .lock()
                        .await
                        .registry
                        .insert(entry.verifying_key.clone(), filter);
                }
            }
        }

        // Push flow: route to peers whose filter matches.
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
        for peer in state
            .registry
            .peers_matching(&entry.verifying_key, &entry.name)
        {
            if let Some(tx) = state.peer_outbound.get(&peer) {
                let _ = tx.send(msg.clone());
            }
        }
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

    /// Real implementation of `publish_subscription`'s server side.
    async fn do_publish_subscription(
        &self,
        filter: Filter,
        ttl: std::time::Duration,
    ) -> Result<()> {
        use sunset_store::canonical::signing_payload;
        use sunset_store::{ContentBlock, SignedKvEntry};

        let value = postcard::to_stdvec(&filter)
            .map_err(|e| Error::Decode(format!("encode filter: {e}")))?;
        let block = ContentBlock {
            data: Bytes::from(value),
            references: vec![],
        };
        let now_secs = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut entry = SignedKvEntry {
            verifying_key: self.signer.verifying_key(),
            name: Bytes::from_static(reserved::SUBSCRIBE_NAME),
            value_hash: block.hash(),
            priority: now_secs,
            expires_at: Some(now_secs.saturating_add(ttl.as_secs())),
            signature: Bytes::new(),
        };
        let payload = signing_payload(&entry);
        entry.signature = self.signer.sign(&payload);
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
        let net = TestNetwork::new();
        let local_peer = PeerId(vk(peer_label));
        let transport = net.transport(
            local_peer.clone(),
            PeerAddr::new(Bytes::copy_from_slice(addr.as_bytes())),
        );
        let store = Arc::new(MemoryStore::with_accept_all());
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
                engine
                    .state
                    .lock()
                    .await
                    .peer_outbound
                    .insert(PeerId(vk(b"requester")), tx);

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
                engine
                    .state
                    .lock()
                    .await
                    .peer_outbound
                    .insert(PeerId(vk(b"remote")), tx);

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
}
