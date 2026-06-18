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
//! The substrate is a scriptable `Bus` whose `current_peers()` and
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
use sunset_core::bus::{Bus, BusEvent};
use sunset_store::{ContentBlock, Filter, SignedDatagram, SignedKvEntry, VerifyingKey};
use sunset_sync::routing::SubscriptionPolicy;
use sunset_sync::{EngineEvent, FrameVia, PeerId, TransportKind};
use sunset_voice::runtime::{Dialer, FrameSink, PeerStateSink, VoicePeerState, VoiceRuntime};

/// One routing-interest mutation issued by the provider component.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RoutingCall {
    SubscribeVia { filter: Bytes, provider: PeerId },
    UnsubscribeVia { filter: Bytes, provider: PeerId },
}

type RoutingLog = Rc<RefCell<Vec<RoutingCall>>>;

/// Scriptable `Bus` for the provider component. `current_peers` is
/// read from a shared cell the test mutates; engine events are pushed
/// through `events_tx`; presence (roster) and ephemeral traffic are
/// injected directly.
struct ProviderTestBus {
    current_peers: Rc<RefCell<Vec<(PeerId, TransportKind)>>>,
    routing_log: RoutingLog,
    events_tx: tokio::sync::mpsc::UnboundedSender<EngineEvent>,
    events_rx: tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<EngineEvent>>>,
    durable_sinks: tokio::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<SignedKvEntry>>>,
    ephemeral_sinks:
        tokio::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<(SignedDatagram, FrameVia)>>>,
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

    /// Inject an ephemeral datagram delivered over the relay (Primary)
    /// path — the default for a star where the leaf has no direct link.
    async fn inject(&self, dgram: SignedDatagram) {
        self.inject_with_via(dgram, FrameVia::Relay).await;
    }

    /// Inject an ephemeral datagram tagged with the inbound transport that
    /// carried it. `FrameVia::Direct` simulates a frame/heartbeat that
    /// arrived over a direct WebRTC (Secondary) link — the signal the
    /// provider uses to confirm the direct path before dropping the relay.
    async fn inject_with_via(&self, dgram: SignedDatagram, via: FrameVia) {
        for sink in self.ephemeral_sinks.lock().await.iter() {
            let _ = sink.send((dgram.clone(), via));
        }
    }
}

#[async_trait(?Send)]
impl Bus for ProviderTestBus {
    async fn publish_ephemeral(
        &self,
        _name: Bytes,
        _seq: u64,
        _payload: Bytes,
    ) -> Result<(), sunset_core::Error> {
        Ok(())
    }

    async fn publish_durable(
        &self,
        _entry: SignedKvEntry,
        _block: Option<ContentBlock>,
    ) -> Result<(), sunset_core::Error> {
        Ok(())
    }

    async fn subscribe(
        &self,
        filter: Filter,
    ) -> Result<LocalBoxStream<'static, BusEvent>, sunset_core::Error> {
        let prefix = filter_prefix(&filter);
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
    ) -> Result<(), sunset_core::Error> {
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
    ) -> Result<(), sunset_core::Error> {
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
        let (sink_tx, mut sink_rx) =
            tokio::sync::mpsc::unbounded_channel::<(SignedDatagram, FrameVia)>();
        self.ephemeral_sinks.lock().await.push(sink_tx);
        let (out_tx, out_rx) =
            tokio::sync::mpsc::unbounded_channel::<(SignedDatagram, sunset_sync::FrameVia)>();
        // Carry the injected provenance through unchanged: a test injects a
        // datagram with the `FrameVia` of the transport that would have
        // delivered it (`Relay` via `inject`, `Direct` via `inject_with_via`).
        tokio::task::spawn_local(async move {
            while let Some((d, via)) = sink_rx.recv().await {
                if d.name.starts_with(&prefix) {
                    let _ = out_tx.send((d, via));
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
    let bus: Rc<dyn Bus> = bus_impl.clone();
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
    // `subscribe` decodes inbound ephemeral traffic and feeds the
    // per-provenance liveness the provider consults to confirm a direct
    // path before dropping the relay. Spawned here so a test that injects
    // `FrameVia::Direct` traffic drives the real receive path end-to-end.
    tokio::task::spawn_local(tasks.subscribe);
    (runtime, bus_impl.routing_log.clone())
}

/// Build a signed, encrypted Direct-or-Relay heartbeat datagram for
/// `sender` in `room`, on the `voice/<room>/<sender>/hb` subtree the
/// receive loop listens to. Heartbeats keep a silent-but-connected peer's
/// per-provenance liveness fresh, so a direct path stays "confirmed" even
/// when the peer isn't currently talking.
fn make_heartbeat_datagram(sender: &Identity, room: &Room, seq: u64) -> SignedDatagram {
    // Real wall-clock send time so the direct-path liveness sweep (which
    // compares `last_heard_at` against `now`) keeps the peer direct-live for
    // the whole test rather than immediately staling a year-1970 timestamp.
    let now_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let pkt = sunset_voice::packet::VoicePacket::Heartbeat {
        sent_at_ms: now_ms,
        is_muted: false,
    };
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(seq.wrapping_add(1));
    let ev = sunset_voice::packet::encrypt(room, 0, &sender.public(), &pkt, &mut rng).unwrap();
    let payload = postcard::to_stdvec(&ev).unwrap();
    let room_fp = room.fingerprint().to_hex();
    let sender_pk = hex::encode(sender.store_verifying_key().as_bytes());
    let name = Bytes::from(format!("voice/{room_fp}/{sender_pk}/hb"));
    SignedDatagram {
        verifying_key: sender.store_verifying_key(),
        name,
        payload: Bytes::from(payload),
        seq,
        signature: Bytes::new(),
    }
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

/// The provider derives a *set* of providers and converges on each event,
/// across the full lifecycle: direct link appears (co-arm A + relay) →
/// direct link drops (PeerRemoved → relay only) → direct link re-appears
/// (co-arm again, relay kept) → direct traffic confirmed (drop relay). The
/// load-bearing property is that the relay — the always-available fallback —
/// is NEVER withdrawn on mere link presence, only once direct delivery is
/// observed. (The pre-fix version asserted the opposite: that re-adding a
/// direct link immediately withdrew the relay. That eager withdraw WAS the
/// dark-window bug — it stranded the sender's audio whenever the direct path
/// had not yet armed.) A duplicate no-op event must not re-issue any call.
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

            let sub_a = RoutingCall::SubscribeVia {
                filter: a_filter.clone(),
                provider: a_peer.clone(),
            };
            let unsub_a = RoutingCall::UnsubscribeVia {
                filter: a_filter.clone(),
                provider: a_peer.clone(),
            };
            let sub_relay = RoutingCall::SubscribeVia {
                filter: a_filter.clone(),
                provider: relay_peer.clone(),
            };
            let unsub_relay = RoutingCall::UnsubscribeVia {
                filter: a_filter.clone(),
                provider: relay_peer.clone(),
            };

            let bus = ProviderTestBus::new();
            // Start with both relay (Primary) and A direct (Secondary).
            *bus.current_peers.borrow_mut() = vec![
                (relay_peer.clone(), TransportKind::Primary),
                (a_peer.clone(), TransportKind::Secondary),
            ];

            let (_rt, log) = spawn_provider_runtime(&me, &room, &bus);
            tokio::task::yield_now().await;

            bus.inject_durable(make_presence_entry(&alice, &room)).await;

            // Phase 1: direct link present but no direct traffic yet → arm
            // BOTH the direct peer and the relay (co-armed overlap).
            wait_for_log(&log, |calls| {
                calls.contains(&sub_a) && calls.contains(&sub_relay)
            })
            .await;
            // The relay must not have been withdrawn — direct is unproven.
            assert!(!log.borrow().contains(&unsub_relay));
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

            // Phase 2: drop the direct link → withdraw A, keep the relay.
            *bus.current_peers.borrow_mut() = vec![(relay_peer.clone(), TransportKind::Primary)];
            bus.events_tx
                .send(EngineEvent::PeerRemoved {
                    peer_id: a_peer.clone(),
                })
                .unwrap();
            wait_for_log(&log, |calls| calls.contains(&unsub_a)).await;
            // The relay was already armed in Phase 1 and stays armed.
            assert!(!log.borrow().contains(&unsub_relay));

            // Phase 3: re-add the direct link → re-arm A, relay STILL kept
            // (no direct traffic yet). The pre-fix bug lived here: it
            // withdrew the relay the instant the link reappeared.
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
            // A is re-armed: a fresh SubscribeVia{A} after Phase 2's withdraw.
            wait_for_log(&log, |calls| {
                let last_unsub_a = calls.iter().rposition(|c| *c == unsub_a);
                let last_sub_a = calls.iter().rposition(|c| *c == sub_a);
                matches!((last_unsub_a, last_sub_a), (Some(u), Some(s)) if u < s)
            })
            .await;
            assert!(
                !log.borrow().contains(&unsub_relay),
                "relay must stay armed across a direct-link flap until direct traffic is proven"
            );

            // Phase 4: direct traffic from A actually arrives → the relay is
            // now redundant and is dropped (the confirmed downgrade).
            bus.inject_with_via(make_heartbeat_datagram(&alice, &room, 0), FrameVia::Direct)
                .await;
            wait_for_log(&log, |calls| calls.contains(&unsub_relay)).await;
        })
        .await;
}

/// Recovery: after the relay has been dropped in favour of a confirmed
/// direct path, losing the direct (Secondary) link must re-arm the relay
/// (and withdraw the now-unreachable direct peer) so audio keeps flowing.
/// The downgrade is reversible — derived from live connectivity + traffic,
/// not a one-way edge transition.
#[tokio::test(flavor = "current_thread")]
async fn direct_link_drop_after_downgrade_rearms_relay() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (me, room) = make_identity_and_room(14);
            let (alice, _) = make_identity_and_room(15);
            let (relay, _) = make_identity_and_room(16);
            let a_peer = PeerId(alice.store_verifying_key());
            let relay_peer = PeerId(relay.store_verifying_key());
            let a_filter = voice_filter(&room, &alice.store_verifying_key());
            let unsub_relay = RoutingCall::UnsubscribeVia {
                filter: a_filter.clone(),
                provider: relay_peer.clone(),
            };
            let sub_relay = RoutingCall::SubscribeVia {
                filter: a_filter.clone(),
                provider: relay_peer.clone(),
            };
            let sub_a = RoutingCall::SubscribeVia {
                filter: a_filter.clone(),
                provider: a_peer.clone(),
            };
            let unsub_a = RoutingCall::UnsubscribeVia {
                filter: a_filter.clone(),
                provider: a_peer.clone(),
            };

            let bus = ProviderTestBus::new();
            // Co-armed from the start: relay + direct link to A.
            *bus.current_peers.borrow_mut() = vec![
                (relay_peer.clone(), TransportKind::Primary),
                (a_peer.clone(), TransportKind::Secondary),
            ];
            let (_rt, log) = spawn_provider_runtime(&me, &room, &bus);
            tokio::task::yield_now().await;
            bus.inject_durable(make_presence_entry(&alice, &room)).await;

            // The peer joins and is co-armed (relay + direct) before any
            // direct traffic flows — the realistic ordering.
            wait_for_log(&log, |calls| {
                calls.contains(&sub_a) && calls.contains(&sub_relay)
            })
            .await;

            // Confirm the direct path → relay dropped (direct-only).
            bus.inject_with_via(make_heartbeat_datagram(&alice, &room, 0), FrameVia::Direct)
                .await;
            wait_for_log(&log, |calls| calls.contains(&unsub_relay)).await;

            // The direct link drops.
            *bus.current_peers.borrow_mut() = vec![(relay_peer.clone(), TransportKind::Primary)];
            bus.events_tx
                .send(EngineEvent::PeerRemoved {
                    peer_id: a_peer.clone(),
                })
                .unwrap();

            // The relay is re-armed (a SubscribeVia{relay} *after* the
            // earlier drop) and the unreachable direct peer is withdrawn.
            wait_for_log(&log, |calls| {
                let last_unsub_relay = calls.iter().rposition(|c| *c == unsub_relay);
                let last_sub_relay = calls.iter().rposition(|c| *c == sub_relay);
                let relay_rearmed =
                    matches!((last_unsub_relay, last_sub_relay), (Some(u), Some(s)) if u < s);
                relay_rearmed && calls.contains(&unsub_a)
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
            let bus_dyn: Rc<dyn Bus> = bus.clone();
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

/// Regression for the relay→direct "dark window".
///
/// When a direct (Secondary) link to A appears, the provider must arm the
/// direct path for A while KEEPING the relay armed — it must not withdraw
/// the relay until direct traffic from A is actually observed. The pre-fix
/// code derived a single provider from mere link presence and withdrew the
/// relay the instant the Secondary link showed up. That stranded A's audio
/// for as long as A had not yet armed our interest on the direct path
/// (seconds to minutes, healing only via ~30s anti-entropy), even though A
/// could still hear us — the exact asymmetric "B can't hear A" bug.
#[tokio::test(flavor = "current_thread")]
async fn secondary_link_does_not_withdraw_relay_before_direct_traffic() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (me, room) = make_identity_and_room(11);
            let (alice, _) = make_identity_and_room(12);
            let (relay, _) = make_identity_and_room(13);
            let a_peer = PeerId(alice.store_verifying_key());
            let relay_peer = PeerId(relay.store_verifying_key());
            let a_filter = voice_filter(&room, &alice.store_verifying_key());

            let bus = ProviderTestBus::new();
            // Relay only at first: A is reachable through the relay.
            *bus.current_peers.borrow_mut() = vec![(relay_peer.clone(), TransportKind::Primary)];

            let (_rt, log) = spawn_provider_runtime(&me, &room, &bus);
            tokio::task::yield_now().await;
            bus.inject_durable(make_presence_entry(&alice, &room)).await;

            // Relay-only → relay armed for A.
            wait_for_log(&log, |calls| {
                calls.contains(&RoutingCall::SubscribeVia {
                    filter: a_filter.clone(),
                    provider: relay_peer.clone(),
                })
            })
            .await;

            // A direct WebRTC (Secondary) link to A appears.
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

            // The provider must arm the direct path for A...
            wait_for_log(&log, |calls| {
                calls.contains(&RoutingCall::SubscribeVia {
                    filter: a_filter.clone(),
                    provider: a_peer.clone(),
                })
            })
            .await;

            // ...without having withdrawn the relay. No direct traffic has
            // been observed yet, so the relay is still the only proven path.
            let snap = log.borrow().clone();
            assert!(
                !snap.contains(&RoutingCall::UnsubscribeVia {
                    filter: a_filter.clone(),
                    provider: relay_peer.clone(),
                }),
                "relay must stay armed until direct traffic from A is observed; log: {snap:?}"
            );
        })
        .await;
}
