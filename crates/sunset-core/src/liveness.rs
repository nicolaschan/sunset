//! Generic per-namespace liveness tracker for ephemeral consumers.
//!
//! See `docs/superpowers/specs/2026-04-28-sunset-core-liveness-design.md`
//! for the architecture. Short version: this is a pure bookkeeper —
//! no Bus subscription, no decryption, no protocol awareness. Consumers
//! decode their payloads, extract a sender-claimed timestamp, and pipe
//! `(peer, sender_time)` observations into `Liveness::observe`. Stale
//! detection runs on every `observe` call and on the subscribe stream's
//! internal sweep interval.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::stream::LocalBoxStream;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use sunset_sync::PeerId;

/// Whether a peer is "live" (recently heard) or "stale" (silent
/// for longer than the configured `stale_after` duration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LivenessState {
    Live,
    Stale,
}

/// One state-transition event delivered to a `Liveness` subscriber.
/// `last_heard_at` is always the sender-claimed timestamp of the most
/// recent observation we accepted for this peer (useful for tooltips
/// like "last heard 8s ago").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerLivenessChange {
    pub peer: PeerId,
    pub state: LivenessState,
    pub last_heard_at: SystemTime,
}

/// Wall-clock abstraction so tests can pin "now" deterministically.
pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

/// Production clock — reads `SystemTime::now()`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Sugar trait so consumer payloads that already carry a sender
/// timestamp can be observed in one call: `liveness.observe_event(peer, &decoded)`.
pub trait HasSenderTime {
    fn sender_time(&self) -> SystemTime;
}

/// Per-peer bookkeeping entry held inside `Liveness::inner`.
struct PeerEntry {
    last_heard_at: SystemTime,
    state: LivenessState,
}

struct Inner {
    peers: HashMap<PeerId, PeerEntry>,
    subscribers: Vec<mpsc::UnboundedSender<PeerLivenessChange>>,
}

/// The tracker. Cheap to clone via `Arc`; share one instance across
/// all consumers that care about the same liveness window.
pub struct Liveness {
    stale_after: Duration,
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
}

impl Liveness {
    /// Construct with the production `SystemClock`.
    pub fn new(stale_after: Duration) -> Arc<Self> {
        Self::with_clock(stale_after, Arc::new(SystemClock))
    }

    /// Construct with a custom clock (typically `MockClock` in tests).
    pub fn with_clock(stale_after: Duration, clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            stale_after,
            clock,
            inner: Mutex::new(Inner {
                peers: HashMap::new(),
                subscribers: Vec::new(),
            }),
        })
    }

    /// Record that we received a fresh event from `peer` claiming it
    /// was produced at `sender_time`. Out-of-order observations (older
    /// than our current `last_heard_at`) are ignored — liveness state
    /// never goes backwards from a single observation.
    pub async fn observe(&self, peer: PeerId, sender_time: SystemTime) {
        let now = self.clock.now();
        let mut inner = self.inner.lock().await;
        // First: process the new observation.
        let observe_change = match inner.peers.get_mut(&peer) {
            Some(entry) if sender_time <= entry.last_heard_at => None,
            Some(entry) => {
                let was_live = entry.state == LivenessState::Live;
                entry.last_heard_at = sender_time;
                entry.state = LivenessState::Live;
                if was_live {
                    None
                } else {
                    Some(PeerLivenessChange {
                        peer: peer.clone(),
                        state: LivenessState::Live,
                        last_heard_at: sender_time,
                    })
                }
            }
            None => {
                inner.peers.insert(
                    peer.clone(),
                    PeerEntry {
                        last_heard_at: sender_time,
                        state: LivenessState::Live,
                    },
                );
                Some(PeerLivenessChange {
                    peer: peer.clone(),
                    state: LivenessState::Live,
                    last_heard_at: sender_time,
                })
            }
        };
        // Second: sweep all OTHER peers and emit Stale for any timed out.
        let stale_after = self.stale_after;
        let mut stale_changes: Vec<PeerLivenessChange> = Vec::new();
        for (other_peer, entry) in inner.peers.iter_mut() {
            if other_peer == &peer {
                continue;
            }
            if entry.state == LivenessState::Live
                && now
                    .duration_since(entry.last_heard_at)
                    .ok()
                    .is_some_and(|d| d > stale_after)
            {
                entry.state = LivenessState::Stale;
                stale_changes.push(PeerLivenessChange {
                    peer: other_peer.clone(),
                    state: LivenessState::Stale,
                    last_heard_at: entry.last_heard_at,
                });
            }
        }
        // Broadcast: stale events first, then the new observation.
        // Order matches Task 4 test `observe_triggers_sweep_for_other_peers`.
        for c in &stale_changes {
            broadcast(&mut inner.subscribers, c);
        }
        if let Some(ref c) = observe_change {
            broadcast(&mut inner.subscribers, c);
        }
    }

    /// Subscribe to state-change events. New peers fire `Live`; peers
    /// that exceed `stale_after` since `last_heard_at` fire `Stale`
    /// (Task 4); stale peers that observe again fire `Live`. No event
    /// fires when a Live peer simply observes again.
    ///
    /// The returned stream **does not replay existing state** — use
    /// `snapshot()` for the initial picture and the stream for changes.
    pub async fn subscribe(self: &Arc<Self>) -> LocalBoxStream<'static, PeerLivenessChange> {
        let (tx, rx) = mpsc::unbounded_channel::<PeerLivenessChange>();
        self.inner.lock().await.subscribers.push(tx);

        let me = Arc::clone(self);
        let sweep_period = self.stale_after / 2;
        let stream = async_stream::stream! {
            use futures::stream::StreamExt;
            let mut rx_stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
            let mut ticker = tokio::time::interval(sweep_period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Skip the immediate first tick so we don't run a redundant
            // sweep before any observation could have fired.
            ticker.tick().await;
            loop {
                tokio::select! {
                    biased;
                    maybe_change = rx_stream.next() => {
                        match maybe_change {
                            Some(change) => yield change,
                            None => break,
                        }
                    }
                    _ = ticker.tick() => {
                        me.run_sweep().await;
                    }
                }
            }
        };
        Box::pin(stream)
    }

    /// Sweep all peers and fire `Stale` events for any whose
    /// `last_heard_at` exceeds `stale_after` relative to the clock's
    /// current time AND whose current state is `Live`. Idempotent —
    /// peers already in `Stale` are not re-emitted.
    pub async fn run_sweep(&self) {
        let now = self.clock.now();
        let mut inner = self.inner.lock().await;
        let stale_after = self.stale_after;
        let mut to_emit: Vec<PeerLivenessChange> = Vec::new();
        for (peer, entry) in inner.peers.iter_mut() {
            if entry.state == LivenessState::Live
                && now
                    .duration_since(entry.last_heard_at)
                    .ok()
                    .is_some_and(|d| d > stale_after)
            {
                entry.state = LivenessState::Stale;
                to_emit.push(PeerLivenessChange {
                    peer: peer.clone(),
                    state: LivenessState::Stale,
                    last_heard_at: entry.last_heard_at,
                });
            }
        }
        for change in &to_emit {
            broadcast(&mut inner.subscribers, change);
        }
    }

    /// Read the current state of every tracked peer.
    pub async fn snapshot(&self) -> HashMap<PeerId, PeerLivenessChange> {
        let inner = self.inner.lock().await;
        inner
            .peers
            .iter()
            .map(|(peer, entry)| {
                (
                    peer.clone(),
                    PeerLivenessChange {
                        peer: peer.clone(),
                        state: entry.state,
                        last_heard_at: entry.last_heard_at,
                    },
                )
            })
            .collect()
    }
}

/// Send `change` to every live subscriber, dropping any whose
/// receiver has been closed. Caller must hold the inner lock so the
/// "subscribe registers vs broadcast fires" race is closed: a
/// subscriber registered before the lock release sees this event;
/// one registered after gets the next event but not this one.
fn broadcast(
    subs: &mut Vec<mpsc::UnboundedSender<PeerLivenessChange>>,
    change: &PeerLivenessChange,
) {
    subs.retain(|tx| tx.send(change.clone()).is_ok());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test clock that returns whatever the test sets via `set`.
    pub(super) struct MockClock {
        now: std::sync::Mutex<SystemTime>,
    }

    impl MockClock {
        pub fn new(start: SystemTime) -> Arc<Self> {
            Arc::new(Self {
                now: std::sync::Mutex::new(start),
            })
        }

        pub fn set(&self, t: SystemTime) {
            *self.now.lock().unwrap() = t;
        }

        #[allow(dead_code)]
        pub fn advance(&self, d: Duration) {
            let mut g = self.now.lock().unwrap();
            *g += d;
        }
    }

    impl Clock for MockClock {
        fn now(&self) -> SystemTime {
            *self.now.lock().unwrap()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn skeleton_constructs() {
        let clock = MockClock::new(SystemTime::UNIX_EPOCH);
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        // Just checks the value type — Arc<Liveness>, with a clock and a
        // 3-second window. Behaviour is added in subsequent tasks.
        assert_eq!(liveness.stale_after, Duration::from_secs(3));
    }

    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn pk(seed: u8) -> PeerId {
        PeerId(VerifyingKey::new(Bytes::copy_from_slice(&[seed; 32])))
    }

    fn t_secs(s: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(s)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observe_records_peer_in_snapshot() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        liveness.observe(pk(1), t_secs(99)).await;
        let snap = liveness.snapshot().await;
        assert_eq!(snap.len(), 1);
        let entry = snap.get(&pk(1)).expect("peer 1 present");
        assert_eq!(entry.state, LivenessState::Live);
        assert_eq!(entry.last_heard_at, t_secs(99));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observe_out_of_order_is_ignored() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        liveness.observe(pk(1), t_secs(99)).await;
        // Older sender_time than what we already have — must not regress.
        liveness.observe(pk(1), t_secs(80)).await;
        let snap = liveness.snapshot().await;
        let entry = snap.get(&pk(1)).expect("peer 1 present");
        assert_eq!(entry.last_heard_at, t_secs(99));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observe_newer_replaces_older() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        liveness.observe(pk(1), t_secs(99)).await;
        liveness.observe(pk(1), t_secs(100)).await;
        let snap = liveness.snapshot().await;
        assert_eq!(snap.get(&pk(1)).unwrap().last_heard_at, t_secs(100));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_independent_per_peer() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        liveness.observe(pk(1), t_secs(99)).await;
        liveness.observe(pk(2), t_secs(98)).await;
        let snap = liveness.snapshot().await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap.get(&pk(1)).unwrap().last_heard_at, t_secs(99));
        assert_eq!(snap.get(&pk(2)).unwrap().last_heard_at, t_secs(98));
    }

    use futures::StreamExt;

    #[tokio::test(flavor = "current_thread")]
    async fn subscribe_receives_live_on_first_observation() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        let mut sub = liveness.subscribe().await;
        liveness.observe(pk(1), t_secs(100)).await;
        let change = sub.next().await.expect("change emitted");
        assert_eq!(change.peer, pk(1));
        assert_eq!(change.state, LivenessState::Live);
        assert_eq!(change.last_heard_at, t_secs(100));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subscribe_does_not_replay_existing_state() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        // Observe BEFORE subscribing — pre-existing peers should NOT
        // be replayed to new subscribers. Use snapshot() for that.
        liveness.observe(pk(1), t_secs(100)).await;
        let mut sub = liveness.subscribe().await;
        // Trigger one observation so the stream wakes up; that
        // observation's change SHOULD be delivered.
        liveness.observe(pk(2), t_secs(101)).await;
        let change = sub.next().await.expect("peer 2 change emitted");
        assert_eq!(change.peer, pk(2));
        // We must NOT see a peer 1 event — it was registered before subscribe.
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subscribe_no_event_for_repeat_live_observation() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        let mut sub = liveness.subscribe().await;
        liveness.observe(pk(1), t_secs(100)).await;
        let _first = sub.next().await.expect("first change emitted");
        liveness.observe(pk(1), t_secs(101)).await;
        // Same peer, still Live — no second change. Trigger another peer
        // so the stream yields and we can verify peer 1 didn't sneak in.
        liveness.observe(pk(2), t_secs(102)).await;
        let next = sub.next().await.expect("peer 2 change");
        assert_eq!(next.peer, pk(2));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn multiple_subscribers_receive_same_event() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        let mut sub_a = liveness.subscribe().await;
        let mut sub_b = liveness.subscribe().await;
        liveness.observe(pk(1), t_secs(100)).await;
        let a = sub_a.next().await.expect("sub_a sees change");
        let b = sub_b.next().await.expect("sub_b sees change");
        assert_eq!(a, b);
        assert_eq!(a.peer, pk(1));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_sweep_emits_stale_after_window() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock.clone());
        let mut sub = liveness.subscribe().await;
        liveness.observe(pk(1), t_secs(100)).await;
        let live = sub.next().await.expect("live emitted");
        assert_eq!(live.state, LivenessState::Live);

        // Advance past the stale window (3s) and run sweep manually.
        clock.set(t_secs(104));
        liveness.run_sweep().await;

        let stale = sub.next().await.expect("stale emitted");
        assert_eq!(stale.peer, pk(1));
        assert_eq!(stale.state, LivenessState::Stale);
        assert_eq!(stale.last_heard_at, t_secs(100));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observe_triggers_sweep_for_other_peers() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock.clone());
        let mut sub = liveness.subscribe().await;
        liveness.observe(pk(1), t_secs(100)).await;
        let _live1 = sub.next().await.unwrap();

        // Advance time. Observing peer 2 should also trigger a sweep
        // that fires Stale for peer 1.
        clock.set(t_secs(105));
        liveness.observe(pk(2), t_secs(105)).await;

        // Two events should arrive: peer 2 Live AND peer 1 Stale.
        // Order: stale-sweep fires before the new observation's broadcast,
        // so we see Stale(1) then Live(2). Assert without depending on
        // order by collecting both.
        let mut got: Vec<PeerLivenessChange> = Vec::new();
        got.push(sub.next().await.unwrap());
        got.push(sub.next().await.unwrap());
        assert!(
            got.iter()
                .any(|c| c.peer == pk(1) && c.state == LivenessState::Stale)
        );
        assert!(
            got.iter()
                .any(|c| c.peer == pk(2) && c.state == LivenessState::Live)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_to_live_transition_emits_live() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock.clone());
        let mut sub = liveness.subscribe().await;
        liveness.observe(pk(1), t_secs(100)).await;
        let _live = sub.next().await.unwrap();

        clock.set(t_secs(104));
        liveness.run_sweep().await;
        let stale = sub.next().await.unwrap();
        assert_eq!(stale.state, LivenessState::Stale);

        // Observe again — should fire Live.
        liveness.observe(pk(1), t_secs(104)).await;
        let live_again = sub.next().await.unwrap();
        assert_eq!(live_again.peer, pk(1));
        assert_eq!(live_again.state, LivenessState::Live);
        assert_eq!(live_again.last_heard_at, t_secs(104));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_sweep_is_idempotent_for_already_stale_peer() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock.clone());
        let mut sub = liveness.subscribe().await;
        liveness.observe(pk(1), t_secs(100)).await;
        let _live = sub.next().await.unwrap();

        clock.set(t_secs(104));
        liveness.run_sweep().await;
        let _stale = sub.next().await.unwrap();

        // Second sweep should NOT re-emit Stale.
        liveness.run_sweep().await;

        // Drive one more change so the stream yields, and verify the
        // second sweep didn't sneak in anything.
        liveness.observe(pk(2), t_secs(105)).await;
        let next = sub.next().await.unwrap();
        assert_eq!(next.peer, pk(2));
        assert_eq!(next.state, LivenessState::Live);
    }
}
