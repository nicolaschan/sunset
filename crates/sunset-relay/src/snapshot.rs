//! Engine-side snapshot construction for the dashboard / identity routes.
//!
//! Reads `Rc<SyncEngine>` + `Arc<FsStore>` and produces `Send` PODs
//! (`DashboardSnapshot` / `IdentitySnapshot`). Runs inside the LocalSet
//! command pump; never crosses runtimes itself.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use sunset_store::{Filter, Store};
use sunset_store_fs::FsStore;
use sunset_sync::SyncEngine;

use crate::bridge::{DashboardSnapshot, EntryTtl, IdentitySnapshot, StoreStats};

/// Static-for-the-process metadata that the snapshot builder needs.
/// Bundled to keep `build_dashboard_snapshot` under clippy's
/// `too_many_arguments` threshold without papering over the lint.
pub struct RelayMeta<'a> {
    pub data_dir: &'a Path,
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],
    pub listen_addr: std::net::SocketAddr,
    pub dial_url: &'a str,
    pub configured_peers: &'a [String],
}

/// The concrete `SyncEngine` type the relay holds. The `SpawningAcceptor`
/// wrapping is a private detail of `relay.rs` — for snapshot purposes we
/// only need the engine APIs (`connected_peers`, `subscriptions_snapshot`)
/// which are independent of the wrapping transport. We type-erase via
/// generics in the function signature so the snapshot builder doesn't
/// need to know the wrapper's full type.
pub async fn build_dashboard_snapshot<T>(
    engine: &Rc<SyncEngine<FsStore, T>>,
    store: &Arc<FsStore>,
    meta: &RelayMeta<'_>,
) -> DashboardSnapshot
where
    T: sunset_sync::Transport + 'static,
    T::Connection: 'static,
{
    let connected_peers = engine.connected_peers().await;
    let subscriptions = engine.subscriptions_snapshot().await;
    let store_stats = collect_store_stats(&**store).await;
    let on_disk_size = dir_size(meta.data_dir).unwrap_or(0);

    DashboardSnapshot {
        ed25519_public: meta.ed25519_public,
        x25519_public: meta.x25519_public,
        listen_addr: meta.listen_addr,
        dial_url: meta.dial_url.to_owned(),
        configured_peers: meta.configured_peers.to_vec(),
        connected_peers,
        subscriptions,
        data_dir: meta.data_dir.to_path_buf(),
        on_disk_size,
        store_stats,
    }
}

pub fn build_identity_snapshot(
    ed25519_public: [u8; 32],
    x25519_public: [u8; 32],
    dial_url: &str,
    webtransport_cert_sha256: Option<&str>,
) -> IdentitySnapshot {
    IdentitySnapshot {
        ed25519_public,
        x25519_public,
        dial_url: dial_url.to_owned(),
        webtransport_cert_sha256: webtransport_cert_sha256.map(str::to_owned),
    }
}

async fn collect_store_stats<S: Store>(store: &S) -> StoreStats {
    let mut stats = StoreStats::default();
    if let Ok(c) = store.current_cursor().await {
        stats.cursor = Some(c.0);
    }
    let mut iter = match store.iter(Filter::NamePrefix(Bytes::new())).await {
        Ok(s) => s,
        Err(_) => return stats,
    };
    while let Some(item) = iter.next().await {
        let entry = match item {
            Ok(e) => e,
            Err(_) => continue,
        };
        stats.entry_count += 1;
        if entry.name.as_ref() == sunset_sync::reserved::SUBSCRIBE_NAME {
            stats.subscription_entries += 1;
        }
        match entry.expires_at {
            None => stats.entries_without_ttl += 1,
            Some(t) => {
                stats.entries_with_ttl += 1;
                let candidate = EntryTtl {
                    expires_at: t,
                    vk: entry.verifying_key.clone(),
                    name: entry.name.clone(),
                };
                if stats
                    .soonest_expiry
                    .as_ref()
                    .is_none_or(|s| t < s.expires_at)
                {
                    stats.soonest_expiry = Some(EntryTtl {
                        expires_at: candidate.expires_at,
                        vk: candidate.vk.clone(),
                        name: candidate.name.clone(),
                    });
                }
                if stats
                    .latest_expiry
                    .as_ref()
                    .is_none_or(|s| t > s.expires_at)
                {
                    stats.latest_expiry = Some(candidate);
                }
            }
        }
    }
    stats
}

fn dir_size(root: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let rd = match std::fs::read_dir(&p) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}
