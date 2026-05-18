//! End-to-end honest test: two QuicRawTransport instances over a
//! real UDP socket on 127.0.0.1, sharing a MemoryStore-backed
//! RelaySignaler. One side calls connect(); the other side's
//! accept() returns the matching connection. Both roundtrip a
//! reliable message and a datagram.
//!
//! No stub signaler, no probe-loop bypass, no test-only inspector
//! poking — CLAUDE.md debugging discipline.

use std::sync::Arc;

use bytes::Bytes;
use tokio::task::LocalSet;

use sunset_core::crypto::constants::test_fast_params;
use sunset_core::{Ed25519Verifier, Identity, RelaySignaler, Room};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, RawConnection, RawTransport};
use sunset_sync_quic::QuicRawTransport;

#[tokio::test(flavor = "current_thread")]
async fn loopback_holepunch_reliable_and_datagram_roundtrip() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let alice_id = Identity::from_secret_bytes(&[1u8; 32]);
            let bob_id = Identity::from_secret_bytes(&[2u8; 32]);
            let alice_pk = PeerId(alice_id.store_verifying_key());
            let bob_pk = PeerId(bob_id.store_verifying_key());

            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let room = Room::open_with_params("alpha", &test_fast_params()).expect("open room");
            let fp = room.fingerprint();

            let alice_signaler = RelaySignaler::new(alice_id, fp.to_hex(), &store);
            let bob_signaler = RelaySignaler::new(bob_id, fp.to_hex(), &store);

            let alice_t = QuicRawTransport::bind(alice_signaler, alice_pk.clone(), vec![])
                .await
                .expect("alice bind");
            let bob_t = QuicRawTransport::bind(bob_signaler, bob_pk.clone(), vec![])
                .await
                .expect("bob bind");

            let bob_addr = PeerAddr::new(Bytes::from(format!(
                "quic://{}",
                hex::encode(bob_pk.verifying_key().as_bytes())
            )));

            let (a_conn, b_conn) = tokio::join!(alice_t.connect(bob_addr), bob_t.accept());
            let a_conn = a_conn.expect("alice connect");
            let b_conn = b_conn.expect("bob accept");

            a_conn
                .send_reliable(Bytes::from_static(b"hello bob"))
                .await
                .expect("alice send_reliable");
            let got = b_conn.recv_reliable().await.expect("bob recv_reliable");
            assert_eq!(got.as_ref(), b"hello bob");

            b_conn
                .send_unreliable(Bytes::from_static(b"dgram"))
                .await
                .expect("bob send_unreliable");
            let dg = a_conn
                .recv_unreliable()
                .await
                .expect("alice recv_unreliable");
            assert_eq!(dg.as_ref(), b"dgram");

            a_conn.close().await.expect("close alice");
            b_conn.close().await.expect("close bob");
        })
        .await;
}
