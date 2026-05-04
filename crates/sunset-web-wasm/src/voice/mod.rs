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

use sunset_core::bus::BusImpl;
use sunset_store_memory::MemoryStore;
use sunset_sync::MultiTransport;
use sunset_voice::runtime::{DynBus, FrameSink, VoiceRuntime};

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

/// Start the voice subsystem. Constructs `WebDialer`, `WebFrameSink`,
/// `WebPeerStateSink`, builds `VoiceRuntime`, and spawns all five
/// runtime tasks via `wasm_bindgen_futures::spawn_local`.
pub(crate) fn voice_start(
    cell: &VoiceCell,
    identity: &sunset_core::Identity,
    room_handle: &RoomHandle,
    bus: &BusArc,
    on_pcm: Function,
    on_drop_peer: Function,
    on_voice_peer_state: Function,
) -> Result<(), JsError> {
    if cell.borrow().is_some() {
        return Err(JsError::new("voice already started"));
    }

    let on_pcm_rc = Rc::new(RefCell::new(Some(on_pcm)));
    let on_drop_rc = Rc::new(RefCell::new(Some(on_drop_peer)));
    let on_state_rc = Rc::new(RefCell::new(Some(on_voice_peer_state)));

    let web_frame_sink: Rc<dyn FrameSink> = Rc::new(frame_sink::WebFrameSink {
        on_pcm: on_pcm_rc,
        on_drop: on_drop_rc,
    });
    let dialer: Rc<dyn sunset_voice::Dialer> = Rc::new(dialer::WebDialer {
        open_room: room_handle.open_room_rc(),
    });
    let peer_state_sink: Rc<dyn sunset_voice::PeerStateSink> =
        Rc::new(peer_state_sink::WebPeerStateSink {
            handler: on_state_rc,
        });

    // Upcast the Rc<BusImpl> to Rc<dyn DynBus>. Single-threaded data plane.
    let dyn_bus: Rc<dyn DynBus> = bus.clone();

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
    wasm_bindgen_futures::spawn_local(tasks.jitter_pump);
    wasm_bindgen_futures::spawn_local(tasks.voice_presence_publisher);

    *cell.borrow_mut() = Some(ActiveVoice {
        runtime,
        #[cfg(feature = "test-hooks")]
        frame_sink_rc: web_frame_sink,
        #[cfg(feature = "test-hooks")]
        recorder: RefCell::new(None),
    });

    Ok(())
}

pub(crate) fn voice_stop(cell: &VoiceCell) -> Result<(), JsError> {
    // Dropping `ActiveVoice` drops `VoiceRuntime`, which cancels all tasks.
    *cell.borrow_mut() = None;
    Ok(())
}

pub(crate) fn voice_input(cell: &VoiceCell, pcm: &Float32Array) -> Result<(), JsError> {
    let slot = cell.borrow();
    let v = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    if pcm.length() as usize != sunset_voice::FRAME_SAMPLES {
        return Err(JsError::new(&format!(
            "voice_input: expected {} samples, got {}",
            sunset_voice::FRAME_SAMPLES,
            pcm.length()
        )));
    }
    let mut buf = vec![0.0_f32; sunset_voice::FRAME_SAMPLES];
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
            &JsValue::from_str("seq_in_frame"),
            &JsValue::from_f64(frame.seq_in_frame as f64),
        )
        .unwrap();
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
pub(crate) fn jitter_depths(cell: &VoiceCell) -> Result<JsValue, JsError> {
    use js_sys::{Array, Object, Uint8Array};
    let slot = cell.borrow();
    let v = slot
        .as_ref()
        .ok_or_else(|| JsError::new("voice not started"))?;
    let arr = Array::new();
    for (peer, depth) in v.runtime.jitter_depths() {
        let obj = Object::new();
        let id = Uint8Array::from(peer.0.as_bytes());
        js_sys::Reflect::set(&obj, &JsValue::from_str("peer_id"), &id).unwrap();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("depth"),
            &JsValue::from_f64(depth as f64),
        )
        .unwrap();
        arr.push(&obj);
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
