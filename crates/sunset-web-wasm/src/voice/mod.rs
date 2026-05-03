//! Voice runtime — orchestrates encoder, network publish, and subscribe.
//!
//! `voice_start(on_frame, on_voice_peer_state)` constructs a VoiceState,
//! spawns the heartbeat timer (transport.rs), the subscribe loop
//! (subscriber.rs), and the Liveness state combiner (liveness.rs).
//! `voice_input(pcm)` encodes one frame and publishes the encrypted
//! bytes via `Bus::publish_ephemeral`.
//!
//! Splitting into submodules keeps each file focused on one responsibility.

mod liveness;
mod subscriber;
mod transport;

use std::cell::RefCell;
use std::rc::Rc;

use bytes::Bytes;
use js_sys::{Float32Array, Function};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use wasm_bindgen::prelude::*;

use sunset_core::bus::Bus;
use sunset_core::{Identity, Room};
use sunset_voice::{FRAME_SAMPLES, VoiceEncoder};

pub(crate) use transport::{BusArc, spawn_heartbeat};

/// Per-`Client` voice runtime state. `None` until `voice_start` is
/// called; cleared on `voice_stop` (Drop on inner Rc cancels everything).
pub(crate) struct VoiceState {
    encoder: VoiceEncoder,
    /// Monotonic frame sequence; incremented per voice_input call.
    seq: u64,
    /// Identity to sign Bus publishes (cloned from Client).
    identity: Identity,
    /// Room used to derive the voice key + AAD.
    room: Rc<Room>,
    /// Bus handle (publishes encrypted VoicePackets).
    bus: BusArc,
    /// Per-process RNG for nonces. ChaCha20Rng implements CryptoRngCore
    /// and is wasm-friendly (no OsRng dependency at construction time).
    rng: ChaCha20Rng,
    /// Liveness arcs held here so that `voice_stop` (which clears the
    /// cell) drops the outside strong refs the combiner doesn't own.
    /// The underscore prefix marks this as held-for-Drop only — the
    /// combiner reads from its own `Arc<Liveness>` clones and exits on
    /// the next event after `state.borrow().is_none()` becomes true.
    _liveness: liveness::VoiceLiveness,
}

pub(crate) type VoiceCell = Rc<RefCell<Option<VoiceState>>>;

pub(crate) fn new_voice_cell() -> VoiceCell {
    Rc::new(RefCell::new(None))
}

/// Start the voice subsystem. Constructs the encoder, spawns the
/// heartbeat task, the Bus subscribe loop, and the Liveness state
/// combiner.
pub(crate) fn voice_start(
    state: &VoiceCell,
    identity: &Identity,
    room: &Rc<Room>,
    bus: &BusArc,
    on_frame: &Function,
    on_voice_peer_state: &Function,
) -> Result<(), JsError> {
    if state.borrow().is_some() {
        return Err(JsError::new("voice already started"));
    }

    let encoder = VoiceEncoder::new().map_err(|e| JsError::new(&format!("encoder: {e}")))?;

    let now_nanos = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let rng = ChaCha20Rng::seed_from_u64(now_nanos);

    let arcs = liveness::VoiceLiveness::new();

    *state.borrow_mut() = Some(VoiceState {
        encoder,
        seq: 0,
        identity: identity.clone(),
        room: room.clone(),
        bus: bus.clone(),
        rng,
        _liveness: arcs.clone(),
    });

    spawn_heartbeat(state.clone(), identity.clone(), room.clone(), bus.clone());

    liveness::spawn_combiner(state.clone(), &arcs, on_voice_peer_state.clone());
    subscriber::spawn_subscriber(
        state.clone(),
        room.clone(),
        bus.clone(),
        arcs,
        on_frame.clone(),
        identity.store_verifying_key(),
    );

    Ok(())
}

pub(crate) fn voice_stop(state: &VoiceCell) -> Result<(), JsError> {
    *state.borrow_mut() = None;
    Ok(())
}

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
        .map_err(|e| JsError::new(&format!("encode: {e}")))?;

    let now_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let packet = sunset_voice::packet::VoicePacket::Frame {
        codec_id: sunset_voice::CODEC_ID.to_string(),
        seq: voice.seq,
        sender_time_ms: now_ms,
        payload: encoded,
    };
    voice.seq = voice.seq.saturating_add(1);

    let ev = sunset_voice::packet::encrypt(
        &voice.room,
        0,
        &voice.identity.public(),
        &packet,
        &mut voice.rng,
    )
    .map_err(|e| JsError::new(&format!("encrypt: {e}")))?;
    let payload_bytes =
        postcard::to_stdvec(&ev).map_err(|e| JsError::new(&format!("postcard encode: {e}")))?;

    let room_fp_hex = voice.room.fingerprint().to_hex();
    let sender_pk_hex = hex::encode(voice.identity.store_verifying_key().as_bytes());
    let name = Bytes::from(format!("voice/{room_fp_hex}/{sender_pk_hex}"));

    let bus = voice.bus.clone();
    let payload = Bytes::from(payload_bytes);
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = bus.publish_ephemeral(name, payload).await {
            tracing::warn!(error = %e, "voice_input publish_ephemeral failed");
        }
    });

    Ok(())
}
