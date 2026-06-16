//! `#[wasm_bindgen]` wrapper around `sunset_core::OpenRoom`.

use std::rc::Rc;

use wasm_bindgen::prelude::*;

use base64::Engine as _;
use sunset_core::{ImageAttachment, OpenRoom};
use sunset_store_memory::MemoryStore;
use sunset_sync::MultiTransport;

use crate::client::{RtcT, WsT};

/// Decode a JS `Array<{ data_base64 }>` of staged attachments into a
/// `Vec<ImageAttachment>`, **running each entry through
/// `ImageAttachment::preprocess`** so the bytes that hit the wire are
/// the normalised JPEG (or pass-through GIF / WebP for animated
/// formats) produced by `sunset-image`, never the original camera
/// payload.
///
/// The JS bridge keeps the raw bytes around for the composer's
/// thumbnail strip; what crosses this boundary is the post-preprocess
/// wire form. The JS-side `mime_type` field is sent but ignored — the
/// sniffer trusts magic bytes (browsers mis-label HEIC and renamed
/// files). Errors surface back to JS as `JsError` strings the caller
/// can render.
fn images_from_js(arr: &js_sys::Array) -> Result<Vec<ImageAttachment>, JsError> {
    let len = arr.length() as usize;
    let mut out = Vec::with_capacity(len);
    let b64 = base64::engine::general_purpose::STANDARD;
    for i in 0..arr.length() {
        let item = arr.get(i);
        let data = js_sys::Reflect::get(&item, &JsValue::from_str("data_base64"))
            .map_err(|_| JsError::new(&format!("images[{i}]: missing data_base64")))?
            .as_string()
            .ok_or_else(|| JsError::new(&format!("images[{i}].data_base64 must be a string")))?;
        let raw = b64
            .decode(&data)
            .map_err(|e| JsError::new(&format!("images[{i}]: base64 decode: {e}")))?;
        let attachment = ImageAttachment::preprocess(&raw)
            .map_err(|e| JsError::new(&format!("images[{i}]: {e}")))?;
        out.push(attachment);
    }
    Ok(out)
}

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
    /// Send a chat post under `channel`. `images` is a JS `Array` of
    /// `{ mime_type, data_base64 }` plain objects; pass an empty array
    /// for a text-only message. An empty `body` is allowed when
    /// `images` is non-empty (image-only post). Returns the composed
    /// entry's value-hash hex.
    pub async fn send_message(
        &self,
        channel: String,
        body: String,
        images: js_sys::Array,
        sent_at_ms: f64,
    ) -> Result<String, JsError> {
        let channel = sunset_core::ChannelLabel::try_new(channel)
            .map_err(|e| JsError::new(&format!("send_message channel: {e}")))?;
        let images = images_from_js(&images)?;
        let value_hash = self
            .inner
            .send_post_in_channel(channel, body, images, sent_at_ms as u64)
            .await
            .map_err(|e| JsError::new(&format!("send_post: {e}")))?;
        Ok(value_hash.to_hex())
    }

    pub fn on_message(&self, callback: js_sys::Function) {
        self.inner.on_message(move |decoded, is_self| {
            if let sunset_core::MessageBody::Text { text, images } = &decoded.body {
                let im = crate::messages::from_decoded_text(
                    decoded,
                    text.clone(),
                    images,
                    decoded.value_hash.to_hex(),
                    is_self,
                );
                let _ = callback.call1(&JsValue::NULL, &JsValue::from(im));
            }
        });
    }

    /// Sorted snapshot of every Text message in this room, ordered by
    /// sender-claimed `sent_at_ms` ascending (tie-broken on value-hash
    /// for stability). Receipts and Reactions are not included — they
    /// don't render as messages. Returns an `Array<IncomingMessage>`
    /// that the JS bridge can iterate directly.
    pub fn ordered_messages(&self) -> js_sys::Array {
        let arr = js_sys::Array::new();
        let identity_pub = self.inner.local_identity_key();
        for decoded in self.inner.ordered_messages() {
            if let sunset_core::MessageBody::Text { text, images } = &decoded.body {
                let is_self = identity_pub.as_ref() == Some(&decoded.author_key);
                let im = crate::messages::from_decoded_text(
                    &decoded,
                    text.clone(),
                    images,
                    decoded.value_hash.to_hex(),
                    is_self,
                );
                arr.push(&JsValue::from(im));
            }
        }
        arr
    }

    /// Register a JS callback fired (immediately with the current
    /// sorted snapshot, then again on every change) with an
    /// `Array<IncomingMessage>` of all Text messages in this room,
    /// ordered by sender-claimed `sent_at_ms`. The bridge handles all
    /// ordering so JS / Gleam clients can render the array as-is.
    pub fn on_messages_changed(&self, callback: js_sys::Function) {
        let inner = self.inner.clone();
        self.inner.on_messages_changed(move |msgs| {
            // Resolve `is_self` from the *current* identity each fire.
            // The Peer's identity doesn't change at runtime, but
            // taking the value here (instead of capturing at register
            // time) keeps the wiring resilient if a future refactor
            // ever does swap it.
            let identity_pub = inner.local_identity_key();
            let arr = js_sys::Array::new();
            for decoded in msgs {
                if let sunset_core::MessageBody::Text { text, images } = &decoded.body {
                    let is_self = identity_pub.as_ref() == Some(&decoded.author_key);
                    let im = crate::messages::from_decoded_text(
                        decoded,
                        text.clone(),
                        images,
                        decoded.value_hash.to_hex(),
                        is_self,
                    );
                    arr.push(&JsValue::from(im));
                }
            }
            let _ = callback.call1(&JsValue::NULL, &arr);
        });
    }

    pub fn on_receipt(&self, callback: js_sys::Function) {
        self.inner
            .on_receipt(move |for_hash, from_pubkey, channel, sent_at_ms| {
                let incoming = crate::messages::receipt_to_js(
                    for_hash.to_hex(),
                    from_pubkey,
                    channel,
                    sent_at_ms,
                );
                let _ = callback.call1(&JsValue::NULL, &JsValue::from(incoming));
            });
    }

    /// Sorted snapshot of channels the decode loop has observed in this
    /// room so far. Always contains `"general"`.
    pub fn observed_channels(&self) -> js_sys::Array {
        let arr = js_sys::Array::new();
        for c in self.inner.observed_channels() {
            arr.push(&JsValue::from_str(c.as_str()));
        }
        arr
    }

    /// Register a JS callback that fires (immediately with the current
    /// sorted snapshot, then again on every change) with an Array of
    /// channel name strings.
    pub fn on_channels_changed(&self, callback: js_sys::Function) {
        self.inner.on_channels_changed(move |chans| {
            let arr = js_sys::Array::new();
            for c in chans {
                arr.push(&JsValue::from_str(c.as_str()));
            }
            let _ = callback.call1(&JsValue::NULL, &arr);
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
        // The inner call now returns an `IntentId` so session-scoped
        // callers (the voice runtime) can cancel the intent on stop —
        // see `OpenRoom::connect_direct` rustdoc. JS callers go through
        // this wrapper which has always been fire-and-forget, so the
        // id is discarded here to keep the JS signature stable.
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
            .on_reactions_changed(move |target, channel, snapshot| {
                let payload = crate::reactions::snapshot_to_js(target, channel, snapshot);
                let _ = callback.call1(&JsValue::NULL, &payload);
            });
    }

    pub async fn send_reaction(
        &self,
        channel: String,
        target_value_hash_hex: String,
        emoji: String,
        action: String,
    ) -> Result<(), JsError> {
        let channel = sunset_core::ChannelLabel::try_new(channel)
            .map_err(|e| JsError::new(&format!("send_reaction channel: {e}")))?;
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
            .send_reaction_in_channel(channel, target, emoji, action, now_ms)
            .await
            .map_err(|e| JsError::new(&format!("send_reaction: {e}")))?;
        Ok(())
    }
}
