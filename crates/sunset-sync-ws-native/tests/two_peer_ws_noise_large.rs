//! Round-trip a 2 MiB `SyncMessage::EventDelivery` between two peers
//! over a real localhost WebSocket wrapped in Noise. The payload is
//! sized so the per-connection `ChunkedConnection` inside
//! `NoiseConnection` must fire ~32 times each direction; before that
//! decorator landed, the send would error at `snow.write_message` and
//! never reach the wire.
//!
//! The test sets up `NoiseTransport` on both sides (axum-served on
//! bob, dialer-only on alice), drives the noise handshake via
//! `NoiseTransport::accept` / `::connect`, then sends a
//! postcard-encoded `SyncMessage::EventDelivery` carrying a single
//! `ContentBlock` of ~2 MiB random-ish data and asserts byte-for-byte
//! equality on the receiver. Exercises the full real I/O stack —
//! sunset-sync postcard encode → chunked noise → tungstenite WS
//! framing → tungstenite WS framing → chunked noise → postcard decode
//! — at a payload size that is unreachable without the chunker.

use std::sync::Arc;

use axum::routing::get;
use bytes::Bytes;
use rand_core::OsRng;
use zeroize::Zeroizing;

use sunset_core::Identity;
use sunset_noise::{NoiseIdentity, NoiseTransport, ed25519_seed_to_x25519_secret};
use sunset_store::{ContentBlock, Hash, SignedKvEntry, VerifyingKey};
use sunset_sync::{PeerAddr, SyncMessage, Transport, TransportConnection};
use sunset_sync_ws_native::{WebSocketRawTransport, axum_integration};

/// Adapter so sunset-core's `Identity` can be used as a NoiseIdentity
/// without sunset-core itself depending on sunset-noise. Mirrors the
/// adapter in the existing `two_peer_ws_noise.rs` integration test.
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
async fn large_payload_over_ws_noise() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let alice = Identity::generate(&mut OsRng);
            let bob = Identity::generate(&mut OsRng);

            // ---- bob serves via axum on a random local port ----
            let (bob_raw, ws_tx) = WebSocketRawTransport::serving();
            let app = axum::Router::new().route(
                "/",
                get({
                    let ws_tx = ws_tx.clone();
                    move |ws: axum::extract::WebSocketUpgrade| {
                        axum_integration::ws_handler(ws, ws_tx.clone())
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bob_bound = listener.local_addr().unwrap();
            let _serve_handle = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let bob_noise =
                NoiseTransport::new(bob_raw, Arc::new(IdentityNoiseAdapter(bob.clone())));

            // ---- alice dials ----
            let alice_raw = WebSocketRawTransport::dial_only();
            let alice_noise =
                NoiseTransport::new(alice_raw, Arc::new(IdentityNoiseAdapter(alice.clone())));

            // bob's X25519 public key, derived from his ed25519 seed.
            let bob_seed = bob.secret_bytes();
            let bob_x25519_secret = ed25519_seed_to_x25519_secret(&bob_seed);
            let bob_x25519_pub: [u8; 32] = {
                use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
                let scalar = Scalar::from_bytes_mod_order(*bob_x25519_secret);
                MontgomeryPoint::mul_base(&scalar).to_bytes()
            };
            let bob_addr = PeerAddr::new(Bytes::from(format!(
                "ws://{}#x25519={}",
                bob_bound,
                hex::encode(bob_x25519_pub),
            )));

            // ---- handshake (parallel: alice connects, bob accepts) ----
            let bob_accept = tokio::task::spawn_local(async move { bob_noise.accept().await });
            let alice_conn = alice_noise
                .connect(bob_addr)
                .await
                .expect("alice connect+handshake");
            let bob_conn = bob_accept
                .await
                .expect("bob accept task")
                .expect("bob handshake");

            // ---- build a ~2 MiB SyncMessage::EventDelivery ----
            let n: usize = 2 * 1024 * 1024;
            let big: Vec<u8> = (0..n).map(|i| i.wrapping_mul(17) as u8).collect();
            let block = ContentBlock {
                data: Bytes::from(big),
                references: Vec::new(),
            };
            let block_hash: Hash = block.hash();
            let entry = SignedKvEntry {
                verifying_key: VerifyingKey::new(Bytes::copy_from_slice(&[7u8; 32])),
                name: Bytes::from_static(b"large/integration/test"),
                value_hash: block_hash,
                priority: 1,
                expires_at: None,
                signature: Bytes::copy_from_slice(&[0u8; 64]),
            };
            let msg = SyncMessage::EventDelivery {
                entries: vec![entry],
                blobs: vec![block],
            };
            let encoded = msg.encode().expect("encode");
            assert!(
                encoded.len() > 2 * 1024 * 1024,
                "encoded payload should be > 2 MiB; was {}",
                encoded.len()
            );

            // ---- send + receive + decode + assert ----
            alice_conn
                .send_reliable(encoded.clone())
                .await
                .expect("alice send_reliable");
            let received = bob_conn.recv_reliable().await.expect("bob recv_reliable");
            assert_eq!(received.len(), encoded.len(), "wire length mismatch");
            assert_eq!(received, encoded, "wire content mismatch");
            let decoded = SyncMessage::decode(&received).expect("decode");
            assert_eq!(decoded, msg, "SyncMessage round-trip mismatch");
        })
        .await;
}
