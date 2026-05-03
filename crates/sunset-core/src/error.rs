//! Crate-level error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ed25519 signature error: {0}")]
    Signature(#[from] ed25519_dalek::SignatureError),

    #[error("postcard codec error: {0}")]
    Postcard(#[from] postcard::Error),

    #[error("AEAD authentication failed (forged or wrong key)")]
    AeadAuthFailed,

    #[error("argon2 key derivation failed: {0}")]
    Argon2(String),

    #[error("entry name did not match `<hex_fingerprint>/msg/<hex_value_hash>`: {0}")]
    BadName(String),

    #[error("content block hash did not match entry.value_hash")]
    BadValueHash,

    #[error("decoded message's room_fingerprint did not match the room used to decrypt")]
    RoomMismatch,

    #[error("decoded message's epoch_id did not match the epoch used to decrypt")]
    EpochMismatch,

    #[error("inner-signature payload too long for postcard encoding")]
    PayloadTooLarge,

    #[error("emoji exceeds 64-byte limit: {len} bytes")]
    EmojiTooLong { len: usize },

    #[error("store: {0}")]
    Store(String),

    #[error("sync: {0}")]
    Sync(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
