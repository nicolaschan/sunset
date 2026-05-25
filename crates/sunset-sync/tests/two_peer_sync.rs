//! Two-peer end-to-end integration test for sunset-sync.

#![cfg(feature = "test-helpers")]

use std::time::Duration;

use sunset_store::{Filter, Store as _};
use sunset_sync::routing::SubscriptionPolicy;
use sunset_sync::test_helpers::{TestPeer, make_entry, vk, wait_for_entry};
use sunset_sync::test_transport::TestNetwork;

#[tokio::test(flavor = "current_thread")]
async fn alice_writes_bob_receives() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let alice = TestPeer::spawn(&net, b"alice");
            let bob = TestPeer::spawn(&net, b"bob");

            // Bob declares interest in the `chat` keyspace.
            bob.engine
                .subscribe(
                    Filter::Keyspace(vk(b"chat")),
                    SubscriptionPolicy::store_data(),
                )
                .await
                .unwrap();

            // Alice connects to Bob.
            alice.engine.add_peer(bob.addr.clone()).await.unwrap();

            // Alice writes (chat, k).
            let (entry, block) = make_entry(&vk(b"chat"), b"k", b"hello-bob", 1);
            alice
                .store
                .insert(entry.clone(), Some(block))
                .await
                .unwrap();

            // Bob should receive it via push.
            assert!(
                wait_for_entry(&bob.store, &vk(b"chat"), b"k", Duration::from_secs(2)).await,
                "bob did not receive alice's entry",
            );

            let bob_view = bob
                .store
                .get_entry(&vk(b"chat"), b"k")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(bob_view, entry);
        })
        .await;
}
