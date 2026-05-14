//! Wasm32 implementation of `sunset-store-indexeddb`.
//!
//! Pulled in via `cfg(target_arch = "wasm32")` from the crate root.

mod db;
mod req;
mod store;
mod subscription;

pub use db::delete_database;
pub use store::{DEFAULT_DATABASE_NAME, IndexedDbStore};
