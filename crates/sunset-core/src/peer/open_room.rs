//! `OpenRoom` is the per-room handle returned by `Peer::open_room`.
//! Method bodies (send_text, on_message, presence, etc.) are filled in
//! by Phase 5+ of the multi-room plan. This file currently only declares
//! the data shape so `Peer` can hold its registry of `Weak<RoomState>`.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use sunset_sync::Transport;
use sunset_store::Store;

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
                    crate::MessageBody::Receipt { for_value_hash: _ } => {
                        if let Some(cb) = cbs.on_receipt.as_ref() {
                            cb(decoded.value_hash, &decoded.author_key);
                        }
                    }
                }
            }
        });
    }
}

impl<St: Store + 'static, T: Transport + 'static> Drop for RoomState<St, T> {
    fn drop(&mut self) {
        self.cancel_decode.set(true);
        if let Some(peer) = self.peer_weak.upgrade() {
            peer.rtc_signaler_dispatcher.unregister(&self.room.fingerprint());
        }
    }
}
