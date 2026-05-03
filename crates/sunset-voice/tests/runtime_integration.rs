//! Integration tests for `VoiceRuntime` with an in-memory `Bus`.
//!
//! Uses tokio's `LocalSet` to spawn the runtime tasks alongside test
//! assertions. All `Bus` traffic loops back through a broadcast channel.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::LocalBoxStream;
use rand_chacha::rand_core::SeedableRng;

use sunset_core::Identity;
use sunset_core::Room;
use sunset_core::bus::BusEvent;
use sunset_store::SignedDatagram;
use sunset_sync::PeerId;
use sunset_voice::runtime::{
    Dialer, DynBus, FrameSink, PeerStateSink, VoicePeerState, VoiceRuntime,
};

/// Type alias to avoid clippy::type_complexity.
type DeliveredSink = Rc<RefCell<Vec<(PeerId, Vec<f32>)>>>;
/// Type alias to avoid clippy::type_complexity.
type DroppedSink = Rc<RefCell<Vec<PeerId>>>;
/// Type alias to avoid clippy::type_complexity.
type EventSink = Rc<RefCell<Vec<VoicePeerState>>>;

/// Minimal in-memory `DynBus` for tests. Supports `publish_ephemeral`
/// and `subscribe_voice_prefix`. Loopback is included
/// (publishes are visible to subscribers including the publisher).
///
/// Uses an mpsc list for fan-out. `inject` delivers a datagram directly
/// to all registered subscribers (used by tests to simulate inbound traffic).
struct TestBus {
    self_pk: sunset_store::VerifyingKey,
    /// Sinks registered by subscribe_voice_prefix calls. Guarded by a Mutex
    /// because publish_ephemeral is async and called from the runtime.
    sinks: tokio::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<SignedDatagram>>>,
    /// Broadcast channel for publish_ephemeral so tests can observe outbound traffic.
    obs_tx: tokio::sync::broadcast::Sender<SignedDatagram>,
}

impl TestBus {
    fn new(
        self_pk: sunset_store::VerifyingKey,
    ) -> (Arc<Self>, tokio::sync::broadcast::Sender<SignedDatagram>) {
        let (obs_tx, _) = tokio::sync::broadcast::channel(64);
        let bus = Arc::new(Self {
            self_pk,
            sinks: tokio::sync::Mutex::new(vec![]),
            obs_tx: obs_tx.clone(),
        });
        (bus, obs_tx)
    }

    /// Inject a datagram (from tests) into all subscribe streams.
    async fn inject(&self, dgram: SignedDatagram) {
        let sinks = self.sinks.lock().await;
        for sink in sinks.iter() {
            let _ = sink.send(dgram.clone());
        }
    }
}

#[async_trait(?Send)]
impl DynBus for TestBus {
    async fn publish_ephemeral(
        &self,
        name: Bytes,
        payload: Bytes,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Build a SignedDatagram with self as verifying_key.
        let dgram = SignedDatagram {
            verifying_key: self.self_pk.clone(),
            name,
            payload,
            signature: Bytes::new(),
        };
        // Fan out to subscribers (loopback).
        let sinks = self.sinks.lock().await;
        for sink in sinks.iter() {
            let _ = sink.send(dgram.clone());
        }
        // Also notify test observers.
        let _ = self.obs_tx.send(dgram);
        Ok(())
    }

    async fn subscribe_voice_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, BusEvent>, Box<dyn std::error::Error>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SignedDatagram>();
        // Register sink — all subsequent inject() and publish_ephemeral() calls
        // will fan out to this sender.
        self.sinks.lock().await.push(tx);

        let stream = async_stream::stream! {
            let mut r = rx;
            while let Some(d) = r.recv().await {
                if d.name.starts_with(&prefix) {
                    yield BusEvent::Ephemeral(d);
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

struct CountingDialer {
    calls: DroppedSink,
}
#[async_trait::async_trait(?Send)]
impl Dialer for CountingDialer {
    async fn ensure_direct(&self, peer: PeerId) {
        self.calls.borrow_mut().push(peer);
    }
}

struct RecordingFrameSink {
    delivered: DeliveredSink,
    dropped: DroppedSink,
}
impl FrameSink for RecordingFrameSink {
    fn deliver(&self, peer: &PeerId, pcm: &[f32]) {
        self.delivered
            .borrow_mut()
            .push((peer.clone(), pcm.to_vec()));
    }
    fn drop_peer(&self, peer: &PeerId) {
        self.dropped.borrow_mut().push(peer.clone());
    }
}

struct RecordingPeerStateSink {
    events: EventSink,
}
impl PeerStateSink for RecordingPeerStateSink {
    fn emit(&self, state: &VoicePeerState) {
        self.events.borrow_mut().push(state.clone());
    }
}

fn make_identity_and_room(seed_byte: u8) -> (Identity, Rc<Room>) {
    let seed = [seed_byte; 32];
    let identity = Identity::from_secret_bytes(&seed);
    let room = Rc::new(Room::open("test-room").unwrap());
    (identity, room)
}

#[tokio::test(flavor = "current_thread")]
async fn heartbeat_publishes_periodically_with_is_muted_flag() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(1);
            let pk = alice.store_verifying_key();
            let (bus_impl, tx) = TestBus::new(pk.clone());
            let bus: Arc<dyn DynBus> = bus_impl;

            let dialer_calls: DroppedSink = Rc::new(RefCell::new(vec![]));
            let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                calls: dialer_calls,
            });
            let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
                delivered: Rc::new(RefCell::new(vec![])),
                dropped: Rc::new(RefCell::new(vec![])),
            });
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: Rc::new(RefCell::new(vec![])),
            });

            let (runtime, tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                alice.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            tokio::task::spawn_local(tasks.heartbeat);

            // Subscribe ahead of the first heartbeat.
            let mut rx = tx.subscribe();

            // Initial heartbeat fires within ~10 ms, then every 2 s.
            // Speed-test: collect one heartbeat under a 3 s timeout.
            let hb = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let d = rx.recv().await.unwrap();
                    if d.name.starts_with(b"voice/") {
                        return d;
                    }
                }
            })
            .await
            .expect("first heartbeat within 3s");

            // Decode and verify is_muted == false (default).
            let ev: sunset_voice::packet::EncryptedVoicePacket =
                postcard::from_bytes(&hb.payload).unwrap();
            let pkt = sunset_voice::packet::decrypt(&room, 0, &alice.public(), &ev).unwrap();
            match pkt {
                sunset_voice::packet::VoicePacket::Heartbeat { is_muted, .. } => {
                    assert!(!is_muted, "default is_muted should be false");
                }
                _ => panic!("expected Heartbeat"),
            }

            // Toggle mute and capture another heartbeat.
            runtime.set_muted(true);
            let hb2 = tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    let d = rx.recv().await.unwrap();
                    if d.name.starts_with(b"voice/") {
                        return d;
                    }
                }
            })
            .await
            .expect("second heartbeat within 4s");
            let ev2: sunset_voice::packet::EncryptedVoicePacket =
                postcard::from_bytes(&hb2.payload).unwrap();
            let pkt2 = sunset_voice::packet::decrypt(&room, 0, &alice.public(), &ev2).unwrap();
            match pkt2 {
                sunset_voice::packet::VoicePacket::Heartbeat { is_muted, .. } => {
                    assert!(is_muted);
                }
                _ => panic!("expected Heartbeat"),
            }

            drop(runtime); // task should exit
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn send_pcm_publishes_frame_when_unmuted() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(2);
            let pk = alice.store_verifying_key();
            let (bus_impl, tx) = TestBus::new(pk.clone());
            let bus: Arc<dyn DynBus> = bus_impl;
            let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                calls: Rc::new(RefCell::new(vec![])),
            });
            let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
                delivered: Rc::new(RefCell::new(vec![])),
                dropped: Rc::new(RefCell::new(vec![])),
            });
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: Rc::new(RefCell::new(vec![])),
            });

            let (runtime, _tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                alice.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            let mut rx = tx.subscribe();

            let pcm: Vec<f32> = (0..960).map(|i| (i as f32) / 1000.0).collect();
            runtime.send_pcm(&pcm);

            let frame = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let d = rx.recv().await.unwrap();
                    if d.name.starts_with(b"voice/") {
                        return d;
                    }
                }
            })
            .await
            .expect("frame within 1s");

            let ev: sunset_voice::packet::EncryptedVoicePacket =
                postcard::from_bytes(&frame.payload).unwrap();
            let pkt = sunset_voice::packet::decrypt(&room, 0, &alice.public(), &ev).unwrap();
            let bytes = match pkt {
                sunset_voice::packet::VoicePacket::Frame { payload, .. } => payload,
                _ => panic!("expected Frame"),
            };
            let mut decoder = sunset_voice::VoiceDecoder::new().unwrap();
            let decoded = decoder.decode(&bytes).unwrap();
            assert_eq!(decoded, pcm);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn send_pcm_drops_frames_when_muted() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(3);
            let pk = alice.store_verifying_key();
            let (bus_impl, tx) = TestBus::new(pk.clone());
            let bus: Arc<dyn DynBus> = bus_impl;
            let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                calls: Rc::new(RefCell::new(vec![])),
            });
            let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
                delivered: Rc::new(RefCell::new(vec![])),
                dropped: Rc::new(RefCell::new(vec![])),
            });
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: Rc::new(RefCell::new(vec![])),
            });
            let (runtime, _tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                alice.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            runtime.set_muted(true);

            let mut rx = tx.subscribe();
            let pcm = vec![0.1_f32; 960];
            runtime.send_pcm(&pcm);

            // Wait briefly: no frame packet should arrive (heartbeats may).
            let r = tokio::time::timeout(Duration::from_millis(300), async {
                loop {
                    let d = rx.recv().await.unwrap();
                    if d.name.starts_with(b"voice/") {
                        // Decrypt and check whether it's a Frame.
                        let ev: sunset_voice::packet::EncryptedVoicePacket =
                            postcard::from_bytes(&d.payload).unwrap();
                        let pkt =
                            sunset_voice::packet::decrypt(&room, 0, &alice.public(), &ev).unwrap();
                        if matches!(pkt, sunset_voice::packet::VoicePacket::Frame { .. }) {
                            return d;
                        }
                    }
                }
            })
            .await;
            assert!(r.is_err(), "no Frame should be published while muted");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn subscribe_decrypts_frame_and_pushes_to_jitter() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(4);
            let (bob, _) = make_identity_and_room(5);
            let alice_pk = alice.store_verifying_key();

            let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bob_bus: Arc<dyn DynBus> = bob_bus_impl.clone();
            let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                calls: Rc::new(RefCell::new(vec![])),
            });
            let delivered: DeliveredSink = Rc::new(RefCell::new(vec![]));
            let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
                delivered: delivered.clone(),
                dropped: Rc::new(RefCell::new(vec![])),
            });
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: Rc::new(RefCell::new(vec![])),
            });

            let (runtime, tasks) = VoiceRuntime::new(
                bob_bus,
                room.clone(),
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            tokio::task::spawn_local(tasks.subscribe);

            // Yield to let the subscribe task start up and register its sink.
            tokio::task::yield_now().await;

            // Alice publishes one Frame as if she were on the network.
            let pcm: Vec<f32> = (0..960).map(|i| (i as f32) * 0.001).collect();
            let mut enc = sunset_voice::VoiceEncoder::new().unwrap();
            let bytes = enc.encode(&pcm).unwrap();
            let pkt = sunset_voice::packet::VoicePacket::Frame {
                codec_id: sunset_voice::CODEC_ID.to_string(),
                seq: 1,
                sender_time_ms: 1000,
                payload: bytes,
            };
            let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(42);
            let ev =
                sunset_voice::packet::encrypt(&room, 0, &alice.public(), &pkt, &mut rng).unwrap();
            let payload = postcard::to_stdvec(&ev).unwrap();
            let room_fp = room.fingerprint().to_hex();
            let sender_pk = hex::encode(alice_pk.as_bytes());
            let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}"));

            // Inject as if it came through the bus from alice.
            let dgram = SignedDatagram {
                verifying_key: alice_pk.clone(),
                name,
                payload: Bytes::from(payload),
                signature: Bytes::new(),
            };
            bob_bus_impl.inject(dgram).await;

            // Wait for the subscribe loop to push the frame into the jitter buffer.
            let alice_peer = PeerId(alice_pk.clone());
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if runtime.test_jitter_len(&alice_peer) >= 1 {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("frame pushed to jitter within 1s");

            assert_eq!(runtime.test_jitter_len(&alice_peer), 1);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn combiner_emits_state_on_heartbeat() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(6);
            let (bob, _) = make_identity_and_room(7);
            let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bob_bus: Arc<dyn DynBus> = bob_bus_impl.clone();
            let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                calls: Rc::new(RefCell::new(vec![])),
            });
            let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
                delivered: Rc::new(RefCell::new(vec![])),
                dropped: Rc::new(RefCell::new(vec![])),
            });
            let events: EventSink = Rc::new(RefCell::new(vec![]));
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: events.clone(),
            });

            let (_runtime, tasks) = VoiceRuntime::new(
                bob_bus,
                room.clone(),
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            tokio::task::spawn_local(tasks.subscribe);
            tokio::task::spawn_local(tasks.combiner);

            // Yield to let subscribe and combiner tasks start up.
            tokio::task::yield_now().await;

            // Inject one Heartbeat from alice with is_muted=true.
            let pkt = sunset_voice::packet::VoicePacket::Heartbeat {
                sent_at_ms: 5000,
                is_muted: true,
            };
            let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(99);
            let ev =
                sunset_voice::packet::encrypt(&room, 0, &alice.public(), &pkt, &mut rng).unwrap();
            let payload = postcard::to_stdvec(&ev).unwrap();
            let room_fp = room.fingerprint().to_hex();
            let sender_pk = hex::encode(alice.store_verifying_key().as_bytes());
            let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}"));
            let dgram = SignedDatagram {
                verifying_key: alice.store_verifying_key(),
                name,
                payload: Bytes::from(payload),
                signature: Bytes::new(),
            };
            bob_bus_impl.inject(dgram).await;

            // Wait for emitted state.
            let result = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let Some(ev) = events.borrow().last().cloned() {
                        return ev;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("emit within 1s");

            assert_eq!(result.peer, PeerId(alice.store_verifying_key()));
            assert!(result.in_call);
            assert!(!result.talking);
            assert!(result.is_muted);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_connect_dials_first_heartbeat_only_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(8);
            let (bob, _) = make_identity_and_room(9);
            let (bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bus: Arc<dyn DynBus> = bus_impl.clone();
            let calls: DroppedSink = Rc::new(RefCell::new(vec![]));
            let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                calls: calls.clone(),
            });
            let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
                delivered: Rc::new(RefCell::new(vec![])),
                dropped: Rc::new(RefCell::new(vec![])),
            });
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: Rc::new(RefCell::new(vec![])),
            });
            let (_runtime, tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            tokio::task::spawn_local(tasks.subscribe);
            tokio::task::spawn_local(tasks.auto_connect);

            // Yield to let subscribe and auto_connect tasks start up.
            tokio::task::yield_now().await;

            // Three heartbeats from alice — only the first should trigger ensure_direct.
            for _ in 0..3 {
                let pkt = sunset_voice::packet::VoicePacket::Heartbeat {
                    sent_at_ms: 1000,
                    is_muted: false,
                };
                let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(0xab);
                let ev = sunset_voice::packet::encrypt(&room, 0, &alice.public(), &pkt, &mut rng)
                    .unwrap();
                let payload = postcard::to_stdvec(&ev).unwrap();
                let room_fp = room.fingerprint().to_hex();
                let sender_pk = hex::encode(alice.store_verifying_key().as_bytes());
                let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}"));
                bus_impl
                    .inject(SignedDatagram {
                        verifying_key: alice.store_verifying_key(),
                        name,
                        payload: Bytes::from(payload),
                        signature: Bytes::new(),
                    })
                    .await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(
                calls.borrow().len(),
                1,
                "ensure_direct must be called exactly once"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn jitter_pump_delivers_at_20ms_cadence_and_pads_silence() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(10);
            let (bus_impl, _tx) = TestBus::new(alice.store_verifying_key());
            let bus: Arc<dyn DynBus> = bus_impl;
            let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                calls: Rc::new(RefCell::new(vec![])),
            });
            let delivered: DeliveredSink = Rc::new(RefCell::new(vec![]));
            let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
                delivered: delivered.clone(),
                dropped: Rc::new(RefCell::new(vec![])),
            });
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: Rc::new(RefCell::new(vec![])),
            });
            let (runtime, tasks) = VoiceRuntime::new(
                bus,
                room,
                alice.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );

            let peer = PeerId(alice.store_verifying_key());
            let frame1: Vec<f32> = (0..960).map(|i| i as f32 * 0.001).collect();
            let frame2: Vec<f32> = (0..960).map(|i| i as f32 * 0.002).collect();

            // Push two frames directly into the jitter buffer.
            runtime.test_push_frame(peer.clone(), frame1.clone());
            runtime.test_push_frame(peer.clone(), frame2.clone());

            tokio::task::spawn_local(tasks.jitter_pump);

            // Poll for 1st delivery (frame1).
            tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    if !delivered.borrow().is_empty() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("1st frame delivered");
            assert_eq!(delivered.borrow()[0].1, frame1);

            // Poll for 2nd delivery (frame2).
            tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    if delivered.borrow().len() >= 2 {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("2nd frame delivered");
            assert_eq!(delivered.borrow()[1].1, frame2);

            // Poll for 3rd delivery (repeat-last = frame2, first underrun).
            tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    if delivered.borrow().len() >= 3 {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("3rd frame delivered (repeat last)");
            assert_eq!(
                delivered.borrow()[2].1,
                frame2,
                "first underrun = repeat last"
            );

            // Poll for 4th delivery (silence, second underrun).
            tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    if delivered.borrow().len() >= 4 {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("4th frame delivered (silence)");
            let silence = vec![0.0_f32; 960];
            assert_eq!(
                delivered.borrow()[3].1,
                silence,
                "second underrun = silence"
            );

            // Push a new frame — next pump cycle delivers it normally.
            // We track how many frames have been delivered so far to find frame3's position.
            let frame3: Vec<f32> = (0..960).map(|i| i as f32 * 0.003).collect();
            // Push frame3 immediately after observing the silence delivery.
            // The next pump tick will pick it up and deliver it normally.
            runtime.test_push_frame(peer.clone(), frame3.clone());
            let base_len = delivered.borrow().len();
            tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    if delivered.borrow().len() > base_len {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("frame3 delivered");
            // Find frame3 in the delivered list — it may not be at index 4 if extra
            // silence frames were pumped before we pushed it.
            let deliveries = delivered.borrow();
            let frame3_pos = deliveries
                .iter()
                .position(|(_, f)| f == &frame3)
                .expect("frame3 must appear in delivered list");
            assert_eq!(deliveries[frame3_pos].1, frame3);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_runtime_terminates_all_tasks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(11);
            let (bus_impl, _tx) = TestBus::new(alice.store_verifying_key());
            let bus: Arc<dyn DynBus> = bus_impl;
            let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                calls: Rc::new(RefCell::new(vec![])),
            });
            let frame_sink: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
                delivered: Rc::new(RefCell::new(vec![])),
                dropped: Rc::new(RefCell::new(vec![])),
            });
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: Rc::new(RefCell::new(vec![])),
            });
            let (runtime, tasks) =
                VoiceRuntime::new(bus, room, alice, dialer, frame_sink, peer_state_sink);

            let handles = vec![
                tokio::task::spawn_local(tasks.heartbeat),
                tokio::task::spawn_local(tasks.subscribe),
                tokio::task::spawn_local(tasks.combiner),
                tokio::task::spawn_local(tasks.auto_connect),
                tokio::task::spawn_local(tasks.jitter_pump),
            ];

            drop(runtime);
            // Allow each task to observe the upgrade failure.
            tokio::time::sleep(Duration::from_millis(100)).await;
            for h in handles {
                assert!(
                    tokio::time::timeout(Duration::from_millis(500), h)
                        .await
                        .is_ok(),
                    "task should finish after Drop"
                );
            }
        })
        .await;
}
