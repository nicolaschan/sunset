//! JS marshaling for the reaction tracker's snapshot callbacks.

use js_sys::Map;
use sunset_core::ReactionSnapshot;
use sunset_store::Hash;
use wasm_bindgen::prelude::*;

/// Build the JS payload object dispatched to the FE's
/// `on_reactions_changed` callback. Shape:
///
/// ```ts
/// {
///   target_hex: string,
///   reactions: Map<emoji_string, Map<author_pubkey_hex, sent_at_ms>>
/// }
/// ```
///
pub fn snapshot_to_js(target: &Hash, snapshot: &ReactionSnapshot) -> JsValue {
    let map = Map::new();
    for (emoji, authors) in snapshot {
        let inner = Map::new();
        for (author, sent_at_ms) in authors {
            inner.set(
                &JsValue::from_str(&hex::encode(author.as_bytes())),
                &JsValue::from_f64(*sent_at_ms as f64),
            );
        }
        map.set(&JsValue::from_str(emoji), &inner.into());
    }
    let obj = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &obj,
        &JsValue::from_str("target_hex"),
        &JsValue::from_str(&target.to_hex()),
    );
    let _ = js_sys::Reflect::set(&obj, &JsValue::from_str("reactions"), &map.into());
    obj.into()
}
