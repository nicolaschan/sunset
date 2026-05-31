//! Integration tests for `SyncEngine::subscribe` / `subscribe_via`.
//!
//! Each test sets up two `SyncEngine`s over `TestNetwork`; the receiver calls
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

            // Provider writes BEFORE the subscribe; this exercises the
            // backfill DigestRequest, not the live forwarding path.
            let (entry, block) = make_entry(&vk(b"chat"), b"k", b"hello-bob", 1);
            alice.store.insert(entry, Some(block)).await.unwrap();

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

            // Subscribe BEFORE any connection: this records a
            // BroadcastIntent but `my_subs` stays empty until peers
            // arrive and the PeerHello auto-resubscriber replays it.
            bob.engine
                .subscribe(
                    Filter::Keyspace(vk(b"chat")),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            bob.engine.add_peer(alice.addr.clone()).await.unwrap();

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

            let (entry_y, block_y) = make_entry(&vk(b"chat"), b"y", b"y", 1);
            alice.store.insert(entry_y, Some(block_y)).await.unwrap();

            assert!(
                wait_for_entry(&bob.store, &vk(b"chat"), b"y", Duration::from_secs(2)).await,
                "bob should receive y while subscribed",
            );

            bob.engine
                .unsubscribe_via(filter.clone(), alice.id.clone())
                .await
                .unwrap();

            assert!(
                alice
                    .engine
                    .wait_for_peer_interest_withdrawn(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not observe bob's unsubscribe",
            );

            let (entry_z, block_z) = make_entry(&vk(b"chat"), b"z", b"z", 1);
            alice.store.insert(entry_z, Some(block_z)).await.unwrap();

            // Forwarding gate is verified-closed above; this short
            // window only surfaces a spurious in-flight delivery.
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

            // Negative: a write whose writer-key does NOT match
            // `Filter::Keyspace(vk("chat"))` must not reach either
            // receiver — the forwarding gate must filter by the
            // subscription's filter, not blanket-fan-out.
            let (off_filter_entry, off_filter_block) =
                make_entry(&vk(b"other"), b"m", b"off-filter", 1);
            alice
                .store
                .insert(off_filter_entry, Some(off_filter_block))
                .await
                .unwrap();
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

            // Negative: per-filter independence — "subscribed to
            // writer1 + writer2" must not collapse to "subscribed to
            // all".
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

            // bob ↔ alice and bob ↔ carol; a broadcast subscribe on
            // bob must fan out to BOTH providers; an unsubscribe must
            // withdraw from BOTH.
            let filter = Filter::Keyspace(vk(b"chat"));
            bob.engine
                .subscribe(filter.clone(), SubscriptionPolicy::store_data())
                .await
                .unwrap();
            bob.engine.add_peer(alice.addr.clone()).await.unwrap();
            bob.engine.add_peer(carol.addr.clone()).await.unwrap();

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

            let (entry_y, block_y) = make_entry(&vk(b"chat"), b"y", b"y", 1);
            alice.store.insert(entry_y, Some(block_y)).await.unwrap();
            assert!(
                wait_for_entry(&bob.store, &vk(b"chat"), b"y", Duration::from_secs(2)).await,
                "bob should receive y from alice while broadcast-subscribed",
            );

            // `unsubscribe` on the broadcast intent must retract the
            // per-peer subscription to BOTH providers, not just one.
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
/// interest. Load-bearing for `subscribe_via`: the `SubscriptionEntry`
/// is itself a self-authored write and reaches the named provider via
/// this same broadcast path; without it, subscribe_via would only take
/// effect after the anti-entropy interval (default 30 s).
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

            // Wait for PeerHello on alice's side: the outbound channel
            // to bob must exist by the time alice writes.
            let connected = sunset_sync::test_helpers::wait_for(
                Duration::from_secs(2),
                Duration::from_millis(20),
                || async { alice.engine.connected_peers().await.contains(&bob.id) },
            )
            .await;
            assert!(connected, "alice did not see bob connect");

            // Alice writes under her own verifying_key. Bob holds no
            // interest for this key, but the self-author broadcast
            // invariant means he should still receive it.
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

/// A peer's declared interest must survive a transport reconnect.
///
/// `interests` lives in the per-connection `PeerSession`, so a fresh
/// `PeerHello` for the same `PeerId` (a reconnect) starts the session
/// with an empty interest map. The provider must re-derive the
/// reconnecting peer's interest from the durable `SubscriptionEntry`
/// still in its own store — without the receiver re-subscribing and
/// without waiting for the next routing-tick republish.
///
/// This is the three-way-voice reliability bug: voice frames ride
/// `publish_ephemeral`, which does not persist or retry, so every frame
/// emitted while a peer's interest is dark is lost forever and the
/// receiver never marks the sender voice-connected. A 1-hour freshness
/// threshold pins the routing tick out of the picture, so the only thing
/// that can re-arm the interest after the reconnect is the provider
/// re-deriving it from its store.
#[tokio::test(flavor = "current_thread")]
async fn interest_survives_peer_reconnect() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice"); // provider
            let bob = TestPeer::spawn(&net, b"bob"); // receiver

            // Long freshness => the routing tick will not republish bob's
            // subscription during this test, so a re-arm can only come
            // from alice re-deriving it on reconnect.
            let policy = SubscriptionPolicy {
                freshness_threshold: Duration::from_secs(3600),
            };
            let filter = Filter::Keyspace(vk(b"voice"));

            bob.engine.add_peer(alice.addr.clone()).await.unwrap();
            bob.engine
                .subscribe_via(filter.clone(), alice.id.clone(), policy)
                .await
                .unwrap();

            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not arm bob's interest after the initial subscribe_via",
            );

            // Tear the bob<->alice connection down and re-establish it,
            // WITHOUT bob re-subscribing.
            bob.engine.remove_peer(alice.id.clone()).await.unwrap();
            assert!(
                sunset_sync::test_helpers::wait_for(
                    Duration::from_secs(2),
                    Duration::from_millis(20),
                    || async { !alice.engine.connected_peers().await.contains(&bob.id) },
                )
                .await,
                "alice did not observe bob disconnect",
            );

            bob.engine.add_peer(alice.addr.clone()).await.unwrap();
            assert!(
                sunset_sync::test_helpers::wait_for(
                    Duration::from_secs(2),
                    Duration::from_millis(20),
                    || async { alice.engine.connected_peers().await.contains(&bob.id) },
                )
                .await,
                "alice did not observe bob reconnect",
            );

            // The invariant: bob's interest is live again on alice even
            // though bob never re-subscribed and the routing tick has not
            // (and for an hour will not) republish it.
            assert!(
                alice
                    .engine
                    .wait_for_peer_interest(&bob.id, &filter, Duration::from_secs(2))
                    .await,
                "alice did not re-arm bob's interest after reconnect: forwarding to bob went \
                 dark, so ephemeral voice frames would be dropped with no recovery",
            );
        })
        .await;
}
