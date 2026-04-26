//! Verify that MemoryStore satisfies the sunset-store conformance suite.

use sunset_store::test_helpers::run_conformance_suite;
use sunset_store_memory::MemoryStore;

#[tokio::test]
async fn memory_store_passes_conformance_suite() {
    run_conformance_suite(MemoryStore::with_accept_all).await;
}
