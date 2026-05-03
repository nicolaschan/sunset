//! `#[wasm_bindgen]` wrapper around `sunset_core::OpenRoom`.

use wasm_bindgen::prelude::*;

use sunset_core::OpenRoom;
use sunset_store_memory::MemoryStore;
use sunset_sync::MultiTransport;

use crate::client::{RtcT, WsT};

#[wasm_bindgen]
pub struct RoomHandle {
    inner: OpenRoom<MemoryStore, MultiTransport<WsT, RtcT>>,
}

impl RoomHandle {
    pub(crate) fn new(inner: OpenRoom<MemoryStore, MultiTransport<WsT, RtcT>>) -> Self {
        Self { inner }
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
        self.inner.on_receipt(move |for_hash, from_pubkey| {
            let incoming = crate::messages::receipt_to_js(for_hash.to_hex(), from_pubkey);
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

    pub fn on_relay_status_changed(&self, callback: js_sys::Function) {
        self.inner.on_relay_status_changed(move |status| {
            let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(status));
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
}
