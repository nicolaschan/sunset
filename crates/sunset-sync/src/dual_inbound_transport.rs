//! Two-transport combinator for relays' inbound side: race
//! WebSocket-accept and WebTransport-accept; route outbound dials by
//! URL scheme.
//!
//! Conceptually parallel to [`crate::MultiTransport`] (which the
//! browser uses to combine WebSocket-relay + WebRTC-direct), but with
//! WebTransport-aware routing rather than WebRTC. The browser
//! combinator's scheme-routing rule (`ws`/`wss`/`webrtc`) differs from
//! what server hosts need (`ws`/`wss`/`wt`/`wts`), so they're separate
//! types.
//!
//! Both halves are themselves [`Transport`] implementations — typically
//! [`crate::SpawningAcceptor`] wrappers around a raw transport, so the
//! caller is responsible for ensuring per-connection handshakes
//! complete on a side task rather than inside the `accept()` future
//! `select!`'d below. (If someone passes a non-`SpawningAcceptor` half
//! and the loser arm of the `accept` race held a half-finished
//! handshake, the cancel would tear it down — see
//! `project_engine_select_drop_handshakes` in the user's auto-memory
//! for the prior incident.)

use async_trait::async_trait;
use bytes::Bytes;
use futures::future::FutureExt;

use crate::error::{Error, Result};
use crate::transport::{Transport, TransportConnection, TransportKind};
use crate::types::{PeerAddr, PeerId};

/// Combines a WebSocket-side and WebTransport-side [`Transport`].
pub struct DualInboundTransport<WsT: Transport, WtT: Transport> {
    ws: WsT,
    wt: WtT,
}

impl<WsT: Transport, WtT: Transport> DualInboundTransport<WsT, WtT> {
    pub fn new(ws: WsT, wt: WtT) -> Self {
        Self { ws, wt }
    }
}

#[async_trait(?Send)]
impl<WsT, WtT> Transport for DualInboundTransport<WsT, WtT>
where
    WsT: Transport,
    WsT::Connection: 'static,
    WtT: Transport,
    WtT::Connection: 'static,
{
    type Connection = DualConnection<WsT::Connection, WtT::Connection>;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        let s = std::str::from_utf8(addr.as_bytes())
            .map_err(|e| Error::Transport(format!("dual: addr not utf-8: {e}")))?;
        if s.starts_with("ws://") || s.starts_with("wss://") {
            Ok(DualConnection::Ws(self.ws.connect(addr).await?))
        } else if s.starts_with("wt://") || s.starts_with("wts://") {
            Ok(DualConnection::Wt(self.wt.connect(addr).await?))
        } else {
            Err(Error::Transport(format!(
                "dual: unknown scheme in {s} (expected ws:// wss:// wt:// or wts://)"
            )))
        }
    }

    async fn accept(&self) -> Result<Self::Connection> {
        let ws = self.ws.accept().fuse();
        let wt = self.wt.accept().fuse();
        futures::pin_mut!(ws, wt);
        futures::select! {
            w = ws => Ok(DualConnection::Ws(w?)),
            t = wt => Ok(DualConnection::Wt(t?)),
        }
    }
}

pub enum DualConnection<WsC, WtC> {
    Ws(WsC),
    Wt(WtC),
}

#[async_trait(?Send)]
impl<WsC, WtC> TransportConnection for DualConnection<WsC, WtC>
where
    WsC: TransportConnection,
    WtC: TransportConnection,
{
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
        match self {
            DualConnection::Ws(c) => c.send_reliable(bytes).await,
            DualConnection::Wt(c) => c.send_reliable(bytes).await,
        }
    }

    async fn recv_reliable(&self) -> Result<Bytes> {
        match self {
            DualConnection::Ws(c) => c.recv_reliable().await,
            DualConnection::Wt(c) => c.recv_reliable().await,
        }
    }

    async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
        match self {
            DualConnection::Ws(c) => c.send_unreliable(bytes).await,
            DualConnection::Wt(c) => c.send_unreliable(bytes).await,
        }
    }

    async fn recv_unreliable(&self) -> Result<Bytes> {
        match self {
            DualConnection::Ws(c) => c.recv_unreliable().await,
            DualConnection::Wt(c) => c.recv_unreliable().await,
        }
    }

    fn peer_id(&self) -> PeerId {
        match self {
            DualConnection::Ws(c) => c.peer_id(),
            DualConnection::Wt(c) => c.peer_id(),
        }
    }

    fn kind(&self) -> TransportKind {
        // Inbound connections never participate in a `MultiTransport`
        // discriminator, so reporting Unknown for both arms is fine —
        // the relay doesn't surface a per-connection transport kind in
        // its UI today (and the browser Client tags primary/secondary
        // separately).
        TransportKind::Unknown
    }

    async fn close(&self) -> Result<()> {
        match self {
            DualConnection::Ws(c) => c.close().await,
            DualConnection::Wt(c) => c.close().await,
        }
    }
}
