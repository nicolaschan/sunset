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
    // Used by run_sweep (Task 4); suppress dead_code until then.
    #[allow(dead_code)]
    stale_after: Duration,
    // Used by run_sweep (Task 4); suppress dead_code until then.
    #[allow(dead_code)]
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
        let mut inner = self.inner.lock().await;
        let change = match inner.peers.get_mut(&peer) {
            Some(entry) if sender_time <= entry.last_heard_at => {
                // Older or equal observation — ignore, no state change.
                None
            }
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
        if let Some(c) = change {
            broadcast(&mut inner.subscribers, &c);
        }
    }

    /// Subscribe to state-change events. New peers fire `Live`; peers
    /// that exceed `stale_after` since `last_heard_at` fire `Stale`
    /// (Task 4); stale peers that observe again fire `Live`. No event
    /// fires when a Live peer simply observes again.
    ///
    /// The returned stream **does not replay existing state** — use
    /// `snapshot()` for the initial picture and the stream for changes.
    pub async fn subscribe(&self) -> LocalBoxStream<'static, PeerLivenessChange> {
        use futures::stream::StreamExt;
        let (tx, rx) = mpsc::unbounded_channel::<PeerLivenessChange>();
        self.inner.lock().await.subscribers.push(tx);
        Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(|c| c))
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

        // Used by Task 4 stale-detection tests.
        #[allow(dead_code)]
        pub fn set(&self, t: SystemTime) {
            *self.now.lock().unwrap() = t;
        }

        // Used by Task 4 stale-detection tests.
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
}
