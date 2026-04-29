# sunset-core Liveness Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a generic per-namespace liveness tracker (`Liveness`) to `sunset-core` so ephemeral consumers (first user: voice in Plan C2) can answer "which peers have I heard from recently?" without reinventing the bookkeeping.

**Architecture:** Pure tracker — no Bus subscription, no decryption, no protocol-specific knowledge. Consumers decode payloads via `Room`, extract a sender-claimed timestamp from the decoded struct, and pipe `(peer, sender_time)` observations into `Liveness`. Liveness bookkeeps state (`Live | Stale`) per peer and emits change events to subscribers. Stale detection runs on every `observe` call and on a subscribe-stream-internal `tokio::time::interval` tick (no background spawn — keeps WASM compatibility simple).

**Tech Stack:** Rust + `tokio` (`sync` + `time` features) + `async-stream` for the merged subscribe stream + `futures` for `LocalBoxStream`. Targets both native and `wasm32-unknown-unknown`.

**Spec:** `docs/superpowers/specs/2026-04-28-sunset-core-liveness-design.md`

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/sunset-core/src/liveness.rs` (new) | The entire `Liveness` tracker, `LivenessState`, `PeerLivenessChange`, `Clock` trait, `SystemClock`, `MockClock` (cfg-test), `HasSenderTime` trait, co-located unit tests |
| `crates/sunset-core/src/lib.rs` (modify) | `pub mod liveness;` + re-exports |
| `crates/sunset-core/Cargo.toml` (modify) | Add `tokio = { workspace = true, features = ["sync", "time"] }` as a runtime dep (currently only a dev-dep) |
| `crates/sunset-core/tests/liveness_with_bus.rs` (new) | End-to-end integration: simulated decryption, Bus → decoder → `Liveness`, verify state transitions through real subscription path |

The unit-test pattern matches `bus.rs` (`#[cfg(test)] mod tests` at the bottom of the same file). The integration test goes in `tests/` next to `bus_integration.rs`.

`PeerId` is the existing `sunset_sync::PeerId` (newtype around `VerifyingKey`); we re-use it rather than introduce a new type.

---

## Task 1: Skeleton + types + Clock + Cargo deps + lib.rs re-exports

**Files:**
- Create: `crates/sunset-core/src/liveness.rs`
- Modify: `crates/sunset-core/src/lib.rs`
- Modify: `crates/sunset-core/Cargo.toml`

**Why this task:** Get the module compiling with all the types declared, so subsequent tasks can drop in behavior without churning the surface.

- [ ] **Step 1: Add tokio to runtime dependencies**

In `crates/sunset-core/Cargo.toml`, find the `[dependencies]` block (lines 14-32 in current file). Insert this line in alphabetical order (between `thiserror.workspace = true` and `tokio-stream = { workspace = true }`):

```toml
tokio = { workspace = true, features = ["sync", "time"] }
```

The full block should now contain (other lines unchanged, showing only the relevant region):

```toml
thiserror.workspace = true
tokio = { workspace = true, features = ["sync", "time"] }
tokio-stream = { workspace = true }
```

- [ ] **Step 2: Create `crates/sunset-core/src/liveness.rs` with skeleton**

Create the file with this exact content:

```rust
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
```

- [ ] **Step 3: Add `pub mod liveness;` and re-exports to `crates/sunset-core/src/lib.rs`**

In `crates/sunset-core/src/lib.rs`, after the existing `pub mod` declarations (line 16, after `pub mod verifier;`), add:

```rust
pub mod liveness;
```

After the existing re-exports (line 25, after the `pub use verifier::Ed25519Verifier;` line), add:

```rust
pub use liveness::{
    Clock, HasSenderTime, Liveness, LivenessState, PeerLivenessChange, SystemClock,
};
```

- [ ] **Step 4: Verify the crate compiles on the host target**

Run:

```bash
nix develop --command cargo build -p sunset-core
```

Expected: `Finished` with no errors and no warnings.

- [ ] **Step 5: Verify the crate compiles for wasm32**

Run:

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-core
```

Expected: `Finished` with no errors and no warnings. Confirms the new tokio features (`sync`, `time`) work on wasm32.

- [ ] **Step 6: Run the skeleton test**

Run:

```bash
nix develop --command cargo test -p sunset-core --lib liveness::tests::skeleton_constructs
```

Expected: `test liveness::tests::skeleton_constructs ... ok`.

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-core/Cargo.toml crates/sunset-core/src/liveness.rs crates/sunset-core/src/lib.rs
git commit -m "Add Liveness skeleton: types, Clock trait, MockClock

Plain types + Arc constructor for the per-namespace liveness tracker.
No behaviour yet — observe/subscribe/sweep land in subsequent tasks.
Adds tokio to sunset-core runtime deps (sync + time features) and
re-exports the public types from lib.rs."
```

---

## Task 2: `observe` + `snapshot` (with out-of-order rejection)

**Files:**
- Modify: `crates/sunset-core/src/liveness.rs`

**Why this task:** Establish the core bookkeeping (peer map updates, out-of-order ignored) before adding subscription/broadcast. TDD: write failing tests first.

- [ ] **Step 1: Write the failing tests**

In `crates/sunset-core/src/liveness.rs`, inside the existing `#[cfg(test)] mod tests` block (after the `skeleton_constructs` test), append:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run:

```bash
nix develop --command cargo test -p sunset-core --lib liveness::tests
```

Expected: compile errors — `observe` and `snapshot` don't exist yet.

- [ ] **Step 3: Implement `observe` and `snapshot`**

In `crates/sunset-core/src/liveness.rs`, inside the existing `impl Liveness { … }` block (after the `with_clock` constructor), append:

```rust
    /// Record that we received a fresh event from `peer` claiming it
    /// was produced at `sender_time`. Out-of-order observations (older
    /// than our current `last_heard_at`) are ignored — liveness state
    /// never goes backwards from a single observation.
    pub async fn observe(&self, peer: PeerId, sender_time: SystemTime) {
        let mut inner = self.inner.lock().await;
        match inner.peers.get_mut(&peer) {
            Some(entry) if sender_time <= entry.last_heard_at => {
                // Older or equal observation — ignore.
            }
            Some(entry) => {
                entry.last_heard_at = sender_time;
                // State transitions land in Task 3; for now just record.
            }
            None => {
                inner.peers.insert(
                    peer,
                    PeerEntry {
                        last_heard_at: sender_time,
                        state: LivenessState::Live,
                    },
                );
            }
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:

```bash
nix develop --command cargo test -p sunset-core --lib liveness::tests
```

Expected: all 5 liveness tests pass (`skeleton_constructs`, `observe_records_peer_in_snapshot`, `observe_out_of_order_is_ignored`, `observe_newer_replaces_older`, `snapshot_independent_per_peer`).

- [ ] **Step 5: Build for wasm32 to confirm cross-target compatibility**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-core
```

Expected: `Finished` clean.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/liveness.rs
git commit -m "Liveness: observe + snapshot with out-of-order rejection

observe records (peer, sender_time) into the inner map; older
observations don't regress state. snapshot returns the current entry
set as PeerLivenessChange values. State-transition broadcast lands in
Task 3."
```

---

## Task 3: `subscribe` + `Live` emissions on observe

**Files:**
- Modify: `crates/sunset-core/src/liveness.rs`

**Why this task:** Add subscriber registration and the broadcast path that fires when a peer transitions to `Live` (new peer, or stale → live). The `Stale` direction lands in Task 4.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` mod in `crates/sunset-core/src/liveness.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
nix develop --command cargo test -p sunset-core --lib liveness::tests
```

Expected: compile errors — `subscribe` doesn't exist; the new tests fail to build.

- [ ] **Step 3: Add `subscribe` and broadcast plumbing**

In `crates/sunset-core/src/liveness.rs`, modify the existing `observe` method to broadcast on Live transitions. Replace the entire `observe` method body with:

```rust
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
            broadcast(&mut inner.subscribers, c);
        }
    }
```

Then add the `subscribe` method, also inside the `impl Liveness { … }` block (after `snapshot`):

```rust
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
```

Then add the free helper at the bottom of the file (above the `#[cfg(test)] mod tests` line):

```rust
/// Send `change` to every live subscriber, dropping any whose
/// receiver has been closed. Caller must hold the inner lock so the
/// "subscribe registers vs broadcast fires" race is closed: a
/// subscriber registered before the lock release sees this event;
/// one registered after gets the next event but not this one.
fn broadcast(subs: &mut Vec<mpsc::UnboundedSender<PeerLivenessChange>>, change: PeerLivenessChange) {
    subs.retain(|tx| tx.send(change.clone()).is_ok());
}
```

- [ ] **Step 4: Add `tokio-stream` (already present) imports if missing**

`tokio-stream` is already in `[dependencies]` (workspace dep). Ensure `crates/sunset-core/src/liveness.rs` has the import — at the top of the file, the `use tokio::sync::mpsc;` line is already there from Task 1. The new code uses `tokio_stream::wrappers::UnboundedReceiverStream` inline (full path), so no extra `use` is required.

- [ ] **Step 5: Run the tests**

```bash
nix develop --command cargo test -p sunset-core --lib liveness::tests
```

Expected: 9 tests pass (5 from before + 4 new).

- [ ] **Step 6: Build for wasm32**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-core
```

Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-core/src/liveness.rs
git commit -m "Liveness: subscribe + broadcast on Live transitions

subscribe registers a subscriber slot inside the inner-mutex critical
section (closes the register-vs-broadcast race the same way the
sunset-store-memory subscription registry does). observe now emits a
PeerLivenessChange on new-peer and stale->live transitions; same-state
observations emit nothing. Stale transitions land in Task 4."
```

---

## Task 4: Stale detection (`run_sweep` + observe-side sweep + subscribe-stream interval)

**Files:**
- Modify: `crates/sunset-core/src/liveness.rs`

**Why this task:** Make peers transition `Live → Stale` when their `last_heard_at` exceeds `stale_after` relative to the configured clock. Two trigger paths: (a) `observe` runs a sweep of *other* peers under the same lock, and (b) the subscribe stream wakes on a `tokio::time::interval` and runs the sweep so subscribers see Stale events even with no incoming observations. A public `run_sweep` method exists primarily for tests but is also useful for callers who want to drive sweep cadence externally.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` mod:

```rust
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
        assert!(got.iter().any(|c| c.peer == pk(1) && c.state == LivenessState::Stale));
        assert!(got.iter().any(|c| c.peer == pk(2) && c.state == LivenessState::Live));
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
```

`MockClock` needs `clone()` for tests to share the clock between the test body and the `Liveness`. Update `MockClock` to implement `Clone` — already-shared via `Arc`, so `.clone()` on the `Arc` works. Replace the existing `MockClock::new` signature usage in tests if needed; the current `Arc<MockClock>` already supports `.clone()`. No code change in `MockClock` itself is required — the tests above call `clock.clone()` on the `Arc`, which is what they already had.

- [ ] **Step 2: Run the tests — they should fail to compile**

```bash
nix develop --command cargo test -p sunset-core --lib liveness::tests
```

Expected: `run_sweep` doesn't exist.

- [ ] **Step 3: Add `run_sweep` and observe-side sweep**

In `crates/sunset-core/src/liveness.rs`, add a new method to the `impl Liveness { … }` block (after `subscribe`):

```rust
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
        for change in to_emit {
            broadcast(&mut inner.subscribers, change);
        }
    }
```

Then modify `observe` to also run the same sweep logic at the end. Replace the existing `observe` body with:

```rust
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
        for c in stale_changes {
            broadcast(&mut inner.subscribers, c);
        }
        if let Some(c) = observe_change {
            broadcast(&mut inner.subscribers, c);
        }
    }
```

- [ ] **Step 4: Modify `subscribe` to drive the sweep on a timer**

Replace the existing `subscribe` method body with:

```rust
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
```

Note: `subscribe` now takes `&Arc<Self>` instead of `&self` so the stream can keep a `me: Arc<Liveness>` alive for the sweep callback. Update call sites in tests to use `liveness.subscribe()` — `liveness` is already `Arc<Liveness>`, so the dot-call deref-converts.

- [ ] **Step 5: Run the tests**

```bash
nix develop --command cargo test -p sunset-core --lib liveness::tests
```

Expected: 13 tests pass (9 from before + 4 new). The Task-3 tests still pass because `run_sweep` is a no-op when no peers are stale.

- [ ] **Step 6: Build for wasm32**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-core
```

Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/sunset-core/src/liveness.rs
git commit -m "Liveness: stale detection via observe-side sweep + subscribe interval

run_sweep walks all peers and fires Stale for any Live peer whose
last_heard_at exceeds stale_after relative to the configured clock.
observe runs the same sweep for OTHER peers under the same lock so
busy systems see Stale events promptly. subscribe's stream wakes on a
tokio::time::interval ticker every stale_after/2 and runs the sweep
so subscribers also see Stale events when no observations are landing
anywhere."
```

---

## Task 5: `HasSenderTime` trait + `observe_event` sugar

**Files:**
- Modify: `crates/sunset-core/src/liveness.rs`

**Why this task:** Provide the one-liner consumers (Plan C2 voice, future typing, etc) will reach for, removing the `decoded.sender_time()` boilerplate at every call site.

- [ ] **Step 1: Write the failing test**

Append to the `tests` mod:

```rust
    struct DummyEvent {
        sender_time: SystemTime,
    }

    impl HasSenderTime for DummyEvent {
        fn sender_time(&self) -> SystemTime {
            self.sender_time
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observe_event_equivalent_to_observe() {
        let clock = MockClock::new(t_secs(100));
        let liveness = Liveness::with_clock(Duration::from_secs(3), clock);
        let event = DummyEvent { sender_time: t_secs(99) };
        liveness.observe_event(pk(1), &event).await;
        let snap = liveness.snapshot().await;
        let entry = snap.get(&pk(1)).expect("peer 1 present");
        assert_eq!(entry.last_heard_at, t_secs(99));
        assert_eq!(entry.state, LivenessState::Live);
    }
```

- [ ] **Step 2: Run to verify it fails**

```bash
nix develop --command cargo test -p sunset-core --lib liveness::tests::observe_event_equivalent_to_observe
```

Expected: compile error — `observe_event` doesn't exist.

- [ ] **Step 3: Implement `observe_event`**

In `crates/sunset-core/src/liveness.rs`, add to the `impl Liveness { … }` block (after `run_sweep`):

```rust
    /// Convenience: observe an event whose decoded payload knows its
    /// own sender time. Equivalent to `observe(peer, ev.sender_time())`.
    pub async fn observe_event<T: HasSenderTime>(&self, peer: PeerId, ev: &T) {
        self.observe(peer, ev.sender_time()).await;
    }
```

- [ ] **Step 4: Run the tests**

```bash
nix develop --command cargo test -p sunset-core --lib liveness::tests
```

Expected: 14 tests pass.

- [ ] **Step 5: Build for wasm32**

```bash
nix develop --command cargo build --target wasm32-unknown-unknown -p sunset-core
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-core/src/liveness.rs
git commit -m "Liveness: HasSenderTime sugar + observe_event helper

Consumers whose decoded payloads carry a sender_time field can use
observe_event instead of explicitly threading sender_time() through
every call site. Trait is opt-in — observe(peer, time) remains the
primary API."
```

---

## Task 6: Bus integration test

**Files:**
- Create: `crates/sunset-core/tests/liveness_with_bus.rs`

**Why this task:** Verify `Liveness` composes with `Bus`-delivered events through the same wiring Plan C2 voice will use. The integration test simulates the consumer protocol (decode + extract `sender_time`) without involving `Room` — encryption belongs in Plan C2's voice tests. A "decoded event" here is just a postcard struct with `{ sender_time_micros, payload }`.

The scaffolding (two-engine TestTransport setup, `set_trust` after `engine.run()`, Bob subscribes BEFORE Alice connects, poll `knows_peer_subscription` to wait for registry propagation) is modelled directly on `crates/sunset-core/tests/bus_integration.rs:42-150`. Diverges only in the consumer side: instead of asserting on the `BusEvent` directly, decode it and pipe into `Liveness`.

- [ ] **Step 1: Create `crates/sunset-core/tests/liveness_with_bus.rs`**

Write this exact content:

```rust
//! End-to-end: Alice publishes ephemeral events carrying a sender_time
//! field; Bob's decoder loop pulls them from Bus::subscribe, decodes,
//! and feeds (peer, sender_time) into Liveness. Verifies Liveness
//! fires Live for Alice through the real Bus subscription path. Same
//! scaffolding voice (Plan C2) will use, minus the encryption +
//! Opus parts.

#![cfg(feature = "test-helpers")]

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures::StreamExt as _;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};

use sunset_core::{
    Bus, BusEvent, BusImpl, HasSenderTime, Identity, Liveness, LivenessState,
};
use sunset_store::{AcceptAllVerifier, Filter};
use sunset_store_memory::MemoryStore;
use sunset_sync::test_transport::TestNetwork;
use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine, TrustSet};

type TestEngine = SyncEngine<MemoryStore, sunset_sync::test_transport::TestTransport>;

/// Mimics what a real consumer (e.g. voice) would publish inside its
/// encrypted payload — a sender-claimed timestamp + opaque bytes.
#[derive(Serialize, Deserialize)]
struct TestEvent {
    sender_time_micros: u64,
    payload: Vec<u8>,
}

impl TestEvent {
    fn encode(&self) -> Bytes {
        Bytes::from(postcard::to_stdvec(self).unwrap())
    }

    fn decode(bytes: &[u8]) -> Self {
        postcard::from_bytes(bytes).unwrap()
    }
}

impl HasSenderTime for TestEvent {
    fn sender_time(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_micros(self.sender_time_micros)
    }
}

fn build(
    net: &TestNetwork,
    addr: &str,
) -> (
    BusImpl<MemoryStore, sunset_sync::test_transport::TestTransport>,
    Rc<TestEngine>,
    tokio::task::JoinHandle<()>,
    Identity,
) {
    let identity = Identity::generate(&mut OsRng);
    let local_peer = PeerId(identity.store_verifying_key());
    let store = Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)));
    let transport = net.transport(
        local_peer.clone(),
        PeerAddr::new(Bytes::copy_from_slice(addr.as_bytes())),
    );
    let engine = Rc::new(SyncEngine::new(
        store.clone(),
        transport,
        SyncConfig::default(),
        local_peer,
        Arc::new(identity.clone()) as Arc<dyn Signer>,
    ));
    let bus = BusImpl::new(store, engine.clone(), identity.clone());
    let run_handle = {
        let engine = engine.clone();
        tokio::task::spawn_local(async move {
            let _ = engine.run().await;
        })
    };
    (bus, engine, run_handle, identity)
}

#[tokio::test(flavor = "current_thread")]
async fn liveness_tracks_alice_via_bob_bus_subscription() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let net = TestNetwork::new();
            let (alice_bus, alice_engine, alice_run, alice_identity) = build(&net, "alice");
            let (bob_bus, alice_view_of_bob, bob_run, bob_identity) = build(&net, "bob");

            alice_engine.set_trust(TrustSet::All).await.unwrap();
            alice_view_of_bob.set_trust(TrustSet::All).await.unwrap();

            // Bob subscribes BEFORE Alice connects so the registry
            // entry is in Bob's store at bootstrap-digest time.
            let mut bob_stream = bob_bus
                .subscribe(Filter::NamePrefix(Bytes::from_static(b"liveness-test/")))
                .await
                .unwrap();

            // Connect alice → bob.
            alice_engine
                .add_peer(PeerAddr::new(Bytes::from_static(b"bob")))
                .await
                .unwrap();

            // Wait for Alice's registry to learn Bob's filter.
            let bob_vk = bob_identity.store_verifying_key();
            let propagated = async {
                loop {
                    if alice_engine.knows_peer_subscription(&bob_vk).await {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            };
            tokio::time::timeout(Duration::from_secs(2), propagated)
                .await
                .expect("alice learned bob's subscription");

            // Bob keeps a Liveness tracker (production-clock; we won't
            // exercise stale transitions here, just the Live transition).
            let liveness = Liveness::new(Duration::from_secs(3));
            let mut state_changes = liveness.subscribe().await;

            // Bob's decoder loop: pull from Bus, decode, feed Liveness.
            let liveness_for_decoder = Arc::clone(&liveness);
            tokio::task::spawn_local(async move {
                while let Some(ev) = bob_stream.next().await {
                    if let BusEvent::Ephemeral(dg) = ev {
                        let event = TestEvent::decode(&dg.payload);
                        let peer = PeerId(dg.verifying_key);
                        liveness_for_decoder.observe_event(peer, &event).await;
                    }
                }
            });

            // Alice publishes one event. The sender_time inside the
            // payload is what Liveness will record.
            let alice_claimed_time_micros = 100_000_000;
            let event = TestEvent {
                sender_time_micros: alice_claimed_time_micros,
                payload: vec![0xAA; 8],
            };
            alice_bus
                .publish_ephemeral(
                    Bytes::from_static(b"liveness-test/alice/0001"),
                    event.encode(),
                )
                .await
                .unwrap();

            // Bob's Liveness should report Live for Alice within a
            // generous window.
            let change = tokio::time::timeout(
                Duration::from_millis(500),
                state_changes.next(),
            )
            .await
            .expect("change arrived in time")
            .expect("subscriber stream open");
            assert_eq!(change.state, LivenessState::Live);
            assert_eq!(
                change.peer.0.as_bytes(),
                alice_identity.store_verifying_key().as_bytes()
            );
            assert_eq!(
                change.last_heard_at,
                SystemTime::UNIX_EPOCH + Duration::from_micros(alice_claimed_time_micros),
            );

            alice_run.abort();
            bob_run.abort();
        })
        .await;
}
```

- [ ] **Step 2: Run the integration test**

```bash
nix develop --command cargo test -p sunset-core --all-features --test liveness_with_bus
```

Expected: `liveness_tracks_alice_via_bob_bus_subscription ... ok`.

- [ ] **Step 3: Run the full sunset-core test suite to confirm no regressions**

```bash
nix develop --command cargo test -p sunset-core --all-features
```

Expected: all tests pass — unit tests in `liveness.rs`, `bus.rs`, and the existing `bus_integration` and `liveness_with_bus`.

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-core/tests/liveness_with_bus.rs
git commit -m "Liveness: integration test with Bus over TestTransport

Two-engine setup mirroring bus_integration.rs: Alice publishes an
ephemeral event carrying a sender_time field; Bob's decoder loop
pulls from Bus.subscribe, decodes, and feeds (peer, sender_time)
into Liveness. Verifies Liveness fires Live for Alice with the
correct sender-claimed timestamp through the real Bus subscription
path. Same scaffolding Plan C2 voice will use, minus encryption +
Opus."
```

---

## Task 7: Lint, format, and full workspace build

**Files:** No code changes; verifies the workspace is clean.

- [ ] **Step 1: Clippy on host target**

```bash
nix develop --command cargo clippy -p sunset-core --all-targets --all-features -- -D warnings
```

Expected: exit 0, no warnings. If clippy flags an issue (e.g. an unused import, a redundant clone), fix it inline before committing.

- [ ] **Step 2: Clippy on wasm32 target**

```bash
nix develop --command cargo clippy --target wasm32-unknown-unknown -p sunset-core -- -D warnings
```

Expected: exit 0, no warnings. (Note: `--all-targets` doesn't apply on wasm32 because the integration test is host-only; just lint the lib.)

- [ ] **Step 3: Workspace clippy to confirm no downstream breakage**

```bash
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: exit 0. Liveness is a new module in `sunset-core`; nothing downstream should reference it yet, so this should be clean modulo any unrelated drift.

- [ ] **Step 4: cargo fmt check**

```bash
nix develop --command cargo fmt --all --check
```

Expected: exit 0. If fmt drift, run `nix develop --command cargo fmt --all` and commit the formatting changes as a separate commit (per project rule — never amend).

- [ ] **Step 5: Full workspace test**

```bash
nix develop --command cargo test --workspace --all-features
```

Expected: all tests pass. This is the final "nothing else broke" gate.

- [ ] **Step 6: Commit any fmt drift**

If Step 4 produced formatting changes:

```bash
git add -u
git commit -m "fmt: apply rustfmt after Liveness wiring"
```

If no changes, skip.

---

## Spec coverage check (self-review)

| Spec section / requirement | Implemented in |
|---|---|
| `Liveness` is a pure tracker (no Bus, no decryption) | Tasks 1–4: liveness.rs has no Bus or Room imports |
| Per-namespace granularity (caller chooses filter) | Implicit — `Liveness` doesn't subscribe; each consumer instance is one tracker, one namespace |
| Sender-claimed time, not receive time | Task 2: `observe(peer, sender_time)` takes sender's claim |
| Out-of-order observations ignored | Task 2: test `observe_out_of_order_is_ignored` |
| `LivenessState = Live | Stale` | Task 1: enum definition |
| `PeerLivenessChange` carries `last_heard_at` for tooltips | Task 1: struct definition |
| `Clock` trait for testability + `SystemClock` + `MockClock` | Task 1: types; tests use `MockClock` from Task 2 onward |
| `observe(peer, sender_time)` API | Task 2 + Task 4 (broadcast added) |
| `subscribe()` returns stream of changes (no replay) | Task 3: test `subscribe_does_not_replay_existing_state` |
| `snapshot()` returns current state | Task 2: snapshot method + test |
| `HasSenderTime` trait + `observe_event` sugar | Task 5 |
| Stale detection on observe (sweep other peers) | Task 4: `observe` body |
| Stale detection without observations (subscribe interval) | Task 4: `subscribe` uses `tokio::time::interval` |
| `run_sweep` public for test/manual cadence control | Task 4 |
| Multiple subscribers receive same events | Task 3: test `multiple_subscribers_receive_same_event` |
| Slow subscriber doesn't block | Task 4 + Task 3: per-subscriber unbounded mpsc; broadcast retains live senders only |
| Broadcast happens inside the inner-mutex critical section | Task 3 + Task 4: `broadcast()` called while `inner` lock is held |
| Replay-window enforcement is consumer's job, not Liveness's | Task 6 integration test does not test replay rejection (consumer concern); spec out-of-scope honored |
| Wire-format unchanged (no SignedDatagram edits) | No tasks touch sunset-store; verified by Task 7 workspace build |
| End-to-end with Bus | Task 6 |
| Lint + fmt clean | Task 7 |

Self-review: every spec requirement maps to a concrete task. No placeholders. Type names (`Liveness`, `LivenessState`, `PeerLivenessChange`, `Clock`, `SystemClock`, `MockClock`, `HasSenderTime`, `observe`, `observe_event`, `subscribe`, `snapshot`, `run_sweep`) are consistent across all tasks.

One non-obvious type-consistency note: `subscribe` takes `&self` in Task 3 but `&Arc<Self>` in Task 4 — Task 4 explicitly notes this signature change and that call sites already pass `Arc<Liveness>` so the change is invisible at usage. Tests are unaffected.

---

## Done criteria

- [ ] Task 1 commit landed: skeleton + types + Cargo + lib.rs.
- [ ] Task 2 commit landed: observe + snapshot + out-of-order ignored.
- [ ] Task 3 commit landed: subscribe + Live emissions on observe.
- [ ] Task 4 commit landed: stale detection (run_sweep + observe-side sweep + subscribe-stream interval).
- [ ] Task 5 commit landed: HasSenderTime + observe_event.
- [ ] Task 6 commit landed: Bus integration test.
- [ ] Task 7: clippy clean (host + wasm32 + workspace), fmt clean, full workspace tests pass.
- [ ] Spec coverage table fully checked.
