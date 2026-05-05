//! `#[wasm_bindgen]` wrapper around `sunset_core::OpenRoom`.

use std::rc::Rc;

use wasm_bindgen::prelude::*;

use sunset_core::OpenRoom;
use sunset_store_memory::MemoryStore;
use sunset_sync::MultiTransport;

use crate::client::{RtcT, WsT};

pub(crate) type OpenRoomT = OpenRoom<MemoryStore, MultiTransport<WsT, RtcT>>;

#[wasm_bindgen]
pub struct RoomHandle {
    /// Rc-wrapped so the voice subsystem can hold a clone without
    /// requiring `RoomHandle` to outlive the voice session.
    inner: Rc<OpenRoomT>,
}

impl RoomHandle {
    pub(crate) fn new(inner: OpenRoomT) -> Self {
        Self {
            inner: Rc::new(inner),
        }
    }

    /// Clone the inner `Rc<OpenRoom>` for the voice dialer.
    pub(crate) fn open_room_rc(&self) -> Rc<OpenRoomT> {
        self.inner.clone()
    }

    /// Extract the `Rc<Room>` from the inner `OpenRoom`.
    pub(crate) fn room_rc(&self) -> Rc<sunset_core::crypto::room::Room> {
        self.inner.room()
    }
}

#[wasm_bindgen]
impl RoomHandle {
    pub async fn send_message(&self, body: String, sent_at_ms: f64) -> Result<String, JsError> {
        let value_hash = self
            .inner
            .send_text(body, sent_at_ms as u64)
            .await
            .map_err(|e| JsError::new(&format!("send_text: {e}")))?;
        Ok(value_hash.to_hex())
    }

    pub fn on_message(&self, callback: js_sys::Function) {
        self.inner.on_message(move |decoded, is_self| {
            if let sunset_core::MessageBody::Text(text) = &decoded.body {
                let im = crate::messages::from_decoded_text(
                    decoded,
                    text.clone(),
                    decoded.value_hash.to_hex(),
                    is_self,
                );
                let _ = callback.call1(&JsValue::NULL, &JsValue::from(im));
            }
        });
    }

    pub fn on_receipt(&self, callback: js_sys::Function) {
        self.inner
            .on_receipt(move |for_hash, from_pubkey, _channel, sent_at_ms| {
                // Channel is plumbed through to the JS surface in Task
                // 6; the current `receipt_to_js` shape is unchanged so
                // we ignore the channel here for now.
                let incoming =
                    crate::messages::receipt_to_js(for_hash.to_hex(), from_pubkey, sent_at_ms);
                let _ = callback.call1(&JsValue::NULL, &JsValue::from(incoming));
            });
    }

    pub fn on_members_changed(&self, callback: js_sys::Function) {
        self.inner.on_members_changed(move |members| {
            let arr = js_sys::Array::new();
            for m in members {
                arr.push(&JsValue::from(crate::members::MemberJs::from(m)));
            }
            let _ = callback.call1(&JsValue::NULL, &arr);
        });
    }

    pub async fn start_presence(&self, interval_ms: u32, ttl_ms: u32, refresh_ms: u32) {
        self.inner
            .start_presence(interval_ms as u64, ttl_ms as u64, refresh_ms as u64)
            .await;
    }

    pub async fn connect_direct(&self, peer_pubkey: &[u8]) -> Result<(), JsError> {
        let pk: [u8; 32] = peer_pubkey
            .try_into()
            .map_err(|_| JsError::new("peer_pubkey must be 32 bytes"))?;
        self.inner
            .connect_direct(pk)
            .await
            .map_err(|e| JsError::new(&format!("connect_direct: {e}")))?;
        Ok(())
    }

    pub fn peer_connection_mode(&self, peer_pubkey: &[u8]) -> String {
        let pk: [u8; 32] = match peer_pubkey.try_into() {
            Ok(p) => p,
            Err(_) => return "unknown".to_owned(),
        };
        self.inner.peer_connection_mode(pk).to_owned()
    }

    pub fn on_reactions_changed(&self, callback: js_sys::Function) {
        self.inner
            .on_reactions_changed(move |target, _channel, snapshot| {
                let payload = crate::reactions::snapshot_to_js(target, snapshot);
                let _ = callback.call1(&JsValue::NULL, &payload);
            });
    }

    pub async fn send_reaction(
        &self,
        target_value_hash_hex: String,
        emoji: String,
        action: String,
    ) -> Result<(), JsError> {
        let action = match action.as_str() {
            "add" => sunset_core::ReactionAction::Add,
            "remove" => sunset_core::ReactionAction::Remove,
            other => {
                return Err(JsError::new(&format!(
                    "send_reaction: action must be \"add\" or \"remove\", got {other:?}"
                )));
            }
        };
        let target_bytes = hex::decode(&target_value_hash_hex)
            .map_err(|e| JsError::new(&format!("send_reaction: bad target hex: {e}")))?;
        if target_bytes.len() != 32 {
            return Err(JsError::new(
                "send_reaction: target hex must decode to 32 bytes",
            ));
        }
        let mut target_arr = [0u8; 32];
        target_arr.copy_from_slice(&target_bytes);
        let target: sunset_store::Hash = target_arr.into();
        let now_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.inner
            .send_reaction(target, emoji, action, now_ms)
            .await
            .map_err(|e| JsError::new(&format!("send_reaction: {e}")))?;
        Ok(())
    }
}
