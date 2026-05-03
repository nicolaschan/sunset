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

// Fields used by Phase 5+ methods (open_room, send_text, etc.).
#[allow(dead_code)]
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
    #[allow(dead_code)]
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }

    #[allow(dead_code)]
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
