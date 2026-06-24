//! Core types: PeerId, PeerAddr, SyncConfig, TrustSet.

use std::collections::HashSet;
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use sunset_store::{Filter, VerifyingKey};

use crate::routing;

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
    /// Filter used for the bootstrap digest exchange. Always covers the
    /// `_sunset-sync/subscribe/` prefix so a (re)connected peer
    /// rehydrates its view of our per-(filter, provider) subscription
    /// entries.
    pub bootstrap_filter: Filter,
    /// Cadence at which each per-peer task sends `SyncMessage::Ping`.
    /// Default 15 s. Three intervals must elapse without a `Pong`
    /// before the connection is declared dead.
    pub heartbeat_interval: Duration,
    /// If no `Pong` arrives within this window, the per-peer task emits
    /// `Disconnected { reason: "heartbeat timeout" }`. Default 45 s
    /// (= 3 × `heartbeat_interval`).
    pub heartbeat_timeout: Duration,
    /// Cadence at which each per-peer task sends `SyncMessage::UnreliablePing`
    /// *over the datagram channel* to probe whether its outbound datagram
    /// path is still delivering. Independent of (and faster than) the
    /// reliable `heartbeat_interval`, because a silently-dead datagram path
    /// is invisible to the reliable Ping/Pong yet still drops voice. Default
    /// 2 s.
    pub datagram_probe_interval: Duration,
    /// If no `UnreliablePong` arrives within this window, the datagram path
    /// is considered dead and ephemeral (voice) traffic falls back to the
    /// reliable channel until the datagram path recovers. The connection
    /// itself stays up (reliable Ping/Pong is the connection-liveness
    /// signal); only the *channel choice* for voice changes. Default 6 s
    /// (= 3 × `datagram_probe_interval`).
    pub datagram_path_timeout: Duration,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            protocol_version: 1,
            anti_entropy_interval: Duration::from_secs(30),
            bloom_size_bits: 4096,
            bloom_hash_fns: 4,
            bootstrap_filter: Filter::NamePrefix(routing::SUBSCRIBE_PREFIX.into()),
            heartbeat_interval: Duration::from_secs(15),
            heartbeat_timeout: Duration::from_secs(45),
            datagram_probe_interval: Duration::from_secs(2),
            datagram_path_timeout: Duration::from_secs(6),
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
