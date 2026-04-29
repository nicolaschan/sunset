//! Pub/sub abstraction over both durable (CRDT-replicated) and
//! ephemeral (real-time, fire-and-forget) message delivery. Same
//! filter system, same signing model; different persistence + transport.
//!
//! See `docs/superpowers/specs/2026-04-28-sunset-bus-pubsub-design.md`
//! for the architecture.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::LocalBoxStream;

use sunset_store::{ContentBlock, Filter, SignedDatagram, SignedKvEntry};

use crate::error::Result;

/// A message delivered to a Bus subscriber. Tagged by delivery mode
/// so consumers can act differently (e.g. voice consumes Ephemeral,
/// chat consumes Durable).
#[derive(Clone, Debug)]
pub enum BusEvent {
    Durable {
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    },
    Ephemeral(SignedDatagram),
}

/// Unified pub/sub interface. `publish_durable` writes a signed KV
/// entry to the local store and lets the engine fan out via CRDT
/// replication. `publish_ephemeral` signs the payload, hands it to
/// the engine for unreliable fan-out, and dispatches a loopback copy
/// to local subscribers. `subscribe` opens a single stream that
/// merges both delivery modes.
#[async_trait(?Send)]
pub trait Bus {
    async fn publish_durable(
        &self,
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    ) -> Result<()>;

    async fn publish_ephemeral(&self, name: Bytes, payload: Bytes) -> Result<()>;

    async fn subscribe(&self, filter: Filter) -> Result<LocalBoxStream<'static, BusEvent>>;
}

use std::rc::Rc;
use std::sync::Arc;

use sunset_store::{Store, canonical::datagram_signing_payload};
use sunset_sync::{SyncEngine, Transport};

use crate::identity::Identity;

/// Concrete `Bus` impl wrapping the engine + store + identity.
/// Generic over the same `Store` and `Transport` types the engine
/// uses. Cheap to clone (Rc + Arc internally).
#[derive(Clone)]
pub struct BusImpl<S: Store + 'static, T: Transport + 'static>
where
    T::Connection: 'static,
{
    store: Arc<S>,
    engine: Rc<SyncEngine<S, T>>,
    identity: Identity,
}

impl<S: Store + 'static, T: Transport + 'static> BusImpl<S, T>
where
    T::Connection: 'static,
{
    pub fn new(store: Arc<S>, engine: Rc<SyncEngine<S, T>>, identity: Identity) -> Self {
        Self {
            store,
            engine,
            identity,
        }
    }
}

#[async_trait(?Send)]
impl<S: Store + 'static, T: Transport + 'static> Bus for BusImpl<S, T>
where
    T::Connection: 'static,
{
    async fn publish_durable(
        &self,
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    ) -> Result<()> {
        self.store
            .insert(entry, block)
            .await
            .map_err(|e| crate::Error::Store(format!("{e}")))
    }

    async fn publish_ephemeral(&self, name: Bytes, payload: Bytes) -> Result<()> {
        // Build the unsigned shape, sign the canonical bytes, and
        // assemble the final SignedDatagram.
        let unsigned = SignedDatagram {
            verifying_key: self.identity.store_verifying_key(),
            name: name.clone(),
            payload: payload.clone(),
            signature: Bytes::new(),
        };
        let payload_bytes = datagram_signing_payload(&unsigned);
        let signature = Bytes::copy_from_slice(&self.identity.sign(&payload_bytes).to_bytes());
        let datagram = SignedDatagram {
            verifying_key: unsigned.verifying_key,
            name: unsigned.name,
            payload: unsigned.payload,
            signature,
        };
        self.engine
            .publish_ephemeral(datagram)
            .await
            .map_err(|e| crate::Error::Sync(format!("{e}")))
    }

    async fn subscribe(&self, filter: Filter) -> Result<LocalBoxStream<'static, BusEvent>> {
        // Implementation in Task 10.
        let _ = filter;
        unimplemented!("subscribe lands in Task 10")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use sunset_store::{AcceptAllVerifier, Filter};
    use sunset_store_memory::MemoryStore;
    use sunset_sync::test_transport::TestNetwork;
    use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};

    use crate::identity::Identity;
    use rand_core::OsRng;

    fn make_bus() -> (
        BusImpl<MemoryStore, sunset_sync::test_transport::TestTransport>,
        Identity,
    ) {
        let net = TestNetwork::new();
        let identity = Identity::generate(&mut OsRng);
        let local_peer = PeerId(identity.store_verifying_key());
        let store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
        let transport = net.transport(local_peer.clone(), PeerAddr::new("self"));
        let engine = std::rc::Rc::new(SyncEngine::new(
            store.clone(),
            transport,
            SyncConfig::default(),
            local_peer,
            Arc::new(identity.clone()) as Arc<dyn Signer>,
        ));
        let bus = BusImpl::new(store, engine, identity.clone());
        (bus, identity)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publish_ephemeral_loopback_via_engine() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (bus, _identity) = make_bus();
                let mut sub = bus
                    .engine
                    .subscribe_ephemeral(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                    .await;
                bus.publish_ephemeral(
                    Bytes::from_static(b"voice/me/0001"),
                    Bytes::from_static(b"frame"),
                )
                .await
                .unwrap();
                let got = tokio::time::timeout(Duration::from_millis(50), sub.recv())
                    .await
                    .expect("loopback fired in time")
                    .expect("subscription open");
                assert_eq!(&got.name, &Bytes::from_static(b"voice/me/0001"));
                assert_eq!(&got.payload, &Bytes::from_static(b"frame"));
            })
            .await;
    }
}
