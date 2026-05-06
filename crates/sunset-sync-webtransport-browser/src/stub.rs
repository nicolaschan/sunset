//! Native fallback. Compiled on non-wasm targets so the workspace builds
//! without wasm tooling. Calls return `Error::Transport`.

use async_trait::async_trait;
use bytes::Bytes;

use sunset_sync::{Error, PeerAddr, RawConnection, RawTransport, Result};

pub struct WebTransportRawTransport;

impl WebTransportRawTransport {
    pub fn dial_only() -> Self {
        Self
    }
}

#[async_trait(?Send)]
impl RawTransport for WebTransportRawTransport {
    type Connection = WebTransportRawConnection;

    async fn connect(&self, _: PeerAddr) -> Result<Self::Connection> {
        Err(Error::Transport(
            "sunset-sync-webtransport-browser: native stub — must be built for wasm32".into(),
        ))
    }

    async fn accept(&self) -> Result<Self::Connection> {
        std::future::pending::<()>().await;
        unreachable!();
    }
}

pub struct WebTransportRawConnection;

#[async_trait(?Send)]
impl RawConnection for WebTransportRawConnection {
    async fn send_reliable(&self, _: Bytes) -> Result<()> {
        Err(Error::Transport(
            "sunset-sync-webtransport-browser: native stub".into(),
        ))
    }
    async fn recv_reliable(&self) -> Result<Bytes> {
        Err(Error::Transport(
            "sunset-sync-webtransport-browser: native stub".into(),
        ))
    }
    async fn send_unreliable(&self, _: Bytes) -> Result<()> {
        Err(Error::Transport(
            "sunset-sync-webtransport-browser: native stub".into(),
        ))
    }
    async fn recv_unreliable(&self) -> Result<Bytes> {
        Err(Error::Transport(
            "sunset-sync-webtransport-browser: native stub".into(),
        ))
    }
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}
