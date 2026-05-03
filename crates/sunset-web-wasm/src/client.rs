//! JS-exported Client: identity + sync engine wired together.
//! Per-room operations (send_message, on_message, on_receipt, presence,
//! reactions, members, etc.) live in `RoomHandle`.

use std::rc::Rc;
use std::sync::Arc;

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
    /// supervisor handle is needed for `on_intent_changed` /
    /// `intents`. Identity could move into `Peer` accessors later;
    /// for now the duplication is small and explicit.
    identity: Identity,
    supervisor: Rc<sunset_sync::PeerSupervisor<MemoryStore, MultiTransport<WsT, RtcT>>>,
    /// Voice subsystem (single concurrent call per Client). See
    /// `voice_start` for the room-handle parameter that selects which
    /// room the call targets.
    voice: crate::voice::VoiceCell,
    /// Bus shared between voice sessions. Rc so it can be upcast to
    /// Rc<dyn DynBus> without allocation.
    bus: crate::voice::BusArc,
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

        // Rc so it can be upcast to Rc<dyn DynBus>.
        let bus: crate::voice::BusArc = Rc::new(sunset_core::bus::BusImpl::new(
            store.clone(),
            engine.clone(),
            identity.clone(),
        ));

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
        let supervisor = self.supervisor.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut rx = supervisor.subscribe_intents().await;
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

    /// Start voice in the given room. The `room_handle` must have been
    /// obtained via `open_room`. Constructs `WebDialer`/`WebFrameSink`/
    /// `WebPeerStateSink`, builds `VoiceRuntime`, and spawns all five
    /// protocol tasks. Only one active voice session per `Client`.
    ///
    /// JS callback signatures:
    /// - `on_pcm(peer_id: Uint8Array, pcm: Float32Array)` — per-frame delivery
    /// - `on_drop_peer(peer_id: Uint8Array)` — peer left the call
    /// - `on_voice_peer_state(peer_id, in_call, talking, is_muted)` — state change
    /// - `on_set_peer_volume(peer_id: Uint8Array, gain: number)` — set GainNode
    pub fn voice_start(
        &self,
        room_handle: &crate::room_handle::RoomHandle,
        on_pcm: js_sys::Function,
        on_drop_peer: js_sys::Function,
        on_voice_peer_state: js_sys::Function,
        on_set_peer_volume: js_sys::Function,
    ) -> Result<(), JsError> {
        crate::voice::voice_start(
            &self.voice,
            &self.identity,
            room_handle,
            &self.bus,
            on_pcm,
            on_drop_peer,
            on_voice_peer_state,
            on_set_peer_volume,
        )
    }

    /// Stop the voice subsystem and release all resources. Dropping
    /// `VoiceRuntime` cancels all five protocol tasks.
    pub fn voice_stop(&self) -> Result<(), JsError> {
        crate::voice::voice_stop(&self.voice)
    }

    /// Forward PCM from the browser capture worklet to the runtime's
    /// encode + publish path. Called once per 20 ms audio frame.
    pub fn voice_input(&self, pcm: &js_sys::Float32Array) -> Result<(), JsError> {
        crate::voice::voice_input(&self.voice, pcm)
    }

    /// Toggle microphone mute. When muted, `send_pcm` drops frames and
    /// heartbeats carry `is_muted: true`.
    pub fn voice_set_muted(&self, muted: bool) {
        crate::voice::voice_set_muted(&self.voice, muted);
    }

    /// Toggle deafen. When deafened, the jitter pump skips
    /// `FrameSink::deliver` (so the user hears silence) but liveness
    /// tracking continues.
    pub fn voice_set_deafened(&self, deafened: bool) {
        crate::voice::voice_set_deafened(&self.voice, deafened);
    }

    /// Set per-peer playback volume. `gain` is a linear multiplier
    /// (0.0 = mute, 1.0 = unity, >1.0 = boost). Forwarded to JS via
    /// the `on_set_peer_volume` callback registered at `voice_start`.
    pub fn voice_set_peer_volume(&self, peer_id: &[u8], gain: f32) {
        crate::voice::voice_set_peer_volume(&self.voice, peer_id, gain);
    }

    // ---- Test hooks (compiled in only with feature "test-hooks") ----

    /// Bypass the capture worklet and inject PCM directly into the
    /// runtime's encode + publish path. Used by Playwright tests to
    /// generate deterministic synthetic frames.
    #[cfg(feature = "test-hooks")]
    pub fn voice_inject_pcm(&self, pcm: &js_sys::Float32Array) -> Result<(), JsError> {
        crate::voice::voice_input(&self.voice, pcm)
    }

    /// Wrap the current `FrameSink` in a recording adapter that captures
    /// per-peer frame metadata into an in-memory ring buffer. Call once
    /// after `voice_start`; subsequent `voice_recorded_frames` queries
    /// return data from that point onward.
    #[cfg(feature = "test-hooks")]
    pub fn voice_install_frame_recorder(&self) -> Result<(), JsError> {
        crate::voice::install_recorder(&self.voice)
    }

    /// Return per-peer recorded frames as `[{seq_in_frame, len, checksum}]`.
    /// Requires `voice_install_frame_recorder` to have been called first.
    #[cfg(feature = "test-hooks")]
    pub fn voice_recorded_frames(&self, peer_id: &[u8]) -> Result<JsValue, JsError> {
        crate::voice::recorded_frames(&self.voice, peer_id)
    }

    /// Return the runtime's current per-peer `VoicePeerState` snapshot
    /// as `[{peer_id, in_call, talking, is_muted}]`.
    #[cfg(feature = "test-hooks")]
    pub fn voice_active_peers(&self) -> Result<JsValue, JsError> {
        crate::voice::active_peers(&self.voice)
    }

    /// Generate a synthetic PCM frame with an embedded counter in
    /// `pcm[0]`. Useful from JS test code to create deterministic
    /// frames whose counter can be verified at the receiver.
    /// `pcm[0] = counter / 1_000_000.0`.
    #[cfg(feature = "test-hooks")]
    pub fn voice_synth_pcm(counter: i32) -> js_sys::Float32Array {
        let pcm = crate::voice::test_hooks::synth_pcm_with_counter(counter);
        js_sys::Float32Array::from(pcm.as_slice())
    }
}
