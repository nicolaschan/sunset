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
}
