//! Native fallback. Compiled on non-wasm targets so the workspace builds
//! without wasm tooling. Calls return `Error::Transport`.

use std::rc::Rc;

use async_trait::async_trait;
use bytes::Bytes;

use sunset_sync::{Error, PeerAddr, PeerId, RawConnection, RawTransport, Result, Signaler};

pub struct WebRtcRawTransport {
    _signaler: Rc<dyn Signaler>,
    _local_peer: PeerId,
    _ice_urls: Vec<String>,
}

impl WebRtcRawTransport {
    pub fn new(signaler: Rc<dyn Signaler>, local_peer: PeerId, ice_urls: Vec<String>) -> Self {
        Self {
            _signaler: signaler,
            _local_peer: local_peer,
            _ice_urls: ice_urls,
        }
    }
}

#[async_trait(?Send)]
impl RawTransport for WebRtcRawTransport {
    type Connection = WebRtcRawConnection;

    async fn connect(&self, _: PeerAddr) -> Result<Self::Connection> {
        Err(Error::Transport(
            "sunset-sync-webrtc-browser: native stub — must be built for wasm32".into(),
        ))
    }

    async fn accept(&self) -> Result<Self::Connection> {
        std::future::pending::<()>().await;
        unreachable!();
    }
}

pub struct WebRtcRawConnection;

#[async_trait(?Send)]
impl RawConnection for WebRtcRawConnection {
    async fn send_reliable(&self, _: Bytes) -> Result<()> {
        Err(Error::Transport(
            "sunset-sync-webrtc-browser: native stub".into(),
        ))
    }
    async fn recv_reliable(&self) -> Result<Bytes> {
        Err(Error::Transport(
            "sunset-sync-webrtc-browser: native stub".into(),
        ))
    }
    async fn send_unreliable(&self, _: Bytes) -> Result<()> {
        Err(Error::Transport(
            "sunset-sync-webrtc-browser: native stub".into(),
        ))
    }
    async fn recv_unreliable(&self) -> Result<Bytes> {
        Err(Error::Transport(
            "sunset-sync-webrtc-browser: native stub".into(),
        ))
    }
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}
