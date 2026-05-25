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
    let on_disk_size = match dir_size(meta.data_dir) {
        Ok(n) => n,
        Err(e) => {
            // Surface the read failure rather than silently lying with 0.
            // We still hand the dashboard *something* (0) so the rest of
            // the snapshot remains useful, but the operator gets a log
            // line explaining why disk usage looks wrong.
            tracing::warn!(
                data_dir = %meta.data_dir.display(),
                error = %e,
                "snapshot: dir_size failed; reporting 0",
            );
            0
        }
    };

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

/// Tracks the soonest- and latest-expiring TTL'd entries seen so far.
#[derive(Default)]
struct TtlRange {
    soonest: Option<EntryTtl>,
    latest: Option<EntryTtl>,
}

impl TtlRange {
    fn observe(&mut self, candidate: EntryTtl) {
        let is_soonest = self
            .soonest
            .as_ref()
            .is_none_or(|s| candidate.expires_at < s.expires_at);
        let is_latest = self
            .latest
            .as_ref()
            .is_none_or(|s| candidate.expires_at > s.expires_at);
        match (is_soonest, is_latest) {
            (true, true) => {
                self.soonest = Some(candidate.clone());
                self.latest = Some(candidate);
            }
            (true, false) => self.soonest = Some(candidate),
            (false, true) => self.latest = Some(candidate),
            (false, false) => {}
        }
    }

    fn into_parts(self) -> (Option<EntryTtl>, Option<EntryTtl>) {
        (self.soonest, self.latest)
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
    let mut ttl_range = TtlRange::default();
    while let Some(item) = iter.next().await {
        let entry = match item {
            Ok(e) => e,
            Err(_) => continue,
        };
        stats.entry_count += 1;
        if sunset_sync::routing::is_subscription_name(entry.name.as_ref()) {
            stats.subscription_entries += 1;
        }
        match entry.expires_at {
            None => stats.entries_without_ttl += 1,
            Some(t) => {
                stats.entries_with_ttl += 1;
                ttl_range.observe(EntryTtl {
                    expires_at: t,
                    vk: entry.verifying_key.clone(),
                    name: entry.name.clone(),
                });
            }
        }
    }
    (stats.soonest_expiry, stats.latest_expiry) = ttl_range.into_parts();
    stats
}

/// Recursive disk usage in bytes for the regular files under `root`.
///
/// Returns `Ok(0)` for an empty tree, `Err` if any directory listing or
/// metadata read fails. Callers that want best-effort reporting must
/// translate the error explicitly (typically: log + fall back to 0);
/// the function deliberately does not swallow errors itself.
fn dir_size(root: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        for entry in std::fs::read_dir(&p)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use sunset_store::{AcceptAllVerifier, SignedKvEntry, VerifyingKey};
    use sunset_store_memory::MemoryStore;

    fn vk(b: &'static [u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::from_static(b))
    }

    fn block(payload: &'static [u8]) -> sunset_store::ContentBlock {
        sunset_store::ContentBlock {
            data: Bytes::from_static(payload),
            references: vec![],
        }
    }

    fn entry_for(
        b: &sunset_store::ContentBlock,
        vk_bytes: &'static [u8],
        name: &[u8],
        priority: u64,
        expires_at: Option<u64>,
    ) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name: Bytes::copy_from_slice(name),
            value_hash: b.hash(),
            priority,
            expires_at,
            signature: Bytes::from_static(b"sig"),
        }
    }

    fn fresh_store() -> MemoryStore {
        MemoryStore::new(Arc::new(AcceptAllVerifier))
    }

    #[tokio::test]
    async fn collect_store_stats_empty_store_is_all_zero() {
        let store = fresh_store();
        let stats = collect_store_stats(&store).await;
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.entries_with_ttl, 0);
        assert_eq!(stats.entries_without_ttl, 0);
        assert_eq!(stats.subscription_entries, 0);
        assert!(stats.soonest_expiry.is_none());
        assert!(stats.latest_expiry.is_none());
        // Cursor on an empty store is the next-to-be-assigned sequence.
        assert!(stats.cursor.is_some());
    }

    #[tokio::test]
    async fn collect_store_stats_single_entry_min_eq_max() {
        let store = fresh_store();
        let b = block(b"x");
        store
            .insert(entry_for(&b, b"a", b"r", 1, Some(500)), Some(b))
            .await
            .unwrap();
        let stats = collect_store_stats(&store).await;
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.entries_with_ttl, 1);
        assert_eq!(stats.entries_without_ttl, 0);
        let soonest = stats.soonest_expiry.expect("soonest");
        let latest = stats.latest_expiry.expect("latest");
        assert_eq!(soonest.expires_at, 500);
        assert_eq!(latest.expires_at, 500);
        assert_eq!(soonest.name.as_ref(), b"r");
        assert_eq!(latest.name.as_ref(), b"r");
    }

    #[tokio::test]
    async fn collect_store_stats_multiple_ttls_picks_min_and_max() {
        let store = fresh_store();
        let b = block(b"x");
        // Insert out of order so the test would catch an `observe()`
        // that depended on insertion order.
        for (name, ttl) in [
            (&b"mid"[..], 500u64),
            (&b"late"[..], 1000),
            (&b"early"[..], 100),
            (&b"middle2"[..], 400),
        ] {
            let e = entry_for(&b, b"a", name, 1, Some(ttl));
            store.insert(e, Some(b.clone())).await.unwrap();
        }
        let stats = collect_store_stats(&store).await;
        assert_eq!(stats.entries_with_ttl, 4);
        assert_eq!(stats.entries_without_ttl, 0);
        let soonest = stats.soonest_expiry.expect("soonest");
        let latest = stats.latest_expiry.expect("latest");
        assert_eq!(soonest.expires_at, 100, "earliest wins");
        assert_eq!(soonest.name.as_ref(), b"early");
        assert_eq!(latest.expires_at, 1000, "latest wins");
        assert_eq!(latest.name.as_ref(), b"late");
    }

    #[tokio::test]
    async fn collect_store_stats_mixed_ttl_and_no_ttl_counts_split() {
        let store = fresh_store();
        let b = block(b"x");
        store
            .insert(entry_for(&b, b"a", b"forever", 1, None), Some(b.clone()))
            .await
            .unwrap();
        store
            .insert(entry_for(&b, b"a", b"will-expire", 1, Some(200)), Some(b))
            .await
            .unwrap();
        let stats = collect_store_stats(&store).await;
        assert_eq!(stats.entry_count, 2);
        assert_eq!(stats.entries_with_ttl, 1);
        assert_eq!(stats.entries_without_ttl, 1);
        let soonest = stats.soonest_expiry.expect("soonest");
        let latest = stats.latest_expiry.expect("latest");
        assert_eq!(soonest.expires_at, 200);
        assert_eq!(latest.expires_at, 200);
    }

    #[tokio::test]
    async fn collect_store_stats_counts_subscription_entries() {
        let store = fresh_store();
        let b = block(b"x");
        store
            .insert(entry_for(&b, b"a", b"regular", 1, None), Some(b.clone()))
            .await
            .unwrap();
        let mut sub_name = sunset_sync::routing::SUBSCRIBE_PREFIX.to_vec();
        sub_name.extend_from_slice(b"some-suffix");
        store
            .insert(entry_for(&b, b"a", &sub_name, 1, None), Some(b))
            .await
            .unwrap();
        let stats = collect_store_stats(&store).await;
        assert_eq!(stats.entry_count, 2);
        assert_eq!(stats.subscription_entries, 1);
    }

    #[test]
    fn ttl_range_empty_yields_none_none() {
        let r = TtlRange::default();
        let (s, l) = r.into_parts();
        assert!(s.is_none());
        assert!(l.is_none());
    }

    #[test]
    fn ttl_range_single_observation_sets_both_bounds() {
        let mut r = TtlRange::default();
        r.observe(EntryTtl {
            expires_at: 42,
            vk: vk(b"k"),
            name: Bytes::from_static(b"n"),
        });
        let (s, l) = r.into_parts();
        assert_eq!(s.unwrap().expires_at, 42);
        assert_eq!(l.unwrap().expires_at, 42);
    }

    #[test]
    fn ttl_range_handles_descending_then_ascending_order() {
        let mut r = TtlRange::default();
        for (n, t) in [(&b"a"[..], 500u64), (&b"b"[..], 100), (&b"c"[..], 1000)] {
            r.observe(EntryTtl {
                expires_at: t,
                vk: vk(b"k"),
                name: Bytes::copy_from_slice(n),
            });
        }
        let (s, l) = r.into_parts();
        assert_eq!(s.as_ref().unwrap().expires_at, 100);
        assert_eq!(s.unwrap().name.as_ref(), b"b");
        assert_eq!(l.as_ref().unwrap().expires_at, 1000);
        assert_eq!(l.unwrap().name.as_ref(), b"c");
    }

    #[test]
    fn dir_size_empty_dir_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dir_size(dir.path()).unwrap(), 0);
    }

    #[test]
    fn dir_size_single_file_is_file_len() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        std::fs::write(&path, b"hello world").unwrap();
        assert_eq!(dir_size(dir.path()).unwrap(), 11);
    }

    #[test]
    fn dir_size_nested_dirs_sums_all_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"abc").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("b.txt"), b"defgh").unwrap();
        let subsub = sub.join("deeper");
        std::fs::create_dir(&subsub).unwrap();
        std::fs::write(subsub.join("c.txt"), b"ij").unwrap();
        assert_eq!(dir_size(dir.path()).unwrap(), 3 + 5 + 2);
    }

    #[test]
    fn dir_size_missing_path_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let err = dir_size(&missing).expect_err("missing path must error");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    /// Missing nested path: parent exists, tail does not. Exercises the
    /// `read_dir(p)?` propagation from inside the walk loop, not just
    /// the initial push.
    #[test]
    fn dir_size_missing_nested_dir_path_errors() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        let err = dir_size(&nested).expect_err("missing nested path must error");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
