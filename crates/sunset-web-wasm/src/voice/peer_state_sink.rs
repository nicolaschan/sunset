//! `PeerStateSink` that calls the JS `on_voice_peer_state` callback.
//!
//! Called as `on_voice_peer_state(peer_id: Uint8Array, in_call: bool,
//! talking: bool, is_muted: bool)`.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, Uint8Array};
use wasm_bindgen::JsValue;

use sunset_voice::{PeerStateSink, VoicePeerState};

pub(crate) struct WebPeerStateSink {
    pub handler: Rc<RefCell<Option<Function>>>,
}

impl PeerStateSink for WebPeerStateSink {
    fn emit(&self, state: &VoicePeerState) {
        if let Some(f) = self.handler.borrow().as_ref() {
            let id = Uint8Array::from(state.peer.0.as_bytes());
            let args = Array::of4(
                &id,
                &JsValue::from_bool(state.in_call),
                &JsValue::from_bool(state.talking),
                &JsValue::from_bool(state.is_muted),
            );
            let _ = f.apply(&JsValue::NULL, &args);
        }
    }
}
