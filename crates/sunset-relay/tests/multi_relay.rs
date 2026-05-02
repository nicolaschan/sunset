//! Multi-relay integration tests.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rand_core::OsRng;
use zeroize::Zeroizing;

use sunset_core::crypto::constants::test_fast_params;
use sunset_core::{
    ComposedMessage, Ed25519Verifier, Identity, MessageBody, Room, compose_message, decode_message,
    room_messages_filter,
};
use sunset_noise::{NoiseIdentity, NoiseTransport, ed25519_seed_to_x25519_secret};
use sunset_relay::{Config, Relay};
use sunset_store::{ContentBlock, Hash, Store as _, VerifyingKey};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_sync_ws_native::WebSocketRawTransport;

// -- helpers --

struct IdentityAdapter(Identity);

impl NoiseIdentity for IdentityAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

#[allow(dead_code)]
fn ed25519_to_x25519_pub(secret_seed: &[u8; 32]) -> [u8; 32] {
    let s = ed25519_seed_to_x25519_secret(secret_seed);
    use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
    let scalar = Scalar::from_bytes_mod_order(*s);
    MontgomeryPoint::mul_base(&scalar).to_bytes()
}

/// Spin up a client SyncEngine that dials a relay address.
async fn make_client(
    identity: Identity,
    relay_addr: &str,
) -> (
    Arc<MemoryStore>,
    Rc<SyncEngine<MemoryStore, NoiseTransport<WebSocketRawTransport>>>,
) {
    let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
    let raw = WebSocketRawTransport::dial_only();
    let noise = NoiseTransport::new(raw, Arc::new(IdentityAdapter(identity.clone())));
    let local_peer = PeerId(identity.store_verifying_key());
    let signer: Arc<dyn Signer> = Arc::new(identity.clone());
    let engine = Rc::new(SyncEngine::new(
        store.clone(),
        noise,
        SyncConfig::default(),
        local_peer,
        signer,
    ));
    let engine_clone = engine.clone();
    tokio::task::spawn_local(async move { engine_clone.run().await });

    let addr = PeerAddr::new(Bytes::from(relay_addr.to_owned()));
    engine.add_peer(addr).await.expect("client dial relay");

    (store, engine)
}

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

fn relay_config(data_dir: &std::path::Path, listen_addr: &str, peers: &[String]) -> Config {
    let toml = format!(
        r#"
        listen_addr = "{}"
        data_dir = "{}"
        interest_filter = "all"
        identity_secret = "auto"
        peers = [{}]
        "#,
        listen_addr,
        data_dir.display(),
        peers
            .iter()
            .map(|p| format!("\"{}\"", p))
            .collect::<Vec<_>>()
            .join(", "),
    );
    Config::from_toml(&toml).unwrap()
}

// -- Test 1: two-relay propagation --

#[tokio::test(flavor = "current_thread")]
async fn alice_to_bob_via_two_relays() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir_a = tempfile::tempdir().unwrap();
            let dir_b = tempfile::tempdir().unwrap();

            // Relay A: listen, no federated peers yet (we'll learn its address first).
            let config_a = relay_config(dir_a.path(), "127.0.0.1:0", &[]);
            let mut relay_a = Relay::new(config_a).await.expect("relay A new");
            let relay_a_addr = relay_a.dial_address();
            let _engine_a_task = relay_a.run_for_test().await.expect("relay A run");

            // Relay B: listen, with relay A as federated peer.
            let config_b = relay_config(
                dir_b.path(),
                "127.0.0.1:0",
                std::slice::from_ref(&relay_a_addr),
            );
            let mut relay_b = Relay::new(config_b).await.expect("relay B new");
            let relay_b_addr = relay_b.dial_address();
            let _engine_b_task = relay_b.run_for_test().await.expect("relay B run");

            // Brief settle for federation handshake.
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Clients.
            let alice = Identity::generate(&mut OsRng);
            let bob = Identity::generate(&mut OsRng);
            let alice_room = Room::open_with_params("plan-d-test", &test_fast_params()).unwrap();
            let bob_room = Room::open_with_params("plan-d-test", &test_fast_params()).unwrap();

            let (alice_store, alice_engine) = make_client(alice.clone(), &relay_a_addr).await;
            let (bob_store, bob_engine) = make_client(bob.clone(), &relay_b_addr).await;

            // Bob declares interest.
            bob_engine
                .publish_subscription(room_messages_filter(&bob_room), Duration::from_secs(60))
                .await
                .unwrap();

            // Wait for alice's engine to know relay A's broad subscription so the
            // push path alice→relay-A is armed before we insert.
            let relay_a_vk = VerifyingKey::new(Bytes::copy_from_slice(&relay_a.ed25519_public));
            let alice_knows_relay_a = wait_for(
                Duration::from_secs(5),
                Duration::from_millis(50),
                || async { alice_engine.knows_peer_subscription(&relay_a_vk).await },
            )
            .await;
            assert!(
                alice_knows_relay_a,
                "alice did not learn relay A's subscription"
            );

            // Wait for relay B's own broad subscription to be known by alice's engine
            // (relay A should have propagated it transitively through the federation);
            // this indirectly confirms that relay A <-> relay B federation is established.
            let relay_b_vk = VerifyingKey::new(Bytes::copy_from_slice(&relay_b.ed25519_public));
            let alice_knows_relay_b = wait_for(
                Duration::from_secs(5),
                Duration::from_millis(50),
                || async { alice_engine.knows_peer_subscription(&relay_b_vk).await },
            )
            .await;
            assert!(
                alice_knows_relay_b,
                "alice did not learn relay B's subscription (federation not established)"
            );

            // Alice composes + inserts.
            let body = "hello bob across two relays";
            let sent_at = 1_700_000_000_000u64;
            let ComposedMessage { entry, block } =
                compose_message(&alice, &alice_room, 0, sent_at, MessageBody::Text(body.to_owned()), &mut OsRng).unwrap();
            let expected_hash: Hash = block.hash();
            alice_store
                .insert(entry.clone(), Some(block.clone()))
                .await
                .expect("alice's local store accepts her entry");

            // Wait for entry + block to land at bob's store.
            let bob_has_entry = wait_for(
                Duration::from_secs(10),
                Duration::from_millis(50),
                || async {
                    bob_store
                        .get_entry(&alice.store_verifying_key(), &entry.name)
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(
                bob_has_entry,
                "bob did not receive alice's entry via two relays"
            );

            let bob_has_block = wait_for(
                Duration::from_secs(10),
                Duration::from_millis(50),
                || async {
                    bob_store
                        .get_content(&expected_hash)
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(
                bob_has_block,
                "bob did not receive alice's content block via two relays"
            );

            // Decode + assert.
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
            assert_eq!(decoded.body, MessageBody::Text(body.to_owned()));
        })
        .await;
}

// -- Test 2: failover when one relay dies --

#[tokio::test(flavor = "current_thread")]
async fn failover_when_relay_a_dies() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir_a = tempfile::tempdir().unwrap();
            let dir_b = tempfile::tempdir().unwrap();

            // Relay A.
            let config_a = relay_config(dir_a.path(), "127.0.0.1:0", &[]);
            let mut relay_a = Relay::new(config_a).await.expect("relay A new");
            let relay_a_addr = relay_a.dial_address();
            let engine_a_task = relay_a.run_for_test().await.expect("relay A run");

            // Relay B (federated to A).
            let config_b = relay_config(
                dir_b.path(),
                "127.0.0.1:0",
                std::slice::from_ref(&relay_a_addr),
            );
            let mut relay_b = Relay::new(config_b).await.expect("relay B new");
            let relay_b_addr = relay_b.dial_address();
            let relay_b_vk = VerifyingKey::new(Bytes::copy_from_slice(&relay_b.ed25519_public));
            let _engine_b_task = relay_b.run_for_test().await.expect("relay B run");

            tokio::time::sleep(Duration::from_millis(500)).await;

            // Alice connects to BOTH; bob connects to BOTH.
            let alice = Identity::generate(&mut OsRng);
            let bob = Identity::generate(&mut OsRng);
            let alice_room =
                Room::open_with_params("plan-d-failover", &test_fast_params()).unwrap();
            let bob_room = Room::open_with_params("plan-d-failover", &test_fast_params()).unwrap();

            let (alice_store, alice_engine) = make_client(alice.clone(), &relay_a_addr).await;
            alice_engine
                .add_peer(PeerAddr::new(Bytes::from(relay_b_addr.clone())))
                .await
                .expect("alice dial relay B");

            let (bob_store, bob_engine) = make_client(bob.clone(), &relay_a_addr).await;
            bob_engine
                .add_peer(PeerAddr::new(Bytes::from(relay_b_addr.clone())))
                .await
                .expect("bob dial relay B");

            bob_engine
                .publish_subscription(room_messages_filter(&bob_room), Duration::from_secs(60))
                .await
                .unwrap();

            // Wait for alice to learn relay B's subscription (confirms federation + alice→B path
            // is established before we kill relay A).
            let alice_knows_relay_b = wait_for(
                Duration::from_secs(5),
                Duration::from_millis(50),
                || async { alice_engine.knows_peer_subscription(&relay_b_vk).await },
            )
            .await;
            assert!(
                alice_knows_relay_b,
                "alice did not learn relay B's subscription before starting"
            );

            // Compose msg-1; expect it to arrive normally (both relays alive).
            let ComposedMessage {
                entry: e1,
                block: b1,
            } = compose_message(
                &alice,
                &alice_room,
                0,
                1,
                MessageBody::Text("msg-1 (both relays alive)".to_owned()),
                &mut OsRng,
            )
            .unwrap();
            alice_store
                .insert(e1.clone(), Some(b1.clone()))
                .await
                .unwrap();

            let bob_has_msg1 = wait_for(
                Duration::from_secs(10),
                Duration::from_millis(50),
                || async {
                    bob_store
                        .get_entry(&alice.store_verifying_key(), &e1.name)
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(bob_has_msg1, "bob did not receive msg-1");

            // Kill relay A.
            engine_a_task.abort();
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Compose msg-2; expect it to still arrive via relay B.
            let ComposedMessage {
                entry: e2,
                block: b2,
            } = compose_message(
                &alice,
                &alice_room,
                0,
                2,
                MessageBody::Text("msg-2 (after relay A killed)".to_owned()),
                &mut OsRng,
            )
            .unwrap();
            alice_store
                .insert(e2.clone(), Some(b2.clone()))
                .await
                .unwrap();

            let bob_has_msg2 = wait_for(
                Duration::from_secs(15),
                Duration::from_millis(50),
                || async {
                    bob_store
                        .get_entry(&alice.store_verifying_key(), &e2.name)
                        .await
                        .unwrap()
                        .is_some()
                },
            )
            .await;
            assert!(bob_has_msg2, "bob did not receive msg-2 after relay A died");
        })
        .await;
}
