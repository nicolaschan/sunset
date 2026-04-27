//! Ed25519 implementation of `sunset_store::SignatureVerifier`.

use ed25519_dalek::{Signature, VerifyingKey as DalekVerifyingKey};

use sunset_store::{
    Error as StoreError, Result as StoreResult, SignatureVerifier, SignedKvEntry,
    canonical::signing_payload,
};

/// Stateless verifier for entries signed by Ed25519 keys.
#[derive(Debug, Default, Clone, Copy)]
pub struct Ed25519Verifier;

impl SignatureVerifier for Ed25519Verifier {
    fn verify(&self, entry: &SignedKvEntry) -> StoreResult<()> {
        let vk_bytes: [u8; 32] = entry
            .verifying_key
            .as_bytes()
            .try_into()
            .map_err(|_| StoreError::SignatureInvalid)?;
        let vk =
            DalekVerifyingKey::from_bytes(&vk_bytes).map_err(|_| StoreError::SignatureInvalid)?;

        let sig_bytes: &[u8] = &entry.signature;
        let sig_arr: &[u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| StoreError::SignatureInvalid)?;
        let sig = Signature::from_bytes(sig_arr);

        let payload = signing_payload(entry);
        vk.verify_strict(&payload, &sig)
            .map_err(|_| StoreError::SignatureInvalid)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use rand_core::OsRng;
    use sunset_store::{Hash, SignedKvEntry, VerifyingKey};

    use crate::identity::Identity;
    use sunset_store::canonical::signing_payload;

    use super::*;

    fn signed_entry(id: &Identity) -> SignedKvEntry {
        let mut entry = SignedKvEntry {
            verifying_key: id.store_verifying_key(),
            name: Bytes::from_static(b"room/general/msg/00"),
            value_hash: Hash::from_bytes([1u8; 32]),
            priority: 1,
            expires_at: None,
            signature: Bytes::new(),
        };
        let sig = id.sign(&signing_payload(&entry));
        entry.signature = Bytes::copy_from_slice(&sig.to_bytes());
        entry
    }

    #[test]
    fn accepts_valid_signature() {
        let id = Identity::generate(&mut OsRng);
        let entry = signed_entry(&id);
        assert!(Ed25519Verifier.verify(&entry).is_ok());
    }

    #[test]
    fn rejects_tampered_payload() {
        let id = Identity::generate(&mut OsRng);
        let mut entry = signed_entry(&id);
        entry.priority += 1;
        assert!(Ed25519Verifier.verify(&entry).is_err());
    }

    #[test]
    fn rejects_wrong_signer() {
        let alice = Identity::generate(&mut OsRng);
        let bob = Identity::generate(&mut OsRng);
        let mut entry = signed_entry(&alice);
        entry.verifying_key = bob.store_verifying_key();
        assert!(Ed25519Verifier.verify(&entry).is_err());
    }

    #[test]
    fn rejects_malformed_verifying_key() {
        let id = Identity::generate(&mut OsRng);
        let mut entry = signed_entry(&id);
        entry.verifying_key = VerifyingKey::new(Bytes::from_static(b"too short"));
        assert!(Ed25519Verifier.verify(&entry).is_err());
    }

    #[test]
    fn rejects_malformed_signature() {
        let id = Identity::generate(&mut OsRng);
        let mut entry = signed_entry(&id);
        entry.signature = Bytes::from_static(b"too short");
        assert!(Ed25519Verifier.verify(&entry).is_err());
    }
}
