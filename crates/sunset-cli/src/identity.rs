//! Load-or-generate the user's 32-byte ed25519 secret seed.

use std::path::Path;

use rand_core::{OsRng, RngCore};
use sunset_core::Identity;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("identity file at {path:?} is {got} bytes, expected 32")]
    WrongSize {
        path: std::path::PathBuf,
        got: usize,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Read a 32-byte seed from `path`, or generate one and persist it
/// (mode 0600 on Unix). Returns the resulting `Identity`.
pub async fn load_or_generate(path: &Path) -> Result<Identity> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            if bytes.len() != 32 {
                return Err(Error::WrongSize {
                    path: path.to_path_buf(),
                    got: bytes.len(),
                });
            }
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            Ok(Identity::from_secret_bytes(&seed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut seed = [0u8; 32];
            OsRng.fill_bytes(&mut seed);
            write_secret(path, &seed).await?;
            Ok(Identity::from_secret_bytes(&seed))
        }
        Err(e) => Err(Error::Io(e)),
    }
}

#[cfg(unix)]
async fn write_secret(path: &Path, seed: &[u8; 32]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::write(path, seed).await?;
    let perms = std::fs::Permissions::from_mode(0o600);
    tokio::fs::set_permissions(path, perms).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn write_secret(path: &Path, seed: &[u8; 32]) -> Result<()> {
    tokio::fs::write(path, seed).await?;
    Ok(())
}

/// Default identity path: `$SUNSET_IDENTITY_PATH` if set, else
/// `<config_dir>/sunset/identity.bin`. `config_dir()` returns
/// `~/.config` on Linux, `~/Library/Application Support` on macOS.
pub fn default_path() -> std::path::PathBuf {
    if let Ok(v) = std::env::var("SUNSET_IDENTITY_PATH") {
        return std::path::PathBuf::from(v);
    }
    let base = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("sunset").join("identity.bin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn generates_then_persists_then_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.bin");

        let id1 = load_or_generate(&path).await.expect("first call generates");
        let id2 = load_or_generate(&path).await.expect("second call reads");

        assert_eq!(id1.public().as_bytes(), id2.public().as_bytes());
        let raw = tokio::fs::read(&path).await.unwrap();
        assert_eq!(raw.len(), 32);
    }

    #[tokio::test]
    async fn refuses_wrong_size_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.bin");
        tokio::fs::write(&path, b"too-short").await.unwrap();
        let err = load_or_generate(&path).await.unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("32"), "{s}");
    }
}
