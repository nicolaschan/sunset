use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("toml: {0}")]
    Toml(String),

    #[error("store: {0}")]
    Store(#[from] sunset_store::Error),

    #[error("sync: {0}")]
    Sync(#[from] sunset_sync::Error),

    #[error("noise: {0}")]
    Noise(#[from] sunset_noise::Error),

    #[error("identity: {0}")]
    Identity(String),
}

pub type Result<T> = std::result::Result<T, Error>;
