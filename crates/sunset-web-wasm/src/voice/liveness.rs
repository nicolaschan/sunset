//! Two `Liveness` arcs (frame + membership) and a state-combiner task
//! that emits `(peer, in_call, talking)` to the JS callback.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use js_sys::Function;
use wasm_bindgen::prelude::*;

use sunset_core::liveness::{Liveness, LivenessState};
use sunset_sync::PeerId;

use super::VoiceCell;

pub(crate) const FRAME_STALE_AFTER: Duration = Duration::from_millis(1000);
pub(crate) const MEMBERSHIP_STALE_AFTER: Duration = Duration::from_secs(5);

/// Pair of `Liveness` arcs (one for actively-talking, one for in-call
/// membership). Cheap to clone — the underlying `Arc<Liveness>` shares
/// state across all clones, so observers and the combiner both see the
/// same observations.
#[derive(Clone)]
pub(crate) struct VoiceLiveness {
    pub frame: Arc<Liveness>,
    pub membership: Arc<Liveness>,
}

impl VoiceLiveness {
    pub fn new() -> Self {
        Self {
            frame: Liveness::new(FRAME_STALE_AFTER),
            membership: Liveness::new(MEMBERSHIP_STALE_AFTER),
        }
    }
}

/// Spawn the state combiner. Listens to both Liveness streams and emits
/// `(peer_id_uint8array, in_call, talking)` whenever the combined state
/// for any peer changes.
///
/// **Exit timing:** the combiner notices `voice_stop` only on the *next*
/// Liveness event after the cell is cleared. With the 5-second
/// membership stale window and 2-second heartbeat cadence (transport.rs)
/// the combiner exits within a few seconds; if the cell is cleared
/// while no peer is sending or heart-beating, the combiner can linger
/// up to one membership-stale interval before its next event arrives
/// and lets it exit. The Liveness arcs are dropped at that point so no
/// memory leak.
///
/// The `else => break` arm only fires if both Liveness mpsc channels
/// close, which only happens when this task drops its own `Arc<Liveness>`
/// clones — practically unreachable.
pub(crate) fn spawn_combiner(
    state: VoiceCell,
    arcs: &VoiceLiveness,
    on_voice_peer_state: Function,
) {
    let arcs = arcs.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let mut frame_sub = arcs.frame.subscribe().await;
        let mut membership_sub = arcs.membership.subscribe().await;
        let mut frame_state: HashMap<PeerId, bool> = HashMap::new();
        let mut membership_state: HashMap<PeerId, bool> = HashMap::new();
        let mut last_emitted: HashMap<PeerId, (bool, bool)> = HashMap::new();

        loop {
            tokio::select! {
                Some(ev) = frame_sub.next() => {
                    if state.borrow().is_none() { return; }
                    let alive = ev.state == LivenessState::Live;
                    frame_state.insert(ev.peer.clone(), alive);
                    emit_if_changed(
                        &on_voice_peer_state,
                        &ev.peer,
                        &frame_state,
                        &membership_state,
                        &mut last_emitted,
                    );
                }
                Some(ev) = membership_sub.next() => {
                    if state.borrow().is_none() { return; }
                    let alive = ev.state == LivenessState::Live;
                    membership_state.insert(ev.peer.clone(), alive);
                    emit_if_changed(
                        &on_voice_peer_state,
                        &ev.peer,
                        &frame_state,
                        &membership_state,
                        &mut last_emitted,
                    );
                }
                else => break,
            }
        }
    });
}

fn emit_if_changed(
    handler: &Function,
    peer: &PeerId,
    frame_state: &HashMap<PeerId, bool>,
    membership_state: &HashMap<PeerId, bool>,
    last_emitted: &mut HashMap<PeerId, (bool, bool)>,
) {
    let talking = *frame_state.get(peer).unwrap_or(&false);
    let in_call = talking || *membership_state.get(peer).unwrap_or(&false);
    let prev = last_emitted.get(peer).copied();
    if prev != Some((in_call, talking)) {
        last_emitted.insert(peer.clone(), (in_call, talking));
        let id_arr = js_sys::Uint8Array::from(peer.0.as_bytes());
        let _ = handler.call3(
            &JsValue::NULL,
            &id_arr,
            &JsValue::from_bool(in_call),
            &JsValue::from_bool(talking),
        );
    }
}
