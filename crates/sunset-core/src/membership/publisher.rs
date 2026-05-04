//! Heartbeat publisher: spawns a task that periodically writes a
//! `<room_fp>/presence/<my_pk>` entry into the local store. The
//! engine's existing room_filter subscription propagates these to
//! peers automatically.
//!
//! The body of each heartbeat is a postcard-encoded `PresenceBody`
//! carrying the user's chosen display name (or `None` if unset).
//! `update_name` swaps the current name and notifies the publisher
//! task so a fresh heartbeat lands within milliseconds, not on the
//! next interval tick.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::FutureExt;
use tokio::sync::Notify;

#[cfg(not(target_arch = "wasm32"))]
use tokio::time::sleep;
#[cfg(target_arch = "wasm32")]
use wasmtimer::tokio::sleep;

use crate::Identity;
use crate::membership::body::PresenceBody;
use sunset_store::{ContentBlock, SignedKvEntry, Store, canonical::signing_payload};

/// Maximum display-name length, counted in `chars()` (Unicode scalar
/// values, NOT grapheme clusters). Defense in depth — the UI input
/// also enforces this via `maxlength`.
pub const MAX_NAME_CHARS: usize = 64;

/// Cloneable handle returned by `spawn_publisher`. Keeps a shared
/// `current_name` cell and a `Notify` that wakes the publisher loop.
#[derive(Clone)]
pub struct PublisherHandle {
    current_name: Rc<RefCell<Option<String>>>,
    notify: Rc<Notify>,
    /// Last priority used so we can guarantee strict monotonic increase
    /// even when two publishes happen within the same millisecond.
    last_priority: Rc<RefCell<u64>>,
}

impl PublisherHandle {
    /// Set the display name. Trims and truncates per `MAX_NAME_CHARS`.
    /// Empty after trim ⇒ `None`. Idempotent — equal value is a no-op
    /// (no extra publish).
    pub fn update_name(&self, raw: &str) {
        let normalized = normalize_name(raw);
        let mut slot = self.current_name.borrow_mut();
        if *slot == normalized {
            return;
        }
        *slot = normalized;
        drop(slot);
        self.notify.notify_one();
    }

    /// Read the current name. Useful for tests + the `Client::set_self_name`
    /// "first heartbeat carries the name" wiring.
    pub fn name(&self) -> Option<String> {
        self.current_name.borrow().clone()
    }
}

fn normalize_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_NAME_CHARS).collect::<String>())
}

/// Spawn the heartbeat publisher. Runs forever (host-process / page lifetime).
pub fn spawn_publisher<S: Store + 'static>(
    identity: Identity,
    room_fp_hex: String,
    store: Arc<S>,
    interval_ms: u64,
    ttl_ms: u64,
) -> PublisherHandle {
    let handle = PublisherHandle {
        current_name: Rc::new(RefCell::new(None)),
        notify: Rc::new(Notify::new()),
        last_priority: Rc::new(RefCell::new(0)),
    };
    let task_handle = handle.clone();
    sunset_sync::spawn::spawn_local(async move {
        let my_hex = hex::encode(identity.store_verifying_key().as_bytes());
        let name_str = format!("{room_fp_hex}/presence/{my_hex}");
        loop {
            if let Err(e) = publish_once(&identity, &name_str, &*store, ttl_ms, &task_handle).await
            {
                tracing::warn!("presence publisher: {e}");
            }
            // Sleep OR wake on update_name.
            futures::select_biased! {
                _ = task_handle.notify.notified().fuse() => {}
                _ = sleep(Duration::from_millis(interval_ms)).fuse() => {}
            }
        }
    });
    handle
}

async fn publish_once<S: Store + 'static>(
    identity: &Identity,
    name_str: &str,
    store: &S,
    ttl_ms: u64,
    handle: &PublisherHandle,
) -> Result<(), String> {
    let body = PresenceBody {
        name: handle.name(),
    };
    let data = postcard::to_stdvec(&body).map_err(|e| format!("encode body: {e}"))?;
    let block = ContentBlock {
        data: Bytes::from(data),
        references: vec![],
    };
    let value_hash = block.hash();
    let wall_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // Ensure strictly monotonic priority even when two publishes land in the
    // same millisecond (common in tests). `last_priority` is updated only on
    // a successful insert so a Stale error doesn't advance the counter.
    let now = {
        let last = *handle.last_priority.borrow();
        wall_ms.max(last + 1)
    };
    let mut entry = SignedKvEntry {
        verifying_key: identity.store_verifying_key(),
        name: Bytes::from(name_str.to_owned()),
        value_hash,
        priority: now,
        expires_at: Some(now + ttl_ms),
        signature: Bytes::new(),
    };
    let payload = signing_payload(&entry);
    let sig = identity.sign(&payload);
    entry.signature = Bytes::copy_from_slice(&sig.to_bytes());
    store
        .insert(entry, Some(block))
        .await
        .map_err(|e| format!("{e}"))?;
    *handle.last_priority.borrow_mut() = now;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;
    use rand_core::OsRng;
    use std::sync::Arc;
    use sunset_store::{AcceptAllVerifier, Store};
    use sunset_store_memory::MemoryStore;

    fn test_store() -> Arc<MemoryStore> {
        Arc::new(MemoryStore::new(Arc::new(AcceptAllVerifier)))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_name_trims_whitespace() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let identity = Identity::generate(&mut OsRng);
                let store = test_store();
                let handle = spawn_publisher(
                    identity.clone(),
                    "ff00".to_owned(),
                    store.clone(),
                    60_000,
                    180_000,
                );
                handle.update_name("  alice  ");
                assert_eq!(handle.name(), Some("alice".to_owned()));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_name_empty_becomes_none() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let identity = Identity::generate(&mut OsRng);
                let store = test_store();
                let handle = spawn_publisher(
                    identity.clone(),
                    "ff00".to_owned(),
                    store.clone(),
                    60_000,
                    180_000,
                );
                handle.update_name("alice");
                handle.update_name("   ");
                assert_eq!(handle.name(), None);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_name_truncates_to_64_chars() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let identity = Identity::generate(&mut OsRng);
                let store = test_store();
                let handle = spawn_publisher(
                    identity.clone(),
                    "ff00".to_owned(),
                    store.clone(),
                    60_000,
                    180_000,
                );
                handle.update_name(&"a".repeat(100));
                assert_eq!(handle.name().unwrap().chars().count(), 64);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_name_triggers_immediate_republish() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let identity = Identity::generate(&mut OsRng);
                let store = test_store();
                let handle = spawn_publisher(
                    identity.clone(),
                    "ff00".to_owned(),
                    store.clone(),
                    60_000,
                    180_000,
                );
                let key_name = format!(
                    "ff00/presence/{}",
                    hex::encode(identity.store_verifying_key().as_bytes()),
                );
                wait_for_entry(&store, &key_name).await;
                let first_hash = current_hash(&store, &key_name).await;

                handle.update_name("alice");
                wait_for_hash_change(&store, &key_name, first_hash).await;

                let block = block_for(&store, &key_name).await;
                let body: PresenceBody = postcard::from_bytes(&block.data).unwrap();
                assert_eq!(body.name, Some("alice".to_owned()));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_name_idempotent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let identity = Identity::generate(&mut OsRng);
                let store = test_store();
                let handle = spawn_publisher(
                    identity.clone(),
                    "ff00".to_owned(),
                    store.clone(),
                    60_000,
                    180_000,
                );
                let key_name = format!(
                    "ff00/presence/{}",
                    hex::encode(identity.store_verifying_key().as_bytes()),
                );
                wait_for_entry(&store, &key_name).await;
                handle.update_name("alice");
                wait_for_body_name(&store, &key_name, Some("alice".to_owned())).await;
                let h_after_first = current_hash(&store, &key_name).await;

                handle.update_name("alice");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                let h_now = current_hash(&store, &key_name).await;
                assert_eq!(
                    h_now, h_after_first,
                    "no extra publish for idempotent update"
                );
            })
            .await;
    }

    // -- test helpers --

    async fn wait_for_entry(store: &Arc<MemoryStore>, name: &str) {
        for _ in 0..200 {
            if get_entry(store, name).await.is_some() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("entry {name} never appeared");
    }

    async fn wait_for_hash_change(store: &Arc<MemoryStore>, name: &str, old: sunset_store::Hash) {
        for _ in 0..200 {
            if let Some(e) = get_entry(store, name).await {
                if e.value_hash != old {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("hash for {name} never changed");
    }

    async fn wait_for_body_name(store: &Arc<MemoryStore>, name: &str, expected: Option<String>) {
        for _ in 0..200 {
            if let Some(e) = get_entry(store, name).await {
                if let Ok(Some(b)) = store.get_content(&e.value_hash).await {
                    if let Ok(body) = postcard::from_bytes::<PresenceBody>(&b.data) {
                        if body.name == expected {
                            return;
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("body name for {name} never became {expected:?}");
    }

    async fn current_hash(store: &Arc<MemoryStore>, name: &str) -> sunset_store::Hash {
        get_entry(store, name).await.unwrap().value_hash
    }

    async fn block_for(store: &Arc<MemoryStore>, name: &str) -> sunset_store::ContentBlock {
        let e = get_entry(store, name).await.unwrap();
        store.get_content(&e.value_hash).await.unwrap().unwrap()
    }

    async fn get_entry(
        store: &Arc<MemoryStore>,
        name: &str,
    ) -> Option<sunset_store::SignedKvEntry> {
        use bytes::Bytes;
        use futures::StreamExt;
        use sunset_store::{Filter, Replay};
        let mut sub = store
            .subscribe(Filter::Namespace(Bytes::from(name.to_owned())), Replay::All)
            .await
            .unwrap();
        let next = sub.next().await;
        match next {
            Some(Ok(sunset_store::Event::Inserted(e))) => Some(e),
            Some(Ok(sunset_store::Event::Replaced { new, .. })) => Some(new),
            _ => None,
        }
    }
}
