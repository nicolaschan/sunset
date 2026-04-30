//! IncomingMessage / IncomingReceipt types exposed to JS, plus helpers
//! to convert from sunset-core's DecodedMessage.

use wasm_bindgen::prelude::*;

use sunset_core::{DecodedMessage, IdentityKey};

/// JS-facing decoded text message.
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

/// JS-facing decoded delivery receipt.
#[wasm_bindgen]
pub struct IncomingReceipt {
    /// Hex-encoded value_hash of the Text being acknowledged.
    #[wasm_bindgen(getter_with_clone)]
    pub for_value_hash_hex: String,
    /// Verifying key bytes of the peer who composed this receipt.
    #[wasm_bindgen(getter_with_clone)]
    pub from_pubkey: Vec<u8>,
}

/// Build an IncomingMessage from a decoded Text. The text is passed in
/// separately so the caller can pattern-match `MessageBody` upstream
/// and pass only the inner String.
pub fn from_decoded_text(
    decoded: DecodedMessage,
    text: String,
    value_hash_hex: String,
    is_self: bool,
) -> IncomingMessage {
    IncomingMessage {
        author_pubkey: decoded.author_key.as_bytes().to_vec(),
        epoch_id: decoded.epoch_id,
        sent_at_ms: decoded.sent_at_ms as f64,
        body: text,
        value_hash_hex,
        is_self,
    }
}

/// Build an IncomingReceipt JS object.
pub fn receipt_to_js(for_value_hash_hex: String, from_pubkey: IdentityKey) -> IncomingReceipt {
    IncomingReceipt {
        for_value_hash_hex,
        from_pubkey: from_pubkey.as_bytes().to_vec(),
    }
}
