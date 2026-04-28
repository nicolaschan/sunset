# UI presence + peer-status — Design

**Goal:** wire the chat UI so the relay-status badge, the per-peer connection-mode indicator, and the member rail reflect real sunset-sync state instead of one-shot strings + fixture data.

**Non-goals:** voice presence (Speaking/MutedP), bridge status, real per-message receipts, friendly nicknames, federation-only RelayStatus variants (TwoHop, ViaPeer, BridgeRelay, SelfRelay). All deferred.

---

## Background

After V1, the Client exposes:

- `relay_status() -> String` — written *once* at `add_relay` time. Doesn't react to disconnects.
- `peer_connection_mode(pk) -> String` — only marks a peer "direct" when *we* called `connect_direct`. The accept side stays "via_relay" forever.
- `EngineState.peer_outbound: HashMap<PeerId, _>` — authoritative live-peers set. Not exposed.

The Gleam UI's member rail, presence pills, and per-message routing badges all render `fixture.members()` — pure mock data with no relationship to the engine.

---

## Architecture

```
sunset-sync                  sunset-web-wasm Client                  Gleam UI
─────────                    ─────────────────────                   ────────
EngineEvent stream     ───→  peer_kinds: Map<Pk, Kind>          ───→ on_relay_status_changed
  PeerAdded {pk, kind}       relay_status (derived)                  on_members_changed
  PeerRemoved {pk}                                                   (Lustre Msg → Model)
                                  ▲
                                  │ also drives per-peer mode
TransportConnection::kind   ──────┘
  Primary | Secondary | Unknown
  (MultiConnection maps variant)

Local presence dispatcher    ───→ presence_map: Map<Pk, last_ms>
  subscribe <fp>/presence/        Online <interval, Away interval-ttl,
                                  Offline >ttl/missing

Heartbeat publisher
  every interval: store.insert
  <fp>/presence/<my_pk>
  TTL = ttl_ms
```

---

## Components

### sunset-sync additions

**`TransportKind` (new):**
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    /// Primary half of a MultiTransport — for our v1 wiring this is
    /// the relay-mediated WebSocket path.
    Primary,
    /// Secondary half of a MultiTransport — for our v1 wiring this is
    /// the direct WebRTC datachannel.
    Secondary,
    /// Used by transports that don't participate in MultiTransport
    /// (e.g. the test transport, future single-transport setups).
    Unknown,
}
```

`TransportConnection` gets a default-impl method:
```rust
fn kind(&self) -> TransportKind { TransportKind::Unknown }
```

`MultiConnection<C1, C2>` overrides:
```rust
fn kind(&self) -> TransportKind {
    match self {
        MultiConnection::Primary(_) => TransportKind::Primary,
        MultiConnection::Secondary(_) => TransportKind::Secondary,
    }
}
```

`NoiseConnection<C>` delegates to its inner: `self.raw.kind()`. So a `NoiseConnection<MultiConnection<...>>` reports the right kind.

**`EngineEvent` (new):**
```rust
#[derive(Clone, Debug)]
pub enum EngineEvent {
    PeerAdded { peer_id: PeerId, kind: TransportKind },
    PeerRemoved { peer_id: PeerId },
}
```

**`SyncEngine::subscribe_engine_events()`:** returns a fresh `mpsc::UnboundedReceiver<EngineEvent>`. The engine maintains `Vec<UnboundedSender<EngineEvent>>` in `EngineState`, fans out each event to every live subscriber, and drops dead senders lazily on the next emission.

`handle_inbound_event`:
- `PeerHello { peer_id, .. }` → after registering `peer_outbound`, call `conn.kind()` (need to thread `kind` through; see below) and emit `EngineEvent::PeerAdded { peer_id, kind }`.
- `Disconnected { peer_id, .. }` → emit `PeerRemoved { peer_id }` after removing from `peer_outbound`.

**Plumbing the kind:** `run_peer` already holds `Rc<C>` for the connection. Capture `conn.kind()` once at startup and include it in `InboundEvent::PeerHello { peer_id, kind, out_tx }`. Engine forwards to `EngineEvent`.

### sunset-web-wasm additions

**`Client::start_presence(interval_ms: u32, ttl_ms: u32, refresh_ms: u32)`** (new):
- Idempotent: ignores second call.
- Spawns a heartbeat publisher task that ticks every `interval_ms`. Each tick inserts a signed entry named `<room_fp_hex>/presence/<my_pk_hex>` with `priority = now_ms`, `expires_at = now_ms + ttl_ms`, content block = empty (`ContentBlock { data: Bytes::new(), references: vec![] }`).
- Spawns a `MembershipTracker` task that:
  1. Subscribes to the local store with filter `Filter::NamePrefix(<fp>/presence/)` (Replay::All), updates `presence_map[pk] = priority_ms` on each Inserted/Replaced.
  2. Subscribes to `engine.subscribe_engine_events()`, updates `peer_kinds[pk]` on PeerAdded, removes on PeerRemoved.
  3. Runs a `refresh_ms` periodic ticker that recomputes the derived state and fires `on_members_changed` if the JSON-shape of the member list changed since last fire.

**`Client::on_members_changed(callback: js_sys::Function)`** (new): registers a single callback. Fires whenever the derived member list's shape changes. Argument: `Array<MemberJs>`.

**`Client::on_relay_status_changed(callback: js_sys::Function)`** (new): registers a single callback. Fires when relay status string changes. Argument: `String`.

**`MemberJs`** (new wasm-bindgen exported struct):
```rust
#[wasm_bindgen]
pub struct MemberJs {
    pub(crate) pubkey: Vec<u8>,
    pub(crate) presence: String,        // "online" | "away" | "offline"
    pub(crate) connection_mode: String, // "direct" | "via_relay" | "self" | "unknown"
    pub(crate) is_self: bool,
}
```
Field accessors via `#[wasm_bindgen(getter)]` (mirrors `IncomingMessage`).

**Derived state rules:**
- Member list = self ∪ {pk : pk ∈ presence_map and pk ≠ self and bucket(age) ≠ dropped}.
- For each member:
  - `presence = bucket(now_ms - presence_map[pk])`:
    - age < interval_ms → `online`
    - interval_ms ≤ age < ttl_ms → `away`
    - age ≥ ttl_ms → drop from list (also evict from presence_map)
  - The `presence` field on `MemberJs` therefore only ever takes the values `"online"` or `"away"` for non-self members. `"offline"` exists only as the absence of the member from the list (and is the value used for the self entry only if the heartbeat task hasn't started yet — practically never observed).
  - `is_self = (pk == self)`
  - `connection_mode`:
    - if `is_self` → `self`
    - else if `peer_kinds[pk] == Secondary` → `direct`
    - else if `peer_kinds[pk] == Primary` → `via_relay`
    - else → `unknown`
- Self always present, always `online`, always `connection_mode = self`.

**Relay-status derivation:**
- `connecting`: `add_relay` is in flight (set by the existing call site).
- `connected`: at least one `peer_kinds[pk] == Primary` exists.
- `error`: `add_relay` returned Err.
- `disconnected`: none of the above (initial state, or all primary peers gone).

The existing `relay_status()` getter stays. The internal write at `add_relay` Ok now ALSO fires the on_relay_status_changed callback.

**Cleanup:**
- `direct_peers: HashSet<PeerId>` field — removed; superseded by `peer_kinds`.
- `peer_connection_mode(pk) -> String` — kept as a public method, reimplemented as a thin lookup against `peer_kinds` (Primary → `via_relay`, Secondary → `direct`, absent → `unknown`). The existing kill_relay test continues to use it; the new member-list path is additive.

### Gleam UI additions

**Externals (in `sunset.gleam` / `sunset.ffi.mjs`):**
```gleam
pub type MemberJs

@external(javascript, "./sunset.ffi.mjs", "startPresence")
pub fn start_presence(
  client: ClientHandle,
  interval_ms: Int, ttl_ms: Int, refresh_ms: Int,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "onMembersChanged")
pub fn on_members_changed(
  client: ClientHandle,
  callback: fn(List(MemberJs)) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "onRelayStatusChanged")
pub fn on_relay_status_changed(
  client: ClientHandle,
  callback: fn(String) -> Nil,
) -> Nil

@external(javascript, "./sunset.ffi.mjs", "memPubkey")
pub fn mem_pubkey(m: MemberJs) -> BitArray

@external(javascript, "./sunset.ffi.mjs", "memPresence")
pub fn mem_presence(m: MemberJs) -> String

@external(javascript, "./sunset.ffi.mjs", "memConnectionMode")
pub fn mem_connection_mode(m: MemberJs) -> String

@external(javascript, "./sunset.ffi.mjs", "memIsSelf")
pub fn mem_is_self(m: MemberJs) -> Bool

@external(javascript, "./sunset.ffi.mjs", "presenceParamsFromUrl")
pub fn presence_params_from_url() -> #(Int, Int, Int)
```

`presenceParamsFromUrl()`: parses `?presence_interval=&presence_ttl=&presence_refresh=` query params, returns the tuple, defaulting to `#(30000, 60000, 5000)` if absent. This is the test-fast hook.

**Model + Msg:**
```gleam
pub type Model {
  Model(
    ...,
    members: List(domain.Member),  // NEW
    ...
  )
}

pub type Msg {
  ...
  MembersUpdated(List(domain.Member))   // NEW
  RelayStatusUpdated(String)            // CHANGED: was implicit, now explicit
  ...
}
```

**Bootstrap wiring:** in `IdentityReady` / `ClientReady` handler, after `add_relay`:
1. Read presence params from URL (`sunset.presence_params_from_url()`).
2. Call `sunset.start_presence(client, interval, ttl, refresh)`.
3. Register `sunset.on_members_changed(client, fn(ms) { dispatch(MembersUpdated(map_members(ms))) })`.
4. Register `sunset.on_relay_status_changed(client, fn(s) { dispatch(RelayStatusUpdated(s)) })`.

**Member mapping (Gleam):**
```gleam
fn map_members(ms: List(MemberJs)) -> List(domain.Member) {
  list.map(ms, fn(m) {
    let pk = sunset.mem_pubkey(m)
    domain.Member(
      id: domain.MemberId(short_pubkey(pk)),
      name: short_pubkey(pk),
      initials: short_initials(pk),
      status: presence_to_status(sunset.mem_presence(m)),
      relay: connection_mode_to_relay(sunset.mem_connection_mode(m)),
      you: sunset.mem_is_self(m),
      in_call: False,
      bridge: domain.NoBridge,
      role: domain.NoRole,
    )
  })
}

fn presence_to_status(s: String) -> domain.Presence {
  case s {
    "online" -> domain.Online
    "away" -> domain.Away
    // The wasm side drops Offline members from the list, so this branch
    // is only reached for transient/unknown values from the FFI surface.
    _ -> domain.OfflineP
  }
}

fn connection_mode_to_relay(s: String) -> domain.RelayStatus {
  case s {
    "direct" -> domain.Direct
    "via_relay" -> domain.OneHop
    "self" -> domain.SelfRelay
    _ -> domain.NoRelay
  }
}
```

**View consumers:** Replace every `fixture.members()` call site (`view.members`, `voice_popover` lookups, `details_panel` mappings) with `model.members`. Where the resulting list is empty (e.g., before presence kicks in), fall through to `fixture.members()` as a placeholder OR show an "alone in the room" empty state. **Decision: show the real empty state** (just the self pill) — pure fixture rendering when real data exists is misleading.

The `voice_popover` reads voice_settings keyed by member name. Member names are now `short_pubkey(pk)` strings. This works as long as `voice_settings` is empty (no voice in this scope).

---

## Data flow walkthrough

### Heartbeat tick (every `interval_ms`)
1. Heartbeat task: `store.insert(<fp>/presence/<my_hex>, priority=now_ms, expires_at=now_ms+ttl_ms, ContentBlock::empty)`.
2. Local store insert → engine's existing `local_sub.next()` event → push to peers via the `room_filter` subscription (already covers `<fp>/presence/`).
3. Remote peer's relay-side per-peer task receives → `handle_event_delivery` → `store.insert` on remote.
4. Remote peer's `MembershipTracker.presence_subscription.next()` fires → `presence_map[pk] = priority_ms` → re-derive member list → fire `on_members_changed` if shape changed.

### Refresh tick (every `refresh_ms`)
1. `MembershipTracker` periodic ticker computes new buckets.
2. If any member transitioned (Online↔Away, Away↔dropped), fire `on_members_changed`.

### Engine event
1. `PeerAdded(pk, kind)` → `peer_kinds[pk] = kind` → fire callbacks (relay if kind==Primary; members if pk in presence_map).
2. `PeerRemoved(pk)` → `peer_kinds.remove(pk)` → re-derive → fire callbacks.

### Relay death (kill_relay scenario)
1. Relay process dies → ws-browser per-peer task hits a recv error → emits `InboundEvent::Disconnected { peer_id: relay_pk }`.
2. Engine emits `EngineEvent::PeerRemoved { peer_id: relay_pk }`.
3. Client's tracker removes `peer_kinds[relay_pk]`. No more Primary connections → relay_status → `disconnected`.
4. `on_relay_status_changed("disconnected")` fires; UI badge flips.

---

## Error handling

| Failure | Behavior |
|---|---|
| Heartbeat insert returns Err | `console.warn`, continue; next tick retries |
| Presence subscription stream ends | log error, restart subscription with backoff |
| Engine event subscription stream ends | log error, restart |
| JS callback throws | `try/catch` on the JS side; log; never kills the worker |
| `start_presence` called twice | second call is a no-op |

---

## Testing

### Rust unit tests (sunset-web-wasm)
- `presence_bucket(age_ms, interval_ms, ttl_ms) -> Presence` — table-driven thresholds.
- `MultiConnection::kind` returns Primary/Secondary correctly.
- `NoiseConnection::kind` delegates correctly.

### Rust unit tests (sunset-sync)
- `EngineEvent` is fanned out to multiple subscribers (subscribe N times, emit 1 event, all receive).
- `subscribe_engine_events` after the engine is running still receives subsequent events.

### wasm-bindgen-test (sunset-web-wasm)
- `MembershipTracker` constructs against a `MemoryStore` + a stub engine event stream; receives a synthetic store event; fires the callback with the right shape.

### Playwright (`web/e2e/presence.spec.js`, NEW)
URL params: `?presence_interval=300&presence_ttl=900&presence_refresh=100`.

Test 1: **two-browser presence membership**
- Open A and B, give them ~1s to publish heartbeats.
- A's `window.sunsetClient` member list contains B with `presence: online`, `connection_mode: via_relay`.
- Same for B.

Test 2: **direct-mode flip**
- A calls `connect_direct(B)`.
- A's member list shows B with `connection_mode: direct` within 1s.

Test 3: **offline transition**
- Close A's tab.
- B sees A transition to `away` within ~600ms, `offline` (or removed) within ~1.2s.

Total wall-clock for the new spec: < 5s.

### Existing tests
- `kill_relay.spec.js` — should continue to pass. The `peer_connection_mode` getter is preserved (now backed by `peer_kinds`).
- `two_browser_chat.spec.js` — unaffected; messages still flow.

---

## Configuration knobs (production defaults)

| Param | Default | Description |
|---|---|---|
| `interval_ms` | 30000 | Heartbeat publish cadence |
| `ttl_ms` | 60000 | Presence entry expiry; also Online→Away threshold = `interval_ms`, Away→Offline threshold = `ttl_ms` |
| `refresh_ms` | 5000 | Local re-evaluation tick (catches threshold crossings between heartbeats) |

URL-param overrides (read by `presenceParamsFromUrl()`):
- `?presence_interval=<ms>`
- `?presence_ttl=<ms>`
- `?presence_refresh=<ms>`

Production never sets these — they exist only for the Playwright suite to compress wall-clock time.

---

## Out-of-scope (explicit, won't do in this plan)

- Voice presence states (Speaking, MutedP) — V3 scope.
- Bridge / federation RelayStatus variants (TwoHop, ViaPeer, BridgeRelay) — federation plan scope.
- Real per-message receipts (`Receipt(name, time, relay)` in the details panel) — separate plan, needs sync-level read receipts.
- Friendly nicknames or avatars — separate plan, needs profile entries.
- Per-member auto-upgrade to direct (V1.5).
- Invisible mode / opt-out of heartbeat publishing — privacy follow-up.
