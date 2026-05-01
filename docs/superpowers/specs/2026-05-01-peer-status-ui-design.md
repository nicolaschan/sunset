# Peer Status UI Design

**Date:** 2026-05-01
**Scope:** Per-peer status surface in the web client. Adds a small inline transport icon next to each member name and a click-triggered popover with full status (transport, last app-level heartbeat age, short pubkey). No changes below the wasm bridge — this is a pure UI feature plus one additive field on the existing `MemberJs` JS-exported struct.
**Out of scope (explicit):** transport-level Ping/Pong exposure, multi-relay routing detail, hover-to-preview, self-row popover, click-to-copy pubkey.

## Goal

Make per-peer connection state inspectable for the user without leaving the chat view. Today the members rail shows a presence dot and a name; there is no way to tell whether a peer is reached directly or via the relay, and no way to see when we last heard from them. The connection-liveness work just landed and gives us reliable transport routing data — surface it so users can debug "why isn't my message getting through?" or "is my connection healthy?" without console digging.

The information already exists in the wasm bridge:

- `MemberJs.connection_mode: String` is one of `"self" | "direct" | "via_relay" | "unknown"`, derived from the engine's `TransportKind` (per the connection-liveness merge).
- The membership tracker's `presence_map: HashMap<PeerId, u64>` holds each peer's last app-level presence-heartbeat timestamp (unix-ms).

What's missing: the heartbeat timestamp isn't exposed to JS, and the Gleam UI never reads `connection_mode` at all.

## Architecture

```
sunset-store presence entries
   ↓ (Replay::All subscription)
membership_tracker.rs presence_map: HashMap<PeerId, u64>     [unchanged]
   ↓
members.rs derive_members → MemberJs                          [+last_heartbeat_ms field]
   ↓ wasm-bindgen
JS / Gleam map_members → domain.Member                        [Member gets a last_heartbeat field]
   ↓
sunset_web/views/members.gleam                                [adds inline icon + click handler]
   ↓ on click
sunset_web/views/peer_status_popover.gleam (NEW)              [renders the detail popover]
```

The popover is mounted at the shell level (alongside `voice_popover_overlay`) and its visibility is driven by `Model.peer_status_popover: Option(PeerStatusPopover)`. A 1-second ticker in the Gleam shell drives the popover's age readout independently of the membership tracker's debounced re-emits, so "heard from 3s ago" updates live without forcing a member-list re-render every tick.

### Why expose timestamp, not derived age, across the bridge

The membership tracker today debounces emits via a `(pubkey, presence_bucket, connection_mode)` signature. If we exposed `age_ms` directly, every age change would be a new signature and every callback fire would re-render the whole member list — wasteful for what's just a string update inside a popover. By passing the raw `last_heartbeat_ms` (a unix-ms timestamp) and computing age in the Gleam side from a 1-second ticker, the popover updates smoothly while the member-list callback still only fires on real shape changes (someone went Online → Away, or moved between connection modes).

## Components

### `MemberJs` (modified — `crates/sunset-web-wasm/src/members.rs`)

Add one optional field to the existing struct:

```rust
#[wasm_bindgen]
pub struct MemberJs {
    pub(crate) pubkey: Vec<u8>,
    pub(crate) presence: String,
    pub(crate) connection_mode: String,
    pub(crate) is_self: bool,
    /// Unix-ms timestamp of the last app-level presence heartbeat we
    /// observed for this peer. `None` for self (we don't track our own
    /// presence) and for any peer we've heard nothing from. Reading
    /// side computes "age" as `now - last_heartbeat_ms`.
    pub(crate) last_heartbeat_ms: Option<u64>,
}
```

Add a getter:

```rust
#[wasm_bindgen(getter)]
pub fn last_heartbeat_ms(&self) -> Option<u64> { self.last_heartbeat_ms }
```

`derive_members` populates it from the existing `presence_map` (priority is unix-ms, see `presence_publisher.rs:46`).

### Membership-tracker debounce signature (modified — `crates/sunset-web-wasm/src/membership_tracker.rs`)

The `MemberSig` type today is `Vec<(Vec<u8>, String, String)>` — `(pubkey, presence, connection_mode)`. Leave this **unchanged**: we deliberately do not add the heartbeat timestamp to the signature, so age-only changes don't fire the JS callback. The popover gets its live updates from the Gleam ticker reading `last_heartbeat_ms` from the most-recent member snapshot.

### Gleam domain (modified — `web/src/sunset_web/domain.gleam`)

Extend the `Member` record with one optional field:

```gleam
pub type Member {
  Member(
    id: MemberId,
    name: String,
    initials: String,
    status: Presence,
    relay: RelayStatus,
    you: Bool,
    in_call: Bool,
    bridge: BridgeOpt,
    role: RoleOpt,
    /// Unix-ms timestamp from the wasm bridge. `None` for self or
    /// peers we haven't heard from. The popover renders age = now - this.
    last_heartbeat_ms: option.Option(Int),
  )
}
```

The `relay: RelayStatus` field already exists in the Gleam domain but is currently unused. Populate it in `map_members` from `MemberJs.connection_mode`:

```
"self"      → SelfRelay
"direct"    → Direct
"via_relay" → OneHop("")        (empty relay name in v1)
"unknown"   → NoRelay
```

### `views/members.gleam` (modified)

Each member row gains:

1. A small inline transport icon (right-aligned in the row, before the optional bridge tag) — single Unicode glyph, `palette.text_faint` color:
   - `Direct` → `↔`
   - `OneHop(_)` → `⤴`
   - `BridgeRelay | TwoHop | ViaPeer | NoRelay` → no icon (deferred topology)
   - `SelfRelay` → no icon
2. An `event.on_click` on the row that dispatches `OpenPeerStatusPopover(member_id, anchor_rect)` to the shell.

The icon is purely visual; click anywhere on the row opens the popover (mirrors the voice-channel-row click → `OpenVoicePopover` pattern).

### `views/peer_status_popover.gleam` (new file)

```gleam
pub type Placement {
  Floating
  InSheet
}

pub fn view(
  palette p: Palette,
  member m: Member,
  now_ms now: Int,
  placement: Placement,
) -> Element(msg)
```

Renders three rows of text:

```
[transport icon] [transport label]      ← "Direct (WebRTC)" or "Via relay"
heard from 3s ago                       ← humanize(now_ms - last_heartbeat_ms)
83fb…d4da                               ← short pubkey: first 4 + last 4 hex bytes
```

Humanization rules:
- `< 1s` → `"just now"`
- `< 60s` → `"{N}s ago"`
- `< 3600s` → `"{N}m ago"`
- `≥ 3600s` → `"hours"` (floor to hours)
- `last_heartbeat_ms == None` → `"never"`

`Floating` is anchored to the row that opened it (matches `voice_popover.Floating`). `InSheet` renders inside the existing mobile bottom-sheet container — pick the placement at the shell level based on viewport (same logic as voice popover).

### Shell wiring (modified — `web/src/sunset_web.gleam`)

1. New model field `peer_status_popover: option.Option(PeerStatusPopover)` and corresponding `OpenPeerStatusPopover(MemberId, Rect)` / `ClosePeerStatusPopover` messages.
2. New 1-second ticker that updates `model.now_ms` (a new field) so the popover's age readout re-evaluates every tick. The ticker is a `lustre.effect` that schedules `Tick` messages.
3. Add `peer_status_popover_overlay(palette, model)` next to `voice_popover_overlay(palette, model)` in the shell rendering.
4. `map_members` populates the new `relay` and `last_heartbeat_ms` fields from `MemberJs`.

## Data flow walkthrough

```
T = 0ms     Alice publishes presence heartbeat (interval default ~5s).
T = 0ms     Bob's presence_publisher writes <fp>/presence/<bob_pk> locally.
T = 0ms     Bob's local_sub fires Inserted; engine fans out to relay; relay forwards to Alice.
T = ~50ms   Alice's local_sub fires Inserted; presence_map[bob] = 0.
T = ~50ms   Tracker's maybe_fire computes derive_members:
              MemberJs { ..bob.., last_heartbeat_ms: Some(0) }
            Signature unchanged from the previous bucket → callback NOT fired.
T = 100ms   User clicks Bob's row → OpenPeerStatusPopover(bob_id, anchor) dispatched.
T = 100ms   Shell sets model.peer_status_popover = Some(...).
T = 100ms   Shell renders peer_status_popover.view(... now_ms = 100, m = bob_member ...)
              → "heard from just now"
T = 1100ms  Tick fires → model.now_ms = 1100. Popover re-renders.
              → "heard from 1s ago"
T = 5050ms  Bob publishes again; presence_map[bob] = 5000; bucket unchanged → no callback.
T = 5100ms  Tick fires; popover reads m.last_heartbeat_ms = Some(0)  ← STALE
            because the member snapshot wasn't re-emitted. Renders "5s ago".
T = ~10s    Bob's bucket flips Online→Away (interval boundary). Signature changes
            → tracker fires callback → MembersUpdated → model.members updated with
            the new last_heartbeat_ms = 5000.
T = 10s+    Popover now reads the up-to-date timestamp.
```

This staleness is bounded — the heartbeat timestamp in the popover can lag the truth by up to one bucket transition (default `interval_ms`, on the order of seconds). The age STRING the popover renders is always monotonic-increasing because `now_ms` ticks every second; it just snaps back to the actual age at each member-list re-emit. Acceptable for a debug surface.

If we ever want truly live timestamps in the popover without spurious member-list re-renders, the cleanest fix is a separate per-peer `latest_heartbeat_ms(pubkey) -> Option<u64>` getter on `Client` that the popover reads directly each tick — sidesteps the debounce entirely. Out of scope for v1; flagged for the future-work list.

## Failure modes

| Scenario | Behaviour |
|---|---|
| Self-row clicked | Popover opens with member-row data; shows `connection_mode = "self"` → renders no transport icon, age "—" or omitted. **Decision: self row is not clickable in v1**; skip the click handler when `m.you == true`. |
| Peer with `last_heartbeat_ms = None` | Popover shows "heard from: never" — typically only happens for peers we have a transport connection to but no presence entry yet. Rare; acceptable text. |
| Peer with `connection_mode = "unknown"` | No inline icon; popover shows "Transport: unknown". This is the existing `TransportKind::Unknown` for test transports; production paths set Primary or Secondary. |
| Popover open when member goes Offline / leaves | The shell's `MembersUpdated` reducer drops offline members. The popover's `member: Member` reference becomes stale. Resolution: on `MembersUpdated`, if `peer_status_popover.member_id` is no longer in `model.members`, set `peer_status_popover = None`. |
| Two clicks on different rows | Second click replaces the first (single popover). Same as voice popover. |
| Popover open while user resizes window | Anchor rect is captured at open time and is approximate; on next render the popover may be slightly off. Acceptable; voice popover has the same property. |

## Testing

### Rust unit tests (`crates/sunset-web-wasm/src/members.rs`)

Extend the existing `mod tests`:

1. **`derive_members_includes_last_heartbeat`** — given a `presence_map` with `(bob_pk, 12345_u64)`, the resulting `MemberJs` for Bob has `last_heartbeat_ms == Some(12345)`.
2. **`self_row_has_no_heartbeat`** — `MemberJs` with `is_self == true` has `last_heartbeat_ms == None`.
3. **`signature_ignores_heartbeat_timestamp`** — `members_signature` of two member lists that differ only in `last_heartbeat_ms` returns equal signatures (so the tracker doesn't re-emit).

### Gleam unit tests (`web/test/sunset_web/views/peer_status_popover_test.gleam`)

(If the project has gleeunit set up — based on `web/test/`.)

1. **`renders_direct_transport_label`** — `view` with `relay = Direct` includes `"Direct"` text.
2. **`renders_via_relay_label`** — `view` with `relay = OneHop("")` includes `"Via relay"` text.
3. **`humanize_just_now`** — `(now=100, last=50)` → contains `"just now"`.
4. **`humanize_seconds`** — `(now=5500, last=500)` → contains `"5s"`.
5. **`humanize_minutes`** — `(now=120000, last=0)` → contains `"2m"`.
6. **`humanize_never`** — `last_heartbeat_ms = None` → contains `"never"`.

### Playwright e2e (`web/e2e/peer_status_popover.spec.js`)

One test, end-to-end:

1. Two browser contexts join the same room; wait until each sees the other in the members list.
2. Page A clicks Bob's row.
3. Assert popover is visible on page A.
4. Assert popover contains the text `"Via relay"` (since both go through the relay).
5. Assert popover contains a substring matching `/heard from \d+s ago|just now/`.
6. Wait 3 seconds; assert the popover age increased (text changed).
7. Click outside the popover; assert it closes.

## Out of scope (full list)

- **Transport-level Ping/Pong heartbeat exposure.** Per-connection RTT or Pong age would require new plumbing from the engine and isn't useful per-peer (only per-connection, which is more naturally surfaced on the relay-status indicator).
- **Multi-relay routing detail.** "Via which relay?" requires multi-relay topology support in `sunset-sync`. v1 single-relay → `OneHop("")` with empty name.
- **Hover-to-preview on desktop.** Click-only on both surfaces. Mirrors voice popover; avoids dual desktop/mobile code paths.
- **Self-row popover.** Self isn't actionable in this context; rendering "you" with no transport info adds clutter without information.
- **Click-to-copy pubkey.** Would be nice; the popover renders the short hex but no clipboard integration in v1.
- **Live-stream timestamps via direct getter.** The bucket-debounce-then-Gleam-tick approach has bounded staleness (≤ `interval_ms`) and is good enough for v1. Future work if needed.

## Risks

1. **Bridge type widening.** Adding `last_heartbeat_ms: Option<u64>` to `MemberJs` is a JS-API change. Existing code reading `MemberJs` won't break (new optional field, getter only), but the `map_members` Gleam side must read the field; if it doesn't, the Gleam `Member` carries `None` and the popover shows "never" forever. Mitigated by integration test and unit tests on `map_members`.
2. **Popover anchor accuracy.** Voice popover already has this trade-off; we accept the same approximation.
3. **Bucket-staleness.** Discussed in "Data flow walkthrough." Bounded by `interval_ms`; acceptable for a debug surface. Documented in code comments at the popover render site.
4. **Gleam ticker added to the shell.** A 1Hz `Tick` message will fire forever as long as the page is open. The reducer's only effect is bumping `model.now_ms`; since the popover is mounted lazily (only when `peer_status_popover != None`), the tick has no UI cost when no popover is open. The ticker keeps running regardless — small constant cost that matches how voice waveforms already tick in `voice_popover.gleam`.
5. **`MemberId` ↔ pubkey mapping.** The popover model stores `MemberId` to identify the member. The current code derives `MemberId` from a hex prefix of the pubkey (matches existing `map_members`). The lookup `members.find(...)` in the shell is O(n) per render — fine for room sizes in practice (n ≤ 50).

## Review summary

- **Placeholders:** none.
- **Internal consistency:** the `last_heartbeat_ms: Option<u64>` field flows from `presence_map` → `MemberJs` → Gleam `Member.last_heartbeat_ms` → popover render. The signature deliberately ignores it (callbacks are bucket-driven) and the popover compensates via a 1Hz ticker. The transport mapping (`connection_mode` String → `RelayStatus` enum) is one-to-one and exhaustive over the v1 set.
- **Scope:** one Rust struct field + getter, one Gleam record field, one new Gleam view, two member-row tweaks, one shell ticker + popover overlay, three test layers. Suitable for a single implementation plan.
- **Ambiguity:** "self isn't clickable" is called out explicitly in the failure-mode table to remove the obvious ambiguity. Bucket-staleness is called out in the data-flow walkthrough so a future reader understands the design choice.
