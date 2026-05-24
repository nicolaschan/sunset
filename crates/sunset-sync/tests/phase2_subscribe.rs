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

#[tokio::test(flavor = "current_thread")]
async fn unsubscribe_stops_forwarding() {
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

            bob_engine.add_peer(alice_addr).await.unwrap();
            let filter = Filter::Keyspace(vk(b"chat"));
            bob_engine
                .subscribe_via(
                    filter.clone(),
                    alice_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // First write — Bob should see it.
            let block_y = ContentBlock {
                data: Bytes::from_static(b"y"),
                references: vec![],
            };
            let entry_y = SignedKvEntry {
                verifying_key: vk(b"chat"),
                name: Bytes::from_static(b"y"),
                value_hash: block_y.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store.insert(entry_y, Some(block_y)).await.unwrap();

            assert!(
                wait_for(
                    Duration::from_secs(2),
                    Duration::from_millis(20),
                    || async {
                        bob_store
                            .get_entry(&vk(b"chat"), b"y")
                            .await
                            .unwrap()
                            .is_some()
                    }
                )
                .await,
                "bob should receive y while subscribed",
            );

            // Unsubscribe.
            bob_engine
                .unsubscribe_via(filter.clone(), alice_id.clone())
                .await
                .unwrap();

            // Give the Withdrawn entry time to propagate from Bob to Alice and
            // for Alice's SUBSCRIBE_PREFIX branch to remove the interest.
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Second write — Bob should NOT see it.
            let block_z = ContentBlock {
                data: Bytes::from_static(b"z"),
                references: vec![],
            };
            let entry_z = SignedKvEntry {
                verifying_key: vk(b"chat"),
                name: Bytes::from_static(b"z"),
                value_hash: block_z.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store.insert(entry_z, Some(block_z)).await.unwrap();

            // Wait a window long enough that any forward would have delivered.
            let leaked = wait_for(
                Duration::from_millis(500),
                Duration::from_millis(20),
                || async {
                    bob_store
                        .get_entry(&vk(b"chat"), b"z")
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(!leaked, "bob should NOT receive z after unsubscribing");

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn two_receivers_one_provider_each_sees_match() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice_addr = PeerAddr::new("alice");
            let bob_addr = PeerAddr::new("bob");
            let carol_addr = PeerAddr::new("carol");
            let alice_id = PeerId(vk(b"alice"));
            let bob_id = PeerId(vk(b"bob"));
            let carol_id = PeerId(vk(b"carol"));

            let alice_transport = net.transport(alice_id.clone(), alice_addr.clone());
            let bob_transport = net.transport(bob_id.clone(), bob_addr.clone());
            let carol_transport = net.transport(carol_id.clone(), carol_addr.clone());

            let alice_store = Arc::new(MemoryStore::with_accept_all());
            let bob_store = Arc::new(MemoryStore::with_accept_all());
            let carol_store = Arc::new(MemoryStore::with_accept_all());

            let alice_signer = Arc::new(StubSigner {
                vk: alice_id.0.clone(),
            });
            let bob_signer = Arc::new(StubSigner {
                vk: bob_id.0.clone(),
            });
            let carol_signer = Arc::new(StubSigner {
                vk: carol_id.0.clone(),
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
            let carol_engine = Rc::new(SyncEngine::new(
                carol_store.clone(),
                carol_transport,
                SyncConfig::default(),
                carol_id.clone(),
                carol_signer,
            ));

            let alice_run = tokio::task::spawn_local({
                let e = alice_engine.clone();
                async move { e.run().await }
            });
            let bob_run = tokio::task::spawn_local({
                let e = bob_engine.clone();
                async move { e.run().await }
            });
            let carol_run = tokio::task::spawn_local({
                let e = carol_engine.clone();
                async move { e.run().await }
            });

            bob_engine.add_peer(alice_addr.clone()).await.unwrap();
            carol_engine.add_peer(alice_addr).await.unwrap();

            let filter = Filter::Keyspace(vk(b"chat"));
            bob_engine
                .subscribe_via(
                    filter.clone(),
                    alice_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();
            carol_engine
                .subscribe_via(filter, alice_id.clone(), SubscriptionPolicy::store_data())
                .await
                .unwrap();

            let block = ContentBlock {
                data: Bytes::from_static(b"hi-everyone"),
                references: vec![],
            };
            let entry = SignedKvEntry {
                verifying_key: vk(b"chat"),
                name: Bytes::from_static(b"m"),
                value_hash: block.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store.insert(entry, Some(block)).await.unwrap();

            for (name, store) in [("bob", &bob_store), ("carol", &carol_store)] {
                let got = wait_for(
                    Duration::from_secs(2),
                    Duration::from_millis(20),
                    || async { store.get_entry(&vk(b"chat"), b"m").await.unwrap().is_some() },
                )
                .await;
                assert!(got, "{name} did not receive alice's entry");
            }

            alice_run.abort();
            bob_run.abort();
            carol_run.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn one_receiver_two_filters_each_delivers_independently() {
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

            bob_engine.add_peer(alice_addr).await.unwrap();

            bob_engine
                .subscribe_via(
                    Filter::Keyspace(vk(b"writer1")),
                    alice_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();
            bob_engine
                .subscribe_via(
                    Filter::Keyspace(vk(b"writer2")),
                    alice_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            for writer in [b"writer1", b"writer2"] {
                let block = ContentBlock {
                    data: Bytes::copy_from_slice(writer),
                    references: vec![],
                };
                let entry = SignedKvEntry {
                    verifying_key: vk(writer),
                    name: Bytes::from_static(b"k"),
                    value_hash: block.hash(),
                    priority: 1,
                    expires_at: None,
                    signature: Bytes::new(),
                };
                alice_store.insert(entry, Some(block)).await.unwrap();
            }

            for writer in [b"writer1", b"writer2"] {
                let got = wait_for(
                    Duration::from_secs(2),
                    Duration::from_millis(20),
                    || async {
                        bob_store
                            .get_entry(&vk(writer), b"k")
                            .await
                            .unwrap()
                            .is_some()
                    },
                )
                .await;
                assert!(
                    got,
                    "bob did not receive entry from {}",
                    String::from_utf8_lossy(writer)
                );
            }

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unsubscribe_broadcast_intent_drops_all_per_peer_subs() {
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

            // Bob broadcast-subscribes; auto-resubscriber fires for alice once
            // they're connected.
            let filter = Filter::Keyspace(vk(b"chat"));
            bob_engine
                .subscribe(filter.clone(), SubscriptionPolicy::store_data())
                .await
                .unwrap();
            bob_engine.add_peer(alice_addr).await.unwrap();

            // First write should arrive.
            let block_y = ContentBlock {
                data: Bytes::from_static(b"y"),
                references: vec![],
            };
            let entry_y = SignedKvEntry {
                verifying_key: vk(b"chat"),
                name: Bytes::from_static(b"y"),
                value_hash: block_y.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store.insert(entry_y, Some(block_y)).await.unwrap();
            assert!(
                wait_for(
                    Duration::from_secs(2),
                    Duration::from_millis(20),
                    || async {
                        bob_store
                            .get_entry(&vk(b"chat"), b"y")
                            .await
                            .unwrap()
                            .is_some()
                    }
                )
                .await,
                "bob should receive y while broadcast-subscribed",
            );

            // unsubscribe should tear down the per-peer subscriptions
            // produced by the auto-resubscriber.
            bob_engine.unsubscribe(filter).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Subsequent write should NOT arrive.
            let block_z = ContentBlock {
                data: Bytes::from_static(b"z"),
                references: vec![],
            };
            let entry_z = SignedKvEntry {
                verifying_key: vk(b"chat"),
                name: Bytes::from_static(b"z"),
                value_hash: block_z.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store.insert(entry_z, Some(block_z)).await.unwrap();
            let leaked = wait_for(
                Duration::from_millis(500),
                Duration::from_millis(20),
                || async {
                    bob_store
                        .get_entry(&vk(b"chat"), b"z")
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(!leaked, "bob should NOT receive z after unsubscribing");

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}
