//! Transport trait surface that hosts implement (browser WebRTC, native
//! webrtc-rs, the in-memory `TestTransport`, etc.).

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::Result;
use crate::types::{PeerAddr, PeerId};

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

    /// Close the connection. Subsequent send/recv calls return
    /// `Error::Transport("closed")` or similar.
    async fn close(&self) -> Result<()>;
}
