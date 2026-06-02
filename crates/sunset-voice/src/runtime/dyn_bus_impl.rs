//! Blanket `impl DynBus for BusImpl<S, T>`.
//!
//! Lives here (in `sunset-voice`) rather than in `sunset-core` to avoid a
//! dependency cycle: `sunset-voice` already depends on `sunset-core`, so
//! `sunset-voice` can see both `DynBus` (defined in this crate) and
//! `BusImpl` (from `sunset-core`). The inverse direction would be cyclic.

use bytes::Bytes;
use futures::stream::LocalBoxStream;
use tokio::sync::mpsc;

use sunset_core::bus::{
    Bus, BusEvent, BusImpl, EngineEvent, PeerId, SignedDatagram, SubscriptionPolicy, TransportKind,
};
use sunset_store::{ContentBlock, Filter, SignedKvEntry, Store};
use sunset_sync::Transport;

use super::DynBus;

#[async_trait::async_trait(?Send)]
impl<S, T> DynBus for BusImpl<S, T>
where
    S: Store + 'static,
    T: Transport + 'static,
    T::Connection: 'static,
{
    async fn publish_ephemeral(
        &self,
        name: Bytes,
        seq: u64,
        payload: Bytes,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Bus::publish_ephemeral(self, name, seq, payload)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }

    async fn publish_durable(
        &self,
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Bus::publish_durable(self, entry, block)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }

    async fn subscribe_voice_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, BusEvent>, Box<dyn std::error::Error>> {
        Bus::subscribe(self, Filter::NamePrefix(prefix))
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }

    async fn subscribe_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, BusEvent>, Box<dyn std::error::Error>> {
        Bus::subscribe(self, Filter::NamePrefix(prefix))
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }

    async fn subscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
        policy: SubscriptionPolicy,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Bus::subscribe_via(self, filter, provider, policy)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }

    async fn unsubscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Bus::unsubscribe_via(self, filter, provider)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    }

    async fn current_peers(&self) -> Vec<(PeerId, TransportKind)> {
        Bus::current_peers(self).await
    }

    async fn subscribe_engine_events(&self) -> mpsc::UnboundedReceiver<EngineEvent> {
        Bus::subscribe_engine_events(self).await
    }

    async fn subscribe_ephemeral_local(
        &self,
        filter: Filter,
    ) -> mpsc::UnboundedReceiver<SignedDatagram> {
        Bus::subscribe_ephemeral_local(self, filter).await
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use bytes::Bytes;
    use futures::StreamExt;

    use sunset_core::bus::{BusEvent, BusImpl};
    use sunset_core::identity::Identity;
    use sunset_store::AcceptAllVerifier;
    use sunset_store_memory::MemoryStore;
    use sunset_sync::test_transport::{TestNetwork, TestTransport};
    use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};

    use crate::runtime::DynBus;

    fn make_bus_impl() -> (
        BusImpl<MemoryStore, TestTransport>,
        Identity,
        Rc<SyncEngine<MemoryStore, TestTransport>>,
    ) {
        let net = TestNetwork::new();
        let identity = Identity::generate(&mut rand_core::OsRng);
        let local_peer = PeerId(identity.store_verifying_key());
        let store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
        let transport = net.transport(local_peer.clone(), PeerAddr::new("self"));
        let engine = Rc::new(SyncEngine::new(
            store.clone(),
            transport,
            SyncConfig::default(),
            local_peer,
            Arc::new(identity.clone()) as Arc<dyn Signer>,
        ));
        let bus = BusImpl::new(store, engine.clone(), identity.clone());
        (bus, identity, engine)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bus_impl_implements_dyn_bus() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (bus, _identity, engine) = make_bus_impl();

                // Spawn the engine run loop so subscribe/publish don't deadlock.
                let run_handle = tokio::task::spawn_local(async move {
                    let _ = engine.run().await;
                });

                // Upcast to Rc<dyn DynBus> — this is the key test.
                let dyn_bus: Rc<dyn DynBus> = Rc::new(bus);

                // subscribe_voice_prefix
                let prefix = Bytes::from_static(b"voice/test/");
                let mut stream = dyn_bus
                    .subscribe_voice_prefix(prefix)
                    .await
                    .expect("subscribe succeeded");

                // publish_ephemeral loopback
                dyn_bus
                    .publish_ephemeral(
                        Bytes::from_static(b"voice/test/peer1"),
                        0,
                        Bytes::from_static(b"payload"),
                    )
                    .await
                    .expect("publish succeeded");

                // Verify the loopback event arrives.
                let ev = tokio::time::timeout(std::time::Duration::from_millis(200), stream.next())
                    .await
                    .expect("event arrived within timeout")
                    .expect("stream open");

                match ev {
                    BusEvent::Ephemeral(d) => {
                        assert_eq!(&d.name, &Bytes::from_static(b"voice/test/peer1"));
                    }
                    other => panic!("expected Ephemeral, got {other:?}"),
                }

                // Routing/observation seam forwards through the trait object.
                use sunset_core::bus::{PeerId, SubscriptionPolicy, TransportKind};
                use sunset_store::Filter;

                let peers: Vec<(PeerId, TransportKind)> = dyn_bus.current_peers().await;
                assert!(peers.is_empty(), "no peers in this single-engine harness");

                let other = PeerId(
                    sunset_core::identity::Identity::generate(&mut rand_core::OsRng)
                        .store_verifying_key(),
                );
                dyn_bus
                    .subscribe_via(
                        Filter::NamePrefix(Bytes::from_static(b"voice/")),
                        other.clone(),
                        SubscriptionPolicy::store_data(),
                    )
                    .await
                    .expect("subscribe_via forwards");
                dyn_bus
                    .unsubscribe_via(Filter::NamePrefix(Bytes::from_static(b"voice/")), other)
                    .await
                    .expect("unsubscribe_via forwards");

                let _events = dyn_bus.subscribe_engine_events().await;
                let _eph = dyn_bus
                    .subscribe_ephemeral_local(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                    .await;

                run_handle.abort();
            })
            .await;
    }
}
