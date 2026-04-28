//! Test-only helpers shared across the crate's `#[cfg(test)]` modules.

#![cfg(test)]

use async_trait::async_trait;
use bytes::Bytes;

use crate::Result;
use crate::transport::TransportConnection;
use crate::types::PeerId;
use sunset_store::VerifyingKey;

/// Minimal `TransportConnection` impl used by tests that need a value
/// of *some* connection type but don't actually exercise its
/// behaviour. All methods are no-ops; `peer_id` returns an all-zero
/// 32-byte key.
pub(crate) struct DummyConn;

#[async_trait(?Send)]
impl TransportConnection for DummyConn {
    async fn send_reliable(&self, _: Bytes) -> Result<()> {
        Ok(())
    }
    async fn recv_reliable(&self) -> Result<Bytes> {
        Ok(Bytes::new())
    }
    async fn send_unreliable(&self, _: Bytes) -> Result<()> {
        Ok(())
    }
    async fn recv_unreliable(&self) -> Result<Bytes> {
        Ok(Bytes::new())
    }
    fn peer_id(&self) -> PeerId {
        PeerId(VerifyingKey::new(Bytes::from_static(&[0u8; 32])))
    }
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}
