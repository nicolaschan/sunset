//! `quinn::AsyncUdpSocket` wrapper that siphons holepunch probes
//! (datagrams whose first 4 bytes equal [`crate::wire::MAGIC`]) off the
//! quinn data path and forwards them to a separate channel.

use std::fmt;
use std::io;
use std::io::IoSliceMut;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, Runtime, TokioRuntime, UdpPoller};
use tokio::sync::mpsc;

use crate::wire::MAGIC;

/// `quinn::AsyncUdpSocket` impl that interposes on `poll_recv` to route
/// magic-prefixed probe datagrams to a side channel. QUIC traffic
/// (everything not prefixed with [`MAGIC`]) passes through to the
/// underlying delegate socket unchanged.
pub struct HolepunchSocket {
    delegate: Arc<dyn AsyncUdpSocket>,
    probe_tx: mpsc::UnboundedSender<(SocketAddr, Bytes)>,
}

impl fmt::Debug for HolepunchSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HolepunchSocket")
            .field("local_addr", &self.delegate.local_addr().ok())
            .finish()
    }
}

impl HolepunchSocket {
    /// Wrap a `std::net::UdpSocket` using quinn's [`TokioRuntime`]
    /// adapter as the underlying I/O. Probes (first 4 bytes ==
    /// [`MAGIC`]) are routed to `probe_tx`; other datagrams flow on to
    /// quinn.
    pub fn new(
        udp: std::net::UdpSocket,
        probe_tx: mpsc::UnboundedSender<(SocketAddr, Bytes)>,
    ) -> io::Result<Self> {
        udp.set_nonblocking(true)?;
        let delegate = TokioRuntime.wrap_udp_socket(udp)?;
        Ok(Self { delegate, probe_tx })
    }

    /// Send a holepunch probe (bypasses any per-connection QUIC state).
    /// Uses the underlying socket so the probe shares the same 5-tuple
    /// as QUIC traffic — essential for the NAT mapping to be consistent.
    pub fn try_send_probe(&self, dst: SocketAddr, bytes: &[u8]) -> io::Result<()> {
        let transmit = Transmit {
            destination: dst,
            ecn: None,
            contents: bytes,
            segment_size: None,
            src_ip: None,
        };
        self.delegate.try_send(&transmit)
    }
}

impl AsyncUdpSocket for HolepunchSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Arc::clone(&self.delegate).create_io_poller()
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        self.delegate.try_send(transmit)
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            let n = match self.delegate.poll_recv(cx, bufs, meta) {
                Poll::Ready(Ok(n)) => n,
                other => return other,
            };
            let mut quinn_segments = 0usize;
            for i in 0..n {
                let len = meta[i].len;
                let is_probe = len >= MAGIC.len() && bufs[i][..MAGIC.len()] == MAGIC;
                if is_probe {
                    let bytes = Bytes::copy_from_slice(&bufs[i][..len]);
                    let _ = self.probe_tx.send((meta[i].addr, bytes));
                } else {
                    if quinn_segments != i {
                        let (left, right) = bufs.split_at_mut(i);
                        let dst = &mut left[quinn_segments];
                        let src = &right[0];
                        dst[..len].copy_from_slice(&src[..len]);
                        meta[quinn_segments] = meta[i];
                    }
                    quinn_segments += 1;
                }
            }
            if quinn_segments > 0 {
                return Poll::Ready(Ok(quinn_segments));
            }
            // All n segments were probes. Loop and re-poll the delegate;
            // it'll return Pending if its buffers are empty, which we
            // propagate. If it has more buffered we keep draining.
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.delegate.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.delegate.max_transmit_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.delegate.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.delegate.may_fragment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::wire::{Probe, ProbeRole};

    /// Send a magic-prefixed probe datagram to a HolepunchSocket and
    /// confirm it lands on the probe channel instead of being handed
    /// to quinn. The pump task drives poll_recv until cancelled.
    #[tokio::test(flavor = "current_thread")]
    async fn magic_prefixed_datagram_is_routed_to_probe_channel() {
        let listener_std = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let listener_addr = listener_std.local_addr().unwrap();
        let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
        let hole = Arc::new(HolepunchSocket::new(listener_std, probe_tx).unwrap());

        let hole_pump = Arc::clone(&hole);
        let pump = tokio::spawn(async move {
            let mut storage = vec![0u8; 2048];
            loop {
                let mut bufs = [IoSliceMut::new(&mut storage)];
                let mut meta = [RecvMeta::default()];
                let _ =
                    std::future::poll_fn(|cx| hole_pump.poll_recv(cx, &mut bufs, &mut meta)).await;
            }
        });

        let p = Probe {
            session_id: [1u8; 16],
            role: ProbeRole::Ping,
            sender_pk: [2u8; 32],
            nonce: [3u8; 16],
        };
        let wire = p.encode().unwrap();
        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender.send_to(&wire, listener_addr).await.unwrap();

        let (src, body) = tokio::time::timeout(std::time::Duration::from_secs(2), probe_rx.recv())
            .await
            .expect("probe channel timed out")
            .expect("probe channel closed");
        assert!(src.ip().is_loopback(), "got src {src:?}");
        assert_eq!(&body[..MAGIC.len()], &MAGIC);
        let decoded = Probe::decode(&body).unwrap().unwrap();
        assert_eq!(decoded, p);

        pump.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_magic_datagram_returned_to_quinn_path() {
        // Send a datagram that does NOT start with MAGIC. Poll the
        // HolepunchSocket once and confirm we get a non-zero return
        // value (quinn would receive the bytes).
        let listener_std = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let listener_addr = listener_std.local_addr().unwrap();
        let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
        let hole = Arc::new(HolepunchSocket::new(listener_std, probe_tx).unwrap());

        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender
            .send_to(&[0xab, 0xcd, 0xef, 0x01, 0x02], listener_addr)
            .await
            .unwrap();

        let mut storage = vec![0u8; 2048];
        let mut bufs = [IoSliceMut::new(&mut storage)];
        let mut meta = [RecvMeta::default()];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            std::future::poll_fn(|cx| hole.poll_recv(cx, &mut bufs, &mut meta)).await
        })
        .await
        .expect("poll_recv timed out")
        .expect("poll_recv err");
        assert!(n >= 1);
        assert_eq!(&storage[..meta[0].len], &[0xab, 0xcd, 0xef, 0x01, 0x02]);
        assert!(probe_rx.try_recv().is_err(), "probe channel must be empty");
    }
}
