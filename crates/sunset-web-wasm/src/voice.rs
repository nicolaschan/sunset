//! wasm-bindgen surface for the voice pipeline (C2a).
//!
//! Three methods land on `Client`: `voice_start`, `voice_stop`,
//! `voice_input`. JS pushes mono 48 kHz PCM in via `voice_input`; Rust
//! encodes, in C2a routes through an in-process loopback queue, decodes,
//! and calls a registered JS handler with each decoded frame's PCM.
//!
//! In C2b the loopback queue is replaced by `Bus::publish_ephemeral`
//! (capture side) and `Bus::subscribe` (playback side). The wasm-bindgen
//! API surface here does not change.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Float32Array, Function};
use tokio::sync::mpsc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use sunset_voice::{FRAME_SAMPLES, VoiceDecoder, VoiceEncoder};

/// Per-`Client` voice runtime state. `None` until `voice_start` is
/// called; cleared on `voice_stop`.
pub(crate) struct VoiceState {
    encoder: VoiceEncoder,
    /// Capture side: encoded bytes go in here.
    loopback_tx: mpsc::UnboundedSender<Vec<u8>>,
}

/// Inner data shared across `Client` for voice. Wrapped in
/// `Rc<RefCell<…>>` like the rest of `Client`'s mutable state.
pub(crate) type VoiceCell = Rc<RefCell<Option<VoiceState>>>;

pub(crate) fn new_voice_cell() -> VoiceCell {
    Rc::new(RefCell::new(None))
}

/// Start the voice subsystem. Spawns the loopback decode loop. The
/// `output_handler` is a JS callback invoked with a `Float32Array(960)`
/// for each decoded frame.
pub(crate) fn voice_start(state: &VoiceCell, output_handler: Function) -> Result<(), JsError> {
    let mut slot = state.borrow_mut();
    if slot.is_some() {
        return Err(JsError::new("voice already started"));
    }
    let encoder = VoiceEncoder::new().map_err(|e| JsError::new(&format!("{e}")))?;
    let mut decoder = VoiceDecoder::new().map_err(|e| JsError::new(&format!("{e}")))?;
    let (loopback_tx, mut loopback_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Spawn the loopback decode loop. In C2b this is replaced by a
    // Bus subscribe loop, but the shape (decode + call JS handler) is
    // identical.
    spawn_local(async move {
        while let Some(bytes) = loopback_rx.recv().await {
            match decoder.decode(&bytes) {
                Ok(pcm) => {
                    let arr = Float32Array::from(pcm.as_slice());
                    // Ignore handler errors — JS-side issues shouldn't
                    // tear down the decoder.
                    let _ = output_handler.call1(&JsValue::NULL, &arr);
                }
                Err(_) => {
                    // Single-frame loss — log via console.warn and
                    // continue. C2c may add metrics here.
                    web_sys::console::warn_1(
                        &"sunset-voice: decode failed for one frame; dropped".into(),
                    );
                }
            }
        }
    });

    *slot = Some(VoiceState {
        encoder,
        loopback_tx,
    });
    Ok(())
}

/// Stop the voice subsystem. Drops the encoder + loopback sender; the
/// decode loop exits when it next sees an empty channel.
pub(crate) fn voice_stop(state: &VoiceCell) -> Result<(), JsError> {
    *state.borrow_mut() = None;
    Ok(())
}

/// Submit one 20 ms frame of PCM. Length must be exactly 960.
pub(crate) fn voice_input(state: &VoiceCell, pcm: &Float32Array) -> Result<(), JsError> {
    let mut slot = state.borrow_mut();
    let voice = slot
        .as_mut()
        .ok_or_else(|| JsError::new("voice not started"))?;
    let len = pcm.length() as usize;
    if len != FRAME_SAMPLES {
        return Err(JsError::new(&format!(
            "voice_input expected {FRAME_SAMPLES} samples, got {len}"
        )));
    }
    let mut buf = vec![0.0_f32; FRAME_SAMPLES];
    pcm.copy_to(&mut buf);
    let encoded = voice
        .encoder
        .encode(&buf)
        .map_err(|e| JsError::new(&format!("{e}")))?;
    voice
        .loopback_tx
        .send(encoded)
        .map_err(|_| JsError::new("loopback channel closed"))?;
    Ok(())
}
