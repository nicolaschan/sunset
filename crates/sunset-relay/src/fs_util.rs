//! Small filesystem helpers shared across the relay's persistence paths.

use std::path::Path;

use crate::error::Result;

/// Restrict a freshly-written secret file to owner-only (`0600`) on Unix.
/// No-op on non-Unix platforms.
#[cfg(unix)]
pub(crate) async fn set_mode_0600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = tokio::fs::metadata(path).await?.permissions();
    perms.set_mode(0o600);
    tokio::fs::set_permissions(path, perms).await?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) async fn set_mode_0600(_path: &Path) -> Result<()> {
    Ok(())
}
