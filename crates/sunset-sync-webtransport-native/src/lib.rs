//! Native WebTransport (HTTP/3 / QUIC) implementation of
//! [`sunset_sync::RawTransport`].
//!
//! Reliable channel = one persistent bidirectional QUIC stream framed with
//! a 4-byte big-endian length prefix per `SyncMessage`.
//!
//! Unreliable channel = QUIC datagrams. Each datagram is one whole
//! message; messages exceeding [`MAX_DATAGRAM_PAYLOAD`] return `Err` on
//! send rather than silently truncating.
//!
//! Wrap with [`sunset_noise::NoiseTransport`] to get the authenticated
//! encrypted layer expected by `SyncEngine`.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use wtransport::{Endpoint, RecvStream, SendStream, ServerConfig};
use wtransport::config::ClientConfig;
use wtransport::tls::Sha256Digest;

use sunset_sync::{
    Error as SyncError, PeerAddr, RawConnection, RawTransport, Result as SyncResult,
};

/// Hard ceiling for outbound datagram payload size. Matches the WebRTC
/// datachannel SCTP MTU we already use elsewhere; values above this are
/// likely to be silently dropped on path links with smaller MTUs anyway.
pub const MAX_DATAGRAM_PAYLOAD: usize = 1200;

/// 4-byte big-endian length prefix; messages above this can't be framed.
/// `u32::MAX` would let one bad write stall the whole stream pump for
/// tens of seconds, so we cap at 16 MiB — the same envelope `sunset-sync`
/// considers reasonable for a single SyncMessage.
const MAX_RELIABLE_FRAME: usize = 16 * 1024 * 1024;

mod address;
mod cert;

pub use address::{ParsedWebTransportAddr, parse_addr};
pub use cert::{parse_cert_hash_hex, sha256_digest_to_hex};

/// Either a dial-only client or a server-side transport whose `accept()`
/// drains a channel of WebTransport sessions accepted by an upstream
/// listener.
pub struct WebTransportRawTransport {
    mode: TransportMode,
}

enum TransportMode {
    /// Outbound dialer. Holds a wtransport [`Endpoint`] configured to
    /// accept self-signed certs via SHA-256 hash pinning. Note: each
    /// `connect()` builds a fresh client endpoint so the pinned hashes
    /// can vary per-call — the WT spec requires the hash list to be
    /// known *at handshake time*.
    DialOnly,
    /// Drains a channel of already-accepted [`wtransport::Connection`]s.
    /// Populated by an upstream relay-side accept loop (see
    /// `sunset-relay`'s `wt::serve`).
    Serving {
        rx: Mutex<mpsc::UnboundedReceiver<wtransport::Connection>>,
    },
}

impl WebTransportRawTransport {
    /// Outbound-only client transport. Each call to `connect()` builds a
    /// fresh wtransport [`Endpoint`] configured with the cert hashes
    /// supplied via the address fragment (`cert-sha256=<hex>`).
    pub fn dial_only() -> Self {
        Self {
            mode: TransportMode::DialOnly,
        }
    }

    /// Server-side transport whose `accept()` drains pre-accepted
    /// `wtransport::Connection`s pushed by an upstream listener task.
    pub fn serving() -> (Self, mpsc::UnboundedSender<wtransport::Connection>) {
        let (tx, rx) = mpsc::unbounded_channel::<wtransport::Connection>();
        let transport = Self {
            mode: TransportMode::Serving { rx: Mutex::new(rx) },
        };
        (transport, tx)
    }
}

#[async_trait(?Send)]
impl RawTransport for WebTransportRawTransport {
    type Connection = WebTransportRawConnection;

    async fn connect(&self, addr: PeerAddr) -> SyncResult<Self::Connection> {
        let parsed = parse_addr(&addr)?;
        let endpoint = build_client_endpoint(&parsed.cert_hashes)?;
        let connection = endpoint
            .connect(parsed.https_url())
            .await
            .map_err(|e| SyncError::Transport(format!("wt connect: {e}")))?;
        // Open the single persistent reliable bidi stream. Server-side
        // mirrors this with `accept_bi`. Both sides must be ready before
        // we hand back a `RawConnection`, otherwise the first
        // `send_reliable` would deadlock on a stream that doesn't exist.
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| SyncError::Transport(format!("wt open_bi: {e}")))?
            .await
            .map_err(|e| SyncError::Transport(format!("wt open_bi finish: {e}")))?;
        Ok(WebTransportRawConnection::new(connection, send, recv))
    }

    async fn accept(&self) -> SyncResult<Self::Connection> {
        match &self.mode {
            TransportMode::DialOnly => {
                std::future::pending::<()>().await;
                unreachable!()
            }
            TransportMode::Serving { rx } => {
                let mut rx = rx.lock().await;
                let connection = rx.recv().await.ok_or_else(|| {
                    SyncError::Transport("wt serving channel closed".into())
                })?;
                let (send, recv) = connection.accept_bi().await.map_err(|e| {
                    SyncError::Transport(format!("wt accept_bi: {e}"))
                })?;
                Ok(WebTransportRawConnection::new(connection, send, recv))
            }
        }
    }
}

/// Build a fresh client `Endpoint` configured to validate server certs
/// via SHA-256 SPKI hashes (matching the W3C WebTransport
/// `serverCertificateHashes` semantics). Empty hash list falls back to
/// the system's native CA roots.
fn build_client_endpoint(hashes: &[Sha256Digest]) -> SyncResult<Endpoint<wtransport::endpoint::endpoint_side::Client>> {
    let builder = ClientConfig::builder().with_bind_default();
    let cfg = if hashes.is_empty() {
        builder.with_native_certs().build()
    } else {
        builder
            .with_server_certificate_hashes(hashes.iter().cloned())
            .build()
    };
    Endpoint::client(cfg).map_err(|e| SyncError::Transport(format!("wt client endpoint: {e}")))
}

/// Build a server endpoint identity + bind address. The caller drives the
/// accept loop themselves (this crate stays decoupled from the relay's
/// task topology).
pub fn build_server_endpoint(
    bind_addr: std::net::SocketAddr,
    identity: wtransport::Identity,
    keep_alive: Option<std::time::Duration>,
) -> SyncResult<Endpoint<wtransport::endpoint::endpoint_side::Server>> {
    let mut builder = ServerConfig::builder()
        .with_bind_address(bind_addr)
        .with_identity(identity);
    if let Some(ka) = keep_alive {
        builder = builder.keep_alive_interval(Some(ka));
    }
    Endpoint::server(builder.build()).map_err(|e| SyncError::Transport(format!("wt server endpoint: {e}")))
}

pub struct WebTransportRawConnection {
    /// Held alive so dropping the connection closes the QUIC connection
    /// (wtransport ties session lifetime to this handle).
    session: Arc<wtransport::Connection>,
    send: Mutex<SendStream>,
    recv: Mutex<RecvStream>,
}

impl WebTransportRawConnection {
    fn new(connection: wtransport::Connection, send: SendStream, recv: RecvStream) -> Self {
        Self {
            session: Arc::new(connection),
            send: Mutex::new(send),
            recv: Mutex::new(recv),
        }
    }
}

#[async_trait(?Send)]
impl RawConnection for WebTransportRawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
        if bytes.len() > MAX_RELIABLE_FRAME {
            return Err(SyncError::Transport(format!(
                "wt send_reliable: frame too large ({} > {MAX_RELIABLE_FRAME})",
                bytes.len()
            )));
        }
        let len = u32::try_from(bytes.len())
            .map_err(|_| SyncError::Transport("wt send_reliable: len > u32::MAX".into()))?;
        let mut send = self.send.lock().await;
        send.write_all(&len.to_be_bytes())
            .await
            .map_err(|e| SyncError::Transport(format!("wt send len: {e}")))?;
        send.write_all(&bytes)
            .await
            .map_err(|e| SyncError::Transport(format!("wt send body: {e}")))?;
        Ok(())
    }

    async fn recv_reliable(&self) -> SyncResult<Bytes> {
        let mut recv = self.recv.lock().await;
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| SyncError::Transport(format!("wt recv len: {e}")))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_RELIABLE_FRAME {
            return Err(SyncError::Transport(format!(
                "wt recv_reliable: frame too large ({len} > {MAX_RELIABLE_FRAME})"
            )));
        }
        let mut buf = vec![0u8; len];
        recv.read_exact(&mut buf)
            .await
            .map_err(|e| SyncError::Transport(format!("wt recv body: {e}")))?;
        Ok(Bytes::from(buf))
    }

    async fn send_unreliable(&self, bytes: Bytes) -> SyncResult<()> {
        if bytes.len() > MAX_DATAGRAM_PAYLOAD {
            return Err(SyncError::Transport(format!(
                "wt send_unreliable: payload too large ({} > {MAX_DATAGRAM_PAYLOAD})",
                bytes.len()
            )));
        }
        // wtransport's `IntoPayload` is implemented for `&[u8]`; pass a
        // borrow rather than allocating a Vec. (Internally wtransport
        // copies into its own buffer either way.)
        self.session
            .send_datagram(&bytes)
            .map_err(|e| SyncError::Transport(format!("wt send datagram: {e}")))
    }

    async fn recv_unreliable(&self) -> SyncResult<Bytes> {
        let dg = self
            .session
            .receive_datagram()
            .await
            .map_err(|e| SyncError::Transport(format!("wt recv datagram: {e}")))?;
        Ok(Bytes::copy_from_slice(dg.payload().as_ref()))
    }

    async fn close(&self) -> SyncResult<()> {
        // wtransport closes when the Connection drops; explicit close
        // sends an APP_CLOSE with code 0. We don't surface failures
        // because there's nothing useful for callers to do.
        self.session.close(wtransport::VarInt::from_u32(0), b"closed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimum end-to-end test: spawn an in-process server, dial it,
    /// roundtrip a reliable message and a datagram.
    #[tokio::test(flavor = "current_thread")]
    async fn reliable_and_unreliable_roundtrip() {
        let identity = wtransport::Identity::self_signed(["localhost", "127.0.0.1"]).unwrap();
        let cert_hash = identity.certificate_chain().as_slice()[0].hash();
        let endpoint = build_server_endpoint(
            "127.0.0.1:0".parse().unwrap(),
            identity,
            Some(std::time::Duration::from_secs(3)),
        )
        .unwrap();
        let bound = endpoint.local_addr().unwrap();
        let (server_raw, accept_tx) = WebTransportRawTransport::serving();

        // accept_tx requires Send (it crosses a tokio::spawn boundary)
        // but `wtransport::Endpoint::accept` is also Send-friendly.
        // We spawn the upstream accept loop on the multi-thread runtime;
        // the `?Send`-bound `WebTransportRawTransport::accept` runs on
        // a LocalSet so it can drop the un-Send `Connection`.
        let accept_handle = tokio::spawn(async move {
            loop {
                let incoming = endpoint.accept().await;
                let session_request = match incoming.await {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let conn = match session_request.accept().await {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if accept_tx.send(conn).is_err() {
                    break;
                }
            }
        });

        let local_set = tokio::task::LocalSet::new();
        local_set
            .run_until(async {
                // Server task holds the connection alive until the test
                // signals shutdown — otherwise dropping the connection
                // racing the client's `recv_unreliable` results in a
                // "connection closed by peer" error before the
                // datagram the server just sent reaches the client's
                // datagram inbox. This shutdown_rx pattern matches how
                // production callers manage WT lifecycles.
                let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
                let server_task = tokio::task::spawn_local(async move {
                    let conn = server_raw.accept().await.unwrap();
                    let msg = conn.recv_reliable().await.unwrap();
                    conn.send_reliable(msg).await.unwrap();
                    let dg = conn.recv_unreliable().await.unwrap();
                    conn.send_unreliable(dg).await.unwrap();
                    let _ = shutdown_rx.await;
                });

                let cert_hex = sha256_digest_to_hex(&cert_hash);
                let addr = PeerAddr::new(Bytes::from(format!(
                    "wt://127.0.0.1:{}#cert-sha256={cert_hex}",
                    bound.port()
                )));
                let client = WebTransportRawTransport::dial_only();
                let conn = client.connect(addr).await.unwrap();

                conn.send_reliable(Bytes::from_static(b"hello wt"))
                    .await
                    .unwrap();
                let echo = conn.recv_reliable().await.unwrap();
                assert_eq!(echo.as_ref(), b"hello wt");

                conn.send_unreliable(Bytes::from_static(b"dgram one"))
                    .await
                    .unwrap();
                let dg = conn.recv_unreliable().await.unwrap();
                assert_eq!(dg.as_ref(), b"dgram one");

                let _ = shutdown_tx.send(());
                server_task.await.unwrap();
                accept_handle.abort();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_datagram_returns_err() {
        let identity = wtransport::Identity::self_signed(["localhost"]).unwrap();
        let cert_hash = identity.certificate_chain().as_slice()[0].hash();
        let endpoint = build_server_endpoint(
            "127.0.0.1:0".parse().unwrap(),
            identity,
            Some(std::time::Duration::from_secs(3)),
        )
        .unwrap();
        let bound = endpoint.local_addr().unwrap();
        let (server_raw, accept_tx) = WebTransportRawTransport::serving();

        let accept_handle = tokio::spawn(async move {
            let incoming = endpoint.accept().await;
            if let Ok(req) = incoming.await
                && let Ok(conn) = req.accept().await
            {
                let _ = accept_tx.send(conn);
            }
        });

        let local_set = tokio::task::LocalSet::new();
        local_set
            .run_until(async {
                let server_task = tokio::task::spawn_local(async move {
                    let _ = server_raw.accept().await.unwrap();
                });

                let cert_hex = sha256_digest_to_hex(&cert_hash);
                let addr = PeerAddr::new(Bytes::from(format!(
                    "wt://127.0.0.1:{}#cert-sha256={cert_hex}",
                    bound.port()
                )));
                let client = WebTransportRawTransport::dial_only();
                let conn = client.connect(addr).await.unwrap();
                let huge = Bytes::from(vec![0u8; MAX_DATAGRAM_PAYLOAD + 1]);
                let err = conn.send_unreliable(huge).await.unwrap_err();
                assert!(format!("{err}").contains("payload too large"), "got: {err}");

                accept_handle.abort();
                server_task.abort();
            })
            .await;
    }
}
