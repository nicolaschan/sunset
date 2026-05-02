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
    /// Cadence at which each per-peer task sends `SyncMessage::Ping`.
    /// Default 15 s. Three intervals must elapse without a `Pong`
    /// before the connection is declared dead.
    pub heartbeat_interval: Duration,
    /// If no `Pong` arrives within this window, the per-peer task emits
    /// `Disconnected { reason: "heartbeat timeout" }`. Default 45 s
    /// (= 3 × `heartbeat_interval`).
    pub heartbeat_timeout: Duration,
    /// Per-inbound-handshake budget for `transport.accept()`. Bounds the
    /// time a misbehaving client can wedge the engine's accept loop —
    /// e.g. a peer that completes the WebSocket upgrade but never
    /// sends the Noise IK initiator message. On timeout, the connection
    /// is dropped and the engine continues accepting. Default 15 s.
    pub accept_handshake_timeout: Duration,
    /// Bound on concurrent in-flight inbound handshakes for transports
    /// that adopt `spawn_accept_worker`. Past this, new inbound items
    /// wait for a permit before spawning a handshake task. Default 256
    /// — large enough for hobby-scale traffic, small enough that a
    /// flood of bad probes can't exhaust task / FD budgets.
    pub accept_max_inflight: usize,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            anti_entropy_interval: Duration::from_secs(30),
            bloom_size_bits: 4096,
            bloom_hash_fns: 4,
            bootstrap_filter: Filter::Namespace(reserved::SUBSCRIBE_NAME.into()),
            heartbeat_interval: Duration::from_secs(15),
            heartbeat_timeout: Duration::from_secs(45),
            accept_handshake_timeout: Duration::from_secs(15),
            accept_max_inflight: 256,
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

    #[test]
    fn default_heartbeat_settings() {
        let c = SyncConfig::default();
        assert_eq!(c.heartbeat_interval, std::time::Duration::from_secs(15));
        assert_eq!(c.heartbeat_timeout, std::time::Duration::from_secs(45));
    }
}
