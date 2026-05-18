//! IncomingMessage / IncomingReceipt types exposed to JS, plus helpers
//! to convert from sunset-core's DecodedMessage.

use wasm_bindgen::prelude::*;

use sunset_core::{ChannelLabel, DecodedMessage, IdentityKey, ImageAttachment};

/// JS-facing decoded text message.
#[wasm_bindgen]
pub struct IncomingMessage {
    #[wasm_bindgen(getter_with_clone)]
    pub author_pubkey: Vec<u8>,
    pub epoch_id: u64,
    pub sent_at_ms: f64,
    /// Channel this message was sent in (e.g. `"general"`). Always
    /// present; defaults to `"general"` for un-channeled (legacy) sends.
    #[wasm_bindgen(getter_with_clone)]
    pub channel: String,
    #[wasm_bindgen(getter_with_clone)]
    pub body: String,
    #[wasm_bindgen(getter_with_clone)]
    pub value_hash_hex: String,
    pub is_self: bool,
    /// JS-side `Array<{ mime_type, data_base64 }>`. Built per-message so
    /// the JS bridge can render each entry as a `<img>` tag directly
    /// (`src="data:${mime_type};base64,${data_base64}"`). Empty array
    /// for text-only messages.
    #[wasm_bindgen(getter_with_clone)]
    pub images: js_sys::Array,
}

/// JS-facing decoded delivery receipt.
#[wasm_bindgen]
pub struct IncomingReceipt {
    /// Hex-encoded value_hash of the Text being acknowledged.
    #[wasm_bindgen(getter_with_clone)]
    pub for_value_hash_hex: String,
    /// Verifying key bytes of the peer who composed this receipt.
    #[wasm_bindgen(getter_with_clone)]
    pub from_pubkey: Vec<u8>,
    /// Channel the acknowledged message was sent in.
    #[wasm_bindgen(getter_with_clone)]
    pub channel: String,
    /// Wall-clock unix-ms when the acknowledging peer composed this
    /// receipt. Surfaced in the message-details panel as the
    /// "delivered-at" stamp per recipient.
    pub sent_at_ms: f64,
}

/// Build an IncomingMessage from a decoded Text. The text and image
/// list are passed in separately so the caller can pattern-match
/// `MessageBody` upstream and pass the inner fields directly without
/// re-cloning the body.
pub fn from_decoded_text(
    decoded: &DecodedMessage,
    text: String,
    images: &[ImageAttachment],
    value_hash_hex: String,
    is_self: bool,
) -> IncomingMessage {
    IncomingMessage {
        author_pubkey: decoded.author_key.as_bytes().to_vec(),
        epoch_id: decoded.epoch_id,
        sent_at_ms: decoded.sent_at_ms as f64,
        channel: decoded.channel.as_str().to_owned(),
        body: text,
        value_hash_hex,
        is_self,
        images: images_to_js(images),
    }
}

/// Encode a slice of [`ImageAttachment`]s into a JS `Array` of plain
/// `{ mime_type, data_base64 }` objects. The JS side reads these
/// directly without any wasm-bindgen wrapper class.
fn images_to_js(images: &[ImageAttachment]) -> js_sys::Array {
    let arr = js_sys::Array::new_with_length(images.len() as u32);
    for (i, img) in images.iter().enumerate() {
        let obj = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("mime_type"),
            &JsValue::from_str(&img.mime_type),
        );
        let _ = js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("data_base64"),
            &JsValue::from_str(&img.data_base64),
        );
        arr.set(i as u32, obj.into());
    }
    arr
}

/// Build an IncomingReceipt JS object.
pub fn receipt_to_js(
    for_value_hash_hex: String,
    from_pubkey: &IdentityKey,
    channel: &ChannelLabel,
    sent_at_ms: u64,
) -> IncomingReceipt {
    IncomingReceipt {
        for_value_hash_hex,
        from_pubkey: from_pubkey.as_bytes().to_vec(),
        channel: channel.as_str().to_owned(),
        sent_at_ms: sent_at_ms as f64,
    }
}
