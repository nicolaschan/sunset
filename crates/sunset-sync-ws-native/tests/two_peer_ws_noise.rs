//! End-to-end: alice (dialer) and bob (listener) exchange a real
//! sunset-core encrypted+signed message over a real localhost WebSocket
//! wrapped in Noise. Both stores use Ed25519Verifier — proves the
//! sync-internal signing path is real.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rand_core::OsRng;
use zeroize::Zeroizing;

use sunset_core::crypto::constants::test_fast_params;
use sunset_core::{
    ComposedMessage, Ed25519Verifier, Identity, Room, compose_message, decode_message,
    room_messages_filter,
};
use sunset_noise::{NoiseIdentity, NoiseTransport, ed25519_seed_to_x25519_secret};
use sunset_store::{ContentBlock, Hash, Store as _};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, SyncConfig, SyncEngine};
use sunset_sync_ws_native::WebSocketRawTransport;

/// Adapter so sunset-core's `Identity` can be used as a NoiseIdentity
/// without sunset-core itself depending on sunset-noise.
struct IdentityNoiseAdapter(Identity);

impl NoiseIdentity for IdentityNoiseAdapter {
    fn ed25519_public(&self) -> [u8; 32] {
        self.0.public().as_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.secret_bytes())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn alice_encrypts_bob_decrypts_over_ws_and_noise() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // ---- identities + rooms ----
            let alice = Identity::generate(&mut OsRng);
            let bob = Identity::generate(&mut OsRng);
            let alice_room = Room::open_with_params("plan-c-test", &test_fast_params()).unwrap();
            let bob_room = Room::open_with_params("plan-c-test", &test_fast_params()).unwrap();
            assert_eq!(alice_room.fingerprint(), bob_room.fingerprint());

            // ---- both stores use Ed25519Verifier ----
            let alice_store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let bob_store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));

            // ---- bob listens on a random port ----
            let bob_raw = WebSocketRawTransport::listening_on("127.0.0.1:0".parse().unwrap())
                .await
                .unwrap();
            let bob_bound = bob_raw.local_addr().unwrap();
            let bob_noise =
                NoiseTransport::new(bob_raw, Arc::new(IdentityNoiseAdapter(bob.clone())));

            // ---- alice dials ----
            let alice_raw = WebSocketRawTransport::dial_only();
            let alice_noise =
                NoiseTransport::new(alice_raw, Arc::new(IdentityNoiseAdapter(alice.clone())));

            // PeerAddr for alice to dial bob: ws://<bob_bound>#x25519=<bob_x25519_pub_hex>
            let bob_seed = bob.secret_bytes();
            let bob_x25519_secret = ed25519_seed_to_x25519_secret(&bob_seed);
            let bob_x25519_pub = {
                use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
                let scalar = Scalar::from_bytes_mod_order(*bob_x25519_secret);
                MontgomeryPoint::mul_base(&scalar).to_bytes()
            };
            let bob_addr = PeerAddr::new(Bytes::from(format!(
                "ws://{}#x25519={}",
                bob_bound,
                hex::encode(bob_x25519_pub),
            )));

            // ---- engines (with real signers) ----
            let alice_signer: Arc<dyn sunset_sync::Signer> = Arc::new(alice.clone());
            let bob_signer: Arc<dyn sunset_sync::Signer> = Arc::new(bob.clone());

            // PeerId uses the Ed25519 verifying key — the application-layer
            // identity declared in Hello. This matches what the subscription
            // registry keys on, so push routing from alice to bob works end-to-end.
            // The Noise X25519 key is used only for the handshake (derived
            // internally by NoiseTransport from the Ed25519 seed via SHA-512 clamp).
            let alice_peer = PeerId(alice.store_verifying_key());
            let bob_peer = PeerId(bob.store_verifying_key());

            let alice_engine = Rc::new(SyncEngine::new(
                alice_store.clone(),
                alice_noise,
                SyncConfig::default(),
                alice_peer.clone(),
                alice_signer,
            ));
            let bob_engine = Rc::new(SyncEngine::new(
                bob_store.clone(),
                bob_noise,
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

            // ---- bob declares interest ----
            bob_engine
                .publish_subscription(room_messages_filter(&bob_room), Duration::from_secs(60))
                .await
                .unwrap();

            // ---- alice connects to bob ----
            alice_engine.add_peer(bob_addr).await.unwrap();

            // ---- wait for subscription propagation ----
            let registered = wait_for(
                Duration::from_secs(5),
                Duration::from_millis(50),
                || async {
                    alice_engine
                        .knows_peer_subscription(&bob.store_verifying_key())
                        .await
                },
            )
            .await;
            assert!(registered, "alice did not learn bob's subscription");

            // ---- alice composes + inserts ----
            let body = "hello bob via real ws + noise";
            let sent_at = 1_700_000_000_000u64;
            let ComposedMessage { entry, block } =
                compose_message(&alice, &alice_room, 0, sent_at, body, &mut OsRng).unwrap();
            let expected_hash: Hash = block.hash();
            alice_store
                .insert(entry.clone(), Some(block.clone()))
                .await
                .expect("alice's own store accepts her real-signed entry");

            // ---- bob receives entry + block ----
            let bob_has_entry = wait_for(
                Duration::from_secs(5),
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
            assert!(bob_has_entry, "bob did not receive alice's entry");

            let bob_has_block = wait_for(
                Duration::from_secs(5),
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
            assert_eq!(decoded.body, body);

            alice_run.abort();
            bob_run.abort();
        })
        .await;
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
