//! Ephemeral Ed25519 identities.
//!
//! `Identity` wraps a private signing key; `IdentityKey` wraps the matching
//! public verifying key. Both round-trip losslessly through the byte form
//! used by `sunset_store::VerifyingKey`.

use bytes::Bytes;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey as DalekVerifyingKey};
use rand_core::CryptoRngCore;

use sunset_store::VerifyingKey as StoreVerifyingKey;

use crate::error::{Error, Result};

/// A keypair that can sign messages on behalf of an ephemeral identity.
#[derive(Clone)]
pub struct Identity {
    signing: SigningKey,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret bytes.
        f.debug_struct("Identity")
            .field("public", &self.public())
            .finish()
    }
}

impl Identity {
    /// Generate a fresh identity from the supplied RNG.
    pub fn generate<R: CryptoRngCore + ?Sized>(rng: &mut R) -> Self {
        let mut seed = [0u8; 32];
        rng.fill_bytes(&mut seed);
        Self { signing: SigningKey::from_bytes(&seed) }
    }

    /// Reconstruct an identity from its 32-byte secret seed.
    pub fn from_secret_bytes(bytes: &[u8; 32]) -> Self {
        Self { signing: SigningKey::from_bytes(bytes) }
    }

    /// Export the 32-byte secret seed.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// Public half of this identity.
    pub fn public(&self) -> IdentityKey {
        IdentityKey { verifying: self.signing.verifying_key() }
    }

    /// Convenience: the public half encoded as a `sunset_store::VerifyingKey`.
    pub fn store_verifying_key(&self) -> StoreVerifyingKey {
        self.public().store_verifying_key()
    }

    /// Sign an arbitrary byte slice with this identity's secret key.
    pub fn sign(&self, msg: &[u8]) -> Signature {
        self.signing.sign(msg)
    }
}

/// The public side of an `Identity`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IdentityKey {
    verifying: DalekVerifyingKey,
}

impl IdentityKey {
    /// Parse a 32-byte Ed25519 verifying key.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self> {
        Ok(Self { verifying: DalekVerifyingKey::from_bytes(bytes)? })
    }

    /// Raw 32-byte encoding.
    pub fn as_bytes(&self) -> [u8; 32] {
        self.verifying.to_bytes()
    }

    /// Lossless conversion to the store's bytes-only form.
    pub fn store_verifying_key(&self) -> StoreVerifyingKey {
        StoreVerifyingKey::new(Bytes::copy_from_slice(&self.verifying.to_bytes()))
    }

    /// Inverse of `store_verifying_key`.
    pub fn from_store_verifying_key(vk: &StoreVerifyingKey) -> Result<Self> {
        let bytes: &[u8] = vk.as_bytes();
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::BadName(format!(
                "verifying key must be 32 bytes, got {}",
                bytes.len(),
            )))?;
        Self::from_bytes(&arr)
    }

    /// Verify a signature against this key.
    pub fn verify(&self, msg: &[u8], sig: &Signature) -> Result<()> {
        Ok(self.verifying.verify(msg, sig)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;

    fn fresh_identity() -> Identity {
        Identity::generate(&mut OsRng)
    }

    #[test]
    fn secret_bytes_roundtrip() {
        let id = fresh_identity();
        let bytes = id.secret_bytes();
        let id2 = Identity::from_secret_bytes(&bytes);
        assert_eq!(id.public(), id2.public());
        assert_eq!(id.secret_bytes(), id2.secret_bytes());
    }

    #[test]
    fn sign_verify_roundtrip_succeeds() {
        let id = fresh_identity();
        let msg = b"hello sunset";
        let sig = id.sign(msg);
        assert!(id.public().verify(msg, &sig).is_ok());
    }

    #[test]
    fn sign_verify_rejects_wrong_message() {
        let id = fresh_identity();
        let sig = id.sign(b"original");
        assert!(id.public().verify(b"tampered", &sig).is_err());
    }

    #[test]
    fn sign_verify_rejects_wrong_key() {
        let alice = fresh_identity();
        let bob = fresh_identity();
        let sig = alice.sign(b"msg");
        assert!(bob.public().verify(b"msg", &sig).is_err());
    }

    #[test]
    fn store_verifying_key_roundtrip() {
        let id = fresh_identity();
        let svk = id.store_verifying_key();
        assert_eq!(svk.as_bytes().len(), 32);
        let recovered = IdentityKey::from_store_verifying_key(&svk).unwrap();
        assert_eq!(recovered, id.public());
    }

    #[test]
    fn store_verifying_key_rejects_wrong_length() {
        let svk = StoreVerifyingKey::new(Bytes::from_static(b"not 32 bytes"));
        let err = IdentityKey::from_store_verifying_key(&svk).unwrap_err();
        assert!(matches!(err, Error::BadName(_)));
    }
}
