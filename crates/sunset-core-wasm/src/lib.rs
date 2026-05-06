//! WASM bridge exposing `sunset-core`'s pure functions to JavaScript.
//!
//! See `docs/superpowers/specs/2026-04-26-sunset-core-wasm-bridge-design.md`
//! for the design and the full JS surface contract.

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use wasm_bindgen::prelude::*;

use sunset_core::{
    ChannelLabel, ComposedMessage as CoreComposedMessage, Ed25519Verifier, Identity, MessageBody,
    Room, compose_message as core_compose, decode_message as core_decode,
};
use sunset_store::{ContentBlock, SignatureVerifier, SignedKvEntry};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a sunset-core/sunset-store error into a `JsError` with a stable
/// `"sunset-core: <variant>: <display>"` message prefix.
fn js_err<E: std::fmt::Display>(prefix: &str, e: E) -> JsError {
    JsError::new(&format!("sunset-core: {}: {}", prefix, e))
}

fn require_32(label: &str, slice: &[u8]) -> Result<[u8; 32], JsError> {
    <[u8; 32]>::try_from(slice).map_err(|_| {
        JsError::new(&format!(
            "sunset-core: {}: expected 32 bytes, got {}",
            label,
            slice.len(),
        ))
    })
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

#[wasm_bindgen]
#[derive(Debug)]
pub struct GeneratedIdentity {
    #[wasm_bindgen(getter_with_clone)]
    pub secret: Vec<u8>,
    #[wasm_bindgen(getter_with_clone)]
    pub public: Vec<u8>,
}

/// Derive an Ed25519 identity from a 32-byte caller-supplied seed.
///
/// JS callers should produce the seed via `crypto.getRandomValues(new Uint8Array(32))`.
#[wasm_bindgen]
pub fn identity_generate(seed: &[u8]) -> Result<GeneratedIdentity, JsError> {
    let seed = require_32("identity_generate seed", seed)?;
    let id = Identity::from_secret_bytes(&seed);
    Ok(GeneratedIdentity {
        secret: seed.to_vec(),
        public: id.public().as_bytes().to_vec(),
    })
}

/// Recover the public half from a stored 32-byte secret seed.
#[wasm_bindgen]
pub fn identity_public_from_secret(secret: &[u8]) -> Result<Vec<u8>, JsError> {
    let seed = require_32("identity_public_from_secret secret", secret)?;
    Ok(Identity::from_secret_bytes(&seed)
        .public()
        .as_bytes()
        .to_vec())
}

// ---------------------------------------------------------------------------
// Room
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub struct OpenedRoom {
    #[wasm_bindgen(getter_with_clone)]
    pub fingerprint: Vec<u8>,
    #[wasm_bindgen(getter_with_clone)]
    pub k_room: Vec<u8>,
    #[wasm_bindgen(getter_with_clone)]
    pub epoch_0_root: Vec<u8>,
}

/// Open a room with PRODUCTION Argon2id params.
///
/// Slow (tens to hundreds of ms). JS callers should cache the result per
/// room name in a session-scoped `Map<string, OpenedRoom>` to avoid paying
/// the Argon2 cost on every compose / decode.
#[wasm_bindgen]
pub fn room_open(name: &str) -> Result<OpenedRoom, JsError> {
    let r = Room::open(name).map_err(|e| js_err("room_open", e))?;
    Ok(OpenedRoom {
        fingerprint: r.fingerprint().as_bytes().to_vec(),
        k_room: r.k_room().to_vec(),
        epoch_0_root: r.epoch_root(0).expect("epoch 0 always present").to_vec(),
    })
}

/// Build the `NamePrefix` filter bytes for "all messages in this room".
///
/// Pairs with the entry name format `<hex(fingerprint)>/msg/<hex(value_hash)>`
/// produced by `compose_message`. JS hands these bytes to sunset-sync (via
/// later plans) as the subscription filter.
#[wasm_bindgen]
pub fn room_messages_filter_prefix(fingerprint: &[u8]) -> Result<Vec<u8>, JsError> {
    let fp = require_32("room_messages_filter_prefix fingerprint", fingerprint)?;
    Ok(format!("{}/msg/", hex::encode(fp)).into_bytes())
}

// ---------------------------------------------------------------------------
// Compose / decode
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub struct ComposedMessage {
    #[wasm_bindgen(getter_with_clone)]
    pub entry: Vec<u8>,
    #[wasm_bindgen(getter_with_clone)]
    pub block: Vec<u8>,
}

/// Compose: full encrypt + sign pipeline.
///
/// Returns postcard-encoded `entry` + `block` bytes. JS hands these to
/// sunset-sync (Plans C/D/E) for transport + insert.
#[wasm_bindgen]
pub fn compose_message(
    secret: &[u8],
    room_name: &str,
    epoch_id: u64,
    sent_at_ms: u64,
    body: &str,
    nonce_seed: &[u8],
) -> Result<ComposedMessage, JsError> {
    let secret = require_32("compose_message secret", secret)?;
    let nonce_seed = require_32("compose_message nonce_seed", nonce_seed)?;

    let identity = Identity::from_secret_bytes(&secret);
    let room = Room::open(room_name).map_err(|e| js_err("compose_message room_open", e))?;
    let mut rng = ChaCha20Rng::from_seed(nonce_seed);

    let CoreComposedMessage { entry, block } = core_compose(
        &identity,
        &room,
        epoch_id,
        sent_at_ms,
        ChannelLabel::default_general(),
        MessageBody::Text(body.to_owned()),
        &mut rng,
    )
    .map_err(|e| js_err("compose_message", e))?;

    Ok(ComposedMessage {
        entry: postcard::to_stdvec(&entry)
            .map_err(|e| js_err("compose_message entry encode", e))?,
        block: postcard::to_stdvec(&block)
            .map_err(|e| js_err("compose_message block encode", e))?,
    })
}

#[wasm_bindgen]
#[derive(Debug)]
pub struct DecodedMessage {
    #[wasm_bindgen(getter_with_clone)]
    pub author_pubkey: Vec<u8>,
    pub epoch_id: u64,
    pub sent_at_ms: u64,
    #[wasm_bindgen(getter_with_clone)]
    pub body: String,
}

/// Decode: AEAD-decrypt + inner-sig verify.
#[wasm_bindgen]
pub fn decode_message(
    room_name: &str,
    entry: &[u8],
    block: &[u8],
) -> Result<DecodedMessage, JsError> {
    let entry: SignedKvEntry =
        postcard::from_bytes(entry).map_err(|e| js_err("decode_message entry decode", e))?;
    let block: ContentBlock =
        postcard::from_bytes(block).map_err(|e| js_err("decode_message block decode", e))?;
    let room = Room::open(room_name).map_err(|e| js_err("decode_message room_open", e))?;

    let decoded = core_decode(&room, &entry, &block).map_err(|e| js_err("decode_message", e))?;

    let body_text = match decoded.body {
        MessageBody::Text(t) => t,
        other => {
            return Err(JsError::new(&format!(
                "sunset-core: decode_message: unsupported body variant: {:?}",
                other
            )));
        }
    };

    Ok(DecodedMessage {
        author_pubkey: decoded.author_key.as_bytes().to_vec(),
        epoch_id: decoded.epoch_id,
        sent_at_ms: decoded.sent_at_ms,
        body: body_text,
    })
}

/// Verify an entry's outer Ed25519 signature.
///
/// JS callers can use this to gate entries received via sunset-sync before
/// forwarding them into a local store with `Ed25519Verifier` enabled.
#[wasm_bindgen]
pub fn verify_entry_signature(entry: &[u8]) -> Result<(), JsError> {
    let entry: SignedKvEntry =
        postcard::from_bytes(entry).map_err(|e| js_err("verify_entry_signature decode", e))?;
    Ed25519Verifier
        .verify(&entry)
        .map_err(|e| js_err("verify_entry_signature", e))?;
    Ok(())
}
