//! Core types: PeerId, PeerAddr, SyncConfig, TrustSet.

use std::collections::HashSet;
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use sunset_store::{Filter, VerifyingKey};

use crate::reserved;

/// A peer's identity. Currently transparent over `VerifyingKey` — the peer
/// is identified by its public key. Future schemes (e.g., a separate
/// transport-layer identity) can extend this without breaking callers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub VerifyingKey);

impl PeerId {
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.0
    }
}

/// Transport-specific peer address. The transport interprets these bytes
/// (e.g., a WebRTC SDP signaling endpoint, a TestNetwork peer name).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerAddr(pub Bytes);

impl PeerAddr {
    pub fn new(bytes: impl Into<Bytes>) -> Self {
        Self(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Tunables for a `SyncEngine`. v1 uses fixed defaults; tuning is a follow-up.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    pub protocol_version: u32,
    pub anti_entropy_interval: Duration,
    pub bloom_size_bits: usize,
    pub bloom_hash_fns: u32,
    /// Filter used for the bootstrap digest exchange (always
    /// `_sunset-sync/subscribe` namespace).
    pub bootstrap_filter: Filter,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            anti_entropy_interval: Duration::from_secs(30),
            bloom_size_bits: 4096,
            bloom_hash_fns: 4,
            bootstrap_filter: Filter::Namespace(reserved::SUBSCRIBE_NAME.into()),
        }
    }
}

/// Whose entries this peer is willing to accept on inbound sync. Set via
/// `SyncEngine::set_trust`. Default for v1 is `All` (accept anyone — typical
/// for an open chat room).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TrustSet {
    #[default]
    All,
    Whitelist(HashSet<VerifyingKey>),
}

impl TrustSet {
    pub fn contains(&self, vk: &VerifyingKey) -> bool {
        match self {
            TrustSet::All => true,
            TrustSet::Whitelist(set) => set.contains(vk),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    #[test]
    fn trust_all_accepts_everyone() {
        let t = TrustSet::All;
        assert!(t.contains(&vk(b"alice")));
        assert!(t.contains(&vk(b"bob")));
    }

    #[test]
    fn trust_whitelist_only_accepts_listed() {
        let mut s = HashSet::new();
        s.insert(vk(b"alice"));
        let t = TrustSet::Whitelist(s);
        assert!(t.contains(&vk(b"alice")));
        assert!(!t.contains(&vk(b"bob")));
    }

    #[test]
    fn sync_config_default_is_v1() {
        let c = SyncConfig::default();
        assert_eq!(c.protocol_version, 1);
        assert_eq!(c.bloom_size_bits, 4096);
        assert_eq!(c.bloom_hash_fns, 4);
    }
}
