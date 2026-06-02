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
    /// Session frames (the sender's `seq >= 1`) that arrived before this
    /// handshake's `msg2` (`seq == 0`) established the session. The CRDT
    /// signaling channel can deliver entries out of order; a session frame
    /// must never be fed to the handshake (`read_message_2` consumes the
    /// initiator by value), so it waits here, keyed by seq, and is drained
    /// in order the moment the session comes up. Cleared on reset / rejoin.
    pending: std::collections::BTreeMap<u64, Vec<u8>>,
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

        // Decrypting a `msg2` (seq 0) can release session frames that were
        // buffered while it was missing, so this yields zero or more
        // plaintexts, each tagged with its own seq.
        let delivered = self.decrypt_inbound(&from, seq, ciphertext).await?;
        for (out_seq, plaintext) in delivered {
            let _ = inbound_tx.unbounded_send(SignalMessage {
                from: from.clone(),
                to: to.clone(),
                seq: out_seq,
                payload: Bytes::from(plaintext),
            });
        }
        Ok(())
    }

    /// Decrypt one inbound signaling frame and return the plaintext(s) to
    /// surface — paired with their seq, since establishing the session here
    /// can drain previously-buffered session frames.
    ///
    /// `seq` carries the protocol's own frame discriminator: a handshake
    /// frame is *always* the sender's `seq == 0` (a fresh `msg1`, or the
    /// responder's `msg2`; `reset_peer` rewinds to 0 so a rejoin's `msg1`
    /// is seq 0 too), and a session/ICE frame is *always* `seq >= 1`. The
    /// CRDT signaling channel can deliver entries out of order, so routing
    /// by this seq is what keeps a reordered session frame from ever
    /// reaching the handshake state (`read_message_2` consumes the
    /// initiator by value; feeding it a session frame would destroy a live
    /// dial — the three-way-voice flake).
    async fn decrypt_inbound(
        &self,
        from: &PeerId,
        seq: u64,
        ciphertext: &[u8],
    ) -> SyncResult<Vec<(u64, Vec<u8>)>> {
        let mut inner = self.inner.lock().await;
        let slot = inner.peers.entry(from.clone()).or_default();

        if seq >= 1 {
            // Session frame. Only the live session may touch it; the
            // handshake must never see it.
            if let Some(session) = slot.session.as_mut() {
                return match session.decrypt(ciphertext) {
                    Ok(pt) => Ok(vec![(seq, pt)]),
                    // Undecryptable session frame ⇒ stale (a superseded
                    // generation). Drop it; never fall back to the handshake.
                    Err(_) => Ok(vec![]),
                };
            }
            // Handshake not finished yet: hold the frame until `msg2`
            // (seq 0) brings the session up, then it drains in seq order.
            slot.pending.insert(seq, ciphertext.to_vec());
            return Ok(vec![]);
        }

        // seq == 0: a handshake frame. If a peer with the same static
        // identity restarts (page refresh) it sends a fresh `msg1`, which
        // looks like a corrupted session message to whichever side held the
        // live session and like a corrupted `msg2` to whichever side was
        // mid-handshake; in both cases we fall through to "treat this as a
        // new responder kicking off a fresh handshake" rather than dropping
        // it. KK's static-key authentication still constrains who can
        // produce a valid `msg1`, so the fallback can't be exploited for
        // impersonation. (Replaying an old valid `msg1` can force a session
        // reset, but that is a pre-existing DoS surface: anyone who can
        // write to the relay can already censor signaling.)
        if let Some(session) = slot.session.as_mut() {
            match session.decrypt(ciphertext) {
                Ok(pt) => return Ok(vec![(seq, pt)]),
                Err(_) => { /* fall through to rehandshake attempt */ }
            }
        }
        if let Some(init) = slot.initiator.take() {
            match init.read_message_2(ciphertext) {
                Ok((pt, mut session)) => {
                    for waiter in slot.on_session_ready.drain(..) {
                        let _ = waiter.send(());
                    }
                    // Session is live: drain the frames that arrived ahead
                    // of this msg2, in ascending seq order.
                    let mut out = vec![(seq, pt)];
                    for (s, ct) in std::mem::take(&mut slot.pending) {
                        if let Ok(p) = session.decrypt(&ct) {
                            out.push((s, p));
                        }
                    }
                    slot.session = Some(session);
                    return Ok(out);
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
        // writes msg2. Buffered session frames belong to the dead
        // generation and can never decrypt against the new key, so drop them.
        slot.session = None;
        slot.initiator = None;
        slot.responder = Some(resp);
        slot.pending.clear();
        // Rewind the send seq so this generation's `msg2` lands at seq 0,
        // exactly as `reset_peer` does for the dialer's `msg1`. Without
        // this, a rejoin's `msg2` would inherit the prior call's
        // next_send_seq (> 0), and the dialer's seq-routing would mistake a
        // seq>=1 `msg2` for a session frame and hang. (The new msg2
        // overwrites the dead generation's msg2 at seq 0 by LWW; its
        // orphaned higher-seq frames are the same acceptable noise
        // `reset_peer` already documents.)
        slot.next_send_seq = 0;
        Ok(vec![(seq, pt)])
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
            // Session frames buffered against the old handshake are stale.
            slot.pending.clear();
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

                let alice = RelaySignaler::new(alice_id, fp_hex.clone(), &alice_store);
                let bob = RelaySignaler::new(bob_id, fp_hex.clone(), &bob_store);
                let alice_disp = MultiRoomSignaler::new();
                alice_disp.register(fp, alice);
                let bob_disp = MultiRoomSignaler::new();
                bob_disp.register(fp, bob);

                // 1. Alice dials: writes msg1 (her seq 0).
                alice_disp
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
                let got = tokio::time::timeout(std::time::Duration::from_secs(2), bob_disp.recv())
                    .await
                    .expect("bob recv msg1 timed out")
                    .expect("bob recv msg1");
                assert_eq!(got.payload.as_ref(), b"offer");

                // 3. Bob answers (msg2 = his seq 0), then trickles a session
                //    frame (his seq 1).
                bob_disp
                    .send(SignalMessage {
                        from: bob_pk.clone(),
                        to: alice_pk.clone(),
                        seq: 0,
                        payload: Bytes::from_static(b"answer"),
                    })
                    .await
                    .expect("bob send msg2");
                bob_disp
                    .send(SignalMessage {
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
                let answer =
                    tokio::time::timeout(std::time::Duration::from_secs(2), alice_disp.recv())
                        .await
                        .expect(
                            "alice recv timed out — initiator destroyed by reordered session frame",
                        )
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
                let alice1 = RelaySignaler::new(alice_id.clone(), fp_hex.clone(), &alice1_store);
                let bob = RelaySignaler::new(bob_id, fp_hex.clone(), &bob_store);
                let alice1_disp = MultiRoomSignaler::new();
                alice1_disp.register(fp, alice1);
                let bob_disp = MultiRoomSignaler::new();
                bob_disp.register(fp, bob);

                alice1_disp
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
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), bob_disp.recv())
                    .await
                    .expect("bob recv v1 offer")
                    .expect("bob recv v1 offer err");
                bob_disp
                    .send(SignalMessage {
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
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), alice1_disp.recv())
                    .await
                    .expect("alice1 recv answer")
                    .expect("alice1 recv answer err");

                // Rejoin: alice refreshes — a fresh signaler + store, same
                // identity. Her new msg1 is at seq 0 (fresh slot).
                let alice2_store = store();
                let alice2 = RelaySignaler::new(alice_id, fp_hex.clone(), &alice2_store);
                let alice2_disp = MultiRoomSignaler::new();
                alice2_disp.register(fp, alice2);

                alice2_disp
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
                let got = tokio::time::timeout(std::time::Duration::from_secs(2), bob_disp.recv())
                    .await
                    .expect("bob recv v2 offer")
                    .expect("bob recv v2 offer err");
                assert_eq!(got.payload.as_ref(), b"offer-v2");

                // Bob rebuilds his responder and answers — his next_send_seq
                // is past 0 from the first call.
                bob_disp
                    .send(SignalMessage {
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

                let answer =
                    tokio::time::timeout(std::time::Duration::from_secs(2), alice2_disp.recv())
                        .await
                        .expect("alice2 recv timed out — responder's rejoin msg2 not at seq 0")
                        .expect("alice2 recv answer err");
                assert_eq!(answer.payload.as_ref(), b"answer-v2");
            })
            .await;
    }
}
