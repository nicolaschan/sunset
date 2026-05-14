# Noise transport chunking — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a generic `ChunkedConnection<C: TransportConnection>` decorator and make `NoiseConnection` compose it internally, so `send_reliable` / `recv_reliable` accept arbitrary-size payloads transparently and no longer hit snow's 65 535-byte per-message ceiling.

**Architecture:** New `crates/sunset-sync/src/chunked.rs` module owns the generic chunker (knows nothing about noise). `crates/sunset-noise/src/handshake.rs` is reshaped: a private `NoiseInner<C: RawConnection>` does the existing snow encrypt/decrypt one chunk at a time, and the public `NoiseConnection<C>` becomes a thin wrapper around `ChunkedConnection<NoiseInner<C>>`. The chunker frames each chunk as `[continuation: u8] [payload]` inside the AEAD envelope so observers can't see chunk boundaries or flip the flag.

**Tech Stack:** Rust 1.x stable, `async-trait`, `tokio::sync::Mutex`, `bytes`, `snow` (already vendored). All native + `wasm32-unknown-unknown` targets (the chunker is `?Send`-friendly and uses no thread-spawn).

**Spec:** `docs/superpowers/specs/2026-05-13-noise-transport-chunking-design.md`

**Test commands** (run from repo root, in the worktree's dev shell):

```
nix develop --command cargo test -p sunset-sync chunked::tests
nix develop --command cargo test -p sunset-noise tests::
nix develop --command cargo test -p sunset-sync-ws-native --test two_peer_ws_noise_large
nix develop --command cargo test --workspace --all-features
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
nix run .#web-test -- --grep "image"
```

---

### Task 1: Add `ChunkedConnection<C>` with full implementation and core roundtrip tests

The chunker module is small (~80 LOC implementation + tests). Land it all in one commit: struct, constructor, full `TransportConnection` impl, an in-memory `PipeConnection` test fixture, and tests covering empty / single-chunk / chunk-boundary / large random roundtrips. The locks and reassembly cap are wired up from the start so subsequent tasks just add tests against them — no need to retrofit.

**Files:**
- Create: `crates/sunset-sync/src/chunked.rs`
- Modify: `crates/sunset-sync/src/lib.rs` (add `pub mod chunked;` and `pub use chunked::ChunkedConnection;`)

- [ ] **Step 1: Write the module skeleton + tests**

Create `crates/sunset-sync/src/chunked.rs`:

```rust
//! Generic chunking decorator: splits arbitrary-size reliable payloads
//! into per-frame-budget-sized chunks, sends each through an inner
//! `TransportConnection`, and reassembles on the receive side. Each
//! chunk's plaintext is prefixed with a 1-byte continuation flag
//! (0x00 = last chunk, 0x01 = more follow) so the inner connection
//! sees `[flag, ...payload]` frames bounded by `max_chunk_size`.
//!
//! Knows nothing about cryptography. Composes with any
//! `TransportConnection`; sunset-noise uses it internally to work
//! around snow's 65 535-byte per-message ceiling. A future non-noise
//! transport with its own per-message limit can wrap with this same
//! decorator.

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::transport::{TransportConnection, TransportKind};
use crate::types::PeerId;

/// Per-chunk plaintext continuation byte. The flag lives inside the
/// inner connection's frame (and therefore, when composed with noise,
/// inside the AEAD envelope), so observers cannot detect or tamper
/// with chunk boundaries.
const CONTINUATION_FLAG_BYTE: usize = 1;
const FLAG_LAST: u8 = 0x00;
const FLAG_MORE: u8 = 0x01;

/// Wraps any `TransportConnection` and adds reliable-channel chunking
/// + reassembly. The unreliable channel passes straight through (its
/// payloads — datagrams — are already small).
///
/// Two mutexes preserve the "one logical message in, one out" contract
/// under concurrent use: `send_lock` serialises multi-chunk sends so
/// two concurrent callers can't interleave their chunk streams on the
/// inner connection; `recv_lock` serialises multi-chunk reassemblies
/// so two concurrent receivers can't each consume part of the same
/// logical message.
pub struct ChunkedConnection<C: TransportConnection> {
    inner: C,
    /// Maximum bytes per call to `inner.send_reliable` — i.e. the
    /// inclusive per-frame budget the underlying connection accepts.
    /// One of those bytes is spent on the continuation flag, so the
    /// effective payload cap per chunk is `max_chunk_size - 1`.
    max_chunk_size: usize,
    /// Hard cap on a single reassembled logical message, in bytes.
    /// Hitting this cap returns `Error::Transport("oversized message")`
    /// and the inner connection is left in an indeterminate state
    /// (caller should close it).
    max_reassembled_size: usize,
    send_lock: Mutex<()>,
    recv_lock: Mutex<()>,
}

impl<C: TransportConnection> ChunkedConnection<C> {
    /// Construct a chunker. Panics if `max_chunk_size < 2` — a
    /// chunker that can't fit the framing byte plus one payload byte
    /// makes no forward progress and is a programmer bug, not a
    /// runtime error.
    pub fn new(inner: C, max_chunk_size: usize, max_reassembled_size: usize) -> Self {
        assert!(
            max_chunk_size >= 2,
            "ChunkedConnection::new: max_chunk_size must be >= 2 (1 byte for the continuation flag + at least 1 payload byte); got {max_chunk_size}"
        );
        Self {
            inner,
            max_chunk_size,
            max_reassembled_size,
            send_lock: Mutex::new(()),
            recv_lock: Mutex::new(()),
        }
    }
}

#[async_trait(?Send)]
impl<C: TransportConnection> TransportConnection for ChunkedConnection<C> {
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
        let _guard = self.send_lock.lock().await;
        let payload_per_chunk = self.max_chunk_size - CONTINUATION_FLAG_BYTE;

        // Empty / single-chunk fast path.
        if bytes.len() <= payload_per_chunk {
            let mut frame = BytesMut::with_capacity(1 + bytes.len());
            frame.extend_from_slice(&[FLAG_LAST]);
            frame.extend_from_slice(&bytes);
            return self.inner.send_reliable(frame.freeze()).await;
        }

        // Multi-chunk: send all-but-last with FLAG_MORE, last with FLAG_LAST.
        let mut offset = 0;
        while offset < bytes.len() {
            let end = (offset + payload_per_chunk).min(bytes.len());
            let is_last = end == bytes.len();
            let flag = if is_last { FLAG_LAST } else { FLAG_MORE };
            let mut frame = BytesMut::with_capacity(1 + (end - offset));
            frame.extend_from_slice(&[flag]);
            frame.extend_from_slice(&bytes[offset..end]);
            self.inner.send_reliable(frame.freeze()).await?;
            offset = end;
        }
        Ok(())
    }

    async fn recv_reliable(&self) -> Result<Bytes> {
        let _guard = self.recv_lock.lock().await;
        let mut buf = BytesMut::new();
        loop {
            let frame = self.inner.recv_reliable().await?;
            if frame.is_empty() {
                return Err(Error::Transport("chunked: empty inner frame".into()));
            }
            let flag = frame[0];
            let chunk = &frame[1..];
            if buf.len() + chunk.len() > self.max_reassembled_size {
                return Err(Error::Transport("chunked: oversized message".into()));
            }
            buf.extend_from_slice(chunk);
            match flag {
                FLAG_LAST => return Ok(buf.freeze()),
                FLAG_MORE => continue,
                other => {
                    return Err(Error::Transport(format!(
                        "chunked: bad continuation flag 0x{other:02x}"
                    )));
                }
            }
        }
    }

    async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
        self.inner.send_unreliable(bytes).await
    }

    async fn recv_unreliable(&self) -> Result<Bytes> {
        self.inner.recv_unreliable().await
    }

    fn peer_id(&self) -> PeerId {
        self.inner.peer_id()
    }

    fn kind(&self) -> TransportKind {
        self.inner.kind()
    }

    async fn close(&self) -> Result<()> {
        self.inner.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PeerId;
    use sunset_store::VerifyingKey;
    use tokio::sync::mpsc;

    /// In-memory pipe pair impl of `TransportConnection`. Mirrors
    /// `PipeRawConnection` in `sunset-noise::handshake::tests` but at
    /// the `TransportConnection` layer (no encryption, no
    /// authentication) so the chunker can be exercised in isolation.
    struct PipeConnection {
        tx: Mutex<mpsc::UnboundedSender<Bytes>>,
        rx: Mutex<mpsc::UnboundedReceiver<Bytes>>,
    }

    #[async_trait(?Send)]
    impl TransportConnection for PipeConnection {
        async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
            self.tx
                .lock()
                .await
                .send(bytes)
                .map_err(|_| Error::Transport("pipe closed".into()))
        }
        async fn recv_reliable(&self) -> Result<Bytes> {
            self.rx
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| Error::Transport("pipe closed".into()))
        }
        async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
            // Same channel as reliable for test simplicity — the
            // tests that touch the unreliable path use a separate
            // pipe pair so the two channels don't collide.
            self.send_reliable(bytes).await
        }
        async fn recv_unreliable(&self) -> Result<Bytes> {
            self.recv_reliable().await
        }
        fn peer_id(&self) -> PeerId {
            PeerId(VerifyingKey::new(Bytes::copy_from_slice(&[0u8; 32])))
        }
        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    fn pipe_pair() -> (PipeConnection, PipeConnection) {
        let (a_tx, a_rx) = mpsc::unbounded_channel::<Bytes>();
        let (b_tx, b_rx) = mpsc::unbounded_channel::<Bytes>();
        (
            PipeConnection {
                tx: Mutex::new(a_tx),
                rx: Mutex::new(b_rx),
            },
            PipeConnection {
                tx: Mutex::new(b_tx),
                rx: Mutex::new(a_rx),
            },
        )
    }

    /// Test-only `max_chunk_size`. Small enough to exercise multi-chunk
    /// behaviour without allocating MB; the on-wire framing logic is
    /// size-independent.
    const TEST_MAX_CHUNK: usize = 64;
    const TEST_MAX_REASSEMBLED: usize = 1 << 20; // 1 MiB

    fn make_chunked() -> (
        ChunkedConnection<PipeConnection>,
        ChunkedConnection<PipeConnection>,
    ) {
        let (a, b) = pipe_pair();
        (
            ChunkedConnection::new(a, TEST_MAX_CHUNK, TEST_MAX_REASSEMBLED),
            ChunkedConnection::new(b, TEST_MAX_CHUNK, TEST_MAX_REASSEMBLED),
        )
    }

    async fn roundtrip(a: &ChunkedConnection<PipeConnection>, b: &ChunkedConnection<PipeConnection>, payload: Bytes) {
        a.send_reliable(payload.clone()).await.unwrap();
        let recv = b.recv_reliable().await.unwrap();
        assert_eq!(recv, payload, "roundtrip {} bytes", payload.len());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_roundtrip() {
        let (a, b) = make_chunked();
        roundtrip(&a, &b, Bytes::new()).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn single_chunk_roundtrip() {
        let (a, b) = make_chunked();
        // 1 byte through `payload_per_chunk` bytes — all fit in one chunk.
        for n in [1usize, 10, TEST_MAX_CHUNK - 1] {
            roundtrip(&a, &b, Bytes::from(vec![0xab; n])).await;
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn chunk_boundary_roundtrips() {
        let (a, b) = make_chunked();
        let p = TEST_MAX_CHUNK - CONTINUATION_FLAG_BYTE;
        for n in [p, p + 1, 2 * p, 2 * p + 1, 3 * p, 3 * p + 1] {
            // Distinct content per size so a misorder would show up.
            let payload: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(7)).collect();
            roundtrip(&a, &b, Bytes::from(payload)).await;
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn large_random_roundtrip() {
        let (a, b) = make_chunked();
        // 100 KB, 500 KB, 1 MiB - 1 (just under the test cap).
        for n in [100_000usize, 500_000, TEST_MAX_REASSEMBLED - 1] {
            let payload: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(13)).collect();
            roundtrip(&a, &b, Bytes::from(payload)).await;
        }
    }
}
```

Modify `crates/sunset-sync/src/lib.rs`. Add `pub mod chunked;` after the existing `pub mod ` lines (alphabetical order — between `pub mod ` lines for `connectable` and `digest`):

```rust
pub mod chunked;
```

And add to the public re-export block at the bottom:

```rust
pub use chunked::ChunkedConnection;
```

- [ ] **Step 2: Run tests, expect them to pass**

```
nix develop --command cargo test -p sunset-sync chunked::tests
```

Expected: 4 tests pass (`empty_roundtrip`, `single_chunk_roundtrip`, `chunk_boundary_roundtrips`, `large_random_roundtrip`).

- [ ] **Step 3: Run clippy + fmt, fix anything they flag**

```
nix develop --command cargo clippy -p sunset-sync --all-features --all-targets -- -D warnings
nix develop --command cargo fmt -p sunset-sync --check
```

Both must pass with no output. If clippy flags something, fix it in source (no `#[allow(...)]` suppressions — see CLAUDE.md). If fmt flags drift, run `cargo fmt -p sunset-sync` and re-stage.

- [ ] **Step 4: Commit**

```
git add crates/sunset-sync/src/chunked.rs crates/sunset-sync/src/lib.rs
git commit -m "$(cat <<'EOF'
sunset-sync: ChunkedConnection<C> decorator

Generic chunking layer that splits arbitrary-size reliable payloads
into per-frame-budget-sized chunks with a 1-byte continuation flag,
sends each through an inner TransportConnection, and reassembles on
the receive side up to a hard reassembly cap. Knows nothing about
cryptography. Two locks (send + recv) preserve "one logical message
in, one out" under concurrent use.

Unit tests cover empty, single-chunk, chunk-boundary, and ~1 MB
roundtrips. Concurrent-send, malformed-frame, oversized-message, and
unreliable pass-through tests follow in later tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Add concurrent-send test (proves send_lock works)

**Files:**
- Modify: `crates/sunset-sync/src/chunked.rs` (add one test in `mod tests`)

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`, after `large_random_roundtrip`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn concurrent_sends_do_not_interleave() {
    // Two concurrent multi-chunk sends from the same side must each
    // arrive intact on the receiver, not interleaved at the inner
    // frame level. Without `send_lock` the chunker would race and the
    // receiver would deframe one corrupted blob.
    let (a, b) = make_chunked();
    let a = std::rc::Rc::new(a);
    let p = TEST_MAX_CHUNK - CONTINUATION_FLAG_BYTE;

    let payload_x: Vec<u8> = vec![0xaa; 5 * p]; // 5 chunks
    let payload_y: Vec<u8> = vec![0xbb; 4 * p]; // 4 chunks

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let a1 = a.clone();
            let a2 = a.clone();
            let p_x = payload_x.clone();
            let p_y = payload_y.clone();
            let h1 = tokio::task::spawn_local(async move {
                a1.send_reliable(Bytes::from(p_x)).await.unwrap();
            });
            let h2 = tokio::task::spawn_local(async move {
                a2.send_reliable(Bytes::from(p_y)).await.unwrap();
            });
            h1.await.unwrap();
            h2.await.unwrap();

            // Two complete logical messages arrive, in some order.
            let r1 = b.recv_reliable().await.unwrap();
            let r2 = b.recv_reliable().await.unwrap();
            let mut got = [r1, r2];
            // Sort by length so the asserts below don't depend on
            // scheduling order.
            got.sort_by_key(|b| b.len());
            assert_eq!(got[0].as_ref(), payload_y.as_slice());
            assert_eq!(got[1].as_ref(), payload_x.as_slice());
        })
        .await;
}
```

- [ ] **Step 2: Run it and expect it to pass**

```
nix develop --command cargo test -p sunset-sync chunked::tests::concurrent_sends_do_not_interleave
```

Expected: PASS (the `send_lock` field added in Task 1 already serialises sends).

- [ ] **Step 3: Commit**

```
git add crates/sunset-sync/src/chunked.rs
git commit -m "$(cat <<'EOF'
sunset-sync: concurrent-send test for ChunkedConnection

Two concurrent multi-chunk send_reliable calls on the same connection
must each arrive intact, not interleaved at the inner-frame level.
The send_lock added with ChunkedConnection itself already guarantees
this; the test pins the invariant against future refactors.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Add reassembly-cap test

**Files:**
- Modify: `crates/sunset-sync/src/chunked.rs` (one test)

- [ ] **Step 1: Write the test**

Add inside `mod tests`, after `concurrent_sends_do_not_interleave`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn recv_rejects_oversized_message() {
    // Sender uses a chunker with a generous reassembly cap so it will
    // happily send a 3 KB payload. Receiver uses a chunker with a
    // tight 1 KB cap. The receive must error part-way through with
    // "oversized message".
    let (a_pipe, b_pipe) = pipe_pair();
    let sender = ChunkedConnection::new(a_pipe, TEST_MAX_CHUNK, TEST_MAX_REASSEMBLED);
    let receiver = ChunkedConnection::new(b_pipe, TEST_MAX_CHUNK, 1024);

    let payload = Bytes::from(vec![0u8; 3000]);
    sender.send_reliable(payload).await.unwrap();

    let err = receiver
        .recv_reliable()
        .await
        .expect_err("receive should reject oversized message");
    let msg = format!("{err}");
    assert!(
        msg.contains("oversized message"),
        "unexpected error: {msg}"
    );
}
```

- [ ] **Step 2: Run it and expect it to pass**

```
nix develop --command cargo test -p sunset-sync chunked::tests::recv_rejects_oversized_message
```

Expected: PASS.

- [ ] **Step 3: Commit**

```
git add crates/sunset-sync/src/chunked.rs
git commit -m "$(cat <<'EOF'
sunset-sync: oversized-message test for ChunkedConnection

Asymmetric chunker pair: sender with generous cap, receiver with
tight cap. The receiver's recv_reliable must error with
"oversized message" once accumulated chunks exceed
max_reassembled_size. Bounds memory against a misbehaving or
adversarial peer that streams chunks without ever flagging "last".

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Add malformed-frame tests

**Files:**
- Modify: `crates/sunset-sync/src/chunked.rs` (two tests)

- [ ] **Step 1: Write the tests**

Add inside `mod tests`, after `recv_rejects_oversized_message`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn recv_rejects_empty_inner_frame() {
    // An inner frame with zero bytes has no continuation flag at all.
    // We inject one directly into the inner pipe to bypass our own
    // sender's framing.
    let (a_pipe, b_pipe) = pipe_pair();
    let receiver = ChunkedConnection::new(b_pipe, TEST_MAX_CHUNK, TEST_MAX_REASSEMBLED);
    a_pipe.send_reliable(Bytes::new()).await.unwrap();

    let err = receiver
        .recv_reliable()
        .await
        .expect_err("receive should reject empty frame");
    assert!(format!("{err}").contains("empty inner frame"));
}

#[tokio::test(flavor = "current_thread")]
async fn recv_rejects_unknown_continuation_flag() {
    let (a_pipe, b_pipe) = pipe_pair();
    let receiver = ChunkedConnection::new(b_pipe, TEST_MAX_CHUNK, TEST_MAX_REASSEMBLED);
    // 0xff is neither FLAG_LAST (0x00) nor FLAG_MORE (0x01).
    a_pipe
        .send_reliable(Bytes::from_static(&[0xff, b'x']))
        .await
        .unwrap();

    let err = receiver
        .recv_reliable()
        .await
        .expect_err("receive should reject bad flag");
    assert!(format!("{err}").contains("bad continuation flag"));
}
```

- [ ] **Step 2: Run them and expect them to pass**

```
nix develop --command cargo test -p sunset-sync chunked::tests::recv_rejects_empty_inner_frame chunked::tests::recv_rejects_unknown_continuation_flag
```

Expected: both PASS.

- [ ] **Step 3: Commit**

```
git add crates/sunset-sync/src/chunked.rs
git commit -m "$(cat <<'EOF'
sunset-sync: malformed-frame tests for ChunkedConnection

Inject an empty inner frame (no continuation flag at all) and an
inner frame with an unknown flag byte (0xff) directly into the pipe
and assert recv_reliable surfaces them as Error::Transport with
human-readable messages. Closes the "what does the receiver do under
adversarial input" gap that the well-formed roundtrip tests can't
cover.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Add unreliable-channel pass-through test

**Files:**
- Modify: `crates/sunset-sync/src/chunked.rs` (one test)

- [ ] **Step 1: Write the test**

Add inside `mod tests`, after `recv_rejects_unknown_continuation_flag`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn unreliable_passes_through_verbatim() {
    // send_unreliable must NOT add a continuation flag — the
    // unreliable channel carries small datagrams (opus frames,
    // ephemeral deliveries) that don't need chunking and shouldn't
    // gain framing overhead.
    let (a, b) = make_chunked();
    let payload = Bytes::from_static(b"hello unreliable");
    a.send_unreliable(payload.clone()).await.unwrap();
    let recv = b.recv_unreliable().await.unwrap();
    assert_eq!(recv, payload, "unreliable channel must pass through unchanged");
}
```

- [ ] **Step 2: Run it and expect it to pass**

```
nix develop --command cargo test -p sunset-sync chunked::tests::unreliable_passes_through_verbatim
```

Expected: PASS.

- [ ] **Step 3: Final cargo test for the chunker module, then commit**

```
nix develop --command cargo test -p sunset-sync chunked::tests
```

Expected: 8 tests pass (all from Tasks 1–5).

```
git add crates/sunset-sync/src/chunked.rs
git commit -m "$(cat <<'EOF'
sunset-sync: unreliable-channel pass-through test

send_unreliable must not add the continuation framing byte — the
unreliable channel carries small datagrams (opus voice frames,
ephemeral deliveries) that don't need chunking and shouldn't pay the
framing-overhead tax.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Extract `NoiseInner<C>` and the size constants in `sunset-noise`

Pull the existing per-message snow encrypt/decrypt out of `NoiseConnection` and into a private `NoiseInner<C>` that implements `TransportConnection`. The existing `NoiseConnection<C>` becomes a temporary thin shim (still no chunking yet — that lands in Task 7) so the existing handshake tests stay green throughout.

**Files:**
- Modify: `crates/sunset-noise/src/handshake.rs`

- [ ] **Step 1: Add the size constants near the top of the file (just after the `use` block, before `pub fn do_handshake_initiator`)**

```rust
/// Maximum plaintext bytes accepted by `snow::TransportState::write_message`
/// — snow's `MAXMSGLEN` (65 535) minus the 16-byte ChaChaPoly AEAD tag.
/// `NoiseConnection` configures its internal `ChunkedConnection` with
/// this as `max_chunk_size`; the chunker spends 1 byte on its
/// continuation flag, leaving 65 518 bytes of payload per chunk.
pub(crate) const NOISE_MAX_PLAINTEXT_CHUNK: usize = 65_535 - 16;

/// Hard cap on a single reassembled noise reliable message, in bytes.
/// Well above any expected legitimate sunset payload (chat post +
/// inline image, presence, sync digest) but well under "memory
/// exhaustion" territory. Hitting this cap surfaces as
/// `Error::Transport("chunked: oversized message")` from
/// `recv_reliable` and leaves the connection in an indeterminate
/// state; the caller should close.
pub(crate) const NOISE_MAX_REASSEMBLED_MESSAGE: usize = 16 * 1024 * 1024;
```

- [ ] **Step 2: Introduce `NoiseInner<C>` (private) implementing `TransportConnection`**

Insert the following just before the existing `pub struct NoiseConnection<C: RawConnection>` definition:

```rust
/// Per-chunk noise transport-mode encrypt/decrypt. Implements
/// `TransportConnection` and is wrapped by `ChunkedConnection`
/// inside `NoiseConnection`. Each call to `send_reliable` /
/// `recv_reliable` handles exactly one snow message (≤
/// `NOISE_MAX_PLAINTEXT_CHUNK` plaintext + 16-byte tag); the chunker
/// upstream enforces the size precondition.
pub(crate) struct NoiseInner<C: RawConnection> {
    raw: C,
    state: Arc<Mutex<TransportState>>,
    peer_id: PeerId,
}

#[async_trait(?Send)]
impl<C: RawConnection> TransportConnection for NoiseInner<C> {
    async fn send_reliable(&self, bytes: Bytes) -> sunset_sync::Result<()> {
        let mut buf = vec![0u8; bytes.len() + 16];
        let n = {
            let mut state = self.state.lock().await;
            state
                .write_message(&bytes, &mut buf)
                .map_err(|e| sunset_sync::Error::Transport(format!("noise encrypt: {e:?}")))?
        };
        self.raw
            .send_reliable(Bytes::copy_from_slice(&buf[..n]))
            .await
    }

    async fn recv_reliable(&self) -> sunset_sync::Result<Bytes> {
        let ct = self.raw.recv_reliable().await?;
        let mut pt = vec![0u8; ct.len()];
        let n = {
            let mut state = self.state.lock().await;
            state
                .read_message(&ct, &mut pt)
                .map_err(|e| sunset_sync::Error::Transport(format!("noise decrypt: {e:?}")))?
        };
        Ok(Bytes::copy_from_slice(&pt[..n]))
    }

    async fn send_unreliable(&self, bytes: Bytes) -> sunset_sync::Result<()> {
        self.raw.send_unreliable(bytes).await
    }

    async fn recv_unreliable(&self) -> sunset_sync::Result<Bytes> {
        self.raw.recv_unreliable().await
    }

    fn peer_id(&self) -> PeerId {
        self.peer_id.clone()
    }

    async fn close(&self) -> sunset_sync::Result<()> {
        self.raw.close().await
    }
}
```

- [ ] **Step 3: Run the existing noise tests to confirm nothing regressed**

```
nix develop --command cargo test -p sunset-noise
```

Expected: existing tests (including `noise_handshake_roundtrip`) still pass. (The new `NoiseInner` is defined but not yet wired into `NoiseConnection` — that happens in Task 7.)

- [ ] **Step 4: Clippy + fmt**

```
nix develop --command cargo clippy -p sunset-noise --all-features --all-targets -- -D warnings
nix develop --command cargo fmt -p sunset-noise --check
```

If clippy complains about `NoiseInner` being unused (dead-code warning), that's expected — Task 7 wires it in immediately and the lint will clear there. If clippy fails the build for it, the fix in Task 7 will make it pass — do NOT add `#[allow(dead_code)]`; instead, stage Tasks 6 and 7 as one logical change by deferring this clippy check until the end of Task 7.

If clippy passes (because the `pub(crate)` use is enough), proceed to Step 5.

- [ ] **Step 5: Commit**

```
git add crates/sunset-noise/src/handshake.rs
git commit -m "$(cat <<'EOF'
sunset-noise: extract NoiseInner per-chunk encrypt/decrypt

Pulls the per-snow-message encrypt/decrypt out of NoiseConnection
into a private NoiseInner<C> that implements TransportConnection,
plus two new pub(crate) constants for the noise plaintext-chunk size
(snow's MAXMSGLEN minus 16-byte tag) and a 16 MiB reassembly cap.
No behaviour change yet — NoiseConnection still holds raw/state/peer_id
directly. Task 7 swaps it to ChunkedConnection<NoiseInner<C>>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Reshape `NoiseConnection<C>` to compose `ChunkedConnection<NoiseInner<C>>`

The shim is now thin. After this task, `NoiseConnection::send_reliable` accepts arbitrary-size payloads transparently.

**Files:**
- Modify: `crates/sunset-noise/src/handshake.rs`
- Modify: `crates/sunset-noise/Cargo.toml` (only if `sunset-sync`'s `ChunkedConnection` isn't already imported via path; verify first)

- [ ] **Step 1: Confirm the sunset-noise → sunset-sync dependency direction**

```
nix develop --command bash -c "grep -n 'sunset-sync' crates/sunset-noise/Cargo.toml"
```

Expected: at least one line referencing `sunset-sync.workspace = true` (or similar) so `use sunset_sync::ChunkedConnection;` resolves. If absent, add `sunset-sync.workspace = true` to the `[dependencies]` table and re-run the grep to confirm.

- [ ] **Step 2: Replace `NoiseConnection<C>` and its `TransportConnection` impl with the chunked-composition form**

In `crates/sunset-noise/src/handshake.rs`, find the existing `pub struct NoiseConnection<C: RawConnection> { ... }` and its `impl ... TransportConnection for NoiseConnection<C>` block. Replace both with:

```rust
/// Authenticated, encrypted, **chunked** connection. `send_reliable`
/// / `recv_reliable` accept arbitrary-size payloads (up to
/// `NOISE_MAX_REASSEMBLED_MESSAGE` reassembled) by composing
/// `ChunkedConnection<NoiseInner<C>>` internally — callers see no
/// per-message size limit.
pub struct NoiseConnection<C: RawConnection> {
    chunked: sunset_sync::ChunkedConnection<NoiseInner<C>>,
    peer_id: PeerId,
}

impl<C: RawConnection> NoiseConnection<C> {
    /// Construct a `NoiseConnection` from an already-completed noise
    /// handshake. Used by `do_handshake_initiator` /
    /// `do_handshake_responder` and by tests that drive a paired
    /// `TransportState` directly.
    pub fn from_handshake(raw: C, transport: TransportState, peer_id: PeerId) -> Self {
        let inner = NoiseInner {
            raw,
            state: Arc::new(Mutex::new(transport)),
            peer_id: peer_id.clone(),
        };
        let chunked = sunset_sync::ChunkedConnection::new(
            inner,
            NOISE_MAX_PLAINTEXT_CHUNK,
            NOISE_MAX_REASSEMBLED_MESSAGE,
        );
        Self { chunked, peer_id }
    }
}

#[async_trait(?Send)]
impl<C: RawConnection> TransportConnection for NoiseConnection<C> {
    async fn send_reliable(&self, bytes: Bytes) -> sunset_sync::Result<()> {
        self.chunked.send_reliable(bytes).await
    }
    async fn recv_reliable(&self) -> sunset_sync::Result<Bytes> {
        self.chunked.recv_reliable().await
    }
    async fn send_unreliable(&self, bytes: Bytes) -> sunset_sync::Result<()> {
        self.chunked.send_unreliable(bytes).await
    }
    async fn recv_unreliable(&self) -> sunset_sync::Result<Bytes> {
        self.chunked.recv_unreliable().await
    }
    fn peer_id(&self) -> PeerId {
        self.peer_id.clone()
    }
    async fn close(&self) -> sunset_sync::Result<()> {
        self.chunked.close().await
    }
}
```

- [ ] **Step 3: Update the two handshake construction sites**

In the same file, find each of the two existing `Ok(NoiseConnection { raw, state: Arc::new(Mutex::new(transport)), peer_id })` struct-literal expressions (one in `do_handshake_initiator`, one in `do_handshake_responder`) and replace each with:

```rust
    Ok(NoiseConnection::from_handshake(raw, transport, peer_id))
```

- [ ] **Step 4: Run the noise crate's existing tests**

```
nix develop --command cargo test -p sunset-noise
```

Expected: all existing tests still pass (including `noise_handshake_roundtrip` — the chunker is transparent for small payloads).

- [ ] **Step 5: Clippy + fmt**

```
nix develop --command cargo clippy -p sunset-noise --all-features --all-targets -- -D warnings
nix develop --command cargo fmt -p sunset-noise --check
```

Both must pass (the `NoiseInner` dead-code warning from Task 6 should clear here since it is now used by `NoiseConnection::from_handshake`).

- [ ] **Step 6: Commit**

```
git add crates/sunset-noise/src/handshake.rs crates/sunset-noise/Cargo.toml
git commit -m "$(cat <<'EOF'
sunset-noise: NoiseConnection composes ChunkedConnection internally

NoiseConnection<C> now holds ChunkedConnection<NoiseInner<C>> and
forwards the TransportConnection trait through it. The 65 535-byte
per-message noise limit is no longer a leaky property of the public
API — send_reliable accepts arbitrary-size payloads (up to the
16 MiB reassembly cap) and recv_reliable transparently reassembles
them. Continuation framing rides inside the AEAD envelope so
observers can't see chunk boundaries or tamper with the flag.

Existing handshake test (sub-65 KB roundtrip) still passes. Large
payload integration test follows in Task 8.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Integration test in `sunset-noise` — 4 MB roundtrip

**Files:**
- Modify: `crates/sunset-noise/src/handshake.rs` (one new test in `mod tests`)

- [ ] **Step 1: Write the test**

Add inside `mod tests`, after the existing `noise_handshake_roundtrip` test:

```rust
#[tokio::test(flavor = "current_thread")]
async fn noise_send_recv_handles_4mb_payload() {
    // Pre-chunking, this errored at snow.write_message because 4 MB
    // is well past MAXMSGLEN = 65535. With the chunker composed
    // inside NoiseConnection, a single send_reliable / recv_reliable
    // pair transparently splits + reassembles ~65 chunks worth of
    // ciphertext.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let alice = Arc::new(StaticIdentity { seed: [1u8; 32] });
            let bob = Arc::new(StaticIdentity { seed: [2u8; 32] });

            let (a_pipe, b_pipe) = make_pipe_pair();

            let bob_x25519_secret = ed25519_seed_to_x25519_secret(&bob.seed);
            use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
            let bob_x25519_pub: [u8; 32] = {
                let scalar = Scalar::from_bytes_mod_order(*bob_x25519_secret);
                MontgomeryPoint::mul_base(&scalar).to_bytes()
            };

            let alice_handle = tokio::task::spawn_local({
                let alice_id = alice.clone();
                async move { do_handshake_initiator(a_pipe, alice_id, bob_x25519_pub).await }
            });
            let bob_handle = tokio::task::spawn_local({
                let bob_id = bob.clone();
                async move { do_handshake_responder(b_pipe, bob_id).await }
            });

            let alice_conn = alice_handle.await.unwrap().expect("alice handshake");
            let bob_conn = bob_handle.await.unwrap().expect("bob handshake");

            // 4 MiB of deterministic content (every byte distinct
            // mod 256 from its neighbours so a reordering would show
            // up immediately).
            let n = 4 * 1024 * 1024;
            let payload: Vec<u8> = (0..n).map(|i| (i.wrapping_mul(31) as u8)).collect();
            let payload = Bytes::from(payload);

            alice_conn.send_reliable(payload.clone()).await.unwrap();
            let received = bob_conn.recv_reliable().await.unwrap();
            assert_eq!(received.len(), payload.len(), "length mismatch");
            assert_eq!(received, payload, "content mismatch");
        })
        .await;
}
```

- [ ] **Step 2: Run it**

```
nix develop --command cargo test -p sunset-noise tests::noise_send_recv_handles_4mb_payload
```

Expected: PASS. (May take a couple of seconds — ~65 chunks each encrypt/decrypt cycle.)

- [ ] **Step 3: Commit**

```
git add crates/sunset-noise/src/handshake.rs
git commit -m "$(cat <<'EOF'
sunset-noise: 4 MiB roundtrip test exercises ChunkedConnection inside

Single send_reliable / recv_reliable on a real (in-memory) noise
connection moves a 4 MiB payload — well past snow's 65 535-byte
per-message ceiling. Before the chunker landed this would error
at snow.write_message; now it transparently splits + reassembles
~65 chunks each direction.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: End-to-end integration test — 2 MB EventDelivery over real WebSocket + noise

Set up two `NoiseTransport`s over a real localhost WebSocket (axum on the server side, tokio-tungstenite on the dialer side) and round-trip a 2 MiB `SyncMessage::EventDelivery`. This exercises the chunker against real frame-by-frame WS I/O rather than an in-memory pipe.

**Files:**
- Create: `crates/sunset-sync-ws-native/tests/two_peer_ws_noise_large.rs`

- [ ] **Step 1: Write the new test file**

Create `crates/sunset-sync-ws-native/tests/two_peer_ws_noise_large.rs`:

```rust
//! Round-trip a 2 MiB SyncMessage::EventDelivery between two peers
//! over a real localhost WebSocket wrapped in Noise. The payload is
//! sized so the per-connection ChunkedConnection inside
//! NoiseConnection must fire ~32 times each direction; before that
//! decorator landed, the send would error at snow.write_message and
//! never reach the wire.

use std::sync::Arc;

use axum::routing::get;
use bytes::Bytes;
use zeroize::Zeroizing;

use sunset_noise::{NoiseIdentity, NoiseTransport, ed25519_seed_to_x25519_secret};
use sunset_store::{ContentBlock, Hash, SignedKvEntry, VerifyingKey};
use sunset_sync::{PeerAddr, SyncMessage, Transport, TransportConnection};
use sunset_sync_ws_native::{WebSocketRawTransport, axum_integration};

/// Minimal identity used only to drive the Noise handshake — no
/// sunset-core dependency, no signing path. Mirrors the
/// `StaticIdentity` in `sunset-noise`'s own tests.
struct StaticIdentity {
    seed: [u8; 32],
}
impl NoiseIdentity for StaticIdentity {
    fn ed25519_public(&self) -> [u8; 32] {
        use ed25519_dalek::SigningKey;
        SigningKey::from_bytes(&self.seed).verifying_key().to_bytes()
    }
    fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.seed)
    }
}

fn x25519_pub_for(seed: &[u8; 32]) -> [u8; 32] {
    use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
    let secret = ed25519_seed_to_x25519_secret(seed);
    let scalar = Scalar::from_bytes_mod_order(*secret);
    MontgomeryPoint::mul_base(&scalar).to_bytes()
}

#[tokio::test(flavor = "current_thread")]
async fn large_payload_over_ws_noise() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let alice = Arc::new(StaticIdentity { seed: [1u8; 32] });
            let bob = Arc::new(StaticIdentity { seed: [2u8; 32] });

            // ---- bob serves via axum on a random local port ----
            let (bob_raw, ws_tx) = WebSocketRawTransport::serving();
            let app = axum::Router::new().route(
                "/",
                get({
                    let ws_tx = ws_tx.clone();
                    move |ws: axum::extract::WebSocketUpgrade| {
                        axum_integration::ws_handler(ws, ws_tx.clone())
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bob_bound = listener.local_addr().unwrap();
            let _serve_handle = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let bob_noise = NoiseTransport::new(bob_raw, bob.clone());

            // ---- alice dials ----
            let alice_raw = WebSocketRawTransport::dial_only();
            let alice_noise = NoiseTransport::new(alice_raw, alice.clone());

            let bob_x25519_pub = x25519_pub_for(&bob.seed);
            let bob_addr = PeerAddr::new(Bytes::from(format!(
                "ws://{}#x25519={}",
                bob_bound,
                hex::encode(bob_x25519_pub),
            )));

            // ---- handshake (parallel: alice connects, bob accepts) ----
            let bob_accept = tokio::task::spawn_local(async move {
                bob_noise.accept().await
            });
            let alice_conn = alice_noise
                .connect(bob_addr)
                .await
                .expect("alice connect+handshake");
            let bob_conn = bob_accept
                .await
                .expect("bob accept task")
                .expect("bob handshake");

            // ---- build a ~2 MiB SyncMessage::EventDelivery ----
            let n = 2 * 1024 * 1024;
            let big: Vec<u8> = (0..n).map(|i| (i.wrapping_mul(17) as u8)).collect();
            let block = ContentBlock {
                data: Bytes::from(big),
                references: Vec::new(),
            };
            let block_hash: Hash = block.hash();
            let entry = SignedKvEntry {
                verifying_key: VerifyingKey::new(Bytes::copy_from_slice(&[7u8; 32])),
                name: Bytes::from_static(b"large/integration/test"),
                value_hash: block_hash,
                priority: 1,
                expires_at: None,
                signature: Bytes::copy_from_slice(&[0u8; 64]),
            };
            let msg = SyncMessage::EventDelivery {
                entries: vec![entry],
                blobs: vec![block],
            };
            let encoded = msg.encode().expect("encode");
            assert!(
                encoded.len() > 2 * 1024 * 1024,
                "encoded payload should be > 2 MiB; was {}",
                encoded.len()
            );

            // ---- send + receive + decode + assert ----
            alice_conn
                .send_reliable(encoded.clone())
                .await
                .expect("alice send_reliable");
            let received = bob_conn
                .recv_reliable()
                .await
                .expect("bob recv_reliable");
            assert_eq!(received.len(), encoded.len(), "wire length mismatch");
            assert_eq!(received, encoded, "wire content mismatch");
            let decoded = SyncMessage::decode(&received).expect("decode");
            assert_eq!(decoded, msg, "SyncMessage round-trip mismatch");
        })
        .await;
}
```

- [ ] **Step 2: Verify the new dev-dependencies are all already in `Cargo.toml`**

```
nix develop --command bash -c "grep -E 'axum|hex|curve25519-dalek|ed25519-dalek|sunset-store|sunset-noise|sunset-sync|tokio|zeroize|bytes' crates/sunset-sync-ws-native/Cargo.toml"
```

Expected: all of `axum`, `bytes`, `curve25519-dalek`, `ed25519-dalek`, `hex`, `sunset-noise`, `sunset-store`, `sunset-sync`, `tokio`, `zeroize` appear in either `[dependencies]` or `[dev-dependencies]`. If any are missing from dev-deps (the existing `two_peer_ws_noise.rs` already uses most of them, so this is likely a no-op), add them:

```toml
# crates/sunset-sync-ws-native/Cargo.toml — [dev-dependencies] table
axum = { workspace = true }
bytes = { workspace = true }
curve25519-dalek = { workspace = true }
ed25519-dalek = { workspace = true }
hex = { workspace = true }
sunset-store = { workspace = true }
zeroize = { workspace = true }
# (sunset-noise, sunset-sync, tokio are likely already present)
```

- [ ] **Step 3: Run the new test**

```
nix develop --command cargo test -p sunset-sync-ws-native --test two_peer_ws_noise_large
```

Expected: PASS. May take a few seconds (~32 chunks each way, real tungstenite WS frames).

- [ ] **Step 4: Run the existing two-peer test to confirm nothing regressed**

```
nix develop --command cargo test -p sunset-sync-ws-native --test two_peer_ws_noise
```

Expected: PASS (the small-payload path is unchanged).

- [ ] **Step 5: Clippy + fmt on the whole workspace, since changes have touched two crates**

```
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
nix develop --command bash scripts/check-no-clippy-allow.sh
```

All three must pass.

- [ ] **Step 6: Run the full workspace test suite**

```
nix develop --command cargo test --workspace --all-features
```

Expected: all tests pass. If any unrelated test fails, see if it's a known flake (`docs/superpowers/` or git history may mention it); if not, investigate before continuing.

- [ ] **Step 7: Commit**

```
git add crates/sunset-sync-ws-native/tests/two_peer_ws_noise_large.rs
git commit -m "$(cat <<'EOF'
sunset-sync-ws-native: 2 MiB EventDelivery e2e test

Mirror of two_peer_ws_noise but with a SyncMessage::EventDelivery
carrying a ContentBlock of ~2 MiB random data, decoded byte-for-byte
on the receiving peer. Exercises the full stack — sunset-sync
postcard encode → chunked noise (~32 chunks each way) → tungstenite
WS framing → tungstenite WS framing → chunked noise → postcard
decode — at a payload size that is unreachable without the
NoiseConnection chunker. Catches regressions if anyone reintroduces
a per-message size assumption above sunset-noise.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: Image e2e — synthetic ~300 KB image roundtrip

Adds one Playwright case to `web/e2e/images.spec.js` that drives the real browser + wasm + relay stack with a payload over the old 65 KB Noise ceiling. The image is a synthetic PNG generated at test time so the spec stays deterministic and dependency-free.

**Files:**
- Modify: `web/e2e/images.spec.js`

- [ ] **Step 1: Add a helper that builds a ~300 KB valid PNG**

In `web/e2e/images.spec.js`, near the existing `fileFrom` helper, add:

```javascript
/// Build a synthetic ~300 KB PNG by repeating the 1×1 red PNG's
/// IDAT chunk data inside a 600×600 single-colour image. Result is a
/// real, browser-decodable PNG (the receiver's `<img>` actually
/// renders it) — necessary so this test exercises the same
/// data-URL render path as the 1×1 fixtures, not a fake byte blob.
///
/// We don't depend on a pre-canned ~300 KB binary: keeping the
/// fixture programmatic means the spec has zero on-disk assets and
/// the size knob is `widthHeight` here, not a file we have to
/// regenerate.
function makeLargePng(widthHeight = 600) {
  const w = widthHeight, h = widthHeight;
  // PNG signature
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  function chunk(type, data) {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length, 0);
    const typeBuf = Buffer.from(type, "ascii");
    // Trivial CRC: the browser tolerates an incorrect CRC on some
    // chunks, but `pngcrush`-strict decoders won't. Compute it.
    const crc = pngCrc(Buffer.concat([typeBuf, data]));
    return Buffer.concat([len, typeBuf, data, crc]);
  }
  // IHDR: w, h, bit depth 8, color type 2 (RGB), compression 0,
  // filter 0, interlace 0.
  const ihdrData = Buffer.alloc(13);
  ihdrData.writeUInt32BE(w, 0);
  ihdrData.writeUInt32BE(h, 4);
  ihdrData[8] = 8; ihdrData[9] = 2;
  // IDAT: deflate-stored block(s) of (w*3 + 1) bytes per scanline
  // (filter byte + RGB triples). Pure red.
  const scanline = Buffer.alloc(1 + w * 3);
  for (let x = 0; x < w; x++) {
    scanline[1 + x * 3] = 0xff;     // R
    scanline[1 + x * 3 + 1] = 0x00; // G
    scanline[1 + x * 3 + 2] = 0x00; // B
  }
  const raw = Buffer.alloc(scanline.length * h);
  for (let y = 0; y < h; y++) scanline.copy(raw, y * scanline.length);
  // Wrap in a minimal zlib stream: 0x78 0x01 (header) + stored
  // deflate blocks + Adler32. Use node's zlib for correctness.
  const zlib = require("zlib");
  const idat = zlib.deflateSync(raw, { level: 9 });
  const ihdr = chunk("IHDR", ihdrData);
  const idatChunk = chunk("IDAT", idat);
  const iend = chunk("IEND", Buffer.alloc(0));
  return Buffer.concat([sig, ihdr, idatChunk, iend]);
}

/// Streaming CRC32 over the bytes in `buf` using the PNG-standard
/// polynomial (0xedb88320). Pulled in inline so we don't introduce
/// a new dev-dep just for one test.
function pngCrc(buf) {
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    crc = (crc ^ buf[i]) >>> 0;
    for (let k = 0; k < 8; k++) {
      crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
    }
  }
  crc = (crc ^ 0xffffffff) >>> 0;
  const out = Buffer.alloc(4);
  out.writeUInt32BE(crc, 0);
  return out;
}
```

- [ ] **Step 2: Add a test case that sends the synthetic image**

Append to `web/e2e/images.spec.js`, after the existing four tests:

```javascript
test("large image (~300 KB) survives noise chunking end-to-end", async ({
  browser,
}) => {
  const { ctx: ctxA, page: pageA, composer: inputA } = await openPage(
    browser,
    "large",
  );
  const { ctx: ctxB, page: pageB } = await openPage(browser, "large");

  // 600×600 pure-red PNG. Deflate of 600 identical scanlines is
  // very efficient (~200-400 KB depending on level=9), but well
  // past the 65 KB ceiling and the base64 inflation pushes the
  // on-wire payload further still.
  const bigPng = makeLargePng(600);
  expect(bigPng.length).toBeGreaterThan(150_000);

  await stageImages(pageA, [
    { name: "big-red.png", mimeType: "image/png", buffer: bigPng },
  ]);
  await expect(pageA.getByTestId("composer-attachment")).toHaveCount(1, {
    timeout: 15_000,
  });

  const text = `large-image — ${Date.now()}`;
  await inputA.fill(text);
  await inputA.press("Enter");

  // Sender's composer clears.
  await expect(pageA.getByTestId("composer-attachment")).toHaveCount(0, {
    timeout: 15_000,
  });

  // Both sides render the image. Use a longer timeout: the chunker
  // does ~5 noise round-trips for a ~300 KB payload and there's
  // also store-insert + relay-forward latency.
  await expect(pageB.getByText(text)).toBeVisible({ timeout: 30_000 });
  await expect(pageB.getByTestId("message-image")).toHaveCount(1, {
    timeout: 30_000,
  });
  await expect(pageA.getByTestId("message-image")).toHaveCount(1, {
    timeout: 30_000,
  });

  // Byte-for-byte check on the receiver: the base64 in the `<img src>`
  // must equal the base64 of what the sender staged.
  const expectedBase64 = bigPng.toString("base64");
  const src = await pageB.getByTestId("message-image").first().getAttribute("src");
  expect(src).toBe(`data:image/png;base64,${expectedBase64}`);

  await ctxA.close();
  await ctxB.close();
});
```

- [ ] **Step 3: Run the new test (only)**

```
nix run .#web-test -- --grep "large image"
```

Expected: PASS in both `chromium` and `mobile-chrome` projects.

- [ ] **Step 4: Run the whole image e2e suite to confirm no regression**

```
nix run .#web-test -- --grep "image"
```

Expected: all 5 image tests pass × 2 projects = 10 PASS.

- [ ] **Step 5: Commit**

```
git add web/e2e/images.spec.js
git commit -m "$(cat <<'EOF'
e2e: ~300 KB image roundtrip pins noise chunker against regression

Existing image specs use 1×1 PNG/GIF fixtures (~75 bytes base64) —
well under snow's 65 535-byte ceiling and useless for catching a
re-introduction of the chunking gap. New test programmatically
builds a 600×600 pure-red PNG (200+ KB on the wire) and asserts
both sender and receiver render byte-for-byte the same image.

Exercises the full stack: real browser file picker → Gleam composer
→ sunset-core compose_post → AEAD-encrypted ContentBlock → sunset-
sync EventDelivery → chunked noise (~5 chunks each way) → relay
forward → chunked noise → decode → Gleam render.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 11: Final verification + PR

- [ ] **Step 1: Full workspace test pass**

```
nix develop --command cargo test --workspace --all-features
```

Expected: clean pass.

- [ ] **Step 2: Full lint pass**

```
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
nix develop --command bash scripts/check-no-clippy-allow.sh
nix develop --command bash scripts/check-desktop-lints.sh
```

All four must pass with no output (or only the cargo-clippy "Finished" lines).

- [ ] **Step 3: Whole image e2e suite once more**

```
nix run .#web-test -- --grep "image"
```

Expected: 10 PASS.

- [ ] **Step 4: Push the branch and open a PR**

```
git push -u origin debug/image-rendering
gh pr create --title "noise: transparent chunking for arbitrary-size reliable payloads" --body "$(cat <<'EOF'
## Summary

- Generic `ChunkedConnection<C: TransportConnection>` in `sunset-sync` splits arbitrary-size reliable payloads into ≤`max_chunk_size`-byte frames (1-byte continuation flag + payload), sends each via the inner connection, and reassembles on the receive side up to a hard cap. Unreliable channel passes straight through.
- `NoiseConnection<C>` (in `sunset-noise`) now composes `ChunkedConnection<NoiseInner<C>>` internally. The previously-leaky 65 535-byte snow ceiling stops being a property of the public API — `send_reliable` accepts arbitrary-size payloads transparently. Continuation framing lives inside the AEAD envelope so observers can't see chunk boundaries.
- Fixes the user-visible image disconnect-loop on master: sending any image larger than ~65 KB used to fail at `snow.write_message`, tear down the peer connection, and reconnect into the same failure on the supervisor's next attempt.

## Test plan

- [ ] `cargo test -p sunset-sync chunked::tests` — 8 unit tests (empty/single/multi-chunk roundtrip, concurrent sends, oversized rejection, malformed frames, unreliable pass-through)
- [ ] `cargo test -p sunset-noise` — existing handshake test + new 4 MiB roundtrip
- [ ] `cargo test -p sunset-sync-ws-native --test two_peer_ws_noise_large` — 2 MiB EventDelivery over real WebSocket + noise
- [ ] `cargo test --workspace --all-features` — clean
- [ ] `cargo clippy --workspace --all-features --all-targets -- -D warnings` — clean
- [ ] `cargo fmt --all --check` — clean
- [ ] `nix run .#web-test -- --grep image` — 5 image specs × 2 projects = 10 pass, including the new ~300 KB synthetic-PNG roundtrip

## Design

`docs/superpowers/specs/2026-05-13-noise-transport-chunking-design.md`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 5: Report the PR URL back to the human reviewer.**
