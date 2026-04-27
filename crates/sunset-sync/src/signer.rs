//! Per-peer signing capability injected into `SyncEngine`.
//!
//! `sunset-core::Identity` implements this; tests can implement a stub.

use bytes::Bytes;

use sunset_store::VerifyingKey;

pub trait Signer: Send + Sync {
    /// The verifying-key bytes that match this signer's signatures.
    fn verifying_key(&self) -> VerifyingKey;

    /// Produce an Ed25519 signature over `payload`. Returns 64 bytes.
    fn sign(&self, payload: &[u8]) -> Bytes;
}
