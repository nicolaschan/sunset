//! Canonical synchronous test fixtures shared by the conformance suite, the
//! crate's own unit tests, and every backend's unit tests.
//!
//! Always-compiled `pub(crate)` module — within the crate, anything
//! `#[cfg(test)]` reaches it via `use crate::fixtures::{vk, n, ...}`. For
//! external consumers, the same items are re-exported from
//! [`crate::test_helpers`] under the `test-helpers` feature gate; backends
//! that already enable `sunset-store = { features = ["test-helpers"] }`
//! in their `[dev-dependencies]` therefore reach for
//! `sunset_store::test_helpers::{vk, n, block, entry, entry_expiring_at}`.
//!
//! The module is unconditional so rust-analyzer (and any other tool that
//! indexes without enabling features) sees the module tree consistently;
//! these functions are dead code in production builds and are stripped by
//! LTO. There is no third place — local re-declarations in backends are
//! duplication.

use std::sync::Arc;

use crate::error::Result;
use crate::types::{ContentBlock, SignedKvEntry, VerifyingKey};
use crate::verifier::SignatureVerifier;

/// Helper: a verifying key from static bytes.
pub fn vk(b: &'static [u8]) -> VerifyingKey {
    VerifyingKey::new(bytes::Bytes::from_static(b))
}

/// Helper: a name from static bytes.
pub fn n(b: &'static [u8]) -> bytes::Bytes {
    bytes::Bytes::from_static(b)
}

/// Helper: a small leaf block.
pub fn block(payload: &'static [u8]) -> ContentBlock {
    ContentBlock {
        data: bytes::Bytes::from_static(payload),
        references: vec![],
    }
}

/// Helper: an entry pointing at `block`'s hash, with the given key/name/priority.
pub fn entry(
    block: &ContentBlock,
    vk_bytes: &'static [u8],
    name: &'static [u8],
    priority: u64,
) -> SignedKvEntry {
    SignedKvEntry {
        verifying_key: vk(vk_bytes),
        name: n(name),
        value_hash: block.hash(),
        priority,
        expires_at: None,
        signature: bytes::Bytes::from_static(b"sig"),
    }
}

/// Helper: like [`entry`], but with an explicit TTL expiry.
pub fn entry_expiring_at(
    block: &ContentBlock,
    vk_bytes: &'static [u8],
    name: &'static [u8],
    priority: u64,
    expires_at: u64,
) -> SignedKvEntry {
    SignedKvEntry {
        verifying_key: vk(vk_bytes),
        name: n(name),
        value_hash: block.hash(),
        priority,
        expires_at: Some(expires_at),
        signature: bytes::Bytes::from_static(b"sig"),
    }
}

/// A verifier that asserts entries pass through it. Useful to detect when a
/// backend forgets to call its verifier on insert.
pub struct CountingVerifier(pub Arc<std::sync::atomic::AtomicUsize>);
impl SignatureVerifier for CountingVerifier {
    fn verify(&self, _entry: &SignedKvEntry) -> Result<()> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn verify_raw(&self, _vk: &VerifyingKey, _payload: &[u8], _sig: &[u8]) -> Result<()> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}
