//! `Signaler` impl that sits on top of an existing `Store` +
//! `SyncEngine`. Each outbound `SignalMessage` becomes a `SignedKvEntry`
//! named `<room_fp_hex>/webrtc/<from_hex>/<to_hex>/<seq:016x>` whose
//! content block carries the Noise_KK ciphertext for the payload.
//!
//! Moved from `sunset-web-wasm::relay_signaler` so non-web hosts can
//! signal Noise_KK setup via the same CRDT-entry path.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use crate::Identity;
use sunset_noise::{KkInitiator, KkResponder, KkSession, ed25519_seed_to_x25519_secret};
use sunset_store::{
    ContentBlock, Filter, Replay, SignedKvEntry, Store, VerifyingKey, canonical::signing_payload,
};
use sunset_sync::{Error as SyncError, PeerId, Result as SyncResult, SignalMessage, Signaler};

pub fn signaling_filter(room_fp_hex: &str) -> Filter {
    Filter::NamePrefix(Bytes::from(format!("{room_fp_hex}/webrtc/")))
}

fn entry_name(room_fp_hex: &str, from: &PeerId, to: &PeerId, seq: u64) -> Bytes {
    let from_hex = hex::encode(from.verifying_key().as_bytes());
    let to_hex = hex::encode(to.verifying_key().as_bytes());
    Bytes::from(format!(
        "{room_fp_hex}/webrtc/{from_hex}/{to_hex}/{seq:016x}"
    ))
}

fn parse_entry_name(name: &[u8], room_fp_hex: &str) -> Option<(PeerId, PeerId, u64)> {
    let s = std::str::from_utf8(name).ok()?;
    let suffix = s.strip_prefix(&format!("{room_fp_hex}/webrtc/"))?;
    let mut parts = suffix.splitn(3, '/');
    let from_hex = parts.next()?;
    let to_hex = parts.next()?;
    let seq_hex = parts.next()?;
    let from_bytes = hex::decode(from_hex).ok()?;
    let to_bytes = hex::decode(to_hex).ok()?;
    let seq = u64::from_str_radix(seq_hex, 16).ok()?;
    Some((
        PeerId(VerifyingKey::new(Bytes::from(from_bytes))),
        PeerId(VerifyingKey::new(Bytes::from(to_bytes))),
        seq,
    ))
}

#[derive(Default)]
struct PeerKkSlot {
    initiator: Option<KkInitiator>,
    responder: Option<KkResponder>,
    session: Option<KkSession>,
    next_send_seq: u64,
    on_session_ready: Vec<oneshot::Sender<()>>,
}

struct Inner {
    peers: HashMap<PeerId, PeerKkSlot>,
}

pub struct RelaySignaler<S: Store + 'static> {
    local_identity: Identity,
    local_x25519_secret: Zeroizing<[u8; 32]>,
    x25519_pub_cache: Mutex<HashMap<PeerId, [u8; 32]>>,
    pub(crate) room_fp_hex: String,
    store: Arc<S>,
    inner: Mutex<Inner>,
    inbound_rx: Mutex<mpsc::UnboundedReceiver<SignalMessage>>,
}

impl<S: Store + 'static> RelaySignaler<S> {
    pub fn new(local_identity: Identity, room_fp_hex: String, store: &Arc<S>) -> Rc<Self> {
        let local_x25519_secret = ed25519_seed_to_x25519_secret(&local_identity.secret_bytes());
        let (inbound_tx, inbound_rx) = mpsc::unbounded::<SignalMessage>();
        let signaler = Rc::new(Self {
            local_identity,
            local_x25519_secret,
            x25519_pub_cache: Mutex::new(HashMap::new()),
            room_fp_hex,
            store: store.clone(),
            inner: Mutex::new(Inner {
                peers: HashMap::new(),
            }),
            inbound_rx: Mutex::new(inbound_rx),
        });
        let me = signaler.clone();
        sunset_sync::spawn::spawn_local(async move {
            me.run_dispatcher(inbound_tx).await;
        });
        signaler
    }

    fn local_peer(&self) -> PeerId {
        PeerId(self.local_identity.store_verifying_key())
    }

    async fn x25519_pub_for(&self, peer: &PeerId) -> SyncResult<[u8; 32]> {
        if let Some(p) = self.x25519_pub_cache.lock().await.get(peer) {
            return Ok(*p);
        }
        let bytes: &[u8] = peer.verifying_key().as_bytes();
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            SyncError::Transport(format!("peer pubkey wrong length: {}", bytes.len()))
        })?;
        let x = sunset_noise::ed25519_public_to_x25519(&arr)
            .map_err(|e| SyncError::Transport(format!("x25519 derive: {e}")))?;
        self.x25519_pub_cache.lock().await.insert(peer.clone(), x);
        Ok(x)
    }

    async fn run_dispatcher(&self, inbound_tx: mpsc::UnboundedSender<SignalMessage>) {
        let filter = signaling_filter(&self.room_fp_hex);
        let mut events = match self.store.subscribe(filter, Replay::All).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("RelaySignaler subscribe: {e}");
                return;
            }
        };
        while let Some(ev) = events.next().await {
            let entry = match ev {
                Ok(sunset_store::Event::Inserted(e)) => e,
                Ok(sunset_store::Event::Replaced { new, .. }) => new,
                Ok(_) => continue,
                Err(e) => {
                    tracing::error!("RelaySignaler event: {e}");
                    continue;
                }
            };
            if let Err(e) = self.handle_entry(&entry, &inbound_tx).await {
                tracing::warn!("RelaySignaler handle_entry: {e}");
            }
        }
    }

    async fn handle_entry(
        &self,
        entry: &SignedKvEntry,
        inbound_tx: &mpsc::UnboundedSender<SignalMessage>,
    ) -> SyncResult<()> {
        let (from, to, seq) = parse_entry_name(&entry.name, &self.room_fp_hex)
            .ok_or_else(|| SyncError::Transport("bad signaling entry name".into()))?;
        if to != self.local_peer() {
            return Ok(());
        }
        if from == self.local_peer() {
            return Ok(());
        }

        let block = self
            .store
            .get_content(&entry.value_hash)
            .await?
            .ok_or_else(|| SyncError::Transport("missing content block".into()))?;
        let ciphertext: &[u8] = &block.data;

        let plaintext = self.decrypt_inbound(&from, ciphertext).await?;

        let _ = inbound_tx.unbounded_send(SignalMessage {
            from,
            to,
            seq,
            payload: Bytes::from(plaintext),
        });
        Ok(())
    }

    async fn decrypt_inbound(&self, from: &PeerId, ciphertext: &[u8]) -> SyncResult<Vec<u8>> {
        let mut inner = self.inner.lock().await;
        let slot = inner.peers.entry(from.clone()).or_default();
        if slot.session.is_none() && slot.initiator.is_none() && slot.responder.is_none() {
            let remote_x = self.x25519_pub_for(from).await?;
            let mut resp = KkResponder::new(&self.local_x25519_secret, &remote_x)
                .map_err(|e| SyncError::Transport(format!("KkResponder::new: {e}")))?;
            let pt = resp
                .read_message_1(ciphertext)
                .map_err(|e| SyncError::Transport(format!("read_message_1: {e}")))?;
            slot.responder = Some(resp);
            return Ok(pt);
        }
        if let Some(init) = slot.initiator.take() {
            let (pt, session) = init
                .read_message_2(ciphertext)
                .map_err(|e| SyncError::Transport(format!("read_message_2: {e}")))?;
            slot.session = Some(session);
            for waiter in slot.on_session_ready.drain(..) {
                let _ = waiter.send(());
            }
            return Ok(pt);
        }
        if let Some(session) = slot.session.as_mut() {
            return session
                .decrypt(ciphertext)
                .map_err(|e| SyncError::Transport(format!("session.decrypt: {e}")));
        }
        Err(SyncError::Transport(
            "inbound before responder sent msg2; dropped".into(),
        ))
    }

    async fn next_send_seq(&self, to: &PeerId) -> u64 {
        let mut inner = self.inner.lock().await;
        let slot = inner.peers.entry(to.clone()).or_default();
        let s = slot.next_send_seq;
        slot.next_send_seq = s + 1;
        s
    }

    async fn write_entry(&self, to: &PeerId, seq: u64, ciphertext: Vec<u8>) -> SyncResult<()> {
        let from = self.local_peer();
        let block = ContentBlock {
            data: Bytes::from(ciphertext),
            references: vec![],
        };
        let value_hash = block.hash();
        let priority = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut entry = SignedKvEntry {
            verifying_key: self.local_identity.store_verifying_key(),
            name: entry_name(&self.room_fp_hex, &from, to, seq),
            value_hash,
            priority,
            expires_at: Some(priority + 3_600_000),
            signature: Bytes::new(),
        };
        let payload = signing_payload(&entry);
        let sig = self.local_identity.sign(&payload);
        entry.signature = Bytes::copy_from_slice(&sig.to_bytes());

        self.store
            .insert(entry, Some(block))
            .await
            .map_err(SyncError::Store)?;
        Ok(())
    }
}

#[async_trait(?Send)]
impl<S: Store + 'static> Signaler for RelaySignaler<S> {
    async fn send(&self, message: SignalMessage) -> SyncResult<()> {
        let to = message.to.clone();
        let plaintext = message.payload;

        loop {
            let ciphertext_opt = {
                let mut inner = self.inner.lock().await;
                let slot = inner.peers.entry(to.clone()).or_default();
                if slot.initiator.is_none() && slot.responder.is_none() && slot.session.is_none() {
                    let remote_x = self.x25519_pub_for(&to).await?;
                    let mut init = KkInitiator::new(&self.local_x25519_secret, &remote_x)
                        .map_err(|e| SyncError::Transport(format!("KkInitiator::new: {e}")))?;
                    let ct = init
                        .write_message_1(&plaintext)
                        .map_err(|e| SyncError::Transport(format!("write_message_1: {e}")))?;
                    slot.initiator = Some(init);
                    Some(ct)
                } else if let Some(resp) = slot.responder.take() {
                    let (ct, session) = resp
                        .write_message_2(&plaintext)
                        .map_err(|e| SyncError::Transport(format!("write_message_2: {e}")))?;
                    slot.session = Some(session);
                    for waiter in slot.on_session_ready.drain(..) {
                        let _ = waiter.send(());
                    }
                    Some(ct)
                } else if let Some(session) = slot.session.as_mut() {
                    let ct = session
                        .encrypt(&plaintext)
                        .map_err(|e| SyncError::Transport(format!("session.encrypt: {e}")))?;
                    Some(ct)
                } else {
                    let (tx, rx) = oneshot::channel::<()>();
                    slot.on_session_ready.push(tx);
                    drop(inner);
                    let _ = rx.await;
                    None
                }
            };
            if let Some(ciphertext) = ciphertext_opt {
                let seq = self.next_send_seq(&to).await;
                self.write_entry(&to, seq, ciphertext).await?;
                return Ok(());
            }
        }
    }

    async fn recv(&self) -> SyncResult<SignalMessage> {
        let mut rx = self.inbound_rx.lock().await;
        rx.next()
            .await
            .ok_or_else(|| SyncError::Transport("signaler closed".into()))
    }
}

use crate::crypto::room::RoomFingerprint;
use std::cell::RefCell;

/// Routes signaling for a `WebRtcRawTransport` across N open rooms.
/// Holds a per-room `RelaySignaler` for each open room. `send` picks any
/// registered signaler (the receiver subscribes to all its open rooms,
/// so the message reaches them via any one); `recv` fans across all
/// per-room receivers via select!.
///
/// The Signaler trait impl comes in a follow-up task; this task only
/// implements register/unregister + introspection.
pub struct MultiRoomSignaler {
    by_room: RefCell<HashMap<RoomFingerprint, Rc<dyn Signaler>>>,
    /// Notifier fired when a new signaler is registered, so an in-flight
    /// `recv` blocked on the current set can re-do its select!.
    register_notify: tokio::sync::Notify,
}

impl MultiRoomSignaler {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            by_room: RefCell::new(HashMap::new()),
            register_notify: tokio::sync::Notify::new(),
        })
    }

    pub fn register<S: Store + 'static>(
        self: &Rc<Self>,
        fp: RoomFingerprint,
        signaler: Rc<RelaySignaler<S>>,
    ) {
        let dyn_signaler: Rc<dyn Signaler> = signaler;
        self.by_room.borrow_mut().insert(fp, dyn_signaler);
        self.register_notify.notify_waiters();
    }

    pub fn unregister(&self, fp: &RoomFingerprint) {
        self.by_room.borrow_mut().remove(fp);
    }

    pub fn len(&self) -> usize {
        self.by_room.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_room.borrow().is_empty()
    }

    pub fn contains(&self, fp: &RoomFingerprint) -> bool {
        self.by_room.borrow().contains_key(fp)
    }
}

#[async_trait(?Send)]
impl Signaler for MultiRoomSignaler {
    async fn send(&self, message: SignalMessage) -> SyncResult<()> {
        // Pick the first registered per-room signaler (HashMap iteration
        // order, but it's stable within a process so connect_direct
        // retries land on the same room). The receiver subscribes to
        // all its open rooms, so any single room is a sufficient
        // carrier — provided both peers have that room open.
        //
        // KNOWN LIMITATION: if `to` doesn't have the chosen carrier
        // room open, the signaling entry lands in the relay's store
        // but `to` never reads it; `connect_direct` then times out at
        // the WebRTC layer with no helpful error here. Callers who
        // *can* check shared-room overlap (membership tracker) should
        // do so before invoking `connect_direct`. Broadcasting through
        // every room is NOT a valid workaround: per-room signalers
        // hold independent Noise_KK state, so N copies of msg1 would
        // each initiate an independent handshake and confuse the
        // receiver's slot machine.
        //
        // If no rooms are registered, fail loudly.
        let signaler = {
            let map = self.by_room.borrow();
            map.values().next().cloned()
        };
        match signaler {
            Some(s) => s.send(message).await,
            None => Err(SyncError::Transport(
                "MultiRoomSignaler::send with no rooms registered \
                 (call Peer::open_room before connect_direct)"
                    .into(),
            )),
        }
    }

    async fn recv(&self) -> SyncResult<SignalMessage> {
        // Loop: snapshot the current set of per-room signalers, race
        // their recv()s + the register_notify. If a new signaler
        // registers, re-snapshot.
        loop {
            let signalers: Vec<Rc<dyn Signaler>> =
                { self.by_room.borrow().values().cloned().collect() };
            if signalers.is_empty() {
                // No signalers — wait for a registration.
                self.register_notify.notified().await;
                continue;
            }
            // Build a select! across N recvs + the notify.
            let mut futures: futures::stream::FuturesUnordered<_> = signalers
                .iter()
                .map(|s| {
                    let s = s.clone();
                    async move { s.recv().await }
                })
                .collect();
            tokio::select! {
                biased;
                _ = self.register_notify.notified() => {
                    // New room registered; re-snapshot.
                    continue;
                }
                Some(result) = futures::StreamExt::next(&mut futures) => {
                    return result;
                }
            }
        }
    }
}

#[cfg(test)]
mod multi_room_tests {
    use super::*;
    use crate::Ed25519Verifier;
    use crate::Identity;
    use crate::Room;
    use crate::crypto::constants::test_fast_params;
    use std::sync::Arc;
    use sunset_store_memory::MemoryStore;

    fn ident(seed: u8) -> Identity {
        Identity::from_secret_bytes(&[seed; 32])
    }

    fn store() -> Arc<MemoryStore> {
        Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn register_inserts_and_unregister_removes() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dispatcher = MultiRoomSignaler::new();
                let id = ident(1);
                let st = store();
                let room = Room::open_with_params("alpha", &test_fast_params())
                    .expect("Room::open_with_params");
                let fp = room.fingerprint();
                let signaler = RelaySignaler::new(id, fp.to_hex(), &st);

                assert_eq!(dispatcher.len(), 0);
                assert!(!dispatcher.contains(&fp));

                dispatcher.register(fp, signaler);
                assert_eq!(dispatcher.len(), 1);
                assert!(dispatcher.contains(&fp));

                dispatcher.unregister(&fp);
                assert_eq!(dispatcher.len(), 0);
                assert!(!dispatcher.contains(&fp));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_routes_to_registered_signaler_and_reaches_via_recv() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Two peers (Alice, Bob) sharing one room. Each builds a
                // MultiRoomSignaler with one entry. Alice sends to Bob; Bob
                // recv's the message.
                let alice_id = ident(1);
                let bob_id = ident(2);
                let alice_pk = PeerId(alice_id.store_verifying_key());
                let bob_pk = PeerId(bob_id.store_verifying_key());

                // Shared store, simulating a fully-replicated relay so both
                // signalers see the same entries.
                let st = store();
                let room =
                    Room::open_with_params("alpha", &test_fast_params()).expect("Room::open");
                let fp = room.fingerprint();

                let alice_signaler = RelaySignaler::new(alice_id, fp.to_hex(), &st);
                let bob_signaler = RelaySignaler::new(bob_id, fp.to_hex(), &st);

                let alice_dispatcher = MultiRoomSignaler::new();
                alice_dispatcher.register(fp, alice_signaler);
                let bob_dispatcher = MultiRoomSignaler::new();
                bob_dispatcher.register(fp, bob_signaler);

                let payload = bytes::Bytes::from_static(b"hello-bob");
                alice_dispatcher
                    .send(SignalMessage {
                        from: alice_pk.clone(),
                        to: bob_pk.clone(),
                        seq: 0,
                        payload: payload.clone(),
                    })
                    .await
                    .expect("alice.send");

                let received =
                    tokio::time::timeout(std::time::Duration::from_secs(2), bob_dispatcher.recv())
                        .await
                        .expect("recv timed out")
                        .expect("recv error");

                // The payload that arrives is decrypted Noise plaintext, which is
                // our original `payload` bytes (KK first message carries an attached
                // payload that's plaintext after decryption).
                assert_eq!(received.from, alice_pk);
                assert_eq!(received.to, bob_pk);
                assert_eq!(received.payload.as_ref(), b"hello-bob");
            })
            .await;
    }
}
