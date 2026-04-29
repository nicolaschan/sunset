//! Pub/sub abstraction over both durable (CRDT-replicated) and
//! ephemeral (real-time, fire-and-forget) message delivery. Same
//! filter system, same signing model; different persistence + transport.
//!
//! See `docs/superpowers/specs/2026-04-28-sunset-bus-pubsub-design.md`
//! for the architecture.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::LocalBoxStream;

use sunset_store::{ContentBlock, Filter, Replay, SignedDatagram, SignedKvEntry};

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
        use futures::stream::StreamExt as _;

        // Publish our subscription so peers learn what we want. TTL is
        // 1 hour; consumers that need a different lifetime can call
        // engine.publish_subscription directly.
        self.engine
            .publish_subscription(filter.clone(), std::time::Duration::from_secs(3600))
            .await
            .map_err(|e| crate::Error::Sync(format!("{e}")))?;

        // Ephemeral side: in-process dispatch from the engine.
        let ephemeral_rx = self.engine.subscribe_ephemeral(filter.clone()).await;

        // Durable side: open the store subscription inside an
        // async_stream so the owned `Arc<S>` keeps the substream
        // alive for `'static`. The Store trait's subscribe borrows
        // `&'a self`; wrapping it lets us hand back a 'static stream.
        let store_for_subscribe = self.store.clone();
        let store_for_block_fetch = self.store.clone();
        let durable_filter = filter;
        let durable_mapped = async_stream::stream! {
            let mut substream = match store_for_subscribe
                .subscribe(durable_filter, Replay::All)
                .await
            {
                Ok(s) => s,
                Err(_) => return,
            };
            while let Some(ev) = substream.next().await {
                let entry = match ev {
                    Ok(sunset_store::Event::Inserted(e)) => e,
                    Ok(sunset_store::Event::Replaced { new, .. }) => new,
                    // Expired / BlobAdded / BlobRemoved are not
                    // application-relevant for the bus.
                    Ok(_) => continue,
                    Err(_) => continue,
                };
                // Lazily fetch the block. None if not yet local
                // (dangling-ref allowed per store contract).
                let block = store_for_block_fetch
                    .get_content(&entry.value_hash)
                    .await
                    .ok()
                    .flatten();
                yield BusEvent::Durable { entry, block };
            }
        };

        let ephemeral_mapped = tokio_stream::wrappers::UnboundedReceiverStream::new(ephemeral_rx)
            .map(BusEvent::Ephemeral);

        let merged = futures::stream::select(Box::pin(durable_mapped), ephemeral_mapped);
        Ok(Box::pin(merged))
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
        tokio::task::JoinHandle<()>,
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
        let bus = BusImpl::new(store, engine.clone(), identity.clone());
        // bus.subscribe calls engine.publish_subscription, which sends a
        // command to the engine's run loop and awaits a oneshot ack —
        // it deadlocks unless run() is driving the loop. Spawn run()
        // here so all bus-level tests get a working engine for free;
        // tests should `.abort()` the handle in their cleanup.
        let run_handle = tokio::task::spawn_local(async move {
            let _ = engine.run().await;
        });
        (bus, identity, run_handle)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publish_ephemeral_loopback_via_engine() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (bus, _identity, run_handle) = make_bus();
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
                run_handle.abort();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subscribe_merges_durable_and_ephemeral() {
        use bytes::Bytes;
        use sunset_store::{ContentBlock, SignedKvEntry, canonical::signing_payload};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (bus, identity, run_handle) = make_bus();
                let mut stream = bus
                    .subscribe(Filter::NamePrefix(Bytes::from_static(b"chat/")))
                    .await
                    .unwrap();

                // Publish a durable entry under chat/ — should arrive as
                // Durable on the merged stream.
                let block = ContentBlock {
                    data: Bytes::from_static(b"hello"),
                    references: vec![],
                };
                let value_hash = block.hash();
                let mut entry = SignedKvEntry {
                    verifying_key: identity.store_verifying_key(),
                    name: Bytes::from_static(b"chat/me/abc"),
                    value_hash,
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                let sig = identity.sign(&signing_payload(&entry));
                entry.signature = Bytes::copy_from_slice(&sig.to_bytes());

                bus.publish_durable(entry, Some(block.clone()))
                    .await
                    .unwrap();

                // Publish an ephemeral on chat/ — should arrive as Ephemeral.
                bus.publish_ephemeral(
                    Bytes::from_static(b"chat/me/eph"),
                    Bytes::from_static(b"now"),
                )
                .await
                .unwrap();

                // Read first two events from the merged stream. Order
                // is unspecified; assert the SET of (kind, name) pairs.
                use futures::StreamExt as _;
                let mut got = Vec::new();
                for _ in 0..2 {
                    let ev =
                        tokio::time::timeout(std::time::Duration::from_millis(200), stream.next())
                            .await
                            .expect("event arrived")
                            .expect("stream open");
                    got.push(match ev {
                        BusEvent::Durable { entry, .. } => ("durable", entry.name.to_vec()),
                        BusEvent::Ephemeral(d) => ("ephemeral", d.name.to_vec()),
                    });
                }
                got.sort();
                assert_eq!(
                    got,
                    vec![
                        ("durable", b"chat/me/abc".to_vec()),
                        ("ephemeral", b"chat/me/eph".to_vec()),
                    ],
                );
                run_handle.abort();
            })
            .await;
    }
}
