//! Native WebSocket implementation of `sunset_sync::RawTransport`.
//!
//! Wrap with `sunset_noise::NoiseTransport` to get authenticated
//! encrypted connections suitable for `SyncEngine`.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use sunset_sync::{
    Error as SyncError, PeerAddr, RawConnection, RawTransport, Result as SyncResult,
};

// Unified stream type covering both dial (MaybeTlsStream) and accept (plain TcpStream).
enum WsStream {
    Client(WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>),
    Server(WebSocketStream<tokio::net::TcpStream>),
}

impl WsStream {
    async fn send(&mut self, msg: Message) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        match self {
            WsStream::Client(s) => s.send(msg).await,
            WsStream::Server(s) => s.send(msg).await,
        }
    }

    async fn next(&mut self) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
        match self {
            WsStream::Client(s) => s.next().await,
            WsStream::Server(s) => s.next().await,
        }
    }

    async fn close(&mut self) {
        match self {
            WsStream::Client(s) => {
                s.close(None).await.ok();
            }
            WsStream::Server(s) => {
                s.close(None).await.ok();
            }
        }
    }
}

/// Either a dial-only client or a listening server.
pub struct WebSocketRawTransport {
    mode: TransportMode,
}

enum TransportMode {
    DialOnly,
    Listening { listener: Mutex<TcpListener> },
}

impl WebSocketRawTransport {
    pub fn dial_only() -> Self {
        Self {
            mode: TransportMode::DialOnly,
        }
    }

    pub async fn listening_on(bind: std::net::SocketAddr) -> SyncResult<Self> {
        let listener = TcpListener::bind(bind)
            .await
            .map_err(|e| SyncError::Transport(format!("bind {bind}: {e}")))?;
        Ok(Self {
            mode: TransportMode::Listening {
                listener: Mutex::new(listener),
            },
        })
    }

    /// Bound address (useful when binding to port 0).
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        match &self.mode {
            TransportMode::Listening { listener } => {
                listener.try_lock().ok().and_then(|l| l.local_addr().ok())
            }
            TransportMode::DialOnly => None,
        }
    }
}

#[async_trait(?Send)]
impl RawTransport for WebSocketRawTransport {
    type Connection = WebSocketRawConnection;

    async fn connect(&self, addr: PeerAddr) -> SyncResult<Self::Connection> {
        let s = std::str::from_utf8(addr.as_bytes())
            .map_err(|e| SyncError::Transport(format!("addr not utf-8: {e}")))?;
        let url_no_frag = s.split('#').next().unwrap_or(s);
        let url = url::Url::parse(url_no_frag)
            .map_err(|e| SyncError::Transport(format!("addr parse: {e}")))?;
        let (ws, _resp) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .map_err(|e| SyncError::Transport(format!("ws connect: {e}")))?;
        Ok(WebSocketRawConnection::new(WsStream::Client(ws)))
    }

    async fn accept(&self) -> SyncResult<Self::Connection> {
        let listener = match &self.mode {
            TransportMode::Listening { listener } => listener,
            TransportMode::DialOnly => {
                std::future::pending::<()>().await;
                unreachable!();
            }
        };
        let listener = listener.lock().await;
        let (tcp, _peer) = listener
            .accept()
            .await
            .map_err(|e| SyncError::Transport(format!("accept: {e}")))?;
        let ws = tokio_tungstenite::accept_async(tcp)
            .await
            .map_err(|e| SyncError::Transport(format!("ws upgrade: {e}")))?;
        Ok(WebSocketRawConnection::new(WsStream::Server(ws)))
    }
}

pub struct WebSocketRawConnection {
    stream: Arc<Mutex<WsStream>>,
}

impl WebSocketRawConnection {
    fn new(ws: WsStream) -> Self {
        Self {
            stream: Arc::new(Mutex::new(ws)),
        }
    }
}

#[async_trait(?Send)]
impl RawConnection for WebSocketRawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
        let mut s = self.stream.lock().await;
        s.send(Message::Binary(bytes.to_vec()))
            .await
            .map_err(|e| SyncError::Transport(format!("ws send: {e}")))
    }

    async fn recv_reliable(&self) -> SyncResult<Bytes> {
        loop {
            let mut s = self.stream.lock().await;
            let msg = s
                .next()
                .await
                .ok_or_else(|| SyncError::Transport("ws closed".into()))?
                .map_err(|e| SyncError::Transport(format!("ws recv: {e}")))?;
            match msg {
                Message::Binary(b) => return Ok(Bytes::from(b)),
                Message::Ping(p) => {
                    s.send(Message::Pong(p)).await.ok();
                }
                Message::Pong(_) => continue,
                Message::Close(_) => {
                    return Err(SyncError::Transport("ws closed by peer".into()));
                }
                Message::Text(_) | Message::Frame(_) => {
                    return Err(SyncError::Transport("unexpected ws message kind".into()));
                }
            }
        }
    }

    async fn send_unreliable(&self, _: Bytes) -> SyncResult<()> {
        Err(SyncError::Transport(
            "websocket: unreliable channel unsupported".into(),
        ))
    }

    async fn recv_unreliable(&self) -> SyncResult<Bytes> {
        Err(SyncError::Transport(
            "websocket: unreliable channel unsupported".into(),
        ))
    }

    async fn close(&self) -> SyncResult<()> {
        let mut s = self.stream.lock().await;
        s.close().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn raw_send_recv_roundtrip() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let server = WebSocketRawTransport::listening_on("127.0.0.1:0".parse().unwrap())
                    .await
                    .unwrap();
                let bound = server.local_addr().unwrap();

                let server_handle = tokio::task::spawn_local(async move {
                    let conn = server.accept().await.unwrap();
                    let msg = conn.recv_reliable().await.unwrap();
                    conn.send_reliable(msg).await.unwrap();
                });

                let client = WebSocketRawTransport::dial_only();
                let addr = PeerAddr::new(Bytes::from(format!("ws://{bound}")));
                let conn = client.connect(addr).await.unwrap();

                conn.send_reliable(Bytes::from_static(b"hello ws"))
                    .await
                    .unwrap();
                let echo = conn.recv_reliable().await.unwrap();
                assert_eq!(echo.as_ref(), b"hello ws");

                server_handle.await.unwrap();
            })
            .await;
    }
}
