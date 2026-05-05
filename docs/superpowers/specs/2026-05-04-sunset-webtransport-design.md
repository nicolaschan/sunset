# sunset-webtransport — design

Date: 2026-05-04 · Author: Claude Opus 4.7 · Status: proposed

## Summary

Add a new transport family — **WebTransport over HTTP/3** — that the relay accepts inbound and that browsers + native peers use as the **primary path** to the relay, with WebSocket as the **fallback** when WebTransport fails (cert-pinning failure, UDP blocked, browser doesn't support WT, etc).

WebTransport gives us two things WebSocket cannot:
1. **QUIC datagrams** — true unreliable delivery for voice frames the relay routes between peers when WebRTC P2P fails.
2. **Faster handshakes** — 0-RTT/1-RTT QUIC vs. TCP+TLS+WS.

Reliable + unreliable channels both terminate at the relay; the same transport is used for relay-to-relay federation as well.

## Why now

Two driving requirements:
- Voice over the relay. Today `Bus::publish_ephemeral` requires `send_unreliable` on the peer-to-relay link, but the WebSocket transport returns `Err("websocket: unreliable channel unsupported")`. WebRTC handles unreliable peer-to-peer, but when WebRTC P2P fails (NAT, firewall) we have no fallback for voice. WT gives us peer→relay datagrams.
- Production hardening. WT is the modern answer; relays without WT keep working, browsers without WT keep working — fallback covers everyone.

## Layered model (unchanged)

WebTransport plugs in at the same layer as WebSocket and WebRTC: it implements `sunset_sync::RawTransport` + `RawConnection`, gets wrapped by `NoiseTransport`, and feeds `MultiTransport`/`SyncEngine` like any other transport. **No engine changes.**

```
Application
    ↓
Sync (SyncEngine)
    ↓
MultiTransport<Primary, WebRTC>            ← Primary slot is now FallbackTransport<WT, WS>
    ↓
NoiseTransport<RawTransport>
    ↓
RawTransport ∈ { WebTransport, WebSocket, WebRTC }
```

## Crates

Two new crates, parallel to the existing WS / WebRTC ones:

- **`sunset-sync-webtransport-native`** — client + server-accept (uses `wtransport` ≈ quinn + h3). Used by the relay for inbound and for relay-to-relay outbound, and by future native CLI peers.
- **`sunset-sync-webtransport-browser`** — WASM client (uses `web_sys::WebTransport`). Dial-only, like the WebSocket browser crate.

Plus one new module in `sunset-sync` (or its own crate, decided in plan): **`FallbackTransport<Primary, Fallback>`** — a generic `Transport` adapter that tries the primary on `connect`, on connection failure (or after a 3 s deadline) falls back to the secondary, and surfaces `accept` from the primary only (relays never accept fallback connections, only initiate them; the server-side WS path comes through its own listener directly).

## Wire layout

WebTransport gives us:
- **Bidirectional QUIC streams** — byte streams (not message streams). We open exactly **one** persistent bidirectional stream at session start. Frame messages with a 4-byte big-endian length prefix per `SyncMessage`. This is the `reliable` channel.
- **QUIC datagrams** — message-shaped, may be lost/reordered. Each datagram = one message. This is the `unreliable` channel. Datagram size is bounded by path MTU; we set a hard ceiling of 1200 bytes (matches WebRTC datachannel SCTP's MTU and what we already use for voice frames in `sunset-voice`).

Why a single persistent bidi stream rather than one stream per message: WebTransport stream open is cheap but not free (head-of-line blocking is per-stream in QUIC), and the existing transports (WS, WebRTC datachannel) all expose a single ordered byte pipe. A single stream makes WT semantically identical to WS for the engine, with datagrams as the bonus side-channel.

## URL schemes & address descriptors

- **`wt://host[:port]`** — plain WebTransport (insecure, only for tests / loopback). HTTP/3 still requires TLS underneath, but a self-signed cert with pinning makes this practical.
- **`wts://host[:port]`** — WebTransport with WebPKI cert.

The pinned-cert hash is carried as a URL fragment alongside `x25519=`, e.g. `wt://127.0.0.1:8443#x25519=<hex>&cert-sha256=<hex>`. (Fragment chosen because the existing `WebSocketRawTransport::connect` already strips fragments before dialing, and the resolver writes addresses in this style.)

The relay's `GET /` identity descriptor JSON gains an optional field:

```json
{
  "ed25519": "...",
  "x25519": "...",
  "address": "ws://relay.example#x25519=...",       // existing — WS fallback
  "webtransport_address": "wt://relay.example#x25519=...&cert-sha256=..."  // new
}
```

Old clients ignore the new field and connect via `address` (WS) — backward compatible.

## Fallback strategy

`FallbackTransport<P, F>` (where `P, F: Transport`):

```rust
async fn connect(&self, addr: PeerAddr) -> Result<MultiConnection<P::Conn, F::Conn>> {
    // 1. If the addr starts with the primary's scheme, try primary with
    //    a bounded deadline (default 3 s). If it succeeds, return Primary.
    // 2. If primary fails (any reason — connect refused, cert mismatch,
    //    deadline) AND a fallback URL is available, try the fallback.
    // 3. If fallback also fails, surface the *primary*'s error
    //    (more diagnostic) plus a "fallback also failed: ..." note.
}
```

The fallback URL is derived from the primary URL by scheme rewrite: `wt://` → `ws://`, `wts://` → `wss://`. Same host, same port. (The relay will be configured to listen on the *same* port for both UDP/QUIC and TCP/WS — they don't conflict.) When the descriptor only provides a `ws://` address, the resolver doesn't synthesize a `wt://` address; only WS is attempted.

## Relay changes

The relay grows a second listener:

```rust
let ws_listener = TcpListener::bind(config.listen_addr).await?;          // unchanged
let wt_endpoint = WebTransportEndpoint::server(...).bind(config.listen_addr).await?;  // new — UDP, same port
```

Dual-stack on one port works because UDP and TCP have separate socket spaces.

Self-signed cert generation lives in a new `cert.rs` module:
- ECDSA-P256 key (browser cert-pinning constraint)
- Validity = 13 days (Chrome's `serverCertificateHashes` requires ≤14)
- SAN includes the configured listen host + `127.0.0.1` + `localhost`
- Cert + key persisted to `<data_dir>/wt-cert.pem` + `wt-key.pem`; rotated automatically when within 24 h of expiry on startup

The cert SPKI is hashed (SHA-256), the hex hash is exposed in the identity descriptor's `webtransport_address`. Production relays behind a CA-issued cert can set `webtransport_address` to a `wts://...` URL with no `cert-sha256=` fragment — browsers accept it via the normal CA chain.

Inbound WT plumbing mirrors WS:
- `WebTransportRawTransport::serving()` returns `(transport, accept_tx)`
- A wtransport `Endpoint::accept()` loop in the relay's startup pushes accepted sessions into `accept_tx`
- A `SpawningAcceptor` wraps it for Noise IK (same shape as `WebSocketRawTransport` already does)

Config additions (TOML):
```toml
listen_addr = "0.0.0.0:8443"        # existing — TCP for HTTP/WS
webtransport = "auto"                # new — "auto" / "off" / cert path config
                                     # "auto" = bind UDP on same port, generate self-signed
```

## Browser plumbing

`sunset-sync-webtransport-browser` shape mirrors the existing `sunset-sync-ws-browser`:

```rust
pub struct WebTransportRawTransport;
pub struct WebTransportRawConnection {
    session: WebTransport,                  // web_sys::WebTransport
    bidi_writer: WritableStreamDefaultWriter,
    bidi_reader: ReadableStreamDefaultReader,
    datagram_writer: WritableStreamDefaultWriter,
    datagram_reader: ReadableStreamDefaultReader,
    // ... lifecycle hooks like ws-browser does
}
```

Cert pinning: the WT constructor receives `serverCertificateHashes` — the SHA-256 hash extracted from the URL fragment.

`Client::new` swaps the primary half of MultiTransport:
```rust
let ws_raw = WebSocketRawTransport::dial_only();
let wt_raw = WebTransportRawTransport::dial_only();
let primary = FallbackTransport::new(
    NoiseTransport::new(wt_raw, identity.clone()),
    NoiseTransport::new(ws_raw, identity.clone()),
);
let multi = MultiTransport::new(primary, rtc);
```

(In wasm, `FallbackTransport` is `?Send` like everything else.)

The browser-side resolver gains a new arm: when the descriptor JSON includes `webtransport_address`, return that as the canonical address; otherwise return `address` as today. (Existing `Connectable::Resolving` flow unchanged at the supervisor level.)

## Native plumbing

Symmetric: the relay uses `wtransport` for both server-accept and outbound dial when peering with another relay. The same `FallbackTransport` wrapper applies on the dial side. `sunset-sync-webtransport-native::WebTransportRawTransport` exposes both `dial_only()` and `serving()` constructors.

## Error & lifecycle handling

Borrowing the discipline already encoded in `sunset-sync-ws-browser/src/wasm.rs` (close-detection via `close_rx`, JS-callback drop-safety):

- `WebTransport.closed` promise (browser) → fires `close_rx` so `recv_*` can return `Err` immediately rather than hanging forever.
- Native `wtransport::Connection::closed()` / SessionConnection state similarly drains a close channel.
- Drop impl detaches all event listeners before dropping closures (browser) / aborts task before dropping handles (native).
- Datagrams are best-effort: send returns `Ok(())` even if the datagram is dropped on the wire, per `RawConnection`'s docs ("Transports that don't support unreliable should return `Err`; the per-peer task drops failed unreliable sends silently"). Native `wtransport::Connection::send_datagram` errors only when the session is dead; we surface those.
- Browser: WT datagram size limit is enforced before send; oversized datagrams return `Err` rather than silently truncating.

## Testing strategy

### Unit tests
- Each new crate: round-trip on reliable + unreliable channels via in-process server (native) / mock fixture (browser).
- Cert generator: produced cert validates with the same SPKI hash; expiry checked; SAN entries present.
- Length-framing: pathological-size messages, partial reads.
- `FallbackTransport`: primary-success returns Primary; primary-fail-fallback-success returns Fallback; both-fail surfaces primary error.

### Integration tests
- `sunset-relay/tests/webtransport_e2e.rs`: start a relay, native WT client connects, send reliable + unreliable, assert receive.
- `sunset-relay/tests/wt_fallback_to_ws.rs`: start a relay with WT cert mangled (or UDP blocked), native client tries WT then WS, assert WS path wins and engine still syncs.

### Playwright e2e
- `web/e2e/webtransport_relay.spec.js` — browser opens relay UI, the test reads the relay descriptor's `webtransport_address`, asserts the `Client` connects via WT (asserted via a new test-hook `Client.transport_kind_for(intent_id)`), and that a chat message round-trips.
- `web/e2e/webtransport_unreliable.spec.js` — two browsers, WebRTC disabled (forced to relay), assert voice frames arrive via WT datagrams (per-peer frame counter via existing `voice_recorded_frames` hook).
- `web/e2e/webtransport_fallback.spec.js` — relay started with WT broken (stale cert hash advertised), browser must fall back to WS, chat still works.

### Cert pinning in tests
The relay prints both `wt-cert.pem` path and the SHA-256 hash to its startup banner; tests parse the banner. Playwright's chromium does not need extra flags — `serverCertificateHashes` is the documented WT API for self-signed cert pinning.

## What this design does **not** include (deferred)

- Routing voice frames through the relay end-to-end (the wire path is in scope; the application-layer relay-as-TURN logic is a follow-up plan).
- Supervisor-level transport selection telemetry / UI (the existing `TransportKind::{Primary, Secondary, Unknown}` already lets us label a connection; surfacing WT-vs-WS in the peer-status pill is a follow-up).
- Cross-relay federation tests via WT (the wire works; the e2e is deferred).

## Risks & mitigations

- **`wtransport` maturity.** Has had recent (2025) releases, used in production by some Rust shops. We pin a specific version; fallback to WS means a broken WT path doesn't take down the relay.
- **Cert pinning expiry.** Auto-rotation 24 h before expiry. If a relay is down for >14 days, browsers cached against the old hash will fail-open to WS.
- **CI UDP support.** GitHub Actions runners allow UDP on loopback; tests bind 127.0.0.1 so this is fine.
- **Browser support.** Chromium / Edge ship WT; Firefox flagged off as of 2025; Safari partial. The fallback path covers users on browsers without WT — `Client::new` checks `globalThis.WebTransport` and skips the WT path entirely if absent.
