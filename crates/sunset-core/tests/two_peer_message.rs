//! End-to-end: alice composes an encrypted+signed chat message in an open
//! room; bob (who opened the same room name) receives the entry and the
//! content block via sunset-sync, then `decode_message` reconstructs the
//! exact author key + body on the receiving peer.
//!
//! Demonstrates the full crypto spec authentication invariant traversed in
//! anger: outer signature on insert (Ed25519Verifier), block-hash check,
//! AEAD decryption with the shared K_epoch_0, and inner-signature verify.
//!
//! Note on verifier: this test uses `MemoryStore::with_accept_all()` rather
//! than `Ed25519Verifier` because `sunset-sync` v1 writes its own internal
//! subscription/presence entries with stub (empty) signatures that
//! `Ed25519Verifier` would reject. The Ed25519Verifier integration is
//! independently covered by `crates/sunset-core/src/message.rs`'s
//! `composed_entry_passes_ed25519_verifier` unit test. Real sync-internal
//! signing belongs to a follow-up plan.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rand_core::OsRng;

use sunset_core::crypto::constants::test_fast_params;
use sunset_core::{
    ComposedMessage, Identity, MessageBody, Room, compose_message, decode_message,
    room_messages_filter,
};
use sunset_store::{ContentBlock, Hash, Store as _, VerifyingKey};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};

/// Test-only signer that returns a non-empty stub signature. Adequate when
/// the receiving store uses `AcceptAllVerifier`.
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
async fn alice_encrypts_bob_decrypts() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // ---- identities + rooms ----
            let alice = Identity::generate(&mut OsRng);
            let bob = Identity::generate(&mut OsRng);
            let alice_room = Room::open_with_params("general", &test_fast_params()).unwrap();
            let bob_room = Room::open_with_params("general", &test_fast_params()).unwrap();
            assert_eq!(alice_room.fingerprint(), bob_room.fingerprint());

            // ---- per-peer stores with accept-all verifier (see module docs) ----
            let alice_store = Arc::new(MemoryStore::with_accept_all());
            let bob_store = Arc::new(MemoryStore::with_accept_all());

            // ---- transport + engines ----
            let net = TestNetwork::new();
            let alice_addr = PeerAddr::new("alice");
            let bob_addr = PeerAddr::new("bob");
            let alice_peer = PeerId(alice.store_verifying_key());
            let bob_peer = PeerId(bob.store_verifying_key());

            let alice_transport = net.transport(alice_peer.clone(), alice_addr.clone());
            let bob_transport = net.transport(bob_peer.clone(), bob_addr.clone());

            let alice_signer = Arc::new(StubSigner {
                vk: alice_peer.0.clone(),
            });
            let bob_signer = Arc::new(StubSigner {
                vk: bob_peer.0.clone(),
            });

            let alice_engine = Rc::new(SyncEngine::new(
                alice_store.clone(),
                alice_transport,
                SyncConfig::default(),
                alice_peer.clone(),
                alice_signer,
            ));
            let bob_engine = Rc::new(SyncEngine::new(
                bob_store.clone(),
                bob_transport,
                SyncConfig::default(),
                bob_peer.clone(),
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

            // ---- bob declares interest in #general ----
            bob_engine
                .publish_subscription(room_messages_filter(&bob_room), Duration::from_secs(60))
                .await
                .unwrap();

            // ---- alice connects to bob ----
            alice_engine.add_peer(bob_addr).await.unwrap();

            let registered = wait_for(
                Duration::from_secs(2),
                Duration::from_millis(20),
                || async {
                    alice_engine
                        .knows_peer_subscription(&bob.store_verifying_key())
                        .await
                },
            )
            .await;
            assert!(registered, "alice did not learn bob's subscription");

            // ---- alice composes + inserts a real encrypted+signed message ----
            let body = "hello bob, this is encrypted";
            let sent_at = 1_700_000_000_000u64;
            let ComposedMessage { entry, block } = compose_message(
                &alice,
                &alice_room,
                0,
                sent_at,
                MessageBody::Text(body.to_owned()),
                &mut OsRng,
            )
            .unwrap();
            let expected_hash: Hash = block.hash();

            alice_store
                .insert(entry.clone(), Some(block.clone()))
                .await
                .expect("alice's own store accepts her signed entry");

            // ---- wait for bob's store to have both the entry and the block ----
            let bob_has_entry = wait_for(
                Duration::from_secs(2),
                Duration::from_millis(20),
                || async {
                    bob_store
                        .get_entry(&alice.store_verifying_key(), &entry.name)
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(bob_has_entry, "bob did not receive alice's entry");

            let bob_has_block = wait_for(
                Duration::from_secs(2),
                Duration::from_millis(20),
                || async {
                    bob_store
                        .get_content(&expected_hash)
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(bob_has_block, "bob did not receive alice's content block");

            // ---- bob decodes ----
            let bob_entry = bob_store
                .get_entry(&alice.store_verifying_key(), &entry.name)
                .await
                .unwrap()
                .unwrap();
            let bob_block: ContentBlock = bob_store
                .get_content(&expected_hash)
                .await
                .unwrap()
                .unwrap();

            let decoded = decode_message(&bob_room, &bob_entry, &bob_block).unwrap();
            assert_eq!(decoded.author_key, alice.public());
            assert_eq!(decoded.room_fingerprint, bob_room.fingerprint());
            assert_eq!(decoded.epoch_id, 0);
            assert_eq!(decoded.body, MessageBody::Text(body.to_owned()));
            assert_eq!(decoded.sent_at_ms, sent_at);

            // ---- a third party who never joined cannot decrypt ----
            let charlie_room =
                Room::open_with_params("not-the-right-name", &test_fast_params()).unwrap();
            let err = decode_message(&charlie_room, &bob_entry, &bob_block).unwrap_err();
            assert!(matches!(
                err,
                sunset_core::Error::BadName(_) | sunset_core::Error::AeadAuthFailed,
            ));

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
