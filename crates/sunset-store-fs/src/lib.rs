//! On-disk implementation of `sunset-store::Store` using SQLite for the KV
//! index and the filesystem for content blobs.

pub(crate) mod blobs;
mod gc;
mod kv;
mod schema;
mod store;
mod subscription;

pub use store::FsStore;
