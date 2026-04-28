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
}
