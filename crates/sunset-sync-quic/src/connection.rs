//! `RawConnection` over a single `quinn::Connection`: one persistent
//! bidi stream framed with a 4-byte big-endian length prefix
//! (reliable) plus QUIC datagrams (unreliable, cap 1200 B).

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::Mutex;

use sunset_sync::{Error as SyncError, RawConnection, Result as SyncResult};

pub const MAX_DATAGRAM_PAYLOAD: usize = 1200;
const MAX_RELIABLE_FRAME: usize = 16 * 1024 * 1024;

/// A `RawConnection` implemented on top of a single
/// [`quinn::Connection`] with one persistent bidi stream and QUIC
/// datagrams.
pub struct QuicRawConnection {
    connection: quinn::Connection,
    send: Mutex<quinn::SendStream>,
    recv: Mutex<quinn::RecvStream>,
}

impl QuicRawConnection {
    pub fn new(
        connection: quinn::Connection,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
    ) -> Self {
        Self {
            connection,
            send: Mutex::new(send),
            recv: Mutex::new(recv),
        }
    }

    /// The peer's address at the QUIC layer. After holepunch this is
    /// the confirmed working candidate.
    pub fn remote_addr(&self) -> std::net::SocketAddr {
        self.connection.remote_address()
    }
}

#[async_trait(?Send)]
impl RawConnection for QuicRawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
        if bytes.len() > MAX_RELIABLE_FRAME {
            return Err(SyncError::Transport(format!(
                "quic send_reliable: frame too large ({} > {MAX_RELIABLE_FRAME})",
                bytes.len()
            )));
        }
        let len = u32::try_from(bytes.len())
            .map_err(|_| SyncError::Transport("quic send_reliable: len > u32::MAX".into()))?;
        let mut s = self.send.lock().await;
        s.write_all(&len.to_be_bytes())
            .await
            .map_err(|e| SyncError::Transport(format!("quic send len: {e}")))?;
        s.write_all(&bytes)
            .await
            .map_err(|e| SyncError::Transport(format!("quic send body: {e}")))?;
        Ok(())
    }

    async fn recv_reliable(&self) -> SyncResult<Bytes> {
        let mut r = self.recv.lock().await;
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf)
            .await
            .map_err(|e| SyncError::Transport(format!("quic recv len: {e}")))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_RELIABLE_FRAME {
            return Err(SyncError::Transport(format!(
                "quic recv_reliable: frame too large ({len} > {MAX_RELIABLE_FRAME})"
            )));
        }
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf)
            .await
            .map_err(|e| SyncError::Transport(format!("quic recv body: {e}")))?;
        Ok(Bytes::from(buf))
    }

    async fn send_unreliable(&self, bytes: Bytes) -> SyncResult<()> {
        if bytes.len() > MAX_DATAGRAM_PAYLOAD {
            return Err(SyncError::Transport(format!(
                "quic send_unreliable: payload too large ({} > {MAX_DATAGRAM_PAYLOAD})",
                bytes.len()
            )));
        }
        self.connection
            .send_datagram(bytes)
            .map_err(|e| SyncError::Transport(format!("quic send_datagram: {e}")))
    }

    async fn recv_unreliable(&self) -> SyncResult<Bytes> {
        let dg = self
            .connection
            .read_datagram()
            .await
            .map_err(|e| SyncError::Transport(format!("quic read_datagram: {e}")))?;
        Ok(dg)
    }

    async fn close(&self) -> SyncResult<()> {
        self.connection.close(0u32.into(), b"closed");
        Ok(())
    }
}
