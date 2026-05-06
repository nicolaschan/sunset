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
use sunset_sync::{
    FallbackTransport, MultiTransport, PeerId, Signer, SpawningAcceptor, SyncConfig, SyncEngine,
};
use sunset_sync_webrtc_browser::WebRtcRawTransport;
use sunset_sync_webtransport_browser::WebTransportRawTransport;
use sunset_sync_ws_browser::WebSocketRawTransport;

use crate::identity::identity_from_seed;

/// Primary half of the browser's outer `MultiTransport`: WebTransport
/// preferred, with WebSocket as the fallback. The relay's identity
/// descriptor advertises a WT URL when the relay has UDP/QUIC bound;
/// `FallbackTransport` rewrites the scheme and falls back to WS on any
/// connect failure (cert mismatch, UDP blocked, browser w/o WT
/// support, …). When the relay only advertises WS,
/// `FallbackTransport::connect` short-circuits straight to the WS half.
pub(crate) type WsT = FallbackTransport<
    NoiseTransport<WebTransportRawTransport>,
    NoiseTransport<WebSocketRawTransport>,
>;
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
    /// Bus shared between voice sessions. Rc so it can be upcast to
    /// Rc<dyn DynBus> without allocation.
    bus: crate::voice::BusArc,
}

#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8], heartbeat_interval_ms: u32) -> Result<Client, JsError> {
        // Route Rust panics through `console.error` once, on the first
        // Client construction. Without this a panic in any background
        // task surfaces only as `RuntimeError: unreachable` in the
        // browser, which is unhelpful when debugging FFI shims.
        // `set_once` makes repeated `Client::new` calls idempotent.
        console_error_panic_hook::set_once();

        let identity = identity_from_seed(seed).map_err(|e| JsError::new(&e))?;
        let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

        let ws_raw = WebSocketRawTransport::dial_only();
        let ws_noise =
            NoiseTransport::new(ws_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));
        let wt_raw = WebTransportRawTransport::dial_only();
        let wt_noise =
            NoiseTransport::new(wt_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));
        // FallbackTransport routes by URL scheme: `wt://`/`wts://` →
        // try WT first, then WS on failure (URL scheme rewritten by
        // `fallback_addr_for`); `ws://`/`wss://` → straight to WS.
        let primary = FallbackTransport::new(wt_noise, ws_noise);

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

        let multi = MultiTransport::new(primary, rtc);
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

    /// Start voice in the given room. The `room_handle` must have been
    /// obtained via `open_room`. Constructs `WebDialer`/`WebFrameSink`/
    /// `WebPeerStateSink`, builds `VoiceRuntime`, and spawns all five
    /// protocol tasks. Only one active voice session per `Client`.
    ///
    /// JS callback signatures:
    /// - `on_pcm(peer_id: Uint8Array, pcm: Float32Array)` — per-frame delivery
    /// - `on_drop_peer(peer_id: Uint8Array)` — peer left the call
    /// - `on_voice_peer_state(peer_id, in_call, talking, is_muted)` — state change
    ///
    /// Per-peer playback volume is intentionally not threaded through Rust:
    /// the GainNode is a browser-shaped concept and the runtime has no need
    /// to know. JS callers manage it directly via `voice.ffi.mjs::setPeerVolume`.
    /// See spec revision (2026-05-03) for the rationale.
    pub fn voice_start(
        &self,
        room_handle: &crate::room_handle::RoomHandle,
        on_pcm: js_sys::Function,
        on_drop_peer: js_sys::Function,
        on_voice_peer_state: js_sys::Function,
    ) -> Result<(), JsError> {
        crate::voice::voice_start(
            &self.voice,
            &self.identity,
            room_handle,
            &self.bus,
            on_pcm,
            on_drop_peer,
            on_voice_peer_state,
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

    /// Switch the active send-side voice quality preset. Accepts
    /// `"voice"` (24 kbps mono VOIP), `"high"` (96 kbps stereo), or
    /// `"maximum"` (510 kbps stereo, the default). Returns an error
    /// if voice isn't started or the label is unknown.
    pub fn voice_set_quality(&self, label: &str) -> Result<(), JsError> {
        crate::voice::voice_set_quality(&self.voice, label)
    }

    /// Read back the active quality preset as one of `"voice"`,
    /// `"high"`, `"maximum"`, or `null` if voice isn't started.
    pub fn voice_quality(&self) -> Option<String> {
        crate::voice::voice_quality(&self.voice).map(str::to_string)
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

    /// Return per-peer recorded frames as `[{len, checksum, rms}]`.
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

    /// Generate one 20 ms PCM frame of continuous 440 Hz sine. The
    /// `counter` parameter advances the phase by exactly one frame so
    /// successive counter values produce a continuous tone (which is
    /// what an Opus encoder is designed to compress + decode
    /// faithfully).
    #[cfg(feature = "test-hooks")]
    pub fn voice_synth_pcm(counter: i32) -> js_sys::Float32Array {
        let pcm = crate::voice::test_hooks::synth_pcm_with_counter(counter);
        js_sys::Float32Array::from(pcm.as_slice())
    }

    /// Peers seen by the voice runtime's auto-connect FSM (i.e. peers
    /// for which we observed a `voice-presence/...` durable entry over
    /// sunset-sync). Returns `Uint8Array[]`.
    #[cfg(feature = "test-hooks")]
    pub fn voice_auto_connect_peers(&self) -> Result<JsValue, JsError> {
        crate::voice::auto_connect_peers(&self.voice)
    }

    /// Peers from whom the voice runtime has decoded at least one
    /// inbound voice payload (Frame or Heartbeat). Returns
    /// `Uint8Array[]`.
    #[cfg(feature = "test-hooks")]
    pub fn voice_observed_voice_peers(&self) -> Result<JsValue, JsError> {
        crate::voice::observed_voice_peers(&self.voice)
    }

    /// Per-peer jitter buffer depth as `[{peer_id, depth}]`. Indicates
    /// frames received but not yet drained by the jitter pump.
    #[cfg(feature = "test-hooks")]
    pub fn voice_jitter_depths(&self) -> Result<JsValue, JsError> {
        crate::voice::jitter_depths(&self.voice)
    }

    /// Peers currently connected at the engine level (i.e. PeerHello
    /// completed). Returns `Uint8Array[]`. Useful for distinguishing
    /// "WebRTC handshake complete" from "WebRTC handshake hung".
    #[cfg(feature = "test-hooks")]
    pub async fn voice_engine_connected_peers(&self) -> Result<JsValue, JsError> {
        use js_sys::{Array, Uint8Array};
        let peers = self.inner.engine_handle().connected_peers().await;
        let arr = Array::new();
        for p in peers {
            arr.push(&Uint8Array::from(p.0.as_bytes()));
        }
        Ok(arr.into())
    }
}
