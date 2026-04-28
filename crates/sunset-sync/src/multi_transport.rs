//! Compose two transports into one. The wrapped `SyncEngine` sees a
//! single Transport; routing across the two underlying transports is
//! invisible to it.
//!
//! Routing rule: PeerAddr's URL prefix decides which underlying
//! transport gets the dial.
//! - `ws://...` or `wss://...` → primary
//! - `webrtc://...`            → secondary
//! - other                     → `Error::Transport("multi: unknown scheme...")`
//!
//! Inbound (`accept`): both transports race; whichever yields first
//! wins. The connection's `peer_id()` carries the per-transport
//! authentication identity.

use async_trait::async_trait;
use bytes::Bytes;
use futures::future::FutureExt;

use crate::error::{Error, Result};
use crate::transport::{Transport, TransportConnection};
use crate::types::{PeerAddr, PeerId};

pub struct MultiTransport<T1: Transport, T2: Transport> {
    primary: T1,
    secondary: T2,
}

impl<T1: Transport, T2: Transport> MultiTransport<T1, T2> {
    pub fn new(primary: T1, secondary: T2) -> Self {
        Self { primary, secondary }
    }
}

#[async_trait(?Send)]
impl<T1, T2> Transport for MultiTransport<T1, T2>
where
    T1: Transport,
    T1::Connection: 'static,
    T2: Transport,
    T2::Connection: 'static,
{
    type Connection = MultiConnection<T1::Connection, T2::Connection>;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        let s = std::str::from_utf8(addr.as_bytes())
            .map_err(|e| Error::Transport(format!("multi: addr not utf-8: {e}")))?;
        if s.starts_with("ws://") || s.starts_with("wss://") {
            Ok(MultiConnection::Primary(self.primary.connect(addr).await?))
        } else if s.starts_with("webrtc://") {
            Ok(MultiConnection::Secondary(
                self.secondary.connect(addr).await?,
            ))
        } else {
            Err(Error::Transport(format!(
                "multi: unknown scheme in {s} (expected ws://, wss://, or webrtc://)"
            )))
        }
    }

    async fn accept(&self) -> Result<Self::Connection> {
        let primary = self.primary.accept().fuse();
        let secondary = self.secondary.accept().fuse();
        futures::pin_mut!(primary, secondary);

        futures::select! {
            p = primary => Ok(MultiConnection::Primary(p?)),
            s = secondary => Ok(MultiConnection::Secondary(s?)),
        }
    }
}

pub enum MultiConnection<C1, C2> {
    Primary(C1),
    Secondary(C2),
}

#[async_trait(?Send)]
impl<C1, C2> TransportConnection for MultiConnection<C1, C2>
where
    C1: TransportConnection,
    C2: TransportConnection,
{
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
        match self {
            MultiConnection::Primary(c) => c.send_reliable(bytes).await,
            MultiConnection::Secondary(c) => c.send_reliable(bytes).await,
        }
    }

    async fn recv_reliable(&self) -> Result<Bytes> {
        match self {
            MultiConnection::Primary(c) => c.recv_reliable().await,
            MultiConnection::Secondary(c) => c.recv_reliable().await,
        }
    }

    async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
        match self {
            MultiConnection::Primary(c) => c.send_unreliable(bytes).await,
            MultiConnection::Secondary(c) => c.send_unreliable(bytes).await,
        }
    }

    async fn recv_unreliable(&self) -> Result<Bytes> {
        match self {
            MultiConnection::Primary(c) => c.recv_unreliable().await,
            MultiConnection::Secondary(c) => c.recv_unreliable().await,
        }
    }

    fn peer_id(&self) -> PeerId {
        match self {
            MultiConnection::Primary(c) => c.peer_id(),
            MultiConnection::Secondary(c) => c.peer_id(),
        }
    }

    fn kind(&self) -> crate::transport::TransportKind {
        use crate::transport::TransportKind;
        match self {
            MultiConnection::Primary(_) => TransportKind::Primary,
            MultiConnection::Secondary(_) => TransportKind::Secondary,
        }
    }

    async fn close(&self) -> Result<()> {
        match self {
            MultiConnection::Primary(c) => c.close().await,
            MultiConnection::Secondary(c) => c.close().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::DummyConn as StubConn;
    use crate::transport::TransportKind;

    #[test]
    fn primary_variant_reports_primary() {
        let c: MultiConnection<StubConn, StubConn> = MultiConnection::Primary(StubConn);
        assert_eq!(c.kind(), TransportKind::Primary);
    }

    #[test]
    fn secondary_variant_reports_secondary() {
        let c: MultiConnection<StubConn, StubConn> = MultiConnection::Secondary(StubConn);
        assert_eq!(c.kind(), TransportKind::Secondary);
    }
}
