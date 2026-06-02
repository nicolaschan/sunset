//! Tests for the per-peer voice-provider convergence component
//! (`runtime/voice_provider.rs`).
//!
//! The component computes, for every roster participant `A`, a *desired
//! provider* that is a pure function of observed connectivity:
//!
//! ```text
//! desired_provider(A) = A      if current_peers() contains (A, Secondary)
//!                     = relay  otherwise (the sole Primary peer)
//! ```
//!
//! and converges idempotently on every engine event / roster change by
//! issuing `unsubscribe_via(old)` + `subscribe_via(new)` only when the
//! armed provider differs from the desired one.
//!
//! The substrate is a scriptable `DynBus` whose `current_peers()` and
//! engine-event stream the test drives directly, recording every
//! `subscribe_via` / `unsubscribe_via` so the convergence is observable.

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
use sunset_store::{ContentBlock, Filter, SignedDatagram, SignedKvEntry, VerifyingKey};
use sunset_sync::routing::SubscriptionPolicy;
use sunset_sync::{EngineEvent, PeerId, TransportKind};
use sunset_voice::runtime::{
    Dialer, DynBus, FrameSink, PeerStateSink, VoicePeerState, VoiceRuntime,
};

/// One routing-interest mutation issued by the provider component.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RoutingCall {
    SubscribeVia { filter: Bytes, provider: PeerId },
    UnsubscribeVia { filter: Bytes, provider: PeerId },
}

type RoutingLog = Rc<RefCell<Vec<RoutingCall>>>;

/// Scriptable `DynBus` for the provider component. `current_peers` is
/// read from a shared cell the test mutates; engine events are pushed
/// through `events_tx`; presence (roster) and ephemeral traffic are
/// injected directly.
struct ProviderTestBus {
    current_peers: Rc<RefCell<Vec<(PeerId, TransportKind)>>>,
    routing_log: RoutingLog,
    events_tx: tokio::sync::mpsc::UnboundedSender<EngineEvent>,
    events_rx: tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<EngineEvent>>>,
    durable_sinks: tokio::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<SignedKvEntry>>>,
    ephemeral_sinks: tokio::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<SignedDatagram>>>,
}

impl ProviderTestBus {
    fn new() -> Rc<Self> {
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        Rc::new(Self {
            current_peers: Rc::new(RefCell::new(vec![])),
            routing_log: Rc::new(RefCell::new(vec![])),
            events_tx,
            events_rx: tokio::sync::Mutex::new(Some(events_rx)),
            durable_sinks: tokio::sync::Mutex::new(vec![]),
            ephemeral_sinks: tokio::sync::Mutex::new(vec![]),
        })
    }

    async fn inject_durable(&self, entry: SignedKvEntry) {
        for sink in self.durable_sinks.lock().await.iter() {
            let _ = sink.send(entry.clone());
        }
    }

    async fn inject(&self, dgram: SignedDatagram) {
        for sink in self.ephemeral_sinks.lock().await.iter() {
            let _ = sink.send(dgram.clone());
        }
    }
}

#[async_trait(?Send)]
impl DynBus for ProviderTestBus {
    async fn publish_ephemeral(
        &self,
        _name: Bytes,
        _seq: u64,
        _payload: Bytes,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    async fn publish_durable(
        &self,
        _entry: SignedKvEntry,
        _block: Option<ContentBlock>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    async fn subscribe_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, BusEvent>, Box<dyn std::error::Error>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SignedKvEntry>();
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

    async fn subscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
        _policy: SubscriptionPolicy,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.routing_log
            .borrow_mut()
            .push(RoutingCall::SubscribeVia {
                filter: filter_prefix(&filter),
                provider,
            });
        Ok(())
    }

    async fn unsubscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.routing_log
            .borrow_mut()
            .push(RoutingCall::UnsubscribeVia {
                filter: filter_prefix(&filter),
                provider,
            });
        Ok(())
    }

    async fn current_peers(&self) -> Vec<(PeerId, TransportKind)> {
        self.current_peers.borrow().clone()
    }

    async fn subscribe_engine_events(&self) -> tokio::sync::mpsc::UnboundedReceiver<EngineEvent> {
        // The component subscribes exactly once; hand it the single rx.
        self.events_rx
            .lock()
            .await
            .take()
            .expect("engine events subscribed at most once")
    }

    async fn subscribe_ephemeral_local(
        &self,
        filter: Filter,
    ) -> tokio::sync::mpsc::UnboundedReceiver<(SignedDatagram, sunset_sync::FrameVia)> {
        let prefix = filter_prefix(&filter);
        let (sink_tx, mut sink_rx) = tokio::sync::mpsc::unbounded_channel::<SignedDatagram>();
        self.ephemeral_sinks.lock().await.push(sink_tx);
        let (out_tx, out_rx) =
            tokio::sync::mpsc::unbounded_channel::<(SignedDatagram, sunset_sync::FrameVia)>();
        // This loopback fixture has no transport substrate, so injected
        // datagrams are tagged `Local` — provenance derivation from a real
        // inbound session kind is covered by the engine's own tests.
        tokio::task::spawn_local(async move {
            while let Some(d) = sink_rx.recv().await {
                if d.name.starts_with(&prefix) {
                    let _ = out_tx.send((d, sunset_sync::FrameVia::Local));
                }
            }
        });
        out_rx
    }
}

fn filter_prefix(filter: &Filter) -> Bytes {
    match filter {
        Filter::NamePrefix(p) => p.clone(),
        _ => Bytes::new(),
    }
}

struct NoopDialer;
#[async_trait(?Send)]
impl Dialer for NoopDialer {
    async fn ensure_direct(&self, _peer: PeerId) {}
}

struct NoopFrameSink;
impl FrameSink for NoopFrameSink {
    fn deliver(&self, _peer: &PeerId, _seq: u32, _pcm: &[f32], _via: sunset_sync::FrameVia) {}
    fn drop_peer(&self, _peer: &PeerId) {}
}

type EventSink = Rc<RefCell<Vec<VoicePeerState>>>;
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
    // Real wall-clock priority so the presence-liveness sweep (which
    // compares against `now`) keeps the participant Live for the whole
    // test rather than immediately staling a year-1970 timestamp.
    let now_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut entry = SignedKvEntry {
        verifying_key: sender.store_verifying_key(),
        name,
        value_hash,
        priority: now_ms,
        expires_at: Some(now_ms + 60_000),
        signature: Bytes::new(),
    };
    let payload = signing_payload(&entry);
    let sig = sender.sign(&payload);
    entry.signature = Bytes::copy_from_slice(&sig.to_bytes());
    entry
}

/// Spin up a `VoiceRuntime` with the provider component spawned and the
/// scriptable bus. Returns the runtime (kept alive), the bus handle, and
/// the routing log.
fn spawn_provider_runtime(
    self_id: &Identity,
    room: &Rc<Room>,
    bus_impl: &Rc<ProviderTestBus>,
) -> (VoiceRuntime, RoutingLog) {
    let bus: Rc<dyn DynBus> = bus_impl.clone();
    let dialer: Rc<dyn Dialer> = Rc::new(NoopDialer);
    let frame_sink: Rc<dyn FrameSink> = Rc::new(NoopFrameSink);
    let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
        events: Rc::new(RefCell::new(vec![])),
    });
    let (runtime, tasks) = VoiceRuntime::new(
        bus,
        room.clone(),
        self_id.clone(),
        dialer,
        frame_sink,
        peer_state_sink,
    );
    runtime.set_active(true);
    // The roster is the live set of the durable voice-presence tracker;
    // `voice_presence_membership` is the task that feeds it from the
    // presence stream. Both always run together in production.
    tokio::task::spawn_local(tasks.voice_presence_membership);
    tokio::task::spawn_local(tasks.voice_provider);
    (runtime, bus_impl.routing_log.clone())
}

/// Wait until `pred(log)` holds or the deadline elapses.
async fn wait_for_log(log: &RoutingLog, pred: impl Fn(&[RoutingCall]) -> bool) -> Vec<RoutingCall> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            {
                let snap = log.borrow().clone();
                if pred(&snap) {
                    return snap;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("routing log reached the expected state within 2s")
}

fn voice_filter(room: &Room, participant: &VerifyingKey) -> Bytes {
    let room_fp = room.fingerprint().to_hex();
    let pk_hex = hex::encode(participant.as_bytes());
    Bytes::from(format!("voice/{room_fp}/{pk_hex}"))
}

/// Roster {A}; a direct (A, Secondary) link exists → the provider arms
/// `subscribe_via(voice/{A}, provider=A)` and never arms the relay for A.
#[tokio::test(flavor = "current_thread")]
async fn provider_direct_when_secondary() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (me, room) = make_identity_and_room(1);
            let (alice, _) = make_identity_and_room(2);
            let a_peer = PeerId(alice.store_verifying_key());

            let bus = ProviderTestBus::new();
            // A is directly connected (Secondary).
            *bus.current_peers.borrow_mut() = vec![(a_peer.clone(), TransportKind::Secondary)];

            let (_rt, log) = spawn_provider_runtime(&me, &room, &bus);
            tokio::task::yield_now().await;

            // A enters the roster via durable presence.
            bus.inject_durable(make_presence_entry(&alice, &room)).await;

            let a_filter = voice_filter(&room, &alice.store_verifying_key());
            let snap = wait_for_log(&log, |calls| {
                calls.contains(&RoutingCall::SubscribeVia {
                    filter: a_filter.clone(),
                    provider: a_peer.clone(),
                })
            })
            .await;

            // The relay must never be armed for A.
            assert!(
                !snap.iter().any(|c| matches!(
                    c,
                    RoutingCall::SubscribeVia { provider, .. } if *provider != a_peer
                )),
                "no provider other than A should be armed for A, got {snap:?}"
            );
        })
        .await;
}

/// Roster {A}; A is reachable only through the relay (the sole Primary
/// peer) → the provider arms `subscribe_via(voice/{A}, provider=relay)`.
#[tokio::test(flavor = "current_thread")]
async fn provider_relay_when_no_direct() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (me, room) = make_identity_and_room(3);
            let (alice, _) = make_identity_and_room(4);
            let (relay, _) = make_identity_and_room(5);
            let relay_peer = PeerId(relay.store_verifying_key());

            let bus = ProviderTestBus::new();
            // Only the relay is connected (Primary); A has no direct link.
            *bus.current_peers.borrow_mut() = vec![(relay_peer.clone(), TransportKind::Primary)];

            let (_rt, log) = spawn_provider_runtime(&me, &room, &bus);
            tokio::task::yield_now().await;

            bus.inject_durable(make_presence_entry(&alice, &room)).await;

            let a_filter = voice_filter(&room, &alice.store_verifying_key());
            wait_for_log(&log, |calls| {
                calls.contains(&RoutingCall::SubscribeVia {
                    filter: a_filter.clone(),
                    provider: relay_peer.clone(),
                })
            })
            .await;
        })
        .await;
}

/// The provider derives the choice and converges on each event: direct →
/// drop direct (PeerRemoved) → relay → re-add direct (PeerAdded Secondary)
/// → direct, issuing exactly the right (un)subscribe sequence, and a
/// duplicate event does not double-arm.
#[tokio::test(flavor = "current_thread")]
async fn convergence_via_consequence() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (me, room) = make_identity_and_room(6);
            let (alice, _) = make_identity_and_room(7);
            let (relay, _) = make_identity_and_room(8);
            let a_peer = PeerId(alice.store_verifying_key());
            let relay_peer = PeerId(relay.store_verifying_key());
            let a_filter = voice_filter(&room, &alice.store_verifying_key());

            let bus = ProviderTestBus::new();
            // Start with both relay (Primary) and A direct (Secondary).
            *bus.current_peers.borrow_mut() = vec![
                (relay_peer.clone(), TransportKind::Primary),
                (a_peer.clone(), TransportKind::Secondary),
            ];

            let (_rt, log) = spawn_provider_runtime(&me, &room, &bus);
            tokio::task::yield_now().await;

            bus.inject_durable(make_presence_entry(&alice, &room)).await;

            // Phase 1: direct link present → provider = A.
            wait_for_log(&log, |calls| {
                calls.contains(&RoutingCall::SubscribeVia {
                    filter: a_filter.clone(),
                    provider: a_peer.clone(),
                })
            })
            .await;
            let after_p1 = log.borrow().len();

            // A duplicate event must NOT re-arm (idempotent recompute).
            bus.events_tx
                .send(EngineEvent::PongObserved {
                    peer_id: a_peer.clone(),
                    rtt_ms: 1,
                    observed_at_unix_ms: 1,
                })
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(
                log.borrow().len(),
                after_p1,
                "a no-op event must not issue any new routing call"
            );

            // Phase 2: drop the direct link → recompute → provider = relay.
            *bus.current_peers.borrow_mut() = vec![(relay_peer.clone(), TransportKind::Primary)];
            bus.events_tx
                .send(EngineEvent::PeerRemoved {
                    peer_id: a_peer.clone(),
                })
                .unwrap();

            wait_for_log(&log, |calls| {
                // The switch must withdraw A and arm the relay.
                calls.contains(&RoutingCall::UnsubscribeVia {
                    filter: a_filter.clone(),
                    provider: a_peer.clone(),
                }) && calls.contains(&RoutingCall::SubscribeVia {
                    filter: a_filter.clone(),
                    provider: relay_peer.clone(),
                })
            })
            .await;

            // Phase 3: re-add the direct link → recompute → provider = A.
            *bus.current_peers.borrow_mut() = vec![
                (relay_peer.clone(), TransportKind::Primary),
                (a_peer.clone(), TransportKind::Secondary),
            ];
            bus.events_tx
                .send(EngineEvent::PeerAdded {
                    peer_id: a_peer.clone(),
                    kind: TransportKind::Secondary,
                })
                .unwrap();

            wait_for_log(&log, |calls| {
                // Withdraw the relay and re-arm A (the last such pair).
                let last_unsub_relay = calls.iter().rposition(|c| {
                    *c == RoutingCall::UnsubscribeVia {
                        filter: a_filter.clone(),
                        provider: relay_peer.clone(),
                    }
                });
                let last_sub_a = calls.iter().rposition(|c| {
                    *c == RoutingCall::SubscribeVia {
                        filter: a_filter.clone(),
                        provider: a_peer.clone(),
                    }
                });
                matches!((last_unsub_relay, last_sub_a), (Some(u), Some(s)) if u < s)
            })
            .await;
        })
        .await;
}

/// A participant reachable only via the relay: its `/hb` heartbeats —
/// delivered through the broad LOCAL ephemeral receive — reach
/// `membership_liveness` and set `in_call`. Proves the subscribe.rs
/// change to `subscribe_ephemeral_local` didn't break membership.
#[tokio::test(flavor = "current_thread")]
async fn relay_only_participant_sets_in_call() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (me, room) = make_identity_and_room(9);
            let (alice, _) = make_identity_and_room(10);

            let bus = ProviderTestBus::new();
            let bus_dyn: Rc<dyn DynBus> = bus.clone();
            let dialer: Rc<dyn Dialer> = Rc::new(NoopDialer);
            let frame_sink: Rc<dyn FrameSink> = Rc::new(NoopFrameSink);
            let events: EventSink = Rc::new(RefCell::new(vec![]));
            let peer_state_sink: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink {
                events: events.clone(),
            });
            let (_runtime, tasks) = VoiceRuntime::new(
                bus_dyn,
                room.clone(),
                me.clone(),
                dialer,
                frame_sink,
                peer_state_sink,
            );
            // The local receive + combiner are what we exercise here.
            tokio::task::spawn_local(tasks.subscribe);
            tokio::task::spawn_local(tasks.combiner);
            tokio::task::yield_now().await;

            // Alice's heartbeat, published on the /hb subtree (Task 2),
            // delivered to us via the relay (so it lands in our local
            // ephemeral receive without any direct link to alice).
            let pkt = sunset_voice::packet::VoicePacket::Heartbeat {
                sent_at_ms: 5000,
                is_muted: false,
            };
            let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(7);
            let ev =
                sunset_voice::packet::encrypt(&room, 0, &alice.public(), &pkt, &mut rng).unwrap();
            let payload = postcard::to_stdvec(&ev).unwrap();
            let room_fp = room.fingerprint().to_hex();
            let sender_pk = hex::encode(alice.store_verifying_key().as_bytes());
            let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}/hb"));
            let dgram = SignedDatagram {
                verifying_key: alice.store_verifying_key(),
                name,
                payload: Bytes::from(payload),
                seq: 0,
                signature: Bytes::new(),
            };
            bus.inject(dgram).await;

            let result = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let Some(ev) = events
                        .borrow()
                        .iter()
                        .rev()
                        .find(|e| e.peer == PeerId(alice.store_verifying_key()))
                        .cloned()
                    {
                        return ev;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("membership state emitted within 1s");

            assert!(
                result.in_call,
                "a relay-delivered heartbeat must set in_call via membership_liveness"
            );
        })
        .await;
}
