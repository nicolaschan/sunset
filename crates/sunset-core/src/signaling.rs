//! `Signaler` over the `Store`: each `SignalMessage` is a `SignedKvEntry`
//! `<room_fp_hex>/webrtc/<from_hex>/<to_hex>/<seq:016x>` carrying Noise_KK
//! ciphertext. Noise state is per peer; the room is only the carrier.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use futures::stream::{AbortHandle, Abortable};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use crate::crypto::room::RoomFingerprint;
use crate::{EntryDraft, Identity};
use sunset_noise::{KkInitiator, KkResponder, KkSession, ed25519_seed_to_x25519_secret};
use sunset_store::{ContentBlock, Filter, Replay, SignedKvEntry, Store, VerifyingKey};
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

fn parse_entry_name(name: &[u8]) -> Option<(PeerId, PeerId, u64)> {
    let s = std::str::from_utf8(name).ok()?;
    let mut parts = s.split('/').skip(2);
    let from_hex = parts.next()?;
    let to_hex = parts.next()?;
    let seq_hex = parts.next()?;
    let peer = |h: &str| Some(PeerId(VerifyingKey::new(Bytes::from(hex::decode(h).ok()?))));
    Some((
        peer(from_hex)?,
        peer(to_hex)?,
        u64::from_str_radix(seq_hex, 16).ok()?,
    ))
}

fn x25519_pub_for(peer: &PeerId) -> SyncResult<[u8; 32]> {
    let bytes: &[u8] = peer.verifying_key().as_bytes();
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| SyncError::Transport(format!("peer pubkey wrong length: {}", bytes.len())))?;
    sunset_noise::ed25519_public_to_x25519(&arr)
        .map_err(|e| SyncError::Transport(format!("x25519 derive: {e}")))
}

#[derive(Default)]
struct PeerKkSlot {
    initiator: Option<KkInitiator>,
    responder: Option<KkResponder>,
    session: Option<KkSession>,
    next_send_seq: u64,
    on_session_ready: Vec<oneshot::Sender<()>>,
    /// Session frames that overtook `msg2`; drained in seq order once the
    /// session exists.
    pending: BTreeMap<u64, Vec<u8>>,
}

pub struct RelaySignaler<S: Store + 'static> {
    local_identity: Identity,
    local_x25519_secret: Zeroizing<[u8; 32]>,
    store: Arc<S>,
    peers: Mutex<HashMap<PeerId, PeerKkSlot>>,
    rooms: RefCell<HashMap<RoomFingerprint, AbortHandle>>,
    inbound_tx: mpsc::UnboundedSender<SignalMessage>,
    inbound_rx: Mutex<mpsc::UnboundedReceiver<SignalMessage>>,
}

impl<S: Store + 'static> RelaySignaler<S> {
    pub fn new(local_identity: Identity, store: &Arc<S>) -> Rc<Self> {
        let local_x25519_secret = ed25519_seed_to_x25519_secret(&local_identity.secret_bytes());
        let (inbound_tx, inbound_rx) = mpsc::unbounded();
        Rc::new(Self {
            local_identity,
            local_x25519_secret,
            store: store.clone(),
            peers: Mutex::new(HashMap::new()),
            rooms: RefCell::new(HashMap::new()),
            inbound_tx,
            inbound_rx: Mutex::new(inbound_rx),
        })
    }

    pub fn register_room(self: &Rc<Self>, room: RoomFingerprint) {
        let mut rooms = self.rooms.borrow_mut();
        if rooms.contains_key(&room) {
            return;
        }
        let (handle, registration) = AbortHandle::new_pair();
        rooms.insert(room, handle);
        let me = self.clone();
        sunset_sync::spawn::spawn_local(async move {
            let _ = Abortable::new(me.pump(room.to_hex()), registration).await;
        });
    }

    pub fn unregister_room(&self, room: &RoomFingerprint) {
        if let Some(handle) = self.rooms.borrow_mut().remove(room) {
            handle.abort();
        }
    }

    pub fn room_count(&self) -> usize {
        self.rooms.borrow().len()
    }

    pub fn has_room(&self, room: &RoomFingerprint) -> bool {
        self.rooms.borrow().contains_key(room)
    }

    fn local_peer(&self) -> PeerId {
        PeerId(self.local_identity.store_verifying_key())
    }

    async fn pump(&self, room_fp_hex: String) {
        let mut events = match self
            .store
            .subscribe(signaling_filter(&room_fp_hex), Replay::All)
            .await
        {
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
            if let Err(e) = self.handle_entry(&entry).await {
                tracing::warn!("RelaySignaler handle_entry: {e}");
            }
        }
    }

    async fn handle_entry(&self, entry: &SignedKvEntry) -> SyncResult<()> {
        let (from, to, seq) = parse_entry_name(&entry.name)
            .ok_or_else(|| SyncError::Transport("bad signaling entry name".into()))?;
        if to != self.local_peer() || from == self.local_peer() {
            return Ok(());
        }
        let block = self
            .store
            .get_content(&entry.value_hash)
            .await?
            .ok_or_else(|| SyncError::Transport("missing content block".into()))?;
        for (out_seq, plaintext) in self.decrypt_inbound(&from, seq, &block.data).await? {
            let _ = self.inbound_tx.unbounded_send(SignalMessage {
                from: from.clone(),
                to: to.clone(),
                seq: out_seq,
                payload: Bytes::from(plaintext),
            });
        }
        Ok(())
    }

    /// `seq == 0` is always a handshake frame (`reset_peer` and rehandshake
    /// rewind to 0), `seq >= 1` a session frame. The CRDT channel reorders
    /// and `read_message_2` consumes the initiator, so a session frame must
    /// never reach the handshake.
    async fn decrypt_inbound(
        &self,
        from: &PeerId,
        seq: u64,
        ciphertext: &[u8],
    ) -> SyncResult<Vec<(u64, Vec<u8>)>> {
        let mut peers = self.peers.lock().await;
        let slot = peers.entry(from.clone()).or_default();

        if seq >= 1 {
            if let Some(session) = slot.session.as_mut() {
                // Undecryptable ⇒ stale generation.
                return Ok(session
                    .decrypt(ciphertext)
                    .map(|pt| vec![(seq, pt)])
                    .unwrap_or_default());
            }
            slot.pending.insert(seq, ciphertext.to_vec());
            return Ok(vec![]);
        }

        // A restarted peer sends a fresh `msg1` that nothing we hold can
        // decrypt, so every arm falls through to a new responder. KK static
        // keys mean only the real peer can produce a valid `msg1`.
        if let Some(session) = slot.session.as_mut() {
            if let Ok(pt) = session.decrypt(ciphertext) {
                return Ok(vec![(seq, pt)]);
            }
        }
        if let Some(init) = slot.initiator.take() {
            if let Ok((pt, mut session)) = init.read_message_2(ciphertext) {
                for waiter in slot.on_session_ready.drain(..) {
                    let _ = waiter.send(());
                }
                let mut out = vec![(seq, pt)];
                for (s, ct) in std::mem::take(&mut slot.pending) {
                    if let Ok(p) = session.decrypt(&ct) {
                        out.push((s, p));
                    }
                }
                slot.session = Some(session);
                return Ok(out);
            }
        }

        let mut resp = KkResponder::new(&self.local_x25519_secret, &x25519_pub_for(from)?)
            .map_err(|e| SyncError::Transport(format!("KkResponder::new: {e}")))?;
        let pt = resp
            .read_message_1(ciphertext)
            .map_err(|e| SyncError::Transport(format!("read_message_1: {e}")))?;
        // Fresh generation; rewind seq so our `msg2` lands at seq 0.
        *slot = PeerKkSlot {
            responder: Some(resp),
            on_session_ready: std::mem::take(&mut slot.on_session_ready),
            ..Default::default()
        };
        Ok(vec![(seq, pt)])
    }

    async fn write_entry(&self, to: &PeerId, ciphertext: Vec<u8>) -> SyncResult<()> {
        let room_fp_hex = self
            .rooms
            .borrow()
            .keys()
            .next()
            .map(RoomFingerprint::to_hex)
            .ok_or_else(|| {
                SyncError::Transport(
                    "RelaySignaler::send with no rooms registered \
                     (call Peer::open_room before connect_direct)"
                        .into(),
                )
            })?;
        let seq = {
            let mut peers = self.peers.lock().await;
            let slot = peers.entry(to.clone()).or_default();
            slot.next_send_seq += 1;
            slot.next_send_seq - 1
        };
        let block = ContentBlock {
            data: Bytes::from(ciphertext),
            references: vec![],
        };
        let value_hash = block.hash();
        let priority = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let entry = self.local_identity.seal_entry(EntryDraft {
            name: entry_name(&room_fp_hex, &self.local_peer(), to, seq),
            value_hash,
            priority,
            expires_at: Some(priority + 3_600_000),
        });
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
        let to = message.to;
        let plaintext = message.payload;
        loop {
            let mut peers = self.peers.lock().await;
            let slot = peers.entry(to.clone()).or_default();
            let ciphertext =
                if slot.initiator.is_none() && slot.responder.is_none() && slot.session.is_none() {
                    let mut init =
                        KkInitiator::new(&self.local_x25519_secret, &x25519_pub_for(&to)?)
                            .map_err(|e| SyncError::Transport(format!("KkInitiator::new: {e}")))?;
                    let ct = init
                        .write_message_1(&plaintext)
                        .map_err(|e| SyncError::Transport(format!("write_message_1: {e}")))?;
                    slot.initiator = Some(init);
                    ct
                } else if let Some(resp) = slot.responder.take() {
                    let (ct, session) = resp
                        .write_message_2(&plaintext)
                        .map_err(|e| SyncError::Transport(format!("write_message_2: {e}")))?;
                    slot.session = Some(session);
                    for waiter in slot.on_session_ready.drain(..) {
                        let _ = waiter.send(());
                    }
                    ct
                } else if let Some(session) = slot.session.as_mut() {
                    session
                        .encrypt(&plaintext)
                        .map_err(|e| SyncError::Transport(format!("session.encrypt: {e}")))?
                } else {
                    let (tx, rx) = oneshot::channel::<()>();
                    slot.on_session_ready.push(tx);
                    drop(peers);
                    let _ = rx.await;
                    continue;
                };
            drop(peers);
            return self.write_entry(&to, ciphertext).await;
        }
    }

    async fn recv(&self) -> SyncResult<SignalMessage> {
        let mut rx = self.inbound_rx.lock().await;
        rx.next()
            .await
            .ok_or_else(|| SyncError::Transport("signaler closed".into()))
    }

    /// Rewinding seq to 0 makes the fresh `msg1` overwrite the old one
    /// (LWW), so a receiver replaying history never answers the dead
    /// session's `msg1`. Parked waiters are dropped: the fresh-initiator
    /// path never wakes them.
    async fn reset_peer(&self, peer: &PeerId) {
        if let Some(slot) = self.peers.lock().await.get_mut(peer) {
            *slot = PeerKkSlot::default();
        }
    }
}

#[cfg(test)]
mod tests {
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
                let signaler = RelaySignaler::new(ident(1), &store());
                let fp = Room::open_with_params("alpha", &test_fast_params())
                    .expect("Room::open_with_params")
                    .fingerprint();

                assert_eq!(signaler.room_count(), 0);
                assert!(!signaler.has_room(&fp));

                signaler.register_room(fp);
                signaler.register_room(fp);
                assert_eq!(signaler.room_count(), 1);
                assert!(signaler.has_room(&fp));

                signaler.unregister_room(&fp);
                assert_eq!(signaler.room_count(), 0);
                assert!(!signaler.has_room(&fp));
            })
            .await;
    }

    // When one side of a Noise_KK signaling pair restarts (page refresh
    // is the canonical case — same identity seed, fresh in-memory state),
    // the restarted side has no session state for its peer and naturally
    // sends a fresh KK msg1. The peer that *didn't* restart still holds
    // the old session, so its `decrypt_inbound` finds an active session
    // and tries `session.decrypt(new_msg1)` — which fails. Pre-fix, the
    // message was dropped on the floor and the restarted side's WebRTC
    // dial hung indefinitely.
    //
    // Fix: `decrypt_inbound` falls back to a fresh `KkResponder` when
    // the existing strategies fail, succeeds against a valid msg1
    // (KK static-key authentication keeps this safe — only the
    // peer's key can produce a valid msg1), and resets the slot to
    // the new responder. See `voice_rejoin_after_refresh.spec.js`
    // for the end-to-end coverage on WebRTC voice rejoin.
    //
    // This is a small DoS surface (an attacker who recorded an old
    // msg1 can replay it to force a session reset), but it's the same
    // DoS surface the relay already has (any peer in the room can
    // also just refuse to forward signaling entries). It does not
    // break confidentiality.
    //
    // Scope of this unit test: only the live-side (Bob's) decrypt
    // path. The post-restart side (Alice v2)'s receipt of Bob's
    // msg2 is *not* exercised here because in this unit setup Alice
    // and Bob share one in-memory store, so Alice v2's dispatcher
    // replays every entry Alice v1 ever wrote — and Alice v2's
    // initiator would be consumed by an `initiator.read_message_2`
    // attempt against a stale session ciphertext (Snow consumes the
    // initiator by value). In production the two peers hold
    // independent stores synced through the relay, and the
    // WebRTC dispatcher's "rejoin → cancel + restart accept" arm
    // (`per_peer` entries are tagged with `PerPeerKind::Accept` and
    // a monotonic generation) handles the equivalent rejoin race
    // at a layer above the signaler.
    #[tokio::test(flavor = "current_thread")]
    async fn alice_restart_with_same_identity_can_rehandshake_against_live_bob() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let alice_id = ident(1);
                let bob_id = ident(2);
                let alice_pk = PeerId(alice_id.store_verifying_key());
                let bob_pk = PeerId(bob_id.store_verifying_key());

                let st = store();
                let room =
                    Room::open_with_params("alpha", &test_fast_params()).expect("Room::open");
                let fp = room.fingerprint();

                // Phase 1: Alice and Bob establish a session.
                let alice_v1 = RelaySignaler::new(alice_id.clone(), &st);
                let bob = RelaySignaler::new(bob_id, &st);
                alice_v1.register_room(fp);
                bob.register_room(fp);

                alice_v1
                    .send(SignalMessage {
                        from: alice_pk.clone(),
                        to: bob_pk.clone(),
                        seq: 0,
                        payload: bytes::Bytes::from_static(b"hello-from-v1"),
                    })
                    .await
                    .expect("alice v1 → bob (msg1)");
                let r1 = tokio::time::timeout(std::time::Duration::from_secs(2), bob.recv())
                    .await
                    .expect("bob recv #1 timed out")
                    .expect("bob recv #1 err");
                assert_eq!(r1.payload.as_ref(), b"hello-from-v1");

                bob.send(SignalMessage {
                    from: bob_pk.clone(),
                    to: alice_pk.clone(),
                    seq: 0,
                    payload: bytes::Bytes::from_static(b"ack-from-bob"),
                })
                .await
                .expect("bob → alice v1 (msg2)");
                let r2 = tokio::time::timeout(std::time::Duration::from_secs(2), alice_v1.recv())
                    .await
                    .expect("alice v1 recv #1 timed out")
                    .expect("alice v1 recv #1 err");
                assert_eq!(r2.payload.as_ref(), b"ack-from-bob");

                // Phase 2: simulate Alice's page refresh. Drop the v1
                // signaler entirely, build a fresh v2 with the same
                // identity but no in-memory peer state.
                drop(alice_v1);
                let alice_v2 = RelaySignaler::new(alice_id, &st);
                alice_v2.register_room(fp);

                // Alice v2's first send is a fresh KK msg1 — her slot is
                // empty, so `send` takes the initiator-creation arm.
                // Bob's slot for Alice still has the v1 session; pre-fix,
                // Bob's `decrypt_inbound` calls `session.decrypt(new_msg1)`,
                // gets a Noise auth failure, and silently drops the
                // message. The recv below times out forever.
                //
                // Post-fix, Bob's `decrypt_inbound` falls back to a fresh
                // `KkResponder::read_message_1` when the session decrypt
                // fails, succeeds (because the message really is a valid
                // msg1 from Alice's static key), resets the slot, and
                // surfaces the plaintext to recv.
                alice_v2
                    .send(SignalMessage {
                        from: alice_pk.clone(),
                        to: bob_pk.clone(),
                        seq: 0,
                        payload: bytes::Bytes::from_static(b"hello-from-v2"),
                    })
                    .await
                    .expect("alice v2 → bob (fresh msg1)");
                let r3 = tokio::time::timeout(std::time::Duration::from_secs(2), bob.recv())
                    .await
                    .expect(
                        "bob recv #2 timed out — restarted Alice's msg1 never delivered \
                     (Noise session not reset on bob's side)",
                    )
                    .expect("bob recv #2 err");
                assert_eq!(r3.payload.as_ref(), b"hello-from-v2");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_routes_to_registered_signaler_and_reaches_via_recv() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Two peers (Alice, Bob) sharing one room.
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

                let alice_signaler = RelaySignaler::new(alice_id, &st);
                let bob_signaler = RelaySignaler::new(bob_id, &st);

                alice_signaler.register_room(fp);
                bob_signaler.register_room(fp);

                let payload = bytes::Bytes::from_static(b"hello-bob");
                alice_signaler
                    .send(SignalMessage {
                        from: alice_pk.clone(),
                        to: bob_pk.clone(),
                        seq: 0,
                        payload: payload.clone(),
                    })
                    .await
                    .expect("alice.send");

                let received =
                    tokio::time::timeout(std::time::Duration::from_secs(2), bob_signaler.recv())
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

    /// Copy the signaling entries authored by `author` from `src` into
    /// `dst`, in ascending (or, if `reversed`, descending) `seq` order.
    /// Stands in for relay replication so a test can choose the delivery
    /// order — the entry name embeds `seq:016x`, so a name sort is a seq
    /// sort.
    async fn replicate_authored(
        src: &Arc<MemoryStore>,
        dst: &Arc<MemoryStore>,
        room_fp_hex: &str,
        author: &VerifyingKey,
        reversed: bool,
    ) {
        let mut entries: Vec<(SignedKvEntry, ContentBlock)> = Vec::new();
        let mut it = src
            .iter(signaling_filter(room_fp_hex))
            .await
            .expect("iter signaling entries");
        while let Some(e) = it.next().await {
            let e = e.expect("entry");
            if &e.verifying_key != author {
                continue;
            }
            let block = src
                .get_content(&e.value_hash)
                .await
                .expect("get_content")
                .expect("block present");
            entries.push((e, block));
        }
        entries.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        if reversed {
            entries.reverse();
        }
        for (e, block) in entries {
            // A re-inserted equal-priority entry is `Stale`; that's fine.
            let _ = dst.insert(e, Some(block)).await;
        }
    }

    /// A session frame (the sender's `seq >= 1`) that the relay delivers
    /// *before* the handshake's `msg2` (the sender's `seq == 0`) must not
    /// break the dialer.
    ///
    /// This reproduces the three-way-voice flake: out-of-order replication
    /// fed the `seq >= 1` frame to the dialer's `KkInitiator::read_message_2`,
    /// which consumes the initiator by value, so the real `msg2` could never
    /// be read and the WebRTC dial hung in Connecting. The fix routes by the
    /// `seq` already in the entry name — a session frame never touches the
    /// handshake state.
    #[tokio::test(flavor = "current_thread")]
    async fn dialer_completes_when_session_frame_precedes_msg2() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let alice_id = ident(1);
                let bob_id = ident(2);
                let alice_pk = PeerId(alice_id.store_verifying_key());
                let bob_pk = PeerId(bob_id.store_verifying_key());

                // Separate stores, replicated by hand so the test owns the
                // delivery order — exactly what a relay does, but reordered.
                let alice_store = store();
                let bob_store = store();
                let room =
                    Room::open_with_params("alpha", &test_fast_params()).expect("Room::open");
                let fp = room.fingerprint();
                let fp_hex = fp.to_hex();

                let alice = RelaySignaler::new(alice_id, &alice_store);
                let bob = RelaySignaler::new(bob_id, &bob_store);
                alice.register_room(fp);
                bob.register_room(fp);

                // 1. Alice dials: writes msg1 (her seq 0).
                alice
                    .send(SignalMessage {
                        from: alice_pk.clone(),
                        to: bob_pk.clone(),
                        seq: 0,
                        payload: Bytes::from_static(b"offer"),
                    })
                    .await
                    .expect("alice send msg1");

                // 2. Replicate msg1 to Bob; Bob builds a responder + surfaces it.
                replicate_authored(
                    &alice_store,
                    &bob_store,
                    &fp_hex,
                    alice_pk.verifying_key(),
                    false,
                )
                .await;
                let got = tokio::time::timeout(std::time::Duration::from_secs(2), bob.recv())
                    .await
                    .expect("bob recv msg1 timed out")
                    .expect("bob recv msg1");
                assert_eq!(got.payload.as_ref(), b"offer");

                // 3. Bob answers (msg2 = his seq 0), then trickles a session
                //    frame (his seq 1).
                bob.send(SignalMessage {
                    from: bob_pk.clone(),
                    to: alice_pk.clone(),
                    seq: 0,
                    payload: Bytes::from_static(b"answer"),
                })
                .await
                .expect("bob send msg2");
                bob.send(SignalMessage {
                    from: bob_pk.clone(),
                    to: alice_pk.clone(),
                    seq: 0,
                    payload: Bytes::from_static(b"ice-1"),
                })
                .await
                .expect("bob send session frame");

                // 4. Replicate Bob's frames to Alice OUT OF ORDER: the seq-1
                //    session frame lands before the seq-0 msg2.
                replicate_authored(
                    &bob_store,
                    &alice_store,
                    &fp_hex,
                    bob_pk.verifying_key(),
                    true,
                )
                .await;

                // 5. Alice must still complete the handshake and surface the
                //    answer. Pre-fix the reordered seq-1 frame destroyed her
                //    initiator and this recv timed out forever.
                let answer = tokio::time::timeout(std::time::Duration::from_secs(2), alice.recv())
                    .await
                    .expect("alice recv timed out — initiator destroyed by reordered session frame")
                    .expect("alice recv answer");
                assert_eq!(answer.payload.as_ref(), b"answer");
            })
            .await;
    }

    /// On a rejoin (the dialer refreshes and re-handshakes), the responder
    /// rebuilds its handshake against the fresh `msg1`, but its
    /// `next_send_seq` is already past 0 from the prior call — so its new
    /// `msg2` must still land at seq 0 (the way `reset_peer` rewinds the
    /// dialer's `msg1`). Otherwise the dialer's seq-routing mistakes the
    /// `msg2` for a session frame, buffers it, and the dial hangs — the
    /// `voice_rejoin_after_refresh` / `voice_rejoin_matrix` failure.
    #[tokio::test(flavor = "current_thread")]
    async fn rejoin_dialer_completes_when_responder_resends_msg2_after_prior_call() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let alice_id = ident(1);
                let bob_id = ident(2);
                let alice_pk = PeerId(alice_id.store_verifying_key());
                let bob_pk = PeerId(bob_id.store_verifying_key());

                let alice1_store = store();
                let bob_store = store();
                let room =
                    Room::open_with_params("alpha", &test_fast_params()).expect("Room::open");
                let fp = room.fingerprint();
                let fp_hex = fp.to_hex();

                // First call: alice_v1 <-> bob complete a handshake, which
                // advances bob's next_send_seq for alice past 0.
                let alice1 = RelaySignaler::new(alice_id.clone(), &alice1_store);
                let bob = RelaySignaler::new(bob_id, &bob_store);
                alice1.register_room(fp);
                bob.register_room(fp);

                alice1
                    .send(SignalMessage {
                        from: alice_pk.clone(),
                        to: bob_pk.clone(),
                        seq: 0,
                        payload: Bytes::from_static(b"offer-v1"),
                    })
                    .await
                    .expect("alice1 msg1");
                replicate_authored(
                    &alice1_store,
                    &bob_store,
                    &fp_hex,
                    alice_pk.verifying_key(),
                    false,
                )
                .await;
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), bob.recv())
                    .await
                    .expect("bob recv v1 offer")
                    .expect("bob recv v1 offer err");
                bob.send(SignalMessage {
                    from: bob_pk.clone(),
                    to: alice_pk.clone(),
                    seq: 0,
                    payload: Bytes::from_static(b"answer-v1"),
                })
                .await
                .expect("bob msg2 v1");
                replicate_authored(
                    &bob_store,
                    &alice1_store,
                    &fp_hex,
                    bob_pk.verifying_key(),
                    false,
                )
                .await;
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), alice1.recv())
                    .await
                    .expect("alice1 recv answer")
                    .expect("alice1 recv answer err");

                // Rejoin: alice refreshes — a fresh signaler + store, same
                // identity. Her new msg1 is at seq 0 (fresh slot).
                let alice2_store = store();
                let alice2 = RelaySignaler::new(alice_id, &alice2_store);
                alice2.register_room(fp);

                alice2
                    .send(SignalMessage {
                        from: alice_pk.clone(),
                        to: bob_pk.clone(),
                        seq: 0,
                        payload: Bytes::from_static(b"offer-v2"),
                    })
                    .await
                    .expect("alice2 msg1");
                replicate_authored(
                    &alice2_store,
                    &bob_store,
                    &fp_hex,
                    alice_pk.verifying_key(),
                    false,
                )
                .await;
                let got = tokio::time::timeout(std::time::Duration::from_secs(2), bob.recv())
                    .await
                    .expect("bob recv v2 offer")
                    .expect("bob recv v2 offer err");
                assert_eq!(got.payload.as_ref(), b"offer-v2");

                // Bob rebuilds his responder and answers — his next_send_seq
                // is past 0 from the first call.
                bob.send(SignalMessage {
                    from: bob_pk.clone(),
                    to: alice_pk.clone(),
                    seq: 0,
                    payload: Bytes::from_static(b"answer-v2"),
                })
                .await
                .expect("bob msg2 v2");
                replicate_authored(
                    &bob_store,
                    &alice2_store,
                    &fp_hex,
                    bob_pk.verifying_key(),
                    false,
                )
                .await;

                let answer = tokio::time::timeout(std::time::Duration::from_secs(2), alice2.recv())
                    .await
                    .expect("alice2 recv timed out — responder's rejoin msg2 not at seq 0")
                    .expect("alice2 recv answer err");
                assert_eq!(answer.payload.as_ref(), b"answer-v2");
            })
            .await;
    }
}
