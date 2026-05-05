//! Integration tests for `VoiceRuntime` with an in-memory `Bus`.
//!
//! Uses tokio's `LocalSet` to spawn the runtime tasks alongside test
//! assertions. All `Bus` traffic loops back through a broadcast channel.

use std::cell::RefCell;
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
type DeliveredSink = Rc<RefCell<Vec<(PeerId, Vec<u8>, String)>>>;
/// Type alias to avoid clippy::type_complexity.
type DroppedSink = Rc<RefCell<Vec<PeerId>>>;
/// Type alias to avoid clippy::type_complexity.
type EventSink = Rc<RefCell<Vec<VoicePeerState>>>;

/// Helper: encode `pcm` as little-endian f32 bytes (the `pcm-f32-le`
/// codec). Used by tests that build `VoicePacket::Frame` payloads
/// inline; the production path runs through JS WebCodecs.
fn pcm_to_bytes(pcm: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 4);
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Helper: decode `pcm-f32-le` bytes back to PCM samples. Mirror of
/// `pcm_to_bytes` for use in test assertions.
fn pcm_from_bytes(bytes: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let arr: [u8; 4] = chunk.try_into().expect("chunks_exact yields [u8; 4]");
        out.push(f32::from_le_bytes(arr));
    }
    out
}

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
        payload: Bytes,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Build a SignedDatagram with self as verifying_key.
        let dgram = SignedDatagram {
            verifying_key: self.self_pk.clone(),
            name,
            payload,
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

struct RecordingFrameSink {
    delivered: DeliveredSink,
    dropped: DroppedSink,
}
impl FrameSink for RecordingFrameSink {
    fn deliver(&self, peer: &PeerId, payload: &[u8], codec_id: &str) {
        self.delivered
            .borrow_mut()
            .push((peer.clone(), payload.to_vec(), codec_id.to_string()));
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
                sunset_voice::packet::VoicePacket::Frame {
                    payload, codec_id, ..
                } => {
                    assert_eq!(codec_id, sunset_voice::CODEC_ID);
                    payload
                }
                _ => panic!("expected Frame"),
            };
            assert_eq!(pcm_from_bytes(&bytes), pcm);
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
            tokio::task::spawn_local(tasks.subscribe);

            // Yield to let the subscribe task start up and register its sink.
            tokio::task::yield_now().await;

            // Alice publishes one Frame as if she were on the network.
            let pcm: Vec<f32> = (0..960).map(|i| (i as f32) * 0.001).collect();
            let bytes = pcm_to_bytes(&pcm);
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
            let bytes = pcm_to_bytes(&pcm);
            let now_ms: u64 = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let pkt = sunset_voice::packet::VoicePacket::Frame {
                codec_id: sunset_voice::CODEC_ID.to_string(),
                seq: 1,
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
            let (_runtime, tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                bob.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
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
            let (_runtime, tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                larger.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
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

            let (_runtime, tasks) = VoiceRuntime::new(
                bus,
                room.clone(),
                alice.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );

            // Subscribe to durable presence entries before spawning the publisher.
            let mut stream = bus_impl
                .subscribe_prefix(presence_prefix)
                .await
                .expect("subscribe succeeded");

            tokio::task::spawn_local(tasks.voice_presence_publisher);

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
async fn jitter_pump_delivers_real_frames_only() {
    // Contract: the runtime is codec-agnostic and forwards each queued
    // frame to the host exactly once. It does NOT synthesise repeat-last
    // or silence frames on underrun — that responsibility moved to the
    // host (browser playback worklet pads silence on its own underflow,
    // and re-feeding the same opus packet to a stateful AudioDecoder
    // would have undefined output).
    tokio::task::LocalSet::new()
        .run_until(async {
            let (alice, room) = make_identity_and_room(10);
            let (bus_impl, _tx) = TestBus::new(alice.store_verifying_key());
            let bus: Rc<dyn DynBus> = bus_impl;
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
            runtime.test_push_frame(peer.clone(), &frame1);
            runtime.test_push_frame(peer.clone(), &frame2);

            tokio::task::spawn_local(tasks.jitter_pump);

            // Poll until both frames have been delivered.
            tokio::time::timeout(Duration::from_millis(200), async {
                loop {
                    if delivered.borrow().len() >= 2 {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("both queued frames delivered within 200ms");
            assert_eq!(pcm_from_bytes(&delivered.borrow()[0].1), frame1);
            assert_eq!(pcm_from_bytes(&delivered.borrow()[1].1), frame2);
            assert_eq!(delivered.borrow()[0].2, sunset_voice::CODEC_ID);
            assert_eq!(delivered.borrow()[1].2, sunset_voice::CODEC_ID);

            // Wait long enough for several pump cycles (20ms each); no
            // additional deliveries should arrive while the buffer is
            // empty.
            tokio::time::sleep(Duration::from_millis(120)).await;
            assert_eq!(
                delivered.borrow().len(),
                2,
                "underrun must not synthesise repeat-last or silence frames"
            );

            // Push a third frame — pump picks it up on the next 20ms tick.
            let frame3: Vec<f32> = (0..960).map(|i| i as f32 * 0.003).collect();
            runtime.test_push_frame(peer.clone(), &frame3);
            tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    if delivered.borrow().len() >= 3 {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("frame3 delivered");
            assert_eq!(pcm_from_bytes(&delivered.borrow()[2].1), frame3);
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
                tokio::task::spawn_local(tasks.jitter_pump),
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
                "jitter_pump",
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
