# sunset-sync-quic — design

Date: 2026-05-12 · Author: Claude Opus 4.7 · Status: proposed

## Summary

Add a new transport family — **QUIC over a NAT-hole-punched UDP socket** — that lets two natted native peers connect directly, peer-to-peer, without going through a relay for the data plane. The relay (or any shared `sunset-store`) is used **only as the signaling side-channel** to exchange candidate addresses, the same way Tailscale uses DERP for coordination only.

Today the relay-fallback path is `WebTransport → WebSocket`, and the only direct P2P path is browser WebRTC (which is unreliable to set up and only works in browsers). Native peers currently have no direct path at all and must relay every byte. `sunset-sync-quic` closes that gap with a more deterministic NAT-traversal story than WebRTC: STUN-based candidate discovery, simultaneous UDP holepunch, then QUIC.

Pluggable as `RawTransport` — no upstream changes. Wraps with `NoiseTransport` for the authenticated layer, slots into `MultiTransport`/`FallbackTransport` like any other transport.

## Why now

- Native ↔ native (TUI, future desktop, relay-to-relay) currently has no direct path.
- WebRTC is browser-only on our stack and notoriously fragile in the wild.
- QUIC over a hole-punched UDP socket gives us reliable streams + unreliable datagrams in one transport, on the same UDP NAT mapping — so a single 5-tuple carries both.
- We already have all the side-channel machinery in `sunset-sync::Signaler` + `sunset_core::RelaySignaler` (Noise_KK over a CRDT entry, replicated via `sunset-store`). Reusing it means a QUIC peer can coordinate the holepunch over **any** signaling room the peers share — no separate rendezvous server.

## Reference

- Tailscale's "How NAT traversal works" — STUN + simultaneous holepunch + DERP fallback (we substitute `sunset-store` for DERP).
- `~/src/udpp` (`veq` crate) — proof-of-concept the user has already validated: STUN-derived candidate set, 1 Hz probe loop to every candidate, first responder wins, Noise XX over UDP. We do **not** reuse the udpp code; we lift the **pattern** and rebuild it on quinn so we get QUIC's reliable streams, datagrams, congestion control, loss recovery, and TLS 1.3 for free.

## Layered model

```
Application
    ↓
SyncEngine
    ↓
MultiTransport< Primary , QuicRawTransport (this) >       ← or any composition
    ↓
NoiseTransport< QuicRawTransport >
    ↓
QuicRawTransport ← RawTransport impl
    ├── HolepunchCoordinator (per-peer)
    │     ├── candidate discovery (local IPs + STUN)
    │     ├── exchange via Signaler (Noise_KK over sunset-store entry)
    │     └── probe loop to every remote candidate
    └── quinn::Endpoint on a shared UDP socket (single instance, demuxes peers)
```

No engine changes. The transport accepts a `Rc<dyn Signaler>` in its constructor; sunset-core (or any host) wires `RelaySignaler` to the `sunset-store` it cares about. The store side-channel is **already authenticated and PFS-encrypted** at the `RelaySignaler` layer — we ride on that and don't reinvent it.

## Crate

- **`sunset-sync-quic`** — native only. WASM has no UDP, no STUN, no quinn — there is nothing meaningful to ship there. The crate doesn't compile on `wasm32-unknown-unknown` and is excluded from the wasm builds by feature-gating (or by simply omitting it from any wasm-targeting crate's dependency tree).

## Wire layout (data plane)

Once the UDP hole is open, the data plane is plain QUIC:

- **Reliable** — one persistent bidirectional QUIC stream opened at connection start, 4-byte big-endian length prefix per `SyncMessage`. Identical framing to `sunset-sync-webtransport-native`.
- **Unreliable** — `quinn::Connection::send_datagram` / `read_datagram`. Hard ceiling `MAX_DATAGRAM_PAYLOAD = 1200` bytes — matches the WT/WebRTC ceiling we already use elsewhere; oversize payloads return `Err` on send (callers in the engine's ephemeral fan-out drop on `Err` and keep going).
- **Frame ceiling** — `MAX_RELIABLE_FRAME = 16 MiB`, same as WT.

## NAT traversal protocol

### Candidate discovery (per local endpoint)

On startup the transport binds **one** dual-stack `tokio::net::UdpSocket` (`0.0.0.0:0` and `[::]:0`, like `udpp::DualSocket`) and discovers its candidate addresses:

1. **Local interfaces** — every IPv4 and IPv6 address bound on every non-loopback NIC, with the socket's actual port. Pulled from `network-interface` (the same crate `udpp` uses).
2. **STUN-derived public** — query `stun.l.google.com:19302` (configurable list) once per IP family on startup. Cached as part of the transport's known candidate set.

The local socket is shared across **all** peer connections — same 5-tuple, same NAT mapping. Crucial: a per-peer socket would force a separate NAT mapping per peer, defeating endpoint-independent NAT (the common case).

Unspecified (`0.0.0.0`, `::`) addresses are filtered out — they're never valid remote candidates. Loopback addresses are *kept* (the integration tests rely on them; in real-world deployments they're harmless because no remote peer can reach them).

### Candidate exchange (signaling)

Per peer, on `connect(addr)` or accept-side dispatch:

1. Each side generates a 16-byte random `session_id` (unique per connect attempt — avoids stale handshake state confusing a new attempt).
2. The "server-side" of the QUIC handshake (the higher-pubkey peer; deterministic, like `udpp::is_initiator`) generates a fresh self-signed Ed25519 cert at startup; both sides reuse the same cert across all connections in the process lifetime.
3. Each side sends one `QuicSignalKind::Candidates` to the other via the `Signaler`:

```rust
#[derive(Serialize, Deserialize)]
struct Candidates {
    session_id: [u8; 16],
    addresses: Vec<SocketAddr>,
    /// SHA-256 of the SubjectPublicKeyInfo for THIS side's QUIC server
    /// cert. The peer pins this hash to validate TLS regardless of CN.
    server_cert_sha256: [u8; 32],
}
```

The payload is opaque bytes to the `Signaler` (postcard-encoded). The signaler (typically `RelaySignaler` over `sunset-store`) wraps it in Noise_KK before it ever lands on the wire — so the candidate set is end-to-end encrypted between peers even though it transits a relay's store.

### Probe loop

Once both sides have received the other's `Candidates`, both start probing every remote candidate address simultaneously:

- 250 ms probe interval. (Tighter than `udpp`'s 1 Hz so first-hole convergence is sub-second on a healthy link.)
- 5 s total budget. If no candidate confirms in 5 s, fail with `Error::Transport("holepunch: no candidate confirmed in 5s")`. The caller (typically `FallbackTransport` or the supervisor's reconnect-with-backoff) decides what to do next.
- Per-probe payload (postcard-encoded, sent as a raw UDP datagram on the shared socket):

```rust
struct Probe {
    magic: [u8; 4],      // b"SnP1" — distinguishes from QUIC packets
    session_id: [u8; 16],
    role: u8,            // 0 = Ping, 1 = Pong
    sender_pk: [u8; 32], // local Ed25519 pubkey (peer identity)
    nonce: [u8; 16],     // random; echoed back by the responder to bind ack
}
```

A `Probe::Ping` arriving from a remote addr causes us to:
1. Reply with `Probe::Pong` (echoing `nonce`).
2. Record `(peer_pk, remote_addr)` as a confirmed working path.

A `Probe::Pong` arriving with our own `nonce` confirms the working path from our side. The first path to be confirmed in either direction "wins".

This is symmetric: both sides may discover the working addr from a Ping, a Pong, or both. The race is fine because the working addr is the same regardless of which event resolves it first.

### Multiplexing QUIC and probes on one socket

quinn's `Endpoint::new_with_abstract_socket` accepts any `Arc<dyn quinn::AsyncUdpSocket>`. We implement that trait with a small wrapper that:

- In `poll_recv`, peeks the first 4 bytes of every datagram:
  - If they match `b"SnP1"`, it's a holepunch probe — route to the `HolepunchCoordinator`'s probe-handler channel; **do not** hand to quinn.
  - Otherwise, hand to quinn.
- In `poll_send`, just forwards to the inner socket (probes and QUIC traffic both go out the same way).

Since the prefix check happens in our `AsyncUdpSocket` layer **before** quinn sees the bytes, there is no ambiguity at the QUIC layer — probes are invisible to quinn, and quinn-generated packets never start with our magic (they begin with a QUIC v1 header byte). Third-party junk that happens to match our magic hits the probe handler, which validates `session_id` / `sender_pk` and drops on no match.

### Why probes don't deadlock with quinn

quinn's accept loop runs entirely inside `poll_recv` events. By siphoning probes off in our wrapper, we deprive quinn of those packets — that is intentional and harmless. Once a candidate is confirmed and the handshake starts, both sides switch to sending QUIC packets to the confirmed addr; probes either stop (we cap the probe loop at 5 s anyway) or are silently ignored by the peer's wrapper.

## QUIC handshake

After the holepunch confirms a working `(peer_pk, remote_addr)`:

- **Client side** (lower pubkey): `endpoint.connect_with(client_config, remote_addr, "sunset")` where `client_config` is a `quinn::ClientConfig` built from a `rustls::ClientConfig` that *only* accepts certs whose SPKI SHA-256 matches the `server_cert_sha256` from the peer's `Candidates`. The SNI `"sunset"` is fixed (the cert is self-signed; SNI is just a routing label, not a trust anchor).
- **Server side** (higher pubkey): `endpoint.accept()`. quinn's connection-accept handler validates the inbound Initial packet, applies our `ServerConfig` (the self-signed cert generated at startup), completes the TLS 1.3 + QUIC handshake.

The client side immediately opens one bidi stream after the handshake completes; the server side accepts it. The two halves of the stream become the reliable channel.

### Why we still need NoiseTransport on top

QUIC's TLS gives us **transport** authentication tied to the self-signed cert. It does **not** identify the peer by its sunset Ed25519 identity. `NoiseTransport` (sunset-noise) wraps the `RawConnection` and performs a Noise_XX handshake that binds the QUIC pipe to the peer's real identity. Same pattern as every other transport in the workspace.

(An alternative would be to generate the QUIC cert deterministically from the peer's Ed25519 identity. That's a Plan-N optimization; v1 takes the boring path.)

## RawTransport surface

```rust
pub struct QuicRawTransport {
    /// Side-channel for candidate exchange. Shared across all peer
    /// connections — typically a RelaySignaler over a sunset-store.
    signaler: Rc<dyn Signaler>,
    /// Local Ed25519 identity (used for "is_initiator" tiebreak).
    local_peer: PeerId,
    /// Shared UDP socket + quinn endpoint. One per QuicRawTransport
    /// instance; demultiplexes all peer connections.
    endpoint: Rc<QuicEndpoint>,
    /// Where confirmed inbound connections land for `accept()` to drain.
    completed_rx: Rc<Mutex<mpsc::UnboundedReceiver<Result<QuicRawConnection>>>>,
    /// STUN servers consulted on startup.
    stun_servers: Vec<String>,
    /// In-flight per-peer holepunch state (mirror webrtc-browser pattern).
    inner: Rc<RefCell<Inner>>,
}

impl QuicRawTransport {
    pub async fn bind(
        signaler: Rc<dyn Signaler>,
        local_peer: PeerId,
        stun_servers: Vec<String>,
    ) -> Result<Self>;
}

#[async_trait(?Send)]
impl RawTransport for QuicRawTransport {
    type Connection = QuicRawConnection;
    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection>;
    async fn accept(&self) -> Result<Self::Connection>;
}
```

### Address format

`PeerAddr` for `QuicRawTransport` is the bare Ed25519 peer pubkey hex, prefixed with `quic://` — mirrors `webrtc-browser`'s `webrtc://<hex>` convention:

```
quic://<peer-ed25519-hex>
```

We don't carry the candidates in the addr — they come over the signaler. The addr's only job is to tell `connect()` *who* to coordinate with; the resolver upstream is responsible for handing the engine `quic://`-style addrs for peers that support QUIC.

### Dispatcher pattern

Mirrors `WebRtcRawTransport` (already reviewed and merged). One shared task drains `signaler.recv()` and routes each `Candidates` by `from`:

- If `per_peer[from]` exists (a `connect()` is mid-flight for that peer), forward the Candidates to its inbound queue.
- Otherwise this is a fresh inbound — spawn a per-peer accept task before any await, drain any early-buffered Candidates for that peer (small TTL-bound buffer, same 30 s as WebRTC), and register `per_peer[from]` so subsequent messages route correctly.

The accept task runs the holepunch + QUIC handshake and pushes the resulting `QuicRawConnection` (or `Err`) into `completed_tx`; `accept()` reads `completed_rx`.

## Failure modes

| Failure | Behavior |
|---|---|
| STUN unreachable | Skip STUN, fall back to local-only candidates. Logged at WARN. Probes still try local addrs (works on same-LAN setups). |
| No remote candidates received within 5 s of `connect()` | `Err("holepunch: signaling timeout")`. |
| No probe confirmed within 5 s | `Err("holepunch: no candidate confirmed in 5s")`. |
| Both sides natted by symmetric NATs (port-randomizing) | Probes typically fail. v1 returns the same timeout. v2 may try birthday-paradox port spraying; out of scope here. |
| QUIC handshake fails after holepunch succeeds | quinn returns an error; we propagate as `Error::Transport`. |
| QUIC connection drops mid-session | quinn closes the streams; `recv_reliable` / `send_reliable` return errors and the per-peer task tears down (existing supervisor reconnect logic does the rest). |

## Tests

### Unit (in-crate)

1. **Magic prefix detection** — probe bytes route to probe handler; QUIC-shaped bytes route to quinn.
2. **Candidate discovery** — `discover_candidates(&socket)` on `127.0.0.1:0` returns at least the loopback IP and the bound port; STUN is omitted via empty server list.
3. **Probe roundtrip** — two `HolepunchCoordinator` instances over the real UDP socket pair confirm a single candidate ping/pong within deadline.
4. **Initiator/responder tiebreak** — given two peer pubkeys, `is_initiator` returns true iff the local key sorts lower.

### Integration

1. **`tests/holepunch_loopback.rs`** — two `QuicRawTransport` instances on `127.0.0.1`, sharing a `MemoryStore`-backed `RelaySignaler`. Side A calls `connect(quic://<B>)`, side B's `accept()` returns the matching connection. Both roundtrip a reliable message and a datagram. This is the *honest* end-to-end test: no probe-loop bypass, no stub signaler — the real holepunch protocol runs over loopback.
2. **`tests/stun_skipped.rs`** — STUN servers set to `[]`, both peers on loopback. Holepunch still completes; the test asserts the working candidate is a loopback addr (via `QuicRawConnection::remote_addr`).

### Non-tests

We do **not** write tests that:
- Insert `tokio::time::sleep` between `connect()` and `recv_reliable()` to "let the handshake settle."
- Poke at private state (`coordinator.is_confirmed()`, etc.) before the user-level `connect()` returns.
- Raise the 5 s holepunch budget arbitrarily — if a test exceeds 5 s on loopback, the *code* is broken.

CLAUDE.md's debugging-discipline rules apply verbatim.

## Out of scope (deferred to follow-ups)

- Symmetric-NAT birthday-paradox port spray (Tailscale's "port-spraying").
- TURN-style relayed fallback at the QUIC layer (`MultiTransport` + the existing relay path is the strategic fallback).
- 0-RTT resumption (quinn supports it; we don't need it on v1, and the engine doesn't expose a "warm" connection request).
- Per-peer self-signed cert tied to Ed25519 identity (replace `NoiseTransport` for this transport).
- Path migration — quinn supports it; we don't actively probe alternate paths after the initial confirm.
- **True simultaneous-open glare resolution.** `sunset_core::RelaySignaler` uses Noise_KK and creates an `Initiator` slot the first time it sends to a peer; if both peers start sending to each other before either has responded, both `decrypt_inbound` calls hit the `Initiator` branch and try to parse the peer's msg1 as a msg2, which fails. This is a property of the *signaler*, not this transport. Practical impact is low — natural latency staggers most calls — but it does mean the v1 `tests/symmetric_simultaneous_open.rs` envisioned earlier in this spec is omitted: it can't pass without a KK-glare fix in `sunset-core`. The `WebRtcRawTransport` has the same limitation. v2 should add KK-glare resolution to `RelaySignaler` (deterministic "higher pubkey discards their Initiator" rule).

## Acceptance checklist

- [ ] `cargo build -p sunset-sync-quic --all-features` succeeds.
- [ ] `cargo nextest run -p sunset-sync-quic --all-features` passes all unit + integration tests 5× consecutive (CLAUDE.md stability gate).
- [ ] `cargo clippy --workspace --all-features --all-targets -- -D warnings` clean. No `#[allow(clippy::...)]` anywhere in the new crate.
- [ ] `scripts/check-no-clippy-allow.sh` passes.
- [ ] No upstream changes to `sunset-sync`, `sunset-core`, or any sibling transport. Only additions: a new crate + workspace member entry.
- [ ] Flake builds the new crate's tests through `nix develop --command cargo test -p sunset-sync-quic` (no system tools).
