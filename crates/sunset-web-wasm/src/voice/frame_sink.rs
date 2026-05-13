//! `FrameSink` that calls a JS function with `(peer_id, seq, pcm)` so
//! JS can route to the per-peer playback worklet. The worklet uses
//! `seq` for sequence-indexed buffering and gap detection. Volume is
//! applied browser-side via per-peer GainNode.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Float32Array, Function, Uint8Array};
use wasm_bindgen::JsValue;

use sunset_sync::PeerId;
use sunset_voice::FrameSink;

pub(crate) struct WebFrameSink {
    /// Called as `on_pcm(peer_id: Uint8Array, seq: number, pcm: Float32Array)`.
    pub on_pcm: Rc<RefCell<Option<Function>>>,
    /// Called as `on_drop(peer_id: Uint8Array)`.
    pub on_drop: Rc<RefCell<Option<Function>>>,
}

impl FrameSink for WebFrameSink {
    fn deliver(&self, peer: &PeerId, seq: u32, pcm: &[f32]) {
        if let Some(f) = self.on_pcm.borrow().as_ref() {
            let id = Uint8Array::from(peer.0.as_bytes());
            let arr = Float32Array::from(pcm);
            let seq_val = JsValue::from_f64(seq as f64);
            let _ = f.call3(&JsValue::NULL, &id, &seq_val, &arr);
        }
    }

    fn drop_peer(&self, peer: &PeerId) {
        if let Some(f) = self.on_drop.borrow().as_ref() {
            let id = Uint8Array::from(peer.0.as_bytes());
            let _ = f.call1(&JsValue::NULL, &id);
        }
    }
}
