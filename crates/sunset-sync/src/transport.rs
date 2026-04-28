//! Transport trait surface that hosts implement (browser WebRTC, native
//! webrtc-rs, the in-memory `TestTransport`, etc.).

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::Result;
use crate::types::{PeerAddr, PeerId};

/// Which side of a `MultiTransport` (or which discriminator a
/// future multi-fanout transport chooses) produced this connection.
/// Used by callers (e.g. UI clients) to render per-peer routing
/// state without having to know the concrete transport type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransportKind {
    /// Primary half of a `MultiTransport`. In v1 this is the
    /// relay-mediated WebSocket path.
    Primary,
    /// Secondary half of a `MultiTransport`. In v1 this is the
    /// direct WebRTC datachannel.
    Secondary,
    /// Used by transports that don't participate in a
    /// `MultiTransport` (e.g. `TestTransport`, single-transport
    /// setups).
    Unknown,
}

/// A factory for inbound and outbound peer connections.
///
/// Implementations are `?Send`-compatible so they work in single-threaded
/// WASM as well as multi-threaded native runtimes.
#[async_trait(?Send)]
pub trait Transport {
    type Connection: TransportConnection;

    /// Initiate a connection to `addr`. Returns when the connection is
    /// established (handshake complete) or fails.
    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection>;

    /// Wait for the next inbound connection. Returns when one arrives.
    /// Implementations that don't accept inbound connections (e.g., a
    /// dial-only client) should return a future that never resolves.
    async fn accept(&self) -> Result<Self::Connection>;
}

/// One peer connection. Carries a reliable channel (used by sunset-sync) and
/// an unreliable channel (used by sunset-core for voice; a no-op for v1).
#[async_trait(?Send)]
pub trait TransportConnection {
    /// Send one message on the reliable channel. Whole-message framing is the
    /// transport's responsibility — `bytes` is one whole `SyncMessage`.
    async fn send_reliable(&self, bytes: Bytes) -> Result<()>;

    /// Receive one whole message from the reliable channel. Blocks until a
    /// message is available, the channel is closed, or an error occurs.
    async fn recv_reliable(&self) -> Result<Bytes>;

    /// Send one message on the unreliable channel (datagram-shaped).
    /// Reserved for sunset-core voice; sunset-sync does not use it.
    async fn send_unreliable(&self, bytes: Bytes) -> Result<()>;

    /// Receive one message from the unreliable channel. May return spurious
    /// errors if datagrams are lost in transit; callers should not rely on
    /// this for protocol state.
    async fn recv_unreliable(&self) -> Result<Bytes>;

    /// The peer's identity at the other end of this connection.
    fn peer_id(&self) -> PeerId;

    /// Identifies which transport produced this connection. Default
    /// is `TransportKind::Unknown`; `MultiConnection` overrides to
    /// return `Primary` or `Secondary`.
    fn kind(&self) -> TransportKind {
        TransportKind::Unknown
    }

    /// Close the connection. Subsequent send/recv calls return
    /// `Error::Transport("closed")` or similar.
    async fn close(&self) -> Result<()>;
}

/// Plain bytes pipe — no authentication, no `peer_id`. Implementations are
/// unaware of any cryptography; a `NoiseTransport<R: RawTransport>` decorator
/// (in the `sunset-noise` crate) wraps any RawTransport into an
/// authenticated `Transport`.
///
/// New transport crates (browser WebSocket, WebRTC, WebTransport, …)
/// implement only this trait — they need no crypto deps.
#[async_trait(?Send)]
pub trait RawTransport {
    type Connection: RawConnection;
    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection>;
    async fn accept(&self) -> Result<Self::Connection>;
}

#[async_trait(?Send)]
pub trait RawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> Result<()>;
    async fn recv_reliable(&self) -> Result<Bytes>;
    async fn send_unreliable(&self, bytes: Bytes) -> Result<()>;
    async fn recv_unreliable(&self) -> Result<Bytes>;
    async fn close(&self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::DummyConn;

    #[test]
    fn default_kind_is_unknown() {
        assert_eq!(DummyConn.kind(), TransportKind::Unknown);
    }
}
