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

/// Wraps any `TransportConnection` and adds reliable-channel
/// chunking and reassembly. The unreliable channel passes straight
/// through (its payloads — datagrams — are already small).
///
/// Two mutexes preserve the "one logical message in, one out" contract
/// under concurrent use:
///
/// - `send_lock` serialises multi-chunk sends so two concurrent callers
///   can't interleave their chunk streams on the inner connection.
/// - `recv_lock` serialises multi-chunk reassemblies so two concurrent
///   receivers can't each consume part of the same logical message.
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

    async fn roundtrip(
        a: &ChunkedConnection<PipeConnection>,
        b: &ChunkedConnection<PipeConnection>,
        payload: Bytes,
    ) {
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

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_sends_do_not_interleave() {
        // Two concurrent multi-chunk sends from the same side must
        // each arrive intact on the receiver, not interleaved at the
        // inner frame level. Without `send_lock` the chunker would
        // race and the receiver would deframe one corrupted blob.
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
}
