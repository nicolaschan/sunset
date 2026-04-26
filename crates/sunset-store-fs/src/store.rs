//! FsStore + Store impl.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sunset_store::{AcceptAllVerifier, Error, Result, SignatureVerifier};
use tokio::sync::Mutex;
use tokio_rusqlite::Connection;

use crate::schema;
use crate::subscription::SubscriptionList;

#[allow(dead_code)]
pub struct FsStore {
    pub(crate) root: Arc<PathBuf>,
    pub(crate) conn: Connection,
    pub(crate) verifier: Arc<dyn SignatureVerifier>,
    pub(crate) subscriptions: Arc<SubscriptionList>,
    pub(crate) writer_mutex: Arc<Mutex<()>>,
}

impl FsStore {
    /// Open or create an FsStore rooted at `root`. Creates `root/content/`
    /// and `root/db.sqlite`, applies the schema, and returns a ready-to-use
    /// store. Default verifier is `AcceptAllVerifier`; use
    /// `with_verifier` to override.
    pub async fn new<P: AsRef<Path>>(root: P) -> Result<Self> {
        Self::with_verifier(root, Arc::new(AcceptAllVerifier)).await
    }

    pub async fn with_verifier<P: AsRef<Path>>(
        root: P,
        verifier: Arc<dyn SignatureVerifier>,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let content_dir = root.join("content");
        let db_path = root.join("db.sqlite");

        std::fs::create_dir_all(&content_dir)
            .map_err(|e| Error::Backend(format!("create content dir: {e}")))?;

        let conn = Connection::open(&db_path)
            .await
            .map_err(|e| Error::Backend(format!("open sqlite: {e}")))?;

        conn.call(|c| {
            schema::apply_schema(c).map_err(tokio_rusqlite::Error::from)
        })
        .await
        .map_err(|e| Error::Backend(format!("apply schema: {e}")))?;

        Ok(Self {
            root: Arc::new(root),
            conn,
            verifier,
            subscriptions: Arc::new(SubscriptionList::new()),
            writer_mutex: Arc::new(Mutex::new(())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn new_creates_directory_and_database() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        assert!(dir.path().join("content").is_dir());
        assert!(dir.path().join("db.sqlite").is_file());
        // Re-opening the same path must succeed (idempotent DDL).
        drop(store);
        let _store2 = FsStore::new(dir.path()).await.unwrap();
    }
}
