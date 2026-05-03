# Relays list and metrics

## Problem

The web UI's channels rail has a "Bridges" section seeded from
`fixture.gleam` containing a single hard-coded `minecraft-bridge`
entry. It is purely cosmetic — nothing in the runtime writes to it,
nothing reads from it, and there is no Minecraft bridge implementation
anywhere in the workspace.

What the UI is missing is a real view onto **the relays the client is
actually connected to**. Today the only relay-related signal exposed
to Gleam is `client.relay_status` — a single client-wide string of
"disconnected" / "connecting" / "connected" / "error", aggregated
across all relays. There is no way to see:

- Which specific relays the client is dialing.
- Per-relay connection state (connecting / connected / backoff /
  cancelled) and retry-attempt count.
- Per-relay liveness: the time of the last `Pong` we received and the
  round-trip time of that probe.
- The relay's identity (peer_id) once the Noise handshake completes.

This information already exists internally: the supervisor exposes
`peer_connection_snapshot()` and `on_peer_connection_state(handler)`
returning `IntentSnapshot { addr, state, peer_id?, attempt }`, and
the per-peer liveness loop in `crates/sunset-sync/src/peer.rs` already
runs Ping/Pong on `heartbeat_interval` (15 s) and tracks `last_pong_at`
locally to enforce `heartbeat_timeout`. It just isn't surfaced.

## Design

Three layers change. The wire format does not.

### 1. sunset-sync: expose liveness in `IntentSnapshot`

The per-peer liveness task in `peer.rs` currently keeps `last_pong_at`
as a private `Instant` used only to fire `Disconnected { reason:
"heartbeat timeout" }`. We extend it to also:

- Stamp `last_ping_sent_at: Instant` each time it sends a `Ping`.
  Because the loop sleeps `heartbeat_interval` between sends and we
  have at most one ping in flight, a single field suffices (no per-
  nonce table).
- On every `Pong` arrival (received via `pong_rx.recv()`), compute
  `rtt = now - last_ping_sent_at` and emit a new engine event:

  ```rust
  EngineEvent::PongObserved {
      peer_id: PeerId,
      rtt_ms: u64,
      observed_at_unix_ms: u64,
  }
  ```

  `observed_at_unix_ms` is sourced from `web_time::SystemTime` (works
  on both wasm and native; same crate the rest of sync already uses
  for `next_attempt_at`). No `conn_id` field — the supervisor keys
  `peer_to_addr` on `PeerId` only, and a stale PongObserved that
  races with a reconnect just hits the unknown-peer guard and is
  dropped (see below).

The supervisor's `run` loop already consumes `EngineEvent`s for
`PeerAdded` / `PeerRemoved`. It gains a third arm:

```rust
EngineEvent::PongObserved { peer_id, rtt_ms, observed_at_unix_ms } => {
    if let Some(addr) = state.peer_to_addr.get(&peer_id).cloned() {
        if let Some(entry) = state.intents.get_mut(&addr) {
            entry.last_pong_at_unix_ms = Some(observed_at_unix_ms);
            entry.last_rtt_ms = Some(rtt_ms);
            Self::broadcast(&mut state, &addr);
        }
    }
}
```

`IntentEntry` and `IntentSnapshot` gain two `Option<u64>` fields:

```rust
pub struct IntentSnapshot {
    pub addr: PeerAddr,
    pub state: IntentState,
    pub peer_id: Option<PeerId>,
    pub attempt: u32,
    pub last_pong_at_unix_ms: Option<u64>,  // None until first Pong
    pub last_rtt_ms: Option<u64>,           // None until first Pong
}
```

**Disconnect semantics:** when an intent transitions to `Backoff`
(disconnected, will redial), the existing code path resets `attempt`
and clears `peer_id`. We do *not* clear the liveness fields on
disconnect — the popover should show "heard from 12s ago" while
reconnecting, not snap to "never". They are cleared only when the
intent itself is removed (`SupervisorCommand::Remove`).

**Chattiness:** PongObserved fires once per heartbeat per connected
peer (default 15 s). With 1–2 relays that is a few events/min —
broadcast to all `IntentSnapshot` subscribers without debouncing. If
the direct-peer count grows we can debounce in the supervisor later.

**Ping/Pong wire format:** unchanged. The nonce continues to be
informational only — we measure RTT against the most recent send time,
which is correct as long as exactly one ping is in flight. The
heartbeat-timeout sender invariant guarantees that.

### 2. sunset-web-wasm: enrich `intent_snapshot_to_js`

The existing `intent_snapshot_to_js` helper in
`crates/sunset-web-wasm/src/client.rs` produces
`{ addr, state, attempt, peer_id? }`. It gains two optional fields:
`last_pong_at_unix_ms` and `last_rtt_ms`, set when `Some` and omitted
when `None`. Both `peer_connection_snapshot()` and
`on_peer_connection_state(handler)` automatically pick up the new
fields — no FFI signature changes.

Add no new FFI surface. Gleam reads the existing JS objects.

### 3. Gleam: domain types, view, popover

#### Domain (`web/src/sunset_web/domain.gleam`)

Add:

```gleam
pub type RelayConnState {
  RelayConnecting
  RelayConnected
  RelayBackoff
  RelayCancelled
}

pub type Relay {
  Relay(
    addr: String,           // raw addr URL, identity for diffing
    host: String,           // parsed display label, e.g. "relay.sunset.chat"
    state: RelayConnState,
    attempt: Int,
    peer_id_short: Option(String),         // 4 + 4 hex once handshake done
    last_pong_at_ms: Option(Int),          // wall-clock ms; None until first Pong
    last_rtt_ms: Option(Int),              // None until first Pong
  )
}
```

Remove `Bridge`, `BridgeKind`, `Minecraft`, `BridgeOpt`, `HasBridge`,
`NoBridge` and the `bridge:` field from `Member` and from `Channel`.
All call sites are in `fixture.gleam`, `views/channels.gleam`, and
`views/main_panel.gleam`; none touch real runtime data. The
`minecraft-bridge` channel is removed from fixtures.

#### View (`web/src/sunset_web/views/relays.gleam`, new)

Two render entry points:

- `rail_section(palette, relays, on_open) -> Element(msg)` — collapsed
  row list rendered inside the channels rail under the heading
  "Relays". One row per relay: small status dot (green/amber/grey),
  hostname, click-to-open. Hidden entirely when `relays` is empty.
- `popover(palette, relay, now_ms, placement, on_close) -> Element(msg)`
  — content panel. `placement: relays.Placement = Floating | InSheet`
  mirrors `peer_status_popover.gleam` so we can put it in
  `bottom_sheet.view` on phone and a fixed-position card on desktop.

Popover body, top-to-bottom:
- Header row: hostname + close button.
- Status row with colored pill: "Connected" / "Connecting" / "Backoff
  (attempt N)" / "Cancelled".
- "Heard from Xs ago" — `humanize_age(now_ms, last_pong_at_ms)`, same
  shape as `peer_status_popover.humanize_age` (extract a shared helper
  later if a third caller appears; do not pre-extract).
- "RTT 42 ms" — `case last_rtt_ms { Some(n) -> "RTT " <> n <> " ms";
  None -> "RTT —" }`.
- Full addr URL in monospace (selectable), so the user can copy/share
  it.
- Short peer_id below ("a1b2c3d4…e5f6a7b8") when `peer_id_short` is
  `Some`, otherwise omitted.

#### Wiring (`web/src/sunset_web.gleam`)

- Model gains `relays: List(Relay)` and
  `relays_popover: Option(String)` (the addr currently open).
- The supervisor is constructed at `new Client(seed)` time, before
  any `add_relay` call. The Gleam client-init effect already gets a
  `ClientHandle`; immediately after `createClient` it calls a new
  `subscribePeerConnections(client, callback)` FFI shim that wires
  `client.on_peer_connection_state(handler)` and dispatches each
  snapshot as `PeerConnectionStateUpdated(snapshot)`. A companion
  `peerConnectionSnapshot(client, callback)` shim seeds the initial
  list — both are independent of whether `add_relay` has been called
  yet (snapshots simply start empty).
- The handler maps the JS object to `Relay`, filtering out non-relay
  schemes (`webrtc://...` is a direct peer, not a relay) by
  inspecting the scheme prefix of `addr`.
- Update is a single replace-by-addr: snapshots are full state per
  intent, so we upsert by `addr` and keep list order stable
  (insertion order).
- `OpenRelayPopover(addr)` / `CloseRelayPopover` messages mirror the
  existing peer-status popover messages.
- The existing now_ms ticker (set up for `peer_status_popover`) drives
  the "heard from" age — no new ticker.

#### Channels rail integration

In `views/channels.gleam`, replace the `bridge_channels` filter and
its `section(p, "Bridges", ...)` block with a call to
`relays.rail_section(p, model.relays, on_open_relay_popover)`, passed
in via the `view`'s argument list (mirroring the popover wiring).

#### Mobile bottom sheet

The desktop floating popover anchors top:120px right:260px (same
constants as `peer_status_popover` Floating). On phone the popover is
rendered inside `bottom_sheet.view`, exactly like
`peer_status_sheet_el` in `sunset_web.gleam`, so it slides up from
the bottom and gets the swipe-to-dismiss handle for free. The shell
already mounts the bottom-sheet host; we only add a new branch in
the same overlay-mount block.

`data-testid` hooks for e2e:
- `relays-section` on the rail container (only present when non-empty).
- `relay-row` on each row, with `data-relay-host="…"`.
- `relay-popover` on the popover container.
- `relay-popover-close` on the close button.
- `relay-popover-status`, `relay-popover-heard-from`, `relay-popover-rtt`,
  `relay-popover-addr`, `relay-popover-peer-id` on the corresponding rows.

### 4. Fixture cleanup

`fixture.gleam` loses every `Bridge(_)`, `HasBridge(_)`, `NoBridge`,
and `Minecraft` reference; the `minecraft-bridge` channel and the
synthesized minecraft-bridged member rows are removed. `bridge:` is
removed from the fixture `Member` and `Channel` constructors. The
`bridge_tag` rendering in `main_panel.gleam` and the
`bridge_channel_row` in `channels.gleam` are deleted along with their
`Bridge` / `Minecraft` imports.

This is required, not optional: removing the types from `domain.gleam`
breaks compilation if the call sites stay.

## Tests

### Rust unit tests

`crates/sunset-sync/src/supervisor.rs`:

- `pong_observed_updates_intent_snapshot`: drive a fake
  `EngineEvent::PongObserved` for a connected peer; assert the
  next `snapshot()` shows `last_rtt_ms = Some(_)` and
  `last_pong_at_unix_ms = Some(_)`.
- `pong_observed_for_unknown_peer_is_dropped`: same, but the peer
  is not in `peer_to_addr` — assert no panic, no spurious broadcast.
- `disconnect_preserves_last_pong`: connected peer accumulates
  liveness; on disconnect the IntentEntry transitions to `Backoff`
  but `last_pong_at_unix_ms` / `last_rtt_ms` remain `Some(_)`.

`crates/sunset-sync/src/peer.rs`:

- `liveness_emits_pong_observed_with_rtt`: drive the per-peer task
  with a fake transport that returns a `Pong` immediately; assert a
  `PongObserved` event is emitted on the inbound channel with
  `rtt_ms` ≥ 0.

### Gleam unit tests

A `relays_test.gleam` covers the pure functions:

- `parse_host` extracts hostname from `wss://…`, `ws://…`,
  `wss://host:port/…?query#frag`. Returns `addr` unchanged for
  unparseable strings (defensive — never crash the rail).
- `is_relay_addr` returns `True` for `wss://` and `ws://`,
  `False` for `webrtc://` and other schemes.
- `format_status` maps `RelayConnState` → user-visible label,
  including `"Backoff (attempt 3)"` for non-zero attempt.

### Playwright e2e (`web/e2e/relays.spec.js`, new)

Pattern follows `web/e2e/peer_status_popover.spec.js`: spawn a real
`sunset-relay` in `beforeAll`, target one browser at it via
`?relay=...`. The page boots, the supervisor connects, the supervisor
broadcasts an `IntentSnapshot` with `state = Connected`. The test:

1. **Desktop renders the row.** Assert `[data-testid="relays-section"]`
   visible. Assert one `[data-testid="relay-row"]` exists with
   `data-relay-host="127.0.0.1:<port>"`.
2. **Status reaches connected.** Poll `[data-relay-state="connected"]`
   on the row within 10 s.
3. **Click opens the popover.** Click the row, assert
   `[data-testid="relay-popover"]` visible, header text equals the
   hostname.
4. **Popover shows live liveness.** Within ~20 s (heartbeat is 15 s)
   the `relay-popover-heard-from` row matches `/^heard from \d+s ago$/`
   and `relay-popover-rtt` matches `/^RTT \d+ ms$/`. The full addr is
   rendered in the `relay-popover-addr` row.
5. **Close button works.** Click `relay-popover-close`; assert the
   popover element is gone.
6. **Phone bottom sheet.** Re-run the same flow with
   `viewport: { width: 390, height: 844 }`. Assert the popover is
   inside the existing bottom-sheet host (assert visibility +
   `data-testid="bottom-sheet"` ancestor).

To keep the heartbeat-window assertion fast, the test passes
`?heartbeat_interval_ms=2000` so the first Pong arrives within ~2 s
rather than the default 15 s.

Plumbing: a new optional URL param `heartbeat_interval_ms` is parsed
in `sunset.ffi.mjs`'s `presenceParamsFromUrl` neighbor (or its own
helper) and passed as a second argument to `new Client(seed,
heartbeatIntervalMs?)`. The wasm `Client::new` accepts an optional
`u64`; when `Some(n)`, it sets `SyncConfig::heartbeat_interval =
Duration::from_millis(n)` (and proportionally
`heartbeat_timeout = 3 × interval`, matching the default ratio).
When `None` it uses defaults. No production code path changes — the
URL param defaults to absent and is e2e-only.

The "heard from" regex must also accept the "just now" case (age
< 1 s, per `humanize_age`): `/^heard from (just now|\d+s ago)$/`.

## Out of scope

- Removing or replacing the `Channel` kind discriminant. Voice and
  text channels still use it; only `Bridge(_)` is removed.
- Per-relay enable/disable UI. Adding/removing relays at runtime is
  reserved for a future spec; this one only displays what's there.
- Server-reported relay metadata (operator, location, build version).
  Future relays may publish a small KV entry describing themselves;
  that's separate work.
- Direct-peer connection detail. Direct WebRTC peers also flow through
  the supervisor and would benefit from the same metrics, but a
  per-peer "connections" UI is a different surface — covered by the
  existing peer-status popover today.
