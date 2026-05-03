//! JS marshaling for the reaction tracker's snapshot callbacks.

use js_sys::{Map, Set};
use sunset_core::ReactionSnapshot;
use sunset_store::Hash;
use wasm_bindgen::prelude::*;

/// Build the JS payload object dispatched to the FE's
/// `on_reactions_changed` callback. Shape:
///
/// ```ts
/// {
///   target_hex: string,
///   reactions: Map<emoji_string, Set<author_pubkey_hex>>
/// }
/// ```
pub fn snapshot_to_js(target: &Hash, snapshot: &ReactionSnapshot) -> JsValue {
    let map = Map::new();
    for (emoji, authors) in snapshot {
        let set = Set::new(&JsValue::UNDEFINED);
        for author in authors {
            set.add(&JsValue::from_str(&hex::encode(author.as_bytes())));
        }
        map.set(&JsValue::from_str(emoji), &set.into());
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
