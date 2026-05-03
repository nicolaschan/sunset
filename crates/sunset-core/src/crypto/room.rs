//! Room-derived secrets: `K_room` (Argon2id of room name), `room_fingerprint`
//! (blake3-keyed hash), and `K_epoch_0` for open rooms (HKDF from `K_room`).

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::crypto::constants::{
    EPOCH_0_DOMAIN, FINGERPRINT_DOMAIN, ROOM_KEY_SALT, production_params,
};
use crate::error::{Error, Result};

/// 32-byte room identifier visible on the wire. Computed from `K_room` via
/// blake3-keyed hashing — the room name itself is never on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoomFingerprint(pub [u8; 32]);

impl RoomFingerprint {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

/// All key material derived from a room name. Held entirely in
/// `Zeroizing<[u8; 32]>` so process memory is wiped on drop.
///
/// Open rooms in v1 use only `epoch_0_root` for message encryption.
/// Invite-only rooms (Plan 8) will keep `epoch_0_root` randomly generated
/// and distributed via key bundles.
#[derive(Clone)]
pub struct Room {
    fingerprint: RoomFingerprint,
    k_room: Zeroizing<[u8; 32]>,
    epoch_0_root: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for Room {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Room")
            .field("fingerprint", &self.fingerprint)
            .field("k_room", &"<redacted>")
            .field("epoch_0_root", &"<redacted>")
            .finish()
    }
}

impl Room {
    /// Open-room construction with **production** Argon2id parameters.
    /// Slow (~tens to hundreds of ms). Use `open_with_params` in tests.
    pub fn open(room_name: &str) -> Result<Self> {
        Self::open_with_params(room_name, &production_params())
    }

    /// Open-room construction with caller-supplied Argon2id parameters.
    /// The frozen test vectors below use `test_fast_params()`.
    pub fn open_with_params(room_name: &str, params: &Params) -> Result<Self> {
        // 1. K_room = Argon2id(room_name, ROOM_KEY_SALT, params).
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone());
        let mut k_room = Zeroizing::new([0u8; 32]);
        argon2
            .hash_password_into(room_name.as_bytes(), ROOM_KEY_SALT, &mut *k_room)
            .map_err(|e| Error::Argon2(e.to_string()))?;

        // 2. room_fingerprint = blake3.keyed_hash(K_room, FINGERPRINT_DOMAIN).
        let fingerprint =
            RoomFingerprint(*blake3::keyed_hash(&k_room, FINGERPRINT_DOMAIN).as_bytes());

        // 3. K_epoch_0 = HKDF-SHA256(K_room, info = EPOCH_0_DOMAIN, 32 bytes).
        let mut epoch_0_root = Zeroizing::new([0u8; 32]);
        let hkdf = Hkdf::<Sha256>::new(None, &*k_room);
        hkdf.expand(EPOCH_0_DOMAIN, &mut *epoch_0_root)
            .expect("HKDF-SHA256 expand of 32 bytes never errors");

        Ok(Self {
            fingerprint,
            k_room,
            epoch_0_root,
        })
    }

    pub fn fingerprint(&self) -> RoomFingerprint {
        self.fingerprint
    }

    /// Layer-1 K_room. Used for control-plane entries (presence, membership ops).
    /// Plan 6 itself doesn't AEAD-encrypt anything with `K_room`; exposed for
    /// Plan 7 / Plan 8 callers and for tests.
    pub fn k_room(&self) -> &[u8; 32] {
        &self.k_room
    }

    /// Look up the root key for an epoch this `Room` knows about. In Plan 6's
    /// scope, only epoch 0 is known; higher epochs return `None`.
    pub fn epoch_root(&self, epoch_id: u64) -> Option<&[u8; 32]> {
        if epoch_id == 0 {
            Some(&self.epoch_0_root)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::constants::test_fast_params;

    #[test]
    fn two_opens_of_the_same_name_yield_the_same_secrets() {
        let a = Room::open_with_params("general", &test_fast_params()).unwrap();
        let b = Room::open_with_params("general", &test_fast_params()).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.k_room(), b.k_room());
        assert_eq!(a.epoch_root(0).unwrap(), b.epoch_root(0).unwrap());
    }

    #[test]
    fn different_names_yield_different_secrets() {
        let a = Room::open_with_params("general", &test_fast_params()).unwrap();
        let b = Room::open_with_params("random", &test_fast_params()).unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.k_room(), b.k_room());
        assert_ne!(a.epoch_root(0).unwrap(), b.epoch_root(0).unwrap());
    }

    #[test]
    fn epoch_root_only_known_for_epoch_zero_in_v1() {
        let r = Room::open_with_params("general", &test_fast_params()).unwrap();
        assert!(r.epoch_root(0).is_some());
        assert!(r.epoch_root(1).is_none());
        assert!(r.epoch_root(u64::MAX).is_none());
    }

    /// Frozen wire-format vector for "general" under `test_fast_params()`.
    /// If any of these hashes change, the v1 chat wire format has drifted —
    /// bump the version before updating the constants.
    #[test]
    fn general_room_secrets_frozen_vector() {
        let r = Room::open_with_params("general", &test_fast_params()).unwrap();
        assert_eq!(
            hex::encode(r.k_room()),
            "ed556dc4531abc958c934d7e89b1ba1d50813a7980e82fac4ba32818b2af395d",
            "If this fails, K_room derivation has drifted — DO NOT update without a wire-format bump.",
        );
        assert_eq!(
            r.fingerprint().to_hex(),
            "7e73b540dcd5ff94ef8a45b209674fa5153591a4d96acc8d23d977388a5bcc78",
            "If this fails, room_fingerprint derivation has drifted — DO NOT update without a wire-format bump.",
        );
        assert_eq!(
            hex::encode(r.epoch_root(0).unwrap()),
            "ffb0d1a3c7e6b2c75d11ef6c44e3cdfb91f6ec04d8a01ac2a8e9aba7382a0ce9",
            "If this fails, K_epoch_0 derivation has drifted — DO NOT update without a wire-format bump.",
        );
    }
}
