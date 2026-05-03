//! JS-exported Client: identity + sync engine wired together.
//! Per-room operations live in `RoomHandle`.

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
    inner: Rc<sunset_core::Peer<MemoryStore, MultiTransport<WsT, RtcT>>>,
    voice: crate::voice::VoiceCell,
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

        let peer = sunset_core::Peer::new(identity, store, engine, supervisor, dispatcher);

        Ok(Client {
            inner: peer,
            voice: crate::voice::new_voice_cell(),
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

    /// Initialise the voice subsystem. Spawns an in-process loopback
    /// decode loop; `output_handler` is invoked with a Float32Array
    /// of `FRAME_SAMPLES` samples (mono PCM at `SAMPLE_RATE`) for each
    /// decoded 20 ms frame. Must be called before `voice_input`.
    pub fn voice_start(&self, output_handler: &js_sys::Function) -> Result<(), JsError> {
        crate::voice::voice_start(&self.voice, output_handler)
    }

    /// Stop the voice subsystem and release its resources.
    pub fn voice_stop(&self) -> Result<(), JsError> {
        crate::voice::voice_stop(&self.voice)
    }

    /// Submit one 20 ms frame of mono PCM (Float32Array of length
    /// `FRAME_SAMPLES` at `SAMPLE_RATE`) for encoding + loopback
    /// delivery to the output handler.
    pub fn voice_input(&self, pcm: &js_sys::Float32Array) -> Result<(), JsError> {
        crate::voice::voice_input(&self.voice, pcm)
    }
}
