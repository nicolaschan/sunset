//! Helpers for constructing sunset-core Identity from a JS-supplied seed.

use sunset_core::Identity;

/// Build an Identity from a 32-byte secret seed.
pub fn identity_from_seed(seed: &[u8]) -> Result<Identity, String> {
    let arr: [u8; 32] = seed
        .try_into()
        .map_err(|_| format!("identity seed must be 32 bytes, got {}", seed.len()))?;
    Ok(Identity::from_secret_bytes(&arr))
}
