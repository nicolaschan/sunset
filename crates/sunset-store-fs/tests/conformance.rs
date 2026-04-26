//! Conformance: drive the entire shared sunset-store conformance suite
//! against FsStore, with a fresh temp directory + FsStore per sub-test.

use sunset_store::test_helpers::run_conformance_suite;
use sunset_store_fs::FsStore;
use tempfile::TempDir;

#[tokio::test]
async fn fs_store_passes_conformance_suite() {
    run_conformance_suite(|| async {
        // Each sub-test gets its own directory. We leak the TempDir
        // (`keep()` since tempfile 3.7+) so it survives until the FsStore
        // is dropped at the end of the sub-test; the OS will clean up the
        // directory eventually, which is acceptable for tests.
        let dir = TempDir::new().unwrap().keep();
        FsStore::new(dir).await.unwrap()
    })
    .await;
}
