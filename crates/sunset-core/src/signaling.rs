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
