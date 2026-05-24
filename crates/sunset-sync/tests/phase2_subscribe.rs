//! Integration tests for cooperative-relay Phase 2: subscribe / subscribe_via.
//!
//! Each test sets up two SyncEngines over `TestNetwork`; the receiver calls
//! one of the new APIs; the provider writes (before or after subscribe) and
//! the receiver checks its local store for the data to appear.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use sunset_store::{ContentBlock, Filter, SignedKvEntry, Store as _, VerifyingKey};
use sunset_store_memory::MemoryStore;
use sunset_sync::routing::SubscriptionPolicy;
use sunset_sync::test_helpers::wait_for;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};

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
async fn subscribe_via_backfills_existing_entry() {
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

            let alice_signer = Arc::new(StubSigner {
                vk: alice_id.0.clone(),
            });
            let bob_signer = Arc::new(StubSigner {
                vk: bob_id.0.clone(),
            });

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

            let alice_run = tokio::task::spawn_local({
                let e = alice_engine.clone();
                async move { e.run().await }
            });
            let bob_run = tokio::task::spawn_local({
                let e = bob_engine.clone();
                async move { e.run().await }
            });

            // Alice (provider) writes an entry BEFORE Bob subscribes.
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
                .insert(entry.clone(), Some(block))
                .await
                .unwrap();

            // Bob connects to Alice.
            bob_engine.add_peer(alice_addr).await.unwrap();

            // Bob subscribes via Alice for the `chat` keyspace. Alice's
            // SUBSCRIBE_PREFIX handler should populate
            // peer_sessions[bob].interests + fire DigestRequest, which
            // backfills the existing entry to Bob.
            bob_engine
                .subscribe_via(
                    Filter::Keyspace(vk(b"chat")),
                    alice_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            let received = wait_for(
                Duration::from_secs(2),
                Duration::from_millis(20),
                || async {
                    bob_store
                        .get_entry(&vk(b"chat"), b"k")
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(
                received,
                "bob did not receive alice's pre-existing entry via subscribe_via backfill"
            );

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn subscribe_before_peer_connect_then_data_flows() {
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

            let alice_signer = Arc::new(StubSigner {
                vk: alice_id.0.clone(),
            });
            let bob_signer = Arc::new(StubSigner {
                vk: bob_id.0.clone(),
            });

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

            let alice_run = tokio::task::spawn_local({
                let e = alice_engine.clone();
                async move { e.run().await }
            });
            let bob_run = tokio::task::spawn_local({
                let e = bob_engine.clone();
                async move { e.run().await }
            });

            // Bob subscribes broadcast-style BEFORE connecting to anyone.
            // This records a BroadcastIntent but my_subs stays empty
            // (no connected peers yet).
            bob_engine
                .subscribe(
                    Filter::Keyspace(vk(b"chat")),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Now Bob connects to Alice. The auto-resubscriber hook on
            // PeerHello should replay Bob's BroadcastIntent as
            // subscribe_via(filter, alice, policy).
            bob_engine.add_peer(alice_addr).await.unwrap();

            // Alice writes a matching entry AFTER Bob is connected.
            let block = ContentBlock {
                data: Bytes::from_static(b"hello-bob-broadcast"),
                references: vec![],
            };
            let entry = SignedKvEntry {
                verifying_key: vk(b"chat"),
                name: Bytes::from_static(b"msg"),
                value_hash: block.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store
                .insert(entry.clone(), Some(block))
                .await
                .unwrap();

            let received = wait_for(
                Duration::from_secs(2),
                Duration::from_millis(20),
                || async {
                    bob_store
                        .get_entry(&vk(b"chat"), b"msg")
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(
                received,
                "bob did not receive alice's entry via the auto-resubscriber path"
            );

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}
