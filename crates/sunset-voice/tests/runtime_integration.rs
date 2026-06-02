//! Integration tests for `VoiceRuntime` with an in-memory `Bus`.
//!
//! Uses tokio's `LocalSet` to spawn the runtime tasks alongside test
//! assertions. All `Bus` traffic loops back through a broadcast channel.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::LocalBoxStream;
use rand_chacha::rand_core::SeedableRng;

use sunset_core::Identity;
use sunset_core::Room;
use sunset_core::bus::BusEvent;
use sunset_store::{ContentBlock, SignedDatagram, SignedKvEntry, VerifyingKey};
use sunset_sync::PeerId;
use sunset_voice::runtime::{
    Dialer, DynBus, FrameSink, PeerStateSink, VoicePeerState, VoiceRuntime,
};

/// Type alias to avoid clippy::type_complexity.
type DeliveredSink = Rc<RefCell<Vec<(PeerId, u32, Vec<f32>)>>>;
/// Type alias to avoid clippy::type_complexity.
type DroppedSink = Rc<RefCell<Vec<PeerId>>>;
/// Type alias to avoid clippy::type_complexity.
type EventSink = Rc<RefCell<Vec<VoicePeerState>>>;

/// Minimal in-memory `DynBus` for tests. Supports ephemeral and durable
/// publish + subscribe. Loopback is included (publishes are visible to
/// subscribers including the publisher).
///
/// `inject` and `inject_durable` deliver traffic directly to all registered
/// subscribers (used by tests to simulate inbound traffic).
struct TestBus {
    self_pk: sunset_store::VerifyingKey,
    /// Sinks registered by subscribe_voice_prefix calls. Guarded by a Mutex
    /// because publish_ephemeral is async and called from the runtime.
    ephemeral_sinks: tokio::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<SignedDatagram>>>,
    /// Sinks registered by subscribe_prefix calls (for durable entries).
    durable_sinks: tokio::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<SignedKvEntry>>>,
    /// Broadcast channel for publish_ephemeral so tests can observe outbound traffic.
    obs_tx: tokio::sync::broadcast::Sender<SignedDatagram>,
}

impl TestBus {
    fn new(
        self_pk: sunset_store::VerifyingKey,
    ) -> (Rc<Self>, tokio::sync::broadcast::Sender<SignedDatagram>) {
        let (obs_tx, _) = tokio::sync::broadcast::channel(64);
        let bus = Rc::new(Self {
            self_pk,
            ephemeral_sinks: tokio::sync::Mutex::new(vec![]),
            durable_sinks: tokio::sync::Mutex::new(vec![]),
            obs_tx: obs_tx.clone(),
        });
        (bus, obs_tx)
    }

    /// Inject an ephemeral datagram (from tests) into all ephemeral subscribe streams.
    async fn inject(&self, dgram: SignedDatagram) {
        let sinks = self.ephemeral_sinks.lock().await;
        for sink in sinks.iter() {
            let _ = sink.send(dgram.clone());
        }
    }

    /// Inject a durable KV entry (from tests) into all durable subscribe streams.
    async fn inject_durable(&self, entry: SignedKvEntry) {
        let sinks = self.durable_sinks.lock().await;
        for sink in sinks.iter() {
            let _ = sink.send(entry.clone());
        }
    }
}

#[async_trait(?Send)]
impl DynBus for TestBus {
    async fn publish_ephemeral(
        &self,
        name: Bytes,
        seq: u64,
        payload: Bytes,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Build a SignedDatagram with self as verifying_key, stamping the
        // caller's per-stream seq onto the envelope.
        let dgram = SignedDatagram {
            verifying_key: self.self_pk.clone(),
            name,
            payload,
            seq,
            signature: Bytes::new(),
        };
        // Fan out to ephemeral subscribers (loopback).
        let sinks = self.ephemeral_sinks.lock().await;
        for sink in sinks.iter() {
            let _ = sink.send(dgram.clone());
        }
        // Also notify test observers.
        let _ = self.obs_tx.send(dgram);
        Ok(())
    }

    async fn publish_durable(
        &self,
        entry: SignedKvEntry,
        _block: Option<ContentBlock>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Fan out to durable subscribers (loopback).
        let sinks = self.durable_sinks.lock().await;
        for sink in sinks.iter() {
            let _ = sink.send(entry.clone());
        }
        Ok(())
    }

    async fn subscribe_voice_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, BusEvent>, Box<dyn std::error::Error>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SignedDatagram>();
        // Register sink — all subsequent inject() and publish_ephemeral() calls
        // will fan out to this sender.
        self.ephemeral_sinks.lock().await.push(tx);

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

    async fn subscribe_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, BusEvent>, Box<dyn std::error::Error>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SignedKvEntry>();
        // Register sink — all subsequent inject_durable() and publish_durable() calls
        // will fan out to this sender.
        self.durable_sinks.lock().await.push(tx);

        let stream = async_stream::stream! {
            let mut r = rx;
            while let Some(entry) = r.recv().await {
                if entry.name.starts_with(&prefix) {
                    yield BusEvent::Durable { entry, block: None };
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

/// Dialer test stub that records BOTH `ensure_direct` and `release`
/// calls so tests can assert the auto-connect FSM's cleanup path
/// (membership-stale → release → fresh dial on the next presence
/// event) without depending on the production WebDialer.
struct ReleaseTrackingDialer {
    dial_calls: DroppedSink,
    release_calls: DroppedSink,
}
#[async_trait::async_trait(?Send)]
impl Dialer for ReleaseTrackingDialer {
    async fn ensure_direct(&self, peer: PeerId) {
        self.dial_calls.borrow_mut().push(peer);
    }
    async fn release(&self, peer: PeerId) {
        self.release_calls.borrow_mut().push(peer);
    }
}

struct RecordingFrameSink {
    delivered: DeliveredSink,
    dropped: DroppedSink,
}
impl FrameSink for RecordingFrameSink {
    fn deliver(&self, peer: &PeerId, seq: u32, pcm: &[f32]) {
        self.delivered
            .borrow_mut()
            .push((peer.clone(), seq, pcm.to_vec()));
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

/// Build two identities sharing a room, with `self_pk < other_pk`. The
/// auto-connect FSM only dials when our pubkey is lexicographically
/// smaller than the peer's (glare avoidance), so tests that exercise
/// the dial path need `self` (the runtime owner) to be the smaller side.
/// Tries seed bytes deterministically until the inequality holds.
fn make_pair_self_smaller(seed_a: u8, seed_b: u8) -> (Identity, Identity, Rc<Room>) {
    let (cand_a, room) = make_identity_and_room(seed_a);
    let (cand_b, _) = make_identity_and_room(seed_b);
    let (self_id, other_id) =
        if cand_a.store_verifying_key().as_bytes() < cand_b.store_verifying_key().as_bytes() {
            (cand_a, cand_b)
        } else {
            (cand_b, cand_a)
        };
    (self_id, other_id, room)
}

/// Build a signed durable voice-presence entry for `sender` in `room`.
fn make_presence_entry(sender: &Identity, room: &Room) -> SignedKvEntry {
    use sunset_store::canonical::signing_payload;

    let room_fp = room.fingerprint().to_hex();
    let sender_pk_hex = hex::encode(sender.store_verifying_key().as_bytes());
    let name = Bytes::from(format!("voice-presence/{room_fp}/{sender_pk_hex}"));
    let block = ContentBlock {
        data: Bytes::new(),
        references: vec![],
    };
    let value_hash = block.hash();
    let now_ms = 1_000_000u64; // fixed for determinism
    let mut entry = SignedKvEntry {
        verifying_key: sender.store_verifying_key(),
        name,
        value_hash,
        priority: now_ms,
        expires_at: Some(now_ms + 6_000),
        signature: Bytes::new(),
    };
    let payload = signing_payload(&entry);
    let sig = sender.sign(&payload);
    entry.signature = Bytes::copy_from_slice(&sig.to_bytes());
    entry
}

#[tokio::test(flavor = "current_thread")]
async fn heartbeat_publishes_periodically_with_is_muted_flag() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(1);
            let pk = alice.store_verifying_key();
            let (bus_impl, tx) = TestBus::new(pk.clone());
            let bus: Rc<dyn DynBus> = bus_impl;

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
            // Runtimes default to observer mode; the heartbeat task only
            // publishes once the local user has joined the call.
            runtime.set_active(true);
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
            let bus: Rc<dyn DynBus> = bus_impl;
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
            // `send_pcm` drops frames when the runtime is in observer
            // mode (the default after `new`); activate so the publish
            // path runs.
            runtime.set_active(true);
            let mut rx = tx.subscribe();

            // Default quality is `Maximum` (stereo): 1920 interleaved
            // samples per frame.
            let pcm: Vec<f32> = (0..1920).map(|i| (i as f32) / 1000.0).collect();
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
            // Opus is lossy; we just need the frame to round-trip
            // through encrypt → bus → decrypt → decode and produce
            // the right-sized stereo PCM frame.
            assert_eq!(
                decoded.len(),
                sunset_voice::FRAME_SAMPLES_PER_CHANNEL * sunset_voice::PLAYBACK_CHANNELS as usize
            );
            assert!(decoded.iter().all(|s| s.is_finite()));
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
            let bus: Rc<dyn DynBus> = bus_impl;
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
            runtime.set_active(true);
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
async fn subscribe_decodes_frame_and_delivers_to_sink() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(4);
            let (bob, _) = make_identity_and_room(5);
            let alice_pk = alice.store_verifying_key();

            let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
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

            let (_runtime, tasks) = VoiceRuntime::new(
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
            let mut enc =
                sunset_voice::VoiceEncoder::new(sunset_voice::VoiceQuality::Voice).unwrap();
            let bytes = enc.encode(&pcm).unwrap();
            let pkt = sunset_voice::packet::VoicePacket::Frame {
                codec_id: sunset_voice::CODEC_ID.to_string(),
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

            // Inject as if it came through the bus from alice. The
            // authoritative seq is on the envelope (42), not in the packet.
            let dgram = SignedDatagram {
                verifying_key: alice_pk.clone(),
                name,
                payload: Bytes::from(payload),
                seq: 42,
                signature: Bytes::new(),
            };
            bob_bus_impl.inject(dgram).await;

            // Wait for the subscribe loop to decode + deliver. The decoder
            // upmixes mono Opus to stereo, so the delivered PCM is the
            // mono-source frame interleaved L=R, length 1920.
            let alice_peer = PeerId(alice_pk.clone());
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if !delivered.borrow().is_empty() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("frame delivered to sink within 1s");

            let snapshot = delivered.borrow();
            assert_eq!(snapshot.len(), 1, "exactly one frame delivered");
            let (peer, seq, pcm) = &snapshot[0];
            assert_eq!(peer, &alice_peer);
            assert_eq!(*seq, 42, "envelope seq must be propagated to sink");
            assert_eq!(
                pcm.len(),
                sunset_voice::FRAME_SAMPLES_PER_CHANNEL * sunset_voice::PLAYBACK_CHANNELS as usize
            );
            assert!(pcm.iter().all(|s| s.is_finite()));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn deafened_skips_decode_and_delivery() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(60);
            let (bob, _) = make_identity_and_room(61);
            let alice_pk = alice.store_verifying_key();

            let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
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
            runtime.set_deafened(true);
            tokio::task::spawn_local(tasks.subscribe);
            tokio::task::yield_now().await;

            let pcm: Vec<f32> = (0..960).map(|i| (i as f32) * 0.001).collect();
            let mut enc =
                sunset_voice::VoiceEncoder::new(sunset_voice::VoiceQuality::Voice).unwrap();
            let bytes = enc.encode(&pcm).unwrap();
            let pkt = sunset_voice::packet::VoicePacket::Frame {
                codec_id: sunset_voice::CODEC_ID.to_string(),
                sender_time_ms: 1000,
                payload: bytes,
            };
            let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(7);
            let ev =
                sunset_voice::packet::encrypt(&room, 0, &alice.public(), &pkt, &mut rng).unwrap();
            let payload = postcard::to_stdvec(&ev).unwrap();
            let room_fp = room.fingerprint().to_hex();
            let sender_pk = hex::encode(alice_pk.as_bytes());
            let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}"));
            let dgram = SignedDatagram {
                verifying_key: alice_pk.clone(),
                name,
                payload: Bytes::from(payload),
                seq: 0,
                signature: Bytes::new(),
            };
            bob_bus_impl.inject(dgram).await;

            // Wait a tick — if the deafened gate is broken, the sink
            // would receive a frame here. Then verify nothing arrived.
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(
                delivered.borrow().is_empty(),
                "deafened receiver must not deliver any frames"
            );
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
            let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
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
                seq: 0,
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

/// Regression: a peer that sends frames but never sends a heartbeat must
/// have `in_call` flip back to `false` once frames stop. Before this was
/// fixed, `in_call` was sticky-true: frame Stale dropped `talking` but
/// couldn't safely flip `in_call` (it didn't know membership state), and
/// no membership Stale ever fired (no entry to time out for). The combiner
/// now tracks frame_alive and membership_alive independently and computes
/// `in_call = frame_alive || membership_alive`, so frame Stale alone is
/// sufficient when membership has never fired Live.
///
/// Mirrors the hard-departure churn case where a peer closes the tab
/// before its first heartbeat reaches us: only the in-flight frames
/// registered the peer, and we still need to evict them within the
/// liveness budget.
#[tokio::test(flavor = "current_thread")]
async fn combiner_evicts_peer_seen_only_via_frames() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(40);
            let (bob, _) = make_identity_and_room(41);
            let alice_pk = alice.store_verifying_key();

            let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
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
            tokio::task::yield_now().await;

            // Inject one frame from alice — no heartbeat ever.
            let pcm: Vec<f32> = (0..960).map(|i| (i as f32) * 0.001).collect();
            let mut enc =
                sunset_voice::VoiceEncoder::new(sunset_voice::VoiceQuality::Voice).unwrap();
            let bytes = enc.encode(&pcm).unwrap();
            let now_ms: u64 = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let pkt = sunset_voice::packet::VoicePacket::Frame {
                codec_id: sunset_voice::CODEC_ID.to_string(),
                sender_time_ms: now_ms,
                payload: bytes,
            };
            let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(401);
            let ev =
                sunset_voice::packet::encrypt(&room, 0, &alice.public(), &pkt, &mut rng).unwrap();
            let payload = postcard::to_stdvec(&ev).unwrap();
            let room_fp = room.fingerprint().to_hex();
            let sender_pk = hex::encode(alice_pk.as_bytes());
            let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}"));
            let dgram = SignedDatagram {
                verifying_key: alice_pk.clone(),
                name,
                payload: Bytes::from(payload),
                seq: 0,
                signature: Bytes::new(),
            };
            bob_bus_impl.inject(dgram).await;

            // Wait for in_call=true emission (frame Live).
            let alice_peer = PeerId(alice_pk.clone());
            let live = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if events
                        .borrow()
                        .iter()
                        .any(|e| e.peer == alice_peer && e.in_call && e.talking)
                    {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            assert!(live.is_ok(), "expected frame Live emission within 2s");

            // No more frames + no heartbeats → frame_liveness must go Stale
            // within FRAME_STALE_AFTER (1000ms) + sweep cycle (~500ms).
            // Then in_call must flip false because membership_alive is also
            // false (never observed). Allow up to 4s for safety.
            let stale = tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    if events
                        .borrow()
                        .iter()
                        .any(|e| e.peer == alice_peer && !e.in_call && !e.talking)
                    {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await;
            assert!(
                stale.is_ok(),
                "expected in_call=false after frame Stale, got events: {:?}",
                events.borrow()
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_connect_dials_on_voice_presence_only_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            // bob is the runtime owner; alice is the remote peer whose
            // voice-presence we'll inject. Glare avoidance only fires
            // the dial when self_pk < peer_pk, so we force that ordering.
            let (bob, alice, room) = make_pair_self_smaller(9, 8);
            let (bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bus: Rc<dyn DynBus> = bus_impl.clone();
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
            let (runtime, tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            // Active mode is required for auto_connect to dial; observer
            // mode drains the presence stream without entering the FSM.
            runtime.set_active(true);
            tokio::task::spawn_local(tasks.auto_connect);

            // Yield to let auto_connect task start up and register its sink.
            tokio::task::yield_now().await;

            // Three voice-presence durable entries from alice — only the first
            // should trigger ensure_direct.
            let entry = make_presence_entry(&alice, &room);
            for _ in 0..3 {
                bus_impl.inject_durable(entry.clone()).await;
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

/// Regression for the leave-then-rejoin failure mode: when
/// `membership_liveness` decides a peer is `Stale`, the auto-connect
/// FSM must call `Dialer::release(peer)` so the host can tear down
/// any session-scoped state (in production: the supervisor's
/// direct-WebRTC intent). Without this, the next voice-presence
/// event for the same peer triggers `ensure_direct` whose
/// `connect_direct` is deduplicated by the supervisor against a
/// stale `Connected` / `Backoff` intent — the user-visible
/// symptom is "rejoined but no audio until the engine's 45 s
/// heartbeat timeout."
///
/// We assert two things in sequence:
///   1. `release(peer)` is called when membership goes Stale, AND
///   2. The auto_connect_state for `peer` is reset to `Unknown`, so
///      that a follow-up presence event triggers a fresh
///      `ensure_direct` call (rather than being skipped by the
///      Dialing-state dedup inside auto_connect itself).
#[tokio::test(flavor = "current_thread")]
async fn auto_connect_releases_dialer_on_membership_stale_and_redials() {
    use sunset_core::liveness::Liveness;
    use sunset_store::VerifyingKey;

    tokio::task::LocalSet::new()
        .run_until(async {
            // bob is the runtime owner; alice is the remote peer.
            // Force `bob < alice` so the dialing arm of glare avoidance
            // fires (otherwise bob is the acceptor and never dials).
            let (bob, alice, room) = make_pair_self_smaller(9, 8);
            let (bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bus: Rc<dyn DynBus> = bus_impl.clone();

            let dial_calls: DroppedSink = Rc::new(RefCell::new(vec![]));
            let release_calls: DroppedSink = Rc::new(RefCell::new(vec![]));
            let dialer: Rc<dyn Dialer> = Rc::new(ReleaseTrackingDialer {
                dial_calls: dial_calls.clone(),
                release_calls: release_calls.clone(),
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
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            // Activate before spawning — auto_connect waits for
            // `is_active=true` before subscribing to the presence
            // stream, so without this flip the injected presence event
            // is queued but never consumed.
            runtime.set_active(true);
            tokio::task::spawn_local(tasks.auto_connect);
            // Give the auto_connect task time to clear its poll-for-active
            // loop (100 ms cadence) and reach subscribe_prefix.
            tokio::time::sleep(Duration::from_millis(200)).await;

            // First presence event from alice: should trigger one dial.
            let entry = make_presence_entry(&alice, &room);
            bus_impl.inject_durable(entry.clone()).await;
            tokio::time::sleep(Duration::from_millis(100)).await;

            assert_eq!(
                dial_calls.borrow().len(),
                1,
                "first voice-presence must trigger exactly one ensure_direct"
            );
            assert!(
                release_calls.borrow().is_empty(),
                "no release before membership goes stale"
            );

            // Force membership_liveness to fire Stale for alice. The
            // existing dropping_runtime_terminates_all_tasks test uses
            // the same observe + run_sweep pattern.
            let (_, membership_liveness): (std::sync::Arc<Liveness>, std::sync::Arc<Liveness>) =
                runtime.test_liveness();
            let alice_peer = PeerId(VerifyingKey::new(Bytes::copy_from_slice(
                alice.store_verifying_key().as_bytes(),
            )));
            membership_liveness
                .observe(alice_peer.clone(), std::time::SystemTime::UNIX_EPOCH)
                .await;
            membership_liveness.run_sweep().await;

            // Yield enough for the auto_connect select! arm to consume
            // the Stale event and call dialer.release.
            tokio::time::sleep(Duration::from_millis(50)).await;

            let releases = release_calls.borrow().clone();
            assert_eq!(
                releases.len(),
                1,
                "release should fire exactly once on the Stale event"
            );
            assert_eq!(releases[0], alice_peer);

            // Re-inject the presence entry (simulating the peer rejoining
            // and republishing). Since auto_connect_state was reset to
            // Unknown by the Stale handler, a second dial must fire —
            // proving the rejoin path doesn't sit forever in the
            // pre-fix "stuck dialing" state.
            bus_impl.inject_durable(entry.clone()).await;
            tokio::time::sleep(Duration::from_millis(50)).await;

            assert_eq!(
                dial_calls.borrow().len(),
                2,
                "after Stale → release, the next presence event must trigger a fresh dial"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_connect_skips_dial_when_self_pk_is_larger() {
    // Glare avoidance: when two peers see each other's voice-presence,
    // only the lexicographically smaller pubkey initiates the dial; the
    // other side waits for the WebRTC accept-side handshake. This test
    // checks the "don't dial" branch.
    tokio::task::LocalSet::new()
        .run_until(async {
            let (smaller, larger, room) = make_pair_self_smaller(8, 9);
            // The runtime owner is the LARGER pubkey here; the injected
            // voice-presence is from the smaller one. Expectation: no
            // dial fires.
            let (bus_impl, _obs_tx) = TestBus::new(larger.store_verifying_key());
            let bus: Rc<dyn DynBus> = bus_impl.clone();
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
            let (runtime, tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                larger.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            // Activate so the gate doesn't trivially mask the glare-avoidance
            // path under test (we want auto_connect to *reach* the comparison
            // and then decline to dial because self_pk > peer_pk, not because
            // it skipped processing entirely).
            runtime.set_active(true);
            tokio::task::spawn_local(tasks.auto_connect);
            tokio::task::yield_now().await;

            let entry = make_presence_entry(&smaller, &room);
            bus_impl.inject_durable(entry).await;
            tokio::time::sleep(Duration::from_millis(150)).await;

            assert_eq!(
                calls.borrow().len(),
                0,
                "auto_connect must not dial when self_pk > peer_pk; the smaller-pk side dials and we accept"
            );
        })
        .await;
}

/// `voice_presence_membership` consumes the durable presence stream
/// and feeds `voice_presence_liveness`, which the combiner reads to
/// flip `in_voice_channel` to `true`. Critically, this should fire
/// *without* any ephemeral traffic — modelling the case where peer A
/// is in the voice channel (publishing presence) but no P2P
/// connection has been established yet, so neither frames nor
/// heartbeats reach us.
#[tokio::test(flavor = "current_thread")]
async fn membership_marks_in_voice_channel_without_p2p_traffic() {
    tokio::task::LocalSet::new()
        .run_until(async {
            // bob's runtime; alice is the remote peer whose presence is injected.
            let (bob, alice, room) = make_pair_self_smaller(20, 21);
            let alice_pk = alice.store_verifying_key();
            let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
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
            tokio::task::spawn_local(tasks.combiner);
            tokio::task::spawn_local(tasks.voice_presence_membership);

            tokio::task::yield_now().await;

            // Inject one durable presence entry from alice. No frames,
            // no heartbeats — this models "alice is in the channel but
            // I'm not connected to her".
            let entry = make_presence_entry(&alice, &room);
            bob_bus_impl.inject_durable(entry).await;

            // Wait for the combiner to emit a state where in_voice_channel
            // is true (and in_call is false — no P2P traffic).
            let result = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(ev) = events.borrow().last().cloned() {
                        if ev.in_voice_channel {
                            return ev;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("in_voice_channel emit within 2s");

            assert_eq!(result.peer, PeerId(alice_pk));
            assert!(
                result.in_voice_channel,
                "expected in_voice_channel=true after presence-only event"
            );
            assert!(
                !result.in_call,
                "in_call must remain false without ephemeral heartbeats / frames"
            );
            assert!(!result.talking, "talking must remain false without frames");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn voice_presence_publisher_emits_periodically() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(12);
            let alice_pk = alice.store_verifying_key();
            let (bus_impl, _obs_tx) = TestBus::new(alice_pk.clone());
            let bus: Rc<dyn DynBus> = bus_impl.clone();
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

            let room_fp = room.fingerprint().to_hex();
            let presence_prefix = Bytes::from(format!("voice-presence/{room_fp}/"));

            let (runtime, tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                alice.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            // Activate so the publisher actually publishes. Observer-mode
            // (the default) intentionally skips publication.
            runtime.set_active(true);

            // Subscribe to durable presence entries before spawning the publisher.
            let mut stream = bus_impl
                .subscribe_prefix(presence_prefix)
                .await
                .expect("subscribe succeeded");

            tokio::task::spawn_local(tasks.voice_presence_publisher);
            // Keep the runtime alive for the duration of the test so the
            // task's `weak.upgrade()` keeps succeeding.
            let _runtime = runtime;

            use futures::StreamExt as _;

            // First presence entry must arrive within 1 s.
            let first = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("first presence entry within 1s")
                .expect("stream open");

            match first {
                BusEvent::Durable { entry, .. } => {
                    assert_eq!(entry.verifying_key, alice_pk, "entry is from alice");
                }
                BusEvent::Ephemeral(_) => panic!("expected Durable"),
            }

            // Second presence entry must arrive within 3 s (refresh ~2s).
            let second = tokio::time::timeout(Duration::from_secs(3), stream.next())
                .await
                .expect("second presence entry within 3s")
                .expect("stream open");

            match second {
                BusEvent::Durable { entry, .. } => {
                    assert_eq!(entry.verifying_key, alice_pk, "second entry is from alice");
                }
                BusEvent::Ephemeral(_) => panic!("expected Durable"),
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_runtime_terminates_all_tasks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(11);
            // Keep a Rc<Room> clone for fingerprint computation (used below to
            // build names that match the subscribe task's prefix filter).
            let room_for_inject = room.clone();
            let (bus_impl, _tx) = TestBus::new(alice.store_verifying_key());
            // Keep a typed reference for post-drop injection (see below).
            let bus_for_inject = bus_impl.clone();
            let bus: Rc<dyn DynBus> = bus_impl;
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

            // Capture liveness arcs BEFORE drop so we can inject a synthetic
            // observation to wake the combiner task.
            let (frame_liveness, membership_liveness) = runtime.test_liveness();

            let handles = vec![
                tokio::task::spawn_local(tasks.heartbeat),
                tokio::task::spawn_local(tasks.subscribe),
                tokio::task::spawn_local(tasks.combiner),
                tokio::task::spawn_local(tasks.auto_connect),
                tokio::task::spawn_local(tasks.voice_presence_publisher),
            ];

            // Let each task enter its loop body and reach its first await
            // point before we drop the runtime.  Without this yield, the
            // tasks haven't started executing yet when drop(runtime) runs,
            // so the very first weak.upgrade() already fails — the test
            // never exercises in-flight cancellation.
            //
            // 50 ms is long enough for all six tasks to complete their
            // initialization (subscribe to the bus/liveness, set up any
            // codecs, etc.) and park inside their respective select!/await
            // loops without spending even a single timer-sleep interval.
            tokio::time::sleep(Duration::from_millis(50)).await;

            drop(runtime);

            // --- Wake each event-driven task so it reaches its
            //     weak.upgrade() check and observes the upgrade failure. ---

            // Compute the room fingerprint so injected names match the
            // tasks' prefix filters exactly.
            let room_fp = room_for_inject.fingerprint().to_hex();

            // subscribe task: blocked on stream.next(); inject a synthetic
            // ephemeral matching the `voice/{room_fp}/` prefix so it wakes up
            // and reaches the upgrade check inside its loop body.
            let dummy_peer_vk = VerifyingKey::new(Bytes::from_static(b"dummydummydummy1"));
            let voice_name = Bytes::from(format!("voice/{room_fp}/dummy-sender"));
            bus_for_inject
                .inject(SignedDatagram {
                    verifying_key: dummy_peer_vk,
                    name: voice_name,
                    payload: Bytes::new(),
                    seq: 0,
                    signature: Bytes::new(),
                })
                .await;

            // auto_connect task: blocked on select! over the durable presence
            // stream + membership liveness stream.  The membership arm fires
            // `weak.upgrade()` on Stale events.  Inject a peer with a timestamp
            // at UNIX_EPOCH (effectively "56 years stale") and then run the sweep
            // so the liveness emits a Stale event immediately — no need to wait
            // the full 5-second MEMBERSHIP_STALE_AFTER window.
            let stale_peer = PeerId(VerifyingKey::new(Bytes::from_static(b"dummydummydummy2")));
            membership_liveness
                .observe(stale_peer, std::time::SystemTime::UNIX_EPOCH)
                .await;
            membership_liveness.run_sweep().await;

            // combiner task: blocked on select! over frame + membership liveness
            // subscription streams (no bus traffic reaches it directly).  Inject
            // a synthetic frame-liveness observation so the frame_sub stream yields
            // an event and the combiner reaches its weak.upgrade() check.
            let active_peer = PeerId(VerifyingKey::new(Bytes::from_static(b"dummydummydummy3")));
            frame_liveness
                .observe(
                    active_peer,
                    std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1),
                )
                .await;

            // After the drop each task will observe the upgrade failure on
            // its next loop iteration.  heartbeat and voice_presence_publisher
            // sleep HEARTBEAT_INTERVAL (2 s) between iterations, so we give
            // each task up to 3 s to exit (one full sleep cycle + slack).
            let task_names = [
                "heartbeat",
                "subscribe",
                "combiner",
                "auto_connect",
                "voice_presence_publisher",
            ];
            for (i, h) in handles.into_iter().enumerate() {
                assert!(
                    tokio::time::timeout(Duration::from_secs(3), h)
                        .await
                        .is_ok(),
                    "task '{}' should finish after Drop",
                    task_names[i]
                );
            }
        })
        .await;
}

/// `set_peer_denoise(alice, false)` plumbs through to the receiver
/// path for that one peer: with denoise off the inbound PCM is
/// delivered raw (modulo Opus quantization), with denoise on the same
/// packets are attenuated by RNNoise. The test feeds pseudo-random
/// noise through a real encrypt → bus → decrypt → decode → sink loop
/// and compares RMS energy at the sink. The runtime has no internal
/// buffer between decode and sink — frames arrive synchronously with
/// the subscribe loop's `inject` calls.
#[tokio::test(flavor = "current_thread")]
async fn set_peer_denoise_toggle_attenuates_inbound_noise() {
    async fn run(denoise_on: bool) -> f32 {
        let delivered_rms_sum = Rc::new(RefCell::new(0.0_f32));
        let delivered_rms_count = Rc::new(RefCell::new(0_usize));

        tokio::task::LocalSet::new()
            .run_until({
                let delivered_rms_sum = delivered_rms_sum.clone();
                let delivered_rms_count = delivered_rms_count.clone();
                async move {
                    let (alice, room) = make_identity_and_room(33);
                    let (bob, _) = make_identity_and_room(34);
                    let alice_pk = alice.store_verifying_key();

                    let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
                    let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
                    let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                        calls: Rc::new(RefCell::new(vec![])),
                    });

                    // Custom sink that accumulates RMS so we don't need to
                    // hold every f32 of every frame.
                    struct RmsSink {
                        sum: Rc<RefCell<f32>>,
                        count: Rc<RefCell<usize>>,
                    }
                    impl FrameSink for RmsSink {
                        fn deliver(&self, _peer: &PeerId, _seq: u32, pcm: &[f32]) {
                            let s: f32 = pcm.iter().map(|s| s * s).sum();
                            let rms = (s / pcm.len() as f32).sqrt();
                            *self.sum.borrow_mut() += rms;
                            *self.count.borrow_mut() += 1;
                        }
                        fn drop_peer(&self, _peer: &PeerId) {}
                    }
                    let frame_sink: Rc<dyn FrameSink> = Rc::new(RmsSink {
                        sum: delivered_rms_sum.clone(),
                        count: delivered_rms_count.clone(),
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
                    let alice_peer = PeerId(alice_pk.clone());
                    runtime.set_peer_denoise(alice_peer.clone(), denoise_on);
                    assert_eq!(runtime.is_peer_denoise_enabled(&alice_peer), denoise_on);

                    tokio::task::spawn_local(tasks.subscribe);
                    tokio::task::yield_now().await;

                    // Feed deterministic pseudo-random noise at amplitude
                    // 0.05 (well below clipping). 30 frames = 600 ms,
                    // enough for RNNoise to settle past its fade-in window.
                    // Mono encoder; decoder upmixes to stereo, so the
                    // RMS sink sees 1920-sample stereo frames either
                    // way and the on/off comparison is fair.
                    let mut enc =
                        sunset_voice::VoiceEncoder::new(sunset_voice::VoiceQuality::Voice).unwrap();
                    let mut rng_seed: u32 = 0xBEEF_F00D;
                    for seq in 0..30 {
                        let mut pcm = vec![0.0_f32; sunset_voice::FRAME_SAMPLES_PER_CHANNEL];
                        for s in pcm.iter_mut() {
                            rng_seed = rng_seed.wrapping_mul(48271) % 0x7FFF_FFFF;
                            let n = (rng_seed as f32 / 0x7FFF_FFFF as f32) * 2.0 - 1.0;
                            *s = n * 0.05;
                        }
                        let bytes = enc.encode(&pcm).unwrap();
                        let pkt = sunset_voice::packet::VoicePacket::Frame {
                            codec_id: sunset_voice::CODEC_ID.to_string(),
                            sender_time_ms: 1000 + seq * 20,
                            payload: bytes,
                        };
                        let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(seq);
                        let ev = sunset_voice::packet::encrypt(
                            &room,
                            0,
                            &alice.public(),
                            &pkt,
                            &mut rng,
                        )
                        .unwrap();
                        let payload = postcard::to_stdvec(&ev).unwrap();
                        let room_fp = room.fingerprint().to_hex();
                        let sender_pk = hex::encode(alice_pk.as_bytes());
                        let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}"));
                        // Strictly increasing envelope seqs so the new
                        // receiver dedup gate doesn't drop them as replays.
                        bob_bus_impl
                            .inject(SignedDatagram {
                                verifying_key: alice_pk.clone(),
                                name,
                                payload: Bytes::from(payload),
                                seq,
                                signature: Bytes::new(),
                            })
                            .await;
                    }

                    // Wait for at least 25 frames to be delivered. The
                    // runtime delivers synchronously inside the subscribe
                    // loop now (no buffer in between), so the 2-second
                    // wall budget is loose CI tolerance — locally this
                    // path completes in milliseconds.
                    tokio::time::timeout(Duration::from_secs(2), async {
                        loop {
                            if *delivered_rms_count.borrow() >= 25 {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .expect("at least 25 frames delivered within 2s");

                    drop(runtime);
                }
            })
            .await;
        let total = *delivered_rms_sum.borrow();
        let count = *delivered_rms_count.borrow();
        total / count as f32
    }

    let off_rms = run(false).await;
    let on_rms = run(true).await;
    // Denoising should drop the inbound RMS substantially. Same input
    // signal, same Opus codec; the only difference is the RNNoise stage.
    assert!(
        off_rms > 0.001,
        "raw path should preserve audible energy: {off_rms}"
    );
    assert!(
        on_rms * 2.0 < off_rms,
        "denoise on should attenuate inbound noise vs off: on={on_rms}, off={off_rms}",
    );
}

/// Observer-mode contract: with `is_active=false`, the runtime emits
/// `in_voice_channel` for remote peers (so the rail can render the
/// roster before the local user joins the call) but does *not* publish
/// our own durable presence. Flipping to `is_active=true` resumes the
/// publisher; flipping back stops it. This is the load-bearing
/// guarantee for "show who is in the voice channel even when I haven't
/// joined yet".
#[tokio::test(flavor = "current_thread")]
async fn observer_mode_emits_in_voice_channel_without_publishing_self() {
    use futures::StreamExt as _;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (bob, alice, room) = make_pair_self_smaller(40, 41);
            let alice_pk = alice.store_verifying_key();
            let bob_pk = bob.store_verifying_key();
            let (bob_bus_impl, _obs_tx) = TestBus::new(bob_pk.clone());
            let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
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

            // Subscribe to our own presence prefix *before* spawning the
            // publisher so the assertion below can observe "nothing was
            // ever published while observing".
            let room_fp = room.fingerprint().to_hex();
            let presence_prefix = Bytes::from(format!("voice-presence/{room_fp}/"));
            let mut own_presence_stream = bob_bus_impl
                .subscribe_prefix(presence_prefix)
                .await
                .expect("presence prefix subscribe");

            let (runtime, tasks) = VoiceRuntime::new(
                bob_bus,
                room.clone(),
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            // Runtime defaults to observer mode. Spawn the three
            // observer-side tasks plus the three gated tasks so we
            // can verify the gates' behaviour end-to-end.
            assert!(!runtime.is_active(), "default mode is observer");
            tokio::task::spawn_local(tasks.combiner);
            tokio::task::spawn_local(tasks.voice_presence_membership);
            tokio::task::spawn_local(tasks.subscribe);
            tokio::task::spawn_local(tasks.auto_connect);
            tokio::task::spawn_local(tasks.heartbeat);
            tokio::task::spawn_local(tasks.voice_presence_publisher);

            tokio::task::yield_now().await;

            // Inject a remote durable-presence entry from alice. The
            // combiner should emit `in_voice_channel=true` even though
            // we never activated.
            let entry = make_presence_entry(&alice, &room);
            bob_bus_impl.inject_durable(entry.clone()).await;

            let observed = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let snap = events.borrow().last().cloned();
                    if let Some(ev) = snap {
                        if ev.in_voice_channel {
                            return ev;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("combiner emits in_voice_channel within 2s");
            assert_eq!(observed.peer, PeerId(alice_pk.clone()));
            assert!(
                observed.in_voice_channel,
                "observer must see alice in channel"
            );
            assert!(!observed.in_call, "no P2P → in_call must stay false");

            // Helper macro: wait up to `$budget` for the next durable
            // entry on `own_presence_stream` whose `verifying_key` equals
            // `bob_pk`. Alice's injected entry from the previous step
            // also flows through this stream, so filtering by key is
            // what isolates self-publishes from observer-side traffic.
            macro_rules! next_self_publish {
                ($budget:expr) => {{
                    let bob_pk = bob_pk.clone();
                    tokio::time::timeout($budget, async {
                        loop {
                            match own_presence_stream.next().await {
                                Some(BusEvent::Durable { entry, .. })
                                    if entry.verifying_key == bob_pk =>
                                {
                                    return Some(entry);
                                }
                                Some(_) => continue,
                                None => return None,
                            }
                        }
                    })
                    .await
                }};
            }

            // The publisher's first publish would land within
            // VOICE_PRESENCE_REFRESH_INTERVAL (2 s) if it were running.
            // Give it 1.5 s (well past task startup, well under the
            // 2 s republish cadence) — observer mode must not publish.
            let saw_self = next_self_publish!(Duration::from_millis(1500));
            assert!(
                saw_self.is_err(),
                "publisher must stay silent in observer mode (saw {saw_self:?})"
            );

            // Activate. The publisher's loop wakes from its sleep on its
            // own cadence (≤2 s) — wait up to 3 s for the first self-publish.
            runtime.set_active(true);
            assert!(runtime.is_active());
            let first_pub = next_self_publish!(Duration::from_secs(3))
                .expect("first own-presence publish within 3s after activate")
                .expect("stream open");
            assert_eq!(
                first_pub.verifying_key, bob_pk,
                "self-publish carries bob's key"
            );

            // Deactivate. The publisher's next iteration must observe
            // `is_active=false` and skip. There may be one in-flight
            // republish if we deactivate mid-sleep, so allow up to one
            // grace publish and then require silence.
            runtime.set_active(false);
            // Drain any already-emitted entries within the first refresh
            // interval, then require no further entries for a full
            // republish cycle. With the 2 s interval, a 4 s window after
            // drain leaves >1 republish-worth of margin.
            let _maybe_grace = next_self_publish!(Duration::from_secs(2));
            let after_grace = next_self_publish!(Duration::from_secs(4));
            assert!(
                after_grace.is_err(),
                "publisher must stop after set_active(false) (saw {after_grace:?})"
            );

            drop(runtime);
        })
        .await;
}

/// Two senders publishing frames interleaved through one receiver:
/// each peer's decoded audio must arrive intact. Opus decoders are
/// stateful — SILK predictor history, CELT pitch tracking, and mode
/// switching all assume one continuous stream — so a single shared
/// decoder corrupts every frame on a stream change. With per-peer
/// decoders each stream owns its own state and the RMS energy
/// survives round-trip on both sides simultaneously.
///
/// The shape that fails on shared-decoder code:
///   - alice encodes a 440 Hz sine, carol a 880 Hz sine, both at
///     amplitude 0.5 → expected decoded RMS ~0.35 per peer.
///   - Frames are interleaved A0, C0, A1, C1, … forcing the decoder
///     to alternate sources on every call.
///   - Per-peer decoders: each peer's average decoded RMS stays
///     within ~15 % of the ideal 0.353.
///   - Shared decoder: at least one peer's RMS collapses well below
///     0.30 because predictor state from the other stream poisons
///     every frame.
#[tokio::test(flavor = "current_thread")]
async fn multi_peer_decoders_do_not_corrupt_each_other() {
    type PerPeerRms = Rc<RefCell<HashMap<PeerId, (f32, usize)>>>;

    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(70);
            let (carol, _) = make_identity_and_room(71);
            let (bob, _) = make_identity_and_room(72);
            let alice_pk = alice.store_verifying_key();
            let carol_pk = carol.store_verifying_key();

            let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
            let dialer: Rc<dyn Dialer> = Rc::new(CountingDialer {
                calls: Rc::new(RefCell::new(vec![])),
            });

            let per_peer_rms: PerPeerRms = Rc::new(RefCell::new(HashMap::new()));
            struct PerPeerRmsSink {
                rms: PerPeerRms,
            }
            impl FrameSink for PerPeerRmsSink {
                fn deliver(&self, peer: &PeerId, _seq: u32, pcm: &[f32]) {
                    let s: f32 = pcm.iter().map(|s| s * s).sum();
                    let rms = (s / pcm.len() as f32).sqrt();
                    let mut map = self.rms.borrow_mut();
                    let entry = map.entry(peer.clone()).or_insert((0.0, 0));
                    entry.0 += rms;
                    entry.1 += 1;
                }
                fn drop_peer(&self, _peer: &PeerId) {}
            }

            let frame_sink: Rc<dyn FrameSink> = Rc::new(PerPeerRmsSink {
                rms: per_peer_rms.clone(),
            });
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: Rc::new(RefCell::new(vec![])),
            });

            // Receiver-side denoise off per peer: RNNoise is trained
            // on speech and attenuates pure 440/880 Hz sines as
            // "non-speech," which would mask whatever the decoder
            // delivers. Disabling it isolates the decoder contract
            // this test exists to verify. The denoise plumbing has
            // its own dedicated test
            // (`set_peer_denoise_toggle_attenuates_inbound_noise`).
            let (runtime, tasks) = VoiceRuntime::new(
                bob_bus,
                room.clone(),
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            runtime.set_peer_denoise(PeerId(alice_pk.clone()), false);
            runtime.set_peer_denoise(PeerId(carol_pk.clone()), false);
            tokio::task::spawn_local(tasks.subscribe);
            tokio::task::yield_now().await;

            const FRAMES: u64 = 40;
            // 0.5 amplitude → expected RMS 0.5/sqrt(2) ≈ 0.353.
            const AMPLITUDE: f32 = 0.5;
            const ALICE_HZ: f32 = 440.0;
            const CAROL_HZ: f32 = 880.0;
            // Both use VoiceQuality::Voice (mono) — the runtime's
            // decoder is always 2-channel and upmixes mono packets, so
            // the per-peer delivered PCM is stereo regardless. Mono
            // saves a downmix round and isolates the cross-peer
            // corruption from any stereo decoding concerns.
            let mut enc_alice =
                sunset_voice::VoiceEncoder::new(sunset_voice::VoiceQuality::Voice).unwrap();
            let mut enc_carol =
                sunset_voice::VoiceEncoder::new(sunset_voice::VoiceQuality::Voice).unwrap();

            fn sine_pcm(start_frame: u64, hz: f32) -> Vec<f32> {
                let mut pcm = vec![0.0_f32; sunset_voice::FRAME_SAMPLES_PER_CHANNEL];
                let base = start_frame * sunset_voice::FRAME_SAMPLES_PER_CHANNEL as u64;
                for (i, s) in pcm.iter_mut().enumerate() {
                    let n = (base + i as u64) as f32;
                    let t = n / sunset_voice::SAMPLE_RATE as f32;
                    *s = AMPLITUDE * (2.0 * std::f32::consts::PI * hz * t).sin();
                }
                pcm
            }

            async fn publish(
                bus_impl: &TestBus,
                room: &Rc<Room>,
                sender: &Identity,
                sender_pk: &VerifyingKey,
                seq: u64,
                bytes: Vec<u8>,
            ) {
                let pkt = sunset_voice::packet::VoicePacket::Frame {
                    codec_id: sunset_voice::CODEC_ID.to_string(),
                    sender_time_ms: 1000 + seq * 20,
                    payload: bytes,
                };
                let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(seq);
                let ev = sunset_voice::packet::encrypt(room, 0, &sender.public(), &pkt, &mut rng)
                    .unwrap();
                let payload = postcard::to_stdvec(&ev).unwrap();
                let room_fp = room.fingerprint().to_hex();
                let name = Bytes::from(format!(
                    "voice/{room_fp}/{}",
                    hex::encode(sender_pk.as_bytes())
                ));
                // Envelope seq is the per-sender stream seq (strictly
                // increasing per sender) so the receiver dedup gate keeps
                // every frame; the gate is keyed per sender.
                bus_impl
                    .inject(SignedDatagram {
                        verifying_key: sender_pk.clone(),
                        name,
                        payload: Bytes::from(payload),
                        seq,
                        signature: Bytes::new(),
                    })
                    .await;
            }

            // Interleave: A0, C0, A1, C1, …  This forces every decode
            // to alternate sources, which is the worst case for the
            // shared-decoder bug (every call sees mismatched state).
            for seq in 0..FRAMES {
                let pcm_a = sine_pcm(seq, ALICE_HZ);
                let pcm_c = sine_pcm(seq, CAROL_HZ);
                let bytes_a = enc_alice.encode(&pcm_a).unwrap();
                let bytes_c = enc_carol.encode(&pcm_c).unwrap();
                publish(&bob_bus_impl, &room, &alice, &alice_pk, seq, bytes_a).await;
                publish(&bob_bus_impl, &room, &carol, &carol_pk, seq, bytes_c).await;
            }

            // Wait until both peers have at least 30 frames delivered.
            // The subscribe loop is synchronous w.r.t. inject and the
            // sink, so under nominal load this resolves in ms — the
            // 2-second budget is CI tolerance.
            let alice_peer = PeerId(alice_pk.clone());
            let carol_peer = PeerId(carol_pk.clone());
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let counts = {
                        let map = per_peer_rms.borrow();
                        (
                            map.get(&alice_peer).map(|(_, n)| *n).unwrap_or(0),
                            map.get(&carol_peer).map(|(_, n)| *n).unwrap_or(0),
                        )
                    };
                    if counts.0 >= 30 && counts.1 >= 30 {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("both peers delivered ≥30 frames within 2s");

            let map = per_peer_rms.borrow();
            let (alice_sum, alice_n) = map[&alice_peer];
            let (carol_sum, carol_n) = map[&carol_peer];
            let alice_avg = alice_sum / alice_n as f32;
            let carol_avg = carol_sum / carol_n as f32;
            // Ideal: 0.5/sqrt(2) ≈ 0.353. Denoise is off for both
            // peers (see above), so RMS lands within Opus quantization
            // distance of the ideal. 0.30 is ~15 % below ideal: easily
            // passed by intact per-peer decoders; failed by
            // shared-decoder corruption because predictor state from
            // the other stream poisons every alternating frame.
            assert!(
                alice_avg > 0.30,
                "alice average RMS collapsed: {alice_avg} (expected ~0.35)"
            );
            assert!(
                carol_avg > 0.30,
                "carol average RMS collapsed: {carol_avg} (expected ~0.35)"
            );
        })
        .await;
}

/// Build a frame `SignedDatagram` from `sender` carrying the given
/// envelope `seq`. Mirrors what the engine stamps on the wire: the seq
/// lives on the envelope, never inside the encrypted packet.
async fn inject_frame_at_seq(
    bus_impl: &TestBus,
    room: &Room,
    sender: &Identity,
    sender_pk: &VerifyingKey,
    seq: u64,
) {
    let mut enc = sunset_voice::VoiceEncoder::new(sunset_voice::VoiceQuality::Voice).unwrap();
    let pcm: Vec<f32> = (0..960).map(|i| (i as f32) * 0.001).collect();
    let bytes = enc.encode(&pcm).unwrap();
    let pkt = sunset_voice::packet::VoicePacket::Frame {
        codec_id: sunset_voice::CODEC_ID.to_string(),
        sender_time_ms: 1000 + seq,
        payload: bytes,
    };
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(seq);
    let ev = sunset_voice::packet::encrypt(room, 0, &sender.public(), &pkt, &mut rng).unwrap();
    let payload = postcard::to_stdvec(&ev).unwrap();
    let room_fp = room.fingerprint().to_hex();
    let sender_pk_hex = hex::encode(sender_pk.as_bytes());
    let name = Bytes::from(format!("voice/{room_fp}/{sender_pk_hex}"));
    bus_impl
        .inject(SignedDatagram {
            verifying_key: sender_pk.clone(),
            name,
            payload: Bytes::from(payload),
            seq,
            signature: Bytes::new(),
        })
        .await;
}

/// Frames and heartbeats are two distinct ephemeral streams: frames
/// publish under `voice/{fp}/{pk}`, heartbeats under
/// `voice/{fp}/{pk}/hb`. A single `voice/{fp}/` prefix still covers both,
/// but each carries its own per-stream seq counter (so the frame seq the
/// jitter buffer consumes is not perturbed by heartbeats).
#[tokio::test(flavor = "current_thread")]
async fn frame_and_heartbeat_distinct_names() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(80);
            let pk = alice.store_verifying_key();
            let (bus_impl, tx) = TestBus::new(pk.clone());
            let bus: Rc<dyn DynBus> = bus_impl;
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

            let (runtime, tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                alice.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            runtime.set_active(true);
            tokio::task::spawn_local(tasks.heartbeat);

            let room_fp = room.fingerprint().to_hex();
            let sender_pk = hex::encode(pk.as_bytes());
            let frame_name = format!("voice/{room_fp}/{sender_pk}");
            let hb_name = format!("voice/{room_fp}/{sender_pk}/hb");

            let mut rx = tx.subscribe();

            // The frame send path publishes under the frame name.
            let pcm: Vec<f32> = (0..1920).map(|i| (i as f32) / 1000.0).collect();
            runtime.send_pcm(&pcm);

            // Collect one frame publish and one heartbeat publish.
            let mut saw_frame = false;
            let mut saw_hb = false;
            tokio::time::timeout(Duration::from_secs(3), async {
                while !(saw_frame && saw_hb) {
                    let d = rx.recv().await.unwrap();
                    let name = String::from_utf8_lossy(&d.name).into_owned();
                    if name == frame_name {
                        saw_frame = true;
                    } else if name == hb_name {
                        saw_hb = true;
                    } else {
                        panic!("unexpected publish name: {name}");
                    }
                }
            })
            .await
            .expect("both frame and heartbeat published within 3s");

            assert!(saw_frame, "frame published under voice/{{fp}}/{{pk}}");
            assert!(saw_hb, "heartbeat published under voice/{{fp}}/{{pk}}/hb");
            drop(runtime);
        })
        .await;
}

/// The receiver dedup gate uses an `Option` keyed on the sender, never
/// `unwrap_or(0)`: envelope seq 0 is a real first value, so the very
/// first frame at seq 0 must be delivered, not silently dropped.
#[tokio::test(flavor = "current_thread")]
async fn receiver_delivers_first_frame_seq_0_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(82);
            let (bob, _) = make_identity_and_room(83);
            let alice_pk = alice.store_verifying_key();

            let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
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

            let (_runtime, tasks) = VoiceRuntime::new(
                bob_bus,
                room.clone(),
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            tokio::task::spawn_local(tasks.subscribe);
            tokio::task::yield_now().await;

            inject_frame_at_seq(&bob_bus_impl, &room, &alice, &alice_pk, 0).await;

            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if !delivered.borrow().is_empty() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("first frame at envelope seq 0 must be delivered, not dropped");

            let snapshot = delivered.borrow();
            assert_eq!(snapshot.len(), 1, "exactly one delivery for seq 0");
            assert_eq!(snapshot[0].1, 0, "delivered seq is the envelope seq 0");
        })
        .await;
}

/// Two datagrams carrying the same `(sender, seq)` — the duplicate a
/// receiver briefly sees during direct/relay switchover — must produce
/// exactly one `deliver`.
#[tokio::test(flavor = "current_thread")]
async fn receiver_dedups_same_sender_seq() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(84);
            let (bob, _) = make_identity_and_room(85);
            let alice_pk = alice.store_verifying_key();

            let (bob_bus_impl, _obs_tx) = TestBus::new(bob.store_verifying_key());
            let bob_bus: Rc<dyn DynBus> = bob_bus_impl.clone();
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

            let (_runtime, tasks) = VoiceRuntime::new(
                bob_bus,
                room.clone(),
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            tokio::task::spawn_local(tasks.subscribe);
            tokio::task::yield_now().await;

            // Same sender, same envelope seq, injected twice.
            inject_frame_at_seq(&bob_bus_impl, &room, &alice, &alice_pk, 5).await;
            inject_frame_at_seq(&bob_bus_impl, &room, &alice, &alice_pk, 5).await;

            // Wait for the first to land, then give the second a chance.
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if !delivered.borrow().is_empty() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("first frame delivered within 1s");
            tokio::time::sleep(Duration::from_millis(100)).await;

            assert_eq!(
                delivered.borrow().len(),
                1,
                "duplicate (sender, seq) must deliver exactly once"
            );
        })
        .await;
}
