//! Two-peer voice round-trip over `BusImpl` + `TestNetwork`.
//!
//! Asserts the C2b wire format works end to end: alice encrypts a
//! VoicePacket::Frame with the room key, publishes via Bus, bob's
//! subscriber decrypts byte-for-byte the same packet AND bob's
//! `frame_liveness` transitions to Live.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures::StreamExt;
use rand_core::OsRng;

use sunset_core::Room;
use sunset_core::bus::{Bus, BusEvent, BusImpl};
use sunset_core::identity::{Identity, IdentityKey};
use sunset_core::liveness::{Liveness, LivenessState};
use sunset_store::{AcceptAllVerifier, Filter};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
use sunset_voice::packet::{VoicePacket, decrypt, encrypt};

#[tokio::test(flavor = "current_thread")]
async fn alice_encrypts_voice_frame_bob_decrypts_and_observes_live() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();

            let alice_id = Identity::generate(&mut OsRng);
            let bob_id = Identity::generate(&mut OsRng);
            let room = Room::open("test-room").unwrap();
            let room_fp_hex = room.fingerprint().to_hex();

            let alice_store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
            let bob_store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));

            let alice_peer = PeerId(alice_id.store_verifying_key());
            let bob_peer = PeerId(bob_id.store_verifying_key());

            let alice_transport = net.transport(
                alice_peer.clone(),
                PeerAddr::new(Bytes::from_static(b"alice")),
            );
            let bob_transport =
                net.transport(bob_peer.clone(), PeerAddr::new(Bytes::from_static(b"bob")));

            let alice_engine = Rc::new(SyncEngine::new(
                alice_store.clone(),
                alice_transport,
                SyncConfig::default(),
                alice_peer.clone(),
                Arc::new(alice_id.clone()) as Arc<dyn Signer>,
            ));
            let bob_engine = Rc::new(SyncEngine::new(
                bob_store.clone(),
                bob_transport,
                SyncConfig::default(),
                bob_peer.clone(),
                Arc::new(bob_id.clone()) as Arc<dyn Signer>,
            ));

            let alice_run = tokio::task::spawn_local({
                let e = alice_engine.clone();
                async move {
                    let _ = e.run().await;
                }
            });
            let bob_run = tokio::task::spawn_local({
                let e = bob_engine.clone();
                async move {
                    let _ = e.run().await;
                }
            });

            alice_engine
                .add_peer(PeerAddr::new(Bytes::from_static(b"bob")))
                .await
                .unwrap();

            let alice_bus =
                BusImpl::new(alice_store.clone(), alice_engine.clone(), alice_id.clone());
            let bob_bus = BusImpl::new(bob_store.clone(), bob_engine.clone(), bob_id.clone());

            let voice_prefix = Bytes::from(format!("voice/{room_fp_hex}/"));
            let mut bob_stream = bob_bus
                .subscribe(Filter::NamePrefix(voice_prefix.clone()))
                .await
                .unwrap();

            let bob_liveness = Liveness::new(Duration::from_millis(1000));
            let mut bob_live_sub = bob_liveness.subscribe().await;

            // Subscriptions need a moment to propagate via the engine's CRDT path.
            tokio::time::sleep(Duration::from_millis(50)).await;

            let original = VoicePacket::Frame {
                codec_id: "pcm-f32-le".to_string(),
                seq: 1,
                sender_time_ms: 1_700_000_000_000,
                payload: vec![0xAB; 3840],
            };
            let ev = encrypt(&room, 0, &alice_id.public(), &original, &mut OsRng).unwrap();
            let payload_bytes = postcard::to_stdvec(&ev).unwrap();
            let alice_pk_hex = hex::encode(alice_id.store_verifying_key().as_bytes());
            let name = Bytes::from(format!("voice/{room_fp_hex}/{alice_pk_hex}"));
            alice_bus
                .publish_ephemeral(name.clone(), Bytes::from(payload_bytes))
                .await
                .unwrap();

            let ev_bus = tokio::time::timeout(Duration::from_secs(2), bob_stream.next())
                .await
                .expect("bus event arrived in time")
                .expect("stream open");
            let datagram = match ev_bus {
                BusEvent::Ephemeral(d) => d,
                BusEvent::Durable { .. } => panic!("expected ephemeral"),
            };
            let sender = IdentityKey::from_store_verifying_key(&datagram.verifying_key).unwrap();
            let received_ev: sunset_voice::packet::EncryptedVoicePacket =
                postcard::from_bytes(&datagram.payload).unwrap();
            let decoded = decrypt(&room, 0, &sender, &received_ev).unwrap();
            assert_eq!(decoded, original);

            if let VoicePacket::Frame { sender_time_ms, .. } = decoded {
                let st = SystemTime::UNIX_EPOCH + Duration::from_millis(sender_time_ms);
                bob_liveness
                    .observe(PeerId(datagram.verifying_key.clone()), st)
                    .await;
            }

            let live_ev = tokio::time::timeout(Duration::from_secs(1), bob_live_sub.next())
                .await
                .expect("liveness event arrived")
                .expect("liveness stream open");
            assert_eq!(live_ev.peer.0, alice_id.store_verifying_key());
            assert_eq!(live_ev.state, LivenessState::Live);

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}
