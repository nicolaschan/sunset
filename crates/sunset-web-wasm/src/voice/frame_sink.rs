//! `FrameSink` that calls a JS function with the codec-encoded
//! payload + codec_id so the host can route to the per-peer
//! WebCodecs `AudioDecoder` (and from there to the playback worklet).
//!
//! Volume is applied browser-side via per-peer `GainNode` (wired in
//! the JS layer); per-peer drop calls `on_drop` so JS can release
//! the AudioWorkletNode + decoder.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Uint8Array};
use wasm_bindgen::JsValue;

use sunset_sync::PeerId;
use sunset_voice::FrameSink;

pub(crate) struct WebFrameSink {
    /// Called as `on_frame(peer_id: Uint8Array, payload: Uint8Array, codec_id: string)`.
    pub on_frame: Rc<RefCell<Option<Function>>>,
    /// Called as `on_drop(peer_id: Uint8Array)`.
    pub on_drop: Rc<RefCell<Option<Function>>>,
}

impl FrameSink for WebFrameSink {
    fn deliver(&self, peer: &PeerId, payload: &[u8], codec_id: &str) {
        if let Some(f) = self.on_frame.borrow().as_ref() {
            let id = Uint8Array::from(peer.0.as_bytes());
            let payload_arr = Uint8Array::from(payload);
            let codec = JsValue::from_str(codec_id);
            let _ = f.call3(&JsValue::NULL, &id, &payload_arr, &codec);
        }
    }

    fn drop_peer(&self, peer: &PeerId) {
        if let Some(f) = self.on_drop.borrow().as_ref() {
            let id = Uint8Array::from(peer.0.as_bytes());
            let _ = f.call1(&JsValue::NULL, &id);
        }
    }
}
