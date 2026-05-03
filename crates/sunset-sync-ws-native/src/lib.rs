//! Native WebSocket implementation of `sunset_sync::RawTransport`.
//!
//! Wrap with `sunset_noise::NoiseTransport` to get authenticated
//! encrypted connections suitable for `SyncEngine`.

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use sunset_sync::{
    Error as SyncError, PeerAddr, RawConnection, RawTransport, Result as SyncResult,
};

// ---- split sink type ----

enum WsSink {
    Client(SplitSink<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, Message>),
    Server(SplitSink<WebSocketStream<tokio::net::TcpStream>, Message>),
}

impl WsSink {
    async fn send(&mut self, msg: Message) -> Result<(), tokio_tungstenite::tungstenite::Error> {
        match self {
            WsSink::Client(s) => s.send(msg).await,
            WsSink::Server(s) => s.send(msg).await,
        }
    }

    async fn close(&mut self) {
        match self {
            WsSink::Client(s) => {
                s.close().await.ok();
            }
            WsSink::Server(s) => {
                s.close().await.ok();
            }
        }
    }
}

// ---- split stream type ----

enum WsStream {
    Client(SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>),
    Server(SplitStream<WebSocketStream<tokio::net::TcpStream>>),
}

impl WsStream {
    async fn next(&mut self) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
        match self {
            WsStream::Client(s) => s.next().await,
            WsStream::Server(s) => s.next().await,
        }
    }
}

/// Either a dial-only client or a listening server.
pub struct WebSocketRawTransport {
    mode: TransportMode,
}

enum TransportMode {
    DialOnly,
    Listening {
        listener: Mutex<TcpListener>,
    },
    /// Accept pre-classified TcpStreams from an external dispatcher.
    ExternalStreams {
        rx: Mutex<tokio::sync::mpsc::Receiver<TcpStream>>,
    },
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

    /// Accept already-classified TcpStreams from an external dispatcher.
    /// The caller has bound the TcpListener and decided which connections
    /// belong to WS (vs other protocols on the same port). When the
    /// channel closes, `accept` returns an error and the engine treats it
    /// as transport failure.
    pub fn external_streams(rx: tokio::sync::mpsc::Receiver<TcpStream>) -> Self {
        Self {
            mode: TransportMode::ExternalStreams { rx: Mutex::new(rx) },
        }
    }

    /// Bound address (useful when binding to port 0).
    ///
    /// Returns `None` for `ExternalStreams` mode — the relay knows its own
    /// bound address from the `TcpListener` it holds directly.
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        match &self.mode {
            TransportMode::Listening { listener } => {
                listener.try_lock().ok().and_then(|l| l.local_addr().ok())
            }
            TransportMode::DialOnly | TransportMode::ExternalStreams { .. } => None,
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
        let (sink, stream) = ws.split();
        Ok(WebSocketRawConnection::new(
            WsSink::Client(sink),
            WsStream::Client(stream),
        ))
    }

    async fn accept(&self) -> SyncResult<Self::Connection> {
        let tcp = match &self.mode {
            TransportMode::Listening { listener } => {
                let listener = listener.lock().await;
                let (tcp, _peer) = listener
                    .accept()
                    .await
                    .map_err(|e| SyncError::Transport(format!("accept: {e}")))?;
                tcp
            }
            TransportMode::ExternalStreams { rx } => {
                let mut rx = rx.lock().await;
                rx.recv()
                    .await
                    .ok_or_else(|| SyncError::Transport("external stream channel closed".into()))?
            }
            TransportMode::DialOnly => {
                std::future::pending::<()>().await;
                unreachable!();
            }
        };
        let ws = tokio_tungstenite::accept_async(tcp)
            .await
            .map_err(|e| SyncError::Transport(format!("ws upgrade: {e}")))?;
        let (sink, stream) = ws.split();
        Ok(WebSocketRawConnection::new(
            WsSink::Server(sink),
            WsStream::Server(stream),
        ))
    }
}

pub struct WebSocketRawConnection {
    /// Write side — protected by its own mutex so send and recv can run
    /// concurrently without blocking each other.
    sink: Mutex<WsSink>,
    /// Read side — protected by its own mutex.
    stream: Mutex<WsStream>,
}

impl WebSocketRawConnection {
    fn new(sink: WsSink, stream: WsStream) -> Self {
        Self {
            sink: Mutex::new(sink),
            stream: Mutex::new(stream),
        }
    }
}

#[async_trait(?Send)]
impl RawConnection for WebSocketRawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
        let mut s = self.sink.lock().await;
        s.send(Message::Binary(bytes.to_vec()))
            .await
            .map_err(|e| SyncError::Transport(format!("ws send: {e}")))
    }

    async fn recv_reliable(&self) -> SyncResult<Bytes> {
        loop {
            let msg = {
                let mut s = self.stream.lock().await;
                s.next()
                    .await
                    .ok_or_else(|| SyncError::Transport("ws closed".into()))?
                    .map_err(|e| SyncError::Transport(format!("ws recv: {e}")))?
            };
            match msg {
                Message::Binary(b) => return Ok(Bytes::from(b)),
                Message::Ping(_) => {
                    // WebSocket ping: the tungstenite library auto-responds with
                    // Pong in most configurations; we just skip and keep reading.
                    continue;
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
        let mut s = self.sink.lock().await;
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
