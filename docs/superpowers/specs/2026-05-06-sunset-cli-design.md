---
title: sunset-cli — native ratatui chat client
date: 2026-05-06
status: draft
---

# sunset-cli: native ratatui chat client

## Goal

Add a new crate `sunset-cli` that lets a user send and receive sunset.chat
messages from a terminal in the same way the web client does today.
Connection to the relay happens over WebSocket or WebTransport, the user
picks at startup. The interface is a `ratatui` app with a clean,
minimal layout: room list, peer list, relay/connection panel, message
log, and a command-driven composer (`/help`, `/join`, …).

## What this PR ships (v1)

1. New workspace crate `sunset-cli` exposing a `sunset-cli` binary.
2. Connect to a relay via `--relay <url>` where the URL accepts:
   - `wss://host:port` / `ws://host:port` — straight WebSocket.
   - `wts://host[:port]` / `wt://host[:port]` — WebTransport with WS
     fallback (matches the web client's primary transport behavior).
   - hostname-only (`relay.sunset.chat`) — resolved to the relay's
     descriptor over HTTPS, mirroring the web client's
     `Connectable::Resolving` path.
3. Identity: load-or-generate a 32-byte Ed25519 secret seed from
   `~/.config/sunset/identity.bin` (or `$SUNSET_IDENTITY_PATH`). Same
   semantics as the relay's `identity::load_or_generate`.
4. Store: `MemoryStore` with the standard `Ed25519Verifier` — same
   choice the web client makes today. Persistent storage is out of
   scope for v1 (`FsStore` is a follow-up; the web client doesn't
   persist either, so parity holds).
5. ratatui TUI with the layout sketched below.
6. `/command` parser dispatching to chat / room / peer / relay /
   identity operations.
7. Headless integration test that spins up an in-process
   `sunset-relay` (via `Relay::run_for_test`) and two CLI clients,
   verifies they exchange a message round-trip, observe each other in
   the peer list, and surface the right `connection_mode` ("via_relay"
   in this setup — there is no native WebRTC, so peers never upgrade
   to "direct" via the CLI).

## Out of scope for v1 (deferred to follow-up plans)

**Voice (`/voice` is a stub in v1).** Native voice cannot reuse the
web client's WebRTC datachannel path because there is no
`sunset-sync-webrtc-native` crate. Two options exist for a later plan:

- (a) Build a `sunset-sync-webrtc-native` crate, mirroring the
  browser's `WebRtcRawTransport` shape, and integrate it the same way
  `sunset-web-wasm/src/client.rs` does.
- (b) Voice-over-relay: native audio I/O via `cpal`, with voice frames
  flowing over the existing primary transport (WS/WT → relay → other
  peers) instead of a direct datachannel. Higher latency than the
  browser path; useful as an interim.

Either approach warrants its own design + plan. For v1, `/voice`
prints a single-line message pointing the user at the web client.

**Persistence (`FsStore`)** — out of scope. `MemoryStore` matches the
web client. Adding persistence is a separate plan.

**Federation between two CLI users without a relay** — out of scope.
Same constraint as the web client (no direct WebRTC native), and the
user's request explicitly mentions a relay.

## Architecture

```
sunset-cli (new crate)
├── bin/sunset-cli       # main(); parses CLI args, drives the TUI
├── core/                # host-agnostic Client wrapping sunset-core::Peer
├── ui/                  # ratatui rendering + input loop
└── commands/            # `/command` parser + dispatch
```

The split between `core` and `ui` is what makes integration tests
viable: tests drive `core::Client` directly without paint/input. The
TUI layer is a thin shell that observes `core` state and sends
commands into it.

### Type alias for the engine transport

Mirrors `sunset-web-wasm/src/client.rs::WsT`, just without WebRTC:

```rust
type Transport = FallbackTransport<
    NoiseTransport<WebTransportRawTransport>,    // sunset-sync-webtransport-native
    NoiseTransport<WebSocketRawTransport>,       // sunset-sync-ws-native
>;
type Peer = sunset_core::Peer<MemoryStore, Transport>;
```

`FallbackTransport` already routes by URL scheme: `wt://`/`wts://`
tries WT first then rewrites to `ws://`/`wss://` and falls back;
`ws://`/`wss://` short-circuits to WS. The user types whichever
scheme matches what they want, and FallbackTransport handles the
transport-selection logic. There is no separate `--transport ws|wt`
flag because the URL already encodes that choice.

### Resolver

For hostname-only relay strings, we mirror the relay's
`resolver_adapter::ReqwestFetch`. Since that type is `pub(crate)` in
`sunset-relay`, the CLI re-implements the same 30-line shim
locally. Factoring it out to `sunset-relay-resolver` is an optional
cleanup; the duplication is small enough that it isn't load-bearing.

### Threading model

Single-threaded `LocalSet`, like every other host. The engine is
`?Send`. ratatui's input loop runs on a `tokio::task::spawn_blocking`
that pushes `crossterm::Event`s into an unbounded mpsc channel; the
main `LocalSet` task selects over that channel + per-room callback
signals.

## UI layout

```
┌─sunset.chat─────────────────────────────────────────────────────┐
│ #alpha                                                          │
├──────────────┬──────────────────────────────────────────────────┤
│ Rooms        │ alice  hello                              12:01  │
│ > #alpha     │ bob    hi everyone                         12:01 │
│   #beta      │ alice  what's the plan for today?          12:02 │
│              │ ...                                              │
│ Peers        │                                                  │
│  alice  D    │                                                  │
│  bob    R    │                                                  │
│              │                                                  │
│ Relays       │                                                  │
│ ✓ relay.s..  │                                                  │
│              │                                                  │
├──────────────┴──────────────────────────────────────────────────┤
│ > _                                                              │
└──────────────────────────────────────────────────────────────────┘
```

- **Top bar**: app title + the active room name.
- **Left rail**: rooms (current marked `>`), peers in the active room
  with a one-character connection-mode suffix (`D` direct,
  `R` via_relay, `?` unknown), relays with a state glyph
  (`✓` connected, `…` connecting, `~` backoff).
- **Right pane**: messages in the active room. Self-messages are
  styled differently (slight dim or color difference); time is the
  message's `sent_at_ms` formatted to local time.
- **Bottom bar**: composer. A leading `/` is a command; otherwise the
  line is sent as a chat message into the active room.

The aesthetic stays minimal: ASCII box-drawing, two foreground colors,
no Unicode flourishes beyond the three state glyphs above.

## /commands

| Command | Behavior |
|---|---|
| `/help` | Print the command list to the message log. |
| `/join <room>` | Open `<room>` (Argon2 derivation) and switch to it. Idempotent — re-joining a room is a no-op + switch. |
| `/switch <room>` | Switch active room to one already open. |
| `/leave [room]` | Drop the `OpenRoom` handle for the named room (default: active). Removes it from the rail. |
| `/rooms` | Echo the open-rooms list to the message log. |
| `/peers` | Echo the active room's members + connection mode + display name. |
| `/relays` | Echo each relay intent: label, state, last RTT, peer pubkey if connected. |
| `/relay add <url>` | Add a durable relay intent. |
| `/name <name>` | `Peer::set_self_name`. Persists in-process; the next presence heartbeat carries the new name. |
| `/me` | Print the user's pubkey (hex), the `wss://...#x25519=...` self-address, and the active relay set. |
| `/voice` | Print "voice not yet implemented in CLI; use the web client at https://sunset.chat" — no audio I/O is wired up in v1. |
| `/quit` | Clean shutdown. |

Bare text (no leading `/`) sends a chat message into the active room.
Empty input is a no-op. Unknown commands echo "unknown command — try
`/help`" to the message log.

## State model

`core::Client` owns the long-lived state. UI is a snapshot view.

- `Rc<Peer<MemoryStore, Transport>>` — sunset-core peer.
- `HashMap<String, OpenRoom<...>>` — open rooms keyed by name.
- Per-room `Rc<RefCell<RoomView>>` containing:
  - `Vec<MessageLine>` (decoded messages in order).
  - `Vec<Member>` (latest snapshot from `on_members_changed`).
- Top-level `Rc<RefCell<TopState>>` containing:
  - active room name (`Option<String>`).
  - `Vec<IntentSnapshot>` for the relay rail.
  - `Vec<String>` self-name + identity hex.

Per-room callbacks (`on_message`, `on_members_changed`) push into the
`RoomView` cell and signal the UI loop to re-render via a
`tokio::sync::Notify`. The UI loop coalesces multiple notifications
into one render frame.

## Errors

- Failed `--relay` parse → exit with a clear message (no TUI startup).
- Identity load/generate IO error → exit with a clear message.
- Relay add at runtime that fails to resolve (`Connectable::Resolving`
  parse error) → message log line "relay add failed: …" and the
  intent isn't recorded. Transient failures (unreachable host) get
  recorded as intents in `Backoff` state, mirroring the web client.
- Send failures (store insert errors) → message log line "send
  failed: …" and the composer keeps the typed text so the user can
  retry.

## Tests

### Unit tests

- Command parser: `/help`, `/join foo`, `/relay add wss://x`,
  `/relay add` (missing arg → error), unknown command, bare text.
- UI render snapshot: render a fixed `TopState` + `RoomView` to a
  ratatui `TestBackend` buffer and string-match key cells (room name,
  composer prompt, peer-row glyph). Style-agnostic snapshot using
  `Buffer::area().rows()` text content only.

### Integration tests

These live in `crates/sunset-cli/tests/` and use the headless `core::Client`
without mounting the TUI.

- **roundtrip_ws**: spin up a relay (`Relay::run_for_test`), connect
  two CLI clients via `wss://...` (axum upgrades, no UDP needed), have
  both `/join alpha`, A sends "hello", B's `RoomView` receives it
  within 1 s. Asserts the chat-write → relay → chat-read path on the
  WS transport.
- **roundtrip_wt**: same shape, but the relay binds UDP and the
  clients dial via `wt://...`. Asserts the WT path. Skipped if WT
  init fails on the test host (matches the relay's "WS-only fallback"
  behavior; logged but not failed).
- **peer_visibility**: after both clients join, A's `core::Client`
  exposes B in its `members` snapshot with
  `connection_mode == "via_relay"` (no native WebRTC; direct upgrade
  is impossible).
- **relay_intent_lifecycle**: B's `IntentSnapshot` for the relay
  starts in `Connecting`, transitions to `Connected`, and reports a
  non-zero RTT after the first heartbeat round-trips.
- **command_dispatch**: `/join alpha` opens the room and updates the
  active-room state; `/leave alpha` closes it. Asserted on the
  `core::Client`'s public state, no UI involved.

These tests timeout at 10 seconds each — picked as the UX bar for
"acceptable connect + first heartbeat over loopback". Increasing the
timeout to mask a slow handshake is forbidden (CLAUDE.md debugging
discipline).

### What we are *not* going to test in v1

- Voice — `/voice` is a stub.
- ratatui terminal interaction. TUI input handling is wrapped in a
  thin `TerminalEventLoop` trait so the unit tests of the command
  pipeline don't need a real terminal. Driving an actual terminal in
  CI is high-noise, low-value here; the headless integration tests
  cover correctness of the underlying `core::Client`, which is what
  the user actually relies on.

## Cargo dependencies (new)

- `ratatui` — TUI framework.
- `crossterm` — terminal backend ratatui uses.
- `dirs` — locate `~/.config/sunset/` portably.
- `chrono` — local-timezone formatting for message timestamps.
  (Project already has `web-time` for monotonic + UTC; `chrono` adds
  the strftime + local-tz piece.)
- `tracing-subscriber` — already a workspace dep, used here for the
  optional `--log-level` flag (logs go to stderr; `RUST_LOG` overrides).

All native-only — no `wasm32` target for this crate. The crate is not
added to the wasm-friendly transports list.

`flake.nix` already provides everything `sunset-cli` needs (cargo,
rust toolchain, the same TLS stack `sunset-relay` uses). No flake
changes required.

## Failure modes we accept

- Lossy peer rail under churn: the rail repaints on every
  `on_members_changed`. If two updates arrive within one frame,
  ratatui's redraw consolidates — fine.
- Relay-only voice latency (when voice ships in a follow-up) will be
  worse than the browser's WebRTC path. Documented; not v1's
  problem.
- No mouse support. ratatui supports it via crossterm; not in v1's
  scope.
- No autoscroll-pause. The message log always autoscrolls to the
  newest message. Adding "pause autoscroll on PageUp" is a follow-up.

## Open questions for review

1. Is `MemoryStore` acceptable for v1, or does the user want
   `FsStore` (separate plan, also reasonable)? The web client uses
   `MemoryStore`, so parity argues for the same here.
2. Voice scope: explicit confirmation that deferring `/voice` to a
   follow-up plan is acceptable. The alternative is a multi-week
   detour to build either native WebRTC or a relay-only voice path
   plus cpal audio I/O. Recommendation: defer, ship chat now.
3. `/peers` and `/relays` panel content: ASCII glyphs (`D`/`R`/`?`,
   `✓`/`…`/`~`) — are these acceptable, or does the user want richer
   indicators?
