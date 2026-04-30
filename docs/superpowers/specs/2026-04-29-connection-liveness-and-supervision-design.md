# Connection Liveness & Supervision Design

**Date:** 2026-04-29
**Scope:** Two changes in `sunset-sync`: (1) per-connection heartbeat in `run_peer`, (2) a new `PeerSupervisor` module that maintains durable connection intents above the engine. Plus a thin rewire of `Client::add_relay` / `Client::connect_direct` in `sunset-web-wasm` to use the supervisor.
**Out of scope (explicit):** wire-format changes outside `SyncMessage` enum extension, transport-level changes (WS/WebRTC raw transports), multi-relay routing, peer presence (already handled by `sunset-core` `Liveness`).

## Goal

When the relay restarts, or a client device sleeps and resumes, the WebSocket relay path currently goes silent: messages stop flowing in both directions until the user reloads the page. Two distinct mechanisms produce the same symptom and need different fixes:

1. **Stale-but-open connection.** The OS / browser hasn't noticed the underlying TCP is dead (most common after laptop sleep/resume). The socket reports as open; reads block forever; writes succeed into a void. No error event ever fires.
2. **Cleanly-closed connection with no redial.** The relay restarts, the socket closes properly, the engine emits `PeerRemoved`, and nothing reconnects.

Both must be solved, and the solution must be **transport-agnostic** (works for WebSocket-via-relay, WebRTC direct datachannel, and any future transport that lands in `MultiTransport`) and keep the top-level API simple (`client.add_relay(url)` / `client.connect_direct(pubkey)` register a durable intent and never need re-arming).

## Layered approach

The two failure modes split cleanly into two pieces, each useful on its own:

```
PeerSupervisor (policy: durable intent + redial backoff)
    ↓ uses public API: engine.add_peer, engine.subscribe_engine_events
SyncEngine (mechanism: connections, peer events)
    ↓ runs per-peer task
run_peer + heartbeat (mechanism: detect dead channels via Ping/Pong)
    ↓ rides
TransportConnection (Noise-encrypted, AEAD per frame)
```

**Heartbeat is always-on**; **redial supervision is opt-in** (the engine itself stays single-shot — `add_peer` is a one-time dial. The supervisor wraps it for callers that want durable connections). This separation matches the rest of the stack: the engine and `Liveness` are mechanism-only, consumers drive policy.

## Why heartbeats live at the `SyncMessage` layer

`SyncMessage::Ping`/`Pong` ride `send_reliable` exactly like every other engine message:

- Encrypted + authenticated by Noise (AEAD per frame, bound to the peer's static key during handshake).
- Replay-protected by Noise's nonce counter.
- An off-path attacker can't inject, replay, or forge them; a malicious relay flipping bytes can't fake liveness either (Noise rejects modified ciphertext).

Putting heartbeats anywhere lower would lose these properties:

- **Browser `WebSocket`** doesn't expose RFC 6455 ping/pong frames — there's no JS API.
- **Even if it did**, those frames sit *below* Noise, so they wouldn't be encrypted/authenticated end-to-end with the peer.
- **Per-transport heartbeats** (e.g., WebRTC's built-in keepalives) leak the responsibility into every transport crate and don't compose with `MultiTransport`.

`SyncMessage`-level heartbeats apply uniformly: WS, WebRTC, future WebTransport, and `TestTransport` all benefit with no per-transport code. Liveness verifies "the channel to the authenticated peer is alive," which is exactly the property we want.

## Piece 1: heartbeat in `run_peer`

### Wire-format additions

Two new variants on `SyncMessage` (defined in `crates/sunset-sync/src/message.rs`):

```rust
SyncMessage::Ping { nonce: u64 }
SyncMessage::Pong { nonce: u64 }
```

Postcard encoding appends new variants by index, so this is non-breaking with existing peers in the sense that older nodes will reject unknown variants — but since we're rolling out on both sides simultaneously (web client and relay), no graceful-degradation is needed. We pin a hex test vector for `SyncMessage::Ping { nonce: 1 }` and `SyncMessage::Pong { nonce: 1 }` analogous to the `ContentBlock::hash()` frozen vector in `sunset-store/src/types.rs`, so accidental wire-format drift surfaces in CI.

### Routing

Both Ping and Pong route to the **reliable** channel in `outbound_kind` (`crates/sunset-sync/src/peer.rs:211`). They are explicitly listed (no wildcard) so adding them forces the compile-time decision the existing comment requires.

### `run_peer` extension

`run_peer` currently runs three concurrent sub-tasks (`recv_reliable_task`, `recv_unreliable_task`, `send_task`) joined by `tokio::join!`. We add a fourth: `liveness_task`, plus a `Ping` arm and a `Pong` arm in `recv_reliable_task`'s match.

**Routing pings and pongs.** Both heartbeat directions go through the **per-peer outbound channel** (`out_tx`), not directly through `conn.send_reliable`. The send-task in `run_peer` is the single writer to the wire; that serialization matters because `NoiseTransport` increments an AEAD nonce per send and concurrent calls from multiple tasks would be a footgun. `run_peer` keeps a clone of `out_tx` for use by the liveness task and the recv-loop's pong responder.

**Ping handling (responder side):** when the recv-reliable loop decodes `SyncMessage::Ping { nonce }`, it sends `SyncMessage::Pong { nonce }` on the cloned `out_tx`. The send-task drains that and writes it to the wire like any other message.

**Pong handling + timeout (initiator side):** the liveness task owns:

```rust
struct LivenessState {
    next_nonce: u64,
    last_pong_at: Instant,         // wasmtimer::Instant on wasm32
}
```

It loops on a tick interval (`heartbeat_interval`; default 15s, configurable on `SyncConfig`):

1. Send `Ping { nonce: next_nonce }` via `out_tx`. Increment `next_nonce`.
2. If `now - last_pong_at > heartbeat_timeout` (default 45s, configurable), emit `InboundEvent::Disconnected { peer_id, reason: "heartbeat timeout" }` on `inbound_tx` and break the task.

When a `Pong { nonce }` arrives (delivered to the liveness task via a small in-task channel from the recv loop), it updates `last_pong_at = now`. The nonce is informational — we don't need to track outstanding pings explicitly; any pong is evidence the channel is alive. Pongs for unknown nonces are still benign for the same reason.

`heartbeat_interval` and `heartbeat_timeout` live on `SyncConfig`. Defaults: 15 s and 45 s (i.e. three missed pings before tear-down). The 3× ratio is conventional and gives a comfortable margin against transient packet loss; `tokio::time` / `wasmtimer::tokio` interval drift is tolerated within those bounds.

The liveness task uses `wasmtimer::tokio::sleep` on `wasm32` (matching the existing `anti_entropy` pattern at `crates/sunset-sync/src/engine.rs:269`), `tokio::time::sleep` on native.

### Engine handling

`SyncEngine::handle_peer_message` (`crates/sunset-sync/src/engine.rs:442`) gets two new no-op arms — `Ping` and `Pong` are entirely handled by the per-peer task. Listing them (no wildcard) keeps the same compile-time-coverage discipline as the existing match.

### Failure-mode table (heartbeat)

| Scenario | Behaviour |
|---|---|
| Channel goes silent (sleep/resume, dead TCP) | Liveness times out after `heartbeat_timeout`; emits `Disconnected`; engine fires `PeerRemoved` like any other disconnect. |
| One Ping or Pong dropped in transit | Tolerated: `heartbeat_timeout = 3 × heartbeat_interval` so 1–2 missed pings recover when the next pong arrives. |
| Peer sends Ping but never Pong (or vice versa) | Asymmetric: each side runs its own liveness loop independently. If one side's pongs aren't arriving, that side disconnects on its own timer. The other side may still be receiving pongs; it disconnects when its own pings stop being answered. |
| Reordered Pongs (nonce 5 arrives before nonce 4) | Both update `last_pong_at`; both removed from `pending`. State is correct. |
| Pong with unknown nonce (e.g., from a reset peer that lost state) | Dropped silently. Doesn't affect timeout. |
| Send-task already torn down when liveness wants to send a Ping | The outbound channel is closed; `send` returns Err; liveness task exits. The reliable recv-task's own error path will independently emit `Disconnected`. |
| Ping arrives before Hello | Cannot happen: `run_peer` blocks on Hello before starting the four concurrent loops. The liveness task only exists post-Hello. |

### Testing (heartbeat)

In `crates/sunset-sync/src/peer.rs` `mod tests`, all using `TestTransport` and `tokio::test(flavor = "current_thread")` like existing tests. Time is controlled via `tokio::time::pause()` + `advance` (native test target):

1. **Happy path: pings flow.** Two peers complete Hello; verify on each side that a `Ping` is sent, the other side's `Pong` arrives, `last_pong_at` advances. (Probe via test-only accessor or via observing that no `Disconnected` event fires after `heartbeat_timeout` elapses.)
2. **Silent peer triggers timeout.** Use a custom `TransportConnection` wrapper that swallows outbound `Pong` (acks nothing). Advance virtual clock past `heartbeat_timeout`; verify `Disconnected { reason: "heartbeat timeout" }` fires.
3. **Recovers from one missed ping.** Wrapper drops every other Pong. Advance clock through several intervals; verify no `Disconnected` (the recovering pongs reset `last_pong_at`).
4. **Reordered Pongs accepted.** Wrapper buffers Pongs and releases them out of order. No `Disconnected`.
5. **Unknown-nonce Pong dropped.** Inject a `Pong { nonce: u64::MAX }` directly into the recv channel (using a `TransportConnection` wrapper that prepends one); verify no `Disconnected` (handler is silent) and that the regular ping/pong loop still progresses.
6. **Frozen-vector regression.** Test that `postcard::to_stdvec(&SyncMessage::Ping { nonce: 1 })` matches a hex-pinned byte string; same for `Pong { nonce: 1 }`.

## Piece 2: `PeerSupervisor`

### Module

`crates/sunset-sync/src/supervisor.rs`, exported from the crate root. Lives in `sunset-sync` because it consumes the engine's public API and there's no value in a separate crate; pure policy that requires no new dependencies beyond what the engine already pulls in.

### Types

```rust
pub struct PeerSupervisor<S, T>
where
    S: Store + 'static,
    T: Transport + 'static,
    T::Connection: 'static,
{
    engine: Rc<SyncEngine<S, T>>,
    cmd_tx: mpsc::UnboundedSender<SupervisorCommand>,
    state: Rc<RefCell<SupervisorState>>,
    policy: BackoffPolicy,
}

struct SupervisorState {
    intents: HashMap<PeerAddr, IntentEntry>,
    /// Map from PeerId to PeerAddr, populated when an intent transitions
    /// to Connected. Used to look up which intent owns a `PeerRemoved`
    /// event by `peer_id`. Multiple intents to the same peer aren't
    /// supported — `add` for an existing addr is idempotent.
    peer_to_addr: HashMap<PeerId, PeerAddr>,
}

struct IntentEntry {
    state: IntentState,
    attempt: u32,                  // backoff counter; reset on Connected
    peer_id: Option<PeerId>,       // learned at PeerAdded
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentState {
    Connecting,
    Connected,
    Backoff,
    Cancelled,                     // pending removal
}

#[derive(Clone, Debug)]
pub struct IntentSnapshot {
    pub addr: PeerAddr,
    pub state: IntentState,
    pub peer_id: Option<PeerId>,
    pub attempt: u32,
}

#[derive(Clone)]
pub struct BackoffPolicy {
    pub initial: Duration,         // default 1s
    pub max: Duration,             // default 30s
    pub multiplier: f32,           // default 2.0
    pub jitter: f32,               // default 0.2 (±20%)
}

enum SupervisorCommand {
    Add { addr: PeerAddr, ack: oneshot::Sender<Result<()>> },
    Remove { addr: PeerAddr, ack: oneshot::Sender<()> },
}
```

### API

```rust
impl<S, T> PeerSupervisor<S, T> { /* trait bounds as above */
    pub fn new(engine: Rc<SyncEngine<S, T>>, policy: BackoffPolicy) -> Rc<Self>;

    /// Long-running task. Caller spawns this with `spawn_local`.
    pub async fn run(self: Rc<Self>);

    /// Register a durable intent. If the addr is already registered, no-op.
    /// Returns once the FIRST connection completes successfully, OR returns
    /// an error if the first dial fails (transient errors after the first
    /// successful connection are absorbed silently).
    pub async fn add(&self, addr: PeerAddr) -> Result<()>;

    /// Cancel an intent and tear down the connection if connected.
    pub async fn remove(&self, addr: PeerAddr);

    /// Snapshot of all active intents. For UI / debugging.
    pub fn snapshot(&self) -> Vec<IntentSnapshot>;
}
```

### `run()` loop

```rust
async fn run(self: Rc<Self>) {
    let mut events = self.engine.subscribe_engine_events().await;
    let mut cmd_rx = /* taken from cmd_tx pair created in new() */;

    loop {
        // Compute next wakeup: min of all intent.next_attempt_at.
        let wakeup = next_backoff_wakeup(&self.state);

        tokio::select! {
            Some(ev) = events.recv() => self.handle_engine_event(ev).await,
            Some(cmd) = cmd_rx.recv() => self.handle_command(cmd).await,
            _ = sleep_until(wakeup) => self.fire_due_backoffs().await,
        }
    }
}
```

`sleep_until` uses `wasmtimer::tokio::sleep` on `wasm32`, `tokio::time::sleep` on native. When no intents are in `Backoff`, the sleep is a far-future placeholder (`Duration::from_secs(86400)`); the engine event / command arms wake us up sooner whenever something happens.

### Engine API change to support correlation

`SyncEngine::add_peer` currently returns `Result<()>` and acks immediately after `transport.connect(addr)` succeeds — *before* the Hello exchange completes. The docstring at `crates/sunset-sync/src/engine.rs:139` already claims it returns "when the connection is established + Hello-exchanged"; the code doesn't match. We fix the code to match the doc *and* return the peer's identity:

```rust
pub async fn add_peer(&self, addr: PeerAddr) -> Result<PeerId>;
```

Implementation: `EngineCommand::AddPeer { addr, ack: oneshot::Sender<Result<PeerId>> }`. Inside the command handler, after `transport.connect(addr).await`, instead of immediately spawning `run_peer` and acking, we wire a one-shot channel into the per-peer task; `run_peer` signals it after `PeerHello` is received and validated, *before* entering its concurrent recv/send loops. The ack carries the resulting `PeerId`.

This change is small and aligns the API with its existing doc. Existing callers (`Client::add_relay`, `Client::connect_direct`) become two-line edits to ignore the new return value or use it.

### Handlers

**`handle_engine_event(EngineEvent::PeerAdded { peer_id, .. })`:**
Confirm `state = Connected`, `attempt = 0`, and update `peer_to_addr`. The `peer_id` was already populated by the supervisor's dial wrapper (which receives it from `engine.add_peer().await`); the event arm is just the latch confirming the engine has the peer in its outbound table.

The supervisor ignores `PeerAdded` for peer_ids that aren't in any intent (i.e., inbound `accept`-path peers — relevant if the supervisor ever runs in a process that also accepts; harmless in v1 where supervised processes are dial-only clients).

**`handle_engine_event(EngineEvent::PeerRemoved { peer_id })`:**
Look up `addr = peer_to_addr.remove(peer_id)`. If found and the intent isn't `Cancelled`, set `state = Backoff`, schedule `next_attempt_at = now + delay(attempt)`, and clear `peer_id`. If not found (peer wasn't supervised), ignore.

**`fire_due_backoffs()`:**
For each intent where `state == Backoff` and `next_attempt_at <= now`:

1. Set `state = Connecting`.
2. `spawn_local(async move { let r = engine.add_peer(addr).await; /* report back */ })`.
3. On success: `state = Connected`, `attempt = 0`. The matching `PeerAdded` event in the events arm will populate `peer_id`.
4. On failure: `attempt += 1`, `state = Backoff`, schedule next.

**`handle_command(Add)`:**
Idempotent. If addr exists, send Ok back without redialing. Otherwise insert with `state = Connecting`, dial via `engine.add_peer`, return the dial result through the ack channel. The first-dial result is what `add()`'s caller sees.

**`handle_command(Remove)`:**
Set `state = Cancelled`. Don't currently have a way to force-disconnect from outside the engine — engine has no `remove_peer`. Add one: `SyncEngine::remove_peer(peer_id)` that closes the outbound channel for that peer (drop the sender; per-peer task's send loop exits → tears down). Already partly supported by the existing `EngineCommand` machinery.

### Backoff policy

`delay(attempt)` returns `min(initial * multiplier^attempt, max)` with multiplicative jitter `1.0 ± jitter`. With defaults: 1 s, 2 s, 4 s, 8 s, 16 s, 30 s, 30 s, … (each ±20 %). Never gives up; an offline relay just keeps retrying every 30 s. Apps that want give-up semantics can read `IntentSnapshot.attempt` and call `remove()` themselves.

### Failure-mode table (supervisor)

| Scenario | Behaviour |
|---|---|
| First dial fails (bad URL, no network, relay down) | `add()` returns Err. Caller decides what to do. Supervisor does NOT auto-retry the first dial — first-dial errors are setup errors, not transient. |
| Connection drops after first success | Supervisor sees `PeerRemoved`, schedules backoff, redials. `add()` already returned successfully; the failure is silent. |
| Engine dropped while supervisor running | `events.recv()` returns None; supervisor exits cleanly. |
| Supervisor dropped while engine running | Engine continues; existing connections keep working. The intents are forgotten. |
| Two concurrent `add(same_addr)` calls | First wins, second observes the addr already exists and returns Ok immediately (idempotent). Both callers see success once the first dial completes. |
| `add()` called for an addr currently in `Backoff` after a prior `remove()` then `add()` | `remove()` set `Cancelled`; if a backoff fired before the second `add()` arrives the redial-result handler checks state == Cancelled and discards the connection. The second `add()` re-inserts a fresh intent. |
| Network briefly flaps (reconnect succeeds on first retry) | One `PeerRemoved` event, one redial after `initial` (1s default), success → `attempt = 0`. |
| Computer sleeps for hours | Heartbeat times out within 45 s of resume → `PeerRemoved` → backoff → redial on next tick. User sees "connecting" → "connected" within seconds of the wake-up. |

### Testing (supervisor)

`crates/sunset-sync/src/supervisor.rs` colocated unit tests + an integration test under `crates/sunset-sync/tests/supervisor_with_engine.rs`. Time is controlled with `tokio::time::pause()` (native test only; the `wasmtimer` path isn't testable inline and is exercised in `crates/sunset-web-wasm`'s playwright suite).

Unit tests (mock engine via the existing `TestTransport`):

1. **First dial success.** `add(addr)` returns Ok; `IntentState = Connected`; `attempt = 0`.
2. **First dial failure.** Make `engine.add_peer` fail (no listener at addr). `add(addr)` returns Err. Intent is removed (no zombie state).
3. **Reconnect after disconnect.** Connect, then close the underlying connection. Advance clock by `initial`. New connection appears. State returns to `Connected`. `attempt = 0`.
4. **Backoff escalation.** Make redials fail. Advance clock; verify attempts at `initial`, `initial * multiplier`, …, capped at `max`.
5. **Idempotent add.** Two `add(same_addr)` calls return Ok; only one connection appears.
6. **Remove cancels backoff.** Connect, disconnect, then `remove(addr)` while in Backoff. Verify no further dial attempts and that `peer_to_addr` is cleared.
7. **Snapshot.** Two intents in different states; `snapshot()` returns both with correct `IntentState`.

Integration test:

8. **Real engine + heartbeat + supervisor.** Two `SyncEngine`s over `TestTransport`. Connect via supervisor on side A. Inject a `TransportConnection` wrapper that drops all outbound traffic from side A's connection (simulating sleep). Advance virtual clock past `heartbeat_timeout`. Verify A emits `PeerRemoved`, supervisor sees it, redials, and the new connection works (B accepts a fresh connection).

## Piece 3: top-level API

`crates/sunset-web-wasm/src/client.rs` becomes:

```rust
pub struct Client {
    /* existing fields */,
    supervisor: Rc<PeerSupervisor<MemoryStore, MultiTransport<WsT, RtcT>>>,
}

// in Client::new, after engine is constructed:
let supervisor = PeerSupervisor::new(engine.clone(), BackoffPolicy::default());
spawn_local({
    let s = supervisor.clone();
    async move { s.run().await }
});

// add_relay becomes:
pub async fn add_relay(&self, url_with_fragment: String) -> Result<(), JsError> {
    let addr = PeerAddr::new(Bytes::from(url_with_fragment));
    self.supervisor.add(addr).await
        .map_err(|e| JsError::new(&format!("add_relay: {e}")))?;
    Ok(())
}

// connect_direct becomes:
pub async fn connect_direct(&self, peer_pubkey: &[u8]) -> Result<(), JsError> {
    let addr = /* same construction as today */;
    self.supervisor.add(addr).await
        .map_err(|e| JsError::new(&format!("connect_direct: {e}")))?;
    Ok(())
}
```

The supervisor doesn't know or care that one addr is "a relay" and the other is "a direct peer." Both are `PeerAddr`s, both get heartbeats, both get redial supervision. The relay binary doesn't run a `PeerSupervisor` — relays only `accept` inbound connections and the heartbeat alone is enough to detect dead clients (the engine's existing `PeerRemoved` flow handles cleanup; the relay never tries to dial back).

The existing `relay_status` field and `on_relay_status_changed` callback continue to work — internally they're driven by the membership tracker (`crates/sunset-web-wasm/src/membership_tracker.rs`), which observes engine events and is unaffected by this change. Optionally, a future PR can switch them to read from the supervisor's `IntentSnapshot` for a more accurate three-state view; not in scope here.

## Risks

1. **Heartbeat traffic on idle connections.** With 15 s interval and ~12-byte encoded `Ping` (postcard variant tag + u64), each idle peer pair generates ~1.6 B/s of plaintext, multiplied by Noise framing overhead — negligible. If we ever care, increase `heartbeat_interval`.
2. **Backoff thundering-herd on relay restart.** If many clients lose their connection at once, they all redial within `initial ± jitter` of each other. The ±20% jitter spreads them somewhat; the relay accept loop already serializes correctly. If this becomes a problem in practice, increase `jitter` or add a relay-side accept rate limit (out of scope here).
3. **`PeerSupervisor` and `MultiTransport` interaction.** The supervisor calls `engine.add_peer(addr)`; `MultiTransport::connect` routes by URL prefix. If a URL with an unrecognized scheme is added, every redial returns the same error and the intent stays in escalating Backoff forever. Acceptable: it surfaces in the `attempt` counter and the caller can `remove()`. The first-dial error already returns to the caller of `add()`.
4. **No "give up" by default.** A client with a persistent typo in a relay URL retries forever. Mitigated by the first-dial error returning to `add()` — typos surface immediately. Truly unreachable URLs that occasionally come back (e.g., self-hosted relay during planned maintenance) should retry forever, which is the intended behavior.
5. **`SyncEngine::remove_peer` is new surface area.** Closing the outbound channel and tearing down the per-peer task is straightforward but needs a test. The existing `Disconnected` plumbing handles the rest.
6. **WASM single-thread.** All new types are `?Send`-compatible (use `Rc<RefCell<…>>` for supervisor state, `tokio::sync::mpsc` for commands, `wasmtimer` for time). No new `Send + Sync` bounds added to data-plane types.

## Out of scope

- **Replacing `Liveness` (sunset-core).** That tracks application-level peer presence ("which peers are publishing in this filter recently"). The new heartbeat tracks transport-level channel liveness ("is this socket actually working"). They are different concerns; the heartbeat does not feed into `Liveness`.
- **Ping/Pong on the unreliable channel.** Reliable-only. Unreliable is best-effort and would need a different timeout structure; if a future need arises (e.g., per-RTC datachannel liveness), it gets its own design.
- **Adaptive backoff** based on observed network conditions. Constants tunable on `BackoffPolicy`, that's all.
- **Multi-relay failover.** Out of scope; landed when the architecture grows beyond single-relay v1.
- **Connection migration** (resuming an old session on a new TCP connection). The protocol is stateless above Noise; redial just runs a fresh handshake. No session resumption.
- **Liveness driving UI heartbeat indicator.** The membership tracker already shows peers as present/absent based on store presence entries (TTL-based). Channel-level heartbeat is orthogonal: a peer can be present in the room but the supervisor's connection to a relay can be in `Backoff`. Whether the UI surfaces both is a future UX question.

## Review summary

- **Placeholders:** none.
- **Internal consistency:** heartbeat lives in `peer.rs` and emits standard `InboundEvent::Disconnected`; supervisor consumes the resulting `EngineEvent::PeerRemoved` via the existing public API. No new event channels. The first-dial vs. transient-error distinction is consistent across `add()`, the failure-mode table, and the test list.
- **Scope:** two focused modules (`peer.rs` extension, new `supervisor.rs`) plus a thin rewire of two `Client` methods. Everything else is unchanged. Suitable for a single implementation plan.
- **Ambiguity:** "first-dial vs transient" semantics for `add()` is the obvious place for misinterpretation; called out explicitly in the API doc, the failure-mode table, and the test list. The intentional distinction between channel-level heartbeat (this design) and application-level `Liveness` (sunset-core) is called out in "Out of scope."
