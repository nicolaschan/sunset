//! Error type for sunset-sync.

use thiserror::Error;

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    /// Transport layer reported a failure (connect, send, recv, close).
    #[error("transport: {0}")]
    Transport(String),

    /// Underlying store returned an error during sync work.
    #[error("store: {0}")]
    Store(#[from] sunset_store::Error),

    /// Failed to decode an incoming wire message.
    #[error("decode: {0}")]
    Decode(String),

    /// Protocol invariant violated by the remote (unexpected message,
    /// version mismatch, malformed digest, etc.).
    #[error("protocol: {0}")]
    Protocol(String),

    /// Per-peer error attributable to a specific peer.
    #[error("peer: {0}")]
    Peer(String),

    /// Engine has been closed (run() returned, channels dropped).
    #[error("closed")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_error_converts_via_from() {
        let store_err = sunset_store::Error::Stale;
        let sync_err: Error = store_err.into();
        assert_eq!(sync_err, Error::Store(sunset_store::Error::Stale));
    }
}
