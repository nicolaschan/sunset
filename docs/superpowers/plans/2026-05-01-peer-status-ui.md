# Peer Status UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a per-peer status popover (transport name, time since last app-level heartbeat, short pubkey) to the web client, anchored on a click-anywhere-on-the-row interaction in the members rail. Plus a small inline transport icon next to each name as an always-visible glanceable indicator.

**Architecture:** One additive field on `MemberJs` (a unix-ms timestamp), threaded through the existing wasm-bindgen → JS getter → Gleam FFI bridge. New Gleam view `peer_status_popover.gleam` mirrors the structure of the existing `voice_popover.gleam`. Shell adds a popover-state field, open/close messages, and a 1Hz ticker so the popover's age readout updates between membership-tracker emits.

**Tech Stack:** Rust 2024 + wasm-bindgen for the bridge, Gleam + Lustre for the UI, Playwright for e2e. Uses the same patterns as the already-merged `voice_popover.gleam`.

**Spec:** `docs/superpowers/specs/2026-05-01-peer-status-ui-design.md`.

---

## File map

**Modify (Rust / wasm bridge):**

- `crates/sunset-web-wasm/src/members.rs` — add `last_heartbeat_ms: Option<u64>` field on `MemberJs`, getter, populate in `derive_members`. Three new tests.

**Modify (Gleam FFI):**

- `web/src/sunset_web/sunset.ffi.mjs` — add `memLastHeartbeatMs` JS getter; add `setIntervalMs` helper for the popover ticker.
- `web/src/sunset_web/sunset.gleam` — add `mem_last_heartbeat_ms` external; add `set_interval_ms` external.

**Modify (Gleam domain + shell):**

- `web/src/sunset_web/domain.gleam` — add `last_heartbeat_ms: option.Option(Int)` field on `Member`.
- `web/src/sunset_web.gleam` — update `map_members` to populate the new field; add `Model.peer_status_popover`, `Model.now_ms`, `OpenPeerStatusPopover`/`ClosePeerStatusPopover`/`Tick` messages, ticker effect, popover overlay.
- `web/src/sunset_web/views/members.gleam` — add inline transport icon glyph + `event.on_click` handler.

**Modify (Gleam fixtures + tests):**

- `web/src/sunset_web/fixture.gleam` — add `last_heartbeat_ms: option.None` to fixture `Member` records so the codebase still compiles.
- `web/test/sunset_web/views/peer_status_popover_test.gleam` (NEW) — gleeunit tests for `humanize_age` and `view` rendering.

**Create (Playwright):**

- `web/e2e/peer_status_popover.spec.js` — e2e test covering click-to-open, transport label, age readout, click-outside-to-close.

**Create (Gleam view):**

- `web/src/sunset_web/views/peer_status_popover.gleam` — the popover view.

---

## Quick reference

**Working directory:** `/home/nicolas/src/sunset/.worktrees/peer-status` (branch `feature/peer-status-ui`).

**Native test:** `nix develop --command cargo test -p sunset-web-wasm --all-features`
**Workspace test:** `nix develop --command cargo test --workspace --all-features`
**Gleam test:** `cd web && nix develop --command gleam test`
**Playwright (single test):** `nix run .#web-test -- peer_status_popover.spec.js --project=chromium`
**Playwright (whole suite):** `nix run .#web-test -- --project=chromium`
**Lint:** `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
**Format:** `nix develop --command cargo fmt --all --check`

---

## Phase 1: Rust wasm bridge

### Task 1: Add `last_heartbeat_ms` to `MemberJs`

**Files:**
- Modify: `crates/sunset-web-wasm/src/members.rs`

- [ ] **Step 1: Read the current `MemberJs` struct and `derive_members` function**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
grep -n "pub struct MemberJs\|pub fn derive_members\|members_signature" crates/sunset-web-wasm/src/members.rs
```

Confirm the struct currently has `pubkey`, `presence`, `connection_mode`, `is_self` fields and the `derive_members` function takes `(now_ms, interval_ms, ttl_ms, self_peer, presence_map, peer_kinds)`.

- [ ] **Step 2: Write the failing tests**

Append to the existing `#[cfg(test)] mod tests` block in `crates/sunset-web-wasm/src/members.rs`:

```rust
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn derive_members_includes_last_heartbeat_for_others() {
        use std::collections::HashMap;
        let me = pk(0);
        let bob = pk(1);
        let mut presence = HashMap::new();
        presence.insert(bob.clone(), 12_345_u64);
        let kinds = HashMap::new();
        let out = derive_members(20_000, 30_000, 60_000, &me, &presence, &kinds);
        // Self is index 0 (always present); Bob is index 1.
        assert_eq!(out[0].is_self, true);
        assert_eq!(out[0].last_heartbeat_ms, None);
        assert_eq!(out[1].is_self, false);
        assert_eq!(out[1].last_heartbeat_ms, Some(12_345_u64));
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn signature_ignores_heartbeat_timestamp() {
        // Two member lists differing ONLY in last_heartbeat_ms must
        // produce equal signatures, otherwise the membership tracker
        // would re-emit on every age tick.
        let m1 = MemberJs {
            pubkey: vec![1; 32],
            presence: "online".to_owned(),
            connection_mode: "via_relay".to_owned(),
            is_self: false,
            last_heartbeat_ms: Some(100),
        };
        let m2 = MemberJs {
            pubkey: vec![1; 32],
            presence: "online".to_owned(),
            connection_mode: "via_relay".to_owned(),
            is_self: false,
            last_heartbeat_ms: Some(200),
        };
        assert_eq!(members_signature(&[m1]), members_signature(&[m2]));
    }
```

- [ ] **Step 3: Run tests to verify compile failure**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
nix develop --command cargo test -p sunset-web-wasm --all-features --lib members 2>&1 | head -20
```

Expected: compile error — `no field 'last_heartbeat_ms' on MemberJs`.

- [ ] **Step 4: Add the field, getter, and populate in `derive_members`**

In `crates/sunset-web-wasm/src/members.rs`, modify the `MemberJs` struct (look for `pub struct MemberJs`):

```rust
#[wasm_bindgen]
pub struct MemberJs {
    pub(crate) pubkey: Vec<u8>,
    pub(crate) presence: String,
    pub(crate) connection_mode: String,
    pub(crate) is_self: bool,
    /// Unix-ms timestamp of the last app-level presence heartbeat we
    /// observed for this peer. `None` for self (we don't track our own
    /// presence) and for any peer we've heard nothing from. The Gleam
    /// popover computes age = now_ms - last_heartbeat_ms.
    pub(crate) last_heartbeat_ms: Option<u64>,
}
```

In the `#[wasm_bindgen] impl MemberJs` block (right after the `is_self` getter), add:

```rust
    #[wasm_bindgen(getter)]
    pub fn last_heartbeat_ms(&self) -> Option<u64> {
        self.last_heartbeat_ms
    }
```

In `derive_members`, modify the self push to set `last_heartbeat_ms: None`:

```rust
    out.push(MemberJs {
        pubkey: self_peer.verifying_key().as_bytes().to_vec(),
        presence: Presence::Online.as_str().to_owned(),
        connection_mode: "self".to_owned(),
        is_self: true,
        last_heartbeat_ms: None,
    });
```

In the per-peer loop body, modify the `MemberJs` construction to include the timestamp:

```rust
        out.push(MemberJs {
            pubkey: pk.verifying_key().as_bytes().to_vec(),
            presence: presence.as_str().to_owned(),
            connection_mode,
            is_self: false,
            last_heartbeat_ms: Some(*last_ms),
        });
```

(The `last_ms` binding is already present from the existing `for (pk, last_ms) in others` loop.)

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
nix develop --command cargo test -p sunset-web-wasm --all-features --lib members
```

Expected: all members tests pass, including the two new ones.

- [ ] **Step 6: Run the full crate tests to confirm no regression**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
nix develop --command cargo test -p sunset-web-wasm --all-features
```

Expected: all tests pass.

- [ ] **Step 7: Verify wasm32 build still works**

```bash
nix develop --command cargo check -p sunset-web-wasm --target wasm32-unknown-unknown
```

Expected: clean.

- [ ] **Step 8: Commit**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
git add crates/sunset-web-wasm/src/members.rs
git commit -m "Expose last_heartbeat_ms on MemberJs"
```

---

## Phase 2: Gleam FFI for the new field + ticker

### Task 2: Add `memLastHeartbeatMs` JS getter and Gleam binding

**Files:**
- Modify: `web/src/sunset_web/sunset.ffi.mjs`
- Modify: `web/src/sunset_web/sunset.gleam`

- [ ] **Step 1: Add the JS getter**

In `web/src/sunset_web/sunset.ffi.mjs`, find the existing block of `memPubkey`/`memPresence`/`memConnectionMode`/`memIsSelf` getters (around line 205) and append:

```javascript
export function memLastHeartbeatMs(m) {
  // Returns null when the wasm side reports None; Gleam decodes
  // null → option.None and number → option.Some(number).
  const v = m.last_heartbeat_ms;
  return v === undefined ? null : v;
}
```

- [ ] **Step 2: Add the Gleam external**

In `web/src/sunset_web/sunset.gleam`, find the existing block of `mem_*` externals (around line 130) and append:

```gleam
import gleam/option

@external(javascript, "./sunset.ffi.mjs", "memLastHeartbeatMs")
pub fn mem_last_heartbeat_ms(m: MemberJs) -> option.Option(Int)
```

(If `gleam/option` is already imported in this file, don't duplicate the import — just add the external.)

- [ ] **Step 3: Add a `set_interval_ms` helper for the popover ticker**

In `web/src/sunset_web/sunset.ffi.mjs`, append to the bottom of the file:

```javascript
/// Schedule a recurring callback every `ms` milliseconds. Returns nothing
/// — there is no cancel handle in v1; the ticker runs for the page
/// lifetime. Use only for cheap, idempotent dispatches.
export function setIntervalMs(ms, callback) {
  setInterval(callback, ms);
}
```

In `web/src/sunset_web/sunset.gleam`, add the corresponding external near the other timing/utility helpers (after `presence_params_from_url`):

```gleam
@external(javascript, "./sunset.ffi.mjs", "setIntervalMs")
pub fn set_interval_ms(ms: Int, callback: fn() -> Nil) -> Nil
```

- [ ] **Step 4: Build to verify both sides compile**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
nix develop --command cargo check -p sunset-web-wasm --target wasm32-unknown-unknown
cd web && nix develop --command gleam build 2>&1 | tail -10
```

Expected: both clean.

- [ ] **Step 5: Commit**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
git add web/src/sunset_web/sunset.ffi.mjs web/src/sunset_web/sunset.gleam
git commit -m "FFI: mem_last_heartbeat_ms getter + set_interval_ms helper"
```

---

## Phase 3: Gleam domain + map_members

### Task 3: Add `last_heartbeat_ms` to `domain.Member` and populate it

**Files:**
- Modify: `web/src/sunset_web/domain.gleam`
- Modify: `web/src/sunset_web.gleam`
- Modify: `web/src/sunset_web/fixture.gleam` (compile-time fixup)

- [ ] **Step 1: Add the field to `domain.Member`**

In `web/src/sunset_web/domain.gleam`, find the `pub type Member { Member(...) }` definition (around line 80) and add the field. The block currently looks like:

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
  )
}
```

Replace with:

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
    /// Unix-ms timestamp of the last app-level presence heartbeat we
    /// received from this peer. `None` for self or peers we have not
    /// heard from. The popover renders age as `now_ms - this`.
    last_heartbeat_ms: option.Option(Int),
  )
}
```

If `gleam/option` isn't already imported at the top of this file, add `import gleam/option` near the other imports.

- [ ] **Step 2: Build to confirm it fails**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status/web
nix develop --command gleam build 2>&1 | tail -20
```

Expected: errors at every `Member(...)` construction site that doesn't supply the new field. There will be several:
- `web/src/sunset_web.gleam` (`map_members`)
- `web/src/sunset_web/fixture.gleam` (fixture members)

- [ ] **Step 3: Update `map_members` in the shell**

In `web/src/sunset_web.gleam`, find `fn map_members` (around line 1183) and update each `domain.Member(...)` construction to include `last_heartbeat_ms: sunset.mem_last_heartbeat_ms(m)`:

```gleam
fn map_members(ms: List(sunset.MemberJs)) -> List(domain.Member) {
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
      last_heartbeat_ms: sunset.mem_last_heartbeat_ms(m),
    )
  })
}
```

- [ ] **Step 4: Update fixture members**

In `web/src/sunset_web/fixture.gleam`, every `domain.Member(...)` construction needs the new field. Add `last_heartbeat_ms: option.None` to each. Find them with:

```bash
grep -n "domain.Member(" web/src/sunset_web/fixture.gleam
```

For every match, add the field. Example (the exact text varies per fixture entry; this is the shape):

```gleam
domain.Member(
  id: domain.MemberId("noor"),
  name: "noor",
  initials: "n",
  status: domain.Online,
  relay: domain.Direct,
  you: False,
  in_call: True,
  bridge: domain.NoBridge,
  role: domain.NoRole,
  last_heartbeat_ms: option.None,
)
```

Add `import gleam/option` to the top of `fixture.gleam` if not already present.

- [ ] **Step 5: Build cleanly**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status/web
nix develop --command gleam build 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 6: Run existing Gleam tests**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status/web
nix develop --command gleam test 2>&1 | tail -20
```

Expected: all existing tests pass (none of them touch the new field, so they only need the construction-site updates).

- [ ] **Step 7: Commit**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
git add web/src/sunset_web/domain.gleam web/src/sunset_web.gleam web/src/sunset_web/fixture.gleam
git commit -m "Thread last_heartbeat_ms through to domain.Member"
```

---

## Phase 4: Popover view

### Task 4: Create `peer_status_popover.gleam` with view + helpers

**Files:**
- Create: `web/src/sunset_web/views/peer_status_popover.gleam`

- [ ] **Step 1: Create the file**

Write `web/src/sunset_web/views/peer_status_popover.gleam`:

```gleam
//// Floating popover that opens when the user clicks a member row.
////
//// Shows three lines:
////   * Transport label ("Direct (WebRTC)" or "Via relay" or "Self" / "Unknown")
////   * Time since last app-level presence heartbeat (humanized)
////   * Short pubkey (first 4 + last 4 hex bytes)
////
//// Anchored at a fixed position over the chat shell to match the
//// existing voice_popover convention. Two placements: Floating (desktop)
//// and InSheet (mobile bottom sheet).

import gleam/int
import gleam/option
import gleam/string
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Member, Direct, NoRelay, OneHop, SelfRelay,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub type Placement {
  Floating
  InSheet
}

pub fn view(
  palette p: Palette,
  member m: Member,
  now_ms now: Int,
  placement placement: Placement,
  on_close on_close: msg,
) -> Element(msg) {
  let body =
    html.div(
      [
        ui.css([
          #("display", "flex"),
          #("flex-direction", "column"),
          #("gap", "10px"),
          #("padding", "14px 16px"),
        ]),
      ],
      [
        header(p, m.name, on_close),
        row(p, transport_label(m.relay)),
        row(p, "heard from " <> humanize_age(now, m.last_heartbeat_ms)),
        row_mono(p, short_pubkey_display(m)),
      ],
    )

  case placement {
    Floating ->
      html.div(
        [
          attribute.attribute("data-testid", "peer-status-popover"),
          ui.css([
            #("position", "fixed"),
            #("top", "120px"),
            #("right", "260px"),
            #("width", "260px"),
            #("background", p.surface),
            #("color", p.text),
            #("border", "1px solid " <> p.border),
            #("border-radius", "10px"),
            #("box-shadow", p.shadow_lg),
            #("z-index", "20"),
          ]),
        ],
        [body],
      )
    InSheet ->
      html.div(
        [
          attribute.attribute("data-testid", "peer-status-popover"),
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("background", p.surface),
            #("color", p.text),
          ]),
        ],
        [body],
      )
  }
}

fn header(p: Palette, name: String, on_close: msg) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "space-between"),
        #("gap", "8px"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("font-size", "16px"),
            #("color", p.text),
            #("white-space", "nowrap"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
          ]),
        ],
        [html.text(name)],
      ),
      html.button(
        [
          attribute.attribute("data-testid", "peer-status-popover-close"),
          event.on_click(on_close),
          ui.css([
            #("background", "transparent"),
            #("border", "none"),
            #("color", p.text_faint),
            #("cursor", "pointer"),
            #("font-size", "16px"),
            #("padding", "0 4px"),
          ]),
        ],
        [html.text("×")],
      ),
    ],
  )
}

fn row(p: Palette, text: String) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("font-size", "14px"),
        #("color", p.text_muted),
      ]),
    ],
    [html.text(text)],
  )
}

fn row_mono(p: Palette, text: String) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("font-family", "monospace"),
        #("font-size", "13px"),
        #("color", p.text_faint),
      ]),
    ],
    [html.text(text)],
  )
}

/// Map domain.RelayStatus → user-facing label. Exhaustive on the v1 set.
pub fn transport_label(r: domain.RelayStatus) -> String {
  case r {
    Direct -> "Direct (WebRTC)"
    OneHop -> "Via relay"
    SelfRelay -> "Self"
    NoRelay -> "Unknown"
    _ -> "Unknown"
  }
}

/// Render age "heard from …": "just now" / "Ns ago" / "Nm ago" / "Nh ago" / "never".
pub fn humanize_age(now_ms: Int, last_ms: option.Option(Int)) -> String {
  case last_ms {
    option.None -> "never"
    option.Some(t) -> {
      let age_ms = case now_ms - t {
        n if n < 0 -> 0
        n -> n
      }
      let age_s = age_ms / 1000
      case age_s {
        s if s < 1 -> "just now"
        s if s < 60 -> int.to_string(s) <> "s ago"
        s if s < 3600 -> int.to_string(s / 60) <> "m ago"
        s -> int.to_string(s / 3600) <> "h ago"
      }
    }
  }
}

/// First 4 + last 4 hex bytes of the pubkey (derived from MemberId in v1
/// where the id IS the short pubkey hex). For v1 the MemberId already
/// holds the short pubkey string, so we just truncate/format.
pub fn short_pubkey_display(m: Member) -> String {
  let domain.MemberId(s) = m.id
  case string.length(s) {
    n if n <= 16 -> s
    _ -> string.slice(s, 0, 8) <> "…" <> string.slice(s, string.length(s) - 8, 8)
  }
}
```

- [ ] **Step 2: Build to confirm it compiles**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status/web
nix develop --command gleam build 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 3: Write the unit test file**

Create `web/test/sunset_web/views/peer_status_popover_test.gleam`:

```gleam
import gleam/option
import gleeunit/should
import sunset_web/domain
import sunset_web/views/peer_status_popover

pub fn humanize_age_never_test() {
  peer_status_popover.humanize_age(1000, option.None)
  |> should.equal("never")
}

pub fn humanize_age_just_now_test() {
  peer_status_popover.humanize_age(100, option.Some(50))
  |> should.equal("just now")
}

pub fn humanize_age_seconds_test() {
  peer_status_popover.humanize_age(5500, option.Some(500))
  |> should.equal("5s ago")
}

pub fn humanize_age_minutes_test() {
  peer_status_popover.humanize_age(125_000, option.Some(0))
  |> should.equal("2m ago")
}

pub fn humanize_age_hours_test() {
  peer_status_popover.humanize_age(7200_000, option.Some(0))
  |> should.equal("2h ago")
}

pub fn humanize_age_clock_skew_test() {
  // Future timestamp (e.g., clock skew) clamps to 0 → "just now".
  peer_status_popover.humanize_age(100, option.Some(500))
  |> should.equal("just now")
}

pub fn transport_label_direct_test() {
  peer_status_popover.transport_label(domain.Direct)
  |> should.equal("Direct (WebRTC)")
}

pub fn transport_label_via_relay_test() {
  peer_status_popover.transport_label(domain.OneHop)
  |> should.equal("Via relay")
}

pub fn transport_label_self_test() {
  peer_status_popover.transport_label(domain.SelfRelay)
  |> should.equal("Self")
}

pub fn transport_label_unknown_test() {
  peer_status_popover.transport_label(domain.NoRelay)
  |> should.equal("Unknown")
}
```

- [ ] **Step 4: Run the Gleam tests**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status/web
nix develop --command gleam test 2>&1 | tail -20
```

Expected: all tests pass, including the new ones.

- [ ] **Step 5: Commit**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
git add web/src/sunset_web/views/peer_status_popover.gleam web/test/sunset_web/views/peer_status_popover_test.gleam
git commit -m "Add peer_status_popover view + humanize/transport_label tests"
```

---

## Phase 5: Members rail click handler + transport icon

### Task 5: Add inline transport icon and click handler in `members.gleam`

**Files:**
- Modify: `web/src/sunset_web/views/members.gleam`

- [ ] **Step 1: Read the existing `member_row` function**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
grep -n "fn member_row\|on_click" web/src/sunset_web/views/members.gleam
```

The current `member_row` is a static row with a status dot, name, and optional bridge tag. We need to:
1. Add an inline transport icon (single Unicode glyph) just before the bridge tag.
2. Add an `event.on_click` to the row's outer `html.div` that dispatches an open-popover message.
3. Skip the click handler when `m.you == true` (self isn't actionable).

The `members.view` function currently takes `(palette, members)`. We need to add an `on_open_status` callback parameter so the row can dispatch a message without `members.gleam` knowing about the shell's `Msg` type.

- [ ] **Step 2: Update `members.view` and `member_row` signatures**

In `web/src/sunset_web/views/members.gleam`, change the imports at the top to add:

```gleam
import lustre/event
import sunset_web/views/peer_status_popover
```

Replace the `view` function signature and body. Find:

```gleam
pub fn view(palette p: Palette, members ms: List(Member)) -> Element(msg) {
```

Replace with:

```gleam
pub fn view(
  palette p: Palette,
  members ms: List(Member),
  on_open_status on_open: fn(domain.MemberId) -> msg,
) -> Element(msg) {
```

In the `view` body, find every call to `member_row(p, m, ...)` and replace with `member_row(p, m, on_open, ...)`. There are typically two:

```gleam
list.map(in_call_others, fn(m) { member_row(p, m, on_open, False) }),
list.map(online_not_in_call, fn(m) { member_row(p, m, on_open, False) }),
...
list.map(offline_members, fn(m) { member_row(p, m, on_open, True) }),
```

- [ ] **Step 3: Update `member_row` to accept the callback and add icon + handler**

Replace the existing `fn member_row(p: Palette, m: Member, dim: Bool)` with:

```gleam
fn member_row(
  p: Palette,
  m: Member,
  on_open: fn(domain.MemberId) -> msg,
  dim: Bool,
) -> Element(msg) {
  let dot_color = case m.status {
    Speaking -> p.live
    Online -> p.live
    Away -> p.warn
    MutedP -> p.text_faint
    OfflineP -> p.text_faint
  }
  let opacity = case dim {
    True -> "0.55"
    False -> "1"
  }
  let weight = case m.status {
    Speaking -> "600"
    _ -> "400"
  }
  let color = case m.status {
    Speaking -> p.text
    _ -> p.text_muted
  }

  // Click anywhere on the row opens the per-peer status popover.
  // Self isn't actionable — no handler, no cursor change.
  let click_attrs = case m.you {
    True -> []
    False -> [
      event.on_click(on_open(m.id)),
      ui.css([#("cursor", "pointer")]),
    ]
  }

  html.div(
    list.append(
      [
        attribute.attribute("data-testid", "member-row"),
        attribute.attribute("data-member-id", member_id_str(m.id)),
        ui.css([
          #("display", "flex"),
          #("align-items", "center"),
          #("gap", "8px"),
          #("padding", "5px 10px"),
          #("opacity", opacity),
        ]),
      ],
      click_attrs,
    ),
    list.flatten([
      [
        html.span(
          [
            ui.css([
              #("width", "7px"),
              #("height", "7px"),
              #("border-radius", "999px"),
              #("background", dot_color),
              #("flex-shrink", "0"),
            ]),
          ],
          [],
        ),
        html.span(
          [
            ui.css([
              #("font-size", "16.25px"),
              #("font-weight", weight),
              #("color", color),
              #("flex", "1"),
              #("min-width", "0"),
              #("white-space", "nowrap"),
              #("overflow", "hidden"),
              #("text-overflow", "ellipsis"),
            ]),
          ],
          [html.text(m.name)],
        ),
        transport_icon(p, m.relay),
      ],
      case m.bridge {
        HasBridge(_) -> [bridge_tag(p)]
        NoBridge -> []
      },
    ]),
  )
}

/// Return a small Unicode glyph indicating the transport route for this
/// peer. `↔` for direct WebRTC, `⤴` for via-relay; nothing for self or
/// unknown topologies (deferred multi-hop / bridge variants).
fn transport_icon(p: Palette, r: domain.RelayStatus) -> Element(msg) {
  let glyph = case r {
    domain.Direct -> "↔"
    domain.OneHop -> "⤴"
    _ -> ""
  }
  case glyph {
    "" -> element.fragment([])
    g ->
      html.span(
        [
          attribute.attribute("data-testid", "member-transport-icon"),
          ui.css([
            #("font-size", "12px"),
            #("color", p.text_faint),
            #("flex-shrink", "0"),
          ]),
        ],
        [html.text(g)],
      )
  }
}

fn member_id_str(id: domain.MemberId) -> String {
  let domain.MemberId(s) = id
  s
}
```

You will need to add these imports to the top of `members.gleam` if they aren't already present:

```gleam
import lustre/attribute
import sunset_web/domain.{
  type Member, Away, HasBridge, MutedP, NoBridge, OfflineP, Online, Speaking,
}
```

(`HasBridge`/`NoBridge` are already imported per the existing file; just add `domain` aliasing or `attribute` if missing.)

- [ ] **Step 4: Update the shell's call site of `members.view`**

In `web/src/sunset_web.gleam`, find every call to `members.view(palette: ..., members: ...)`. There is typically one in the chat-shell render function. Find with:

```bash
grep -n "members.view" web/src/sunset_web.gleam
```

Update each call to pass the new `on_open_status` callback:

```gleam
members.view(
  palette: palette,
  members: model.members,
  on_open_status: OpenPeerStatusPopover,
)
```

(`OpenPeerStatusPopover` is the message constructor we'll add in Task 7. The line will fail to compile until then; that's expected.)

- [ ] **Step 5: Build to confirm only the expected error remains**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status/web
nix develop --command gleam build 2>&1 | tail -15
```

Expected: error about `OpenPeerStatusPopover` not being defined — that's Task 7. No other errors.

- [ ] **Step 6: Commit (work-in-progress — Task 7 will fix the build)**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
git add web/src/sunset_web/views/members.gleam web/src/sunset_web.gleam
git commit -m "members.gleam: add transport icon + on_open_status click handler"
```

---

## Phase 6: Shell wiring

### Task 6: Add the popover state, ticker, messages, and overlay

**Files:**
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Read existing `Msg`, `Model`, and `init` to know where to insert**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
grep -n "pub type Msg\|pub type Model\|pub fn init\|OpenVoicePopover" web/src/sunset_web.gleam | head -15
```

Note the existing `Msg` enum location and `Model` record definition. We'll add new variants and fields next to them.

- [ ] **Step 2: Add the new message variants**

In `web/src/sunset_web.gleam`, find the `pub type Msg {` block and add three new variants alongside `OpenVoicePopover`:

```gleam
  OpenPeerStatusPopover(domain.MemberId)
  ClosePeerStatusPopover
  Tick(Int)
```

- [ ] **Step 3: Add new `Model` fields**

Find the `pub type Model { Model(...) }` block and add two fields:

```gleam
  /// Currently-open peer status popover, if any. `Some(member_id)`
  /// when open, `None` when closed.
  peer_status_popover: option.Option(domain.MemberId),
  /// Wall-clock unix-ms snapshot. Updated every second by the
  /// `Tick(now_ms)` message so the popover's age readout stays live.
  now_ms: Int,
```

- [ ] **Step 4: Initialize the new fields in `init`**

Find the `Model(...)` construction in `init` (where `members: []` is set) and add the new fields:

```gleam
  peer_status_popover: option.None,
  now_ms: 0,
```

- [ ] **Step 5: Add a ticker effect to `init`**

In `init`, find where the existing effects (`bootstrap`, `subscribe_*`) are defined and add a ticker effect:

```gleam
  let ticker_eff =
    effect.from(fn(dispatch) {
      sunset.set_interval_ms(1000, fn() { dispatch(Tick(now_ms_now())) })
    })
```

`now_ms_now` is a small helper we'll add at the bottom of the file:

```gleam
@external(javascript, "./sunset_web/sunset.ffi.mjs", "nowMs")
fn now_ms_now() -> Int
```

Add the corresponding JS helper to `web/src/sunset_web/sunset.ffi.mjs`:

```javascript
export function nowMs() {
  return Date.now();
}
```

Then include `ticker_eff` in the `effect.batch([...])` call where the bootstrap effects are combined. Find with:

```bash
grep -n "effect.batch" web/src/sunset_web.gleam
```

Add `ticker_eff` to the appropriate batch. There's a top-level `effect.batch` near the end of `init` that combines all the startup effects.

- [ ] **Step 6: Add reducers for the new messages**

In the `update` function (the `case msg` block), add three new arms next to `OpenVoicePopover`:

```gleam
    OpenPeerStatusPopover(member_id) -> #(
      Model(..model, peer_status_popover: option.Some(member_id)),
      effect.none(),
    )
    ClosePeerStatusPopover -> #(
      Model(..model, peer_status_popover: option.None),
      effect.none(),
    )
    Tick(now) -> #(
      Model(..model, now_ms: now),
      effect.none(),
    )
```

Also: in the `MembersUpdated` arm, after updating `model.members`, drop the popover if its target member is no longer present. Find the existing arm:

```gleam
    MembersUpdated(ms) -> #(Model(..model, members: ms), effect.none())
```

Replace with:

```gleam
    MembersUpdated(ms) -> {
      // If the open popover's target left, close it.
      let next_popover = case model.peer_status_popover {
        option.None -> option.None
        option.Some(target) ->
          case list.find(ms, fn(m) { m.id == target }) {
            Ok(_) -> option.Some(target)
            Error(_) -> option.None
          }
      }
      #(
        Model(..model, members: ms, peer_status_popover: next_popover),
        effect.none(),
      )
    }
```

- [ ] **Step 7: Add the popover overlay and import the view**

At the top of `web/src/sunset_web.gleam`, add:

```gleam
import sunset_web/views/peer_status_popover
```

In the shell's render function, find the existing `voice_popover_overlay(palette, model)` call and add a sibling:

```gleam
    voice_popover_overlay(palette, model),
    peer_status_popover_overlay(palette, model),
```

Then define the new function near `voice_popover_overlay`:

```gleam
fn peer_status_popover_overlay(palette, model: Model) -> Element(Msg) {
  case model.peer_status_popover {
    option.None -> element.fragment([])
    option.Some(member_id) ->
      case list.find(model.members, fn(m) { m.id == member_id }) {
        Error(_) -> element.fragment([])
        Ok(m) -> {
          let placement = case model.viewport {
            domain.Phone -> peer_status_popover.InSheet
            _ -> peer_status_popover.Floating
          }
          peer_status_popover.view(
            palette: palette,
            member: m,
            now_ms: model.now_ms,
            placement: placement,
            on_close: ClosePeerStatusPopover,
          )
        }
      }
  }
}
```

- [ ] **Step 8: Build to confirm clean**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status/web
nix develop --command gleam build 2>&1 | tail -15
```

Expected: clean.

- [ ] **Step 9: Run all Gleam tests**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status/web
nix develop --command gleam test 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 10: Commit**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
git add web/src/sunset_web.gleam web/src/sunset_web/sunset.ffi.mjs
git commit -m "Shell: peer status popover state, ticker, overlay"
```

---

## Phase 7: Playwright e2e

### Task 7: Add e2e test covering the popover open/close + age tick

**Files:**
- Create: `web/e2e/peer_status_popover.spec.js`

- [ ] **Step 1: Write the test file**

Create `web/e2e/peer_status_popover.spec.js`. Model after `kill_relay.spec.js` for the relay-spawn boilerplate:

```javascript
// Acceptance test for the peer status popover.
//
// Two browser contexts join the same room via a real relay. Each one
// publishes presence heartbeats. We click peer B's row in page A, assert
// the popover shows the expected transport label ("Via relay" since both
// go through the relay), and that the heartbeat-age readout matches the
// pattern "just now" / "Ns ago". After waiting ~3s we assert the age
// string changed (popover ticker keeps it live). Finally we click the
// close button and assert the popover is gone.

import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

let relayProcess = null;
let relayAddress = null;
let relayDataDir = null;

test.beforeAll(async () => {
  relayDataDir = mkdtempSync(join(tmpdir(), "sunset-relay-peerstatus-"));

  const configPath = join(relayDataDir, "relay.toml");
  const fs = await import("fs/promises");
  await fs.writeFile(
    configPath,
    [
      `listen_addr = "127.0.0.1:0"`,
      `data_dir = "${relayDataDir}"`,
      `interest_filter = "all"`,
      `identity_secret = "auto"`,
      `peers = []`,
      "",
    ].join("\n"),
  );

  relayProcess = spawn("sunset-relay", ["--config", configPath], {
    stdio: ["ignore", "pipe", "pipe"],
  });

  relayAddress = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("relay didn't print address banner within 15s")),
      15_000,
    );
    let buffer = "";
    relayProcess.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      const m = buffer.match(/address:\s+(ws:\/\/[^\s]+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    relayProcess.stderr.on("data", (chunk) => {
      process.stderr.write(`[relay] ${chunk}`);
    });
    relayProcess.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    relayProcess.on("exit", (code) => {
      if (code !== null && code !== 0) {
        clearTimeout(timer);
        reject(new Error(`relay exited prematurely with code ${code}`));
      }
    });
  });
});

test.afterAll(async () => {
  if (relayProcess && relayProcess.exitCode === null) {
    relayProcess.kill("SIGTERM");
  }
  if (relayDataDir) {
    rmSync(relayDataDir, { recursive: true, force: true });
  }
});

test.setTimeout(60_000);
test("clicking a peer row opens the status popover with live age", async ({ browser }) => {
  // Use a faster presence cadence so the test sees a heartbeat and an
  // age tick within the test timeout.
  const url = `/?relay=${encodeURIComponent(relayAddress)}` +
              `&presence_interval=2000` +
              `&presence_ttl=10000` +
              `&presence_refresh=1000` +
              `#sunset-peerstatus`;

  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const pageA = await ctxA.newPage();
  const pageB = await ctxB.newPage();

  for (const [name, page] of [["A", pageA], ["B", pageB]]) {
    page.on("pageerror", (err) =>
      process.stderr.write(`[${name} pageerror] ${err.stack || err}\n`),
    );
    page.on("console", (msg) => {
      if (msg.type() === "error") {
        process.stderr.write(`[${name} console] ${msg.text()}\n`);
      }
    });
  }

  await pageA.goto(url);
  await pageB.goto(url);

  // Wait for the chat shell + at least one non-self member row on page A.
  await expect(pageA.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });
  await expect(pageB.getByText("sunset", { exact: true })).toBeVisible({
    timeout: 15_000,
  });

  // Page A should eventually see TWO member rows (self + B).
  await expect(pageA.locator('[data-testid="member-row"]')).toHaveCount(2, {
    timeout: 20_000,
  });

  // Identify the non-self row (the second one, since self is sorted first).
  const otherRow = pageA.locator('[data-testid="member-row"]').nth(1);

  // Click it to open the popover.
  await otherRow.click();

  const popover = pageA.locator('[data-testid="peer-status-popover"]');
  await expect(popover).toBeVisible({ timeout: 5_000 });

  // Should show "Via relay" since both browsers go through the relay.
  await expect(popover).toContainText("Via relay");

  // Should show an age string.
  const initialText = await popover.textContent();
  expect(initialText).toMatch(/heard from (just now|\d+s ago|\d+m ago)/);

  // Wait ~3 seconds; the age readout should change (popover ticker
  // bumps `now_ms` every 1s and re-renders).
  await new Promise((r) => setTimeout(r, 3500));
  const tickedText = await popover.textContent();
  expect(tickedText).not.toBe(initialText);

  // Close button works.
  await pageA.locator('[data-testid="peer-status-popover-close"]').click();
  await expect(popover).toHaveCount(0);
});
```

- [ ] **Step 2: Run the test**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
nix run .#web-test -- peer_status_popover.spec.js --project=chromium 2>&1 | tail -25
```

Expected: pass in under a minute.

- [ ] **Step 3: Run the entire playwright suite to ensure no regressions**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
nix run .#web-test -- --project=chromium 2>&1 | tail -10
```

Expected: all pass (any pre-existing flake — e.g. `presence.spec.js:196` — is unrelated; re-run that one in isolation if it fails).

- [ ] **Step 4: Commit**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
git add web/e2e/peer_status_popover.spec.js
git commit -m "Playwright: peer status popover open/close + age tick"
```

---

## Phase 8: Final sweep

### Task 8: Lint, format, full workspace tests

- [ ] **Step 1: Workspace tests**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
nix develop --command cargo test --workspace --all-features 2>&1 | grep -E "test result|FAILED" | head -30
```

Expected: all green.

- [ ] **Step 2: Clippy**

```bash
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 3: Format check (Rust)**

```bash
nix develop --command cargo fmt --all --check 2>&1 | grep "^Diff in" | awk -F: '{print $1}' | sort -u
```

If any files this branch touched (`crates/sunset-web-wasm/src/members.rs`) appear, run:

```bash
nix develop --command rustfmt --edition 2024 crates/sunset-web-wasm/src/members.rs
git add crates/sunset-web-wasm/src/members.rs
```

Don't reformat files this branch did not touch.

- [ ] **Step 4: Format check (Gleam)**

```bash
cd web && nix develop --command gleam format --check src/ test/ 2>&1 | tail -10
```

If anything's misformatted in this branch's files, run `gleam format src/ test/` and amend.

- [ ] **Step 5: Final commit if cleanup happened**

```bash
cd /home/nicolas/src/sunset/.worktrees/peer-status
git status
# if there are any pending format-only changes:
git commit -m "Lint and format cleanup"
```

---

## Self-review

**Spec coverage:**

| Spec section | Implemented in |
|---|---|
| `MemberJs.last_heartbeat_ms: Option<u64>` field + getter | Task 1 |
| `derive_members` populates `last_heartbeat_ms` | Task 1 |
| `members_signature` ignores the timestamp | Task 1 (existing signature unchanged; tested) |
| Gleam FFI: `mem_last_heartbeat_ms` | Task 2 |
| `set_interval_ms` ticker FFI | Task 2 |
| `domain.Member.last_heartbeat_ms` | Task 3 |
| `map_members` populates the field | Task 3 |
| `connection_mode` → `RelayStatus` mapping | Already exists; documented in Task 3's notes (no change needed) |
| `peer_status_popover.gleam` view (transport label, age, short pubkey) | Task 4 |
| Humanization rules (just now / Ns / Nm / Nh / never) | Task 4 + tests |
| `Floating` (desktop) / `InSheet` (mobile) placements | Task 4 |
| Inline transport icon next to member name | Task 5 |
| Click handler on member row dispatches Open message | Task 5 |
| Self row not clickable | Task 5 (`click_attrs = []` when `m.you`) |
| `Model.peer_status_popover` + `Model.now_ms` + Open/Close/Tick messages | Task 6 |
| 1Hz ticker effect | Task 6 |
| Popover overlay in shell render | Task 6 |
| Popover auto-closes when target leaves | Task 6 (in `MembersUpdated` arm) |
| Playwright e2e | Task 7 |
| Final lint/format | Task 8 |

**Placeholder scan:** none. Every step contains the actual code or command.

**Type consistency:** `last_heartbeat_ms: Option<u64>` (Rust) → JS `null | number` → Gleam `option.Option(Int)` → `domain.Member.last_heartbeat_ms` → `peer_status_popover.humanize_age(now_ms: Int, last_ms: option.Option(Int))`. Consistent end to end.

**Known caveats for the implementer:**
- `domain.OneHop` is a unit constructor (no String argument). The existing `connection_mode_to_relay` already maps `"via_relay" → domain.OneHop` correctly.
- `voice_popover.gleam` uses hardcoded fixed positioning (`top: 120px; left: 540px`); we mirror that with `top: 120px; right: 260px` so the popover floats next to the members rail. No anchor-rect capture needed in v1.
- The bucket-staleness behavior in the spec's data-flow walkthrough is acknowledged: the popover age string can lag the truth by up to one membership-tracker emit (default `refresh_ms` = 5s) when the displayed timestamp itself goes stale, even though the rendered age string ticks every second. Documented in the spec; do not "fix" it in this plan.
- The Gleam record-update syntax `Model(..model, foo: bar)` is the standard form. If you encounter a compiler error about field order or labels, copy from the existing `OpenVoicePopover` arm verbatim.
- `event.on_click(some_msg)` requires the message to already be a value; the `on_open` callback in `member_row` returns the message constructed from `m.id`, which is then passed directly to `event.on_click`.
