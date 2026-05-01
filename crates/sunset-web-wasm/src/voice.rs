//! Voice pipeline plumbing — wasm-bindgen surface for `Client`.
//!
//! Three `Client` methods drive everything: `voice_start(handler)`
//! constructs the encoder + decoder, spawns the loopback decode loop,
//! and registers a JS callback for decoded PCM frames;
//! `voice_input(pcm)` submits a 20 ms `Float32Array` to the encoder.
//! Encoded bytes flow through an internal loopback channel into the
//! decoder; decoded PCM flows out through the registered handler.
//!
//! The actual encoding / decoding lives in `sunset-voice`. This file
//! is intentionally pure plumbing — it owns the `VoiceEncoder` /
//! `VoiceDecoder`, the loopback channel, and the JS handler, and
//! shuttles data between them. Swapping the codec to a real
//! implementation (Opus / WebCodecs / pure-Rust) is a contained
//! change inside `sunset-voice`; this file does not change.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Float32Array, Function};
use tokio::sync::mpsc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use sunset_voice::{FRAME_SAMPLES, VoiceDecoder, VoiceEncoder};

/// Per-`Client` voice runtime state. `None` until `voice_start` is
/// called; cleared on `voice_stop` (which drops the encoder + the
/// loopback sender, letting the decode loop exit).
pub(crate) struct VoiceState {
    encoder: VoiceEncoder,
    /// Encoder side pushes encoded bytes into this channel; the
    /// loopback decode loop pulls from it.
    loopback_tx: mpsc::UnboundedSender<Vec<u8>>,
}

pub(crate) type VoiceCell = Rc<RefCell<Option<VoiceState>>>;

pub(crate) fn new_voice_cell() -> VoiceCell {
    Rc::new(RefCell::new(None))
}

/// Initialise the voice subsystem. Spawns a background task that
/// drains the loopback channel, decodes each packet, and invokes
/// `output_handler` with a `Float32Array(FRAME_SAMPLES)` for each
/// decoded 20 ms frame.
pub(crate) fn voice_start(state: &VoiceCell, output_handler: &Function) -> Result<(), JsError> {
    if state.borrow().is_some() {
        return Err(JsError::new("voice already started"));
    }

    let encoder = VoiceEncoder::new().map_err(|e| JsError::new(&format!("encoder: {e}")))?;
    let mut decoder = VoiceDecoder::new().map_err(|e| JsError::new(&format!("decoder: {e}")))?;

    let (loopback_tx, mut loopback_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Background decode loop. Owns the decoder + the JS handler; the
    // encoder is held synchronously by `voice_input` on the main
    // thread. Loop exits when the sender drops (voice_stop or
    // VoiceState drop).
    let handler = output_handler.clone();
    spawn_local(async move {
        while let Some(bytes) = loopback_rx.recv().await {
            match decoder.decode(&bytes) {
                Ok(pcm) => {
                    let arr = Float32Array::from(pcm.as_slice());
                    // Ignore handler errors — JS-side issues should
                    // not tear down the decoder.
                    let _ = handler.call1(&JsValue::NULL, &arr);
                }
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("sunset-voice: decode failed for one frame: {e}").into(),
                    );
                }
            }
        }
    });

    *state.borrow_mut() = Some(VoiceState {
        encoder,
        loopback_tx,
    });
    Ok(())
}

/// Stop the voice subsystem. Drops the encoder + the loopback
/// sender; the decode loop exits when the channel closes.
pub(crate) fn voice_stop(state: &VoiceCell) -> Result<(), JsError> {
    *state.borrow_mut() = None;
    Ok(())
}

/// Submit one 20 ms frame of mono PCM (`FRAME_SAMPLES` samples at
/// `SAMPLE_RATE`) for encoding + loopback delivery to the registered
/// output handler.
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

    // Copy from the JS-side Float32Array into a Rust Vec, encode,
    // push onto the loopback channel.
    let mut buf = vec![0.0_f32; FRAME_SAMPLES];
    pcm.copy_to(&mut buf);
    let encoded = voice
        .encoder
        .encode(&buf)
        .map_err(|e| JsError::new(&format!("encode: {e}")))?;
    voice
        .loopback_tx
        .send(encoded)
        .map_err(|_| JsError::new("loopback channel closed"))?;
    Ok(())
}
