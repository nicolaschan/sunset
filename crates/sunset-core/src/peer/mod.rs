//! `Peer` is the host-agnostic "running sunset peer" entity.
//! Holds identity, store, sync engine, supervisor, and a registry of
//! open rooms. `Peer::open_room(name)` (added in Phase 5) returns an
//! `OpenRoom` handle.

mod open_room;

pub use open_room::OpenRoom;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use sunset_store::Store;
use sunset_sync::{PeerSupervisor, SyncEngine, Transport};

use crate::crypto::room::RoomFingerprint;
use crate::signaling::MultiRoomSignaler;
use crate::Identity;

pub struct Peer<St: Store + 'static, T: Transport + 'static> {
    identity: Identity,
    store: Arc<St>,
    engine: Rc<SyncEngine<St, T>>,
    supervisor: Rc<PeerSupervisor<St, T>>,
    relay_status: Rc<RefCell<String>>,
    open_rooms: RefCell<HashMap<RoomFingerprint, Weak<open_room::RoomState<St, T>>>>,
    pub(crate) rtc_signaler_dispatcher: Rc<MultiRoomSignaler>,
}

impl<St, T> Peer<St, T>
where
    St: Store + 'static,
    T: Transport + 'static,
    T::Connection: 'static,
{
    pub fn new(
        identity: Identity,
        store: Arc<St>,
        engine: Rc<SyncEngine<St, T>>,
        supervisor: Rc<PeerSupervisor<St, T>>,
        rtc_signaler_dispatcher: Rc<MultiRoomSignaler>,
    ) -> Rc<Self> {
        Rc::new(Self {
            identity,
            store,
            engine,
            supervisor,
            relay_status: Rc::new(RefCell::new("disconnected".to_owned())),
            open_rooms: RefCell::new(HashMap::new()),
            rtc_signaler_dispatcher,
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.identity.public().as_bytes()
    }

    pub fn relay_status(&self) -> String {
        self.relay_status.borrow().clone()
    }

    pub async fn open_room(self: &Rc<Self>, room_name: &str) -> crate::Result<OpenRoom<St, T>> {
        // Open the Room (Argon2id derivation; expensive — ~tens to
        // hundreds of ms with production params).
        let room = Rc::new(crate::Room::open(room_name)?);
        let fp = room.fingerprint();

        // Idempotency check: if this fingerprint is already open and the
        // weak still upgrades, return another handle to the same RoomState.
        if let Some(weak) = self.open_rooms.borrow().get(&fp) {
            if let Some(strong) = weak.upgrade() {
                return Ok(OpenRoom { inner: strong });
            }
        }

        // Build a fresh per-room signaler and register it with the
        // dispatcher.
        let signaler: Rc<crate::signaling::RelaySignaler<St>> =
            crate::signaling::RelaySignaler::new(
                self.identity.clone(),
                fp.to_hex(),
                &self.store,
            );
        self.rtc_signaler_dispatcher.register(fp, signaler.clone());

        // Publish the room subscription.
        let filter = crate::filters::room_filter(&room);
        self.engine
            .publish_subscription(filter, std::time::Duration::from_secs(3600))
            .await
            .map_err(|e| crate::Error::Other(format!("publish_subscription: {e}")))?;

        // Build cancel signal up front so we can hand it to background tasks.
        let cancel = Rc::new(std::cell::Cell::new(false));

        // Spawn the subscription renewal task. Re-publishes at TTL/2.
        let engine_for_renewal = self.engine.clone();
        let room_for_renewal = room.clone();
        let cancel_for_renewal = cancel.clone();
        const SUBSCRIPTION_TTL: std::time::Duration = std::time::Duration::from_secs(3600);
        sunset_sync::spawn::spawn_local(async move {
            #[cfg(not(target_arch = "wasm32"))]
            use tokio::time::sleep;
            #[cfg(target_arch = "wasm32")]
            use wasmtimer::tokio::sleep;
            let renewal = SUBSCRIPTION_TTL / 2;
            loop {
                sleep(renewal).await;
                if cancel_for_renewal.get() {
                    return;
                }
                let f = crate::filters::room_filter(&room_for_renewal);
                if let Err(e) = engine_for_renewal
                    .publish_subscription(f, SUBSCRIPTION_TTL)
                    .await
                {
                    tracing::warn!("subscription renewal failed: {e}");
                }
            }
        });

        let state = Rc::new(open_room::RoomState {
            room,
            peer_weak: Rc::downgrade(self),
            presence_started: std::cell::Cell::new(false),
            tracker_handles: Rc::new(crate::membership::TrackerHandles::new(
                &self.relay_status.borrow(),
            )),
            signaler,
            cancel_decode: cancel,
            callbacks: Rc::new(std::cell::RefCell::new(open_room::RoomCallbacks::default())),
        });

        self.open_rooms.borrow_mut().insert(fp, Rc::downgrade(&state));
        Ok(OpenRoom { inner: state })
    }

    pub async fn add_relay(&self, addr: sunset_sync::PeerAddr) -> sunset_sync::Result<()> {
        *self.relay_status.borrow_mut() = "connecting".to_owned();
        match self.supervisor.add(addr).await {
            Ok(()) => {
                *self.relay_status.borrow_mut() = "connected".to_owned();
                Ok(())
            }
            Err(e) => {
                *self.relay_status.borrow_mut() = "error".to_owned();
                Err(e)
            }
        }
    }

    // Accessor methods consumed by Phase 5+ (open_room, send_text, etc.).
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }

    pub(crate) fn store(&self) -> &Arc<St> {
        &self.store
    }

    #[allow(dead_code)]
    pub(crate) fn engine(&self) -> &Rc<SyncEngine<St, T>> {
        &self.engine
    }

    #[allow(dead_code)]
    pub(crate) fn supervisor(&self) -> &Rc<PeerSupervisor<St, T>> {
        &self.supervisor
    }

    #[allow(dead_code)]
    pub(crate) fn relay_status_cell(&self) -> Rc<RefCell<String>> {
        self.relay_status.clone()
    }

    #[allow(dead_code)]
    pub(crate) fn open_rooms_cell(
        &self,
    ) -> &RefCell<HashMap<RoomFingerprint, Weak<open_room::RoomState<St, T>>>> {
        &self.open_rooms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ed25519Verifier;
    use sunset_store_memory::MemoryStore;

    fn ident(seed: u8) -> Identity {
        Identity::from_secret_bytes(&[seed; 32])
    }

    #[tokio::test(flavor = "current_thread")]
    async fn peer_new_exposes_public_key_and_default_relay_status() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(7)).await;
                assert_eq!(peer.public_key().len(), 32);
                assert_eq!(peer.relay_status(), "disconnected");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn add_relay_with_unreachable_addr_sets_status_error() {
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            let peer = helpers::mk_peer(ident(8)).await;
            assert_eq!(peer.relay_status(), "disconnected");

            // NopTransport's connect() returns Transport("nop") immediately,
            // so add_relay short-circuits to error and the status flips to
            // "error".
            let result = peer.add_relay(sunset_sync::PeerAddr::new(
                bytes::Bytes::from_static(b"wss://nowhere.invalid")
            )).await;
            assert!(result.is_err());
            assert_eq!(peer.relay_status(), "error");
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_text_inserts_a_text_entry() {
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            let peer = helpers::mk_peer(ident(10)).await;
            let room = peer.open_room("alpha").await.expect("open_room");

            let now_ms = 1_700_000_000_000u64;
            let value_hash = room
                .send_text("hello world".to_owned(), now_ms)
                .await
                .expect("send_text");

            // The store should now hold the content block under that hash.
            use sunset_store::Store as _;
            let block = peer
                .store()
                .get_content(&value_hash)
                .await
                .expect("get_content");
            assert!(block.is_some(), "content block missing");
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn open_room_twice_returns_same_state() {
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            let peer = helpers::mk_peer(ident(9)).await;
            let r1 = peer.open_room("alpha").await.expect("open_room r1");
            let r2 = peer.open_room("alpha").await.expect("open_room r2");
            assert_eq!(r1.fingerprint(), r2.fingerprint());
            // Internal: both handles share the same Rc<RoomState>.
            assert!(Rc::ptr_eq(&r1.inner, &r2.inner));
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_message_fires_for_self_send() {
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            let peer = helpers::mk_peer(ident(11)).await;
            let room = peer.open_room("alpha").await.expect("open_room");

            let received: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
            let received_clone = received.clone();
            room.on_message(move |decoded, is_self| {
                if let crate::MessageBody::Text(t) = &decoded.body {
                    received_clone.borrow_mut().push((t.clone(), is_self));
                }
            });

            let _ = room
                .send_text("hello self".to_owned(), 1_700_000_000_000)
                .await
                .expect("send_text");

            // Yield repeatedly so the decode loop's spawn_local runs.
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }

            let got = received.borrow().clone();
            assert_eq!(got, vec![("hello self".to_owned(), true)]);
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_receipt_fires_for_inserted_receipt() {
        use std::cell::RefCell;
        use rand_core::SeedableRng;
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            let peer = helpers::mk_peer(ident(12)).await;
            let room = peer.open_room("alpha").await.expect("open_room");

            let received: Rc<RefCell<Vec<sunset_store::Hash>>> = Rc::new(RefCell::new(Vec::new()));
            let received_clone = received.clone();
            // Register a no-op on_message so the decode loop spawns even
            // though we only care about receipts here. (The loop spawns on
            // first on_message OR on_receipt registration — either works.)
            room.on_message(|_, _| {});
            room.on_receipt(move |for_hash, _from: &crate::IdentityKey| {
                received_clone.borrow_mut().push(for_hash);
            });

            // Compose+insert a Receipt referencing some target hash.
            let target: sunset_store::Hash = blake3::hash(b"target").into();
            let mut rng = rand_chacha::ChaCha20Rng::from_seed([42; 32]);
            let composed = crate::compose_receipt(
                peer.identity(),
                &room.inner.room,
                0,
                1_700_000_000_000,
                target,
                &mut rng,
            ).expect("compose_receipt");
            use sunset_store::Store as _;
            peer.store()
                .insert(composed.entry, Some(composed.block))
                .await
                .expect("insert receipt");

            for _ in 0..50 {
                tokio::task::yield_now().await;
            }
            assert_eq!(received.borrow().clone(), vec![target]);
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn start_presence_publishes_a_heartbeat_entry() {
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            let peer = helpers::mk_peer(ident(13)).await;
            let room = peer.open_room("alpha").await.expect("open_room");
            let my_hex = hex::encode(peer.public_key());

            room.start_presence(50, 1000, 100).await;

            // Wait for the publisher's first iteration.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;

            use futures::StreamExt;
            use sunset_store::{Filter, Replay, Store as _};
            let presence_filter = Filter::NamePrefix(bytes::Bytes::from(format!(
                "{}/presence/{}",
                room.fingerprint().to_hex(),
                my_hex,
            )));
            let mut sub = peer
                .store()
                .subscribe(presence_filter, Replay::All)
                .await
                .expect("subscribe");
            let ev = tokio::time::timeout(std::time::Duration::from_millis(500), sub.next())
                .await
                .expect("no presence entry within 500ms")
                .expect("subscription closed");
            assert!(matches!(ev, Ok(sunset_store::Event::Inserted(_))));
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn renewal_loop_exits_when_cancel_set() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(14)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                // Drop the OpenRoom handle; verify cancel was set.
                let cancel = room.inner.cancel_decode.clone();
                drop(room);
                // The Drop impl on RoomState (Phase 4) fires cancel_decode = true.
                // Yield so the renewal-loop / decode-loop tasks notice (we don't
                // actually assert their termination here — just that the cancel
                // signal is set, which structurally guarantees their exit).
                tokio::task::yield_now().await;
                assert!(
                    cancel.get(),
                    "cancel_decode should be set after OpenRoom drop"
                );
            })
            .await;
    }

    pub(super) mod helpers {
        use super::*;
        use async_trait::async_trait;
        use bytes::Bytes;
        use sunset_sync::{
            BackoffPolicy, PeerId, SyncConfig, SyncEngine, Transport, TransportConnection,
            TransportKind,
        };
        use sunset_sync::types::PeerAddr;

        /// Stub transport for unit tests that don't exercise the network.
        pub(crate) struct NopTransport;

        #[async_trait(?Send)]
        impl Transport for NopTransport {
            type Connection = NopConnection;

            async fn connect(&self, _addr: PeerAddr) -> sunset_sync::Result<Self::Connection> {
                Err(sunset_sync::Error::Transport("nop".into()))
            }

            async fn accept(&self) -> sunset_sync::Result<Self::Connection> {
                std::future::pending().await
            }
        }

        pub(crate) struct NopConnection;

        #[async_trait(?Send)]
        impl TransportConnection for NopConnection {
            async fn send_reliable(&self, _bytes: Bytes) -> sunset_sync::Result<()> {
                Ok(())
            }

            async fn recv_reliable(&self) -> sunset_sync::Result<Bytes> {
                std::future::pending().await
            }

            async fn send_unreliable(&self, _bytes: Bytes) -> sunset_sync::Result<()> {
                Ok(())
            }

            async fn recv_unreliable(&self) -> sunset_sync::Result<Bytes> {
                std::future::pending().await
            }

            fn peer_id(&self) -> PeerId {
                // Unreachable in tests since connect() always errors and accept()
                // never resolves, but we need a valid impl.
                PeerId(sunset_store::VerifyingKey::new(Bytes::from(vec![0u8; 32])))
            }

            fn kind(&self) -> TransportKind {
                TransportKind::Unknown
            }

            async fn close(&self) -> sunset_sync::Result<()> {
                Ok(())
            }
        }

        pub(crate) async fn mk_peer(
            identity: Identity,
        ) -> Rc<Peer<MemoryStore, NopTransport>> {
            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let signer: Arc<dyn sunset_sync::Signer> = Arc::new(identity.clone());
            let local_peer = PeerId(identity.store_verifying_key());
            let engine = Rc::new(SyncEngine::new(
                store.clone(),
                NopTransport,
                SyncConfig::default(),
                local_peer,
                signer,
            ));
            sunset_sync::spawn::spawn_local({
                let e = engine.clone();
                async move {
                    let _ = e.run().await;
                }
            });
            let supervisor = PeerSupervisor::new(engine.clone(), BackoffPolicy::default());
            sunset_sync::spawn::spawn_local({
                let s = supervisor.clone();
                async move { s.run().await }
            });
            let dispatcher = MultiRoomSignaler::new();
            Peer::new(identity, store, engine, supervisor, dispatcher)
        }
    }
}
