#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("malformed input: {0}")]
    MalformedInput(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("bad json: {0}")]
    BadJson(String),
    #[error("bad x25519: {0}")]
    BadX25519(String),
}

pub type Result<T> = std::result::Result<T, Error>;
