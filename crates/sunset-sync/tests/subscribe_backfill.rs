//! Regression test for the subscribe-triggered backfill race.
//!
//! Scenario: alice has an entry E matching filter F. Some time later,
//! bob's `SUBSCRIBE_NAME` for filter F lands in alice's store — but
//! arrives via a path that does NOT also trigger a publisher-side
//! digest (no `publish_subscription` call from bob this session, and
//! no `SUBSCRIBE_NAME` in bob's own store at PeerHello time, so the
//! `fan_out_digests_to_peer` own-filter loop is empty).
//!
//! Without subscribe-triggered backfill, alice's `handle_local_store_event`
//! fires once for E (when E was written, before bob was in the registry),
//! and once for the `SUBSCRIBE_NAME` (registry updated, but no re-eval
//! of E). E is stranded in alice's store until anti-entropy — which is
//! also disabled here because neither side has any `own_published_filters`.
//!
//! With the engine fix, the registry-update is itself a forwarding
//! trigger and alice pushes E to bob.
//!
//! Note: the test injects bob's `SUBSCRIBE_NAME` directly into alice's
//! store via `Store::insert` rather than over the wire. The engine's
//! response to a `SUBSCRIBE_NAME` landing in its local store is
//! identical regardless of how it landed (network EventDelivery vs.
//! direct store insert from a federated source vs. test injection),
//! so this is a legitimate integration-level driver for the
//! `handle_local_store_event` path under test.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use sunset_store::{ContentBlock, Filter, SignedKvEntry, Store as _, VerifyingKey};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};

const SUBSCRIBE_NAME: &[u8] = b"_sunset-sync/subscribe";

fn vk(b: &[u8]) -> VerifyingKey {
    VerifyingKey::new(Bytes::copy_from_slice(b))
}

struct StubSigner {
    vk: VerifyingKey,
}

impl Signer for StubSigner {
    fn verifying_key(&self) -> VerifyingKey {
        self.vk.clone()
    }
    fn sign(&self, _payload: &[u8]) -> Bytes {
        Bytes::from_static(&[0u8; 64])
    }
}

#[tokio::test(flavor = "current_thread")]
async fn registry_update_backfills_already_stored_entries() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice_addr = PeerAddr::new("alice");
            let bob_addr = PeerAddr::new("bob");
            let alice_id = PeerId(vk(b"alice"));
            let bob_id = PeerId(vk(b"bob"));

            let alice_transport = net.transport(alice_id.clone(), alice_addr.clone());
            let bob_transport = net.transport(bob_id.clone(), bob_addr.clone());

            let alice_store = Arc::new(MemoryStore::with_accept_all());
            let bob_store = Arc::new(MemoryStore::with_accept_all());

            let alice_signer = Arc::new(StubSigner { vk: alice_id.0.clone() });
            let bob_signer = Arc::new(StubSigner { vk: bob_id.0.clone() });

            let alice_engine = Rc::new(SyncEngine::new(
                alice_store.clone(),
                alice_transport,
                SyncConfig::default(),
                alice_id.clone(),
                alice_signer,
            ));
            let bob_engine = Rc::new(SyncEngine::new(
                bob_store.clone(),
                bob_transport,
                SyncConfig::default(),
                bob_id.clone(),
                bob_signer,
            ));

            let _alice_run = tokio::task::spawn_local({
                let e = alice_engine.clone();
                async move { e.run().await }
            });
            let _bob_run = tokio::task::spawn_local({
                let e = bob_engine.clone();
                async move { e.run().await }
            });

            // Connect alice -> bob. PeerHello completes; both peer_outbounds
            // populated. Bob's own store has no SUBSCRIBE_NAME, so bob's
            // own_published_filters is empty; PeerHello fan-out exchanges
            // only the bootstrap digest, which finds no SUBSCRIBE_NAME
            // entries on either side.
            alice_engine.add_peer(bob_addr.clone()).await.unwrap();

            // Alice writes E. Alice's handle_local_store_event(E) fires;
            // alice's registry has no entry for bob; peers_matching is
            // empty; E is NOT forwarded.
            let block = ContentBlock {
                data: Bytes::from_static(b"hello-bob"),
                references: vec![],
            };
            let entry = SignedKvEntry {
                verifying_key: vk(b"chat"),
                name: Bytes::from_static(b"k"),
                value_hash: block.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store
                .insert(entry.clone(), Some(block.clone()))
                .await
                .unwrap();

            // Inject bob's SUBSCRIBE_NAME directly into alice's store.
            // Alice's handle_local_store_event(SUBSCRIBE_NAME) fires;
            // registry is updated for bob with filter F = Keyspace(chat).
            // Without backfill, no further trigger fires for E. With
            // backfill, alice walks her store for F, finds E, and pushes
            // EventDelivery(E) to bob.
            let bob_filter = Filter::Keyspace(vk(b"chat"));
            let bob_filter_bytes = postcard::to_stdvec(&bob_filter).unwrap();
            let bob_sub_block = ContentBlock {
                data: Bytes::from(bob_filter_bytes),
                references: vec![],
            };
            let bob_sub_entry = SignedKvEntry {
                verifying_key: bob_id.0.clone(),
                name: Bytes::from_static(SUBSCRIBE_NAME),
                value_hash: bob_sub_block.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::from_static(&[0u8; 64]),
            };
            alice_store
                .insert(bob_sub_entry.clone(), Some(bob_sub_block.clone()))
                .await
                .unwrap();

            // Bob should receive E within a short bound. The 2-second
            // bound is well below the default anti-entropy interval
            // (which would otherwise close the gap eventually); the test
            // is sensitive to the receiver-side trigger specifically.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            let mut received = false;
            while tokio::time::Instant::now() < deadline {
                if bob_store
                    .get_entry(&vk(b"chat"), b"k")
                    .await
                    .unwrap()
                    .is_some()
                {
                    received = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(
                received,
                "bob did not receive E after alice's registry learned bob's filter"
            );
        })
        .await;
}
