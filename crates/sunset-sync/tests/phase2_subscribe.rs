//! Integration tests for cooperative-relay Phase 2: subscribe / subscribe_via.
//!
//! Each test sets up two SyncEngines over `TestNetwork`; the receiver calls
//! one of the new APIs; the provider writes (before or after subscribe) and
//! the receiver checks its local store for the data to appear.

#![cfg(feature = "test-helpers")]

use std::time::Duration;

use sunset_store::{Filter, Store as _};
use sunset_sync::routing::SubscriptionPolicy;
use sunset_sync::test_helpers::{TestPeer, make_entry, vk, wait_for_entry};
use sunset_sync::test_transport::TestNetwork;

#[tokio::test(flavor = "current_thread")]
async fn subscribe_via_backfills_existing_entry() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice");
            let bob = TestPeer::spawn(&net, b"bob");

            // Alice (provider) writes an entry BEFORE Bob subscribes.
            let (entry, block) = make_entry(&vk(b"chat"), b"k", b"hello-bob", 1);
            alice.store.insert(entry, Some(block)).await.unwrap();

            // Bob connects to Alice.
            bob.engine.add_peer(alice.addr.clone()).await.unwrap();

            // Bob subscribes via Alice for the `chat` keyspace. After
            // subscribe_via returns, Alice's engine should observe
            // bob's `SubscriptionEntry::Active(provider=alice)` via
            // sync replication and arm the forwarding path; the
            // matching DigestRequest then backfills the pre-existing
            // entry to Bob.
            let filter = Filter::Keyspace(vk(b"chat"));
            bob.engine
                .subscribe_via(
                    filter.clone(),
                    alice.id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Public completion signal: alice has accepted bob's
            // subscription and queued the backfill DigestRequest. From
            // this point, application-data forwarding for `filter` is
            // armed end-to-end.
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's interest after subscribe_via"
            );

            assert!(
                wait_for_entry(&bob.store, &vk(b"chat"), b"k", Duration::from_secs(2)).await,
                "bob did not receive alice's pre-existing entry via subscribe_via backfill",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn subscribe_before_peer_connect_then_data_flows() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice");
            let bob = TestPeer::spawn(&net, b"bob");

            // Bob subscribes broadcast-style BEFORE connecting to anyone.
            // This records a BroadcastIntent but my_subs stays empty
            // (no connected peers yet).
            bob.engine
                .subscribe(
                    Filter::Keyspace(vk(b"chat")),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Now Bob connects to Alice. The auto-resubscriber hook on
            // PeerHello should replay Bob's BroadcastIntent as
            // subscribe_via(filter, alice, policy).
            bob.engine.add_peer(alice.addr.clone()).await.unwrap();

            // Alice writes a matching entry AFTER Bob is connected.
            let (entry, block) = make_entry(&vk(b"chat"), b"msg", b"hello-bob-broadcast", 1);
            alice.store.insert(entry, Some(block)).await.unwrap();

            assert!(
                wait_for_entry(&bob.store, &vk(b"chat"), b"msg", Duration::from_secs(2)).await,
                "bob did not receive alice's entry via the auto-resubscriber path",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unsubscribe_stops_forwarding() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice");
            let bob = TestPeer::spawn(&net, b"bob");

            bob.engine.add_peer(alice.addr.clone()).await.unwrap();
            let filter = Filter::Keyspace(vk(b"chat"));
            bob.engine
                .subscribe_via(
                    filter.clone(),
                    alice.id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // First write — Bob should see it.
            let (entry_y, block_y) = make_entry(&vk(b"chat"), b"y", b"y", 1);
            alice.store.insert(entry_y, Some(block_y)).await.unwrap();

            assert!(
                wait_for_entry(&bob.store, &vk(b"chat"), b"y", Duration::from_secs(2)).await,
                "bob should receive y while subscribed",
            );

            // Unsubscribe.
            bob.engine
                .unsubscribe_via(filter.clone(), alice.id.clone())
                .await
                .unwrap();

            // Public completion signal: alice has observed bob's
            // Withdrawn entry and cleared the forwarding gate.
            // Forward path for `filter` is closed when this returns.
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest_withdrawn(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not observe bob's unsubscribe",
            );

            // Second write — Bob should NOT see it.
            let (entry_z, block_z) = make_entry(&vk(b"chat"), b"z", b"z", 1);
            alice.store.insert(entry_z, Some(block_z)).await.unwrap();

            // Alice's forwarding gate is verified-closed before this
            // point; the negative window only needs to be long enough
            // to surface a spurious in-flight delivery, not to wait
            // for the gate to close.
            let leaked =
                wait_for_entry(&bob.store, &vk(b"chat"), b"z", Duration::from_millis(200)).await;
            assert!(!leaked, "bob should NOT receive z after unsubscribing");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn two_receivers_one_provider_each_sees_match() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice");
            let bob = TestPeer::spawn(&net, b"bob");
            let carol = TestPeer::spawn(&net, b"carol");

            bob.engine.add_peer(alice.addr.clone()).await.unwrap();
            carol.engine.add_peer(alice.addr.clone()).await.unwrap();

            let filter = Filter::Keyspace(vk(b"chat"));
            bob.engine
                .subscribe_via(
                    filter.clone(),
                    alice.id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();
            carol
                .engine
                .subscribe_via(
                    filter.clone(),
                    alice.id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Both receivers' subscriptions reach alice — forwarding
            // for `filter` is armed to both.
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's interest"
            );
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&carol.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm carol's interest"
            );

            let (entry, block) = make_entry(&vk(b"chat"), b"m", b"hi-everyone", 1);
            alice.store.insert(entry, Some(block)).await.unwrap();

            for (name, peer) in [("bob", &bob), ("carol", &carol)] {
                assert!(
                    wait_for_entry(&peer.store, &vk(b"chat"), b"m", Duration::from_secs(2)).await,
                    "{name} did not receive alice's entry",
                );
            }

            // Negative fanout: a write under a *different* writer-key
            // (so `Filter::Keyspace(vk("chat"))` does NOT match) must
            // NOT reach either receiver. Verifies the forwarding gate
            // really filters by the subscription's filter rather than
            // shipping every alice-write to every connected peer.
            let (off_filter_entry, off_filter_block) =
                make_entry(&vk(b"other"), b"m", b"off-filter", 1);
            alice
                .store
                .insert(off_filter_entry, Some(off_filter_block))
                .await
                .unwrap();
            // Forwarding gate is verified-armed for `filter`; this
            // window only catches a spurious off-filter delivery.
            for (name, peer) in [("bob", &bob), ("carol", &carol)] {
                let leaked =
                    wait_for_entry(&peer.store, &vk(b"other"), b"m", Duration::from_millis(200))
                        .await;
                assert!(
                    !leaked,
                    "{name} must NOT receive entries under non-matching writer-key"
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn one_receiver_two_filters_each_delivers_independently() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice");
            let bob = TestPeer::spawn(&net, b"bob");

            bob.engine.add_peer(alice.addr.clone()).await.unwrap();

            let filter1 = Filter::Keyspace(vk(b"writer1"));
            let filter2 = Filter::Keyspace(vk(b"writer2"));
            bob.engine
                .subscribe_via(
                    filter1.clone(),
                    alice.id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();
            bob.engine
                .subscribe_via(
                    filter2.clone(),
                    alice.id.clone(),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Both per-filter forwarding gates open before any writes.
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &filter1, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's writer1 interest"
            );
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &filter2, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's writer2 interest"
            );

            for writer in [b"writer1", b"writer2"] {
                let (entry, block) = make_entry(&vk(writer), b"k", writer, 1);
                alice.store.insert(entry, Some(block)).await.unwrap();
            }

            for writer in [b"writer1", b"writer2"] {
                assert!(
                    wait_for_entry(&bob.store, &vk(writer), b"k", Duration::from_secs(2)).await,
                    "bob did not receive entry from {}",
                    String::from_utf8_lossy(writer),
                );
            }

            // Negative: a write under a writer-key bob did NOT
            // subscribe to must NOT reach bob. Per-filter independence
            // means "subscribed to writer1 + writer2" doesn't
            // accidentally fall back to "subscribed to all".
            let (writer3_entry, writer3_block) = make_entry(&vk(b"writer3"), b"k", b"writer3", 1);
            alice
                .store
                .insert(writer3_entry, Some(writer3_block))
                .await
                .unwrap();
            let leaked = wait_for_entry(
                &bob.store,
                &vk(b"writer3"),
                b"k",
                Duration::from_millis(200),
            )
            .await;
            assert!(
                !leaked,
                "bob must NOT receive entries from writer3 (not subscribed)"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unsubscribe_broadcast_intent_drops_all_per_peer_subs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice");
            let bob = TestPeer::spawn(&net, b"bob");
            let carol = TestPeer::spawn(&net, b"carol");

            // Topology: bob ↔ alice and bob ↔ carol. A broadcast
            // subscribe on bob must fan out to BOTH providers; an
            // unsubscribe must withdraw from BOTH.
            let filter = Filter::Keyspace(vk(b"chat"));
            bob.engine
                .subscribe(filter.clone(), SubscriptionPolicy::store_data())
                .await
                .unwrap();
            bob.engine.add_peer(alice.addr.clone()).await.unwrap();
            bob.engine.add_peer(carol.addr.clone()).await.unwrap();

            // Both alice and carol observe bob's interest — broadcast
            // intent fanned out to every connected provider.
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's broadcast interest"
            );
            assert!(
                carol
                    .engine
                    .wait_for_peer_interest(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "carol did not arm bob's broadcast interest"
            );

            // Each provider can independently push matching entries to bob.
            let (entry_y, block_y) = make_entry(&vk(b"chat"), b"y", b"y", 1);
            alice.store.insert(entry_y, Some(block_y)).await.unwrap();
            assert!(
                wait_for_entry(&bob.store, &vk(b"chat"), b"y", Duration::from_secs(2)).await,
                "bob should receive y from alice while broadcast-subscribed",
            );

            // Now bob unsubscribes the broadcast intent. Per-peer
            // subscriptions to BOTH providers must be retracted; this
            // is the bug-class the test guards.
            bob.engine.unsubscribe(filter.clone()).await.unwrap();

            assert!(
                alice
                    .engine
                    .wait_for_peer_interest_withdrawn(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not observe bob's unsubscribe",
            );
            assert!(
                carol
                    .engine
                    .wait_for_peer_interest_withdrawn(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "carol did not observe bob's unsubscribe",
            );

            // Subsequent writes from EITHER provider must NOT reach bob.
            let (entry_z_alice, block_z_alice) =
                make_entry(&vk(b"chat"), b"z-alice", b"z-alice", 1);
            alice
                .store
                .insert(entry_z_alice, Some(block_z_alice))
                .await
                .unwrap();

            let (entry_z_carol, block_z_carol) =
                make_entry(&vk(b"chat"), b"z-carol", b"z-carol", 1);
            carol
                .store
                .insert(entry_z_carol, Some(block_z_carol))
                .await
                .unwrap();

            for (name, key) in [("alice", b"z-alice"), ("carol", b"z-carol")] {
                let leaked =
                    wait_for_entry(&bob.store, &vk(b"chat"), key, Duration::from_millis(200)).await;
                assert!(
                    !leaked,
                    "bob must NOT receive {name}'s post-unsubscribe write",
                );
            }
        })
        .await;
}

/// Self-authored application entries fan out to every currently-
/// connected peer regardless of whether that peer has installed an
/// interest. This is the documented Phase 2 invariant and is load-
/// bearing for `subscribe_via`: the `SubscriptionEntry` is itself a
/// self-authored write and reaches the named provider via this same
/// broadcast path, without which subscribe_via would only take effect
/// after the anti-entropy interval (default 30s).
#[tokio::test(flavor = "current_thread")]
async fn self_authored_application_entry_reaches_connected_peer_without_subscribe() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice");
            let bob = TestPeer::spawn(&net, b"bob");

            // Bob connects to Alice but does NOT subscribe to anything.
            bob.engine.add_peer(alice.addr.clone()).await.unwrap();

            // Wait for the PeerHello to land on Alice's side so the
            // outbound channel to Bob exists by the time Alice writes.
            let connected = sunset_sync::test_helpers::wait_for(
                Duration::from_secs(2),
                Duration::from_millis(20),
                || async { alice.engine.connected_peers().await.contains(&bob.id) },
            )
            .await;
            assert!(connected, "alice did not see bob connect");

            // Alice writes an entry under her OWN verifying_key. There's
            // no interest from bob for this key, but the self-author
            // broadcast invariant means bob should still receive it.
            let (entry, block) =
                make_entry(&alice.id.0, b"my-status", b"self-authored-app-data", 1);
            alice.store.insert(entry, Some(block)).await.unwrap();

            assert!(
                wait_for_entry(
                    &bob.store,
                    &alice.id.0,
                    b"my-status",
                    Duration::from_secs(2)
                )
                .await,
                "bob did not receive alice's self-authored entry despite being connected",
            );
        })
        .await;
}
