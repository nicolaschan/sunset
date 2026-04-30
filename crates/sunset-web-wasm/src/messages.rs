//! IncomingMessage type exposed to JS + helpers to convert from sunset-core's
//! DecodedMessage.

use wasm_bindgen::prelude::*;

use sunset_core::{DecodedMessage, MessageBody};

/// JS-facing decoded message. Mirrors sunset-core's DecodedMessage but
/// uses JS-friendly types (BigInt → f64 for timestamps, Vec<u8> → Uint8Array).
#[wasm_bindgen]
pub struct IncomingMessage {
    #[wasm_bindgen(getter_with_clone)]
    pub author_pubkey: Vec<u8>,
    pub epoch_id: u64,
    pub sent_at_ms: f64,
    #[wasm_bindgen(getter_with_clone)]
    pub body: String,
    #[wasm_bindgen(getter_with_clone)]
    pub value_hash_hex: String,
    pub is_self: bool,
}

pub fn from_decoded(
    decoded: DecodedMessage,
    value_hash_hex: String,
    is_self: bool,
) -> IncomingMessage {
    let body = match decoded.body {
        MessageBody::Text(t) => t,
        _ => String::new(),
    };
    IncomingMessage {
        author_pubkey: decoded.author_key.as_bytes().to_vec(),
        epoch_id: decoded.epoch_id,
        sent_at_ms: decoded.sent_at_ms as f64,
        body,
        value_hash_hex,
        is_self,
    }
}
