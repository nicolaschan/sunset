//! WebTransport TLS identity persistence.
//!
//! `wtransport::Identity::self_signed` produces a fresh ECDSA-P256
//! certificate every call (with 14-day validity). Calling it on every
//! relay startup means every restart invalidates browsers' cached
//! `serverCertificateHashes` pin and forces them down the WS fallback
//! path until they re-fetch the identity descriptor — a real UX
//! regression on hosts that auto-restart (systemd-on-OOM, container
//! orchestrators, deploys).
//!
//! This module persists the cert + key under the relay's `data_dir`
//! and only regenerates when the on-disk cert is older than
//! [`ROTATION_INTERVAL`] (or missing / unreadable). The persisted cert
//! shares its validity window with `wtransport`'s default (14 days);
//! we rotate at 13 days so the new cert overlaps before the old one
//! expires.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use wtransport::Identity;

use crate::error::{Error, Result};

/// Filename for the PEM-encoded certificate chain under `data_dir`.
const CERT_FILE: &str = "wt-cert.pem";

/// Filename for the PEM-encoded private key under `data_dir`.
const KEY_FILE: &str = "wt-key.pem";

/// How long an on-disk cert is considered fresh enough to reuse. Set to
/// 13 days so the next-generation cert is created and persisted at
/// least one day before the current cert's 14-day validity expires.
const ROTATION_INTERVAL: Duration = Duration::from_secs(13 * 24 * 60 * 60);

/// Load the persisted identity if it exists and is younger than
/// [`ROTATION_INTERVAL`]; otherwise generate a fresh self-signed
/// identity, persist it under `data_dir`, and return it.
///
/// `subject_alt_names` is forwarded to
/// `Identity::self_signed`. They include the hostnames / IPs the
/// `serverCertificateHashes`-pinning client is allowed to dial; for
/// loopback tests this is `["127.0.0.1", "localhost"]`.
pub async fn load_or_generate<I, S>(data_dir: &Path, subject_alt_names: I) -> Result<Identity>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let cert_path = data_dir.join(CERT_FILE);
    let key_path = data_dir.join(KEY_FILE);

    if let Some(identity) = try_load_fresh(&cert_path, &key_path).await? {
        tracing::debug!(
            cert_path = %cert_path.display(),
            "wt cert: reusing persisted identity"
        );
        return Ok(identity);
    }

    let identity = Identity::self_signed(subject_alt_names)
        .map_err(|e| Error::Identity(format!("wt self-signed: {e}")))?;
    persist(&identity, &cert_path, &key_path).await?;
    tracing::info!(
        cert_path = %cert_path.display(),
        "wt cert: generated and persisted new self-signed identity"
    );
    Ok(identity)
}

/// Try to load a persisted identity. Returns `Ok(None)` (rather than
/// `Err`) when the files are missing or stale, so the caller can fall
/// through to regeneration; `Err` is reserved for IO errors that
/// shouldn't be silently swallowed (e.g. `data_dir` is unreadable).
async fn try_load_fresh(cert_path: &Path, key_path: &Path) -> Result<Option<Identity>> {
    if !cert_path.exists() || !key_path.exists() {
        return Ok(None);
    }
    let meta = match tokio::fs::metadata(cert_path).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };
    let modified = meta.modified().map_err(Error::Io)?;
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    if age >= ROTATION_INTERVAL {
        tracing::info!(
            cert_path = %cert_path.display(),
            age_secs = age.as_secs(),
            "wt cert: persisted cert is stale, regenerating"
        );
        return Ok(None);
    }
    match Identity::load_pemfiles(cert_path, key_path).await {
        Ok(id) => Ok(Some(id)),
        Err(e) => {
            // Corrupt cert file — treat as missing (we'll regenerate).
            tracing::warn!(
                cert_path = %cert_path.display(),
                error = %e,
                "wt cert: failed to load persisted PEM files, regenerating"
            );
            Ok(None)
        }
    }
}

async fn persist(identity: &Identity, cert_path: &PathBuf, key_path: &PathBuf) -> Result<()> {
    if let Some(parent) = cert_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    identity
        .certificate_chain()
        .store_pemfile(cert_path)
        .await
        .map_err(|e| Error::Identity(format!("wt cert write: {e}")))?;
    identity
        .private_key()
        .store_secret_pemfile(key_path)
        .await
        .map_err(|e| Error::Identity(format!("wt key write: {e}")))?;
    set_mode_0600(key_path).await?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn first_call_generates_then_subsequent_calls_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let id1 = load_or_generate(dir.path(), ["localhost", "127.0.0.1"])
            .await
            .unwrap();
        // Files now exist.
        assert!(dir.path().join(CERT_FILE).exists());
        assert!(dir.path().join(KEY_FILE).exists());

        let id2 = load_or_generate(dir.path(), ["localhost", "127.0.0.1"])
            .await
            .unwrap();

        // Same SPKI hash — proves we loaded rather than regenerated.
        let h1 = id1.certificate_chain().as_slice()[0].hash();
        let h2 = id2.certificate_chain().as_slice()[0].hash();
        assert_eq!(h1.as_ref(), h2.as_ref());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_cert_triggers_regeneration() {
        let dir = tempfile::tempdir().unwrap();
        let _id1 = load_or_generate(dir.path(), ["localhost"]).await.unwrap();
        let cert_path = dir.path().join(CERT_FILE);
        // Backdate the cert file's mtime to outside ROTATION_INTERVAL.
        let stale_time = SystemTime::now() - ROTATION_INTERVAL - Duration::from_secs(60);
        let stale_filetime = filetime::FileTime::from_system_time(stale_time);
        filetime::set_file_mtime(&cert_path, stale_filetime).unwrap();

        let id_after = load_or_generate(dir.path(), ["localhost"]).await.unwrap();
        let h_after = id_after.certificate_chain().as_slice()[0].hash();
        // The new file mtime should now be fresh.
        let meta = tokio::fs::metadata(&cert_path).await.unwrap();
        let age = SystemTime::now()
            .duration_since(meta.modified().unwrap())
            .unwrap();
        assert!(age < Duration::from_secs(60), "expected fresh cert mtime");
        // And the SPKI hash differs from a freshly-generated independent
        // identity (cert serials are random — equality would mean we
        // somehow re-loaded the stale one).
        let id_independent = Identity::self_signed(["localhost"]).unwrap();
        let h_indep = id_independent.certificate_chain().as_slice()[0].hash();
        assert_ne!(h_after.as_ref(), h_indep.as_ref(), "spurious match");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn key_file_has_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let _ = load_or_generate(dir.path(), ["localhost"]).await.unwrap();
        let meta = tokio::fs::metadata(dir.path().join(KEY_FILE))
            .await
            .unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}
