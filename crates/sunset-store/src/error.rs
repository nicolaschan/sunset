//! Error type for sunset-store operations.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// Wrapped backend-specific failure (rusqlite, IndexedDB DOM exception, etc.).
    #[error("backend error: {0}")]
    Backend(String),

    /// `SignatureVerifier::verify` rejected the entry.
    #[error("signature verification failed")]
    SignatureInvalid,

    /// Write rejected because an existing entry has equal or higher priority.
    #[error("entry is stale (existing priority >= new)")]
    Stale,

    /// `entry.value_hash` did not match the hash of the supplied `ContentBlock`.
    #[error("entry value_hash does not match supplied blob hash")]
    HashMismatch,

    /// Read returned no result.
    #[error("not found")]
    NotFound,

    /// Internal invariant violation (entry signature unexpectedly fails on read,
    /// malformed ContentBlock, etc.). Indicates data integrity issue.
    #[error("data corruption: {0}")]
    Corrupt(String),

    /// Operation on a closed store handle.
    #[error("store handle is closed")]
    Closed,
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_display() {
        assert!(format!("{}", Error::SignatureInvalid).contains("signature"));
        assert!(format!("{}", Error::Stale).contains("stale"));
        assert!(format!("{}", Error::Backend("oops".into())).contains("oops"));
    }
}
