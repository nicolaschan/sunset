//! Side-channel for transports that need an out-of-band exchange before
//! data flow can begin (WebRTC SDP/ICE, future patterns).
//!
//! The trait is generic — it shovels opaque bytes between named peers.
//! The transport that uses a Signaler defines its own wire format
//! inside `payload`.

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::Result;
use crate::types::PeerId;

/// One signaling message exchanged between two named peers.
#[derive(Clone, Debug)]
pub struct SignalMessage {
    pub from: PeerId,
    pub to: PeerId,
    /// Per-(from,to) monotonic counter so receivers can dedupe + order.
    pub seq: u64,
    /// Opaque payload — the using transport defines the wire format.
    pub payload: Bytes,
}

#[async_trait(?Send)]
pub trait Signaler: 'static {
    /// Send a signaling message to a remote peer.
    async fn send(&self, message: SignalMessage) -> Result<()>;

    /// Wait for the next inbound signaling message addressed to us.
    async fn recv(&self) -> Result<SignalMessage>;

    /// Drop any persistent per-peer state (cryptographic session, partial
    /// handshakes, etc.) for `peer`, forcing the next outbound
    /// `send(peer, ...)` to re-establish from scratch. Called at the
    /// start of each transport-level handshake attempt (e.g. WebRTC
    /// `connect`) so that a remote that has restarted with the same
    /// identity isn't told to decrypt against a key it no longer holds.
    ///
    /// Default impl is a no-op — implementations that hold no
    /// cross-handshake state (stub transports, in-memory tests) are
    /// already correct without it.
    async fn reset_peer(&self, _peer: &PeerId) {}
}
