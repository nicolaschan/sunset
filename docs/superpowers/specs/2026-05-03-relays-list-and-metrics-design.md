# Relays list and metrics

> **Revision 2026-05-03 (post-PR #23):** The original draft predated
> PR #23 ("sunset-sync: durable supervisor on Connectable + IntentId;
> retire relay_status"). This revision rewrites the design against the
> new intent surface — most of the data plumbing is already in place;
> the work shrinks to (a) measuring RTT in the liveness loop and
> surfacing it on `IntentSnapshot`, and (b) building the Gleam UI on
> top of the already-live intent subscription.

## Problem

The web UI's channels rail has a "Bridges" section seeded from
`fixture.gleam` with a single hard-coded `minecraft-bridge` entry. It
is purely cosmetic — nothing in the runtime writes to it, nothing
reads from it, and there is no Minecraft bridge implementation
anywhere in the workspace.

What the UI is missing is a real view onto **the relays the client is
actually connected to**. The supervisor already exposes a per-intent
state stream (`IntentSnapshot` via `on_intent_changed` on the wasm
client; `relay_status_pill` derives a single client-wide
`Connected / Reconnecting / Offline` aggregate from it). What it does
*not* expose is per-intent liveness: time of the last `Pong` and the
round-trip time of the most recent probe. The per-peer liveness loop
in `crates/sunset-sync/src/peer.rs` already runs Ping/Pong every
`heartbeat_interval` (15 s) and tracks `last_pong_at` locally to
enforce `heartbeat_timeout` — it just isn't surfaced.

So this work has two halves:

1. **Surface liveness on `IntentSnapshot`.** New fields, new
   `EngineEvent::PongObserved`, new supervisor arm.
2. **Build the UI.** A "Relays" section in the channels rail driven
   by the existing `intents` dict; click → popover (floating on
   desktop, bottom sheet on phone) with hostname / status / heartbeat
   age / RTT / full URL / short peer id.

## Design

Three layers change. The wire format does not.

### 1. sunset-sync: expose liveness in `IntentSnapshot`

The per-peer liveness task in `peer.rs` currently keeps `last_pong_at`
as a private `Instant` used only to fire `Disconnected { reason:
"heartbeat timeout" }`. We extend it to also:

- Stamp `last_ping_sent_at: Option<Instant>` each time it sends a
  `Ping`. Because the loop sleeps `heartbeat_interval` between sends
  and at most one ping is in flight, a single field suffices (no
  per-nonce table).
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
  for `next_attempt_at`). No `conn_id` field — the supervisor's
  `peer_to_intent` map keys on `PeerId` only, and a stale PongObserved
  that races with a reconnect just hits the unknown-peer guard.

The supervisor's `run` loop already consumes `EngineEvent`s for
`PeerAdded` / `PeerRemoved`. It gains a third arm:

```rust
EngineEvent::PongObserved { peer_id, rtt_ms, observed_at_unix_ms } => {
    let mut state = self.state.borrow_mut();
    let id = match state.peer_to_intent.get(&peer_id) {
        Some(i) => *i,
        None => return,
    };
    if let Some(entry) = state.intents.get_mut(&id) {
        entry.last_pong_at_unix_ms = Some(observed_at_unix_ms);
        entry.last_rtt_ms = Some(rtt_ms);
    }
    Self::broadcast(&mut state, id);
}
```

`IntentEntry` and `IntentSnapshot` gain two `Option<u64>` fields:

```rust
pub struct IntentSnapshot {
    pub id: IntentId,
    pub state: IntentState,
    pub peer_id: Option<PeerId>,
    pub kind: Option<crate::transport::TransportKind>,
    pub attempt: u32,
    pub label: String,
    pub last_pong_at_unix_ms: Option<u64>,  // None until first Pong
    pub last_rtt_ms: Option<u64>,           // None until first Pong
}
```

**Disconnect semantics:** when an intent transitions to `Backoff`
(disconnected, will redial), the existing code path resets `attempt`
and clears `peer_id` (via `PeerRemoved`). We do *not* clear the
liveness fields on disconnect — the popover should show "heard from
12s ago" while reconnecting, not snap to "never". They are cleared
only when the intent itself is removed (`SupervisorCommand::Remove`).

**Chattiness:** PongObserved fires once per heartbeat per connected
peer (default 15 s). With 1–2 relays that is a few events/min —
broadcast to all `IntentSnapshot` subscribers without debouncing. If
the direct-peer count grows we can debounce in the supervisor later.

**Ping/Pong wire format:** unchanged. The nonce continues to be
informational only — we measure RTT against the most recent send time,
which is correct as long as exactly one ping is in flight. The
heartbeat-interval sleep between sends guarantees that. A Pong with
no in-flight ping (peer-initiated probe, replay, post-disconnect
race) falls back to `rtt_ms = 0` — we still surface the heartbeat,
just without a meaningful RTT.

### 2. sunset-web-wasm: enrich `IntentSnapshotJs` and accept heartbeat override

`crates/sunset-web-wasm/src/intent.rs` defines
`IntentSnapshotJs { id, state, label, peer_pubkey, kind, attempt }`.
It gains two optional fields and a corresponding mapping from
`IntentSnapshot`:

```rust
#[wasm_bindgen]
pub struct IntentSnapshotJs {
    // existing fields…
    pub last_pong_at_unix_ms: Option<f64>,
    pub last_rtt_ms: Option<f64>,
}
```

Both `Client::intents()` and `Client::on_intent_changed(...)` already
call `IntentSnapshotJs::from(&snap)`, so they pick up the new fields
without further change.

`Client::new(seed)` becomes `Client::new(seed, heartbeat_interval_ms)`
with `heartbeat_interval_ms: u32` (0 = use the `SyncConfig` default
of 15 s). When non-zero, sets `SyncConfig.heartbeat_interval` =
`Duration::from_millis(n)` and proportionally `heartbeat_timeout =
3 × interval`. This is e2e-only — production callers pass 0.

### 3. Gleam: extend `IntentSnapshot`, build view + popover

#### FFI (`web/src/sunset_web/sunset.ffi.mjs` + `sunset.gleam`)

The existing `IntentSnapshot` Gleam record gains two
`Option(Int)` fields:

```gleam
pub type IntentSnapshot {
  IntentSnapshot(
    id: Float,
    state: String,
    label: String,
    peer_pubkey: option.Option(BitArray),
    kind: option.Option(String),
    attempt: Int,
    last_pong_at_ms: option.Option(Int),    // wall-clock ms; None until first Pong
    last_rtt_ms: option.Option(Int),        // None until first Pong
  )
}
```

The `onIntentChanged` shim's `new IntentSnapshot(...)` call grows two
arguments that read `snap.last_pong_at_unix_ms` /
`snap.last_rtt_ms` and wrap as `Some(n)` / `None`.

Two new shim+external pairs:
- `createClient(seed, heartbeatIntervalMs, callback)` —
  the existing `createClient` shim grows the heartbeat arg.
- `heartbeatIntervalMsFromUrl()` — reads
  `?heartbeat_interval_ms=NNN` from `window.location.search`,
  returning `0` when absent or unparseable.

#### Domain (`web/src/sunset_web/domain.gleam`)

Add a UI-only `Relay` view-model derived per render from the
authoritative `intents` dict. The new type is *not* the source of
truth — `intents` remains so. `Relay` is only what the view needs:

```gleam
pub type RelayConnState {
  RelayConnecting
  RelayConnected
  RelayBackoff
  RelayCancelled
}

pub type Relay {
  Relay(
    id: Float,                              // IntentId — popover key
    host: String,                           // parsed hostname for display
    raw_label: String,                      // full Connectable label
    state: RelayConnState,
    attempt: Int,
    peer_id_short: option.Option(String),   // 4 + 4 hex once handshake done
    last_pong_at_ms: option.Option(Int),
    last_rtt_ms: option.Option(Int),
  )
}
```

Remove `Bridge`, `BridgeKind`, `Minecraft`, `BridgeOpt`, `HasBridge`,
`NoBridge`, `BridgeRelay`, and the `bridge:` field on `Room`,
`Member`, `Message`. All call sites are in `fixture.gleam`,
`views/channels.gleam`, and `views/main_panel.gleam`; none touch real
runtime data.

#### View (`web/src/sunset_web/views/relays.gleam`, new)

Two render entry points:

- `rail_section(palette, relays, on_open) -> Element(msg)` — list
  rendered inside the channels rail under the heading "Relays". One
  row per relay: small status dot (green/amber/grey), hostname,
  click-to-open. Hidden entirely when `relays` is empty.
- `popover(palette, relay, now_ms, placement, on_close) -> Element(msg)`
  — content panel with `Placement = Floating | InSheet`, mirroring
  `peer_status_popover.gleam` so we can put it in `bottom_sheet.view`
  on phone and a fixed-position card on desktop.

Popover body, top-to-bottom:
- Header row: hostname + close button.
- Status row with colored pill: "Connected" / "Connecting" /
  "Backoff (attempt N)" / "Cancelled".
- "Heard from Xs ago" — same shape as
  `peer_status_popover.humanize_age` (kept duplicated; extract a
  shared helper later if a third caller appears).
- "RTT 42 ms" — `case last_rtt_ms { Some(n) -> "RTT " <> n <> " ms";
  None -> "RTT —" }`.
- Full label (the user's relay URL/host) in monospace, selectable.
- Short peer_id ("a1b2c3d4…e5f6a7b8") when `peer_id_short` is
  `Some`; omitted otherwise.

#### Pure helpers (in `views/relays.gleam`)

- `is_relay_label(label)` — `True` when the label does *not* start
  with `"webrtc://"`. This is the distinguisher: `Connectable::Direct`
  for a direct WebRTC peer carries a `webrtc://...` URL; everything
  else (Resolving inputs like `relay.sunset.chat`, or
  `Direct(wss://…)` from the `?relay=` URL param) is a relay.
- `parse_host(label)` — when the label looks like a URL (contains
  `://`), use `gleam/uri.parse` to extract `host[:port]`. When the
  label is a bare hostname (typical for Resolving inputs), return
  it unchanged.
- `format_status(state, attempt)` — see popover body above.
- `format_rtt(last_rtt_ms)` — see popover body above.
- `humanize_age(now_ms, last_ms)` — mirrors
  `peer_status_popover.humanize_age`.
- `parse_state(s)` — `"connected"` → `RelayConnected`, etc.; unknown
  strings → `RelayBackoff` (defensive — keep the row visible).
- `short_peer_id(hex)` — first 8 + last 8 hex chars joined by `…`.
- `from_intent(snap)` — builds a `domain.Relay` from a
  `sunset.IntentSnapshot`.

#### Wiring (`web/src/sunset_web.gleam`)

The `intents: Dict(Float, IntentSnapshot)` Model field already
exists. Two additions:

- `relays_popover: Option(Float)` — the IntentId currently open.
- A view-derived `relays_for_view(model.intents)` helper that filters
  intents to relays via `is_relay_label`, sorts by id (stable
  insertion order), and maps each through `from_intent`.

New Msg variants:
- `OpenRelayPopover(Float)` — open by IntentId.
- `CloseRelayPopover`.

The `IntentChanged` handler is unchanged: the relay-section view is
recomputed each render from the up-to-date `intents` dict. No new
update logic.

The bootstrap `create_client(seed, fn(client) { ... })` call grows a
heartbeat arg via `sunset.heartbeat_interval_ms_from_url()`.

#### Channels rail integration (`views/channels.gleam`)

Replace the `bridge_channels` filter and the `section(p, "Bridges",
...)` block with a call to `relays_view.rail_section(p, relays,
on_open_relay_popover)`, with `relays` passed in via a new `view`
argument.

#### Mobile bottom sheet

The desktop floating popover anchors top:120px right:260px (same
constants as `peer_status_popover` Floating). On phone the popover
renders inside `bottom_sheet.view`, exactly like
`peer_status_sheet_el` in `sunset_web.gleam`, so it slides up from
the bottom and gets the swipe-to-dismiss handle for free. The shell
already mounts the bottom-sheet host; we only add a new branch in
the same overlay-mount block.

#### `data-testid` hooks

- `relays-section` on the rail container (only present when
  non-empty).
- `relay-row` on each row, with `data-relay-host="…"` and
  `data-relay-state="connected|connecting|backoff|cancelled"` for
  selectors.
- `relay-popover` on the popover container.
- `relay-popover-close` on the close button.
- `relay-popover-status`, `relay-popover-heard-from`,
  `relay-popover-rtt`, `relay-popover-label`,
  `relay-popover-peer-id` on the corresponding rows.

### 4. Fixture cleanup

`fixture.gleam` loses every `Bridge(_)`, `HasBridge(_)`, `NoBridge`,
`BridgeRelay`, and `Minecraft` reference; the `minecraft-bridge`
channel and the synthesized minecraft-bridged member rows are
removed. `bridge:` is removed from the fixture `Member`, `Channel`,
`Room`, and `Message` constructors. The `bridge_tag` rendering in
`main_panel.gleam` and the `bridge_channel_row` in `channels.gleam`
are deleted along with their `Bridge` / `Minecraft` imports.

This is required, not optional: removing the types from
`domain.gleam` breaks compilation if the call sites stay.

`Receipt(name: ..., relay: BridgeRelay)` rows in fixtures fall back
to `relay: NoRelay`.

## Tests

### Rust unit tests

`crates/sunset-sync/src/peer.rs`:
- `liveness_emits_pong_observed_with_rtt` — drive the per-peer task
  with the test transport; respond Pong; assert
  `InboundEvent::PongObserved` arrives with `rtt_ms < heartbeat_interval`.

`crates/sunset-sync/src/engine.rs`:
- `pong_observed_inbound_event_propagates_as_engine_event` — call
  `handle_inbound_event(InboundEvent::PongObserved {…})` and assert a
  matching `EngineEvent::PongObserved` is delivered to a subscriber.

`crates/sunset-sync/src/supervisor.rs`:
- `pong_observed_updates_intent_snapshot` — drive a synthetic
  `EngineEvent::PongObserved` for a connected intent; assert
  `snapshot()` shows `last_rtt_ms = Some(_)` and
  `last_pong_at_unix_ms = Some(_)`.
- `pong_observed_for_unknown_peer_is_dropped` — same, but the peer
  is not in `peer_to_intent`; assert no broadcast, no panic.
- `disconnect_preserves_last_pong_and_rtt` — connected intent
  accumulates liveness; on disconnect the entry transitions to
  `Backoff` but `last_pong_at_unix_ms` / `last_rtt_ms` remain
  `Some(_)`.

### Gleam unit tests

A `relays_test.gleam` covers the pure functions:

- `is_relay_label` — `True` for `"relay.sunset.chat"`,
  `"wss://host"`, `"ws://host"`; `False` for `"webrtc://abc#…"`.
- `parse_host` — `"relay.sunset.chat"` → `"relay.sunset.chat"`;
  `"wss://relay.sunset.chat"` → `"relay.sunset.chat"`;
  `"ws://127.0.0.1:8080"` → `"127.0.0.1:8080"`;
  `"wss://relay.sunset.chat:443/api?token=foo#x25519=abc"` →
  `"relay.sunset.chat:443"`.
- `format_status` including `"Backoff (attempt 3)"`.
- `format_rtt`, `humanize_age`, `parse_state`, `short_peer_id` —
  representative cases.
- `from_intent` — sanity check that a known `IntentSnapshot` maps
  to the expected `Relay` (status, host parsed from label, fields
  threaded).

`relay_status_pill_test.gleam` (existing) — already constructs
`IntentSnapshot` records by hand. Adding two fields to that record
breaks the constructor calls; fix by passing `option.None,
option.None` for the new fields. This is mechanical and the test's
assertions stay the same.

### Playwright e2e (`web/e2e/relays.spec.js`, new)

Pattern follows `web/e2e/peer_status_popover.spec.js`: spawn a real
`sunset-relay` in `beforeAll`, target one browser at it via
`?relay=...&heartbeat_interval_ms=2000`. The page boots, the
supervisor connects, the new fields populate. The test:

1. **Desktop renders the row.** `[data-testid="relays-section"]`
   visible. One `[data-testid="relay-row"]` exists with
   `data-relay-host="127.0.0.1:<port>"`.
2. **Status reaches connected.**
   `[data-relay-state="connected"]` on the row within 10 s.
3. **Click opens the popover.**
   `[data-testid="relay-popover"]` visible; header text equals the
   hostname.
4. **Popover shows live liveness** within ~6 s
   (3 × `heartbeat_interval_ms = 2000`):
   `relay-popover-heard-from` matches
   `/^heard from (just now|\d+s ago)$/`, `relay-popover-rtt`
   matches `/^RTT \d+ ms$/`, `relay-popover-label` contains the
   `?relay=` URL.
5. **Close works.** Click `relay-popover-close`; popover gone.
6. **Phone bottom sheet.** Re-run with
   `viewport: { width: 390, height: 844 }`. Assert the popover is
   inside the existing `data-testid="bottom-sheet"` host.

## Out of scope

- Removing the `Channel.kind` discriminant. Voice and text channels
  still use it; only `Bridge(_)` is removed.
- Per-relay enable/disable UI. Adding/removing relays at runtime is
  reserved for a future spec; this one only displays what's there.
- Server-reported relay metadata (operator, location, build
  version). Future relays may publish a small KV entry describing
  themselves; that's separate work.
- Direct-peer connection detail. Direct WebRTC peers also flow
  through the supervisor and would benefit from the same metrics,
  but a per-peer "connections" UI is a different surface — covered
  by the existing peer-status popover today.
