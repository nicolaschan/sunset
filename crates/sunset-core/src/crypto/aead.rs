//! Per-message AEAD primitives.
//!
//! Per-message key:
//!   K_msg = HKDF-SHA256(
//!       ikm  = K_epoch_n,
//!       salt = (none),
//!       info = MSG_KEY_DOMAIN || epoch_id_le || value_hash,
//!   ).expand(32 bytes)
//!
//! AEAD: XChaCha20-Poly1305 with a 24-byte random nonce.
//!   ciphertext = AEAD(
//!       key   = K_msg,
//!       nonce = nonce,
//!       ad    = MSG_AAD_DOMAIN || room_fingerprint || epoch_id_le || sender_id || sent_at_ms_le,
//!       pt    = postcard(SignedMessage),
//!   )

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand_core::CryptoRngCore;
use sha2::Sha256;
use sha2::digest::typenum::Unsigned;
use zeroize::Zeroizing;

use sunset_store::Hash;

use crate::crypto::constants::{MSG_AAD_DOMAIN, MSG_KEY_DOMAIN};
use crate::error::{Error, Result};
use crate::identity::IdentityKey;

/// Derive the per-message AEAD key from the epoch root.
pub fn derive_msg_key(
    epoch_root: &[u8; 32],
    epoch_id: u64,
    value_hash: &Hash,
) -> Zeroizing<[u8; 32]> {
    let mut info = Vec::with_capacity(MSG_KEY_DOMAIN.len() + 8 + 32);
    info.extend_from_slice(MSG_KEY_DOMAIN);
    info.extend_from_slice(&epoch_id.to_le_bytes());
    info.extend_from_slice(value_hash.as_bytes());

    let hkdf = Hkdf::<Sha256>::new(None, epoch_root);
    let mut k = Zeroizing::new([0u8; 32]);
    hkdf.expand(&info, &mut *k)
        .expect("HKDF-SHA256 expand of 32 bytes never errors");
    k
}

/// Build the AEAD additional-data string. Binding sender + room + epoch +
/// timestamp into the AD ensures any tamper of those fields fails decryption.
pub fn build_msg_aad(
    room_fp: &[u8; 32],
    epoch_id: u64,
    sender: &IdentityKey,
    sent_at_ms: u64,
) -> Vec<u8> {
    let mut ad = Vec::with_capacity(MSG_AAD_DOMAIN.len() + 32 + 8 + 32 + 8);
    ad.extend_from_slice(MSG_AAD_DOMAIN);
    ad.extend_from_slice(room_fp);
    ad.extend_from_slice(&epoch_id.to_le_bytes());
    ad.extend_from_slice(&sender.as_bytes());
    ad.extend_from_slice(&sent_at_ms.to_le_bytes());
    ad
}

/// Generate a fresh 24-byte XChaCha20-Poly1305 nonce.
pub fn fresh_nonce<R: CryptoRngCore + ?Sized>(rng: &mut R) -> [u8; 24] {
    let mut n = [0u8; 24];
    rng.fill_bytes(&mut n);
    n
}

/// AEAD-encrypt under the given key, nonce, and additional data.
pub fn aead_encrypt(key: &[u8; 32], nonce: &[u8; 24], ad: &[u8], pt: &[u8]) -> Vec<u8> {
    let aead = XChaCha20Poly1305::new(Key::from_slice(key));
    aead.encrypt(XNonce::from_slice(nonce), Payload { msg: pt, aad: ad })
        .expect("XChaCha20-Poly1305 encrypt is infallible for in-memory inputs")
}

/// AEAD-decrypt. Returns `Error::AeadAuthFailed` for any tag failure.
pub fn aead_decrypt(key: &[u8; 32], nonce: &[u8; 24], ad: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
    let aead = XChaCha20Poly1305::new(Key::from_slice(key));
    aead.decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: ad })
        .map_err(|_| Error::AeadAuthFailed)
}

/// Re-exported nonce-size constant for compile-time confirmation in tests.
pub fn nonce_size() -> usize {
    <XChaCha20Poly1305 as AeadCore>::NonceSize::USIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use sunset_store::Hash;

    use crate::identity::Identity;

    fn sample_root() -> [u8; 32] {
        [42u8; 32]
    }
    fn sample_hash() -> Hash {
        Hash::from_bytes([7u8; 32])
    }

    #[test]
    fn nonce_size_is_24_bytes() {
        assert_eq!(nonce_size(), 24);
    }

    #[test]
    fn derive_msg_key_is_deterministic() {
        let a = derive_msg_key(&sample_root(), 0, &sample_hash());
        let b = derive_msg_key(&sample_root(), 0, &sample_hash());
        assert_eq!(*a, *b);
    }

    #[test]
    fn derive_msg_key_separates_epochs() {
        let a = derive_msg_key(&sample_root(), 0, &sample_hash());
        let b = derive_msg_key(&sample_root(), 1, &sample_hash());
        assert_ne!(*a, *b);
    }

    #[test]
    fn derive_msg_key_separates_value_hashes() {
        let a = derive_msg_key(&sample_root(), 0, &Hash::from_bytes([7u8; 32]));
        let b = derive_msg_key(&sample_root(), 0, &Hash::from_bytes([8u8; 32]));
        assert_ne!(*a, *b);
    }

    #[test]
    fn aead_roundtrip_succeeds() {
        let key = [1u8; 32];
        let nonce = [2u8; 24];
        let ad = b"hello-ad";
        let pt = b"hello world";
        let ct = aead_encrypt(&key, &nonce, ad, pt);
        let recovered = aead_decrypt(&key, &nonce, ad, &ct).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn aead_rejects_wrong_key() {
        let nonce = [2u8; 24];
        let ad = b"x";
        let ct = aead_encrypt(&[1u8; 32], &nonce, ad, b"pt");
        assert!(matches!(
            aead_decrypt(&[9u8; 32], &nonce, ad, &ct),
            Err(Error::AeadAuthFailed),
        ));
    }

    #[test]
    fn aead_rejects_wrong_nonce() {
        let key = [1u8; 32];
        let ad = b"x";
        let ct = aead_encrypt(&key, &[2u8; 24], ad, b"pt");
        assert!(matches!(
            aead_decrypt(&key, &[3u8; 24], ad, &ct),
            Err(Error::AeadAuthFailed),
        ));
    }

    #[test]
    fn aead_rejects_wrong_ad() {
        let key = [1u8; 32];
        let nonce = [2u8; 24];
        let ct = aead_encrypt(&key, &nonce, b"original-ad", b"pt");
        assert!(matches!(
            aead_decrypt(&key, &nonce, b"different-ad", &ct),
            Err(Error::AeadAuthFailed),
        ));
    }

    #[test]
    fn aead_rejects_tampered_ciphertext() {
        let key = [1u8; 32];
        let nonce = [2u8; 24];
        let ad = b"x";
        let mut ct = aead_encrypt(&key, &nonce, ad, b"pt");
        ct[0] ^= 1;
        assert!(matches!(
            aead_decrypt(&key, &nonce, ad, &ct),
            Err(Error::AeadAuthFailed),
        ));
    }

    #[test]
    fn build_msg_aad_includes_all_components() {
        let id = Identity::generate(&mut OsRng);
        let ad = build_msg_aad(&[7u8; 32], 0, &id.public(), 1_700_000_000_000);
        assert!(ad.starts_with(MSG_AAD_DOMAIN));
        assert_eq!(ad.len(), MSG_AAD_DOMAIN.len() + 32 + 8 + 32 + 8);
    }

    /// Frozen vector: `derive_msg_key` for a fixed input triple.
    #[test]
    fn derive_msg_key_frozen_vector() {
        let k = derive_msg_key(&sample_root(), 7, &sample_hash());
        assert_eq!(
            hex::encode(*k),
            "d7ba1b0554c7e4aa47d04b4bba861e64aee83e8602511424579704e615b7f5b4",
            "If this fails, the per-message HKDF derivation has drifted — DO NOT update without a wire-format bump.",
        );
    }
}
