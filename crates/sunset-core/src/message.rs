//! End-to-end chat message envelope: ties identity + room + AEAD + store
//! together. The only file that simultaneously knows about `Identity`,
//! `Room`, `Ed25519Verifier`, the AEAD primitives, and `SignedKvEntry`.

use bytes::Bytes;
use ed25519_dalek::Signature as DalekSignature;
use rand_core::CryptoRngCore;

use sunset_store::{ContentBlock, Hash, SignedKvEntry};

use crate::canonical::signing_payload;
use crate::crypto::aead::{aead_decrypt, aead_encrypt, build_msg_aad, derive_msg_key, fresh_nonce};
use crate::crypto::envelope::{
    EncryptedMessage, MessageBody, ReactionAction, SignedMessage, inner_sig_payload_bytes,
};
use crate::crypto::room::{Room, RoomFingerprint};
use crate::error::{Error, Result};
use crate::identity::{Identity, IdentityKey};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposedMessage {
    pub entry: SignedKvEntry,
    pub block: ContentBlock,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedMessage {
    pub author_key: IdentityKey,
    pub room_fingerprint: RoomFingerprint,
    pub epoch_id: u64,
    pub value_hash: Hash,
    pub sent_at_ms: u64,
    pub body: MessageBody,
}

fn message_name(room_fp: &RoomFingerprint, value_hash: &Hash) -> Bytes {
    Bytes::from(format!("{}/msg/{}", room_fp.to_hex(), value_hash.to_hex()))
}

pub fn compose_message<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    body: MessageBody,
    rng: &mut R,
) -> Result<ComposedMessage> {
    let epoch_root = room.epoch_root(epoch_id).ok_or(Error::EpochMismatch)?;
    let room_fp = room.fingerprint();

    let inner_payload = inner_sig_payload_bytes(&room_fp, epoch_id, sent_at_ms, &body);
    let inner_sig = identity.sign(&inner_payload).to_bytes(); // [u8; 64]

    let signed = SignedMessage {
        inner_signature: inner_sig.into(), // convert [u8; 64] -> Signature newtype
        sent_at_ms,
        body,
    };
    let pt = postcard::to_stdvec(&signed)?;
    let nonce = fresh_nonce(rng);

    let pt_hash: Hash = blake3::hash(&pt).into();
    let k_msg = derive_msg_key(epoch_root, epoch_id, &pt_hash);
    let aad = build_msg_aad(room_fp.as_bytes(), epoch_id, &identity.public(), sent_at_ms);
    let ciphertext = aead_encrypt(&k_msg, &nonce, &aad, &pt);

    let envelope = EncryptedMessage {
        epoch_id,
        nonce,
        ciphertext: Bytes::from(ciphertext),
    };
    let block = ContentBlock {
        data: Bytes::from(envelope.to_bytes()),
        references: vec![pt_hash],
    };
    let value_hash = block.hash();

    let mut entry = SignedKvEntry {
        verifying_key: identity.store_verifying_key(),
        name: message_name(&room_fp, &value_hash),
        value_hash,
        priority: sent_at_ms,
        expires_at: None,
        signature: Bytes::new(),
    };
    let outer_sig = identity.sign(&signing_payload(&entry));
    entry.signature = Bytes::copy_from_slice(&outer_sig.to_bytes());

    Ok(ComposedMessage { entry, block })
}

/// Compose a chat text message. Convenience wrapper over
/// `compose_message` with `MessageBody::Text`.
pub fn compose_text<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    text: &str,
    rng: &mut R,
) -> Result<ComposedMessage> {
    compose_message(
        identity,
        room,
        epoch_id,
        sent_at_ms,
        MessageBody::Text(text.to_owned()),
        rng,
    )
}

/// Compose a delivery receipt referencing the given `for_value_hash`
/// (the `value_hash` of the original Text being acknowledged).
pub fn compose_receipt<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    for_value_hash: Hash,
    rng: &mut R,
) -> Result<ComposedMessage> {
    compose_message(
        identity,
        room,
        epoch_id,
        sent_at_ms,
        MessageBody::Receipt { for_value_hash },
        rng,
    )
}

/// The reaction-specific payload passed to [`compose_reaction`]. Bundled
/// to keep `compose_reaction`'s signature consistent with `compose_text`
/// and `compose_receipt` (and below clippy's `too_many_arguments`
/// threshold).
#[derive(Clone, Debug)]
pub struct ReactionPayload<'a> {
    /// `value_hash` of the message being reacted to.
    pub for_value_hash: Hash,
    /// Free-form unicode emoji string. Caller-validated; we only
    /// enforce the 64-byte length cap (covers all unicode emoji
    /// including ZWJ family sequences).
    pub emoji: &'a str,
    /// Whether this event adds or removes the reaction.
    pub action: ReactionAction,
}

/// Compose a reaction event. See [`ReactionPayload`] for the
/// reaction-specific fields.
pub fn compose_reaction<R: CryptoRngCore + ?Sized>(
    identity: &Identity,
    room: &Room,
    epoch_id: u64,
    sent_at_ms: u64,
    payload: &ReactionPayload<'_>,
    rng: &mut R,
) -> Result<ComposedMessage> {
    if payload.emoji.len() > 64 {
        return Err(Error::EmojiTooLong {
            len: payload.emoji.len(),
        });
    }
    compose_message(
        identity,
        room,
        epoch_id,
        sent_at_ms,
        MessageBody::Reaction {
            for_value_hash: payload.for_value_hash,
            emoji: payload.emoji.to_owned(),
            action: payload.action,
        },
        rng,
    )
}

pub fn decode_message(
    room: &Room,
    entry: &SignedKvEntry,
    block: &ContentBlock,
) -> Result<DecodedMessage> {
    if block.hash() != entry.value_hash {
        return Err(Error::BadValueHash);
    }

    let envelope = EncryptedMessage::from_bytes(&block.data)?;
    let epoch_root = room
        .epoch_root(envelope.epoch_id)
        .ok_or(Error::EpochMismatch)?;

    let pt_hash = *block.references.first().ok_or(Error::BadValueHash)?;

    let author_key = IdentityKey::from_store_verifying_key(&entry.verifying_key)?;

    let k_msg = derive_msg_key(epoch_root, envelope.epoch_id, &pt_hash);
    let aad = build_msg_aad(
        room.fingerprint().as_bytes(),
        envelope.epoch_id,
        &author_key,
        entry.priority,
    );
    let pt = aead_decrypt(&k_msg, &envelope.nonce, &aad, &envelope.ciphertext)?;

    let recomputed: Hash = blake3::hash(&pt).into();
    if recomputed != pt_hash {
        return Err(Error::BadValueHash);
    }

    let signed: SignedMessage = postcard::from_bytes(&pt)?;

    if signed.sent_at_ms != entry.priority {
        return Err(Error::AeadAuthFailed);
    }

    let expected_name = message_name(&room.fingerprint(), &entry.value_hash);
    if entry.name != expected_name {
        return Err(Error::BadName(
            "name does not match `<hex_fp>/msg/<hex_value_hash>` for this room".to_string(),
        ));
    }

    let inner_payload = inner_sig_payload_bytes(
        &room.fingerprint(),
        envelope.epoch_id,
        signed.sent_at_ms,
        &signed.body,
    );
    // Use dalek's Signature type (distinct from our envelope::Signature newtype)
    let dalek_sig = DalekSignature::from_bytes(signed.inner_signature.as_bytes());
    author_key.verify(&inner_payload, &dalek_sig)?;

    if let MessageBody::Reaction { ref emoji, .. } = signed.body {
        if emoji.len() > 64 {
            return Err(Error::EmojiTooLong { len: emoji.len() });
        }
    }

    Ok(DecodedMessage {
        author_key,
        room_fingerprint: room.fingerprint(),
        epoch_id: envelope.epoch_id,
        value_hash: entry.value_hash,
        sent_at_ms: signed.sent_at_ms,
        body: signed.body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use sunset_store::SignatureVerifier;

    use crate::crypto::constants::test_fast_params;
    use crate::verifier::Ed25519Verifier;

    fn alice() -> Identity {
        Identity::generate(&mut OsRng)
    }
    fn general() -> Room {
        Room::open_with_params("general", &test_fast_params()).unwrap()
    }

    #[test]
    fn compose_then_decode_roundtrip() {
        let id = alice();
        let room = general();
        let composed = compose_message(
            &id,
            &room,
            0,
            1_700_000_000_000,
            MessageBody::Text("hi".to_owned()),
            &mut OsRng,
        )
        .unwrap();
        let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
        assert_eq!(decoded.author_key, id.public());
        assert_eq!(decoded.room_fingerprint, room.fingerprint());
        assert_eq!(decoded.epoch_id, 0);
        assert_eq!(decoded.body, MessageBody::Text("hi".to_owned()));
        assert_eq!(decoded.sent_at_ms, 1_700_000_000_000);
    }

    #[test]
    fn composed_entry_passes_ed25519_verifier() {
        let id = alice();
        let room = general();
        let composed = compose_message(
            &id,
            &room,
            0,
            1,
            MessageBody::Text("x".to_owned()),
            &mut OsRng,
        )
        .unwrap();
        assert!(Ed25519Verifier.verify(&composed.entry).is_ok());
    }

    #[test]
    fn decode_rejects_wrong_room() {
        let id = alice();
        let alice_room = general();
        let other_room = Room::open_with_params("random", &test_fast_params()).unwrap();
        let composed = compose_message(
            &id,
            &alice_room,
            0,
            1,
            MessageBody::Text("x".to_owned()),
            &mut OsRng,
        )
        .unwrap();
        let err = decode_message(&other_room, &composed.entry, &composed.block).unwrap_err();
        assert!(matches!(err, Error::BadName(_) | Error::AeadAuthFailed));
    }

    #[test]
    fn decode_rejects_block_hash_mismatch() {
        let id = alice();
        let room = general();
        let composed = compose_message(
            &id,
            &room,
            0,
            1,
            MessageBody::Text("x".to_owned()),
            &mut OsRng,
        )
        .unwrap();
        let mut bad_block = composed.block.clone();
        bad_block.data = Bytes::from_static(b"junk");
        let err = decode_message(&room, &composed.entry, &bad_block).unwrap_err();
        assert!(matches!(err, Error::BadValueHash));
    }

    #[test]
    fn decode_rejects_tampered_ciphertext() {
        let id = alice();
        let room = general();
        let composed = compose_message(
            &id,
            &room,
            0,
            1,
            MessageBody::Text("x".to_owned()),
            &mut OsRng,
        )
        .unwrap();
        let mut envelope = EncryptedMessage::from_bytes(&composed.block.data).unwrap();
        let mut ct = envelope.ciphertext.to_vec();
        ct[0] ^= 1;
        envelope.ciphertext = Bytes::from(ct);
        let new_block = ContentBlock {
            data: Bytes::from(envelope.to_bytes()),
            references: composed.block.references.clone(),
        };
        let err = decode_message(&room, &composed.entry, &new_block).unwrap_err();
        assert!(matches!(err, Error::BadValueHash));
    }

    #[test]
    fn decode_rejects_forged_inner_signature() {
        let alice = alice();
        let mallory = Identity::generate(&mut OsRng);
        let room = general();

        let composed = compose_message(
            &alice,
            &room,
            0,
            1,
            MessageBody::Text("real".to_owned()),
            &mut OsRng,
        )
        .unwrap();

        let mut forged = composed.clone();
        let env = EncryptedMessage::from_bytes(&forged.block.data).unwrap();
        let mut signed: SignedMessage = {
            let pt_hash = *forged.block.references.first().unwrap();
            let k_msg = derive_msg_key(room.epoch_root(0).unwrap(), 0, &pt_hash);
            let aad = build_msg_aad(room.fingerprint().as_bytes(), 0, &alice.public(), 1);
            let pt = aead_decrypt(&k_msg, &env.nonce, &aad, &env.ciphertext).unwrap();
            postcard::from_bytes(&pt).unwrap()
        };
        let mallory_sig = mallory.sign(&inner_sig_payload_bytes(
            &room.fingerprint(),
            0,
            signed.sent_at_ms,
            &signed.body,
        ));
        signed.inner_signature = mallory_sig.to_bytes().into(); // convert [u8; 64] -> Signature newtype

        let pt_new = postcard::to_stdvec(&signed).unwrap();
        let pt_hash_new: Hash = blake3::hash(&pt_new).into();
        let k_msg_new = derive_msg_key(room.epoch_root(0).unwrap(), 0, &pt_hash_new);
        let aad = build_msg_aad(room.fingerprint().as_bytes(), 0, &alice.public(), 1);
        let nonce = env.nonce;
        let ct_new = aead_encrypt(&k_msg_new, &nonce, &aad, &pt_new);
        let env_new = EncryptedMessage {
            epoch_id: 0,
            nonce,
            ciphertext: Bytes::from(ct_new),
        };
        let block_new = ContentBlock {
            data: Bytes::from(env_new.to_bytes()),
            references: vec![pt_hash_new],
        };

        forged.entry.value_hash = block_new.hash();
        forged.entry.name = message_name(&room.fingerprint(), &forged.entry.value_hash);
        forged.block = block_new;

        let err = decode_message(&room, &forged.entry, &forged.block).unwrap_err();
        assert!(matches!(err, Error::Signature(_)));
    }

    #[test]
    fn compose_receipt_roundtrips() {
        let id = alice();
        let room = general();
        let target: Hash = blake3::hash(b"original message").into();
        let composed =
            compose_receipt(&id, &room, 0, 1_700_000_000_000, target, &mut OsRng).unwrap();
        let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
        assert_eq!(
            decoded.body,
            MessageBody::Receipt {
                for_value_hash: target
            }
        );
        assert_eq!(decoded.author_key, id.public());
    }

    #[test]
    fn compose_text_roundtrips() {
        let id = alice();
        let room = general();
        let composed = compose_text(&id, &room, 0, 1_700_000_000_000, "hi", &mut OsRng).unwrap();
        let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
        assert_eq!(decoded.body, MessageBody::Text("hi".to_owned()));
    }

    #[test]
    fn compose_reaction_roundtrips_add() {
        let id = alice();
        let room = general();
        let target: Hash = blake3::hash(b"target message").into();
        let composed = compose_reaction(
            &id,
            &room,
            0,
            1_700_000_000_000,
            &ReactionPayload {
                for_value_hash: target,
                emoji: "👍",
                action: crate::crypto::envelope::ReactionAction::Add,
            },
            &mut OsRng,
        )
        .unwrap();
        let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
        assert_eq!(
            decoded.body,
            MessageBody::Reaction {
                for_value_hash: target,
                emoji: "👍".to_owned(),
                action: crate::crypto::envelope::ReactionAction::Add,
            }
        );
        assert_eq!(decoded.author_key, id.public());
    }

    #[test]
    fn compose_reaction_roundtrips_remove() {
        let id = alice();
        let room = general();
        let target: Hash = blake3::hash(b"target").into();
        let composed = compose_reaction(
            &id,
            &room,
            0,
            2,
            &ReactionPayload {
                for_value_hash: target,
                emoji: "🎉",
                action: crate::crypto::envelope::ReactionAction::Remove,
            },
            &mut OsRng,
        )
        .unwrap();
        let decoded = decode_message(&room, &composed.entry, &composed.block).unwrap();
        assert!(matches!(
            decoded.body,
            MessageBody::Reaction {
                action: crate::crypto::envelope::ReactionAction::Remove,
                ..
            }
        ));
    }

    #[test]
    fn compose_reaction_rejects_oversized_emoji() {
        let id = alice();
        let room = general();
        let target: Hash = blake3::hash(b"target").into();
        let oversized = "a".repeat(65); // 65 bytes
        let err = compose_reaction(
            &id,
            &room,
            0,
            1,
            &ReactionPayload {
                for_value_hash: target,
                emoji: &oversized,
                action: crate::crypto::envelope::ReactionAction::Add,
            },
            &mut OsRng,
        )
        .unwrap_err();
        assert!(matches!(err, Error::EmojiTooLong { len: 65 }));
    }

    #[test]
    fn compose_reaction_accepts_max_size_emoji() {
        let id = alice();
        let room = general();
        let target: Hash = blake3::hash(b"target").into();
        let max_size = "a".repeat(64); // exactly at the limit
        let result = compose_reaction(
            &id,
            &room,
            0,
            1,
            &ReactionPayload {
                for_value_hash: target,
                emoji: &max_size,
                action: crate::crypto::envelope::ReactionAction::Add,
            },
            &mut OsRng,
        );
        assert!(
            result.is_ok(),
            "64 bytes should be accepted (limit is inclusive)"
        );
    }

    #[test]
    fn decode_rejects_oversized_reaction_emoji() {
        use crate::crypto::aead::{aead_encrypt, build_msg_aad, derive_msg_key, fresh_nonce};
        use crate::crypto::envelope::{ReactionAction, Signature};

        let id = alice();
        let room = general();
        let room_fp = room.fingerprint();

        // Hand-craft a SignedMessage with an oversized emoji to bypass
        // compose_reaction's length cap. The signature is computed honestly
        // over the oversized payload so we exercise the decode-side check
        // (not signature failure).
        let target: Hash = blake3::hash(b"target").into();
        let body = MessageBody::Reaction {
            for_value_hash: target,
            emoji: "a".repeat(65),
            action: ReactionAction::Add,
        };
        let inner_payload = inner_sig_payload_bytes(&room_fp, 0, 1, &body);
        let inner_sig: Signature = id.sign(&inner_payload).to_bytes().into();
        let signed = SignedMessage {
            inner_signature: inner_sig,
            sent_at_ms: 1,
            body,
        };

        let pt = postcard::to_stdvec(&signed).unwrap();
        let nonce = fresh_nonce(&mut OsRng);
        let pt_hash: Hash = blake3::hash(&pt).into();
        let k_msg = derive_msg_key(room.epoch_root(0).unwrap(), 0, &pt_hash);
        let aad = build_msg_aad(room_fp.as_bytes(), 0, &id.public(), 1);
        let ct = aead_encrypt(&k_msg, &nonce, &aad, &pt);

        let env = EncryptedMessage {
            epoch_id: 0,
            nonce,
            ciphertext: Bytes::from(ct),
        };
        let block = ContentBlock {
            data: Bytes::from(env.to_bytes()),
            references: vec![pt_hash],
        };
        let value_hash = block.hash();
        let mut entry = sunset_store::SignedKvEntry {
            verifying_key: id.store_verifying_key(),
            name: message_name(&room_fp, &value_hash),
            value_hash,
            priority: 1,
            expires_at: None,
            signature: Bytes::new(),
        };
        let outer_sig = id.sign(&crate::canonical::signing_payload(&entry));
        entry.signature = Bytes::copy_from_slice(&outer_sig.to_bytes());

        let err = decode_message(&room, &entry, &block).unwrap_err();
        assert!(matches!(err, Error::EmojiTooLong { len: 65 }));
    }

    #[test]
    fn decode_rejects_unknown_epoch() {
        let id = alice();
        let room = general();
        let mut composed = compose_message(
            &id,
            &room,
            0,
            1,
            MessageBody::Text("x".to_owned()),
            &mut OsRng,
        )
        .unwrap();
        let mut env = EncryptedMessage::from_bytes(&composed.block.data).unwrap();
        env.epoch_id = 99;
        composed.block = ContentBlock {
            data: Bytes::from(env.to_bytes()),
            references: composed.block.references,
        };
        composed.entry.value_hash = composed.block.hash();
        composed.entry.name = message_name(&room.fingerprint(), &composed.entry.value_hash);

        let err = decode_message(&room, &composed.entry, &composed.block).unwrap_err();
        assert!(matches!(err, Error::EpochMismatch));
    }
}
