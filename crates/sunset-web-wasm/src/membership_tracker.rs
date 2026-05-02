//! Membership + relay-status tracker. One spawned task per Client.
//!
//! Three input streams:
//!   1. local store events on `<room_fp>/presence/` (heartbeats)
//!   2. engine event stream (PeerAdded / PeerRemoved with kind)
//!   3. periodic refresh tick (catches Online↔Away threshold crossings
//!      between heartbeats)
//!
//! On every update, re-derives the member list + relay status and
//! fires the corresponding JS callback if the value changed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use bytes::Bytes;
use futures::channel::mpsc as fmpsc;
use futures::{FutureExt, StreamExt};
use js_sys::Array;
use wasm_bindgen::prelude::*;
use wasmtimer::tokio::sleep;

use sunset_store::{Filter, Replay, Store};
use sunset_store_memory::MemoryStore;
use sunset_sync::{EngineEvent, PeerId, TransportKind};

use crate::members::{derive_members, members_signature};

/// Stable per-member signature row used for debounce: (pubkey, presence, connection_mode).
type MemberSig = Vec<(Vec<u8>, String, String)>;

#[derive(Clone)]
pub struct TrackerHandles {
    pub on_members: Rc<RefCell<Option<js_sys::Function>>>,
    pub on_relay_status: Rc<RefCell<Option<js_sys::Function>>>,
    pub last_relay_status: Rc<RefCell<String>>,
    pub peer_kinds: Rc<RefCell<HashMap<PeerId, TransportKind>>>,
    /// Per-member signature of the most recent fire. Lives on the
    /// handle (not as a tracker-task local) so `on_members_changed`
    /// can clear it on (re-)registration: the next `maybe_fire` then
    /// sees signature ≠ stored signature and fires the callback with
    /// the current state. Without this, a callback registered after
    /// the system has stabilized would never see anything until the
    /// next presence transition (which can be never if heartbeats
    /// land before the refresh tick crosses `interval_ms`).
    pub last_signature: Rc<RefCell<MemberSig>>,
}

impl TrackerHandles {
    pub fn new(initial_relay_status: &str) -> Self {
        Self {
            on_members: Rc::new(RefCell::new(None)),
            on_relay_status: Rc::new(RefCell::new(None)),
            last_relay_status: Rc::new(RefCell::new(initial_relay_status.to_owned())),
            peer_kinds: Rc::new(RefCell::new(HashMap::new())),
            last_signature: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

/// Spawn the tracker. Runs forever (page lifetime).
#[allow(clippy::too_many_arguments)]
pub fn spawn_tracker(
    store: std::sync::Arc<MemoryStore>,
    mut engine_events: tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
    self_peer: PeerId,
    room_fp_hex: String,
    interval_ms: u64,
    ttl_ms: u64,
    refresh_ms: u64,
    handles: TrackerHandles,
) {
    // Periodic refresh ticker as a separate task. Pushes a unit into
    // `refresh_rx` every `refresh_ms`. The main select loop only
    // deals with channels — no inline `sleep().fuse()` pinning
    // gymnastics required.
    let (refresh_tx, refresh_rx) = fmpsc::unbounded::<()>();
    sunset_sync::spawn::spawn_local(async move {
        loop {
            sleep(Duration::from_millis(refresh_ms)).await;
            if refresh_tx.unbounded_send(()).is_err() {
                break;
            }
        }
    });

    sunset_sync::spawn::spawn_local(async move {
        let presence_filter = Filter::NamePrefix(Bytes::from(format!("{room_fp_hex}/presence/")));
        let presence_sub = match store.subscribe(presence_filter, Replay::All).await {
            Ok(s) => s,
            Err(e) => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "MembershipTracker: presence subscribe failed: {e}"
                )));
                return;
            }
        };
        let mut presence_sub = presence_sub.fuse();
        let mut refresh_rx = refresh_rx.fuse();
        let presence_map: Rc<RefCell<HashMap<PeerId, u64>>> = Rc::new(RefCell::new(HashMap::new()));
        let prefix = format!("{room_fp_hex}/presence/");

        loop {
            futures::select! {
                ev = presence_sub.next() => {
                    let Some(ev) = ev else { break };
                    let entry = match ev {
                        Ok(sunset_store::Event::Inserted(e)) => e,
                        Ok(sunset_store::Event::Replaced { new, .. }) => new,
                        Ok(_) => continue,
                        Err(e) => {
                            web_sys::console::warn_1(&JsValue::from_str(&format!(
                                "MembershipTracker presence event: {e}"
                            )));
                            continue;
                        }
                    };
                    let Some(pk) = parse_presence_pk(&entry.name, &prefix) else { continue };
                    presence_map.borrow_mut().insert(pk, entry.priority);
                    maybe_fire(
                        now_ms(),
                        interval_ms,
                        ttl_ms,
                        &self_peer,
                        &presence_map.borrow(),
                        &handles.peer_kinds.borrow(),
                        &handles.last_signature,
                        handles.on_members.borrow().as_ref(),
                    );
                }
                ev = recv_engine(&mut engine_events).fuse() => {
                    let Some(ev) = ev else { break };
                    handle_engine_event(&handles, &ev);
                    maybe_fire_relay_status(&handles);
                    maybe_fire(
                        now_ms(),
                        interval_ms,
                        ttl_ms,
                        &self_peer,
                        &presence_map.borrow(),
                        &handles.peer_kinds.borrow(),
                        &handles.last_signature,
                        handles.on_members.borrow().as_ref(),
                    );
                }
                _ = refresh_rx.next() => {
                    // Periodic re-derive (catches Online↔Away threshold crossings,
                    // and is the path that fires the callback for free shortly after
                    // `Client::on_members_changed` clears `last_signature`).
                    maybe_fire(
                        now_ms(),
                        interval_ms,
                        ttl_ms,
                        &self_peer,
                        &presence_map.borrow(),
                        &handles.peer_kinds.borrow(),
                        &handles.last_signature,
                        handles.on_members.borrow().as_ref(),
                    );
                }
            }
        }
    });
}

fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn recv_engine(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
) -> Option<EngineEvent> {
    rx.recv().await
}

fn parse_presence_pk(name: &[u8], prefix: &str) -> Option<PeerId> {
    let s = std::str::from_utf8(name).ok()?;
    let suffix = s.strip_prefix(prefix)?;
    let bytes = hex::decode(suffix).ok()?;
    Some(PeerId(sunset_store::VerifyingKey::new(Bytes::from(bytes))))
}

fn handle_engine_event(handles: &TrackerHandles, ev: &EngineEvent) {
    match ev {
        EngineEvent::PeerAdded { peer_id, kind } => {
            handles
                .peer_kinds
                .borrow_mut()
                .insert(peer_id.clone(), *kind);
        }
        EngineEvent::PeerRemoved { peer_id } => {
            handles.peer_kinds.borrow_mut().remove(peer_id);
        }
    }
}

fn derive_relay_status(peer_kinds: &HashMap<PeerId, TransportKind>, prior: &str) -> String {
    // Sticky "connecting"/"error" states are owned by the Client
    // (set at add_relay call time). We only flip between
    // "connected" and "disconnected" based on whether any Primary
    // connection exists.
    if prior == "connecting" || prior == "error" {
        // Don't override transient explicit states.
        return prior.to_owned();
    }
    if peer_kinds.values().any(|k| *k == TransportKind::Primary) {
        "connected".to_owned()
    } else {
        "disconnected".to_owned()
    }
}

/// Public re-evaluation entry point used by `Client::start_presence`
/// after seeding `peer_kinds` from the engine snapshot. Mirrors
/// `maybe_fire_relay_status` but exposed for one-shot kicks.
pub fn fire_relay_status_now(handles: &TrackerHandles) {
    maybe_fire_relay_status(handles);
}

fn maybe_fire_relay_status(handles: &TrackerHandles) {
    let prior = handles.last_relay_status.borrow().clone();
    let next = derive_relay_status(&handles.peer_kinds.borrow(), &prior);
    if next != prior {
        *handles.last_relay_status.borrow_mut() = next.clone();
        if let Some(cb) = handles.on_relay_status.borrow().as_ref() {
            let _ = cb.call1(&JsValue::NULL, &JsValue::from_str(&next));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn maybe_fire(
    now_ms: u64,
    interval_ms: u64,
    ttl_ms: u64,
    self_peer: &PeerId,
    presence_map: &HashMap<PeerId, u64>,
    peer_kinds: &HashMap<PeerId, TransportKind>,
    last_signature: &RefCell<MemberSig>,
    callback: Option<&js_sys::Function>,
) {
    let members = derive_members(
        now_ms,
        interval_ms,
        ttl_ms,
        self_peer,
        presence_map,
        peer_kinds,
    );
    let sig = members_signature(&members);
    if sig == *last_signature.borrow() {
        return;
    }
    *last_signature.borrow_mut() = sig;
    let Some(cb) = callback else { return };
    let arr = Array::new();
    for m in members {
        arr.push(&JsValue::from(m));
    }
    let _ = cb.call1(&JsValue::NULL, &arr);
}
