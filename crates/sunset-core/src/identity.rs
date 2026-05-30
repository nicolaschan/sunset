//! Ephemeral Ed25519 identities.
//!
//! `Identity` wraps a private signing key; `IdentityKey` wraps the matching
//! public verifying key. Both round-trip losslessly through the byte form
//! used by `sunset_store::VerifyingKey`.

use bytes::Bytes;
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey as DalekVerifyingKey};
use rand_core::CryptoRngCore;

use sunset_store::canonical::signing_payload;
use sunset_store::{Hash, SignedKvEntry, VerifyingKey as StoreVerifyingKey};

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
        Self {
            signing: SigningKey::generate(rng),
        }
    }

    /// Reconstruct an identity from its 32-byte secret seed.
    pub fn from_secret_bytes(bytes: &[u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(bytes),
        }
    }

    /// Export the 32-byte secret seed.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// Public half of this identity.
    pub fn public(&self) -> IdentityKey {
        IdentityKey {
            verifying: self.signing.verifying_key(),
        }
    }

    /// Convenience: the public half encoded as a `sunset_store::VerifyingKey`.
    pub fn store_verifying_key(&self) -> StoreVerifyingKey {
        self.public().store_verifying_key()
    }

    /// Sign an arbitrary byte slice with this identity's secret key.
    pub fn sign(&self, msg: &[u8]) -> Signature {
        self.signing.sign(msg)
    }

    /// Seal an entry draft under this identity: fill `verifying_key` from this
    /// identity, compute the entry's canonical signing payload, sign it, and
    /// return the entry with its `signature` field filled in.
    ///
    /// `verifying_key` always comes from the sealing identity, so the signature
    /// is guaranteed to match the stated key. Centralizes the "build payload,
    /// sign, fill signature" sequence so write-side callers don't re-derive it
    /// by hand.
    pub fn seal_entry(&self, draft: EntryDraft) -> SignedKvEntry {
        let mut entry = SignedKvEntry {
            verifying_key: self.store_verifying_key(),
            name: draft.name,
            value_hash: draft.value_hash,
            priority: draft.priority,
            expires_at: draft.expires_at,
            signature: Bytes::new(),
        };
        let payload = signing_payload(&entry);
        entry.signature = Bytes::copy_from_slice(&self.sign(&payload).to_bytes());
        entry
    }
}

/// The signer-independent content of a store entry, before it is sealed.
///
/// `verifying_key` and `signature` are intentionally absent: [`Identity::seal_entry`]
/// fills `verifying_key` from the sealing identity (so the signature always
/// matches the stated key) and computes `signature` over the canonical payload.
pub struct EntryDraft {
    pub name: Bytes,
    pub value_hash: Hash,
    pub priority: u64,
    pub expires_at: Option<u64>,
}

/// The public side of an `Identity`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IdentityKey {
    verifying: DalekVerifyingKey,
}

impl PartialOrd for IdentityKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IdentityKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.verifying.as_bytes().cmp(other.verifying.as_bytes())
    }
}

impl IdentityKey {
    /// Parse a 32-byte Ed25519 verifying key.
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self> {
        Ok(Self {
            verifying: DalekVerifyingKey::from_bytes(bytes)?,
        })
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
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            Error::BadName(format!(
                "verifying key must be 32 bytes, got {}",
                bytes.len(),
            ))
        })?;
        Self::from_bytes(&arr)
    }

    /// Verify a signature against this key.
    pub fn verify(&self, msg: &[u8], sig: &Signature) -> Result<()> {
        Ok(self.verifying.verify(msg, sig)?)
    }
}

use ed25519_dalek::Signer as DalekSigner;

impl sunset_sync::Signer for Identity {
    fn verifying_key(&self) -> sunset_store::VerifyingKey {
        self.store_verifying_key()
    }

    fn sign(&self, payload: &[u8]) -> bytes::Bytes {
        let sig = DalekSigner::sign(&self.signing, payload);
        bytes::Bytes::copy_from_slice(&sig.to_bytes())
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

    #[test]
    fn identity_implements_sync_signer() {
        use sunset_sync::Signer as _;
        let id = Identity::generate(&mut OsRng);
        let sig: bytes::Bytes = sunset_sync::Signer::sign(&id, b"payload");
        assert_eq!(sig.len(), 64);
        assert_eq!(id.verifying_key(), id.store_verifying_key());
    }

    #[test]
    fn seal_entry_produces_verifiable_signature() {
        use sunset_store::canonical::signing_payload;
        use sunset_store::{Hash, SignatureVerifier};

        let id = fresh_identity();
        let draft = EntryDraft {
            name: Bytes::from_static(b"room/general/msg/00"),
            value_hash: Hash::from_bytes([1u8; 32]),
            priority: 7,
            expires_at: None,
        };

        let sealed = id.seal_entry(draft);

        // The store's verifier signs/verifies over `signing_payload`; the
        // sealed signature must validate through that same path.
        assert!(crate::verifier::Ed25519Verifier.verify(&sealed).is_ok());

        // The signature is over the canonical payload — re-deriving it and
        // checking against the verifying key must also succeed, and tampering
        // with a covered field must make it fail (so the test can fail for a
        // real reason).
        assert!(
            crate::verifier::Ed25519Verifier
                .verify_raw(
                    &sealed.verifying_key,
                    &signing_payload(&sealed),
                    &sealed.signature,
                )
                .is_ok()
        );
        let mut tampered = sealed.clone();
        tampered.priority += 1;
        assert!(crate::verifier::Ed25519Verifier.verify(&tampered).is_err());
    }
}
