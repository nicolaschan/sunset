//! Honest test for the disconnect path: after the connection is
//! established, one side calls `close()`. The other side's pending
//! `recv_reliable` returns `Err`, and the per-peer supervisor in
//! `sunset-sync` would tear down cleanly. We don't poll for state or
//! sleep — we assert what a real caller would observe.

use std::sync::Arc;

use bytes::Bytes;
use tokio::task::LocalSet;

use sunset_core::crypto::constants::test_fast_params;
use sunset_core::{Ed25519Verifier, Identity, RelaySignaler, Room};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, RawConnection, RawTransport};
use sunset_sync_quic::QuicRawTransport;

#[tokio::test(flavor = "current_thread")]
async fn peer_disconnect_surfaces_as_recv_reliable_err() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let alice_id = Identity::from_secret_bytes(&[1u8; 32]);
            let bob_id = Identity::from_secret_bytes(&[2u8; 32]);
            let alice_pk = PeerId(alice_id.store_verifying_key());
            let bob_pk = PeerId(bob_id.store_verifying_key());

            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let room = Room::open_with_params("alpha", &test_fast_params()).unwrap();
            let fp = room.fingerprint();

            let alice_signaler = RelaySignaler::new(alice_id, fp.to_hex(), &store);
            let bob_signaler = RelaySignaler::new(bob_id, fp.to_hex(), &store);

            let alice_t = QuicRawTransport::bind(alice_signaler, alice_pk.clone(), vec![])
                .await
                .unwrap();
            let bob_t = QuicRawTransport::bind(bob_signaler, bob_pk.clone(), vec![])
                .await
                .unwrap();

            let bob_addr = PeerAddr::new(Bytes::from(format!(
                "quic://{}",
                hex::encode(bob_pk.verifying_key().as_bytes())
            )));

            let (a_conn, b_conn) = tokio::join!(alice_t.connect(bob_addr), bob_t.accept());
            let a_conn = a_conn.expect("alice connect");
            let b_conn = b_conn.expect("bob accept");

            // Confirm the pipe works one direction first.
            a_conn
                .send_reliable(Bytes::from_static(b"hi"))
                .await
                .unwrap();
            let got = b_conn.recv_reliable().await.unwrap();
            assert_eq!(got.as_ref(), b"hi");

            // Alice closes. Bob's NEXT recv_reliable must return Err —
            // not panic, not hang. This is what the per-peer supervisor
            // in sunset-sync watches for to declare the link dead.
            a_conn.close().await.unwrap();

            let err = b_conn
                .recv_reliable()
                .await
                .expect_err("bob recv_reliable must surface the close as Err");
            let msg = format!("{err}");
            assert!(
                msg.contains("quic recv") || msg.contains("closed") || msg.contains("Closed"),
                "expected close-related error, got: {msg}"
            );
        })
        .await;
}
