//! Multi-peer correctness: one Bob accepting concurrent connect()s
//! from Alice and Charlie. Each side roundtrips a peer-identifying
//! message so we catch any cross-routing where Bob accepts a
//! connection but matches it to the wrong responder task.
//!
//! Honest test: real RelaySignaler, real holepunch, real QUIC per
//! peer. We assert end-to-end that Alice→Bob and Charlie→Bob each
//! exchange the correct bytes — the failure mode for the v1 FIFO
//! routing was a deterministic cross-mix where Alice's connection
//! ended up handed to the Charlie-acceptor (and vice versa).

use std::sync::Arc;

use bytes::Bytes;
use tokio::task::LocalSet;

use sunset_core::crypto::constants::test_fast_params;
use sunset_core::{Ed25519Verifier, Identity, RelaySignaler, Room};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, RawConnection, RawTransport};
use sunset_sync_quic::QuicRawTransport;

#[tokio::test(flavor = "current_thread")]
async fn bob_routes_concurrent_incoming_to_correct_responder() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let alice_id = Identity::from_secret_bytes(&[1u8; 32]);
            let bob_id = Identity::from_secret_bytes(&[2u8; 32]);
            let charlie_id = Identity::from_secret_bytes(&[3u8; 32]);
            let alice_pk = PeerId(alice_id.store_verifying_key());
            let bob_pk = PeerId(bob_id.store_verifying_key());
            let charlie_pk = PeerId(charlie_id.store_verifying_key());

            // Shared store + room: a single relay carries signaling
            // for all three peers, as would happen in production.
            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let room = Room::open_with_params("alpha", &test_fast_params()).unwrap();
            let fp = room.fingerprint();

            let alice_signaler = RelaySignaler::new(alice_id, fp.to_hex(), &store);
            let bob_signaler = RelaySignaler::new(bob_id, fp.to_hex(), &store);
            let charlie_signaler = RelaySignaler::new(charlie_id, fp.to_hex(), &store);

            let alice_t = QuicRawTransport::bind(alice_signaler, alice_pk.clone(), vec![])
                .await
                .unwrap();
            let bob_t = QuicRawTransport::bind(bob_signaler, bob_pk.clone(), vec![])
                .await
                .unwrap();
            let charlie_t = QuicRawTransport::bind(charlie_signaler, charlie_pk.clone(), vec![])
                .await
                .unwrap();

            let bob_addr_for_alice = PeerAddr::new(Bytes::from(format!(
                "quic://{}",
                hex::encode(bob_pk.verifying_key().as_bytes())
            )));
            let bob_addr_for_charlie = PeerAddr::new(Bytes::from(format!(
                "quic://{}",
                hex::encode(bob_pk.verifying_key().as_bytes())
            )));

            // Race three handshakes: Alice→Bob, Charlie→Bob, plus
            // Bob's two accepts (one for each). The order of accept
            // results is intentionally unspecified — the test asserts
            // that whichever way it lands, each pair received the
            // correct bytes.
            let (a_res, c_res, b_first_res, b_second_res) = tokio::join!(
                alice_t.connect(bob_addr_for_alice),
                charlie_t.connect(bob_addr_for_charlie),
                bob_t.accept(),
                bob_t.accept()
            );
            let a_conn = a_res.expect("alice connect");
            let c_conn = c_res.expect("charlie connect");
            let b1 = b_first_res.expect("bob first accept");
            let b2 = b_second_res.expect("bob second accept");

            // Alice and Charlie each send a unique tag. Bob's two
            // connections each receive exactly one tag. We don't
            // know which Bob-side conn corresponds to which sender,
            // so collect both reads and assert the SET matches.
            a_conn
                .send_reliable(Bytes::from_static(b"from-alice"))
                .await
                .unwrap();
            c_conn
                .send_reliable(Bytes::from_static(b"from-charlie"))
                .await
                .unwrap();
            let b1_got = b1.recv_reliable().await.unwrap();
            let b2_got = b2.recv_reliable().await.unwrap();

            let mut seen: Vec<&[u8]> = vec![b1_got.as_ref(), b2_got.as_ref()];
            seen.sort();
            let mut expected: Vec<&[u8]> = vec![b"from-alice", b"from-charlie"];
            expected.sort();
            assert_eq!(
                seen, expected,
                "Bob's two accepts received the wrong combined set of messages: \
                 b1={:?} b2={:?}",
                b1_got, b2_got,
            );

            // Roundtrip the reverse direction too, ensuring both
            // pipes are actually independent and routed correctly:
            b1.send_reliable(Bytes::copy_from_slice(b1_got.as_ref()))
                .await
                .unwrap();
            b2.send_reliable(Bytes::copy_from_slice(b2_got.as_ref()))
                .await
                .unwrap();
            let a_echo = a_conn.recv_reliable().await.unwrap();
            let c_echo = c_conn.recv_reliable().await.unwrap();
            assert_eq!(a_echo.as_ref(), b"from-alice");
            assert_eq!(c_echo.as_ref(), b"from-charlie");
        })
        .await;
}
