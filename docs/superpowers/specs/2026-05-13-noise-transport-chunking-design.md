# Noise transport chunking

Status: draft

## Problem

`NoiseConnection::send_reliable` (in `crates/sunset-noise/src/handshake.rs`) hands its input straight to `snow::TransportState::write_message`, which enforces a hard `MAXMSGLEN = 65 535` byte ceiling per Noise transport message (`snow-0.10.0/src/constants.rs:7`). Any reliable payload larger than ~65 519 bytes plaintext (65 535 − 16-byte AEAD tag) returns `Error::Input` from snow, which bubbles up as a `Transport` error in the sync engine's per-peer task. The task then closes the connection, sends `SyncMessage::Goodbye`, and the supervisor's reconnect loop re-runs the same oversize push and tears down again. From the user's perspective, the sender is stuck in a permanent disconnect/reconnect cycle, the message never reaches the receiver, and the relay logs show alternating `peer goodbye` and `axum ws send: IO error: Broken pipe` lines.

The current image-attachment feature (cf80b25) exposes this because a single chat post's `ContentBlock` carries the base64-encoded image bytes inline. A photo straight off a phone camera is multiple MB and blows past the limit by 30–100×. The same problem would arise for any future feature whose blob-or-message size crosses 65 KB — file attachments, larger voice messages, big presence payloads.

`web/e2e/images.spec.js` did not catch this: its fixtures are 1×1 PNG/GIFs that base64-encode to ~75 bytes each.

## Scope

This document covers the transport-layer fix only — chunking large reliable messages so they survive the Noise size limit. It is one piece of a two-plan split for the image-rendering bug:

- **Plan A (this spec).** Add a generic chunker that lets `NoiseConnection::send_reliable` accept arbitrary-size payloads transparently. Fixes the disconnect symptom for images today and removes the limit for any future blob-bearing feature.
- **Plan B (future, separate spec).** Image preprocessing in wasm — downscale + format-normalise picked images (handling HEIC, JPEG, PNG, WebP, GIF) before staging, so a 50 MB camera RAW does not become a 50 MB chat message. Independent of Plan A; can ship in either order.

## Goals and non-goals

**Goals**

- `NoiseConnection::send_reliable` / `recv_reliable` accept and reassemble arbitrary-size byte payloads (up to a configurable cap) without the caller knowing chunking happened.
- Chunking metadata (the per-chunk continuation flag) rides inside the noise AEAD envelope, so an on-path observer cannot detect or tamper with the chunk boundary.
- The 65 535-byte limit stops being a leaky property of the noise abstraction. No caller of `NoiseConnection` / `NoiseTransport` outside `sunset-noise` needs code changes.
- The chunker itself is generic and noise-independent — any other `TransportConnection` with a per-message size limit can wrap with it.
- A reassembly cap (16 MiB) bounds memory in the face of an adversarial or misbehaving peer streaming chunks forever.

**Non-goals**

- Image preprocessing, downscaling, format normalisation (Plan B).
- Per-chunk progress reporting or partial-restart on disconnect — a half-delivered logical message is dropped on the next disconnect just as it is today.
- Chunking the unreliable channel (`send_unreliable` / `recv_unreliable`). Datagram payloads in sunset are small (opus voice frames, ~150 bytes) and the unreliable channel does not currently flow through noise anyway; that channel passes straight through.
- Changing the Noise handshake. Only the post-handshake transport-mode send/recv paths are touched.

## Design

### Module layout

```
crates/sunset-sync/src/chunked.rs          (new)
  ChunkedConnection<C: TransportConnection>

crates/sunset-noise/src/handshake.rs       (modified)
  NoiseInner<C: RawConnection>             (new, private)
  NoiseConnection<C: RawConnection>        (re-shaped, public)
```

A symmetric `ChunkedTransport<T: Transport>` decorator would be trivial (wrap a `Transport`, return `ChunkedConnection<T::Connection>` from `connect` / `accept`). It is not added in this plan because no call site needs it — noise composes the chunker internally below `NoiseTransport`. If a future non-noise transport needs the same shape, add it then.

`sunset-sync` owns the `Transport` / `TransportConnection` traits, so the generic chunker naturally lives there. `sunset-noise` depends on `sunset-sync` already; the noise crate composes the chunker internally and re-exposes a single `NoiseConnection` type with the chunking baked in.

### Layering

```
caller -> NoiseConnection::send_reliable(bytes)            [public]
          └─ ChunkedConnection<NoiseInner<C>>              [private composition inside NoiseConnection]
             ├─ splits into ≤NOISE_MAX_PLAINTEXT_CHUNK-byte chunks
             └─ for each chunk:
                └─ NoiseInner::send_reliable(chunk)        [private, sunset-noise]
                   ├─ snow.write_message(chunk, ...)       [≤MAXMSGLEN bytes ciphertext]
                   └─ raw.send_reliable(ciphertext)
```

Receive mirrors: `NoiseConnection::recv_reliable` → `ChunkedConnection::recv_reliable` → loop `NoiseInner::recv_reliable` → snow.decrypt each chunk → accumulate until continuation flag is `0x00`.

### Chunk framing

Each chunk's plaintext (the bytes fed to `snow.write_message`) is:

```
byte 0:   continuation flag  (0x00 = last chunk, 0x01 = more follow)
bytes 1+: chunk_plaintext    (up to NOISE_MAX_PLAINTEXT_CHUNK − 1 bytes)
```

`NOISE_MAX_PLAINTEXT_CHUNK = 65_519` (snow's `MAXMSGLEN` 65 535 minus the 16-byte AEAD tag) — this is the byte cap on what `snow.write_message` will accept. Of those 65 519 plaintext bytes, the chunker spends 1 on the continuation flag and uses the remaining 65 518 for caller payload. Chunk size and reassembly cap live as constants in `sunset-noise` (`NOISE_MAX_PLAINTEXT_CHUNK`, `NOISE_MAX_REASSEMBLED_MESSAGE` = 16 MiB) because they are noise-specific. `ChunkedConnection`'s constructor accepts both as arguments so a future non-noise user picks their own.

The continuation flag lives inside the AEAD-protected plaintext for two reasons. First, an observer sees only N similarly-sized ciphertext frames over the wire and cannot tell from outside that they form one logical message — the framing is hidden. Second, an active attacker who flips the continuation byte invalidates the AEAD tag for that chunk, so a `recv_reliable` mid-stream tamper surfaces as a snow decrypt error and tears the connection down rather than silently truncating or extending a message.

A single-chunk message (the common case — ping, pong, small text, presence, receipts) costs +1 byte plaintext over the pre-change wire format. Multi-chunk messages cost +1 byte per chunk plus one extra noise message per ~65 KB.

### `ChunkedConnection<C: TransportConnection>`

Generic decorator implementing `TransportConnection`.

```rust
pub struct ChunkedConnection<C: TransportConnection> {
    inner: C,
    /// Maximum bytes per call to `inner.send_reliable` — i.e. the
    /// inclusive per-frame budget the underlying connection accepts.
    /// One of those bytes is spent on the continuation flag, so the
    /// effective payload cap per chunk is `max_chunk_size - 1`.
    max_chunk_size: usize,
    /// Hard cap on a single reassembled logical message, in bytes.
    max_reassembled_size: usize,
    send_lock: Mutex<()>,            // serialises multi-chunk sends
    recv_lock: Mutex<()>,            // serialises multi-chunk reassemblies
}
```

`max_chunk_size` must be `>= 2` (one byte for the flag plus at least one payload byte) or construction panics — a misconfigured chunker that could not make forward progress is a programmer bug, not a runtime error.

Send pseudocode:

```
fn send_reliable(bytes):
    _guard = send_lock.lock().await
    payload_per_chunk = max_chunk_size - 1
    if bytes.is_empty() or bytes.len() <= payload_per_chunk:
        inner.send_reliable([0x00] ++ bytes)
        return
    for (i, chunk) in split(bytes, payload_per_chunk):
        flag = 0x01 if more_chunks_after else 0x00
        inner.send_reliable([flag] ++ chunk)
```

An empty input sends one inner frame containing only the `0x00` flag and reassembles to an empty `Bytes` on the receive side. (`send_reliable` is not expected to be called with empty input in practice, but the chunker shouldn't panic or misbehave if it is.)

Recv pseudocode:

```
fn recv_reliable():
    _guard = recv_lock.lock().await
    buf = Vec::new()
    loop:
        frame = inner.recv_reliable().await?
        if frame.is_empty():
            return Err(Transport("empty chunk"))
        (flag, chunk) = (frame[0], &frame[1..])
        if buf.len() + chunk.len() > max_reassembled_size:
            return Err(Transport("oversized message"))
        buf.extend_from_slice(chunk)
        if flag == 0x00:
            return Ok(buf.into())
        if flag != 0x01:
            return Err(Transport("bad continuation flag"))
```

`send_unreliable` / `recv_unreliable` pass straight through to `inner.send_unreliable` / `inner.recv_unreliable` with no framing.

The two locks (`send_lock`, `recv_lock`) preserve the "one logical message in, one out" abstraction under concurrent use. Without them, two concurrent `send_reliable` calls would interleave their chunk streams on the wire and the receiver would deframe one corrupted blob plus one missing tail. The locks are held across all `inner.send_reliable` calls of a single logical message — slow sends serialise other sends on the same connection, but that is already implicit in the existing `Arc<Mutex<TransportState>>` (snow's cipher counter cannot interleave anyway) and is the right semantic.

The reassembly cap returns the error and leaves the inner connection in an indeterminate state: the chunker has consumed an unknown number of chunks of an oversize message and any subsequent chunks belonging to it will be misinterpreted as the next message's first frame. The caller is expected to close the connection after `Error::Transport("oversized message")`. This is acceptable because the cap (16 MiB) is well above any expected legitimate payload — hitting it means a buggy or malicious peer.

### `NoiseInner<C: RawConnection>`

Private to `sunset-noise`. Holds the snow `TransportState` and the raw connection; implements `TransportConnection` for one chunk at a time.

```rust
pub(crate) struct NoiseInner<C: RawConnection> {
    raw: C,
    state: Arc<Mutex<TransportState>>,
    peer_id: PeerId,
}

#[async_trait(?Send)]
impl<C: RawConnection> TransportConnection for NoiseInner<C> {
    async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
        // Caller (ChunkedConnection) guarantees bytes.len() <= NOISE_MAX_PLAINTEXT_CHUNK.
        let mut buf = vec![0u8; bytes.len() + 16];
        let n = {
            let mut state = self.state.lock().await;
            state.write_message(&bytes, &mut buf)?
        };
        self.raw.send_reliable(Bytes::copy_from_slice(&buf[..n])).await
    }

    async fn recv_reliable(&self) -> SyncResult<Bytes> {
        let ct = self.raw.recv_reliable().await?;
        let mut pt = vec![0u8; ct.len()];
        let n = {
            let mut state = self.state.lock().await;
            state.read_message(&ct, &mut pt)?
        };
        Ok(Bytes::copy_from_slice(&pt[..n]))
    }

    async fn send_unreliable(&self, bytes: Bytes) -> SyncResult<()> {
        self.raw.send_unreliable(bytes).await
    }

    async fn recv_unreliable(&self) -> SyncResult<Bytes> {
        self.raw.recv_unreliable().await
    }

    fn peer_id(&self) -> PeerId { self.peer_id.clone() }
    async fn close(&self) -> SyncResult<()> { self.raw.close().await }
}
```

The unreliable channel passes through verbatim — matching today's behaviour, where noise's unreliable methods are non-encrypting pass-throughs to the raw transport.

### `NoiseConnection<C: RawConnection>`

Public type; same name and constructor surface as today. Internally a `ChunkedConnection<NoiseInner<C>>` plus a stashed `peer_id` for the trait method.

```rust
pub struct NoiseConnection<C: RawConnection> {
    chunked: ChunkedConnection<NoiseInner<C>>,
    peer_id: PeerId,
}

impl<C: RawConnection> NoiseConnection<C> {
    pub fn new(raw: C, state: TransportState, peer_id: PeerId) -> Self {
        let inner = NoiseInner {
            raw,
            state: Arc::new(Mutex::new(state)),
            peer_id: peer_id.clone(),
        };
        let chunked = ChunkedConnection::new(
            inner,
            NOISE_MAX_PLAINTEXT_CHUNK,
            NOISE_MAX_REASSEMBLED_MESSAGE,
        );
        Self { chunked, peer_id }
    }
}

#[async_trait(?Send)]
impl<C: RawConnection> TransportConnection for NoiseConnection<C> {
    async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
        self.chunked.send_reliable(bytes).await
    }
    async fn recv_reliable(&self) -> SyncResult<Bytes> {
        self.chunked.recv_reliable().await
    }
    async fn send_unreliable(&self, bytes: Bytes) -> SyncResult<()> {
        self.chunked.send_unreliable(bytes).await
    }
    async fn recv_unreliable(&self) -> SyncResult<Bytes> {
        self.chunked.recv_unreliable().await
    }
    fn peer_id(&self) -> PeerId { self.peer_id.clone() }
    async fn close(&self) -> SyncResult<()> { self.chunked.close().await }
}
```

`NoiseTransport::connect` / `NoiseTransport::accept` continue to return `NoiseConnection<R::Connection>` — no call-site change in `sunset-web-wasm`, `sunset-relay`, `sunset-sync-ws-native`, or any test.

## Wire-format break

This change shifts the on-wire bytes for every reliable noise message: each Noise transport message now carries a 1-byte continuation flag at the start of its plaintext. Old peers do not understand this byte and will treat it as the first byte of whatever sunset-sync expected. There is no protocol-version handshake guarding this — the change is a flag-day break.

Acceptable because:

1. We are in the alpha window. cf80b25 ("end-to-end image attachments") already shipped a deliberate alpha-window wire-format break for the `MessageBody::Text` shape and pinned the new format with a hex vector; this change fits the same window.
2. The noise wire format has no frozen hex pin test today (only `ContentBlock::hash()` in `crypto/envelope.rs` is hex-pinned, and that test is at a higher layer).
3. All sunset peers reachable through any relay we operate will be running master, since there are no third-party deployments yet.

No protocol-version bump is added; if a versioning need arises later, it should be addressed at the noise-handshake layer or in `SyncMessage::Hello`, both of which are outside this plan.

## Testing

Three layers, each at the smallest scope that exercises the contract.

### `sunset-sync::chunked` unit tests

In `crates/sunset-sync/src/chunked.rs::tests`, using an in-memory `PipeRawConnection`-style fixture wrapped in `ChunkedConnection`:

Let `P = max_chunk_size - 1` (the payload portion per chunk).

- Empty roundtrip: `send_reliable(&[])` → one inner frame of just `0x00` → `recv_reliable()` returns an empty `Bytes`.
- Single-chunk roundtrip (input length 1 through `P`): exactly one inner `send_reliable` call, output bytes equal input.
- Chunk-boundary roundtrips: input sizes `P`, `P + 1`, `2P`, `2P + 1`, `3P`, `3P + 1`. Each yields the expected chunk count on the inner connection (introspected via a counter on the fixture).
- Larger random-size roundtrip: a handful of sizes between 100 KB and 4 MB, asserting byte-for-byte equality.
- Concurrent sends: two tasks each send a distinct multi-chunk payload concurrently on the same `ChunkedConnection`; the receiver gets exactly those two payloads intact (in some order) with no chunk interleaving.
- Reassembly cap: a peer sending chunks past `max_reassembled_size` causes `recv_reliable` to return `Error::Transport` with the "oversized message" string. (The connection is not required to remain usable after this — see Design.)
- Malformed frame: an empty inner frame, or a frame whose first byte is neither `0x00` nor `0x01`, returns `Error::Transport`.
- Unreliable channel pass-through: `send_unreliable(bytes)` makes one inner `send_unreliable` call with exactly `bytes` (no framing byte added); the receive side mirrors.

### `sunset-noise` integration test

In `crates/sunset-noise/src/handshake.rs::tests`, send a 4 MB random payload over the existing `PipeRawConnection` test fixture between paired `NoiseConnection` instances and assert byte-for-byte roundtrip. Today this fails at `snow.write_message`; after the change it passes. This is the smallest test that proves the leaky-limit fix from the public API's perspective.

### End-to-end sync test

New `crates/sunset-sync-ws-native/tests/two_peer_ws_noise_large.rs`. Mirrors the existing `two_peer_ws_noise.rs` topology — two peers, real tokio-tungstenite WebSocket transport, real noise handshake — but the sender pushes a `SyncMessage::EventDelivery` carrying a `ContentBlock` with ~2 MB of random data, and the receiver asserts the decoded `SyncMessage` byte-equal to the sender's. Validates the whole stack at a real-world payload size.

### Image e2e

Add one case to `web/e2e/images.spec.js` that sends a synthetic ~300 KB image (a generated PNG, not a real photo, to keep the test deterministic) and asserts the receiver renders it. Still small versus reality but well past the old 65 KB Noise ceiling, so it catches future regressions from anyone who reintroduces a Noise-size assumption upstream of `ChunkedConnection`.

## Risks and mitigations

- **Slow sender starves recv.** A sender that begins a 16 MiB message and stalls mid-stream holds the receiver's `recv_lock` against any subsequent message on the same connection. Acceptable: the connection is point-to-point and message-ordered, so a stalled message blocks the channel regardless of chunking. Heartbeat / connection-level timeouts (already present in `sunset-sync::peer`) tear down a dead peer.
- **Concurrency footgun in `ChunkedConnection`.** Forgetting either the send or recv lock would let two concurrent operations corrupt each other. Tested explicitly; locks are visible in the type.
- **Reassembly cap left as a const.** A future caller that genuinely needs >16 MiB messages (large file attachments?) would need to plumb through a different cap. Acceptable: `ChunkedConnection`'s constructor already takes the cap as an argument; only `NoiseConnection::new` would change to expose it.
- **Per-chunk lock contention.** Each multi-chunk send takes two locks (the chunker's send_lock and snow's `state` mutex). For a 4 MB payload this is ~64 lock cycles per direction. Measured contention is irrelevant for chat workloads; voice does not flow through reliable noise. No mitigation needed.
