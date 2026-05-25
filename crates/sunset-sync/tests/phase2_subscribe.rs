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

            // Bob subscribes via Alice for the `chat` keyspace. After
            // subscribe_via returns, Alice's engine should observe
            // bob's `SubscriptionEntry::Active(provider=alice)` via
            // sync replication and arm the forwarding path; the
            // matching DigestRequest then backfills the pre-existing
            // entry to Bob.
            let filter = Filter::Keyspace(vk(b"chat"));
            bob_engine
                .subscribe_via(
                    filter.clone(),
                    alice_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Public completion signal: alice has accepted bob's
            // subscription and queued the backfill DigestRequest. From
            // this point, application-data forwarding for `filter` is
            // armed end-to-end.
            assert!(
                alice_engine
                    .wait_for_peer_interest(&bob_id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's interest after subscribe_via"
            );

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

            // Public completion signal: alice has observed bob's
            // Withdrawn entry and cleared the forwarding gate.
            // Forward path for `filter` is closed when this returns.
            assert!(
                alice_engine
                    .wait_for_peer_interest_withdrawn(&bob_id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not observe bob's unsubscribe",
            );

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

            // Alice's forwarding gate is verified-closed before this
            // point; the negative window only needs to be long enough
            // to surface a spurious in-flight delivery, not to wait
            // for the gate to close.
            let leaked = wait_for(
                Duration::from_millis(200),
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
                .subscribe_via(
                    filter.clone(),
                    alice_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Both receivers' subscriptions reach alice — forwarding
            // for `filter` is armed to both.
            assert!(
                alice_engine
                    .wait_for_peer_interest(&bob_id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's interest"
            );
            assert!(
                alice_engine
                    .wait_for_peer_interest(&carol_id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm carol's interest"
            );

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

            // Negative fanout: a write under a *different* writer-key
            // (so `Filter::Keyspace(vk("chat"))` does NOT match) must
            // NOT reach either receiver. Verifies the forwarding gate
            // really filters by the subscription's filter rather than
            // shipping every alice-write to every connected peer.
            let off_filter_block = ContentBlock {
                data: Bytes::from_static(b"off-filter"),
                references: vec![],
            };
            let off_filter_entry = SignedKvEntry {
                verifying_key: vk(b"other"),
                name: Bytes::from_static(b"m"),
                value_hash: off_filter_block.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store
                .insert(off_filter_entry, Some(off_filter_block))
                .await
                .unwrap();
            // Forwarding gate is verified-armed for `filter`; this
            // window only catches a spurious off-filter delivery.
            for (name, store) in [("bob", &bob_store), ("carol", &carol_store)] {
                let leaked = wait_for(
                    Duration::from_millis(200),
                    Duration::from_millis(20),
                    || async {
                        store
                            .get_entry(&vk(b"other"), b"m")
                            .await
                            .unwrap()
                            .is_some()
                    },
                )
                .await;
                assert!(
                    !leaked,
                    "{name} must NOT receive entries under non-matching writer-key"
                );
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

            let filter1 = Filter::Keyspace(vk(b"writer1"));
            let filter2 = Filter::Keyspace(vk(b"writer2"));
            bob_engine
                .subscribe_via(
                    filter1.clone(),
                    alice_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();
            bob_engine
                .subscribe_via(
                    filter2.clone(),
                    alice_id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Both per-filter forwarding gates open before any writes.
            assert!(
                alice_engine
                    .wait_for_peer_interest(&bob_id, &filter1, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's writer1 interest"
            );
            assert!(
                alice_engine
                    .wait_for_peer_interest(&bob_id, &filter2, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's writer2 interest"
            );

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

            // Negative: a write under a writer-key bob did NOT
            // subscribe to must NOT reach bob. Per-filter independence
            // means "subscribed to writer1 + writer2" doesn't
            // accidentally fall back to "subscribed to all".
            let writer3_block = ContentBlock {
                data: Bytes::from_static(b"writer3"),
                references: vec![],
            };
            let writer3_entry = SignedKvEntry {
                verifying_key: vk(b"writer3"),
                name: Bytes::from_static(b"k"),
                value_hash: writer3_block.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store
                .insert(writer3_entry, Some(writer3_block))
                .await
                .unwrap();
            let leaked = wait_for(
                Duration::from_millis(200),
                Duration::from_millis(20),
                || async {
                    bob_store
                        .get_entry(&vk(b"writer3"), b"k")
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(
                !leaked,
                "bob must NOT receive entries from writer3 (not subscribed)"
            );

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

            // Topology: bob ↔ alice and bob ↔ carol. A broadcast
            // subscribe on bob must fan out to BOTH providers; an
            // unsubscribe must withdraw from BOTH.
            let filter = Filter::Keyspace(vk(b"chat"));
            bob_engine
                .subscribe(filter.clone(), SubscriptionPolicy::store_data())
                .await
                .unwrap();
            bob_engine.add_peer(alice_addr).await.unwrap();
            bob_engine.add_peer(carol_addr).await.unwrap();

            // Both alice and carol observe bob's interest — broadcast
            // intent fanned out to every connected provider.
            assert!(
                alice_engine
                    .wait_for_peer_interest(&bob_id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's broadcast interest"
            );
            assert!(
                carol_engine
                    .wait_for_peer_interest(&bob_id, &filter, Duration::from_secs(2))
                    .await,
                "carol did not arm bob's broadcast interest"
            );

            // Each provider can independently push matching entries to bob.
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
                "bob should receive y from alice while broadcast-subscribed",
            );

            // Now bob unsubscribes the broadcast intent. Per-peer
            // subscriptions to BOTH providers must be retracted; this
            // is the bug-class the test guards.
            bob_engine.unsubscribe(filter.clone()).await.unwrap();

            assert!(
                alice_engine
                    .wait_for_peer_interest_withdrawn(&bob_id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not observe bob's unsubscribe",
            );
            assert!(
                carol_engine
                    .wait_for_peer_interest_withdrawn(&bob_id, &filter, Duration::from_secs(2))
                    .await,
                "carol did not observe bob's unsubscribe",
            );

            // Subsequent writes from EITHER provider must NOT reach bob.
            let block_z_alice = ContentBlock {
                data: Bytes::from_static(b"z-alice"),
                references: vec![],
            };
            let entry_z_alice = SignedKvEntry {
                verifying_key: vk(b"chat"),
                name: Bytes::from_static(b"z-alice"),
                value_hash: block_z_alice.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            alice_store
                .insert(entry_z_alice, Some(block_z_alice))
                .await
                .unwrap();

            let block_z_carol = ContentBlock {
                data: Bytes::from_static(b"z-carol"),
                references: vec![],
            };
            let entry_z_carol = SignedKvEntry {
                verifying_key: vk(b"chat"),
                name: Bytes::from_static(b"z-carol"),
                value_hash: block_z_carol.hash(),
                priority: 1,
                expires_at: None,
                signature: Bytes::new(),
            };
            carol_store
                .insert(entry_z_carol, Some(block_z_carol))
                .await
                .unwrap();

            for (name, key) in [("alice", b"z-alice"), ("carol", b"z-carol")] {
                let leaked = wait_for(
                    Duration::from_millis(200),
                    Duration::from_millis(20),
                    || async {
                        bob_store
                            .get_entry(&vk(b"chat"), key)
                            .await
                            .unwrap()
                            .is_some()
                    },
                )
                .await;
                assert!(
                    !leaked,
                    "bob must NOT receive {name}'s post-unsubscribe write",
                );
            }

            alice_run.abort();
            bob_run.abort();
            carol_run.abort();
        })
        .await;
}
