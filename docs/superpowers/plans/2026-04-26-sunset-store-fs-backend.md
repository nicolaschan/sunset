# sunset-store-fs Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `sunset-store-fs` crate — a native-only, on-disk backend for `sunset_store::Store` using SQLite (KV index) plus the filesystem (content blobs).

**Architecture:** SQLite holds the `entries` table (one row per `(verifying_key, name)` LWW winner) and provides the monotonic `sequence` cursor via `AUTOINCREMENT`. Content blobs live on disk under a sharded directory tree (`content/<2-hex>/<remaining-hex>`) and are written via tempfile-then-rename. The Rust→SQLite glue is `tokio_rusqlite`, which owns a single `rusqlite::Connection` on a dedicated worker thread and exposes async-callable closures — keeping the backend `?Send`-compatible and avoiding hand-rolled `spawn_blocking`. Subscriptions reuse the same broadcast-under-lock pattern proven in `sunset-store-memory`. Conformance is delegated to the existing `sunset-store::test_helpers` suite.

**Tech Stack:** Rust 2024 edition, `tokio_rusqlite` (async wrapper around rusqlite, bundled SQLite), `tempfile` (atomic blob writes), `tokio::sync::Mutex` (writer serialization), `async-trait` (`?Send` futures), `async-stream` (streaming results), plus the workspace's existing `bytes` / `blake3` / `postcard` / `futures` / `thiserror`.

**Spec:** [`docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md`](../specs/2026-04-25-sunset-store-and-sync-design.md) — sections "sunset-store-fs" (lines ~206–228) and "Conformance testing" (line ~230).

**Parent architecture spec:** [`docs/superpowers/specs/2026-04-25-sunset-chat-architecture-design.md`](../specs/2026-04-25-sunset-chat-architecture-design.md)

**Predecessor plan (already merged):** [`2026-04-25-sunset-store-core-and-memory-backend.md`](2026-04-25-sunset-store-core-and-memory-backend.md). The `Store` trait, types, error model, filter language, signature verifier, and conformance suite are already in tree. This plan builds *only* the new backend crate.

---

## File Structure

```
sunset/
├── Cargo.toml                                   # workspace root (modify: add member, deps)
└── crates/
    └── sunset-store-fs/                         # NEW crate
        ├── Cargo.toml
        ├── src/
        │   ├── lib.rs                           # FsStore re-export
        │   ├── store.rs                         # FsStore + Store impl (entry + lifecycle)
        │   ├── schema.rs                        # SQL schema constants + apply_schema()
        │   ├── blobs.rs                         # filesystem blob layer (sharded layout, atomic writes)
        │   ├── kv.rs                            # SQLite KV layer (insert/get/iter/delete/expired)
        │   ├── gc.rs                            # mark-and-sweep gc_blobs implementation
        │   └── subscription.rs                  # SubscriptionList (mirrors sunset-store-memory pattern)
        └── tests/
            └── conformance.rs                   # invokes run_conformance_suite against FsStore
```

Boundaries:
- `store.rs` — public `FsStore` struct, `new(path)` constructor, `Store` trait `impl` that delegates to the layer modules.
- `schema.rs` — pure data: SQL DDL strings, schema-version constant, `apply_schema(&Connection)` migration.
- `blobs.rs` — pure filesystem helpers: `blob_path(root, hash)`, `read_blob`, `write_blob_atomic`, `list_blob_hashes`. No SQLite knowledge.
- `kv.rs` — pure SQLite helpers: row → `SignedKvEntry` conversion, parameterized queries for each `Filter` variant. No filesystem knowledge.
- `gc.rs` — combines kv + blobs to walk reachability. Bridges the two layers.
- `subscription.rs` — `SubscriptionList` mirroring the proven memory-backend invariants (broadcast-under-lock, unbounded channels). Drop-in adapter pattern; do *not* import the memory crate's private type.

This decomposition keeps SQLite and filesystem code separately testable and lets `gc.rs` (the only cross-cutting logic) be small and focused.

---

## Cross-cutting design notes

These apply across multiple tasks. Read once before starting Task 1.

### Concurrency model

`FsStore` holds:
- `Arc<tokio_rusqlite::Connection>` — internally serializes all SQL on a worker thread; safe to clone the `Arc` and call `.call(...)` from anywhere.
- `Arc<PathBuf>` — root directory; `content/` and `db.sqlite` live under it.
- `Arc<dyn SignatureVerifier>` — same as `MemoryStore`.
- `Arc<SubscriptionList>` — same shape as `MemoryStore`.
- `Arc<tokio::sync::Mutex<()>>` — **writer serialization mutex**. Held across the whole `insert` / `delete_expired` / `gc_blobs` flow so that:
  1. The blob is durably on disk *before* the SQLite row referencing it is committed.
  2. Subscription broadcasts happen *while holding the write lock*, mirroring the memory backend's race-free invariant.

Read-only ops (`get_entry`, `get_content`, `iter`, `current_cursor`) do not take this mutex — they go straight to SQLite / filesystem.

### Insert ordering (atomicity)

For each `insert(entry, blob)`:
1. Acquire writer mutex.
2. In-memory checks: if `blob` is supplied, assert `entry.value_hash == blob.hash()` (`Error::HashMismatch`).
3. Call `SignatureVerifier::verify(&entry)` (`Error::SignatureInvalid`).
4. If `blob` is supplied, write it to the filesystem via tempfile-then-rename (idempotent; content-addressed). The blob is now durable.
5. Open a SQLite transaction. SELECT the existing row for `(verifying_key, name)`; if `existing.priority >= entry.priority`, ROLLBACK and return `Error::Stale`. Otherwise INSERT OR REPLACE the new row.
6. COMMIT the SQLite transaction. The new `sequence` is now visible.
7. Determine event variant (`Inserted` if no prior row, else `Replaced { old, new }`); broadcast it.
8. If the inserted entry's blob was new to disk, broadcast `BlobAdded { hash }`.
9. Release writer mutex.

If any step before COMMIT errors, the SQLite txn is dropped without committing → no row written. A blob written in step 4 may linger on disk; `gc_blobs` reclaims it later (lazy dangling refs are allowed by spec). Crash between step 6 and step 7 leaves a row with no broadcast — acceptable; subscribers can re-sync via `Replay::Since(cursor)` after restart.

### Cursor semantics

`sequence INTEGER PRIMARY KEY AUTOINCREMENT` gives monotonically increasing values that never roll back even on delete. `current_cursor()` returns `Cursor((SELECT seq FROM sqlite_sequence WHERE name='entries').unwrap_or(0) + 1)` — i.e. the next-to-be-assigned sequence — matching the contract documented at `crates/sunset-store/src/store.rs:62-67`.

`Replay::Since(c)` query: `SELECT * FROM entries WHERE sequence >= ?1 ORDER BY sequence ASC` (the spec defines `Since` as `>=`, not `>`).

### Filter → SQL mapping

| `Filter` variant | SQL `WHERE` clause |
|---|---|
| `Specific { vk, name }` | `verifying_key = ?1 AND name = ?2` (LIMIT 1) |
| `Keyspace(vk)` | `verifying_key = ?1` |
| `Namespace(name)` | `name = ?1` |
| `NamePrefix(prefix)` | `name >= ?1 AND name < ?1 \|\| 0xFF...` (range scan; for prefix, pass two BLOB bounds) |
| `Union(filters)` | OR of the above. v1: simple iteration in Rust over each subfilter and dedupe by `(vk, name)` is acceptable; do not generate one mega-SQL query. |

Implementing `NamePrefix` cleanly: compute the upper bound by appending one `0xFF` byte beyond the prefix length, but a simpler correct approach is `name >= prefix AND substr(name, 1, length(prefix)) = prefix`. Use whichever you can implement correctly — the conformance suite will catch errors.

### Blob filesystem layout

```
<root>/
├── db.sqlite
└── content/
    └── <hash[0..2]>/<hash[2..]>      # 2-byte (4-hex-char) shard prefix; remaining 60 hex chars
```

`blob_path(root, hash) = root.join("content").join(&hex[0..2]).join(&hex[2..])`. Atomic write: `tempfile::NamedTempFile::new_in(content_dir)?.persist(blob_path)?` — `persist` does an atomic rename on the same filesystem. Idempotent: writing the same hash twice is a no-op-or-overwrite, the result on disk is identical.

`list_blob_hashes(root)`: walk `<root>/content/*/*`, parse each filename as hex, yield `Hash`s. Used only by `gc_blobs`.

---

## Tasks

### Task 0: Make the conformance suite accept an async store factory

**Files:**
- Modify: `crates/sunset-store/src/test_helpers.rs:86-107` (signature + each `store_factory()` call site)
- Modify: `crates/sunset-store-memory/tests/conformance.rs` (call site)

**Why this is needed:** Plan 1's conformance suite signature is `pub async fn run_conformance_suite<S, F>(store_factory: F) where F: Fn() -> S`. `FsStore::new` is `async` (it does async filesystem creation + opens a `tokio_rusqlite::Connection`), so a sync factory can't construct it. Generalize to an async factory so both backends fit.

- [ ] **Step 1: Edit the signature in `crates/sunset-store/src/test_helpers.rs`**

Change the function header from:

```rust
pub async fn run_conformance_suite<S, F>(store_factory: F)
where
    S: Store,
    F: Fn() -> S,
```

to:

```rust
pub async fn run_conformance_suite<S, F, Fut>(store_factory: F)
where
    S: Store,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = S>,
```

- [ ] **Step 2: Update every call site inside the function body**

Each `&store_factory()` becomes `&store_factory().await`. There are 14 call sites (one per conformance sub-test, lines ~93–106). Apply the change uniformly.

- [ ] **Step 3: Update the memory backend's caller**

In `crates/sunset-store-memory/tests/conformance.rs`, change the call site from (e.g.) `run_conformance_suite(|| MemoryStore::new()).await` to `run_conformance_suite(|| async { MemoryStore::new() }).await`. (If the existing factory takes parameters like a verifier, wrap the same body in `async { ... }`.)

- [ ] **Step 4: Run the memory backend's conformance test to confirm no regression**

Run: `nix develop --command cargo test -p sunset-store-memory --test conformance --features sunset-store/test-helpers`
Expected: still passes — same tests, just with an async factory.

- [ ] **Step 5: Run the full workspace test suite**

Run: `nix develop --command cargo test --workspace --all-features`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/sunset-store/src/test_helpers.rs crates/sunset-store-memory/tests/conformance.rs
git commit -m "Generalize run_conformance_suite to accept an async store factory"
```

---

### Task 1: Add `sunset-store-fs` crate skeleton

**Files:**
- Modify: `Cargo.toml` (workspace root: add member + workspace dep entry)
- Create: `crates/sunset-store-fs/Cargo.toml`
- Create: `crates/sunset-store-fs/src/lib.rs`

- [ ] **Step 1: Edit `Cargo.toml` (workspace root)**

In the `[workspace]` `members` array, append `"crates/sunset-store-fs"`. In `[workspace.dependencies]`, append:

```toml
async-stream = "0.3"
tokio_rusqlite = { version = "0.6", features = ["bundled"] }
tempfile = "3"
sunset-store-fs = { path = "crates/sunset-store-fs" }
```

(`async-stream` is already used by the memory backend with a non-workspace pin; promote it to a workspace dep so both backends share the same version. If the memory backend's `Cargo.toml` had `async-stream = "0.3"` directly, change it to `async-stream.workspace = true` in the same step.)

- [ ] **Step 2: Create `crates/sunset-store-fs/Cargo.toml`**

```toml
[package]
name = "sunset-store-fs"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
async-trait.workspace = true
async-stream.workspace = true
blake3.workspace = true
bytes.workspace = true
futures.workspace = true
postcard.workspace = true
sunset-store.workspace = true
tempfile.workspace = true
thiserror.workspace = true
tokio.workspace = true
tokio_rusqlite.workspace = true

[dev-dependencies]
sunset-store = { workspace = true, features = ["test-helpers"] }
tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "time"] }
```

- [ ] **Step 3: Create `crates/sunset-store-fs/src/lib.rs`**

```rust
//! On-disk implementation of `sunset-store::Store` using SQLite for the KV
//! index and the filesystem for content blobs.

mod blobs;
mod gc;
mod kv;
mod schema;
mod store;
mod subscription;

pub use store::FsStore;
```

- [ ] **Step 4: Verify the workspace builds**

Run: `nix develop --command cargo build -p sunset-store-fs`
Expected: compiles with no errors. (The crate has no code yet beyond module declarations, but each declared module file must exist as an empty file before this passes — create them in step 5.)

- [ ] **Step 5: Create empty module files**

Create the following empty files (each just a single-line module doc comment so the build passes):

```rust
// crates/sunset-store-fs/src/blobs.rs
//! Filesystem blob layer (placeholder — see Task 3).
```
```rust
// crates/sunset-store-fs/src/gc.rs
//! Mark-and-sweep blob GC (placeholder — see Task 7).
```
```rust
// crates/sunset-store-fs/src/kv.rs
//! SQLite KV index layer (placeholder — see Task 4).
```
```rust
// crates/sunset-store-fs/src/schema.rs
//! SQL schema definitions (placeholder — see Task 2).
```
```rust
// crates/sunset-store-fs/src/store.rs
//! FsStore + Store impl (placeholder — see Task 2).

pub struct FsStore;
```
```rust
// crates/sunset-store-fs/src/subscription.rs
//! Subscription list (placeholder — see Task 8).
```

- [ ] **Step 6: Re-run the build to confirm**

Run: `nix develop --command cargo build -p sunset-store-fs`
Expected: clean compile.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/sunset-store-fs/
git commit -m "Scaffold sunset-store-fs crate with empty module placeholders"
```

---

### Task 2: SQL schema + `FsStore::new` lifecycle

**Files:**
- Modify: `crates/sunset-store-fs/src/schema.rs`
- Modify: `crates/sunset-store-fs/src/store.rs`

This task creates the on-disk layout (database + content directory) and applies the schema on first open.

- [ ] **Step 1: Write the schema constants in `schema.rs`**

```rust
//! SQL schema for the FsStore KV index.

pub const SCHEMA_VERSION: i32 = 1;

/// Idempotent DDL applied on every open. Uses `IF NOT EXISTS` so re-opening
/// an existing store is a no-op.
pub const SCHEMA_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
    sequence       INTEGER PRIMARY KEY AUTOINCREMENT,
    verifying_key  BLOB NOT NULL,
    name           BLOB NOT NULL,
    value_hash     BLOB NOT NULL,
    priority       INTEGER NOT NULL,
    expires_at     INTEGER,
    signature      BLOB NOT NULL,
    UNIQUE(verifying_key, name)
);

CREATE INDEX IF NOT EXISTS idx_entries_name
    ON entries(name);

CREATE INDEX IF NOT EXISTS idx_entries_expires_at
    ON entries(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS schema_meta (
    key   TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
"#;

/// Apply the DDL and record the schema version. Called by `FsStore::new`.
pub fn apply_schema(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_DDL)?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_meta(key, value) VALUES ('version', ?1)",
        [SCHEMA_VERSION],
    )?;
    Ok(())
}
```

(Note: the `rusqlite` types are re-exported through `tokio_rusqlite::rusqlite`. If you import `rusqlite::Connection` directly, add `rusqlite = { version = "0.31", default-features = false }` as a workspace dep. Simpler is `use tokio_rusqlite::rusqlite::{self, Connection};`.)

- [ ] **Step 2: Write `FsStore::new` in `store.rs`**

```rust
//! FsStore + Store impl.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sunset_store::{AcceptAllVerifier, Error, Result, SignatureVerifier};
use tokio::sync::Mutex;
use tokio_rusqlite::Connection;

use crate::schema;
use crate::subscription::SubscriptionList;

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

        tokio::fs::create_dir_all(&content_dir)
            .await
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
```

You will also need a minimal `SubscriptionList::new()` placeholder for this to compile. Add to `subscription.rs`:

```rust
//! SubscriptionList — see Task 8 for the real implementation.
pub struct SubscriptionList;
impl SubscriptionList {
    pub fn new() -> Self { Self }
}
impl Default for SubscriptionList { fn default() -> Self { Self::new() } }
```

- [ ] **Step 3: Write a unit test in `store.rs`**

Append to `store.rs`:

```rust
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
```

(Add `tempfile = { workspace = true }` to `[dev-dependencies]` in this crate's `Cargo.toml`.)

- [ ] **Step 4: Run the test**

Run: `nix develop --command cargo test -p sunset-store-fs new_creates_directory_and_database`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-store-fs/
git commit -m "Add FsStore::new with SQLite schema and content directory init"
```

---

### Task 3: Filesystem blob layer (`blobs.rs`)

**Files:**
- Modify: `crates/sunset-store-fs/src/blobs.rs`

The blob layer is content-agnostic file I/O: store/load `ContentBlock`s by hash on a sharded directory tree. Independent of SQLite — easy to test in isolation.

- [ ] **Step 1: Write the failing tests in `blobs.rs`**

```rust
//! Filesystem blob layer.

use std::path::{Path, PathBuf};

use sunset_store::{ContentBlock, Error, Hash, Result};
use tempfile::NamedTempFile;

/// Path on disk for a given content hash, under the store root's `content/` dir.
pub fn blob_path(root: &Path, hash: &Hash) -> PathBuf {
    let hex = hash.to_hex();
    root.join("content").join(&hex[0..2]).join(&hex[2..])
}

/// Write a content block to its sharded path, atomically. Idempotent: if the
/// file already exists with the same content (which it will, since the path
/// is content-addressed), the rename is a no-op-or-overwrite. Returns `true`
/// if the blob was newly created on disk, `false` if it already existed.
pub async fn write_blob_atomic(root: &Path, block: &ContentBlock) -> Result<bool> {
    let target = blob_path(root, &block.hash());
    if tokio::fs::try_exists(&target)
        .await
        .map_err(|e| Error::Backend(format!("blob exists check: {e}")))?
    {
        return Ok(false);
    }
    let parent = target.parent().expect("blob_path always has parent");
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| Error::Backend(format!("create blob shard dir: {e}")))?;

    let bytes = postcard::to_stdvec(block)
        .map_err(|e| Error::Backend(format!("encode block: {e}")))?;
    let parent = parent.to_path_buf();
    let target_clone = target.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut tmp = NamedTempFile::new_in(&parent)
            .map_err(|e| Error::Backend(format!("create temp blob: {e}")))?;
        std::io::Write::write_all(&mut tmp, &bytes)
            .map_err(|e| Error::Backend(format!("write blob: {e}")))?;
        tmp.persist(&target_clone)
            .map_err(|e| Error::Backend(format!("persist blob: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| Error::Backend(format!("blob join: {e}")))??;
    Ok(true)
}

/// Read a content block by hash. Returns `Ok(None)` if absent.
pub async fn read_blob(root: &Path, hash: &Hash) -> Result<Option<ContentBlock>> {
    let path = blob_path(root, hash);
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Backend(format!("read blob: {e}"))),
    };
    let block: ContentBlock = postcard::from_bytes(&bytes)
        .map_err(|e| Error::Corrupt(format!("decode blob: {e}")))?;
    if block.hash() != *hash {
        return Err(Error::Corrupt(format!(
            "blob hash mismatch on disk: expected {hash}, got {}",
            block.hash()
        )));
    }
    Ok(Some(block))
}

/// Yield the hash of every blob currently on disk under `root/content/`.
/// Used only by gc.
pub async fn list_blob_hashes(root: &Path) -> Result<Vec<Hash>> {
    let content_dir = root.join("content");
    let mut hashes = Vec::new();
    let mut shard_iter = match tokio::fs::read_dir(&content_dir).await {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(hashes),
        Err(e) => return Err(Error::Backend(format!("read content dir: {e}"))),
    };
    while let Some(shard) = shard_iter
        .next_entry()
        .await
        .map_err(|e| Error::Backend(format!("iter content dir: {e}")))?
    {
        if !shard.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let mut blob_iter = tokio::fs::read_dir(shard.path())
            .await
            .map_err(|e| Error::Backend(format!("read shard dir: {e}")))?;
        while let Some(blob) = blob_iter
            .next_entry()
            .await
            .map_err(|e| Error::Backend(format!("iter shard dir: {e}")))?
        {
            let shard_name = shard.file_name();
            let blob_name = blob.file_name();
            let shard_str = shard_name.to_string_lossy();
            let blob_str = blob_name.to_string_lossy();
            let mut hex = String::with_capacity(64);
            hex.push_str(&shard_str);
            hex.push_str(&blob_str);
            if hex.len() != 64 {
                continue; // not a valid blob filename; skip
            }
            let mut bytes = [0u8; 32];
            if hex::decode_to_slice(&hex, &mut bytes).is_err() {
                continue;
            }
            hashes.push(Hash::from(bytes));
        }
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn block(data: &[u8]) -> ContentBlock {
        ContentBlock { data: bytes::Bytes::copy_from_slice(data), references: vec![] }
    }

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        tokio::fs::create_dir_all(dir.path().join("content")).await.unwrap();
        let b = block(b"hello");
        let h = b.hash();
        assert!(write_blob_atomic(dir.path(), &b).await.unwrap());
        let got = read_blob(dir.path(), &h).await.unwrap().unwrap();
        assert_eq!(got, b);
    }

    #[tokio::test]
    async fn write_is_idempotent_on_same_hash() {
        let dir = TempDir::new().unwrap();
        tokio::fs::create_dir_all(dir.path().join("content")).await.unwrap();
        let b = block(b"hello");
        assert!(write_blob_atomic(dir.path(), &b).await.unwrap());
        assert!(!write_blob_atomic(dir.path(), &b).await.unwrap()); // already present
    }

    #[tokio::test]
    async fn read_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        let missing = Hash::from([0u8; 32]);
        assert!(read_blob(dir.path(), &missing).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_returns_all_blobs() {
        let dir = TempDir::new().unwrap();
        tokio::fs::create_dir_all(dir.path().join("content")).await.unwrap();
        let b1 = block(b"one");
        let b2 = block(b"two");
        write_blob_atomic(dir.path(), &b1).await.unwrap();
        write_blob_atomic(dir.path(), &b2).await.unwrap();
        let mut listed = list_blob_hashes(dir.path()).await.unwrap();
        listed.sort_by_key(|h| *h.as_bytes());
        let mut expected = vec![b1.hash(), b2.hash()];
        expected.sort_by_key(|h| *h.as_bytes());
        assert_eq!(listed, expected);
    }
}
```

(Add `hex = "0.4"` to workspace deps and to this crate's `[dependencies]`. `bytes` is already a workspace dep.)

- [ ] **Step 2: Run the tests — they should fail to compile until the code above is in place**

Step 1 already wrote the implementation alongside the tests, so this step is just verification.

Run: `nix develop --command cargo test -p sunset-store-fs --lib blobs::`
Expected: 4 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/sunset-store-fs/src/blobs.rs Cargo.toml crates/sunset-store-fs/Cargo.toml
git commit -m "Add filesystem blob layer with sharded layout and atomic writes"
```

---

### Task 4: SQLite KV layer (`kv.rs`) — insert + get_entry

**Files:**
- Modify: `crates/sunset-store-fs/src/kv.rs`

This task implements the SQLite half of the data plane: row encoding and the LWW-aware insert. `iter` and `delete_expired` are added in later tasks.

- [ ] **Step 1: Write `kv.rs`**

```rust
//! SQLite KV index layer.

use bytes::Bytes;
use sunset_store::{Cursor, Error, Result, SignedKvEntry, VerifyingKey};
use tokio_rusqlite::rusqlite::{self, OptionalExtension, Row, params};

/// Row → SignedKvEntry. The `sequence` column is also returned so callers can
/// use it for cursors / events.
pub fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<(u64, SignedKvEntry)> {
    let sequence: i64 = row.get("sequence")?;
    let verifying_key: Vec<u8> = row.get("verifying_key")?;
    let name: Vec<u8> = row.get("name")?;
    let value_hash: Vec<u8> = row.get("value_hash")?;
    let priority: i64 = row.get("priority")?;
    let expires_at: Option<i64> = row.get("expires_at")?;
    let signature: Vec<u8> = row.get("signature")?;
    let mut hash_bytes = [0u8; 32];
    if value_hash.len() != 32 {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            value_hash.len(), rusqlite::types::Type::Blob,
            Box::<dyn std::error::Error + Send + Sync>::from("value_hash not 32 bytes"),
        ));
    }
    hash_bytes.copy_from_slice(&value_hash);
    let entry = SignedKvEntry {
        verifying_key: VerifyingKey(Bytes::from(verifying_key)),
        name: Bytes::from(name),
        value_hash: sunset_store::Hash::from(hash_bytes),
        priority: priority as u64,
        expires_at: expires_at.map(|x| x as u64),
        signature: Bytes::from(signature),
    };
    Ok((sequence as u64, entry))
}

/// Get the entry for `(vk, name)` if present.
pub fn get_entry(
    conn: &rusqlite::Connection,
    vk: &VerifyingKey,
    name: &[u8],
) -> rusqlite::Result<Option<SignedKvEntry>> {
    conn.query_row(
        "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature
         FROM entries WHERE verifying_key = ?1 AND name = ?2",
        params![vk.as_bytes(), name],
        |row| row_to_entry(row).map(|(_, e)| e),
    )
    .optional()
}

/// Outcome of an attempted insert. The caller uses it to decide which event
/// variant to broadcast.
#[derive(Debug)]
pub enum InsertOutcome {
    Inserted { sequence: u64 },
    Replaced { old: SignedKvEntry, sequence: u64 },
}

/// Apply LWW + insert under an open transaction. Caller is responsible for
/// running this inside `conn.call(|c| { let txn = c.transaction()?; ... txn.commit()?; })`.
pub fn insert_lww(
    txn: &rusqlite::Transaction<'_>,
    entry: &SignedKvEntry,
) -> Result<InsertOutcome> {
    let existing = txn
        .query_row(
            "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature
             FROM entries WHERE verifying_key = ?1 AND name = ?2",
            params![entry.verifying_key.as_bytes(), entry.name.as_ref()],
            |row| row_to_entry(row),
        )
        .optional()
        .map_err(|e| Error::Backend(format!("select existing: {e}")))?;

    if let Some((_, ref old)) = existing {
        if old.priority >= entry.priority {
            return Err(Error::Stale);
        }
    }

    txn.execute(
        "INSERT OR REPLACE INTO entries
            (verifying_key, name, value_hash, priority, expires_at, signature)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            entry.verifying_key.as_bytes(),
            entry.name.as_ref(),
            entry.value_hash.as_bytes(),
            entry.priority as i64,
            entry.expires_at.map(|x| x as i64),
            entry.signature.as_ref(),
        ],
    )
    .map_err(|e| Error::Backend(format!("insert entry: {e}")))?;

    let sequence = txn.last_insert_rowid() as u64;

    Ok(match existing {
        Some((_, old)) => InsertOutcome::Replaced { old, sequence },
        None => InsertOutcome::Inserted { sequence },
    })
}

/// Cursor query: next-to-be-assigned sequence.
pub fn current_cursor(conn: &rusqlite::Connection) -> rusqlite::Result<Cursor> {
    let last: Option<i64> = conn
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'entries'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(Cursor(last.unwrap_or(0) as u64 + 1))
}
```

- [ ] **Step 2: Wire `insert` and `get_entry` into the `Store` impl**

Append to `store.rs`:

```rust
use async_trait::async_trait;

use sunset_store::{
    ContentBlock, Cursor, Event, Filter, Hash, Replay,
    SignedKvEntry, Store, VerifyingKey,
    EntryStream, EventStream,
};

use crate::{blobs, kv};

#[async_trait(?Send)]
impl Store for FsStore {
    async fn insert(
        &self,
        entry: SignedKvEntry,
        blob: Option<ContentBlock>,
    ) -> Result<()> {
        let _w = self.writer_mutex.lock().await;

        if let Some(b) = &blob {
            if entry.value_hash != b.hash() {
                return Err(Error::HashMismatch);
            }
        }
        self.verifier.verify(&entry).map_err(|_| Error::SignatureInvalid)?;

        // Persist the blob first (idempotent, content-addressed). Lazy refs are
        // allowed by spec, so a subsequent SQLite failure leaves at most an
        // orphaned blob, which gc_blobs reclaims later.
        let blob_was_new = if let Some(b) = &blob {
            blobs::write_blob_atomic(&self.root, b).await?
        } else {
            false
        };

        let entry_clone = entry.clone();
        let outcome: kv::InsertOutcome = self
            .conn
            .call(move |c| {
                let txn = c.transaction()?;
                let outcome = kv::insert_lww(&txn, &entry_clone)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                txn.commit()?;
                Ok(outcome)
            })
            .await
            .map_err(unwrap_other)?;

        // Broadcasts are sent under the writer_mutex by virtue of `_w` above —
        // do not drop the guard before this block.
        match outcome {
            kv::InsertOutcome::Inserted { .. } => {
                self.subscriptions.broadcast(&Event::Inserted(entry));
            }
            kv::InsertOutcome::Replaced { old, .. } => {
                self.subscriptions.broadcast(&Event::Replaced { old, new: entry });
            }
        }
        if blob_was_new {
            if let Some(b) = blob {
                self.subscriptions.broadcast(&Event::BlobAdded(b.hash()));
            }
        }

        Ok(())
    }

    async fn put_content(&self, block: ContentBlock) -> Result<Hash> {
        let _w = self.writer_mutex.lock().await;
        let hash = block.hash();
        if blobs::write_blob_atomic(&self.root, &block).await? {
            self.subscriptions.broadcast(&Event::BlobAdded(hash));
        }
        Ok(hash)
    }

    async fn get_content(&self, hash: &Hash) -> Result<Option<ContentBlock>> {
        blobs::read_blob(&self.root, hash).await
    }

    async fn get_entry(&self, vk: &VerifyingKey, name: &[u8]) -> Result<Option<SignedKvEntry>> {
        let vk = vk.clone();
        let name = name.to_vec();
        self.conn
            .call(move |c| {
                kv::get_entry(c, &vk, &name).map_err(tokio_rusqlite::Error::from)
            })
            .await
            .map_err(|e| Error::Backend(format!("get_entry: {e}")))
    }

    async fn iter<'a>(&'a self, _filter: Filter) -> Result<EntryStream<'a>> {
        // implemented in Task 5
        unimplemented!("iter — implemented in Task 5")
    }

    async fn subscribe<'a>(&'a self, _filter: Filter, _replay: Replay) -> Result<EventStream<'a>> {
        // implemented in Task 8
        unimplemented!("subscribe — implemented in Task 8")
    }

    async fn delete_expired(&self, _now: u64) -> Result<usize> {
        // implemented in Task 6
        unimplemented!("delete_expired — implemented in Task 6")
    }

    async fn gc_blobs(&self) -> Result<usize> {
        // implemented in Task 7
        unimplemented!("gc_blobs — implemented in Task 7")
    }

    async fn current_cursor(&self) -> Result<Cursor> {
        self.conn
            .call(|c| kv::current_cursor(c).map_err(tokio_rusqlite::Error::from))
            .await
            .map_err(|e| Error::Backend(format!("current_cursor: {e}")))
    }
}

/// Convert `tokio_rusqlite::Error::Other(boxed_my_error)` back to our `Error`.
fn unwrap_other(e: tokio_rusqlite::Error) -> Error {
    match e {
        tokio_rusqlite::Error::Other(b) => match b.downcast::<Error>() {
            Ok(boxed) => *boxed,
            Err(b) => Error::Backend(format!("backend: {b}")),
        },
        other => Error::Backend(format!("sqlite: {other}")),
    }
}
```

You will also need a no-op `SubscriptionList::broadcast` until Task 8. Update `subscription.rs`:

```rust
//! SubscriptionList placeholder — Task 8 fills in real broadcast/subscribe.

use sunset_store::Event;

#[derive(Default)]
pub struct SubscriptionList;
impl SubscriptionList {
    pub fn new() -> Self { Self }
    pub fn broadcast(&self, _event: &Event) { /* Task 8 */ }
}
```

- [ ] **Step 3: Write tests for insert + get_entry**

Append to `store.rs`:

```rust
#[cfg(test)]
mod insert_tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::{ContentBlock, SignedKvEntry, VerifyingKey};
    use tempfile::TempDir;

    fn vk(b: &[u8]) -> VerifyingKey { VerifyingKey(Bytes::copy_from_slice(b)) }
    fn block(d: &[u8]) -> ContentBlock {
        ContentBlock { data: Bytes::copy_from_slice(d), references: vec![] }
    }
    fn entry(vk_bytes: &[u8], name: &[u8], priority: u64, blob: &ContentBlock) -> SignedKvEntry {
        SignedKvEntry {
            verifying_key: vk(vk_bytes),
            name: Bytes::copy_from_slice(name),
            value_hash: blob.hash(),
            priority,
            expires_at: None,
            signature: Bytes::copy_from_slice(b"sig"),
        }
    }

    #[tokio::test]
    async fn insert_then_get_entry() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b = block(b"v");
        let e = entry(b"a", b"k", 1, &b);
        store.insert(e.clone(), Some(b)).await.unwrap();
        let got = store.get_entry(&vk(b"a"), b"k").await.unwrap().unwrap();
        assert_eq!(got, e);
    }

    #[tokio::test]
    async fn insert_lww_higher_priority_wins() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b1 = block(b"v1");
        let b2 = block(b"v2");
        store.insert(entry(b"a", b"k", 1, &b1), Some(b1)).await.unwrap();
        store.insert(entry(b"a", b"k", 2, &b2.clone()), Some(b2.clone())).await.unwrap();
        let got = store.get_entry(&vk(b"a"), b"k").await.unwrap().unwrap();
        assert_eq!(got.priority, 2);
    }

    #[tokio::test]
    async fn insert_lww_equal_priority_is_stale() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b = block(b"v");
        store.insert(entry(b"a", b"k", 1, &b), Some(b.clone())).await.unwrap();
        let err = store.insert(entry(b"a", b"k", 1, &b), Some(b)).await.unwrap_err();
        assert!(matches!(err, Error::Stale));
    }

    #[tokio::test]
    async fn insert_rejects_hash_mismatch() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        let b1 = block(b"v1");
        let b2 = block(b"v2");
        let mut e = entry(b"a", b"k", 1, &b1);
        e.value_hash = b1.hash();
        // supply b2 (whose hash differs) — must be rejected.
        let err = store.insert(e, Some(b2)).await.unwrap_err();
        assert!(matches!(err, Error::HashMismatch));
    }

    #[tokio::test]
    async fn current_cursor_advances_with_inserts() {
        let dir = TempDir::new().unwrap();
        let store = FsStore::new(dir.path()).await.unwrap();
        assert_eq!(store.current_cursor().await.unwrap(), Cursor(1));
        let b = block(b"v");
        store.insert(entry(b"a", b"k", 1, &b), Some(b)).await.unwrap();
        assert_eq!(store.current_cursor().await.unwrap(), Cursor(2));
    }

    #[tokio::test]
    async fn entries_persist_across_reopen() {
        let dir = TempDir::new().unwrap();
        let b = block(b"v");
        let e = entry(b"a", b"k", 1, &b);
        {
            let store = FsStore::new(dir.path()).await.unwrap();
            store.insert(e.clone(), Some(b)).await.unwrap();
        }
        let store2 = FsStore::new(dir.path()).await.unwrap();
        let got = store2.get_entry(&vk(b"a"), b"k").await.unwrap().unwrap();
        assert_eq!(got, e);
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `nix develop --command cargo test -p sunset-store-fs --lib insert_tests::`
Expected: 6 tests PASS. The reopen test is the killer one — proves persistence works.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-store-fs/
git commit -m "Implement FsStore insert + get_entry + current_cursor with SQLite LWW"
```

---

### Task 5: `iter` for all Filter variants

**Files:**
- Modify: `crates/sunset-store-fs/src/kv.rs`
- Modify: `crates/sunset-store-fs/src/store.rs`

- [ ] **Step 1: Add `iter_with_filter` to `kv.rs`**

```rust
use sunset_store::Filter;

/// Stream-shaped iterator over entries matching `filter`. Returns owned
/// rows so the caller can drop the connection callback. Order is unspecified
/// per Filter (the spec doesn't pin one); within a single Filter we order by
/// `sequence ASC` for determinism in tests.
pub fn iter_with_filter(
    conn: &rusqlite::Connection,
    filter: &Filter,
) -> Result<Vec<SignedKvEntry>> {
    let mut out = Vec::new();
    match filter {
        Filter::Specific { verifying_key, name } => {
            if let Some(e) = get_entry(conn, verifying_key, name)
                .map_err(|e| Error::Backend(format!("specific: {e}")))?
            {
                out.push(e);
            }
        }
        Filter::Keyspace(vk) => {
            let mut stmt = conn
                .prepare(
                    "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature
                     FROM entries WHERE verifying_key = ?1 ORDER BY sequence ASC",
                )
                .map_err(|e| Error::Backend(format!("prep: {e}")))?;
            let rows = stmt.query_map(params![vk.as_bytes()], |r| row_to_entry(r).map(|(_, e)| e))
                .map_err(|e| Error::Backend(format!("query: {e}")))?;
            for r in rows {
                out.push(r.map_err(|e| Error::Backend(format!("row: {e}")))?);
            }
        }
        Filter::Namespace(name) => {
            let mut stmt = conn
                .prepare(
                    "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature
                     FROM entries WHERE name = ?1 ORDER BY sequence ASC",
                )
                .map_err(|e| Error::Backend(format!("prep: {e}")))?;
            let rows = stmt.query_map(params![name.as_ref()], |r| row_to_entry(r).map(|(_, e)| e))
                .map_err(|e| Error::Backend(format!("query: {e}")))?;
            for r in rows {
                out.push(r.map_err(|e| Error::Backend(format!("row: {e}")))?);
            }
        }
        Filter::NamePrefix(prefix) => {
            // Range scan: name >= prefix AND substr(name, 1, len(prefix)) = prefix.
            let mut stmt = conn
                .prepare(
                    "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature
                     FROM entries
                     WHERE substr(name, 1, ?2) = ?1
                     ORDER BY sequence ASC",
                )
                .map_err(|e| Error::Backend(format!("prep: {e}")))?;
            let rows = stmt.query_map(
                params![prefix.as_ref(), prefix.len() as i64],
                |r| row_to_entry(r).map(|(_, e)| e),
            )
            .map_err(|e| Error::Backend(format!("query: {e}")))?;
            for r in rows {
                out.push(r.map_err(|e| Error::Backend(format!("row: {e}")))?);
            }
        }
        Filter::Union(filters) => {
            // v1: collect each subfilter's results and dedupe by (vk, name).
            let mut seen = std::collections::HashSet::<(Vec<u8>, Vec<u8>)>::new();
            for f in filters {
                for e in iter_with_filter(conn, f)? {
                    let key = (e.verifying_key.as_bytes().to_vec(), e.name.to_vec());
                    if seen.insert(key) {
                        out.push(e);
                    }
                }
            }
        }
    }
    Ok(out)
}
```

(Note: this returns `Vec<SignedKvEntry>` rather than streaming row-by-row because rusqlite statements borrow the connection — streaming through `tokio_rusqlite::Connection::call` is awkward. Materializing once per call is acceptable for v1; if it ever becomes a bottleneck the fix is to use `rusqlite::Statement::query_and_then` with batched fetches, deferred to a follow-up.)

- [ ] **Step 2: Replace the `unimplemented!` in `store.rs`'s `iter` with the real impl**

```rust
async fn iter<'a>(&'a self, filter: Filter) -> Result<EntryStream<'a>> {
    let entries = self
        .conn
        .call(move |c| kv::iter_with_filter(c, &filter)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e))))
        .await
        .map_err(unwrap_other)?;
    let stream = async_stream::stream! {
        for e in entries {
            yield Ok(e);
        }
    };
    Ok(Box::pin(stream))
}
```

- [ ] **Step 3: Add a focused unit test**

Append to `store.rs`'s test module:

```rust
#[tokio::test]
async fn iter_keyspace_returns_only_matching_writer() {
    use futures::StreamExt;
    let dir = TempDir::new().unwrap();
    let store = FsStore::new(dir.path()).await.unwrap();
    let b = block(b"v");
    store.insert(entry(b"a", b"k1", 1, &b), Some(b.clone())).await.unwrap();
    store.insert(entry(b"a", b"k2", 1, &b), Some(b.clone())).await.unwrap();
    store.insert(entry(b"b", b"k1", 1, &b), Some(b)).await.unwrap();
    let got: Vec<_> = store
        .iter(Filter::Keyspace(vk(b"a"))).await.unwrap()
        .collect::<Vec<_>>().await
        .into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|e| e.verifying_key == vk(b"a")));
}
```

- [ ] **Step 4: Run the test**

Run: `nix develop --command cargo test -p sunset-store-fs --lib iter_keyspace_returns_only_matching_writer`
Expected: PASS. (The conformance suite in Task 9 will exercise the other Filter variants comprehensively.)

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-store-fs/
git commit -m "Implement FsStore iter for all Filter variants"
```

---

### Task 6: `delete_expired`

**Files:**
- Modify: `crates/sunset-store-fs/src/kv.rs`
- Modify: `crates/sunset-store-fs/src/store.rs`

- [ ] **Step 1: Add `delete_expired` to `kv.rs`**

```rust
/// Delete all entries with `expires_at <= now`. Returns the deleted entries
/// (so the caller can broadcast `Event::Expired` for each).
pub fn delete_expired(
    txn: &rusqlite::Transaction<'_>,
    now: u64,
) -> Result<Vec<SignedKvEntry>> {
    let mut victims = Vec::new();
    {
        let mut stmt = txn.prepare(
            "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature
             FROM entries WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        ).map_err(|e| Error::Backend(format!("prep: {e}")))?;
        let rows = stmt.query_map(params![now as i64], |r| row_to_entry(r).map(|(_, e)| e))
            .map_err(|e| Error::Backend(format!("query: {e}")))?;
        for r in rows {
            victims.push(r.map_err(|e| Error::Backend(format!("row: {e}")))?);
        }
    }
    txn.execute(
        "DELETE FROM entries WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        params![now as i64],
    ).map_err(|e| Error::Backend(format!("delete: {e}")))?;
    Ok(victims)
}
```

- [ ] **Step 2: Replace the `unimplemented!` in `store.rs`'s `delete_expired`**

```rust
async fn delete_expired(&self, now: u64) -> Result<usize> {
    let _w = self.writer_mutex.lock().await;
    let victims: Vec<SignedKvEntry> = self
        .conn
        .call(move |c| {
            let txn = c.transaction()?;
            let v = kv::delete_expired(&txn, now)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            txn.commit()?;
            Ok(v)
        })
        .await
        .map_err(unwrap_other)?;
    let count = victims.len();
    for e in victims {
        self.subscriptions.broadcast(&Event::Expired(e));
    }
    Ok(count)
}
```

- [ ] **Step 3: Test**

Append:

```rust
#[tokio::test]
async fn delete_expired_removes_at_boundary() {
    let dir = TempDir::new().unwrap();
    let store = FsStore::new(dir.path()).await.unwrap();
    let b = block(b"v");
    let mut e = entry(b"a", b"k", 1, &b);
    e.expires_at = Some(100);
    store.insert(e, Some(b)).await.unwrap();
    let n = store.delete_expired(100).await.unwrap();
    assert_eq!(n, 1);
    assert!(store.get_entry(&vk(b"a"), b"k").await.unwrap().is_none());
}
```

- [ ] **Step 4: Run**

Run: `nix develop --command cargo test -p sunset-store-fs --lib delete_expired_removes_at_boundary`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-store-fs/
git commit -m "Implement FsStore delete_expired with boundary-inclusive TTL"
```

---

### Task 7: `gc_blobs` (mark-and-sweep)

**Files:**
- Modify: `crates/sunset-store-fs/src/gc.rs`
- Modify: `crates/sunset-store-fs/src/store.rs`

- [ ] **Step 1: Implement `mark_and_sweep` in `gc.rs`**

```rust
//! Mark-and-sweep blob GC.
//!
//! Marks every blob reachable from the current set of `entries.value_hash`
//! values (transitively, by walking `ContentBlock.references`), then deletes
//! every on-disk blob not in the marked set.

use std::collections::HashSet;
use std::path::Path;

use sunset_store::{Error, Hash, Result};
use tokio_rusqlite::rusqlite::{self, params};

use crate::blobs;

/// Returns all `value_hash`es currently in the entries table — the GC roots.
pub fn read_roots(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<Hash>> {
    let mut stmt = conn.prepare("SELECT value_hash FROM entries")?;
    let rows = stmt.query_map(params![], |row| {
        let bytes: Vec<u8> = row.get(0)?;
        let mut buf = [0u8; 32];
        if bytes.len() != 32 {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                bytes.len(), rusqlite::types::Type::Blob,
                Box::<dyn std::error::Error + Send + Sync>::from("value_hash != 32 bytes"),
            ));
        }
        buf.copy_from_slice(&bytes);
        Ok(Hash::from(buf))
    })?;
    rows.collect()
}

/// Iteratively walk content references from `roots` (DFS via explicit stack).
pub async fn reachable_set(root: &Path, roots: Vec<Hash>) -> Result<HashSet<Hash>> {
    let mut visited: HashSet<Hash> = HashSet::new();
    let mut stack = roots;
    while let Some(h) = stack.pop() {
        if !visited.insert(h) {
            continue;
        }
        if let Some(block) = blobs::read_blob(root, &h).await? {
            for r in block.references {
                if !visited.contains(&r) {
                    stack.push(r);
                }
            }
        }
        // If absent: lazy dangling ref; skip silently.
    }
    Ok(visited)
}

/// Returns the hashes of blobs that were deleted (so the caller can emit
/// `BlobRemoved` events for each).
pub async fn mark_and_sweep(
    root: &Path,
    roots: Vec<Hash>,
) -> Result<Vec<Hash>> {
    let reachable = reachable_set(root, roots).await?;
    let on_disk = blobs::list_blob_hashes(root).await?;
    let mut removed = Vec::new();
    for h in on_disk {
        if reachable.contains(&h) {
            continue;
        }
        let path = blobs::blob_path(root, &h);
        match tokio::fs::remove_file(&path).await {
            Ok(_) => removed.push(h),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::Backend(format!("rm blob: {e}"))),
        }
    }
    Ok(removed)
}
```

- [ ] **Step 2: Replace `unimplemented!` in `store.rs`'s `gc_blobs`**

```rust
async fn gc_blobs(&self) -> Result<usize> {
    let _w = self.writer_mutex.lock().await;
    let roots = self.conn
        .call(|c| crate::gc::read_roots(c).map_err(tokio_rusqlite::Error::from))
        .await
        .map_err(|e| Error::Backend(format!("gc roots: {e}")))?;
    let removed = crate::gc::mark_and_sweep(&self.root, roots).await?;
    let count = removed.len();
    for h in removed {
        self.subscriptions.broadcast(&Event::BlobRemoved(h));
    }
    Ok(count)
}
```

- [ ] **Step 3: Test**

Append to `store.rs`:

```rust
#[tokio::test]
async fn gc_blobs_keeps_reachable_drops_orphans() {
    let dir = TempDir::new().unwrap();
    let store = FsStore::new(dir.path()).await.unwrap();
    let b_used = block(b"used");
    let b_orphan = block(b"orphan");
    store.put_content(b_orphan.clone()).await.unwrap();
    store.insert(entry(b"a", b"k", 1, &b_used), Some(b_used.clone())).await.unwrap();
    let n = store.gc_blobs().await.unwrap();
    assert_eq!(n, 1);
    assert!(store.get_content(&b_used.hash()).await.unwrap().is_some());
    assert!(store.get_content(&b_orphan.hash()).await.unwrap().is_none());
}
```

- [ ] **Step 4: Run**

Run: `nix develop --command cargo test -p sunset-store-fs --lib gc_blobs_keeps_reachable_drops_orphans`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-store-fs/
git commit -m "Implement FsStore gc_blobs with mark-and-sweep over reachable refs"
```

---

### Task 8: Subscriptions (`subscription.rs` + `subscribe`)

**Files:**
- Modify: `crates/sunset-store-fs/src/subscription.rs`
- Modify: `crates/sunset-store-fs/src/store.rs`

This is structurally identical to `sunset-store-memory`'s subscription module, modulo an `?Send` worker pattern that doesn't apply here. Re-derive the type rather than depending on the memory crate.

- [ ] **Step 1: Write `subscription.rs`**

```rust
//! Subscription list. Mirrors the broadcast-under-writer-lock invariant from
//! sunset-store-memory.

use std::sync::{Arc, Mutex, Weak};

use sunset_store::{Event, Filter, Result};
use tokio::sync::mpsc;

/// A live subscription: a filter and the sender half of the live-event channel.
///
/// `tx` MUST stay an `UnboundedSender`. `broadcast` runs while the FsStore
/// writer mutex is held; switching to a bounded channel would let a slow
/// subscriber stall every writer.
pub struct Subscription {
    pub filter: Filter,
    pub tx: mpsc::UnboundedSender<Result<Event>>,
}

#[derive(Default)]
pub struct SubscriptionList {
    /// Holds `Weak<Subscription>` so dropped streams clean up automatically.
    /// The `Mutex` is `std::sync::Mutex` (not tokio): the critical sections are
    /// trivially short, allocation-only, with no `.await` inside. The unwraps
    /// can only panic if a panic occurred *inside* the lock — `add` and
    /// `broadcast` do nothing that can panic (`Vec::retain`, `Vec::push`,
    /// `Filter::matches`, `Event::clone`, `mpsc::send` are all non-panicking,
    /// barring an allocator panic which would already have aborted the
    /// process). Lock poisoning is therefore unreachable in production.
    entries: Mutex<Vec<Weak<Subscription>>>,
}

impl SubscriptionList {
    pub fn new() -> Self { Self::default() }

    pub fn add(&self, sub: &Arc<Subscription>) {
        let mut g = self.entries.lock().unwrap();
        g.retain(|w| w.upgrade().is_some());
        g.push(Arc::downgrade(sub));
    }

    pub fn broadcast(&self, event: &Event) {
        // Extract (vk, name) for filter matching when the event is keyed.
        // BlobAdded / BlobRemoved have no key and are delivered to all subscribers.
        let (vk, name) = match event {
            Event::Inserted(e) | Event::Expired(e) => (Some(&e.verifying_key), Some(&e.name)),
            Event::Replaced { new, .. } => (Some(&new.verifying_key), Some(&new.name)),
            Event::BlobAdded(_) | Event::BlobRemoved(_) => (None, None),
        };
        let mut g = self.entries.lock().unwrap();
        g.retain(|w| {
            let Some(s) = w.upgrade() else { return false };
            let interested = match (vk, name) {
                (Some(v), Some(n)) => s.filter.matches(v, n.as_ref()),
                _ => true,
            };
            if interested {
                let _ = s.tx.send(Ok(event.clone()));
            }
            true
        });
    }
}
```

(This mirrors `crates/sunset-store-memory/src/subscription.rs` exactly. Don't import that type — re-derive it here so the two backend crates stay independent.)

- [ ] **Step 2: Implement `subscribe` in `store.rs`**

```rust
async fn subscribe<'a>(&'a self, filter: Filter, replay: Replay) -> Result<EventStream<'a>> {
    use crate::subscription::Subscription;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<Event>>();
    let sub = Arc::new(Subscription { filter: filter.clone(), tx });

    // Take history snapshot AND register subscription under the writer mutex,
    // so any concurrent insert is serialized and either lands in the snapshot
    // or in the live channel — never both.
    let _w = self.writer_mutex.lock().await;

    let history: Vec<SignedKvEntry> = match &replay {
        Replay::None => Vec::new(),
        Replay::All => {
            self.conn
                .call({
                    let f = filter.clone();
                    move |c| kv::iter_with_filter(c, &f)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
                })
                .await
                .map_err(unwrap_other)?
        }
        Replay::Since(cursor) => {
            // sequence >= cursor.0
            let cursor = *cursor;
            let f = filter.clone();
            self.conn
                .call(move |c| -> std::result::Result<Vec<SignedKvEntry>, tokio_rusqlite::Error> {
                    let mut stmt = c.prepare(
                        "SELECT sequence, verifying_key, name, value_hash, priority, expires_at, signature
                         FROM entries WHERE sequence >= ?1 ORDER BY sequence ASC",
                    )?;
                    let rows = stmt.query_map(
                        tokio_rusqlite::rusqlite::params![cursor.0 as i64],
                        |r| kv::row_to_entry(r).map(|(_, e)| e),
                    )?;
                    let mut out = Vec::new();
                    for r in rows {
                        let e = r?;
                        if f.matches(&e.verifying_key, &e.name) {
                            out.push(e);
                        }
                    }
                    Ok(out)
                })
                .await
                .map_err(|e| Error::Backend(format!("replay since: {e}")))?
        }
    };

    self.subscriptions.add(&sub);
    drop(_w);

    let stream = async_stream::stream! {
        for e in history {
            yield Ok(Event::Inserted(e));
        }
        let _keep_alive = sub;
        while let Some(item) = rx.recv().await {
            yield item;
        }
    };
    Ok(Box::pin(stream))
}
```

- [ ] **Step 3: Smoke test**

Append:

```rust
#[tokio::test]
async fn subscribe_replay_all_then_live() {
    use futures::StreamExt;
    let dir = TempDir::new().unwrap();
    let store = FsStore::new(dir.path()).await.unwrap();
    let b1 = block(b"v1");
    store.insert(entry(b"a", b"k1", 1, &b1), Some(b1)).await.unwrap();
    let mut s = store.subscribe(Filter::Keyspace(vk(b"a")), Replay::All).await.unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_millis(200), s.next())
        .await.unwrap().unwrap().unwrap();
    assert!(matches!(first, Event::Inserted(_)));

    let b2 = block(b"v2");
    store.insert(entry(b"a", b"k2", 1, &b2.clone()), Some(b2)).await.unwrap();
    // The next event the subscriber receives that is NOT BlobAdded should be Inserted for k2.
    loop {
        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), s.next())
            .await.unwrap().unwrap().unwrap();
        if matches!(evt, Event::Inserted(_)) {
            if let Event::Inserted(e) = evt {
                assert_eq!(e.name.as_ref(), b"k2");
                break;
            }
        }
    }
}
```

- [ ] **Step 4: Run**

Run: `nix develop --command cargo test -p sunset-store-fs --lib subscribe_replay_all_then_live`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/sunset-store-fs/
git commit -m "Implement FsStore subscribe with replay modes and broadcast under writer lock"
```

---

### Task 9: Run the conformance suite

**Files:**
- Create: `crates/sunset-store-fs/tests/conformance.rs`

This is the load-bearing task — the entire shared conformance suite must pass against `FsStore`.

- [ ] **Step 1: Add `tempfile` to dev-dependencies if not already there**

```toml
# crates/sunset-store-fs/Cargo.toml under [dev-dependencies]
tempfile = { workspace = true }
```

(`tempfile` is already a workspace dep from Task 1.)

- [ ] **Step 2: Write the integration test**

```rust
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
```

(If your installed tempfile is older than 3.7, use `TempDir::new().unwrap().into_path()` instead — same behavior, different name.)

- [ ] **Step 3: Run**

Run: `nix develop --command cargo test -p sunset-store-fs --test conformance --features sunset-store/test-helpers`
Expected: the conformance test passes.

If any individual conformance test fails, **stop and diagnose** before papering over. The conformance suite *is* the contract; a failure means the FsStore doesn't conform. Common failure shapes and where to look:
- Subscription event ordering off → revisit Task 8 (broadcast inside writer lock)
- LWW comparison wrong → Task 4's `insert_lww`
- TTL boundary off-by-one → Task 6
- GC reclaiming live blobs → Task 7's reachability walk
- Cursor semantics drift → Task 4's `current_cursor` (must return *next* sequence, not last)

- [ ] **Step 4: Commit**

```bash
git add crates/sunset-store-fs/tests/conformance.rs
git commit -m "Run conformance suite against FsStore (passes)"
```

---

### Task 10: Final pass — clippy, fmt, full workspace test

**Files:**
- (No new files; cleanup pass)

- [ ] **Step 1: Run clippy across the workspace**

Run: `nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: no warnings. If clippy flags issues, fix them — do not silence with `#[allow(...)]` unless the lint is genuinely wrong for the code.

- [ ] **Step 2: Run fmt**

Run: `nix develop --command cargo fmt --all --check`
Expected: no diff. If there are differences, run `cargo fmt --all` and amend.

- [ ] **Step 3: Run the full workspace test suite**

Run: `nix develop --command cargo test --workspace --all-features`
Expected: all tests across `sunset-store`, `sunset-store-memory`, and `sunset-store-fs` pass — including both backends' conformance integration tests.

- [ ] **Step 4: Commit any cleanup**

```bash
git add -A
git commit -m "Pass clippy + fmt across the workspace"
```

(If steps 1–3 produced no changes, skip this commit — `git commit` will refuse an empty commit anyway.)

---

## Verification (end-state acceptance)

After all 10 tasks land:

- `cargo test --workspace --all-features` passes.
- The new conformance integration test at `crates/sunset-store-fs/tests/conformance.rs` runs the full shared suite and passes.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings` is clean.
- `cargo fmt --all --check` is clean.
- An `FsStore` opened on the same directory after a process restart sees prior entries (covered by `entries_persist_across_reopen` in Task 4).
- `git log --oneline master..HEAD` shows ten task commits in order.

## Out of scope

- WAL / sync-mode tuning (`PRAGMA journal_mode = WAL`, `PRAGMA synchronous`). v1 uses SQLite defaults; performance tuning is a follow-up.
- Connection pooling (`r2d2`, `deadpool`). The `tokio_rusqlite` worker thread is sufficient for v1 throughput.
- Schema migrations beyond `SCHEMA_VERSION = 1`. The migration mechanism is in place (`schema_meta` table) but no v2 yet.
- Streaming row iteration in `iter` (currently materializes; cf. Task 5 note).
- Concurrent FsStore handles on the same on-disk directory. v1 assumes one process owns the directory.
- Plans 3 (`sunset-store-indexeddb`) and 4 (`sunset-sync`). Each gets its own writing-plans cycle after this plan lands.
