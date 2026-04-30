//! JS-exported Client: identity + room + sync engine wired together.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use rand_core::SeedableRng;
use wasm_bindgen::prelude::*;
use zeroize::Zeroizing;

use sunset_core::{Ed25519Verifier, Identity, MessageBody, Room};
use sunset_noise::{NoiseIdentity, NoiseTransport};
use sunset_store_memory::MemoryStore;
use sunset_sync::{MultiTransport, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_sync_webrtc_browser::WebRtcRawTransport;
use sunset_sync_ws_browser::WebSocketRawTransport;

use crate::identity::identity_from_seed;
use crate::relay_signaler::RelaySignaler;

type WsT = NoiseTransport<WebSocketRawTransport>;
type RtcT = NoiseTransport<WebRtcRawTransport>;
type Engine = SyncEngine<MemoryStore, MultiTransport<WsT, RtcT>>;

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
    identity: Identity,
    room: Rc<Room>,
    store: Arc<MemoryStore>,
    engine: Rc<Engine>,
    on_message: Rc<RefCell<Option<js_sys::Function>>>,
    on_receipt: Rc<RefCell<Option<js_sys::Function>>>,
    relay_status: Rc<RefCell<String>>,
    presence_started: Rc<RefCell<bool>>,
    tracker_handles: Rc<crate::membership_tracker::TrackerHandles>,
}

#[wasm_bindgen]
impl Client {
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8], room_name: &str) -> Result<Client, JsError> {
        let identity = identity_from_seed(seed).map_err(|e| JsError::new(&e))?;
        let room =
            Rc::new(Room::open(room_name).map_err(|e| JsError::new(&format!("Room::open: {e}")))?);

        let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

        // Build the WebSocket transport (relay path).
        let ws_raw = WebSocketRawTransport::dial_only();
        let ws_noise =
            NoiseTransport::new(ws_raw, Arc::new(IdentityNoiseAdapter(identity.clone())));

        // Build the WebRTC transport (direct path), backed by the
        // RelaySignaler that drives Noise_KK over CRDT entries.
        let room_fp_hex = room.fingerprint().to_hex();
        let signaler = RelaySignaler::new(identity.clone(), room_fp_hex.clone(), &store);
        let local_peer = PeerId(identity.store_verifying_key());
        let signaler_dyn: Rc<dyn sunset_sync::Signaler> = signaler;
        let rtc_raw = WebRtcRawTransport::new(
            signaler_dyn,
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

        // Spawn the engine event loop on the browser microtask queue.
        let engine_clone = engine.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = engine_clone.run().await {
                web_sys::console::error_1(&JsValue::from_str(&format!("sync engine exited: {e}")));
            }
        });

        Ok(Client {
            identity,
            room,
            store,
            engine,
            on_message: Rc::new(RefCell::new(None)),
            on_receipt: Rc::new(RefCell::new(None)),
            relay_status: Rc::new(RefCell::new("disconnected".to_owned())),
            presence_started: Rc::new(RefCell::new(false)),
            tracker_handles: Rc::new(crate::membership_tracker::TrackerHandles::new(
                "disconnected",
            )),
        })
    }

    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8> {
        self.identity.public().as_bytes().to_vec()
    }

    #[wasm_bindgen(getter)]
    pub fn relay_status(&self) -> String {
        self.relay_status.borrow().clone()
    }

    pub async fn add_relay(&self, url_with_fragment: String) -> Result<(), JsError> {
        *self.relay_status.borrow_mut() = "connecting".to_owned();
        let addr = sunset_sync::PeerAddr::new(Bytes::from(url_with_fragment));
        match self.engine.add_peer(addr).await {
            Ok(()) => {
                *self.relay_status.borrow_mut() = "connected".to_owned();
                Ok(())
            }
            Err(e) => {
                *self.relay_status.borrow_mut() = "error".to_owned();
                Err(JsError::new(&format!("add_relay: {e}")))
            }
        }
    }

    /// Establish a direct WebRTC peer connection. Signaling rides on the
    /// existing relay-mediated CRDT replication, encrypted under Noise_KK.
    /// After this returns, `peer_connection_mode(peer_pubkey)` will
    /// eventually read `"direct"` once the remote's Hello arrives and the
    /// engine emits `PeerAdded { kind: Secondary }`. The on_members_changed
    /// callback (if registered) will fire when the transition lands.
    pub async fn connect_direct(&self, peer_pubkey: &[u8]) -> Result<(), JsError> {
        let pk: [u8; 32] = peer_pubkey
            .try_into()
            .map_err(|_| JsError::new("peer_pubkey must be 32 bytes"))?;
        let x_pub = sunset_noise::ed25519_public_to_x25519(&pk)
            .map_err(|e| JsError::new(&format!("x25519 derive: {e}")))?;
        let addr_str = format!("webrtc://{}#x25519={}", hex::encode(pk), hex::encode(x_pub));
        let addr = sunset_sync::PeerAddr::new(Bytes::from(addr_str));
        self.engine
            .add_peer(addr)
            .await
            .map_err(|e| JsError::new(&format!("connect_direct: {e}")))?;
        Ok(())
    }

    /// Returns one of `"direct"`, `"via_relay"`, `"unknown"`.
    ///
    /// Reads from the membership tracker's `peer_kinds`, which is only
    /// populated after `start_presence` has been called. Callers that
    /// invoke this before `start_presence` will get `"unknown"` even
    /// when a real connection exists.
    pub fn peer_connection_mode(&self, peer_pubkey: &[u8]) -> String {
        use sunset_sync::TransportKind;
        let pk: [u8; 32] = match peer_pubkey.try_into() {
            Ok(p) => p,
            Err(_) => return "unknown".to_owned(),
        };
        let peer_id = PeerId(sunset_store::VerifyingKey::new(Bytes::copy_from_slice(&pk)));
        match self.tracker_handles.peer_kinds.borrow().get(&peer_id) {
            Some(TransportKind::Secondary) => "direct",
            Some(TransportKind::Primary) => "via_relay",
            _ => "unknown",
        }
        .to_owned()
    }

    /// Start the heartbeat publisher + the membership tracker. Idempotent.
    ///
    /// May be called either before or after `add_relay` / `connect_direct`:
    /// the tracker subscribes to the engine's no-replay event stream AND
    /// snapshots the engine's current peer set, so already-connected peers
    /// are picked up regardless of call order.
    pub async fn start_presence(&self, interval_ms: u32, ttl_ms: u32, refresh_ms: u32) {
        if *self.presence_started.borrow() {
            return;
        }
        *self.presence_started.borrow_mut() = true;

        let room_fp_hex = self.room.fingerprint().to_hex();
        let local_peer = sunset_sync::PeerId(self.identity.store_verifying_key());

        crate::presence_publisher::spawn_publisher(
            self.identity.clone(),
            room_fp_hex.clone(),
            self.store.clone(),
            interval_ms as u64,
            ttl_ms as u64,
        );

        let engine_events = self.engine.subscribe_engine_events().await;

        // Seed peer_kinds from the engine's snapshot. Order matters:
        // subscribe FIRST, then snapshot — so events fired between the
        // two land in the receiver and just produce idempotent re-inserts.
        let snapshot = self.engine.current_peers().await;
        {
            let mut peer_kinds = self.tracker_handles.peer_kinds.borrow_mut();
            for (pk, kind) in snapshot {
                peer_kinds.insert(pk, kind);
            }
        }

        crate::membership_tracker::spawn_tracker(
            self.store.clone(),
            engine_events,
            local_peer,
            room_fp_hex,
            interval_ms as u64,
            ttl_ms as u64,
            refresh_ms as u64,
            (*self.tracker_handles).clone(),
        );

        // Fire an initial relay_status callback in case the seed
        // pushed us into "connected" / "disconnected".
        crate::membership_tracker::fire_relay_status_now(&self.tracker_handles);
    }

    pub fn on_members_changed(&self, callback: js_sys::Function) {
        *self.tracker_handles.on_members.borrow_mut() = Some(callback);
    }

    pub fn on_relay_status_changed(&self, callback: js_sys::Function) {
        *self.tracker_handles.on_relay_status.borrow_mut() = Some(callback);
    }

    pub async fn publish_room_subscription(&self) -> Result<(), JsError> {
        use std::time::Duration;
        // Broader filter: covers <fp>/msg/ + <fp>/webrtc/ + future
        // per-room namespaces. The relay sends us everything in the room;
        // local consumers (chat UI, RelaySignaler) sub-filter as they go.
        let filter = sunset_core::room_filter(&self.room);
        self.engine
            .publish_subscription(filter, Duration::from_secs(3600))
            .await
            .map_err(|e| JsError::new(&format!("publish_subscription: {e}")))?;
        Ok(())
    }

    pub async fn send_message(
        &self,
        body: String,
        sent_at_ms: f64,
        nonce_seed: Vec<u8>,
    ) -> Result<String, JsError> {
        use sunset_store::Store as _;

        let nonce_seed_arr: [u8; 32] = nonce_seed
            .as_slice()
            .try_into()
            .map_err(|_| JsError::new("nonce_seed must be 32 bytes"))?;

        let mut rng = rand_chacha::ChaCha20Rng::from_seed(nonce_seed_arr);

        let composed = sunset_core::compose_message(
            &self.identity,
            &self.room,
            0u64,
            sent_at_ms as u64,
            MessageBody::Text(body),
            &mut rng,
        )
        .map_err(|e| JsError::new(&format!("compose_message: {e}")))?;

        let value_hash_hex = composed.entry.value_hash.to_hex();

        self.store
            .insert(composed.entry, Some(composed.block))
            .await
            .map_err(|e| JsError::new(&format!("store insert: {e}")))?;

        Ok(value_hash_hex)
    }

    pub fn on_message(&self, callback: js_sys::Function) {
        *self.on_message.borrow_mut() = Some(callback);
        self.spawn_message_subscription();
    }

    pub fn on_receipt(&self, callback: js_sys::Function) {
        *self.on_receipt.borrow_mut() = Some(callback);
        // No new subscription needed — spawn_message_subscription handles
        // both Text and Receipt variants.
    }

    fn spawn_message_subscription(&self) {
        let store = self.store.clone();
        let room = self.room.clone();
        let identity = self.identity.clone();
        let identity_pub = self.identity.public();
        let on_message = self.on_message.clone();
        let on_receipt = self.on_receipt.clone();

        wasm_bindgen_futures::spawn_local(async move {
            use futures::StreamExt;
            use std::collections::HashSet;
            use sunset_core::{decode_message, room_messages_filter, MessageBody};
            use sunset_store::{Event, Replay, Store as _};

            // Session-only dedup: which Text value-hashes have we already
            // acked since this subscription started? Replay::All will
            // redeliver them on page load; this set keeps us from writing
            // a fresh receipt every time. Cross-session dedup is out of
            // scope for v1.
            let mut acked: HashSet<sunset_store::Hash> = HashSet::new();

            let filter = room_messages_filter(&room);
            let mut events = match store.subscribe(filter, Replay::All).await {
                Ok(s) => s,
                Err(e) => {
                    web_sys::console::error_1(&JsValue::from_str(&format!("store.subscribe: {e}")));
                    return;
                }
            };

            let mut rng = rand_chacha::ChaCha20Rng::from_entropy();

            while let Some(ev) = events.next().await {
                let entry = match ev {
                    Ok(Event::Inserted(e)) => e,
                    Ok(Event::Replaced { new, .. }) => new,
                    Ok(_) => continue,
                    Err(e) => {
                        web_sys::console::error_1(&JsValue::from_str(&format!("store event: {e}")));
                        continue;
                    }
                };

                let block = match store.get_content(&entry.value_hash).await {
                    Ok(Some(b)) => b,
                    Ok(None) => continue,
                    Err(e) => {
                        web_sys::console::error_1(&JsValue::from_str(&format!("get_content: {e}")));
                        continue;
                    }
                };

                let decoded = match decode_message(&room, &entry, &block) {
                    Ok(d) => d,
                    Err(e) => {
                        web_sys::console::error_1(&JsValue::from_str(&format!(
                            "decode_message: {e}"
                        )));
                        continue;
                    }
                };

                let is_self = decoded.author_key == identity_pub;

                match decoded.body.clone() {
                    MessageBody::Text(text) => {
                        // Deliver to the FE on_message callback.
                        let value_hash_hex = entry.value_hash.to_hex();
                        let incoming = crate::messages::from_decoded_text(
                            decoded,
                            text,
                            value_hash_hex,
                            is_self,
                        );
                        if let Some(cb) = on_message.borrow().as_ref() {
                            let _ = cb.call1(&JsValue::NULL, &JsValue::from(incoming));
                        }

                        // Auto-ack: only for non-self texts, only once per session.
                        if !is_self && !acked.contains(&entry.value_hash) {
                            acked.insert(entry.value_hash);
                            send_receipt(&store, &room, &identity, entry.value_hash, &mut rng)
                                .await;
                        }
                    }
                    MessageBody::Receipt { for_value_hash } => {
                        // Drop self-Receipts at the bridge (see spec:
                        // auto-ack never produces them, so anything here
                        // is from manual composition / future protocol
                        // changes — the FE doesn't need a redundant check).
                        if is_self {
                            continue;
                        }
                        let for_hex = for_value_hash.to_hex();
                        let from_pub = decoded.author_key;
                        let incoming = crate::messages::receipt_to_js(for_hex, from_pub);
                        if let Some(cb) = on_receipt.borrow().as_ref() {
                            let _ = cb.call1(&JsValue::NULL, &JsValue::from(incoming));
                        }
                    }
                }
            }
        });
    }
}

/// Compose and insert a Receipt for `for_value_hash` into the local
/// store. Used by the auto-ack path in `spawn_message_subscription`.
/// Errors are logged via `web_sys::console` and swallowed — receipts
/// are best-effort; failing to ack is not fatal.
async fn send_receipt(
    store: &std::sync::Arc<sunset_store_memory::MemoryStore>,
    room: &sunset_core::Room,
    identity: &sunset_core::Identity,
    for_value_hash: sunset_store::Hash,
    rng: &mut rand_chacha::ChaCha20Rng,
) {
    use sunset_store::Store as _;
    let now_ms = js_sys::Date::now() as u64;
    let composed = match sunset_core::compose_receipt(identity, room, 0, now_ms, for_value_hash, rng) {
        Ok(c) => c,
        Err(e) => {
            web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
                "compose_receipt failed: {e}"
            )));
            return;
        }
    };
    if let Err(e) = store.insert(composed.entry, Some(composed.block)).await {
        web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&format!(
            "store.insert(receipt) failed: {e}"
        )));
    }
}
