//! Voice subsystem — thin browser shell over `VoiceRuntime`.
//!
//! This module only contains browser-specific glue: JS callback bridging,
//! per-peer GainNode volume forwarding, and WASM-bindgen marshalling.
//! All protocol logic lives in `sunset_voice::VoiceRuntime`.

mod dialer;
mod frame_sink;
mod peer_state_sink;
#[cfg(feature = "test-hooks")]
pub(crate) mod test_hooks;

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Float32Array, Function};
use wasm_bindgen::prelude::*;

use sunset_core::bus::{Bus, BusImpl};
use sunset_store_memory::MemoryStore;
use sunset_sync::MultiTransport;
use sunset_voice::runtime::{FrameSink, VoiceRuntime};

use crate::client::{RtcT, WsT};
use crate::room_handle::RoomHandle;

pub(crate) type BusT = BusImpl<MemoryStore, MultiTransport<WsT, RtcT>>;
pub(crate) type BusArc = Rc<BusT>;

pub(crate) struct ActiveVoice {
    runtime: VoiceRuntime,
    /// Shared with `WebFrameSink` so `install_recorder` can find the
    /// original sink to wrap. Also keeps the sink alive alongside the
    /// runtime (belt-and-suspenders, since the runtime holds an Rc too).
    #[cfg(feature = "test-hooks")]
    frame_sink_rc: Rc<dyn FrameSink>,
    #[cfg(feature = "test-hooks")]
    recorder: RefCell<Option<Rc<test_hooks::RecordingFrameSink>>>,
}

pub(crate) type VoiceCell = Rc<RefCell<Option<ActiveVoice>>>;

pub(crate) fn new_voice_cell() -> VoiceCell {
    Rc::new(RefCell::new(None))
}

/// The host (JS) callbacks a voice session delivers into: decoded PCM,
/// per-peer drop, and `VoicePeerState` changes. Bundled so the start
/// functions stay below the argument-count lint.
pub(crate) struct VoiceCallbacks {
    pub on_pcm: Function,
    pub on_drop_peer: Function,
    pub on_voice_peer_state: Function,
}

/// Start the voice subsystem in *observer* mode. Constructs `WebDialer`,
/// `WebFrameSink`, `WebPeerStateSink`, builds `VoiceRuntime`, and spawns
/// all six runtime tasks. The runtime starts inactive: the three
/// observer-side tasks (durable-presence subscription, combiner, voice
/// subscribe) run normally so the UI learns who is in the channel, but
/// the three active tasks (heartbeat, presence publisher, auto-connect)
/// short-circuit until `voice_activate` flips the gate.
///
/// Use this at room load: the call's roster is visible from the moment
/// the user lands in the room, without requiring mic permission or
/// emitting any outbound voice traffic. Pair with `voice_activate` when
/// the user joins the call, `voice_deactivate` when they leave, and
/// `voice_stop` when they leave the room.
pub(crate) fn voice_observe_start(
    cell: &VoiceCell,
    identity: &sunset_core::Identity,
    room_handle: &RoomHandle,
    bus: &BusArc,
    relay_only: bool,
    callbacks: VoiceCallbacks,
) -> Result<(), JsError> {
    if cell.borrow().is_some() {
        return Err(JsError::new("voice already started"));
    }

    let on_pcm_rc = Rc::new(RefCell::new(Some(callbacks.on_pcm)));
    let on_drop_rc = Rc::new(RefCell::new(Some(callbacks.on_drop_peer)));
    let on_state_rc = Rc::new(RefCell::new(Some(callbacks.on_voice_peer_state)));

    let web_frame_sink: Rc<dyn FrameSink> = Rc::new(frame_sink::WebFrameSink {
        on_pcm: on_pcm_rc,
        on_drop: on_drop_rc,
    });
    let dialer: Rc<dyn sunset_voice::Dialer> = Rc::new(dialer::WebDialer {
        open_room: room_handle.open_room_rc(),
        intent_ids: RefCell::new(Default::default()),
        relay_only,
    });
    let peer_state_sink: Rc<dyn sunset_voice::PeerStateSink> =
        Rc::new(peer_state_sink::WebPeerStateSink {
            handler: on_state_rc,
        });

    // Upcast the Rc<BusImpl> to Rc<dyn Bus>. Single-threaded data plane.
    let dyn_bus: Rc<dyn Bus> = bus.clone();

    let (runtime, tasks) = VoiceRuntime::new(
        dyn_bus,
        room_handle.room_rc(),
        identity.clone(),
        dialer,
        web_frame_sink.clone(),
        peer_state_sink,
    );

    wasm_bindgen_futures::spawn_local(tasks.heartbeat);
    wasm_bindgen_futures::spawn_local(tasks.subscribe);
    wasm_bindgen_futures::spawn_local(tasks.combiner);
    wasm_bindgen_futures::spawn_local(tasks.auto_connect);
    wasm_bindgen_futures::spawn_local(tasks.voice_provider);
    wasm_bindgen_futures::spawn_local(tasks.voice_presence_publisher);
    wasm_bindgen_futures::spawn_local(tasks.voice_presence_membership);

    *cell.borrow_mut() = Some(ActiveVoice {
        runtime,
        #[cfg(feature = "test-hooks")]
        frame_sink_rc: web_frame_sink,
        #[cfg(feature = "test-hooks")]
        recorder: RefCell::new(None),
    });

    Ok(())
}

/// Transition the runtime from observer to active. Requires
/// `voice_observe_start` to have been called first; the JS side is
/// expected to have already brought up mic capture (`startCapture`)
/// before invoking. Idempotent if already active.
pub(crate) fn voice_activate(cell: &VoiceCell) -> Result<(), JsError> {
    let slot = cell.borrow();
    let av = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    av.runtime.set_active(true);
    Ok(())
}

/// Transition the runtime from active back to observer. Stops
/// heartbeats and presence publishing; the durable-presence
/// subscription continues so the roster stays populated. Idempotent
/// if already in observer mode.
pub(crate) fn voice_deactivate(cell: &VoiceCell) -> Result<(), JsError> {
    let slot = cell.borrow();
    let av = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    av.runtime.set_active(false);
    Ok(())
}

/// One-shot start that does observer-start + activate in a single call.
/// Preserved for tests and any caller that expresses "I'm in the room
/// AND in the call" in a single step.
pub(crate) fn voice_start(
    cell: &VoiceCell,
    identity: &sunset_core::Identity,
    room_handle: &RoomHandle,
    bus: &BusArc,
    relay_only: bool,
    callbacks: VoiceCallbacks,
) -> Result<(), JsError> {
    voice_observe_start(cell, identity, room_handle, bus, relay_only, callbacks)?;
    voice_activate(cell)
}

pub(crate) fn voice_stop(cell: &VoiceCell) -> Result<(), JsError> {
    // Dropping `ActiveVoice` drops `VoiceRuntime`, which cancels all tasks.
    *cell.borrow_mut() = None;
    Ok(())
}

/// Number of samples the JS-side capture worklet hands us per
/// frame: `FRAME_SAMPLES_PER_CHANNEL × 2` interleaved L/R. We
/// always capture stereo from JS regardless of the active quality
/// preset; the runtime downmixes to mono when the preset selects
/// `OPUS_APPLICATION_VOIP`.
const STEREO_SAMPLES_PER_FRAME: usize = sunset_voice::FRAME_SAMPLES_PER_CHANNEL * 2;

pub(crate) fn voice_input(cell: &VoiceCell, pcm: &Float32Array) -> Result<(), JsError> {
    let slot = cell.borrow();
    let v = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    if pcm.length() as usize != STEREO_SAMPLES_PER_FRAME {
        return Err(JsError::new(&format!(
            "voice_input: expected {} samples (stereo interleaved), got {}",
            STEREO_SAMPLES_PER_FRAME,
            pcm.length()
        )));
    }
    let mut buf = vec![0.0_f32; STEREO_SAMPLES_PER_FRAME];
    pcm.copy_to(&mut buf);
    v.runtime.send_pcm(&buf);
    Ok(())
}

pub(crate) fn voice_set_muted(cell: &VoiceCell, muted: bool) {
    if let Some(v) = cell.borrow().as_ref() {
        v.runtime.set_muted(muted);
    }
}

pub(crate) fn voice_set_deafened(cell: &VoiceCell, deafened: bool) {
    if let Some(v) = cell.borrow().as_ref() {
        v.runtime.set_deafened(deafened);
    }
}

pub(crate) fn voice_set_peer_denoise(
    cell: &VoiceCell,
    peer_bytes: &[u8],
    enabled: bool,
) -> Result<(), JsError> {
    if peer_bytes.len() != 32 {
        return Err(JsError::new("peer_id must be 32 bytes"));
    }
    let pk = sunset_store::VerifyingKey::new(bytes::Bytes::copy_from_slice(peer_bytes));
    let peer = sunset_sync::PeerId(pk);
    if let Some(v) = cell.borrow().as_ref() {
        v.runtime.set_peer_denoise(peer, enabled);
    }
    Ok(())
}

/// Switch the active send-side voice quality preset. Accepts
/// `"voice"`, `"high"`, `"maximum"` (case-sensitive). Returns an
/// error if voice isn't started or the label is unknown.
pub(crate) fn voice_set_quality(cell: &VoiceCell, label: &str) -> Result<(), JsError> {
    let quality = sunset_voice::VoiceQuality::from_str_label(label)
        .ok_or_else(|| JsError::new(&format!("unknown voice quality: {label}")))?;
    let slot = cell.borrow();
    let v = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    v.runtime
        .set_quality(quality)
        .map_err(|e| JsError::new(&format!("set_quality failed: {e}")))
}

/// Read back the active send-side voice quality preset (label).
pub(crate) fn voice_quality(cell: &VoiceCell) -> Option<&'static str> {
    cell.borrow().as_ref().map(|v| v.runtime.quality().as_str())
}

// ---------- Test-hooks helpers (compiled in only with feature "test-hooks") ----------

#[cfg(feature = "test-hooks")]
pub(crate) fn install_recorder(cell: &VoiceCell) -> Result<(), JsError> {
    let slot = cell.borrow();
    let v = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;

    let recorder = Rc::new(test_hooks::RecordingFrameSink::new(v.frame_sink_rc.clone()));
    *v.recorder.borrow_mut() = Some(recorder.clone());

    // Upcast to Rc<dyn FrameSink> so the runtime's RefCell<Rc<dyn FrameSink>>
    // can accept it.
    let as_dyn: Rc<dyn sunset_voice::FrameSink> = recorder;
    v.runtime.set_frame_sink(as_dyn);

    Ok(())
}

#[cfg(feature = "test-hooks")]
pub(crate) fn recorded_frames(cell: &VoiceCell, peer_bytes: &[u8]) -> Result<JsValue, JsError> {
    use js_sys::{Array, Object};
    let slot = cell.borrow();
    let v = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    let recorder = v.recorder.borrow().as_ref().cloned().ok_or_else(|| {
        JsError::new("frame recorder not installed; call voice_install_frame_recorder first")
    })?;

    if peer_bytes.len() != 32 {
        return Err(JsError::new("peer_id must be 32 bytes"));
    }
    let pk = sunset_store::VerifyingKey::new(bytes::Bytes::copy_from_slice(peer_bytes));
    let peer = sunset_sync::PeerId(pk);

    let frames = recorder.get_frames(&peer);
    let arr = Array::new();
    for frame in &frames {
        let obj = Object::new();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("len"),
            &JsValue::from_f64(frame.len as f64),
        )
        .unwrap();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("checksum"),
            &JsValue::from_str(&frame.checksum),
        )
        .unwrap();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("rms"),
            &JsValue::from_f64(frame.rms as f64),
        )
        .unwrap();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("seq"),
            &JsValue::from_f64(frame.seq as f64),
        )
        .unwrap();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("tone_purity_440"),
            &JsValue::from_f64(frame.tone_purity_440 as f64),
        )
        .unwrap();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("via"),
            &JsValue::from_str(frame.via.as_str()),
        )
        .unwrap();
        arr.push(&obj);
    }
    Ok(arr.into())
}

#[cfg(feature = "test-hooks")]
pub(crate) fn auto_connect_peers(cell: &VoiceCell) -> Result<JsValue, JsError> {
    use js_sys::{Array, Uint8Array};
    let slot = cell.borrow();
    let v = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    let arr = Array::new();
    for peer in v.runtime.auto_connect_peers() {
        arr.push(&Uint8Array::from(peer.0.as_bytes()));
    }
    Ok(arr.into())
}

#[cfg(feature = "test-hooks")]
pub(crate) fn observed_voice_peers(cell: &VoiceCell) -> Result<JsValue, JsError> {
    use js_sys::{Array, Uint8Array};
    let slot = cell.borrow();
    let v = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    let arr = Array::new();
    for peer in v.runtime.observed_voice_peers() {
        arr.push(&Uint8Array::from(peer.0.as_bytes()));
    }
    Ok(arr.into())
}

#[cfg(feature = "test-hooks")]
pub(crate) fn active_peers(cell: &VoiceCell) -> Result<JsValue, JsError> {
    use js_sys::{Array, Object, Uint8Array};
    let slot = cell.borrow();
    let v = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    let states = v.runtime.snapshot_states();
    let arr = Array::new();
    for state in &states {
        let obj = Object::new();
        let id = Uint8Array::from(state.peer.0.as_bytes());
        js_sys::Reflect::set(&obj, &JsValue::from_str("peer_id"), &id).unwrap();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("in_call"),
            &JsValue::from_bool(state.in_call),
        )
        .unwrap();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("talking"),
            &JsValue::from_bool(state.talking),
        )
        .unwrap();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("is_muted"),
            &JsValue::from_bool(state.is_muted),
        )
        .unwrap();
        arr.push(&obj);
    }
    Ok(arr.into())
}
