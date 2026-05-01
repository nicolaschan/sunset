//! Integration: SyncEngine + heartbeat + PeerSupervisor.
//!
//! Verifies end-to-end that when a connection is torn down at the engine
//! layer (simulating a real disconnect), the supervisor sees `PeerRemoved`
//! through its public subscription, schedules a backoff, and the next
//! `add_peer` call brings the connection back.

#![cfg(feature = "test-helpers")]

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use sunset_store::VerifyingKey;
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{
    BackoffPolicy, IntentState, PeerAddr, PeerId, PeerSupervisor, Signer, SyncConfig, SyncEngine,
};

fn vk(b: &[u8]) -> VerifyingKey {
    VerifyingKey::new(Bytes::copy_from_slice(b))
}

struct StubSigner(VerifyingKey);
impl Signer for StubSigner {
    fn verifying_key(&self) -> VerifyingKey {
        self.0.clone()
    }
    fn sign(&self, _: &[u8]) -> Bytes {
        Bytes::from_static(&[0u8; 64])
    }
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

#[tokio::test(flavor = "current_thread")]
async fn supervisor_redials_after_disconnect() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();

            let alice_id = PeerId(vk(b"alice"));
            let bob_id = PeerId(vk(b"bob"));

            let alice_transport = net.transport(
                alice_id.clone(),
                PeerAddr::new(Bytes::from_static(b"alice")),
            );
            let bob_transport =
                net.transport(bob_id.clone(), PeerAddr::new(Bytes::from_static(b"bob")));

            let alice = Rc::new(SyncEngine::new(
                Arc::new(MemoryStore::with_accept_all()),
                alice_transport,
                SyncConfig::default(),
                alice_id.clone(),
                Arc::new(StubSigner(alice_id.0.clone())),
            ));
            let bob = Rc::new(SyncEngine::new(
                Arc::new(MemoryStore::with_accept_all()),
                bob_transport,
                SyncConfig::default(),
                bob_id.clone(),
                Arc::new(StubSigner(bob_id.0.clone())),
            ));

            sunset_sync::spawn::spawn_local({
                let a = alice.clone();
                async move {
                    let _ = a.run().await;
                }
            });
            sunset_sync::spawn::spawn_local({
                let b = bob.clone();
                async move {
                    let _ = b.run().await;
                }
            });

            // Tight backoff so the redial happens well within our wait
            // window, and zero jitter so the timing is deterministic.
            let policy = BackoffPolicy {
                initial: Duration::from_millis(50),
                max: Duration::from_millis(200),
                multiplier: 2.0,
                jitter: 0.0,
            };
            let sup = PeerSupervisor::new(alice.clone(), policy);
            sunset_sync::spawn::spawn_local({
                let s = sup.clone();
                async move { s.run().await }
            });

            let bob_addr = PeerAddr::new(Bytes::from_static(b"bob"));
            sup.add(bob_addr.clone()).await.unwrap();

            // After add resolves, the supervisor's intent should be Connected
            // and the engine should know about bob.
            let snap = sup.snapshot().await;
            assert_eq!(snap.len(), 1);
            assert_eq!(snap[0].state, IntentState::Connected);
            let connected_peer_id = snap[0]
                .peer_id
                .clone()
                .expect("supervisor must have learned bob's peer id on connect");
            assert_eq!(connected_peer_id, bob_id);

            // Tear the connection down at the engine layer. This bypasses
            // the supervisor's command channel — exactly what would happen
            // if a heartbeat timeout or send-side failure tripped: the
            // engine emits PeerRemoved, and the supervisor must redial.
            alice.remove_peer(bob_id.clone()).await.unwrap();

            // The supervisor should observe PeerRemoved, schedule a backoff
            // (50ms initial, no jitter), and then redial bob successfully.
            // Allow generous slack — 1s is plenty for a 50ms backoff plus
            // round-trip dial in tests.
            let redialed = wait_for(
                Duration::from_secs(1),
                Duration::from_millis(20),
                || async {
                    let snap = sup.snapshot().await;
                    snap.iter().any(|s| {
                        s.addr == bob_addr
                            && s.state == IntentState::Connected
                            && s.peer_id.as_ref() == Some(&bob_id)
                    })
                },
            )
            .await;
            assert!(redialed, "supervisor failed to redial bob after disconnect");

            // Sanity: the engine actually has bob back in its peer table.
            let connected = alice.connected_peers().await;
            assert!(
                connected.iter().any(|p| p == &bob_id),
                "engine does not have bob connected after redial"
            );
        })
        .await;
}
