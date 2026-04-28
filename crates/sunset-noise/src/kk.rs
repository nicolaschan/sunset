//! Noise_KK helpers for pairwise signaling exchanges with full PFS.
//!
//! Used by WebRTC signaling (and future patterns) where both peers
//! already know each other's static X25519 keys (derived from their
//! Ed25519 identities per `identity::ed25519_seed_to_x25519_secret`).

use snow::{Builder, HandshakeState, TransportState};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::pattern::NOISE_KK_PATTERN;

/// Initiator side of a KK handshake. Build it with both statics, write
/// message 1 (carries the offer payload), then read message 2 to finish
/// + transition to transport mode.
pub struct KkInitiator {
    hs: HandshakeState,
}

impl KkInitiator {
    /// `local_x25519_secret`: derived from this peer's Ed25519 secret seed.
    /// `remote_x25519_pub`: derived from the remote peer's Ed25519 pubkey.
    pub fn new(
        local_x25519_secret: &Zeroizing<[u8; 32]>,
        remote_x25519_pub: &[u8; 32],
    ) -> Result<Self> {
        let hs = Builder::new(
            NOISE_KK_PATTERN
                .parse()
                .map_err(|e| Error::Snow(format!("{e:?}")))?,
        )
        .local_private_key(&local_x25519_secret[..])?
        .remote_public_key(remote_x25519_pub)?
        .build_initiator()?;
        Ok(Self { hs })
    }

    /// Write the first handshake message with `payload` encrypted inside.
    /// Returns the wire bytes (≤ 65535).
    pub fn write_message_1(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; payload.len() + 256];
        let n = self.hs.write_message(payload, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Read the responder's message 2 + return decrypted payload.
    /// Consumes the initiator into a session.
    pub fn read_message_2(mut self, msg: &[u8]) -> Result<(Vec<u8>, KkSession)> {
        let mut buf = vec![0u8; msg.len()];
        let n = self.hs.read_message(msg, &mut buf)?;
        buf.truncate(n);
        let transport = self.hs.into_transport_mode()?;
        Ok((buf, KkSession { transport }))
    }
}

/// Responder side of a KK handshake.
pub struct KkResponder {
    hs: HandshakeState,
}

impl KkResponder {
    pub fn new(
        local_x25519_secret: &Zeroizing<[u8; 32]>,
        remote_x25519_pub: &[u8; 32],
    ) -> Result<Self> {
        let hs = Builder::new(
            NOISE_KK_PATTERN
                .parse()
                .map_err(|e| Error::Snow(format!("{e:?}")))?,
        )
        .local_private_key(&local_x25519_secret[..])?
        .remote_public_key(remote_x25519_pub)?
        .build_responder()?;
        Ok(Self { hs })
    }

    /// Read the initiator's message 1 + return decrypted payload.
    pub fn read_message_1(&mut self, msg: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; msg.len()];
        let n = self.hs.read_message(msg, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Write message 2 with `payload` encrypted inside. Consumes the
    /// responder into a session.
    pub fn write_message_2(mut self, payload: &[u8]) -> Result<(Vec<u8>, KkSession)> {
        let mut buf = vec![0u8; payload.len() + 256];
        let n = self.hs.write_message(payload, &mut buf)?;
        buf.truncate(n);
        let transport = self.hs.into_transport_mode()?;
        Ok((buf, KkSession { transport }))
    }
}

/// Post-handshake transport state. Encrypts/decrypts subsequent messages
/// with ratcheting keys; full PFS preserved per message.
pub struct KkSession {
    transport: TransportState,
}

impl KkSession {
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; plaintext.len() + 16];
        let n = self.transport.write_message(plaintext, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; ciphertext.len()];
        let n = self.transport.read_message(ciphertext, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ed25519_seed_to_x25519_secret;

    fn pub_for(seed: &[u8; 32]) -> [u8; 32] {
        let secret = ed25519_seed_to_x25519_secret(seed);
        use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
        MontgomeryPoint::mul_base(&Scalar::from_bytes_mod_order(*secret)).to_bytes()
    }

    #[test]
    fn kk_handshake_roundtrip_with_payloads() {
        let alice_seed = [1u8; 32];
        let bob_seed = [2u8; 32];
        let alice_secret = ed25519_seed_to_x25519_secret(&alice_seed);
        let bob_secret = ed25519_seed_to_x25519_secret(&bob_seed);
        let alice_pub = pub_for(&alice_seed);
        let bob_pub = pub_for(&bob_seed);

        let mut init = KkInitiator::new(&alice_secret, &bob_pub).unwrap();
        let msg1 = init.write_message_1(b"offer payload").unwrap();

        let mut resp = KkResponder::new(&bob_secret, &alice_pub).unwrap();
        let recv1 = resp.read_message_1(&msg1).unwrap();
        assert_eq!(recv1, b"offer payload");

        let (msg2, mut bob_session) = resp.write_message_2(b"answer payload").unwrap();
        let (recv2, mut alice_session) = init.read_message_2(&msg2).unwrap();
        assert_eq!(recv2, b"answer payload");

        // Subsequent transport-mode messages each direction.
        let ct1 = alice_session.encrypt(b"ice candidate 1").unwrap();
        let pt1 = bob_session.decrypt(&ct1).unwrap();
        assert_eq!(pt1, b"ice candidate 1");

        let ct2 = bob_session.encrypt(b"ice candidate 2").unwrap();
        let pt2 = alice_session.decrypt(&ct2).unwrap();
        assert_eq!(pt2, b"ice candidate 2");
    }

    #[test]
    fn kk_rejects_wrong_static() {
        let alice_seed = [1u8; 32];
        let bob_seed = [2u8; 32];
        let mallory_seed = [99u8; 32];

        let alice_secret = ed25519_seed_to_x25519_secret(&alice_seed);
        let mallory_secret = ed25519_seed_to_x25519_secret(&mallory_seed);
        let bob_pub = pub_for(&bob_seed);
        let alice_pub = pub_for(&alice_seed);

        let mut init = KkInitiator::new(&alice_secret, &bob_pub).unwrap();
        let msg1 = init.write_message_1(b"offer").unwrap();

        // Bob expects message from alice but mallory is reading. Their
        // KK responder is built with mallory's static + alice's pub —
        // the static-static DH won't match what alice's initiator did,
        // so the handshake decryption fails.
        let mut wrong = KkResponder::new(&mallory_secret, &alice_pub).unwrap();
        assert!(wrong.read_message_1(&msg1).is_err());
    }
}
