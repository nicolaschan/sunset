//! `OpenRoom` is the per-room handle returned by `Peer::open_room`.
//! Method bodies (send_text, on_message, presence, etc.) are filled in
//! by Phase 5+ of the multi-room plan. This file currently only declares
//! the data shape so `Peer` can hold its registry of `Weak<RoomState>`.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use sunset_store::Store;
use sunset_sync::Transport;

use crate::crypto::room::Room;
use crate::membership::TrackerHandles;
use crate::message::DecodedMessage;
use crate::signaling::RelaySignaler;

pub(crate) type MessageCallback = Box<dyn Fn(&DecodedMessage, bool /* is_self */)>;
pub(crate) type ReceiptCallback = Box<dyn Fn(sunset_store::Hash, &crate::IdentityKey)>;

#[derive(Default)]
pub(crate) struct RoomCallbacks {
    pub(crate) on_message: Option<MessageCallback>,
    pub(crate) on_receipt: Option<ReceiptCallback>,
}

// Fields presence_started, tracker_handles, signaler are read by
// send_text, on_message, presence methods arriving in Phase 5+.
#[allow(dead_code)]
pub(crate) struct RoomState<St: Store + 'static, T: Transport + 'static> {
    pub(crate) room: Rc<Room>,
    pub(crate) peer_weak: Weak<super::Peer<St, T>>,
    pub(crate) presence_started: Cell<bool>,
    pub(crate) tracker_handles: Rc<TrackerHandles>,
    pub(crate) signaler: Rc<RelaySignaler<St>>,
    pub(crate) cancel_decode: Rc<Cell<bool>>,
    pub(crate) callbacks: Rc<RefCell<RoomCallbacks>>,
}

pub struct OpenRoom<St: Store + 'static, T: Transport + 'static> {
    pub(crate) inner: Rc<RoomState<St, T>>,
}

impl<St: Store + 'static, T: Transport + 'static> OpenRoom<St, T> {
    pub fn fingerprint(&self) -> crate::crypto::room::RoomFingerprint {
        self.inner.room.fingerprint()
    }

    pub async fn send_text(
        &self,
        body: String,
        sent_at_ms: u64,
    ) -> crate::Result<sunset_store::Hash> {
        use crate::MessageBody;
        use crate::compose_message;
        use rand_chacha::ChaCha20Rng;
        use rand_core::SeedableRng;

        let peer = self
            .inner
            .peer_weak
            .upgrade()
            .ok_or_else(|| crate::Error::Other("peer dropped".into()))?;

        let mut rng = ChaCha20Rng::from_entropy();
        let composed = compose_message(
            peer.identity(),
            &self.inner.room,
            0u64,
            sent_at_ms,
            MessageBody::Text(body),
            &mut rng,
        )?;

        let value_hash = composed.entry.value_hash;
        peer.store()
            .insert(composed.entry, Some(composed.block))
            .await
            .map_err(|e| crate::Error::Other(format!("store insert: {e}")))?;
        Ok(value_hash)
    }

    pub fn on_message<F: Fn(&DecodedMessage, bool) + 'static>(&self, cb: F) {
        let mut cbs = self.inner.callbacks.borrow_mut();
        let was_unregistered = cbs.on_message.is_none() && cbs.on_receipt.is_none();
        cbs.on_message = Some(Box::new(cb));
        drop(cbs);

        // First on_message/on_receipt call kicks off the decode loop.
        if was_unregistered {
            self.spawn_decode_loop();
        }
    }

    pub fn on_receipt<F: Fn(sunset_store::Hash, &crate::IdentityKey) + 'static>(&self, cb: F) {
        let mut cbs = self.inner.callbacks.borrow_mut();
        let was_unregistered = cbs.on_message.is_none() && cbs.on_receipt.is_none();
        cbs.on_receipt = Some(Box::new(cb));
        drop(cbs);
        if was_unregistered {
            self.spawn_decode_loop();
        }
    }

    fn spawn_decode_loop(&self) {
        let inner = self.inner.clone();
        let peer = match inner.peer_weak.upgrade() {
            Some(p) => p,
            None => return,
        };
        let store = peer.store().clone();
        let identity_pub = peer.identity().public();
        let room = inner.room.clone();
        let cancel = inner.cancel_decode.clone();
        let callbacks = inner.callbacks.clone();

        sunset_sync::spawn::spawn_local(async move {
            use futures::StreamExt;
            use sunset_store::{Event, Replay};

            let filter = crate::filters::room_messages_filter(&room);
            let mut events = match store.subscribe(filter, Replay::All).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("OpenRoom decode subscribe: {e}");
                    return;
                }
            };
            while let Some(ev) = events.next().await {
                if cancel.get() {
                    return;
                }
                let entry = match ev {
                    Ok(Event::Inserted(e)) => e,
                    Ok(Event::Replaced { new, .. }) => new,
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::error!("OpenRoom decode event: {e}");
                        continue;
                    }
                };
                let block = match store.get_content(&entry.value_hash).await {
                    Ok(Some(b)) => b,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::error!("OpenRoom decode get_content: {e}");
                        continue;
                    }
                };
                let decoded = match crate::message::decode_message(&room, &entry, &block) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("OpenRoom decode_message: {e}");
                        continue;
                    }
                };

                let is_self = decoded.author_key == identity_pub;
                let cbs = callbacks.borrow();
                match &decoded.body {
                    crate::MessageBody::Text(_) => {
                        if let Some(cb) = cbs.on_message.as_ref() {
                            cb(&decoded, is_self);
                        }
                    }
                    crate::MessageBody::Receipt { for_value_hash } => {
                        if let Some(cb) = cbs.on_receipt.as_ref() {
                            cb(*for_value_hash, &decoded.author_key);
                        }
                    }
                }
            }
        });
    }

    pub async fn start_presence(&self, interval_ms: u64, ttl_ms: u64, refresh_ms: u64) {
        if self.inner.presence_started.replace(true) {
            return;
        }
        let peer = match self.inner.peer_weak.upgrade() {
            Some(p) => p,
            None => return,
        };
        let room_fp_hex = self.inner.room.fingerprint().to_hex();
        let local_peer = sunset_sync::PeerId(peer.identity().store_verifying_key());

        crate::membership::spawn_publisher(
            peer.identity().clone(),
            room_fp_hex.clone(),
            peer.store().clone(),
            interval_ms,
            ttl_ms,
        );

        let engine_events = peer.engine().subscribe_engine_events().await;
        let snapshot = peer.engine().current_peers().await;
        {
            let mut peer_kinds = self.inner.tracker_handles.peer_kinds.borrow_mut();
            for (pk, kind) in snapshot {
                peer_kinds.insert(pk, kind);
            }
        }

        crate::membership::spawn_tracker(
            peer.store().clone(),
            engine_events,
            local_peer,
            crate::membership::PresenceConfig {
                room_fp_hex,
                interval_ms,
                ttl_ms,
                refresh_ms,
            },
            (*self.inner.tracker_handles).clone(),
        );

        crate::membership::fire_relay_status_now(&self.inner.tracker_handles);
    }

    pub async fn connect_direct(&self, peer_pubkey: [u8; 32]) -> crate::Result<()> {
        let peer = self
            .inner
            .peer_weak
            .upgrade()
            .ok_or_else(|| crate::Error::Other("peer dropped".into()))?;
        let x_pub = sunset_noise::ed25519_public_to_x25519(&peer_pubkey)
            .map_err(|e| crate::Error::Other(format!("x25519 derive: {e}")))?;
        let addr_str = format!(
            "webrtc://{}#x25519={}",
            hex::encode(peer_pubkey),
            hex::encode(x_pub)
        );
        let addr = sunset_sync::PeerAddr::new(bytes::Bytes::from(addr_str));
        peer.supervisor()
            .add(addr)
            .await
            .map_err(|e| crate::Error::Other(format!("connect_direct: {e}")))?;
        Ok(())
    }

    pub fn peer_connection_mode(&self, peer_pubkey: [u8; 32]) -> &'static str {
        use sunset_sync::TransportKind;
        let peer_id = sunset_sync::PeerId(sunset_store::VerifyingKey::new(
            bytes::Bytes::copy_from_slice(&peer_pubkey),
        ));
        match self.inner.tracker_handles.peer_kinds.borrow().get(&peer_id) {
            Some(TransportKind::Secondary) => "direct",
            Some(TransportKind::Primary) => "via_relay",
            _ => "unknown",
        }
    }

    pub fn on_members_changed<F: Fn(&[crate::membership::Member]) + 'static>(&self, cb: F) {
        *self.inner.tracker_handles.on_members.borrow_mut() = Some(Box::new(cb));
        // Match Client::on_members_changed: clear last_signature so the next
        // refresh tick fires the callback with the current snapshot.
        self.inner
            .tracker_handles
            .last_signature
            .borrow_mut()
            .clear();
    }

    pub fn on_relay_status_changed<F: Fn(&str) + 'static>(&self, cb: F) {
        *self.inner.tracker_handles.on_relay_status.borrow_mut() = Some(Box::new(cb));
    }
}

impl<St: Store + 'static, T: Transport + 'static> Drop for RoomState<St, T> {
    fn drop(&mut self) {
        self.cancel_decode.set(true);
        if let Some(peer) = self.peer_weak.upgrade() {
            peer.rtc_signaler_dispatcher
                .unregister(&self.room.fingerprint());
        }
    }
}
