//! In-memory implementation of `sunset-store::Store`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use sunset_store::{
    ContentBlock, Cursor, Hash, SignedKvEntry, SignatureVerifier, VerifyingKey,
};
use tokio::sync::Mutex;

/// Composite key: `(verifying_key, name)`.
type KvKey = (VerifyingKey, bytes::Bytes);

#[derive(Debug)]
pub(crate) struct StoredEntry {
    pub entry:    SignedKvEntry,
    pub sequence: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Inner {
    pub entries:      BTreeMap<KvKey, StoredEntry>,
    pub blobs:        HashMap<Hash, ContentBlock>,
    pub next_sequence: u64,
}

impl Inner {
    pub fn assign_sequence(&mut self) -> u64 {
        let s = self.next_sequence;
        self.next_sequence += 1;
        s
    }
}

/// In-memory `Store` implementation.
pub struct MemoryStore {
    pub(crate) verifier: Arc<dyn SignatureVerifier>,
    pub(crate) inner:    Arc<Mutex<Inner>>,
}

impl MemoryStore {
    /// Construct with the given signature verifier.
    pub fn new(verifier: Arc<dyn SignatureVerifier>) -> Self {
        Self {
            verifier,
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    /// Convenience: construct with `AcceptAllVerifier`. For tests.
    pub fn with_accept_all() -> Self {
        Self::new(Arc::new(sunset_store::AcceptAllVerifier))
    }

    /// Returns the current cursor (last assigned sequence).
    pub async fn current_cursor_now(&self) -> Cursor {
        let inner = self.inner.lock().await;
        Cursor(inner.next_sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_store_starts_at_cursor_zero() {
        let store = MemoryStore::with_accept_all();
        assert_eq!(store.current_cursor_now().await, Cursor(0));
    }
}
