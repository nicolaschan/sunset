//! JS-exported Client: identity + sync engine wired together.
//! Per-room operations (send_message, on_message, on_receipt, presence,
//! reactions, members, etc.) live in `RoomHandle`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use wasm_bindgen::prelude::*;
use zeroize::Zeroizing;

use sunset_core::{Ed25519Verifier, Identity};
use sunset_noise::{NoiseIdentity, NoiseTransport};
use sunset_store_memory::MemoryStore;
use sunset_sync::{MultiTransport, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_sync_webrtc_browser::WebRtcRawTransport;
use sunset_sync_ws_browser::WebSocketRawTransport;

use crate::identity::identity_from_seed;

pub(crate) type WsT = NoiseTransport<WebSocketRawTransport>;
pub(crate) type RtcT = NoiseTransport<WebRtcRawTransport>;

/// Adapter so sunset-core's `Identity` works as a NoiseIdentity.
struct IdentityNoiseAdapter(Identity);

impl NoiseIdentity for IdentityNoiseAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

#[wasm_bindgen]
pub struct Client {
    /// Multi-room peer (identity + store + engine + supervisor +
    /// per-room registry). Per-room ops route through `RoomHandle`.
    inner: Rc<sunset_core::Peer<MemoryStore, MultiTransport<WsT, RtcT>>>,
    /// Local copies kept on the Client because (a) the voice subsystem
    /// needs identity to start a per-room voice session, and (b) the
    /// supervisor-state forwarder + peer_connection_snapshot need the
    /// supervisor handle. Identity/supervisor could move into `Peer`
    /// accessors later; for now the duplication is small and explicit.
    identity: Identity,
    supervisor: Rc<sunset_sync::PeerSupervisor<MemoryStore, MultiTransport<WsT, RtcT>>>,
    /// Voice subsystem (single concurrent call per Client). See
    /// `voice_start` for the room-handle parameter that selects which
    /// room the call targets.
    voice: crate::voice::VoiceCell,
    bus: crate::voice::BusArc,
    /// JS callback for live peer-connection state changes. The
    /// supervisor forwarder task (spawned at construction) dispatches
    /// to whatever's installed here.
    on_peer_connection_state: Rc<RefCell<Option<js_sys::Function>>>,
}

#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8]) -> Result<Client, JsError> {
        let identity = identity_from_seed(seed).map_err(|e| JsError::new(&e))?;
        let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

        let ws_raw = WebSocketRawTransport::dial_only();
        let ws_noise =
            NoiseTransport::new(ws_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));

        let dispatcher = sunset_core::MultiRoomSignaler::new();
        let dispatcher_dyn: Rc<dyn sunset_sync::Signaler> = dispatcher.clone();
        let local_peer = PeerId(identity.store_verifying_key());
        let rtc_raw = WebRtcRawTransport::new(
            dispatcher_dyn,
            local_peer.clone(),
            vec!["stun:stun.l.google.com:19302".into()],
        );
        let rtc_noise =
            NoiseTransport::new(rtc_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));

        let multi = MultiTransport::new(ws_noise, rtc_noise);
        let signer: Arc<dyn Signer> = Arc::new(identity.clone());
        let engine = Rc::new(SyncEngine::new(
            store.clone(),
            multi,
            SyncConfig::default(),
            local_peer,
            signer,
        ));
        let engine_clone = engine.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = engine_clone.run().await {
                tracing::error!(error = %e, "sync engine exited");
            }
        });

        let supervisor =
            sunset_sync::PeerSupervisor::new(engine.clone(), sunset_sync::BackoffPolicy::default());
        wasm_bindgen_futures::spawn_local({
            let s = supervisor.clone();
            async move { s.run().await }
        });

        let bus: crate::voice::BusArc = Rc::new(sunset_core::bus::BusImpl::new(
            store.clone(),
            engine.clone(),
            identity.clone(),
        ));

        let on_peer_connection_state = Rc::new(RefCell::<Option<js_sys::Function>>::new(None));
        spawn_peer_connection_state_forwarder(supervisor.clone(), on_peer_connection_state.clone());

        let peer = sunset_core::Peer::new(
            identity.clone(),
            store.clone(),
            engine.clone(),
            supervisor.clone(),
            dispatcher,
        );

        Ok(Client {
            inner: peer,
            identity,
            supervisor,
            voice: crate::voice::new_voice_cell(),
            bus,
            on_peer_connection_state,
        })
    }

    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    #[wasm_bindgen(getter)]
    pub fn relay_status(&self) -> String {
        self.inner.relay_status()
    }

    pub async fn add_relay(&self, url_with_fragment: String) -> Result<(), JsError> {
        let resolver = sunset_relay_resolver::Resolver::new(crate::resolver_adapter::WebSysFetch);
        let canonical = resolver
            .resolve(&url_with_fragment)
            .await
            .map_err(|e| JsError::new(&format!("add_relay resolve: {e}")))?;
        let addr = sunset_sync::PeerAddr::new(Bytes::from(canonical));
        self.inner
            .add_relay(addr)
            .await
            .map_err(|e| JsError::new(&format!("add_relay: {e}")))?;
        Ok(())
    }

    pub async fn open_room(&self, name: String) -> Result<crate::room_handle::RoomHandle, JsError> {
        let open = self
            .inner
            .open_room(&name)
            .await
            .map_err(|e| JsError::new(&format!("open_room: {e}")))?;
        Ok(crate::room_handle::RoomHandle::new(open))
    }

    /// Snapshot all current peer connection intents. Returns a JS array
    /// of objects: `{ addr, state, peer_id?, attempt }`.
    pub async fn peer_connection_snapshot(&self) -> Result<JsValue, JsError> {
        let snaps = self.supervisor.snapshot().await;
        let arr = js_sys::Array::new();
        for s in snaps {
            arr.push(&intent_snapshot_to_js(&s)?);
        }
        Ok(arr.into())
    }

    /// Register a callback for live peer connection state changes. The
    /// handler receives one object per transition with the same shape
    /// as `peer_connection_snapshot`'s elements.
    ///
    /// Replaces any previously-registered callback. The forwarder task
    /// is spawned once at `Client::new` and dispatches to whatever
    /// callback is currently installed; calling this method multiple
    /// times does not duplicate dispatches.
    pub fn on_peer_connection_state(&self, handler: js_sys::Function) {
        *self.on_peer_connection_state.borrow_mut() = Some(handler);
    }

    /// Start voice in the room identified by `room_name`. The room
    /// must have been opened (`open_room`) first; this method takes
    /// the name (not a RoomHandle) for FFI simplicity. Spawns the
    /// heartbeat task, subscribe loop, and Liveness state combiner.
    /// Errors if voice is already started.
    ///
    /// `on_frame` is called as `(from_peer_id_bytes: Uint8Array, pcm: Float32Array)`.
    /// `on_voice_peer_state` is called as `(peer_id: Uint8Array, in_call: bool, talking: bool)`.
    ///
    /// Multi-room note: voice can target only one room at a time per
    /// `Client` (a user is in at most one voice call). Switching rooms
    /// requires `voice_stop` then `voice_start` against the new room.
    pub fn voice_start(
        &self,
        room_name: String,
        on_frame: &js_sys::Function,
        on_voice_peer_state: &js_sys::Function,
    ) -> Result<(), JsError> {
        // Re-derive the Room from the name. Duplicates Argon2id work
        // already done by `open_room`, but voice is started rarely so
        // the cost is acceptable. A future API could take a RoomHandle
        // and pull the Room out of `OpenRoom` directly.
        let room = sunset_core::Room::open(&room_name)
            .map_err(|e| JsError::new(&format!("voice_start Room::open: {e}")))?;
        crate::voice::voice_start(
            &self.voice,
            &self.identity,
            &Rc::new(room),
            &self.bus,
            on_frame,
            on_voice_peer_state,
        )
    }

    /// Stop the voice subsystem and release its resources.
    pub fn voice_stop(&self) -> Result<(), JsError> {
        crate::voice::voice_stop(&self.voice)
    }

    pub fn voice_input(&self, pcm: &js_sys::Float32Array) -> Result<(), JsError> {
        crate::voice::voice_input(&self.voice, pcm)
    }
}

fn intent_state_str(s: sunset_sync::IntentState) -> &'static str {
    match s {
        sunset_sync::IntentState::Connecting => "connecting",
        sunset_sync::IntentState::Connected => "connected",
        sunset_sync::IntentState::Backoff => "backoff",
        sunset_sync::IntentState::Cancelled => "cancelled",
    }
}

/// Build the JS-shaped `{addr, state, attempt, peer_id?}` object used
/// by both `peer_connection_snapshot` and `on_peer_connection_state`.
fn intent_snapshot_to_js(snap: &sunset_sync::IntentSnapshot) -> Result<JsValue, JsError> {
    let obj = js_sys::Object::new();
    let addr_str = String::from_utf8_lossy(snap.addr.as_bytes()).into_owned();
    js_sys::Reflect::set(
        &obj,
        &JsValue::from_str("addr"),
        &JsValue::from_str(&addr_str),
    )
    .map_err(|_| JsError::new("Reflect::set addr failed"))?;
    js_sys::Reflect::set(
        &obj,
        &JsValue::from_str("state"),
        &JsValue::from_str(intent_state_str(snap.state)),
    )
    .map_err(|_| JsError::new("Reflect::set state failed"))?;
    js_sys::Reflect::set(
        &obj,
        &JsValue::from_str("attempt"),
        &JsValue::from_f64(snap.attempt as f64),
    )
    .map_err(|_| JsError::new("Reflect::set attempt failed"))?;
    if let Some(pid) = &snap.peer_id {
        let pk_arr = js_sys::Uint8Array::from(pid.0.as_bytes());
        js_sys::Reflect::set(&obj, &JsValue::from_str("peer_id"), &pk_arr)
            .map_err(|_| JsError::new("Reflect::set peer_id failed"))?;
    }
    Ok(obj.into())
}

/// Spawn the single supervisor-state forwarder. Subscribes to
/// `PeerSupervisor::subscribe()` once at Client construction; each event
/// is dispatched to whatever JS handler is currently installed in
/// `on_peer_connection_state` (None means dropped). This keeps the
/// JS-facing `on_peer_connection_state(handler)` setter cheap and
/// idempotent.
fn spawn_peer_connection_state_forwarder(
    supervisor: Rc<sunset_sync::PeerSupervisor<MemoryStore, MultiTransport<WsT, RtcT>>>,
    handler: Rc<RefCell<Option<js_sys::Function>>>,
) {
    use futures::StreamExt as _;
    let mut sub = supervisor.subscribe();
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(snap) = sub.next().await {
            let obj = match intent_snapshot_to_js(&snap) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(error = ?e, "intent_snapshot_to_js failed");
                    continue;
                }
            };
            if let Some(h) = handler.borrow().as_ref() {
                let _ = h.call1(&JsValue::NULL, &obj);
            }
        }
    });
}
