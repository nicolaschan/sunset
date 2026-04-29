//! End-to-end: Alice publishes ephemeral events carrying a sender_time
//! field; Bob's decoder loop pulls them from Bus::subscribe, decodes,
//! and feeds (peer, sender_time) into Liveness. Verifies Liveness
//! fires Live for Alice through the real Bus subscription path. Same
//! scaffolding voice (Plan C2) will use, minus the encryption +
//! Opus parts.

#![cfg(feature = "test-helpers")]

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures::StreamExt as _;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};

use sunset_core::{
    Bus, BusEvent, BusImpl, HasSenderTime, Identity, Liveness, LivenessState,
};
use sunset_store::{AcceptAllVerifier, Filter};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine, TrustSet};

type TestEngine = SyncEngine<MemoryStore, sunset_sync::test_transport::TestTransport>;

/// Mimics what a real consumer (e.g. voice) would publish inside its
/// encrypted payload — a sender-claimed timestamp + opaque bytes.
#[derive(Serialize, Deserialize)]
struct TestEvent {
    sender_time_micros: u64,
    payload: Vec<u8>,
}

impl TestEvent {
    fn encode(&self) -> Bytes {
        Bytes::from(postcard::to_stdvec(self).unwrap())
    }

    fn decode(bytes: &[u8]) -> Self {
        postcard::from_bytes(bytes).unwrap()
    }
}

impl HasSenderTime for TestEvent {
    fn sender_time(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_micros(self.sender_time_micros)
    }
}

fn build(
    net: &TestNetwork,
    addr: &str,
) -> (
    BusImpl<MemoryStore, sunset_sync::test_transport::TestTransport>,
    Rc<TestEngine>,
    tokio::task::JoinHandle<()>,
    Identity,
) {
    let identity = Identity::generate(&mut OsRng);
    let local_peer = PeerId(identity.store_verifying_key());
    let store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
    let transport = net.transport(
        local_peer.clone(),
        PeerAddr::new(Bytes::copy_from_slice(addr.as_bytes())),
    );
    let engine = Rc::new(SyncEngine::new(
        store.clone(),
        transport,
        SyncConfig::default(),
        local_peer,
        Arc::new(identity.clone()) as Arc<dyn Signer>,
    ));
    let bus = BusImpl::new(store, engine.clone(), identity.clone());
    let run_handle = {
        let engine = engine.clone();
        tokio::task::spawn_local(async move {
            let _ = engine.run().await;
        })
    };
    (bus, engine, run_handle, identity)
}

#[tokio::test(flavor = "current_thread")]
async fn liveness_tracks_alice_via_bob_bus_subscription() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let (alice_bus, alice_engine, alice_run, alice_identity) = build(&net, "alice");
            let (bob_bus, alice_view_of_bob, bob_run, bob_identity) = build(&net, "bob");

            alice_engine.set_trust(TrustSet::All).await.unwrap();
            alice_view_of_bob.set_trust(TrustSet::All).await.unwrap();

            // Bob subscribes BEFORE Alice connects so the registry
            // entry is in Bob's store at bootstrap-digest time.
            let mut bob_stream = bob_bus
                .subscribe(Filter::NamePrefix(Bytes::from_static(b"liveness-test/")))
                .await
                .unwrap();

            // Connect alice → bob.
            alice_engine
                .add_peer(PeerAddr::new(Bytes::from_static(b"bob")))
                .await
                .unwrap();

            // Wait for Alice's registry to learn Bob's filter.
            let bob_vk = bob_identity.store_verifying_key();
            let propagated = async {
                loop {
                    if alice_engine.knows_peer_subscription(&bob_vk).await {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            };
            tokio::time::timeout(Duration::from_secs(2), propagated)
                .await
                .expect("alice learned bob's subscription");

            // Bob keeps a Liveness tracker (production-clock; we won't
            // exercise stale transitions here, just the Live transition).
            let liveness = Liveness::new(Duration::from_secs(3));
            let mut state_changes = liveness.subscribe().await;

            // Bob's decoder loop: pull from Bus, decode, feed Liveness.
            let liveness_for_decoder = Arc::clone(&liveness);
            tokio::task::spawn_local(async move {
                while let Some(ev) = bob_stream.next().await {
                    if let BusEvent::Ephemeral(dg) = ev {
                        let event = TestEvent::decode(&dg.payload);
                        let peer = PeerId(dg.verifying_key);
                        liveness_for_decoder.observe_event(peer, &event).await;
                    }
                }
            });

            // Alice publishes one event. The sender_time inside the
            // payload is what Liveness will record.
            let alice_claimed_time_micros = 100_000_000;
            let event = TestEvent {
                sender_time_micros: alice_claimed_time_micros,
                payload: vec![0xAA; 8],
            };
            alice_bus
                .publish_ephemeral(
                    Bytes::from_static(b"liveness-test/alice/0001"),
                    event.encode(),
                )
                .await
                .unwrap();

            // Bob's Liveness should report Live for Alice within a
            // generous window.
            let change = tokio::time::timeout(
                Duration::from_millis(500),
                state_changes.next(),
            )
            .await
            .expect("change arrived in time")
            .expect("subscriber stream open");
            assert_eq!(change.state, LivenessState::Live);
            assert_eq!(
                change.peer.0.as_bytes(),
                alice_identity.store_verifying_key().as_bytes()
            );
            assert_eq!(
                change.last_heard_at,
                SystemTime::UNIX_EPOCH + Duration::from_micros(alice_claimed_time_micros),
            );

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}
