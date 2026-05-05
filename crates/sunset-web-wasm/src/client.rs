//! JS-exported Client: identity + sync engine wired together.
//! Per-room operations (send_message, on_message, on_receipt, presence,
//! reactions, members, etc.) live in `RoomHandle`.

use std::rc::Rc;
use std::sync::Arc;

use wasm_bindgen::prelude::*;
use zeroize::Zeroizing;

use std::time::Duration;

use sunset_core::{Ed25519Verifier, Identity};
use sunset_noise::{NoiseIdentity, NoiseTransport, do_handshake_responder};
use sunset_store_memory::MemoryStore;
use sunset_sync::{MultiTransport, PeerId, Signer, SpawningAcceptor, SyncConfig, SyncEngine};
use sunset_sync_webrtc_browser::WebRtcRawTransport;
use sunset_sync_ws_browser::WebSocketRawTransport;

use crate::identity::identity_from_seed;

pub(crate) type WsT = NoiseTransport<WebSocketRawTransport>;
/// The browser's inbound WebRTC pipeline mirrors the relay's
/// WebSocket wiring (see `sunset-relay/src/relay.rs`):
///   raw WebRTC accept → spawn task → Noise IK responder → ready conn
/// The `SpawningAcceptor` wrapper is load-bearing. Without it, the
/// engine's `select!` loop in `SyncEngine::run` would drop the
/// in-flight `NoiseTransport::accept` future (and the
/// `do_handshake_responder` it's running) every time *any* other arm
/// fires — store events, cmd_rx, anti-entropy ticks. That drop closes
/// the underlying `WebRtcRawConnection`'s data channels, which
/// (combined with the on_close handler in `wasm.rs`) bubbles back to
/// the dialer as `Err("dc closed")` partway through Noise IK. We saw
/// this manifest as `voice_network`/`presence`/`kill_relay` flakes
/// where alice's webrtc:// dial timed out at 10 s waiting for
/// `connection_mode == "direct"`. The connector half of the
/// `SpawningAcceptor` (a clone of the same `WebRtcRawTransport`) is
/// what handles outbound `webrtc://` dials — sharing the underlying
/// transport via the `WebRtcRawTransport: Clone` impl keeps signaling
/// state coherent between the two halves.
pub(crate) type RtcT = SpawningAcceptor<
    WebRtcRawTransport,
    NoiseTransport<WebRtcRawTransport>,
    RtcPromoteFn,
    RtcPromoteFut,
    sunset_noise::NoiseConnection<sunset_sync_webrtc_browser::WebRtcRawConnection>,
>;
type RtcPromoteFut = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = sunset_sync::Result<
                    sunset_noise::NoiseConnection<sunset_sync_webrtc_browser::WebRtcRawConnection>,
                >,
            >,
    >,
>;
type RtcPromoteFn = Box<dyn Fn(sunset_sync_webrtc_browser::WebRtcRawConnection) -> RtcPromoteFut>;

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
    /// per-room registry). Per-room ops route through `RoomHandle`;
    /// supervisor-level ops (`add_relay`, `on_intent_changed`,
    /// `intents`) route through `Peer`'s thin delegators.
    inner: Rc<sunset_core::Peer<MemoryStore, MultiTransport<WsT, RtcT>>>,
    /// Kept on the Client because the voice subsystem needs identity
    /// to start a per-room voice session. Could move into a `Peer`
    /// accessor later; for now the duplication is small and explicit.
    identity: Identity,
    /// Voice subsystem (single concurrent call per Client). See
    /// `voice_start` for the room-handle parameter that selects which
    /// room the call targets.
    voice: crate::voice::VoiceCell,
    bus: crate::voice::BusArc,
}

#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8], heartbeat_interval_ms: u32) -> Result<Client, JsError> {
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
        let rtc_noise_id: Arc<dyn NoiseIdentity> = Arc::new(IdentityNoiseAdapter(identity.clone()));
        // Outbound dialer half: Noise IK initiator on top of the same
        // raw transport. The clone shares signaling state via Rc so
        // outgoing offers/answers/ICE flow through the same dispatcher
        // the inbound pump is reading from.
        let rtc_connector = NoiseTransport::new(rtc_raw.clone(), rtc_noise_id.clone());
        let rtc_promote: RtcPromoteFn = {
            let identity = rtc_noise_id.clone();
            Box::new(move |raw_conn| {
                let identity = identity.clone();
                Box::pin(async move {
                    do_handshake_responder(raw_conn, identity)
                        .await
                        .map_err(|e| sunset_sync::Error::Transport(format!("noise responder: {e}")))
                })
            })
        };
        // 60 s handshake timeout matches the relay's default and is
        // generous for a localhost WebRTC handshake (~tens of ms in
        // practice). Hits only as a backstop against a peer that
        // signals an Offer but never finishes the data-channel
        // handshake.
        let rtc =
            SpawningAcceptor::new(rtc_raw, rtc_connector, rtc_promote, Duration::from_secs(60));

        let multi = MultiTransport::new(ws_noise, rtc);
        let signer: Arc<dyn Signer> = Arc::new(identity.clone());

        let mut config = SyncConfig::default();
        if heartbeat_interval_ms > 0 {
            let interval = std::time::Duration::from_millis(heartbeat_interval_ms as u64);
            config.heartbeat_interval = interval;
            // Match default 3× ratio between interval and timeout.
            config.heartbeat_timeout = interval * 3;
        }

        let engine = Rc::new(SyncEngine::new(
            store.clone(),
            multi,
            config,
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

        let peer = sunset_core::Peer::new(
            identity.clone(),
            store.clone(),
            engine.clone(),
            supervisor,
            dispatcher,
        );

        Ok(Client {
            inner: peer,
            identity,
            voice: crate::voice::new_voice_cell(),
            bus,
        })
    }

    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    /// Register a durable intent to keep connected to `url`. Returns
    /// the supervisor-assigned `IntentId` once the intent is recorded
    /// (one cmd-channel round-trip; does NOT wait for the first
    /// connection). The only `Err` is for malformed input.
    pub async fn add_relay(&self, url: String) -> Result<f64, JsError> {
        let fetch: Rc<dyn sunset_relay_resolver::HttpFetch> =
            Rc::new(crate::resolver_adapter::WebSysFetch);
        let connectable = sunset_sync::Connectable::Resolving { input: url, fetch };
        let id = self
            .inner
            .add_relay(connectable)
            .await
            .map_err(|e| JsError::new(&format!("add_relay: {e}")))?;
        Ok(id as f64)
    }

    /// Register a JS callback that fires:
    ///   * once per existing intent, immediately on register, and
    ///   * once per intent state transition thereafter.
    /// The callback receives an `IntentSnapshotJs`.
    pub fn on_intent_changed(&self, callback: js_sys::Function) {
        let inner = self.inner.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut rx = inner.subscribe_intents().await;
            while let Some(snap) = rx.recv().await {
                let js_snap = crate::intent::IntentSnapshotJs::from(&snap);
                let _ = callback.call1(&JsValue::NULL, &JsValue::from(js_snap));
            }
        });
    }

    /// Snapshot of every registered intent. Returns a Vec
    /// (wasm-bindgen serialises this to a JS array). Used by the
    /// frontend on first paint, before the `on_intent_changed`
    /// callback's replay arrives.
    pub async fn intents(&self) -> Vec<crate::intent::IntentSnapshotJs> {
        self.inner
            .intents()
            .await
            .iter()
            .map(crate::intent::IntentSnapshotJs::from)
            .collect()
    }

    pub async fn open_room(&self, name: String) -> Result<crate::room_handle::RoomHandle, JsError> {
        let open = self
            .inner
            .open_room(&name)
            .await
            .map_err(|e| JsError::new(&format!("open_room: {e}")))?;
        Ok(crate::room_handle::RoomHandle::new(open))
    }

    /// Update the display name carried in every open room's presence
    /// heartbeats. Persists for the lifetime of the Client (the Gleam
    /// layer is responsible for localStorage). Idempotent.
    pub fn set_self_name(&self, name: String) {
        self.inner.set_self_name(&name);
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
        room_name: &str,
        on_frame: &js_sys::Function,
        on_voice_peer_state: &js_sys::Function,
    ) -> Result<(), JsError> {
        // Re-derive the Room from the name. Duplicates Argon2id work
        // already done by `open_room`, but voice is started rarely so
        // the cost is acceptable. A future API could take a RoomHandle
        // and pull the Room out of `OpenRoom` directly.
        let room = sunset_core::Room::open(room_name)
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
