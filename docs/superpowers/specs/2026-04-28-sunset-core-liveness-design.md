# sunset-core Liveness Layer Design

**Date:** 2026-04-28
**Scope:** New `liveness` module in `sunset-core`. Voice and other ephemeral consumers are out of scope (this is Plan C1; Plan C2 is voice).
**Predecessor:** Bus pub/sub (`docs/superpowers/specs/2026-04-28-sunset-bus-pubsub-design.md`) — already merged. Liveness consumes decoded payloads above the Bus, not Bus itself.
**Successor:** Voice end-to-end (Plan C2, future) wires getUserMedia capture, Opus encode/decode, JS audio bridge, and uses `Liveness` for per-peer "in the call" UI state.

## Goal

Provide a generic, decryption-aware liveness tracker that ephemeral consumers can use to answer "which peers have I heard from recently?" without reinventing the bookkeeping. First user is voice (Plan C2); later users include typing indicators, cursors, and any future ephemeral subscription that needs a "live" UX signal.

## The architectural problem

`Bus::publish_ephemeral` / `Bus::subscribe` is pub/sub, not connection-oriented. Datagrams may arrive direct, via relay, or (eventually) multi-hop. There is no "is peer X connected?" question the Bus can answer — only "is peer X publishing in this filter?" Liveness in this design means "have we received a fresh-enough event from this peer in the namespace we care about?"

Three subtleties shape the design:

1. **Receive-time is wrong.** Datagrams can be delayed or reordered. A frame produced 30 seconds ago that just arrived means the sender was alive *then*, not necessarily *now*. We need the sender's claim of when they produced the event.

2. **The sender's claim must be encrypted.** A plaintext timestamp on `SignedDatagram` would let any forwarder build a complete activity graph for any peer they relay. The whole point of `Room` encryption is to keep that metadata hidden. Sender-claimed time therefore lives **inside the encrypted payload**, decoded by the consumer's `Room` before being fed to liveness.

3. **Liveness has no business knowing payload formats.** Voice frames, typing events, and cursor updates have nothing in common except "they're produced at some time by some peer." A trait extracts the timestamp; liveness depends on the trait, not on any specific format.

## Architecture

`Liveness` is a **pure tracker**. It does not subscribe to Bus, does not decrypt, does not know about voice or typing or any specific protocol. It bookkeeps `(peer, sender_time) → state` observations and emits state changes.

```
Bus (encrypted ephemeral events)
    ↓                        Bus.subscribe(filter)
Consumer protocol (Plan C2 voice, future typing, etc.)
    ↓ decrypts payload via Room
    ↓ extracts sender_time field from decoded struct
Liveness.observe(peer, sender_time)   ← the only thing Liveness sees
    ↓ bookkeeping
Liveness.subscribe() → stream of PeerLivenessChange
    ↓
UI (Gleam, via Client FFI in C2)
```

Replay-window enforcement (rejecting frames whose sender_time is impossibly old or impossibly far in the future relative to local clock) is also a consumer responsibility, not a liveness responsibility — it requires per-protocol policy (voice tolerates ~3s of delay, typing tolerates ~30s) and shouldn't be hardcoded in the tracker.

## Components

### `Liveness` (the tracker)

```rust
pub struct Liveness {
    stale_after: Duration,
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
}

struct Inner {
    peers: HashMap<PeerId, PeerEntry>,
    subscribers: Vec<UnboundedSender<PeerLivenessChange>>,
}

struct PeerEntry {
    last_heard_at: SystemTime,
    state: LivenessState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LivenessState {
    Live,
    Stale,
}

#[derive(Clone, Debug)]
pub struct PeerLivenessChange {
    pub peer: PeerId,
    pub state: LivenessState,
    pub last_heard_at: SystemTime,
}
```

API surface:

```rust
impl Liveness {
    pub fn new(stale_after: Duration) -> Arc<Self>;
    pub fn with_clock(stale_after: Duration, clock: Arc<dyn Clock>) -> Arc<Self>;

    /// Record that we received a fresh event from `peer` claiming it was
    /// produced at `sender_time`. Out-of-order observations (older than
    /// our current `last_heard_at`) are ignored — liveness state never
    /// goes backwards from a single observation.
    pub fn observe(&self, peer: PeerId, sender_time: SystemTime);

    /// Subscribe to state-change events. New peers fire `Live`; peers
    /// that exceed `stale_after` since `last_heard_at` fire `Stale`;
    /// stale peers that observe again fire `Live`. No event is emitted
    /// when a Live peer simply observes again.
    pub fn subscribe(&self) -> Stream<PeerLivenessChange>;

    /// Read the current state of every tracked peer.
    pub fn snapshot(&self) -> HashMap<PeerId, PeerLivenessChange>;
}
```

### `HasSenderTime` (sugar trait)

```rust
pub trait HasSenderTime {
    fn sender_time(&self) -> SystemTime;
}

impl Liveness {
    /// Convenience: observe an event whose decoded payload knows its
    /// own sender time. Equivalent to `observe(peer, ev.sender_time())`.
    pub fn observe_event<T: HasSenderTime>(&self, peer: PeerId, ev: &T);
}
```

Voice's decoded `VoiceFrame { sender_time, opus: Bytes }` will implement `HasSenderTime` so the consumer's loop reads:

```rust
liveness.observe_event(peer, &decoded_frame);
```

### `Clock` (testability)

```rust
pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

pub struct SystemClock;
impl Clock for SystemClock { fn now(&self) -> SystemTime { SystemTime::now() } }
```

Tests inject a manually-advanced `MockClock`; production uses `SystemClock`. Required because liveness state transitions are time-driven and tests need deterministic control.

### Stale detection

A background sweep task (started lazily on first `subscribe` or `observe`) wakes up every `stale_after / 2` and fires `Stale` for any peer whose `now() - last_heard_at > stale_after` AND whose current state is `Live`. The sweep also runs once at the end of every `observe` call (cheap; same lock anyway), so transitions don't depend on the sweep cadence in steady state — the periodic sweep just guarantees stale events fire even when no further observations arrive.

## Data flow (voice walkthrough, for context)

```
Alice (publisher):
  encode frame → encrypt with sender_time field → Bus.publish_ephemeral
                                                  ("voice/room-a/alice/0042")

Bob (subscriber):
  Bus.subscribe(NamePrefix("voice/room-a/")) → SignedDatagram
  → Room.decrypt(payload) → DecodedVoiceFrame { sender_time, opus }
  → liveness.observe_event(alice_peer, &frame)  // tracker side-effect
  → opus_decoder.push(frame.opus)               // audio side-effect

Bob's UI:
  liveness.subscribe() → stream of PeerLivenessChange
  → updates green dot next to Alice's name
```

## Failure modes

| Scenario | Behaviour |
|---|---|
| Out-of-order observation (older `sender_time` than current `last_heard_at`) | Ignored. State never moves backward from a single observation. |
| `sender_time` far in the future (clock skew or attack) | Liveness has no policy here — clamping/rejection is the consumer's concern (per-protocol replay window). Liveness records what it's told. |
| Decryption fails on an event | Consumer drops the event before calling `observe`. Liveness sees nothing. |
| Peer disappears (no further events) | Sweep emits `Stale` after `stale_after` since `last_heard_at`. The peer entry stays in the map (so `last_heard_at` is still readable for tooltips). No "Gone" state in v1; consumers can derive it from `last_heard_at` if needed. |
| Multiple subscribers, one slow | Each subscriber has its own unbounded channel — slow subscribers don't block fast ones. (Same invariant as `sunset-store-memory` subscriptions: per-subscriber unbounded sender, broadcast-under-lock.) |
| Subscriber dropped without unsubscribing | `Sender::send` returns Err on the next broadcast; Liveness drops dead senders inside its own lock. |
| `observe` called from multiple tasks concurrently | `Mutex<Inner>` serializes; broadcast happens inside the critical section so a subscriber registered before observe N either sees N or sees nothing. |

## Testing strategy

`crates/sunset-core/src/liveness.rs` with co-located unit tests using `MockClock`:

1. **Single-peer Live transition.** Observe a peer; subscriber sees `Live`. Advance clock past `stale_after`; sweep fires `Stale`. Observe again; subscriber sees `Live`.
2. **Out-of-order ignored.** Observe peer at T=10. Observe peer at T=5 (older). State unchanged, no event fired.
3. **Multi-peer independence.** Observe Alice and Bob. Let Alice go stale; Bob remains Live.
4. **Subscriber registered after observation.** Subscribe; receive snapshot via initial events? Decision: **no initial replay** — `subscribe` returns a stream of changes from the moment of subscription. `snapshot()` is the way to get current state. (Avoids the "do we replay every peer as Live on subscribe?" question.)
5. **Multiple subscribers receive same events.** Two subscribers, one observation; both see the change.
6. **Slow subscriber doesn't block.** Observe rapidly; one subscriber drains, the other doesn't. Both eventually see all changes (unbounded channels).
7. **`HasSenderTime` convenience.** A test struct implementing the trait, fed via `observe_event`, lands the same observation as a manual `observe`.

Plus an integration test in `crates/sunset-core/tests/liveness_with_bus.rs`:

8. **Liveness driven by a real Bus subscription.** Two `Bus` instances over `TestTransport`; Alice publishes a stream of decrypted-payload-equivalent events on a filter; Bob's loop pipes them into `Liveness`; Bob's subscriber observes Alice transition Live → Stale → Live as the stream pauses + resumes. Uses `MockClock` so the test is deterministic.

## Out of scope

- **Voice frames, Opus, audio bridge** — Plan C2.
- **Heartbeat publishing helpers.** Consumers handle their own publish cadence (voice DTX silence frames, typing's own keepalive, etc). If a single helper pattern emerges across three consumers, add a sugar function then.
- **`Gone` state / peer eviction** — derivable from `last_heard_at` if a consumer needs it; v1 stops at `Stale`.
- **Per-namespace liveness composition** — if a peer is "live in voice/room-a" but stale in "voice/room-b", that's two separate `Liveness` instances. Liveness is namespace-scoped by virtue of its consumer's filter, not internally.
- **Wire-format changes to `SignedDatagram`** — sender_time stays in the encrypted payload, never on the envelope. No frozen-vector regenerate.
- **Replay-window policy** — consumer's job (per-protocol).

## Risks

1. **Sweep latency.** A peer that goes silent right after observation won't fire `Stale` until `stale_after + sweep_interval/2` ≈ `1.5 * stale_after`. For voice (`stale_after ≈ 3s`), the worst-case green→grey transition is ~4.5s. Acceptable. If we ever care about tighter bounds, the sweep can be replaced with per-peer `tokio::time::sleep_until` futures.
2. **Clock-skew handling delegated to consumer.** Liveness trusts the `sender_time` it's handed. A malicious peer could fake liveness by claiming future timestamps; an honest peer with a wildly skewed clock could look "alive" forever or "stale" forever. Each consumer protocol enforces its own sanity bound (voice: drop frames > 30s out of [now-3s, now+3s]).
3. **`HasSenderTime` is a sugar trait.** Consumers can also call `observe(peer, time)` directly — no requirement to implement the trait. The trait exists to remove one line of boilerplate per call site.
4. **`Arc<Liveness>` shared across tasks.** Standard `?Send` async-trait pattern in WASM; the inner `tokio::sync::Mutex` is `?Send`-compatible. No `Send + Sync` assumption needed.

## Review summary

- **Placeholders:** none.
- **Internal consistency:** the failure-mode table aligns with the components section. `LivenessState` is two-valued; `PeerLivenessChange` carries `last_heard_at` for tooltips. Tests cover each documented behaviour.
- **Scope:** this is one focused module (`liveness.rs`) plus tests. Voice and other consumers are explicitly Plan C2.
- **Ambiguity:** "subscribe doesn't replay current state" is called out in test 4 to remove the most likely misinterpretation.
