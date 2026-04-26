//! Two-peer end-to-end integration test for sunset-sync.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use sunset_store::{ContentBlock, Filter, SignedKvEntry, Store as _, VerifyingKey};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, SyncConfig, SyncEngine};

fn vk(b: &[u8]) -> VerifyingKey {
    VerifyingKey::new(Bytes::copy_from_slice(b))
}

#[tokio::test(flavor = "current_thread")]
async fn alice_writes_bob_receives() {
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

            let alice_engine = Rc::new(SyncEngine::new(
                alice_store.clone(),
                alice_transport,
                SyncConfig::default(),
                alice_id.clone(),
            ));
            let bob_engine = Rc::new(SyncEngine::new(
                bob_store.clone(),
                bob_transport,
                SyncConfig::default(),
                bob_id.clone(),
            ));

            let alice_run = tokio::task::spawn_local({
                let e = alice_engine.clone();
                async move { e.run().await }
            });
            let bob_run = tokio::task::spawn_local({
                let e = bob_engine.clone();
                async move { e.run().await }
            });

            // Bob declares interest in the `chat` keyspace.
            bob_engine
                .publish_subscription(Filter::Keyspace(vk(b"chat")), Duration::from_secs(60))
                .await
                .unwrap();

            // Alice connects to Bob.
            alice_engine.add_peer(bob_addr).await.unwrap();

            // Wait for Bob's subscription to propagate to Alice's registry
            // via the bootstrap digest exchange.
            let registered = wait_for(
                Duration::from_secs(2),
                Duration::from_millis(20),
                || async { alice_engine.knows_peer_subscription(&vk(b"bob")).await },
            )
            .await;
            assert!(registered, "alice did not learn bob's subscription");

            // Alice writes (chat, k).
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

            // Bob should receive it via push.
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
            assert!(received, "bob did not receive alice's entry");

            let bob_view = bob_store
                .get_entry(&vk(b"chat"), b"k")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(bob_view, entry);

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}

/// Poll `condition` until it returns `true` or the deadline elapses.
async fn wait_for<F, Fut>(deadline: Duration, interval: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = tokio::time::Instant::now();
    while start.elapsed() < deadline {
        if condition().await {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}
