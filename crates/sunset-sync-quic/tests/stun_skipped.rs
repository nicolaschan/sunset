//! With `stun_servers = vec![]`, candidate discovery returns only the
//! local-interface addresses (no STUN-reflexive). Two peers on
//! 127.0.0.1 still complete the holepunch end-to-end, and the
//! confirmed candidate IS a loopback address. Confirms the
//! STUN-unreachable failure mode degrades gracefully.

use std::sync::Arc;

use bytes::Bytes;
use tokio::task::LocalSet;

use sunset_core::crypto::constants::test_fast_params;
use sunset_core::{Ed25519Verifier, Identity, RelaySignaler, Room};
use sunset_store_memory::MemoryStore;
use sunset_sync::{PeerAddr, PeerId, RawConnection, RawTransport};
use sunset_sync_quic::QuicRawTransport;

#[tokio::test(flavor = "current_thread")]
async fn stun_skipped_local_only_confirms_loopback() {
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

            // The two peers are on the same machine, so the confirmed
            // candidate must be local (loopback OR a private LAN IP);
            // it must NOT be a STUN-reflexive public address, since we
            // passed `stun_servers = []`. This is what "STUN-unreachable
            // degrades gracefully" actually means.
            for addr in [a_conn.remote_addr(), b_conn.remote_addr()] {
                let ip = addr.ip();
                assert!(
                    ip.is_loopback() || is_private(&ip),
                    "expected local-only addr, got {addr:?} (no STUN was queried so this \
                     can't be a STUN-reflexive public addr)"
                );
            }

            // Confirm the end-to-end pipe still works with stun_servers=[].
            a_conn
                .send_reliable(Bytes::from_static(b"ping"))
                .await
                .unwrap();
            let got = b_conn.recv_reliable().await.unwrap();
            assert_eq!(got.as_ref(), b"ping");
        })
        .await;
}

fn is_private(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_private() || v4.is_link_local() || v4.octets()[0] == 100 // RFC6598 CGNAT
        }
        std::net::IpAddr::V6(v6) => {
            // fc00::/7 (ULA) or fe80::/10 (link-local) — both non-public.
            let segments = v6.segments();
            (segments[0] & 0xfe00) == 0xfc00 || (segments[0] & 0xffc0) == 0xfe80
        }
    }
}
