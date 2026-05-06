//! Native stub for `sunset-store-indexeddb`. The crate is wasm-only; this
//! stub exists so that `cargo build --workspace` (and `cargo test --workspace`)
//! work on developer machines and in CI. All methods panic — production
//! code paths only construct `IndexedDbStore` from a wasm32 target.

use std::sync::Arc;

/// Default database name (matches the wasm32 build).
pub const DEFAULT_DATABASE_NAME: &str = "sunset-store";

use async_trait::async_trait;
use sunset_store::{
    ContentBlock, Cursor, EntryStream, EventStream, Filter, Hash, Replay, Result,
    SignatureVerifier, SignedKvEntry, Store, VerifyingKey,
};

/// Native stub of the wasm32 `IndexedDbStore`. Construction panics — this
/// type only exists so the crate compiles into `cargo build --workspace`.
pub struct IndexedDbStore;

impl IndexedDbStore {
    pub async fn open(_database_name: &str, _verifier: Arc<dyn SignatureVerifier>) -> Result<Self> {
        panic!("sunset-store-indexeddb is only available on wasm32 targets")
    }
}

/// Stub: native targets cannot delete an IndexedDB database.
pub async fn delete_database(_database_name: &str) -> Result<()> {
    panic!("sunset-store-indexeddb is only available on wasm32 targets")
}

#[async_trait(?Send)]
impl Store for IndexedDbStore {
    async fn insert(&self, _entry: SignedKvEntry, _blob: Option<ContentBlock>) -> Result<()> {
        unreachable!("native stub")
    }
    async fn put_content(&self, _block: ContentBlock) -> Result<Hash> {
        unreachable!("native stub")
    }
    async fn get_content(&self, _hash: &Hash) -> Result<Option<ContentBlock>> {
        unreachable!("native stub")
    }
    async fn get_entry(&self, _vk: &VerifyingKey, _name: &[u8]) -> Result<Option<SignedKvEntry>> {
        unreachable!("native stub")
    }
    async fn iter<'a>(&'a self, _filter: Filter) -> Result<EntryStream<'a>> {
        unreachable!("native stub")
    }
    async fn subscribe<'a>(&'a self, _filter: Filter, _replay: Replay) -> Result<EventStream<'a>> {
        unreachable!("native stub")
    }
    async fn delete_expired(&self, _now: u64) -> Result<usize> {
        unreachable!("native stub")
    }
    async fn gc_blobs(&self) -> Result<usize> {
        unreachable!("native stub")
    }
    async fn current_cursor(&self) -> Result<Cursor> {
        unreachable!("native stub")
    }
    fn verifier(&self) -> Arc<dyn SignatureVerifier> {
        unreachable!("native stub")
    }
}
