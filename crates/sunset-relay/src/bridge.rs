//! Send-friendly types that cross between the axum HTTP layer and the
//! engine-side LocalSet.
//!
//! axum handlers must be `Send` (axum spawns one task per request via
//! `tokio::spawn`, which has a `Send` bound). The engine, by contrast,
//! is `?Send` (it holds `Rc<…>` internally for WASM compatibility). The
//! bridge is just a small set of plain-old-data types and an mpsc-based
//! command protocol — handlers send commands, the engine-side command
//! pump answers via oneshot replies built from immediate-mode reads of
//! `Rc<Engine>` + `Arc<Store>`.

use std::net::SocketAddr;

use bytes::Bytes;
use tokio::sync::oneshot;

use sunset_store::{Filter, VerifyingKey};
use sunset_sync::PeerId;

/// One in-flight request from the axum side to the engine side.
pub enum RelayCommand {
    /// Build a fresh dashboard snapshot. Reply is the rendered POD.
    Snapshot {
        reply: oneshot::Sender<DashboardSnapshot>,
    },
    /// Build a fresh identity snapshot for the JSON `/` endpoint.
    Identity {
        reply: oneshot::Sender<IdentitySnapshot>,
    },
}

/// Send-only POD that captures everything the dashboard renderer needs.
/// Built on the engine-side; rendered (HTML) on the axum side.
#[derive(Clone, Debug)]
pub struct DashboardSnapshot {
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],
    pub listen_addr: SocketAddr,
    pub dial_url: String,

    pub configured_peers: Vec<String>,
    pub connected_peers: Vec<PeerId>,

    pub subscriptions: Vec<(PeerId, Filter)>,

    pub data_dir: std::path::PathBuf,
    pub on_disk_size: u64,
    pub store_stats: StoreStats,
}

/// Subset of `DashboardSnapshot` that's used for the JSON `/` route.
/// Kept separate so the JSON handler can answer with a smaller round-trip
/// to the engine.
#[derive(Clone, Debug)]
pub struct IdentitySnapshot {
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],
    pub dial_url: String,
    /// SHA-256 hex of the SPKI for the relay's self-signed WebTransport
    /// cert (the `serverCertificateHashes` value the browser pins).
    /// `None` when the relay didn't manage to bind its UDP listener.
    ///
    /// We deliberately ship *only the hash*, not a full URL — the relay
    /// has no reliable way to know its own public hostname (it could
    /// be behind any number of proxies, and binds `0.0.0.0` in
    /// production), so the resolver builds the WT URL from the
    /// user-typed authority instead.
    pub webtransport_cert_sha256: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct StoreStats {
    pub entry_count: u64,
    pub entries_with_ttl: u64,
    pub entries_without_ttl: u64,
    pub subscription_entries: u64,
    pub cursor: Option<u64>,
    pub soonest_expiry: Option<EntryTtl>,
    pub latest_expiry: Option<EntryTtl>,
}

#[derive(Clone, Debug)]
pub struct EntryTtl {
    pub expires_at: u64,
    pub vk: VerifyingKey,
    pub name: Bytes,
}
