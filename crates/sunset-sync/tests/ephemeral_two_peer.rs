//! End-to-end ephemeral delivery between two real engines connected
//! via TestTransport. Verifies the wire path: subscriber publishes
//! filter → publisher's engine routes EphemeralDelivery via
//! unreliable channel → subscriber's engine verifies signature +
//! dispatches to local subscribe_ephemeral receiver.

#![cfg(feature = "test-helpers")]

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ed25519_dalek::{Signer as _, SigningKey};

use sunset_store::{
    AcceptAllVerifier, Filter, SignedDatagram, VerifyingKey, canonical::datagram_signing_payload,
};
use sunset_store_memory::MemoryStore;
use sunset_sync::routing::SubscriptionPolicy;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine, TrustSet};

/// Test signer: stub Ed25519 signer using a fixed seed so the test
/// is deterministic.
struct StubSigner {
    key: SigningKey,
}

impl StubSigner {
    fn new(seed: [u8; 32]) -> Self {
        Self {
            key: SigningKey::from_bytes(&seed),
        }
    }
    fn vk(&self) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(self.key.verifying_key().as_bytes()))
    }
    fn sign_payload(&self, payload: &[u8]) -> Bytes {
        let sig = self.key.sign(payload);
        Bytes::copy_from_slice(&sig.to_bytes())
    }
}

impl Signer for StubSigner {
    fn verifying_key(&self) -> VerifyingKey {
        self.vk()
    }
    fn sign(&self, payload: &[u8]) -> Bytes {
        self.sign_payload(payload)
    }
}

fn build_engine(
    net: &TestNetwork,
    seed: [u8; 32],
    addr: &str,
) -> (
    Rc<SyncEngine<MemoryStore, sunset_sync::test_transport::TestTransport>>,
    Arc<MemoryStore>,
    Arc<StubSigner>,
) {
    let signer = Arc::new(StubSigner::new(seed));
    let local_peer = PeerId(signer.vk());
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
        signer.clone() as Arc<dyn Signer>,
    ));
    (engine, store, signer)
}

#[tokio::test(flavor = "current_thread")]
async fn ephemeral_routes_subscriber_match() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let (alice, _alice_store, alice_signer) = build_engine(&net, [1u8; 32], "alice");
            let (bob, _bob_store, bob_signer) = build_engine(&net, [2u8; 32], "bob");

            // Run both engines first — public APIs like set_trust /
            // add_peer go through the engine's command channel and won't
            // make progress until run() is being polled.
            let alice_run = {
                let alice = alice.clone();
                tokio::task::spawn_local(async move { alice.run().await })
            };
            let bob_run = {
                let bob = bob.clone();
                tokio::task::spawn_local(async move { bob.run().await })
            };

            // Trust everyone in the test.
            alice.set_trust(TrustSet::All).await.unwrap();
            bob.set_trust(TrustSet::All).await.unwrap();

            // Bob subscribes to voice/ FIRST so the per-(filter,
            // provider=alice) SubscriptionEntry is in Bob's store before
            // alice connects. After PeerHello, sync replicates that
            // entry to alice; alice's engine arms the forward path for
            // (bob, voice_filter).
            let voice_filter = Filter::NamePrefix(Bytes::from_static(b"voice/"));
            bob.subscribe(voice_filter.clone(), SubscriptionPolicy::store_data())
                .await
                .unwrap();
            let mut bob_sub = bob.subscribe_ephemeral(voice_filter.clone()).await;

            // Connect alice → bob (triggers PeerHello + bootstrap digest).
            alice
                .add_peer(PeerAddr::new(Bytes::from_static(b"bob")))
                .await
                .unwrap();

            let bob_pid = PeerId(bob_signer.vk());
            assert!(
                alice
                    .wait_for_peer_interest(&bob_pid, &voice_filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's interest"
            );

            // Alice publishes a signed ephemeral datagram on voice/.
            let name = Bytes::from_static(b"voice/alice/0001");
            let payload = Bytes::from_static(b"opus-frame-bytes");
            let unsigned = SignedDatagram {
                verifying_key: alice_signer.vk(),
                name: name.clone(),
                payload: payload.clone(),
                seq: 0,
                signature: Bytes::new(),
            };
            let sig = alice_signer.sign_payload(&datagram_signing_payload(&unsigned));
            let datagram = SignedDatagram {
                verifying_key: alice_signer.vk(),
                name,
                payload,
                seq: 0,
                signature: sig,
            };
            alice.publish_ephemeral(datagram.clone()).await.unwrap();

            // Bob's subscriber should receive within a reasonable window.
            let got = tokio::time::timeout(Duration::from_millis(500), bob_sub.recv())
                .await
                .expect("ephemeral arrived in time")
                .expect("subscription open");
            assert_eq!(got, datagram);

            // Cleanup.
            alice_run.abort();
            bob_run.abort();
        })
        .await;
}
