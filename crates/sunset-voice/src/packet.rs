//! `VoicePacket` wire format + AEAD for the voice path.
//!
//! `VoicePacket` is a postcard-encoded enum carrying either an audio
//! frame or a membership heartbeat. `EncryptedVoicePacket` is the
//! XChaCha20-Poly1305 ciphertext + nonce that ends up as the payload of
//! a `SignedDatagram` on the Bus.
//!
//! Per-packet random nonce; AAD binds room fingerprint + sender id, so
//! a packet from sender X cannot be replayed claiming to be from
//! sender Y, and a packet from one room cannot be replayed into another.

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

use sunset_core::Room;
use sunset_core::crypto::aead::{aead_decrypt, aead_encrypt};
use sunset_core::identity::IdentityKey;

pub const VOICE_KEY_DOMAIN: &[u8] = b"sunset/voice/key/v1";
pub const VOICE_AAD_DOMAIN: &[u8] = b"sunset/voice/aad/v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoicePacket {
    Frame {
        codec_id: String,
        seq: u64,
        sender_time_ms: u64,
        payload: Vec<u8>,
    },
    Heartbeat {
        sent_at_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedVoicePacket {
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("epoch {0} not present in room")]
    EpochMissing(u64),
    #[error("postcard encode/decode failed: {0}")]
    Postcard(String),
    #[error("AEAD authentication failed")]
    AeadAuthFailed,
}

pub type Result<T> = core::result::Result<T, Error>;

/// HKDF-SHA256(epoch_root || epoch_id_le, info=VOICE_KEY_DOMAIN || epoch_id_le).
/// Pinned to one epoch per call so future epoch rotation lifts cleanly.
pub fn derive_voice_key(room: &Room, epoch_id: u64) -> Result<Zeroizing<[u8; 32]>> {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let epoch_root = room.epoch_root(epoch_id).ok_or(Error::EpochMissing(epoch_id))?;
    let mut info = Vec::with_capacity(VOICE_KEY_DOMAIN.len() + 8);
    info.extend_from_slice(VOICE_KEY_DOMAIN);
    info.extend_from_slice(&epoch_id.to_le_bytes());
    let hkdf = Hkdf::<Sha256>::new(None, epoch_root);
    let mut k = Zeroizing::new([0u8; 32]);
    hkdf.expand(&info, &mut *k)
        .expect("HKDF-SHA256 expand of 32 bytes never errors");
    Ok(k)
}

fn build_voice_aad(room: &Room, sender: &IdentityKey) -> Vec<u8> {
    let fp = room.fingerprint();
    let mut ad = Vec::with_capacity(VOICE_AAD_DOMAIN.len() + 32 + 32);
    ad.extend_from_slice(VOICE_AAD_DOMAIN);
    ad.extend_from_slice(fp.as_bytes());
    ad.extend_from_slice(&sender.as_bytes());
    ad
}

fn fresh_nonce<R: CryptoRngCore + ?Sized>(rng: &mut R) -> [u8; 24] {
    let mut n = [0u8; 24];
    rng.fill_bytes(&mut n);
    n
}

pub fn encrypt<R: CryptoRngCore + ?Sized>(
    room: &Room,
    epoch_id: u64,
    sender: &IdentityKey,
    packet: &VoicePacket,
    rng: &mut R,
) -> Result<EncryptedVoicePacket> {
    let key = derive_voice_key(room, epoch_id)?;
    let pt = postcard::to_stdvec(packet).map_err(|e| Error::Postcard(format!("{e}")))?;
    let nonce = fresh_nonce(rng);
    let aad = build_voice_aad(room, sender);
    let ct = aead_encrypt(&key, &nonce, &aad, &pt);
    Ok(EncryptedVoicePacket {
        nonce,
        ciphertext: ct,
    })
}

pub fn decrypt(
    room: &Room,
    epoch_id: u64,
    sender: &IdentityKey,
    ev: &EncryptedVoicePacket,
) -> Result<VoicePacket> {
    let key = derive_voice_key(room, epoch_id)?;
    let aad = build_voice_aad(room, sender);
    let pt = aead_decrypt(&key, &ev.nonce, &aad, &ev.ciphertext)
        .map_err(|_| Error::AeadAuthFailed)?;
    postcard::from_bytes(&pt).map_err(|e| Error::Postcard(format!("{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use sunset_core::Identity;

    fn fixed_packet_frame() -> VoicePacket {
        VoicePacket::Frame {
            codec_id: "pcm-f32-le".to_string(),
            seq: 42,
            sender_time_ms: 1_700_000_000_000,
            payload: (0..3840u32).map(|i| (i & 0xff) as u8).collect(),
        }
    }

    fn fixed_heartbeat() -> VoicePacket {
        VoicePacket::Heartbeat { sent_at_ms: 1_700_000_000_000 }
    }

    #[test]
    fn round_trip_frame() {
        let room = Room::open("room-A").unwrap();
        let id = Identity::generate(&mut OsRng);
        let pkt = fixed_packet_frame();
        let ev = encrypt(&room, 0, &id.public(), &pkt, &mut OsRng).unwrap();
        let back = decrypt(&room, 0, &id.public(), &ev).unwrap();
        assert_eq!(pkt, back);
    }

    #[test]
    fn round_trip_heartbeat() {
        let room = Room::open("room-A").unwrap();
        let id = Identity::generate(&mut OsRng);
        let pkt = fixed_heartbeat();
        let ev = encrypt(&room, 0, &id.public(), &pkt, &mut OsRng).unwrap();
        let back = decrypt(&room, 0, &id.public(), &ev).unwrap();
        assert_eq!(pkt, back);
    }

    #[test]
    fn decrypt_wrong_room_fails() {
        let room_a = Room::open("room-A").unwrap();
        let room_b = Room::open("room-B").unwrap();
        let id = Identity::generate(&mut OsRng);
        let ev = encrypt(&room_a, 0, &id.public(), &fixed_packet_frame(), &mut OsRng).unwrap();
        let res = decrypt(&room_b, 0, &id.public(), &ev);
        assert!(matches!(res, Err(Error::AeadAuthFailed)));
    }

    #[test]
    fn decrypt_wrong_sender_fails() {
        let room = Room::open("room-A").unwrap();
        let alice = Identity::generate(&mut OsRng);
        let bob = Identity::generate(&mut OsRng);
        let ev = encrypt(&room, 0, &alice.public(), &fixed_packet_frame(), &mut OsRng).unwrap();
        let res = decrypt(&room, 0, &bob.public(), &ev);
        assert!(matches!(res, Err(Error::AeadAuthFailed)));
    }

    #[test]
    fn decrypt_tampered_ciphertext_fails() {
        let room = Room::open("room-A").unwrap();
        let id = Identity::generate(&mut OsRng);
        let mut ev = encrypt(&room, 0, &id.public(), &fixed_packet_frame(), &mut OsRng).unwrap();
        ev.ciphertext[0] ^= 1;
        let res = decrypt(&room, 0, &id.public(), &ev);
        assert!(matches!(res, Err(Error::AeadAuthFailed)));
    }

    #[test]
    fn missing_epoch_errors() {
        let room = Room::open("room-A").unwrap();
        let id = Identity::generate(&mut OsRng);
        let res = encrypt(&room, 999, &id.public(), &fixed_heartbeat(), &mut OsRng);
        assert!(matches!(res, Err(Error::EpochMissing(999))));
    }
}
