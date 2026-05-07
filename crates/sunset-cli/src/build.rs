//! Type aliases + `build_peer` factory: wires sunset-core::Peer with
//! a MemoryStore + FallbackTransport<NoiseTransport<WtNative>, NoiseTransport<WsNative>>.
//!
//! The transport pattern mirrors `sunset-web-wasm/src/client.rs` minus
//! WebRTC. FallbackTransport routes by URL scheme: `wt://`/`wts://`
//! prefers WT then falls back to WS; `ws://`/`wss://` short-circuits
//! straight to WS.

use std::rc::Rc;
use std::sync::Arc;

use sunset_core::{Ed25519Verifier, Identity, Peer};
use sunset_noise::{NoiseIdentity, NoiseTransport};
use sunset_store::VerifyingKey;
use sunset_store_memory::MemoryStore;
use sunset_sync::{
    BackoffPolicy, FallbackTransport, PeerId, PeerSupervisor, Signer, SyncConfig, SyncEngine,
};
use sunset_sync_webtransport_native::WebTransportRawTransport;
use sunset_sync_ws_native::WebSocketRawTransport;
use zeroize::Zeroizing;

pub type CliTransport = FallbackTransport<
    NoiseTransport<WebTransportRawTransport>,
    NoiseTransport<WebSocketRawTransport>,
>;

pub type CliPeer = Peer<MemoryStore, CliTransport>;
pub type CliEngine = SyncEngine<MemoryStore, CliTransport>;
pub type CliSupervisor = PeerSupervisor<MemoryStore, CliTransport>;

/// Adapter: sunset-core's `Identity` → `NoiseIdentity`.
struct IdentityNoiseAdapter(Identity);

impl NoiseIdentity for IdentityNoiseAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

/// Output of `build_peer`. Engines + supervisors must be `run` by the
/// caller (which spawns them on a `LocalSet`); the factory itself
/// does no I/O and starts no tasks.
pub struct BuiltPeer {
    pub peer: Rc<CliPeer>,
    pub engine: Rc<CliEngine>,
    pub supervisor: Rc<CliSupervisor>,
    pub store: Arc<MemoryStore>,
}

pub fn build_peer(identity: Identity) -> BuiltPeer {
    let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

    let ws_raw = WebSocketRawTransport::dial_only();
    let ws_noise = NoiseTransport::new(ws_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));
    let wt_raw = WebTransportRawTransport::dial_only();
    let wt_noise = NoiseTransport::new(wt_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));
    let transport = FallbackTransport::new(wt_noise, ws_noise);

    let local_peer = PeerId(VerifyingKey::new(bytes::Bytes::copy_from_slice(
        &identity.public().as_bytes(),
    )));
    let signer: Arc<dyn Signer> = Arc::new(identity.clone());

    let engine = Rc::new(SyncEngine::new(
        store.clone(),
        transport,
        SyncConfig::default(),
        local_peer,
        signer,
    ));
    let supervisor = PeerSupervisor::new(engine.clone(), BackoffPolicy::default());
    let dispatcher = sunset_core::MultiRoomSignaler::new();
    let peer = Peer::new(
        identity,
        store.clone(),
        engine.clone(),
        supervisor.clone(),
        dispatcher,
    );

    BuiltPeer {
        peer,
        engine,
        supervisor,
        store,
    }
}
