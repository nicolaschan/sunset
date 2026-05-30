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

        // Try the slot's current strategy first. If a peer with the same
        // static identity restarts (page refresh, process restart) it loses
        // its in-memory KK state and naturally sends a fresh msg1 — which
        // looks like a corrupted session message to whichever side held
        // the live session, and like a corrupted msg2 to whichever side
        // was mid-handshake. In both cases we want to fall back to
        // "treat this as a new responder kicking off a fresh handshake"
        // rather than dropping the message. The KK pattern's static-key
        // authentication still constrains who can produce a valid msg1
        // — only the peer's holder of the matching static key — so the
        // fallback can't be exploited for impersonation. (Replay of an
        // old valid msg1 *can* force a session reset, but that's a
        // pre-existing DoS surface: any party who can write to the
        // relay can already censor signaling.)
        if let Some(session) = slot.session.as_mut() {
            match session.decrypt(ciphertext) {
                Ok(pt) => return Ok(pt),
                Err(_) => { /* fall through to rehandshake attempt */ }
            }
        }
        if let Some(init) = slot.initiator.take() {
            match init.read_message_2(ciphertext) {
                Ok((pt, session)) => {
                    slot.session = Some(session);
                    for waiter in slot.on_session_ready.drain(..) {
                        let _ = waiter.send(());
                    }
                    return Ok(pt);
                }
                Err(_) => { /* fall through to rehandshake attempt */ }
            }
        }

        // Either the slot was fresh, or every higher-priority strategy
        // failed. Try as a new responder; if read_message_1 also fails,
        // the bytes really are garbage and we surface the error.
        let remote_x = self.x25519_pub_for(from).await?;
        let mut resp = KkResponder::new(&self.local_x25519_secret, &remote_x)
            .map_err(|e| SyncError::Transport(format!("KkResponder::new: {e}")))?;
        let pt = resp
            .read_message_1(ciphertext)
            .map_err(|e| SyncError::Transport(format!("read_message_1: {e}")))?;
        // Successful re-handshake: discard whatever stale state we had
        // (it can't decrypt anything sent against the new key) and pin
        // the slot to the fresh responder so the next outbound `send`
        // writes msg2.
        slot.session = None;
        slot.initiator = None;
        slot.responder = Some(resp);
        Ok(pt)
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

        let entry = self.local_identity.seal_entry(EntryDraft {
            name: entry_name(&self.room_fp_hex, &from, to, seq),
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

    async fn reset_peer(&self, peer: &PeerId) {
        // Drop the Noise session / handshake state for `peer`. The next
        // outbound `send` takes the empty-slot arm and writes a fresh KK
        // msg1; the receiver's `decrypt_inbound` falls back to a new
        // responder (see the bug doc above on `decrypt_inbound`).
        //
        // We also rewind `next_send_seq` to 0 so the new msg1 lands at
        // `<from>/<to>/0` and *overwrites* the prior session's msg1
        // (CRDT LWW by priority, and the new entry has a fresh-now
        // priority that beats any historical entry). Without this, the
        // new msg1 would be at a *higher* seq while the old msg1
        // remains at seq=0; a freshly-started receiver replaying
        // signaling history would then `read_message_1` against the
        // *old* msg1 first, build a responder bound to the dead
        // session's ephemeral key, and answer with an msg2 that the
        // dialer's *new* initiator can't decrypt. The dial then
        // hangs and the supervisor's intent stays in `Connecting`
        // until backoff/give-up — exactly the "rejoin → no audio"
        // failure mode `voice_rejoin_after_refresh.spec.js` catches.
        //
        // Leftover ICE candidates at higher seqs from the dead
        // session remain in the store; the receiver routes them to
        // the new per-peer queue where `addIceCandidate` fails
        // non-fatally on the wrong ufrag/pwd (the v2 SDP has
        // different credentials). That's acceptable noise rather
        // than a correctness bug.
        let mut inner = self.inner.lock().await;
        if let Some(slot) = inner.peers.get_mut(peer) {
            slot.session = None;
            slot.initiator = None;
            slot.responder = None;
            slot.next_send_seq = 0;
            // `on_session_ready` waiters are deliberately *not* preserved
            // here: if a concurrent `send` is parked waiting for a
            // session, the next `send` after this reset will take the
            // fresh-initiator arm and that path doesn't go through
            // `on_session_ready` — the parked waiter would block
            // indefinitely. Drop them so they wake (with an effective
            // cancellation) and the corresponding caller can retry.
            slot.on_session_ready.clear();
        }
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

    async fn reset_peer(&self, peer: &PeerId) {
        // Reset the slot in *every* per-room signaler. Per-room
        // signalers hold independent Noise_KK state, so a partial
        // reset (e.g. only the one we'd use for the next `send`) would
        // leave stale state in other rooms ready to corrupt a future
        // dial that picks a different carrier room.
        let signalers: Vec<Rc<dyn Signaler>> =
            { self.by_room.borrow().values().cloned().collect() };
        for s in signalers {
            s.reset_peer(peer).await;
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
                let alice_v1 = RelaySignaler::new(alice_id.clone(), fp.to_hex(), &st);
                let bob = RelaySignaler::new(bob_id, fp.to_hex(), &st);
                let alice_v1_disp = MultiRoomSignaler::new();
                alice_v1_disp.register(fp, alice_v1);
                let bob_disp = MultiRoomSignaler::new();
                bob_disp.register(fp, bob);

                alice_v1_disp
                    .send(SignalMessage {
                        from: alice_pk.clone(),
                        to: bob_pk.clone(),
                        seq: 0,
                        payload: bytes::Bytes::from_static(b"hello-from-v1"),
                    })
                    .await
                    .expect("alice v1 → bob (msg1)");
                let r1 = tokio::time::timeout(std::time::Duration::from_secs(2), bob_disp.recv())
                    .await
                    .expect("bob recv #1 timed out")
                    .expect("bob recv #1 err");
                assert_eq!(r1.payload.as_ref(), b"hello-from-v1");

                bob_disp
                    .send(SignalMessage {
                        from: bob_pk.clone(),
                        to: alice_pk.clone(),
                        seq: 0,
                        payload: bytes::Bytes::from_static(b"ack-from-bob"),
                    })
                    .await
                    .expect("bob → alice v1 (msg2)");
                let r2 =
                    tokio::time::timeout(std::time::Duration::from_secs(2), alice_v1_disp.recv())
                        .await
                        .expect("alice v1 recv #1 timed out")
                        .expect("alice v1 recv #1 err");
                assert_eq!(r2.payload.as_ref(), b"ack-from-bob");

                // Phase 2: simulate Alice's page refresh. Drop the v1
                // signaler entirely, build a fresh v2 with the same
                // identity but no in-memory peer state.
                drop(alice_v1_disp);
                let alice_v2 = RelaySignaler::new(alice_id, fp.to_hex(), &st);
                let alice_v2_disp = MultiRoomSignaler::new();
                alice_v2_disp.register(fp, alice_v2);

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
                alice_v2_disp
                    .send(SignalMessage {
                        from: alice_pk.clone(),
                        to: bob_pk.clone(),
                        seq: 0,
                        payload: bytes::Bytes::from_static(b"hello-from-v2"),
                    })
                    .await
                    .expect("alice v2 → bob (fresh msg1)");
                let r3 = tokio::time::timeout(std::time::Duration::from_secs(2), bob_disp.recv())
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
