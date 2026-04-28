//! Load-or-generate the relay's Ed25519 identity, persisted as a 32-byte
//! secret seed in a file with mode 0600.
//!
//! On generation, prints a startup banner with both the Ed25519 and
//! derived X25519 pubkeys + a copy-pasteable address line.

use std::path::Path;

use rand_core::{OsRng, RngCore};
use zeroize::Zeroizing;

use sunset_core::Identity;
use sunset_noise::ed25519_seed_to_x25519_secret;

use crate::error::{Error, Result};

/// Load the secret seed from `path`, or generate a fresh one and persist it.
/// Returns the constructed `Identity`.
pub async fn load_or_generate(path: &Path) -> Result<Identity> {
    if path.exists() {
        load(path).await
    } else {
        generate_and_persist(path).await
    }
}

async fn load(path: &Path) -> Result<Identity> {
    let bytes = tokio::fs::read(path).await?;
    let seed: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        Error::Identity(format!(
            "expected 32 bytes at {}, got {}",
            path.display(),
            bytes.len(),
        ))
    })?;
    let mode = file_permissions(path).await;
    if let Some(mode) = mode {
        if mode & 0o077 != 0 {
            tracing::warn!(
                path = %path.display(),
                mode = format!("{:o}", mode),
                "identity key file has wider-than-0600 permissions",
            );
        }
    }
    Ok(Identity::from_secret_bytes(&seed))
}

async fn generate_and_persist(path: &Path) -> Result<Identity> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut seed = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(&mut *seed);

    let tmp = path.with_extension("key.tmp");
    tokio::fs::write(&tmp, &*seed).await?;
    set_mode_0600(&tmp).await?;
    tokio::fs::rename(&tmp, path).await?;

    Ok(Identity::from_secret_bytes(&seed))
}

#[cfg(unix)]
async fn set_mode_0600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = tokio::fs::metadata(path).await?.permissions();
    perms.set_mode(0o600);
    tokio::fs::set_permissions(path, perms).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn set_mode_0600(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn file_permissions(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let meta = tokio::fs::metadata(path).await.ok()?;
    Some(meta.permissions().mode())
}

#[cfg(not(unix))]
async fn file_permissions(_path: &Path) -> Option<u32> {
    None
}

/// Format the relay's startup address banner (printed by main on startup).
pub fn format_address(listen_addr: &std::net::SocketAddr, identity: &Identity) -> String {
    let ed_pub = identity.public().as_bytes();
    let x_secret = ed25519_seed_to_x25519_secret(&identity.secret_bytes());
    let x_pub = {
        use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
        let scalar = Scalar::from_bytes_mod_order(*x_secret);
        MontgomeryPoint::mul_base(&scalar).to_bytes()
    };
    format!(
        "sunset-relay starting\n  ed25519: {}\n  x25519:  {}\n  listen:  ws://{}\n  address: ws://{}#x25519={}",
        hex::encode(ed_pub),
        hex::encode(x_pub),
        listen_addr,
        listen_addr,
        hex::encode(x_pub),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn load_or_generate_creates_then_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        assert!(!path.exists());

        let id1 = load_or_generate(&path).await.unwrap();
        assert!(path.exists());

        let id2 = load_or_generate(&path).await.unwrap();
        assert_eq!(id1.public(), id2.public());
        assert_eq!(id1.secret_bytes(), id2.secret_bytes());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn generated_file_has_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        let _ = load_or_generate(&path).await.unwrap();
        let meta = tokio::fs::metadata(&path).await.unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn address_format_is_parseable() {
        let id = Identity::from_secret_bytes(&[7u8; 32]);
        let addr = "127.0.0.1:8443".parse().unwrap();
        let s = format_address(&addr, &id);
        assert!(s.contains("ed25519: "));
        assert!(s.contains("x25519:  "));
        assert!(s.contains("address: ws://127.0.0.1:8443#x25519="));
    }
}
