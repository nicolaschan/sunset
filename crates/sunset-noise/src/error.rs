use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("snow: {0}")]
    Snow(String),

    #[error("address parse error: {0}")]
    Addr(String),

    #[error("missing or malformed x25519 fragment in PeerAddr: {0}")]
    MissingStaticPubkey(String),

    #[error("raw transport error: {0}")]
    RawTransport(#[from] sunset_sync::Error),
}

impl From<snow::Error> for Error {
    fn from(e: snow::Error) -> Self {
        Error::Snow(format!("{:?}", e))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
