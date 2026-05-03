# Durable relay connect — supervisor-owned, peer-status-shaped

## Why

The Lustre frontend currently treats the relay as a one-shot `Result`-bearing
call: `Client::add_relay(url) -> Result<(), JsError>`, expected to succeed
once and stay succeeded forever. PR #18 patched the most-visible failure mode
(retry on `RelayConnectResult(Error)` in the gleam `update`), but that fix
sits at the wrong layer — every future client (TUI, mod, native CLI) would
have to reimplement the same retry loop in its own host language.

Two architectural problems remain:

1. **Retry logic isn't shared.** Resolver-fetch failures (the production
   "503 with no CORS during deploy" case) and supervisor first-dial failures
   are recovered in the gleam frontend, not in `sunset-sync`. Other host
   surfaces would have to repeat this work, and divergence is inevitable.
2. **Relay status is special.** `Client` carries a `relay_status: String`
   field with sticky `"connecting"`/`"error"` semantics, and the membership
   tracker has a parallel `derive_relay_status` that special-cases those
   strings. The relay isn't conceptually different from any other peer the
   host wants to stay connected to — it's just a peer whose identity
   requires an HTTP fetch to discover.

This design pushes recovery into the supervisor and folds relay status
into the existing peer-connection-status surface. Hosts call "stay
connected to this thing" once and observe state through the same channel
they'd use for any other peer.

## North star

```
Frontend (gleam / future TUI / future mod):
    add_relay(url)  →  IntentId
    on_intent_changed(callback)
                    ↑
                    │ IntentSnapshot stream
                    │
sunset-sync / PeerSupervisor:
    add(Connectable) → IntentId               // returns immediately
    subscribe_intents() → mpsc Stream<IntentSnapshot>
    each dial attempt: resolve (if needed) → engine.add_peer(addr)
    backoff is internal; failures never bubble out as a permanent error
```

## Component changes

### `sunset-sync`

#### New: `Connectable`

```rust
pub enum Connectable {
    /// Already-canonical address with `#x25519=<hex>`. No pre-dial work.
    Direct(PeerAddr),
    /// User-typed input (`relay.sunset.chat`, `wss://host:port`, …) that
    /// requires `HTTP GET /` to learn the relay's x25519 key on every
    /// dial attempt. Re-resolving each attempt covers the case where
    /// the relay rotates identity between deploys.
    Resolving {
        input: String,
        fetch: Rc<dyn HttpFetch>,
    },
}
```

`HttpFetch` lives in `sunset-relay-resolver`. `sunset-sync` gains a
dependency on that crate (small, trait-only).

#### Supervisor: durable, no first-dial special case

```rust
pub type IntentId = u64;  // monotonic, allocated by supervisor

impl<S, T> PeerSupervisor<S, T> {
    /// Register a durable intent. Returns immediately. The only Err is
    /// for input that can't be parsed (malformed URL form). Transient
    /// failures (resolver fetch, dial, Hello) never surface — the
    /// supervisor retries with the existing `BackoffPolicy`.
    pub async fn add(&self, c: Connectable) -> Result<IntentId, ParseError>;

    /// Cancel an intent. Tears down the connection if connected.
    pub async fn remove(&self, id: IntentId);

    /// Snapshot every intent's current state.
    pub async fn snapshot(&self) -> Vec<IntentSnapshot>;

    /// Subscribe to per-intent state changes. The receiver is fed the
    /// current snapshot of every intent on subscribe (so late
    /// subscribers don't miss state) and every change after that.
    pub fn subscribe_intents(&self) -> mpsc::UnboundedReceiver<IntentSnapshot>;
}
```

The "remove the intent on first-dial failure" branch in
`handle_command::Add` is deleted. First-dial failures transition the
intent to `Backoff` like any other failure.

**Deduplication.** `add()` returns the existing `IntentId` when called
with a `Connectable` that matches one already registered:
`Direct(addr)` matches by `PeerAddr`; `Resolving { input, .. }`
matches by input string. (Two inputs that resolve to the same canonical
address are NOT deduplicated — too cute, and the implementation would
have to wait on resolution before answering `add`.) This preserves the
existing `idempotent_add` semantics under the new id-keyed model.

#### `IntentSnapshot` (extended)

```rust
pub struct IntentSnapshot {
    pub id: IntentId,
    pub state: IntentState,            // Connecting | Connected | Backoff | Cancelled
    pub peer_id: Option<PeerId>,       // filled on first connect; sticky
    pub kind: Option<TransportKind>,   // filled on first connect
    pub attempt: u32,
    pub label: String,                 // user-input for Resolving;
                                       // canonical URL minus `#x25519=…`
                                       // for Direct. UI-displayable
                                       // before peer_id is known.
}
```

#### Dial loop (per attempt)

```
match connectable {
    Direct(addr)              => engine.add_peer(addr).await
    Resolving { input, fetch } => {
        let r = Resolver::new(fetch.clone());
        let canonical = r.resolve(&input).await?;       // ParseError → permanent;
                                                        // HTTP error → backoff
        let addr = PeerAddr::new(Bytes::from(canonical));
        engine.add_peer(addr).await
    }
}
```

`ParseError` from `parse_input` is the only thing that aborts the intent
permanently — typed garbage. Everything else (HTTP 503, connection
refused, mismatched JSON, dial timeout) is transient and gets the
existing exponential backoff.

### `sunset-web-wasm`

#### `Client`

```rust
impl Client {
    /// Register a durable intent to keep connected to `url`.
    /// Returns the intent id once the supervisor has acknowledged the
    /// registration (one cmd-channel round-trip; does *not* wait for
    /// the first connection). The only `Err` is for malformed input.
    pub async fn add_relay(&self, url: String) -> Result<IntentId, JsError>;

    /// Cancel an intent.
    pub async fn remove_relay(&self, id: IntentId);

    /// Synchronous current snapshot.
    pub fn intents(&self) -> Vec<IntentSnapshot>;

    /// Subscribe to per-intent state changes.
    /// Callback fires for every existing intent on register + every
    /// change after that.
    pub fn on_intent_changed(&self, cb: js_sys::Function);
}
```

Removed:
- `Client::relay_status: Rc<RefCell<String>>` field.
- `Client::relay_status()` getter.
- `*self.relay_status.borrow_mut() = "connecting"|"connected"|"error"`
  assignments inside `add_relay`.
- The implicit "connecting" → "connected" → "error" string state
  machine.

`add_relay` no longer awaits the first connection — it returns as
soon as the supervisor's command pump has registered the intent (one
cmd-channel round-trip). The supervisor's internal task drives every
dial attempt thereafter.

### `sunset-relay-resolver`

No API changes. The resolver gets called per-attempt by the supervisor
instead of one-shot by `Client::add_relay`. The existing `HttpFetch`
trait + `WebSysFetch` adapter work unchanged.

### `sunset-core::membership`

The relay-status reporting in the membership tracker is **retired
entirely** — not just simplified — because `IntentSnapshot` covers it
in one place. Removed:

- `derive_relay_status`, `maybe_fire_relay_status`, `fire_relay_status_now`.
- `TrackerHandles::on_relay_status` callback slot and
  `TrackerHandles::last_relay_status: Rc<RefCell<String>>`.
- The sticky `"connecting"` / `"error"` strings ferried through
  `last_relay_status`.

Justification: every state the membership tracker reported about the
relay (`connected` / `disconnected` / sticky `connecting` / sticky
`error`) is observable from the supervisor's intent snapshots
(`IntentState::Connecting | Connected | Backoff | Cancelled`). The
membership tracker keeps `peer_kinds` and the per-member callbacks
because those still cover **inbound** peers and per-member transport
kind, neither of which the supervisor models.

`Client::on_relay_status_changed` is removed alongside.

### Gleam frontend

`web/src/sunset_web.gleam`:

- Drop `RelayConnectResult(url, Result)` Msg variant and its Ok/Error
  handlers. `add_relay` no longer dispatches a result.
- Drop the `delay_ms` retry effect added in PR #18.
- Drop the `delay_ms` FFI from `sunset.ffi.mjs` and `sunset.gleam`.
- Drop the `RelayStatusUpdated(String)` Msg variant, the
  `on_relay_status_changed` registration in `ClientReady`, and the
  `relay_status_to_conn` string-pattern-matching helper.
- New `IntentChanged(snapshot)` Msg fed from `on_intent_changed`.
- Model field `intents: Dict(IntentId, IntentSnapshot)` replaces
  `relay_status: String`. Add `published: Bool` latch.
- Derive UI status:
  ```gleam
  fn relay_status_pill(intents: Dict(IntentId, IntentSnapshot))
      -> domain.ConnStatus
  {
      let snaps = dict.values(intents)
      case list.any(snaps, fn(s) { s.state == Connected }) {
          True -> domain.Connected
          False -> case list.any(snaps, fn(s) {
              s.state == Connecting || s.state == Backoff
          }) {
              True -> domain.Reconnecting
              False -> domain.Offline
          }
      }
  }
  ```
- `publish_room_subscription` fires once on first transition to
  "any intent Connected" (model holds a `published: Bool` latch).

## Status indexing — pre-connection identity

For `Resolving` intents, the relay's `PeerId` isn't known until the
first successful resolve. Until then, the UI keys status off `IntentId`
and displays the `label` field (the user's input string). Once
connected, `peer_id` and `kind` populate; subsequent retries reuse
the same `IntentId` so the UI row stays put across reconnect cycles.

The gap this leaves: a member-list row keyed by `PeerId` (which is the
existing `peer_kinds` model) can't show "trying to connect" for an
unresolved intent. That's fine — relays aren't members. The room's
top-level connection pill / dot reads from intents directly.

## Inbound peers — the membership-tracker boundary

`PeerSupervisor` only models **outbound** intents (things the host
asked to connect to). Inbound peers — connections that another host
dialed *to* us — aren't supervisor-managed. `peer_kinds` remains the
source of truth for those, fed by engine `PeerAdded`/`PeerRemoved`
events as today.

For the web client, this distinction doesn't matter — web doesn't
accept inbound. For the relay (server) and TUI (peer-to-peer), both
sources coexist. The frontend, when it eventually unifies them, can
merge both views; this spec doesn't require that.

## Testing

### `sunset-sync` unit tests

- **Rewrite** `first_dial_failure_returns_err_and_clears_intent` →
  `first_dial_failure_enters_backoff_and_retries`. Uses
  `tokio::time::pause` so the backoff doesn't slow the suite.
- **New** `resolving_intent_re_resolves_each_attempt`:
  `Connectable::Resolving` with a `FakeFetch` (in-test impl of
  `HttpFetch`) that fails twice then succeeds; assert resolver was
  called 3×, intent ends in `Connected`.
- **New** `subscribe_intents_replays_current_state`: register two
  intents, one `Connected`, one `Backoff`. A late subscriber receives
  both current snapshots before any new changes.
- **New** `add_returns_immediately_on_resolving`: `add()` returns
  inside one tick even when the first dial would take seconds.
- **Existing** `idempotent_add` updated to deduplicate by intent
  identity (still de-duplicates same-input adds).
- **Existing** `remove_cancels_intent` updated to take `IntentId`.

### `sunset-web-wasm`

- Existing tests stay; `add_relay` returning `IntentId` instead of
  awaiting first connect is a small caller-side change.
- New unit test: `client_intents_snapshot_reflects_supervisor_state`.

### Gleam unit tests

- **New** `intent_changed_drives_relay_status_pill`: feed simulated
  `IntentSnapshot` updates through the `update` function; assert the
  pill state derives correctly across Connecting → Connected →
  Backoff → Connected.
- **New** `publish_room_subscription_fires_once_on_first_connected`:
  multiple `Connected` snapshots emit only one `publish_subscription`.

### e2e tests

- **`relay_deploy.spec.js`** stays; the 15 s wait can shrink (no more
  gleam-side 2 s retry cadence, supervisor's first dial fires
  immediately when the port comes back). Still asserts the same
  observable: page loaded during outage → relay restored → both
  clients converge to `Connected` and chat works.
- **`relay_restart.spec.js`** unchanged; behavior is the same from the
  user's perspective.

## Migration plan (sketch — implementation plan will detail order)

1. Add `Connectable` + extended `IntentSnapshot` to `sunset-sync`.
2. Add `subscribe_intents()` channel to supervisor; emit current
   state on subscribe.
3. Make supervisor durable on first-dial failure (rewrite the
   relevant tests).
4. Wire resolver into the dial loop for `Connectable::Resolving`.
5. Update `Client::add_relay` to use the new API; expose
   `on_intent_changed` to JS.
6. Drop `Client::relay_status` field + sticky-status branch in
   `derive_relay_status`.
7. Update gleam frontend: replace `RelayConnectResult` with
   `IntentChanged`; drop `delay_ms` FFI and retry effect; derive UI
   status from intent dict.
8. Verify `relay_deploy.spec.js` and `relay_restart.spec.js` both
   green; tighten the deploy-test waits.

## Out of scope

- Unifying `peer_kinds` and supervisor state into a single status
  surface for **all** peers (inbound + outbound). The supervisor
  covers outbound only; that's enough to retire `relay_status` for
  the web client. A unified model is worth doing later but isn't
  needed for this fix.
- A "permanently bad URL" UI affordance. With infinite retry the user
  sees `Reconnecting` indefinitely for a typo'd hostname. The current
  UX (DNS-resolution retry forever, no manual cancel) is acceptable
  for a v1; an explicit "give up after N attempts" knob can land
  later if it turns out users need it.
- Cancelable resolver fetches. If the user `remove`s an intent while
  a resolver fetch is in-flight, the fetch completes and is
  discarded. Acceptable for v1.
- Reverting PR #18's gleam-side retry. That's done as part of step 7
  in the migration above, not as a separate cleanup.
