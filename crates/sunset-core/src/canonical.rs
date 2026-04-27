//! Canonical signing payload — moved to `sunset_store::canonical` so
//! `sunset-sync` can use it without depending on `sunset-core`.
//! This module is a back-compat re-export.

pub use sunset_store::canonical::signing_payload;
