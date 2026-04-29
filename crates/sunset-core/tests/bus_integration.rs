//! End-to-end Bus test: two engines connected via TestTransport, one
//! publishes ephemeral via the Bus surface, the other receives via
//! `Bus::subscribe`. Exercises the full stack end-to-end:
//!
//! `BusImpl::publish_ephemeral` → `engine.publish_ephemeral` →
//! `SyncMessage::EphemeralDelivery` → unreliable channel →
//! remote `recv` loop → `handle_ephemeral_delivery` (verify signature) →
//! `dispatch_ephemeral_local` → merged `BusImpl::subscribe` stream →
//! `BusEvent::Ephemeral`.
//!
//! Modelled on `sunset-sync`'s `ephemeral_two_peer.rs`; the only
//! deltas are that publishing/subscribing go through `BusImpl` instead
//! of being driven against the engine directly.

#![cfg(feature = "test-helpers")]

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt as _;
use rand_core::OsRng;

use sunset_core::{Bus, BusEvent, BusImpl, Identity};
use sunset_store::{AcceptAllVerifier, Filter};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine, TrustSet};

type TestEngine = SyncEngine<MemoryStore, sunset_sync::test_transport::TestTransport>;

/// Build a bus + engine wired into `net` at the transport address `addr`.
/// Spawns `engine.run()` so subsequent engine commands (set_trust,
/// add_peer, publish_subscription) actually make progress instead of
/// deadlocking on the engine's command channel. The returned
/// `JoinHandle` is the caller's responsibility to abort during cleanup.
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
    // PeerAddr::new takes `impl Into<Bytes>`; `&str` is not `'static`
    // here, so go through `Bytes::copy_from_slice`.
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
async fn ephemeral_publish_arrives_at_remote_subscriber() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let (alice_bus, alice_engine, alice_run, _alice_identity) = build(&net, "alice");
            let (bob_bus, alice_view_of_bob, bob_run, bob_identity) = build(&net, "bob");

            // Trust everyone in the test. Must come AFTER engine.run()
            // is spawned (set_trust round-trips through the engine's
            // command channel).
            alice_engine.set_trust(TrustSet::All).await.unwrap();
            alice_view_of_bob.set_trust(TrustSet::All).await.unwrap();

            // Bob subscribes to voice/ via the Bus surface FIRST so the
            // subscription registry entry is in Bob's store before
            // Alice connects. After Alice's PeerHello, the bootstrap
            // digest exchange will include Bob's filter, so Alice will
            // pull it during the initial round.
            let mut bob_stream = bob_bus
                .subscribe(Filter::NamePrefix(Bytes::from_static(b"voice/")))
                .await
                .unwrap();

            // Connect alice → bob (triggers PeerHello + bootstrap digest).
            alice_engine
                .add_peer(PeerAddr::new(Bytes::from_static(b"bob")))
                .await
                .unwrap();

            // Wait for Alice's registry to learn Bob's filter. Poll
            // `knows_peer_subscription` so we don't depend on a flat
            // sleep — flaky on slow CI.
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

            // Alice publishes an ephemeral via the Bus. The Bus signs
            // the canonical payload with Alice's identity and hands it
            // to the engine for fan-out.
            alice_bus
                .publish_ephemeral(
                    Bytes::from_static(b"voice/alice/0001"),
                    Bytes::from_static(b"opus-frame"),
                )
                .await
                .unwrap();

            // Bob's merged stream should yield an Ephemeral within a
            // generous window (the unreliable channel hop is in-process
            // for TestTransport, so 500 ms is plenty).
            let ev = tokio::time::timeout(Duration::from_millis(500), bob_stream.next())
                .await
                .expect("event arrived in time")
                .expect("stream open");
            match ev {
                BusEvent::Ephemeral(d) => {
                    assert_eq!(&d.name, &Bytes::from_static(b"voice/alice/0001"));
                    assert_eq!(&d.payload, &Bytes::from_static(b"opus-frame"));
                }
                other => panic!("expected BusEvent::Ephemeral, got {other:?}"),
            }

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}
