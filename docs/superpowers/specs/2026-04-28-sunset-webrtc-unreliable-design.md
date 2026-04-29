# sunset-sync-webrtc-browser: Unreliable Datachannel Design

**Date:** 2026-04-28
**Scope:** `crates/sunset-sync-webrtc-browser` only.
**Predecessor:** Bus pub/sub layer (`docs/superpowers/specs/2026-04-28-sunset-bus-pubsub-design.md`) — already merged. `Bus::publish_ephemeral` flows down to `RawConnection::send_unreliable` via `sunset-sync`'s outbound router; today that call returns `Err("not implemented in v1")` in the browser transport, so the path is dead end-to-end.
**Successor:** Voice end-to-end (Plan C, future) wires the microphone capture, Opus framing, and JS↔Rust audio bridge on top of this transport.

## Goal

Make `WebRtcRawConnection::send_unreliable` / `recv_unreliable` actually move bytes between two browsers, so the existing Bus ephemeral path (already wired through `sunset-sync`'s `peer.rs` outbound router) becomes usable in the web client.

## Non-goals

- No voice capture, encoding, jitter buffering, or playback (Plan C).
- No changes to the reliable channel's behaviour, label, framing, or wire format.
- No new `RawTransport` or `RawConnection` API surface — the trait already exposes `send_unreliable` / `recv_unreliable`; we are just removing the stub.
- No native (non-browser) WebRTC transport. Only the `wasm32-unknown-unknown` browser implementation.
- No congestion control, pacing, retransmit policy, sequencing, or loss reporting on top of the SCTP unreliable channel — what the browser delivers is what we deliver.
- No temporary `Bus` surface on `sunset-web-wasm`'s `Client` — the existing `recv_reliable` regression test plus a unit-level open-and-send test cover this layer; full byte-flow validation lands in Plan C alongside the voice plumbing it actually exercises.

## Architecture

### Two named datachannels per peer connection

Each `RtcPeerConnection` carries two `RtcDataChannel`s, distinguished by `label`:

| Label              | Configuration                                           | Purpose                       |
|--------------------|---------------------------------------------------------|-------------------------------|
| `sunset-sync`      | default (ordered, reliable)                             | existing reliable channel     |
| `sunset-sync-unrel`| `ordered: false`, `maxRetransmits: 0`                   | new unreliable / "datagram"-ish |

Two labels (instead of negotiated `id`s) keeps the implementation symmetric with the existing reliable channel — no `negotiated: true` plumbing, no manual ID allocation, the connect side `createDataChannel`s both and the accept side dispatches by label inside `ondatachannel`.

### Open both before returning

Both `connect()` and `accept()` wait until both channels report `open` before returning a `WebRtcRawConnection`. Rationale:

- Callers can issue `send_unreliable` immediately after `connect()` returns without races.
- `sunset-sync`'s engine already assumes the connection is fully wired the moment `RawTransport::connect` resolves; partial readiness would force every caller to retry.
- Cost: one extra open round-trip on top of the existing handshake. WebRTC opens both channels in parallel over the same SCTP association, so this is typically < 50 ms incremental — negligible compared to the ICE/DTLS handshake itself.

If either channel fails to open (peer connection torn down before both report `open`), the whole `connect()`/`accept()` call fails with `Error::Transport`. We do not return a half-functional connection.

### `WebRtcRawConnection` shape

```rust
pub struct WebRtcRawConnection {
    _pc: RtcPeerConnection,
    dc: RtcDataChannel,                                    // reliable, unchanged
    rx: RefCell<mpsc::UnboundedReceiver<Bytes>>,           // reliable inbound
    dc_unrel: RtcDataChannel,                              // NEW: unreliable
    rx_unrel: RefCell<mpsc::UnboundedReceiver<Bytes>>,     // NEW: unreliable inbound
    _on_ice: Closure<...>,
    _on_open: Option<Closure<...>>,                        // reliable open (connect side)
    _on_msg: Option<Closure<...>>,                         // reliable msg (connect side)
    _on_open_unrel: Option<Closure<...>>,                  // NEW: unreliable open (connect side)
    _on_msg_unrel: Option<Closure<...>>,                   // NEW: unreliable msg (connect side)
    _on_dc: Option<Closure<...>>,                          // accept side, dispatches both labels
}
```

`send_unreliable` / `recv_unreliable` mirror the reliable implementations but route through `dc_unrel` and `rx_unrel`.

### Connect side (`RawTransport::connect`)

Today the connect side creates one channel:

```rust
let dc = pc.create_data_channel_with_data_channel_dict("sunset-sync", &dc_init);
```

Plan A change: create two, install onopen/onmessage on each, wait for both to fire `open`:

```rust
let dc_init_rel = RtcDataChannelInit::new();
let dc = pc.create_data_channel_with_data_channel_dict("sunset-sync", &dc_init_rel);
dc.set_binary_type(RtcDataChannelType::Arraybuffer);

let dc_init_unrel = RtcDataChannelInit::new();
dc_init_unrel.set_ordered(false);
dc_init_unrel.set_max_retransmits(0);
let dc_unrel = pc.create_data_channel_with_data_channel_dict("sunset-sync-unrel", &dc_init_unrel);
dc_unrel.set_binary_type(RtcDataChannelType::Arraybuffer);

// Two oneshots, two msg channels, two onopen/onmessage closures.
let (open_tx_rel, open_rx_rel) = oneshot::channel::<()>();
let (open_tx_unrel, open_rx_unrel) = oneshot::channel::<()>();
let (msg_tx_rel, msg_rx_rel) = mpsc::unbounded::<Bytes>();
let (msg_tx_unrel, msg_rx_unrel) = mpsc::unbounded::<Bytes>();
// install closures …

// Wait for both. The existing select! loop driving Answer/ICE keeps
// running until BOTH open futures resolve.
```

The select-loop change: instead of `_ = open_fut.as_mut() => break`, track two booleans and `break` once both open futures have resolved.

### Accept side (`run_accept_one`)

Today the accept side has a single `ondatachannel` handler that captures the inbound `RtcDataChannel` and a single open oneshot. Plan A change: the handler dispatches by `dc.label()`:

```rust
let on_dc = Closure::<dyn FnMut(RtcDataChannelEvent)>::new(move |ev: RtcDataChannelEvent| {
    let dc = ev.channel();
    dc.set_binary_type(RtcDataChannelType::Arraybuffer);
    match dc.label().as_str() {
        "sunset-sync" => {
            // wire onopen → open_tx_rel, onmessage → msg_tx_rel
            // store dc in dc_tx_rel oneshot
        }
        "sunset-sync-unrel" => {
            // wire onopen → open_tx_unrel, onmessage → msg_tx_unrel
            // store dc in dc_tx_unrel oneshot
        }
        other => {
            // Unknown label — log via web_sys::console::warn and drop.
            // We don't error the handshake because future protocol versions
            // may add channels and we want graceful peer-version skew.
        }
    }
});
```

The select-loop change mirrors the connect side: track two `dc_opt`s and two `open_fut`s, only `break` once both channels are present and both have opened.

## Failure modes

| Scenario                                        | Outcome                                                                                  |
|-------------------------------------------------|------------------------------------------------------------------------------------------|
| Unreliable channel never opens                  | `connect()`/`accept()` returns `Error::Transport("…unrel open…")`. No half-conn returned.|
| Reliable opens, unreliable later closes         | `recv_unreliable` returns `Error::Transport("dc closed")`. Reliable channel keeps working. `sunset-sync`'s peer task already drops unreliable send failures silently (commit `a3e18e1`), so a writer hitting a closed unreliable channel does not tear down the peer. |
| Inbound message on unknown label                | Logged via `console.warn`, dropped. Handshake continues.                                  |
| Peer is on a pre-Plan-A build (no second label) | `connect`/`accept` will hang waiting for the second `open`/`ondatachannel`. **Acceptable for v1** — no peers are running in production yet; web peers will all upgrade together. Documented in code; revisit if/when we ship to users. |
| Browser doesn't support `set_max_retransmits`   | Already part of the `web-sys` `RtcDataChannelInit` API; supported in all evergreen browsers. Not a runtime concern. |
| Send when send-buffer full (back-pressure)      | `dc.send_with_u8_array` returns `Err`; we propagate as `Error::Transport`. Sender is responsible for not flooding. (Voice will pace by frame interval.) |

## Testing strategy

This crate's existing tests live in `crates/sunset-sync-webrtc-browser/tests/` and run under `wasm-bindgen-test` in headless Chromium via `wasm-pack test --chrome --headless`. Plan A adds:

### 1. Unit-ish: both channels open

A wasm-bindgen-test that wires up two `WebRtcRawTransport`s in the same JS context (same as the existing connect/accept regression tests), runs `connect`/`accept` to completion, and asserts both `dc.ready_state()` and `dc_unrel.ready_state()` report `Open`. This is the proof that the second channel actually negotiates, separately from any byte-level flow.

### 2. Round-trip: bytes flow on the unreliable channel

A wasm-bindgen-test that, after both peers connect, sends a small payload (e.g. `b"hello-unrel"`) via `send_unreliable` and asserts `recv_unreliable` returns it. **Loss is allowed in production** but on a loopback PeerConnection in a single browser tab, a single small datagram should arrive — if it doesn't, something is structurally wrong with the wiring. We accept that this test is slightly probabilistic in pathological conditions and rely on retry-with-timeout if it ever flakes.

### 3. Regression: reliable still works after Plan A

The existing `connect_accept_round_trip` test (or equivalent) must continue passing without modification. If it requires changes to the public API or test wiring, that itself is a red flag and the reliable channel's behaviour has drifted.

### Out of scope for this plan's tests

- Bus-level end-to-end (`Bus::publish_ephemeral` on Alice → `Bus::subscribe` on Bob via the browser transport). The Bus path itself is already covered by `crates/sunset-core/tests/bus_integration.rs` against the in-process test transport. Wiring the browser transport into a Bus integration test would require either spawning a relay + two browsers (overkill for this plan) or stubbing the signaler in a wasm-bindgen-test (duplicates the existing connect/accept regression scaffolding for no marginal coverage). Plan C will exercise the full path naturally via the voice flow.

## Out of scope

- Native WebRTC transport (`webrtc-rs` or similar) — no native client uses voice today.
- Reliable-channel re-architecture (e.g. negotiated channels, single SCTP stream multiplexing) — not needed.
- Removing the stale `Err("not implemented in v1")` error message anywhere outside `wasm.rs` — `grep` will be done as part of the implementation plan to make sure no tests or docs still reference "v1 unreliable stub".
- Bus or `Client` surface changes — `Client` already exposes the Bus indirectly; once this transport works, voice work in Plan C plugs in without further FFI changes.

## Risks

1. **Both-open semantics introduce a new failure mode.** A peer that crashes between reliable-open and unreliable-open will block the connect/accept call indefinitely (no timeout). Mitigation: rely on the existing engine-level peer eviction that already drops dead peers, and document the behaviour. If this becomes a real problem we add a per-channel open timeout in a follow-up.
2. **Label-based dispatch is silently fragile.** If someone adds a third channel with a typo'd label, traffic disappears into the `console.warn` branch with no test failure. Mitigation: the `match` is exhaustive across known labels and the `other` arm logs; no silent drops without an operator-visible signal.
3. **Closure leak shape gets messier.** The accept side already leaks closures via `forget()` inside `ondatachannel` (page lifetime). Doubling the closures doubles the leak, but the count is still O(peers), not O(messages). Acceptable.

## Review summary

After writing, the spec was self-reviewed for:

- **Placeholders:** none ("v1" appears only in the title of the existing stubbed error message we are *removing*, not as a forward-looking marker).
- **Internal consistency:** the failure-mode table aligns with the testing strategy and the architecture section. The sunset-sync peer task already drops unreliable send failures silently per commit `a3e18e1`, which is what makes "reliable opens, unreliable later closes" non-fatal.
- **Scope:** strictly bounded to `crates/sunset-sync-webrtc-browser`. No other crate is modified.
- **Ambiguity:** "datagram-ish" in the table is loose — clarified inline that we mean SCTP unordered + 0 retransmits, not literal UDP datagrams (browser SCTP still chunks/reassembles each `send`).
